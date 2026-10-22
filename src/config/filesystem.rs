//! Locate the running system's Btrfs top level independently of its booted subvolume.
//! 独立于启动子卷，定位当前系统所在 Btrfs 文件系统的顶层。

use std::{
    path::{Path, PathBuf},
    process::Command,
};

use serde::Deserialize;

use super::{Error, Result, io_error};

#[derive(Debug, Deserialize)]
struct Mount {
    target: PathBuf,
    fstype: String,
    uuid: Option<String>,
    fsroot: PathBuf,
    options: String,
}

#[derive(Deserialize)]
struct Mounts {
    filesystems: Vec<Mount>,
}

/// Query structured mount metadata without parsing SOURCE's subvolume suffix.
/// 查询结构化挂载信息，避免将 SOURCE 中的子卷后缀误当作设备路径。
fn query(arguments: &[&str]) -> Result<Vec<Mount>> {
    let output = Command::new("findmnt")
        .args(["--json", "--output", "TARGET,FSTYPE,UUID,FSROOT,OPTIONS"])
        .args(arguments)
        .output()
        .map_err(|e| io_error("findmnt", e))?;
    if !output.status.success() {
        return Err(Error::Invalid(format!(
            "无法识别当前系统的挂载: {0} / cannot identify the current system mount: {0}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    let mounts: Mounts = serde_json::from_slice(&output.stdout)
        .map_err(|e| Error::Invalid(format!("findmnt 响应无效 / invalid findmnt response: {e}")))?;
    Ok(mounts.filesystems)
}

/// Identify the filesystem containing /, even when / is a snapshot or bind mount.
/// 识别 / 所属文件系统，即使 / 是快照或绑定挂载。
fn system_uuid(mounts: &[Mount]) -> Result<&str> {
    let [root] = mounts else {
        return Err(Error::Invalid(
            "findmnt 必须返回唯一的当前系统根挂载 / findmnt must return exactly one current system root mount".into(),
        ));
    };
    if root.target != Path::new("/") || root.fstype != "btrfs" {
        return Err(Error::Invalid(
            "当前系统根 / 必须位于 Btrfs；可显式指定已挂载的 Btrfs 顶层绝对路径 / the current system root / must reside on Btrfs; alternatively, specify an absolute path to a mounted Btrfs top level".into(),
        ));
    }
    let uuid = root.uuid.as_deref().unwrap_or("");
    if uuid.is_empty() || !uuid.bytes().all(|v| v.is_ascii_hexdigit() || v == b'-') {
        return Err(Error::Invalid(
            "无法识别当前系统所在 Btrfs 文件系统的 UUID / cannot identify the UUID of the Btrfs filesystem containing the current system".into(),
        ));
    }
    Ok(uuid)
}

/// Match both filesystem identity and top-level tree, excluding read-only views.
/// 同时匹配文件系统身份和顶层树，排除只读视图。
fn writable_top_level<'a>(mounts: &'a [Mount], uuid: &str) -> Option<&'a Path> {
    mounts
        .iter()
        .find(|mount| {
            mount.fstype == "btrfs"
                && mount.uuid.as_deref() == Some(uuid)
                && mount.fsroot == Path::new("/")
                && mount.target.is_absolute()
                && mount.options.split(',').any(|option| option == "rw")
        })
        .map(|mount| mount.target.as_path())
}

/// Run an operation at the running system's Btrfs top level, independently of snapshots.
/// 在当前系统所在 Btrfs 的顶层执行操作，独立于启动快照。
///
/// Reuse an existing writable top-level mount when available; otherwise mount tree 5
/// below /run/rstarsleep and retain that mount on success so returned paths remain usable.
/// 优先复用已有可写顶层挂载；否则在 /run/rstarsleep 下挂载树 5，成功后保留挂载供使用。
/// This does not edit fstab. A retained mount must be re-established after reboot.
/// 不修改 fstab；保留的挂载在重启后需重新建立。
///
/// # Errors
/// Returns an error if / is not Btrfs, discovery or mounting fails, or the operation fails.
/// / 不在 Btrfs 上、识别或挂载失败，以及操作失败时返回错误。
pub fn with_current_system_top_level<T>(operation: impl FnOnce(&Path) -> Result<T>) -> Result<T> {
    let roots = query(&["--target", "/"])?;
    let uuid = system_uuid(&roots)?;
    let mounts = query(&["--list"])?;
    if let Some(path) = writable_top_level(&mounts, uuid) {
        return operation(path);
    }

    let top_level = TopLevel::mount(uuid, Path::new("/run/rstarsleep"))?;
    // Verify the mounted filesystem before creating any manager files.
    // 创建任何管理器文件前，先校验挂载的文件系统。
    let mounted = query(&[
        "--target",
        top_level.path().to_str().ok_or_else(|| {
            Error::Invalid(
                "自动挂载目录必须为 UTF-8 路径 / automatic mount directory must be a UTF-8 path"
                    .into(),
            )
        })?,
    ])?;
    if mounted.len() != 1 || writable_top_level(&mounted, uuid) != Some(top_level.path()) {
        return Err(Error::Invalid(
            "自动挂载未指向当前系统所在 Btrfs 的可写顶层 / automatic mount does not point to the writable top level of the Btrfs filesystem containing the current system".into(),
        ));
    }
    let result = operation(top_level.path())?;
    top_level.retain();
    Ok(result)
}

/// Own only a mount created by this invocation, cleaning it up on failure.
/// 只拥有本次调用创建的挂载，失败时负责清理。
struct TopLevel {
    directory: tempfile::TempDir,
    retained: bool,
}

impl TopLevel {
    fn mount(uuid: &str, parent: &Path) -> Result<Self> {
        std::fs::create_dir_all(parent).map_err(|e| io_error(parent, e))?;
        let mut directory = tempfile::Builder::new()
            .prefix("init-top-")
            .tempdir_in(parent)
            .map_err(|e| io_error(parent, e))?;
        let output = Command::new("mount")
            .args(["-t", "btrfs", "-o", "rw,subvolid=5", "--"])
            .arg(format!("UUID={uuid}"))
            .arg(directory.path())
            .output()
            .map_err(|e| io_error("mount", e))?;
        if !output.status.success() {
            return Err(Error::Btrfs(format!(
                "挂载当前系统所在文件系统的顶层失败: {0} / failed to mount the top level of the filesystem containing the current system: {0}",
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        // Never recursively remove a directory while it may still be mounted.
        // 目录可能仍处于挂载状态时，绝不递归删除。
        directory.disable_cleanup(true);
        Ok(Self {
            directory,
            retained: false,
        })
    }

    fn path(&self) -> &Path {
        self.directory.path()
    }

    fn retain(mut self) {
        // The mount remains accessible to subsequent commands until unmounted or rebooted.
        // 后续命令可访问此挂载，直至解除挂载或重启。
        self.retained = true;
    }
}

impl Drop for TopLevel {
    fn drop(&mut self) {
        if self.retained {
            return;
        }
        let unmounted = Command::new("umount")
            .arg("--")
            .arg(self.path())
            .status()
            .is_ok_and(|status| status.success());
        if unmounted {
            let _ = std::fs::remove_dir(self.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mount(target: &str, fsroot: &str, uuid: &str, options: &str) -> Mount {
        Mount {
            target: target.into(),
            fsroot: fsroot.into(),
            fstype: "btrfs".into(),
            uuid: Some(uuid.into()),
            options: options.into(),
        }
    }

    #[test]
    fn snapshot_root_selects_same_filesystem_top_level() {
        let roots = [mount(
            "/",
            "/snapshots/booted/root",
            "ab-12",
            "rw,subvolid=339",
        )];
        let mounts = [
            mount("/mnt/other", "/", "cd-34", "rw,subvolid=5"),
            mount("/", "/snapshots/booted/root", "ab-12", "rw,subvolid=339"),
            mount("/home", "/home", "ab-12", "rw,subvolid=257"),
            mount("/mnt/disk root", "/", "ab-12", "rw,subvolid=5"),
        ];
        assert_eq!(
            writable_top_level(&mounts, system_uuid(&roots).unwrap()),
            Some(Path::new("/mnt/disk root"))
        );
    }

    #[test]
    fn filesystem_root_may_itself_be_the_top_level() {
        let mounts = [mount("/", "/", "ab-12", "rw,subvolid=5")];
        assert_eq!(
            writable_top_level(&mounts, system_uuid(&mounts).unwrap()),
            Some(Path::new("/"))
        );
    }

    #[test]
    fn missing_top_level_and_read_only_views_require_a_new_mount() {
        let mounts = [
            mount("/", "/@", "ab-12", "rw,subvolid=256"),
            mount("/mnt/readonly", "/", "ab-12", "ro,subvolid=5"),
            mount("/mnt/other", "/", "cd-34", "rw,subvolid=5"),
        ];
        assert!(writable_top_level(&mounts, "ab-12").is_none());
    }

    #[test]
    fn refuses_ambiguous_non_btrfs_or_unknown_root_filesystems() {
        assert!(system_uuid(&[]).is_err());
        assert!(
            system_uuid(&[
                mount("/", "/", "ab-12", "rw"),
                mount("/", "/", "ab-12", "rw")
            ])
            .is_err()
        );
        let mut root = mount("/", "/", "ab-12", "rw");
        root.fstype = "ext4".into();
        assert!(system_uuid(&[root]).is_err());
        for uuid in ["", "../../dev/sda", "not-a-uuid"] {
            assert!(system_uuid(&[mount("/", "/", uuid, "rw")]).is_err());
        }
        let mut root = mount("/", "/", "ab-12", "rw");
        root.uuid = None;
        assert!(system_uuid(&[root]).is_err());
    }
}
