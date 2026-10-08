//! Publish main-system Type #1 systemd-boot entries after successful root creation.
//! 系统根创建成功后发布主系统的 systemd-boot Type #1 启动项。
use super::{BuildOutcome, Error, Result, fs, mounts};
use crate::config::RuntimeConfig;
use serde::{Deserialize, Serialize};
use std::{
    fs as disk,
    io::Write,
    path::{Path, PathBuf},
};

/// Ownership record retained with a version independently of later global settings.
/// 与版本一起保存的所有权记录，不受后续全局配置变化影响。
#[derive(Debug, Serialize, Deserialize)]
pub(super) struct BootRecord {
    pub system_id: String,
    pub version: String,
    pub directory: PathBuf,
    pub entry: PathBuf,
}

/// Resolve the actual completed root mount and publish its kernel and initramfs.
/// 解析完成根的实际挂载，并发布内核和 initramfs。
/// # Arguments
/// - `outcome`: Completed version. 完成版本。
/// - `config`: Global boot settings. 全局启动设置。
pub(super) fn publish(outcome: &BuildOutcome, config: &RuntimeConfig) -> Result<()> {
    let mount = mounts::covering(&outcome.root)?;
    let uuid = mount
        .uuid
        .as_deref()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| Error::Invalid("Btrfs 根缺少 UUID".into()))?;
    publish_at(outcome, config, uuid, &mount.subvolume_path(&outcome.root)?)
}

/// Install a complete boot directory before atomically exposing its entry.
/// 原子暴露启动项前安装完整启动目录。
/// # Arguments
/// - `outcome`: Completed version. 完成版本。
/// - `config`: Boot destination and extra options. 启动目标和额外选项。
/// - `uuid`: Actual Btrfs UUID. 实际 Btrfs UUID。
/// - `subvolume`: Top-level root path. 顶层根路径。
fn publish_at(
    outcome: &BuildOutcome,
    config: &RuntimeConfig,
    uuid: &str,
    subvolume: &Path,
) -> Result<()> {
    if uuid.chars().any(|c| !c.is_ascii_alphanumeric() && c != '-') || uuid.is_empty() {
        return Err(Error::Invalid("无效 Btrfs UUID".into()));
    }
    let subvolume = subvolume
        .to_str()
        .filter(|p| {
            !p.chars()
                .any(|c| c.is_whitespace() || c.is_control() || c == ',')
        })
        .ok_or_else(|| Error::Invalid("启动子卷路径不能包含空白、逗号或非 UTF-8".into()))?;
    let parent = config.boot_dir.join("RStarSleep");
    let directory = parent.join(&outcome.version);
    let entries = config.boot_dir.join("loader/entries");
    let entry = entries.join(format!(
        "RStarSleep-{}-{}.conf",
        outcome.system_id, outcome.version
    ));
    fs::checked(&outcome.root)?;
    for path in [&directory, &entry] {
        if fs::exists(path)? {
            return Err(Error::Invalid(format!(
                "启动目标已存在: {}",
                path.display()
            )));
        }
    }
    fs::directory(&parent)?;
    fs::directory(&entries)?;
    let staging = tempfile::Builder::new()
        .prefix(".pending-")
        .tempdir_in(&parent)
        .map_err(|e| fs::io(&parent, e))?;
    for (source, name) in [
        ("boot/initramfs-linux.img", "initramfs-linux.img"),
        ("boot/vmlinuz-linux", "vmlinuz"),
    ] {
        let source = outcome.root.join(source);
        fs::checked(&source)?;
        if !disk::symlink_metadata(&source)
            .map_err(|e| fs::io(&source, e))?
            .is_file()
        {
            return Err(Error::Invalid(format!(
                "启动文件必须为普通文件: {}",
                source.display()
            )));
        }
        let target = staging.path().join(name);
        disk::copy(&source, &target).map_err(|e| fs::io(&target, e))?;
        disk::File::open(&target)
            .and_then(|f| f.sync_all())
            .map_err(|e| fs::io(&target, e))?;
    }
    let record = BootRecord {
        system_id: outcome.system_id.clone(),
        version: outcome.version.clone(),
        directory: directory.clone(),
        entry: entry.clone(),
    };
    // Persist ownership before external publication so interrupted attempts remain cleanable.
    // 外部发布前保存所有权，使中断的尝试仍可清理。
    fs::write(&outcome.root.parent().unwrap().join("boot.toml"), &record)?;
    disk::rename(staging.path(), &directory).map_err(|e| fs::io(&directory, e))?;
    fs::sync(&parent)?;
    let text = format!(
        "title RStarSleep {} {}\nlinux /RStarSleep/{}/vmlinuz\ninitrd /RStarSleep/{}/initramfs-linux.img\noptions root=UUID={uuid} rootflags=subvol={} ro {}\n",
        outcome.system_id,
        outcome.version,
        outcome.version,
        outcome.version,
        subvolume.trim_start_matches('/'),
        config.kernel_options.join(" ")
    );
    if let Err(error) = fs::write_text(&entry, &text) {
        disk::remove_dir_all(&directory).map_err(|e| fs::io(&directory, e))?;
        return Err(error);
    }
    Ok(())
}

/// Synchronize the running manager binary, preserving equal content and executable permissions.
/// 同步正在运行的管理器二进制，保留一致内容和可执行权限。
/// # Arguments
/// - `root`: Manager subvolume root. 管理器子卷根。
pub(super) fn sync_executable(root: &Path) -> Result<()> {
    let source = std::env::current_exe().map_err(|e| fs::io(Path::new("RStarSleep"), e))?;
    let destination = root.join("RStarSleep");
    fs::checked(&destination)?;
    let bytes = disk::read(&source).map_err(|e| fs::io(&source, e))?;
    if fs::exists(&destination)? {
        if !disk::symlink_metadata(&destination)
            .map_err(|e| fs::io(&destination, e))?
            .is_file()
        {
            return Err(Error::Invalid("管理器二进制目标必须为普通文件".into()));
        }
        if disk::read(&destination).map_err(|e| fs::io(&destination, e))? == bytes {
            return Ok(());
        }
    }
    let mut file = tempfile::NamedTempFile::new_in(root).map_err(|e| fs::io(root, e))?;
    file.write_all(&bytes)
        .map_err(|e| fs::io(&destination, e))?;
    file.as_file()
        .set_permissions(
            disk::metadata(&source)
                .map_err(|e| fs::io(&source, e))?
                .permissions(),
        )
        .map_err(|e| fs::io(&destination, e))?;
    file.as_file()
        .sync_all()
        .map_err(|e| fs::io(&destination, e))?;
    file.persist(&destination)
        .map_err(|e| fs::io(&destination, e.error))?;
    fs::sync(root)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Publish exact kernel paths and reject collisions or missing artifacts without a boot entry.
    /// 发布准确内核路径，拒绝冲突或缺失产物且不生成启动项。
    /// # Arguments
    /// No parameters. 无参数。
    #[test]
    fn publishes_complete_entries_and_never_exposes_missing_artifacts() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("versions/v00000001/root");
        disk::create_dir_all(root.join("boot")).unwrap();
        disk::write(root.join("boot/vmlinuz-linux"), "kernel").unwrap();
        let outcome = BuildOutcome {
            system_id: "desktop".into(),
            version: "v00000001".into(),
            root: root.clone(),
        };
        let config = RuntimeConfig {
            schema_version: 1,
            main_system: Some("desktop".into()),
            boot_dir: temporary.path().join("esp"),
            kernel_options: vec!["quiet".into()],
        };
        assert!(
            publish_at(
                &outcome,
                &config,
                "abc-123",
                Path::new("/@RStarSleep/systems/desktop/versions/v00000001/root")
            )
            .is_err()
        );
        let entry = config
            .boot_dir
            .join("loader/entries/RStarSleep-desktop-v00000001.conf");
        assert!(!entry.exists());
        disk::write(root.join("boot/initramfs-linux.img"), "initramfs").unwrap();
        publish_at(
            &outcome,
            &config,
            "abc-123",
            Path::new("/@RStarSleep/systems/desktop/versions/v00000001/root"),
        )
        .unwrap();
        let text = disk::read_to_string(&entry).unwrap();
        assert!(text.contains("root=UUID=abc-123 rootflags=subvol=@RStarSleep/systems/desktop/versions/v00000001/root ro quiet"));
        assert_eq!(
            disk::read(config.boot_dir.join("RStarSleep/v00000001/vmlinuz")).unwrap(),
            b"kernel"
        );
        assert!(publish_at(&outcome, &config, "abc-123", Path::new("/root")).is_err());
        assert!(root.parent().unwrap().join("boot.toml").exists());
    }
}
