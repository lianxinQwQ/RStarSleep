//! Inspect Linux mounts without interpreting shell output or escaped whitespace.
//! 检查 Linux 挂载信息，不解析 shell 输出或被转义的空白。
use super::{Error, Result, fs};
use serde::Deserialize;
use std::{
    path::{Path, PathBuf},
    process::Command,
};

/// One mount covering a requested absolute path.
/// 覆盖所请求绝对路径的挂载记录。
#[derive(Debug, Clone, Deserialize)]
pub(super) struct Mount {
    pub target: PathBuf,
    pub fstype: String,
    pub uuid: Option<String>,
    pub fsroot: PathBuf,
}
#[derive(Deserialize)]
struct Mounts {
    filesystems: Vec<Mount>,
}

/// Query the covering mount with a stable structured findmnt response.
/// 通过稳定的 findmnt 结构化响应查询覆盖挂载。
/// # Arguments
/// - `path`: Existing host path. 已有宿主路径。
pub(super) fn covering(path: &Path) -> Result<Mount> {
    let output = Command::new("findmnt")
        .args([
            "--json",
            "--output",
            "TARGET,SOURCE,FSTYPE,OPTIONS,UUID,FSROOT",
            "--target",
        ])
        .arg(path)
        .output()
        .map_err(|e| fs::io(Path::new("findmnt"), e))?;
    if !output.status.success() {
        return Err(Error::Invalid(format!(
            "无法查询挂载 {}: {}",
            path.display(),
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    let mut mounts: Mounts = serde_json::from_slice(&output.stdout)
        .map_err(|e| Error::Invalid(format!("findmnt 响应无效: {e}")))?;
    if mounts.filesystems.len() != 1 {
        return Err(Error::Invalid("findmnt 必须返回唯一覆盖挂载".into()));
    }
    Ok(mounts.filesystems.remove(0))
}

impl Mount {
    /// Resolve a host path to its Btrfs top-level path.
    /// 将宿主路径解析为 Btrfs 顶层路径。
    /// # Arguments
    /// - `self`: Covering Btrfs mount. 覆盖的 Btrfs 挂载。
    /// - `path`: Path below this mount. 此挂载下的路径。
    pub fn subvolume_path(&self, path: &Path) -> Result<PathBuf> {
        if self.fstype != "btrfs" {
            return Err(Error::Invalid("系统根必须位于 Btrfs".into()));
        }
        let relative = path
            .strip_prefix(&self.target)
            .map_err(|e| Error::Invalid(e.to_string()))?;
        Ok(if relative.as_os_str().is_empty() {
            self.fsroot.clone()
        } else {
            self.fsroot.join(relative)
        })
    }
}

/// Reject mounted descendants which ordinary snapshots cannot faithfully inherit.
/// 拒绝普通快照无法准确继承的后代挂载。
/// # Arguments
/// - `source`: File or directory in the current system. 当前系统中的文件或目录。
pub(super) fn check_descendant_mounts(source: &Path) -> Result<()> {
    check_mount_tree(source, false)
}

/// Reject any mounted cleanup target or descendant.
/// 拒绝已挂载的清理目标或后代。
/// # Arguments
/// - `source`: Owned deletion target. 所属删除目标。
pub(super) fn check_unmounted_tree(source: &Path) -> Result<()> {
    check_mount_tree(source, true)
}

/// Check mount boundaries before recursively reading or deleting paths.
/// 递归读取或删除路径前检查挂载边界。
/// # Arguments
/// - `source`: Tree to inspect. 待检查树。
/// - `include_self`: Also reject a mounted tree root. 也拒绝挂载的树根。
fn check_mount_tree(source: &Path, include_self: bool) -> Result<()> {
    let text = std::fs::read_to_string("/proc/self/mountinfo")
        .map_err(|e| fs::io(Path::new("/proc/self/mountinfo"), e))?;
    for line in text.lines() {
        let target = line
            .split_whitespace()
            .nth(4)
            .ok_or_else(|| Error::Invalid("mountinfo 响应无效".into()))?;
        let target = target
            .replace("\\040", " ")
            .replace("\\011", "\t")
            .replace("\\012", "\n")
            .replace("\\134", "\\");
        let target = Path::new(&target);
        if (include_self || target != source) && target.starts_with(source) {
            return Err(Error::Invalid(format!(
                "继承来源包含挂载点: {}",
                target.display()
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bound_file_locations_do_not_gain_a_trailing_directory_separator() {
        let mount = Mount {
            target: "/etc/settings".into(),
            fstype: "btrfs".into(),
            uuid: Some("uuid".into()),
            fsroot: "/persistent/settings".into(),
        };
        assert_eq!(
            mount
                .subvolume_path(Path::new("/etc/settings"))
                .unwrap()
                .as_os_str(),
            std::ffi::OsStr::new("/persistent/settings")
        );
    }
}
