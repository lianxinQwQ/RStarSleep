//! Publish main-system Type #1 systemd-boot entries after successful root creation.
//! 系统根创建成功后发布主系统的 systemd-boot Type #1 启动项。
use super::{BuildOutcome, Error, Result, fs, mounts, plugins};
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
    /// Exact copied artifacts; older records use the legacy filename allowlist.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub files: Option<Vec<String>>,
}

/// Names are inserted into boot entries and must identify one plain child file.
pub(super) fn artifact_name(name: &str) -> bool {
    !name.is_empty()
        && name.as_bytes()[0].is_ascii_alphanumeric()
        && !name.ends_with('.')
        && name
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-'))
}

/// Read the checked declaration retained with this version, independently of plugin updates.
fn boot_spec(outcome: &BuildOutcome) -> Result<plugins::BootRequirements> {
    let path = outcome
        .root
        .parent()
        .ok_or_else(|| {
            Error::Invalid("启动根缺少版本目录 / Boot root has no version directory".into())
        })?
        .join("boot-spec.toml");
    fs::checked(&path)?;
    let text = disk::read_to_string(&path).map_err(|e| fs::io(&path, e))?;
    let spec: plugins::BootRequirements = toml::from_str(&text).map_err(|e| {
        Error::Invalid(format!(
            "启动产物声明无效 / Invalid boot artifact declaration: {e}"
        ))
    })?;
    plugins::validate_boot(&spec)?;
    Ok(spec)
}

/// Resolve the completed root mount and publish its kernel, initramfs and available microcode.
/// 解析完成根的实际挂载，并发布内核、initramfs 及存在的微码镜像。
/// # Arguments
/// - `outcome`: Completed version. 完成版本。
/// - `config`: Global boot settings. 全局启动设置。
pub(super) fn publish(outcome: &BuildOutcome, config: &RuntimeConfig) -> Result<()> {
    let mount = mounts::covering(&outcome.root)?;
    let uuid = mount
        .uuid
        .as_deref()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| Error::Invalid("Btrfs 根缺少 UUID / Btrfs root has no UUID".into()))?;
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
    let spec = boot_spec(outcome)?;
    if uuid.chars().any(|c| !c.is_ascii_alphanumeric() && c != '-') || uuid.is_empty() {
        return Err(Error::Invalid(
            "无效 Btrfs UUID / Invalid Btrfs UUID".into(),
        ));
    }
    let subvolume = subvolume
        .to_str()
        .filter(|p| {
            !p.chars()
                .any(|c| c.is_whitespace() || c.is_control() || c == ',')
        })
        .ok_or_else(|| Error::Invalid("启动子卷路径不能包含空白、逗号或非 UTF-8 / Boot subvolume path must not contain whitespace, commas, or non-UTF-8 characters".into()))?;
    let pending = outcome
        .root
        .file_name()
        .is_some_and(|s| s == "prepared-root");
    let mut options = config.kernel_options.clone();
    let root_subvolume = if pending {
        let parent_path = Path::new(subvolume).parent().ok_or_else(|| {
            Error::Invalid(
                "待继承启动根缺少版本目录 / Boot root pending inheritance has no version directory"
                    .into(),
            )
        })?;
        let manifest = parent_path.join("inheritance-pending.toml");
        if !fs::exists(
            &outcome
                .root
                .parent()
                .unwrap()
                .join("inheritance-pending.toml"),
        )? {
            return Err(Error::Invalid(
                "候选启动根缺少继承清单 / Candidate boot root has no inheritance manifest".into(),
            ));
        }
        options.push(format!("rstarsleep.storage=UUID={uuid}"));
        options.push(format!("rstarsleep.pending={}", manifest.display()));
        parent_path.join("root")
    } else {
        PathBuf::from(subvolume)
    };
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
                "启动目标已存在: {0} / Boot target already exists: {0}",
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
    let mut initrds = String::new();
    let mut kernel = String::new();
    let mut files = Vec::new();
    for artifact in &spec.artifacts {
        let name = &artifact.destination;
        let source = outcome.root.join(&artifact.source);
        if artifact.optional && !fs::exists(&source)? {
            continue;
        }
        fs::checked(&source)?;
        if !disk::symlink_metadata(&source)
            .map_err(|e| fs::io(&source, e))?
            .is_file()
        {
            return Err(Error::Invalid(format!(
                "启动文件必须为普通文件: {0} / Boot file must be a regular file: {0}",
                source.display()
            )));
        }
        let target = staging.path().join(name);
        disk::copy(&source, &target).map_err(|e| fs::io(&target, e))?;
        disk::File::open(&target)
            .and_then(|f| f.sync_all())
            .map_err(|e| fs::io(&target, e))?;
        files.push(name.clone());
        match artifact.kind {
            plugins::BootArtifactKind::Initrd => {
                initrds.push_str(&format!("initrd /RStarSleep/{}/{name}\n", outcome.version));
            }
            plugins::BootArtifactKind::Kernel => kernel.clone_from(name),
        }
    }
    let record = BootRecord {
        system_id: outcome.system_id.clone(),
        version: outcome.version.clone(),
        directory: directory.clone(),
        entry: entry.clone(),
        files: Some(files),
    };
    // Persist ownership before external publication so interrupted attempts remain cleanable.
    // 外部发布前保存所有权，使中断的尝试仍可清理。
    fs::write(&outcome.root.parent().unwrap().join("boot.toml"), &record)?;
    disk::rename(staging.path(), &directory).map_err(|e| fs::io(&directory, e))?;
    fs::sync(&parent)?;
    let text = format!(
        "title RStarSleep {} {}\nlinux /RStarSleep/{}/{kernel}\n{initrds}options root=UUID={uuid} rootflags=subvol={} rw {}\n",
        outcome.system_id,
        outcome.version,
        outcome.version,
        root_subvolume.to_str().unwrap().trim_start_matches('/'),
        options.join(" ")
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
            return Err(Error::Invalid(
                "管理器二进制目标必须为普通文件 / Manager binary target must be a regular file"
                    .into(),
            ));
        }
        if disk::read(&destination).map_err(|e| fs::io(&destination, e))? == bytes {
            let permissions = disk::metadata(&source)
                .map_err(|e| fs::io(&source, e))?
                .permissions();
            if disk::metadata(&destination)
                .map_err(|e| fs::io(&destination, e))?
                .permissions()
                != permissions
            {
                disk::set_permissions(&destination, permissions)
                    .map_err(|e| fs::io(&destination, e))?;
            }
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

    fn write_arch_spec(outcome: &BuildOutcome) {
        let mut artifacts = Vec::new();
        for (source, destination, kind, optional) in [
            (
                "boot/intel-ucode.img",
                "intel-ucode.img",
                plugins::BootArtifactKind::Initrd,
                true,
            ),
            (
                "boot/amd-ucode.img",
                "amd-ucode.img",
                plugins::BootArtifactKind::Initrd,
                true,
            ),
            (
                "boot/initramfs-linux.img",
                "initramfs-linux.img",
                plugins::BootArtifactKind::Initrd,
                false,
            ),
            (
                "boot/vmlinuz-linux",
                "vmlinuz",
                plugins::BootArtifactKind::Kernel,
                false,
            ),
        ] {
            artifacts.push(plugins::BootArtifact {
                source: source.into(),
                destination: destination.into(),
                kind,
                optional,
            });
        }
        fs::write(
            &outcome.root.parent().unwrap().join("boot-spec.toml"),
            &plugins::BootRequirements {
                packages: vec!["linux".into()],
                artifacts,
            },
        )
        .unwrap();
    }

    #[test]
    fn publishes_available_microcode_before_the_main_initramfs() {
        let cases: &[&[&str]] = &[
            &[],
            &["intel-ucode.img"],
            &["amd-ucode.img"],
            &["intel-ucode.img", "amd-ucode.img"],
        ];
        for &microcode in cases {
            let temporary = tempfile::tempdir().unwrap();
            let root = temporary.path().join("versions/v00000001/root");
            disk::create_dir_all(root.join("boot")).unwrap();
            disk::write(root.join("boot/vmlinuz-linux"), "kernel").unwrap();
            disk::write(root.join("boot/initramfs-linux.img"), "initramfs").unwrap();
            for &name in microcode {
                disk::write(root.join("boot").join(name), name).unwrap();
            }
            let outcome = BuildOutcome {
                system_id: "desktop".into(),
                version: "v00000001".into(),
                root,
            };
            write_arch_spec(&outcome);
            let config = RuntimeConfig {
                schema_version: 1,
                main_system: Some("desktop".into()),
                subsystems: vec![],
                boot_dir: temporary.path().join("esp"),
                kernel_options: vec![],
            };
            publish_at(&outcome, &config, "abc-123", Path::new("/root")).unwrap();
            let text = disk::read_to_string(
                config
                    .boot_dir
                    .join("loader/entries/RStarSleep-desktop-v00000001.conf"),
            )
            .unwrap();
            let expected: Vec<_> = microcode
                .iter()
                .copied()
                .chain(["initramfs-linux.img"])
                .map(|name| format!("initrd /RStarSleep/v00000001/{name}"))
                .collect();
            assert_eq!(
                text.lines()
                    .filter(|line| line.starts_with("initrd "))
                    .collect::<Vec<_>>(),
                expected
            );
            let published = config.boot_dir.join("RStarSleep/v00000001");
            for name in ["intel-ucode.img", "amd-ucode.img"] {
                assert_eq!(published.join(name).exists(), microcode.contains(&name));
                if microcode.contains(&name) {
                    assert_eq!(disk::read(published.join(name)).unwrap(), name.as_bytes());
                }
            }
        }
    }

    #[test]
    fn pending_entry_loads_prepared_artifacts_but_mounts_only_the_final_root() {
        let directory = tempfile::tempdir().unwrap();
        let prepared = directory.path().join("versions/v00000001/prepared-root");
        disk::create_dir_all(prepared.join("boot")).unwrap();
        disk::write(prepared.join("boot/vmlinuz-linux"), "kernel").unwrap();
        disk::write(prepared.join("boot/initramfs-linux.img"), "early-runtime").unwrap();
        disk::write(
            prepared.parent().unwrap().join("inheritance-pending.toml"),
            "pending",
        )
        .unwrap();
        let config = RuntimeConfig {
            schema_version: 1,
            main_system: Some("desktop".into()),
            subsystems: vec![],
            boot_dir: directory.path().join("esp"),
            kernel_options: vec![],
        };
        let outcome = BuildOutcome {
            system_id: "desktop".into(),
            version: "v00000001".into(),
            root: prepared,
        };
        write_arch_spec(&outcome);
        publish_at(
            &outcome,
            &config,
            "abc-123",
            Path::new("/@RStarSleep/systems/desktop/versions/v00000001/prepared-root"),
        )
        .unwrap();
        let text = disk::read_to_string(
            config
                .boot_dir
                .join("loader/entries/RStarSleep-desktop-v00000001.conf"),
        )
        .unwrap();
        assert!(
            text.contains(
                "rootflags=subvol=@RStarSleep/systems/desktop/versions/v00000001/root rw"
            )
        );
        assert!(text.contains("rstarsleep.storage=UUID=abc-123"));
        assert!(text.contains("rstarsleep.pending=/@RStarSleep/systems/desktop/versions/v00000001/inheritance-pending.toml"));
        assert!(!text.contains("prepared-root"));
    }

    /// Publish exact kernel paths and reject collisions, missing or invalid artifacts.
    /// 发布准确内核路径，拒绝冲突、缺失或无效产物且不生成启动项。
    /// # Arguments
    /// No parameters. 无参数。
    #[test]
    fn publishes_complete_entries_and_rejects_missing_or_invalid_artifacts() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("versions/v00000001/root");
        disk::create_dir_all(root.join("boot")).unwrap();
        disk::write(root.join("boot/vmlinuz-linux"), "kernel").unwrap();
        let outcome = BuildOutcome {
            system_id: "desktop".into(),
            version: "v00000001".into(),
            root: root.clone(),
        };
        write_arch_spec(&outcome);
        let config = RuntimeConfig {
            schema_version: 1,
            main_system: Some("desktop".into()),
            subsystems: vec![],
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
        for name in ["intel-ucode.img", "amd-ucode.img"] {
            let source = root.join("boot").join(name);
            for link in [false, true] {
                if link {
                    std::os::unix::fs::symlink("missing-microcode.img", &source).unwrap();
                } else {
                    disk::create_dir(&source).unwrap();
                }
                assert!(publish_at(&outcome, &config, "abc-123", Path::new("/root")).is_err());
                assert!(!entry.exists());
                assert!(!config.boot_dir.join("RStarSleep/v00000001").exists());
                assert!(!root.parent().unwrap().join("boot.toml").exists());
                if link {
                    disk::remove_file(&source).unwrap();
                } else {
                    disk::remove_dir(&source).unwrap();
                }
            }
        }
        publish_at(
            &outcome,
            &config,
            "abc-123",
            Path::new("/@RStarSleep/systems/desktop/versions/v00000001/root"),
        )
        .unwrap();
        let text = disk::read_to_string(&entry).unwrap();
        assert!(text.contains("root=UUID=abc-123 rootflags=subvol=@RStarSleep/systems/desktop/versions/v00000001/root rw quiet"));
        assert_eq!(
            disk::read(config.boot_dir.join("RStarSleep/v00000001/vmlinuz")).unwrap(),
            b"kernel"
        );
        assert!(publish_at(&outcome, &config, "abc-123", Path::new("/root")).is_err());
        assert!(root.parent().unwrap().join("boot.toml").exists());
    }

    #[test]
    fn publishes_declared_distribution_paths_and_retains_only_copied_artifacts() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("versions/v00000001/root");
        disk::create_dir_all(root.join("usr/lib/kernel")).unwrap();
        disk::write(root.join("usr/lib/kernel/image"), "kernel").unwrap();
        disk::write(root.join("usr/lib/kernel/early"), "early").unwrap();
        disk::write(root.join("usr/lib/kernel/main"), "main").unwrap();
        let outcome = BuildOutcome {
            system_id: "desktop".into(),
            version: "v00000001".into(),
            root,
        };
        fs::write_text(
            &outcome.root.parent().unwrap().join("boot-spec.toml"),
            r#"packages = ["kernel-custom"]
[[artifacts]]
source = "usr/lib/kernel/early"
destination = "early.img"
kind = "initrd"
optional = true
[[artifacts]]
source = "usr/lib/kernel/missing"
destination = "optional.img"
kind = "initrd"
optional = true
[[artifacts]]
source = "usr/lib/kernel/image"
destination = "custom-kernel"
kind = "kernel"
optional = false
[[artifacts]]
source = "usr/lib/kernel/main"
destination = "custom-initrd"
kind = "initrd"
optional = false
"#,
        )
        .unwrap();
        let config = RuntimeConfig {
            schema_version: 1,
            main_system: Some("desktop".into()),
            subsystems: vec![],
            boot_dir: temporary.path().join("esp"),
            kernel_options: vec![],
        };
        publish_at(&outcome, &config, "abc-123", Path::new("/root")).unwrap();
        let entry = disk::read_to_string(
            config
                .boot_dir
                .join("loader/entries/RStarSleep-desktop-v00000001.conf"),
        )
        .unwrap();
        assert!(entry.contains("linux /RStarSleep/v00000001/custom-kernel\n"));
        assert_eq!(
            entry
                .lines()
                .filter(|line| line.starts_with("initrd "))
                .collect::<Vec<_>>(),
            [
                "initrd /RStarSleep/v00000001/early.img",
                "initrd /RStarSleep/v00000001/custom-initrd",
            ]
        );
        let record: BootRecord = toml::from_str(
            &disk::read_to_string(outcome.root.parent().unwrap().join("boot.toml")).unwrap(),
        )
        .unwrap();
        assert_eq!(
            record.files.unwrap(),
            ["early.img", "custom-kernel", "custom-initrd"]
        );
        assert!(
            !config
                .boot_dir
                .join("RStarSleep/v00000001/optional.img")
                .exists()
        );
    }

    #[test]
    fn rejects_tampered_retained_declarations_before_publication() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("versions/v00000001/root");
        disk::create_dir_all(&root).unwrap();
        let outcome = BuildOutcome {
            system_id: "desktop".into(),
            version: "v00000001".into(),
            root,
        };
        write_arch_spec(&outcome);
        let valid = boot_spec(&outcome).unwrap();
        let config = RuntimeConfig {
            schema_version: 1,
            main_system: Some("desktop".into()),
            subsystems: vec![],
            boot_dir: temporary.path().join("esp"),
            kernel_options: vec![],
        };
        for (source, destination) in [
            ("../outside", "safe.img"),
            ("boot/./image", "safe.img"),
            ("boot//image", "safe.img"),
            ("boot/image", "../outside"),
            ("boot/image", "x\nlinux"),
            ("boot/image", "VMLINUZ"),
        ] {
            let mut altered = valid.clone();
            altered.artifacts[0].source = source.into();
            altered.artifacts[0].destination = destination.into();
            fs::write(
                &outcome.root.parent().unwrap().join("boot-spec.toml"),
                &altered,
            )
            .unwrap();
            assert!(publish_at(&outcome, &config, "abc-123", Path::new("/root")).is_err());
            assert!(!config.boot_dir.exists());
        }
    }
}
