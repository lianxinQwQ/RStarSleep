//! Inherit current-system data after scripts, with explicit snapshots for directory subvolumes.
//! 脚本后继承当前系统数据，显式为目录子卷建立快照。
use super::{BuildBackend, BuildContext, Error, Result, fs as disk, plugins::LoadedPlugin};
use crate::config::{DEFAULT_MOUNT_POINT, InheritanceConfig, inheritance_name};
use std::{
    fs,
    path::{Path, PathBuf},
};

/// Relative directory subvolumes requiring recursive publication.
/// 需要递归发布的相对目录子卷。
pub(super) struct Inherited {
    pub directories: Vec<PathBuf>,
    pub mounts: Vec<super::PersistentMount>,
}

/// Apply files, independent directory snapshots and persistent mount declarations to a fresh root.
/// 向全新根应用文件、独立目录快照及持久挂载声明。
/// # Arguments
/// - `backend`: CoW and plugin backend. CoW 及插件后端。
/// - `plugin`: Selected distribution plugin. 所选发行版插件。
/// - `context`: Frozen current build inputs. 冻结的当前构建输入。
/// - `root`: Writable post-script root. 脚本后的可写根。
/// - `scratch`: Current attempt scratch directory. 当前尝试的临时目录。
/// - `system`: Owning managed-system directory. 所属被管理系统目录。
/// - `owned`: Temporary subvolumes tracked for failure cleanup. 为失败清理跟踪的临时子卷。
pub(super) fn apply(
    backend: &impl BuildBackend,
    plugin: &LoadedPlugin,
    context: &BuildContext,
    root: &Path,
    scratch: &Path,
    system: &Path,
    owned: &mut Vec<PathBuf>,
) -> Result<Inherited> {
    let parameters = crate::config::read_plugin_parameters_at(&context.config)?;
    let config = InheritanceConfig::read(&parameters)?;
    let mut directories = Vec::new();
    for source in &config.copy_on_write {
        check_source(source, system)?;
        backend.check_cow_source(source, scratch)?;
        let relative = source
            .strip_prefix("/")
            .map_err(|e| Error::Invalid(e.to_string()))?;
        let target = root.join(relative);
        let info = fs::symlink_metadata(source).map_err(|e| disk::io(source, e))?;
        replace_target(backend, &target)?;
        if info.is_dir() {
            let prepared = scratch.join(format!("inherit-{}", inheritance_name(source)?));
            owned.push(prepared.clone());
            backend.create_subvolume(&prepared)?;
            backend.copy_inherited(source, &prepared, true)?;
            backend.snapshot(&prepared, &target, false)?;
            directories.push(relative.to_path_buf());
        } else if info.is_file() {
            backend.copy_inherited(source, &target, true)?;
        } else {
            return Err(Error::Invalid(format!(
                "继承来源必须为普通文件或目录: {}",
                source.display()
            )));
        }
    }
    let mut arguments = Vec::new();
    let mut inherited_mounts = Vec::new();
    let pool = system.join("persistent");
    disk::directory(&pool)?;
    let manager = system
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| Error::Invalid("系统缺少管理根".into()))?;
    let mut needs_manager_mount = false;
    for source in &config.mounts {
        let persistent = pool.join(inheritance_name(source)?);
        if !disk::exists(&persistent)? {
            check_source(source, system)?;
            if backend.check_subvolume(source, false).is_ok()
                || backend.check_subvolume(source, true).is_ok()
            {
                eprintln!(
                    "Warning: Mount 继承来源本身是子卷，使用完整复制 {}",
                    source.display()
                );
            }
            if let Err(error) = backend.check_cow_source(source, &pool) {
                eprintln!(
                    "Warning: Mount 继承允许跨分区、子卷及内部挂载点，使用完整复制 {}: {error}",
                    source.display()
                );
            }
            seed_persistent(backend, source, &persistent, &pool)?;
        }
        let info = fs::symlink_metadata(&persistent).map_err(|e| disk::io(&persistent, e))?;
        let relative = source
            .strip_prefix("/")
            .map_err(|e| Error::Invalid(e.to_string()))?;
        let target = root.join(relative);
        if info.is_dir() {
            if let Err(error) = backend.check_cow_source(&persistent, &pool) {
                eprintln!(
                    "Warning: 复用含子卷或挂载点的 Mount 持久目录 {}: {error}",
                    persistent.display()
                );
            }
            // Ensure the filesystem has an owned writable subvolume; nested children are allowed.
            // 确保文件系统具有所属可写子卷；允许嵌套后代。
            backend.check_subvolume(&persistent, false)?;
            disk::directory(&target)?;
            let (uuid, subvolume) = backend.filesystem_location(&persistent)?;
            arguments.extend([
                format!("UUID={uuid}"),
                source.to_string_lossy().into_owned(),
                "btrfs".into(),
                subvolume_options(&subvolume, false)?,
            ]);
        } else if info.is_file() {
            disk::checked(&target)?;
            disk::directory(target.parent().unwrap())?;
            if !disk::exists(&target)? {
                fs::File::create(&target).map_err(|e| disk::io(&target, e))?;
            }
            if !fs::symlink_metadata(&target)
                .map_err(|e| disk::io(&target, e))?
                .is_file()
            {
                return Err(Error::Invalid("文件挂载目标必须为普通文件".into()));
            }
            let runtime_source = Path::new(DEFAULT_MOUNT_POINT).join(
                persistent
                    .strip_prefix(manager)
                    .map_err(|e| Error::Invalid(e.to_string()))?,
            );
            arguments.extend([
                runtime_source.to_string_lossy().into_owned(),
                source.to_string_lossy().into_owned(),
                "none".into(),
                format!("bind,x-systemd.requires-mounts-for={DEFAULT_MOUNT_POINT}"),
            ]);
            needs_manager_mount = true;
        } else {
            return Err(Error::Invalid(
                "持久继承项目必须为普通文件或子卷目录".into(),
            ));
        }
        inherited_mounts.push(super::PersistentMount {
            source: persistent,
            target: source.clone(),
        });
    }
    if needs_manager_mount {
        let (uuid, subvolume) = backend.filesystem_location(manager)?;
        disk::directory(&root.join(DEFAULT_MOUNT_POINT.trim_start_matches('/')))?;
        arguments.splice(
            0..0,
            [
                format!("UUID={uuid}"),
                DEFAULT_MOUNT_POINT.into(),
                "btrfs".into(),
                subvolume_options(&subvolume, false)?,
            ],
        );
    }
    if !arguments.is_empty() || context.parameters.contains_key("RSTAR_CONTENT_FSTABLINES") {
        if !plugin.metadata.mount_inheritance {
            return Err(Error::Invalid(format!(
                "插件 {} 未声明 mount_inheritance 接口",
                plugin.metadata.id
            )));
        }
        let mut mount_context = context.clone();
        mount_context.inherited_mounts = inherited_mounts
            .iter()
            .filter(|mount| mount.target != Path::new("/etc/fstab"))
            .cloned()
            .collect();
        // A bind-mounted file cannot be atomically replaced. Edit the root copy of fstab,
        // then synchronize its persistent source after the distribution operation succeeds.
        // 绑定挂载的文件无法原子替换。先编辑根内 fstab 副本，发行版操作成功后同步持久来源。
        for mount in &inherited_mounts {
            if mount.target == Path::new("/etc/fstab") {
                let text =
                    fs::read_to_string(&mount.source).map_err(|e| disk::io(&mount.source, e))?;
                disk::write_text(&root.join("etc/fstab"), &text)?;
            }
        }
        backend.invoke(
            plugin,
            "mount_inheritance",
            &arguments,
            root,
            &mount_context,
        )?;
        for mount in &inherited_mounts {
            if mount.target == Path::new("/etc/fstab") {
                let text = fs::read_to_string(root.join("etc/fstab"))
                    .map_err(|e| disk::io(&root.join("etc/fstab"), e))?;
                disk::write_text(&mount.source, &text)?;
            }
            let fstab = if mount.target == Path::new("/etc") {
                Some(mount.source.join("fstab"))
            } else if mount.target == Path::new("/etc/fstab") {
                Some(mount.source.clone())
            } else {
                None
            };
            if let Some(fstab) = fstab {
                disk::checked(&fstab)?;
                let text = fs::read_to_string(&fstab).map_err(|e| disk::io(&fstab, e))?;
                disk::write_text(&root.join("etc/fstab"), &text)?;
            }
        }
    }
    Ok(Inherited {
        directories,
        mounts: inherited_mounts,
    })
}

/// Encode a Btrfs mount option without allowing option-separator injection.
/// 编码 Btrfs 挂载选项，不允许注入选项分隔符。
/// # Arguments
/// - `subvolume`: Filesystem top-level path. 文件系统顶层路径。
/// - `readonly`: Read-only mount choice. 只读挂载选择。
fn subvolume_options(subvolume: &Path, readonly: bool) -> Result<String> {
    let path = subvolume
        .to_str()
        .ok_or_else(|| Error::Invalid("挂载子卷路径必须为 UTF-8".into()))?;
    if path.contains(',') || path.chars().any(char::is_control) {
        return Err(Error::Invalid("挂载子卷路径不能包含逗号或控制字符".into()));
    }
    Ok(format!(
        "{},subvol={path}",
        if readonly { "ro" } else { "rw" }
    ))
}

/// Reject source paths that would copy manager-owned storage back into itself.
/// 拒绝会将管理器所属存储复制回自身的来源路径。
/// # Arguments
/// - `source`: Current-running-system source. 当前运行系统来源。
/// - `system`: Owning system directory. 所属系统目录。
fn check_source(source: &Path, system: &Path) -> Result<()> {
    disk::checked(source)?;
    let manager = system
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| Error::Invalid("系统缺少管理根".into()))?;
    if source.starts_with(manager) || manager.starts_with(source) {
        return Err(Error::Invalid("继承来源不能与管理器目录重叠".into()));
    }
    let info = fs::symlink_metadata(source).map_err(|e| disk::io(source, e))?;
    if !info.is_dir() && !info.is_file() {
        return Err(Error::Invalid("继承来源必须为普通文件或目录".into()));
    }
    Ok(())
}

/// Replace a target only after checking paths and descendant storage.
/// 检查路径及后代存储后替换目标。
/// # Arguments
/// - `backend`: Checked storage backend. 经检查的存储后端。
/// - `target`: Owned destination inside the writable build root. 可写构建根内的所属目标。
fn replace_target(backend: &impl BuildBackend, target: &Path) -> Result<()> {
    disk::checked(target)?;
    super::mounts::check_unmounted_tree(target)?;
    if disk::exists(target)? {
        let info = fs::symlink_metadata(target).map_err(|e| disk::io(target, e))?;
        if info.is_dir() {
            backend.check_no_nested_subvolumes(target)?;
            fs::remove_dir_all(target).map_err(|e| disk::io(target, e))?;
        } else if info.is_file() {
            fs::remove_file(target).map_err(|e| disk::io(target, e))?;
        } else {
            return Err(Error::Invalid("继承目标必须为普通文件或目录".into()));
        }
    }
    disk::directory(
        target
            .parent()
            .ok_or_else(|| Error::Invalid("继承目标没有父目录".into()))?,
    )
}

/// Seed a missing persistent object atomically; allow full copies and never update existing data.
/// 原子初始化缺失持久对象；允许完整复制且从不更新已有数据。
/// # Arguments
/// - `backend`: Copy and subvolume backend. 复制及子卷后端。
/// - `source`: Existing host file or directory. 已有宿主文件或目录。
/// - `destination`: Encoded persistent object. 编码后的持久对象。
/// - `pool`: System persistent directory. 系统持久目录。
fn seed_persistent(
    backend: &impl BuildBackend,
    source: &Path,
    destination: &Path,
    pool: &Path,
) -> Result<()> {
    let staging = tempfile::Builder::new()
        .prefix(".inherit-pending-")
        .tempdir_in(pool)
        .map_err(|e| disk::io(pool, e))?
        .keep();
    let pending = staging.join("data");
    let directory = source.is_dir();
    let result = (|| {
        if directory {
            backend.create_subvolume(&pending)?;
        }
        backend.copy_inherited(source, &pending, false)?;
        fs::rename(&pending, destination).map_err(|e| disk::io(destination, e))?;
        disk::sync(pool)
    })();
    let cleanup = (|| {
        if disk::exists(&pending)? {
            if directory {
                backend.delete_subvolume_recursive(&pending)?;
            } else {
                fs::remove_file(&pending).map_err(|e| disk::io(&pending, e))?;
            }
        }
        fs::remove_dir(&staging).map_err(|e| disk::io(&staging, e))
    })();
    match (result, cleanup) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(e), Ok(())) | (Ok(()), Err(e)) => Err(e),
        (Err(e), Err(c)) => Err(Error::Invalid(format!("{e}; 持久初始化清理失败: {c}"))),
    }
}

/// Snapshot the parent and explicitly snapshot every inherited directory omitted by Btrfs.
/// 为父根建立快照，并显式快照 Btrfs 会遗漏的每个继承目录。
/// # Arguments
/// - `backend`: Snapshot backend. 快照后端。
/// - `source`: Complete source root. 完整来源根。
/// - `target`: New cache or version root. 新缓存或版本根。
/// - `directories`: Relative inherited directory subvolumes. 相对继承目录子卷。
/// - `children_readonly`: Whether child snapshots are immutable cache data. 子快照是否为不可变缓存数据。
pub(super) fn snapshot_tree(
    backend: &impl BuildBackend,
    source: &Path,
    target: &Path,
    directories: &[PathBuf],
    children_readonly: bool,
) -> Result<()> {
    backend.snapshot(source, target, false)?;
    for relative in directories {
        let child = target.join(relative);
        // A Btrfs snapshot retains an empty stub; the simulation may retain copied contents.
        // Btrfs 快照保留空占位目录；模拟后端可能保留已复制内容。
        if disk::exists(&child)? {
            fs::remove_dir_all(&child).map_err(|e| disk::io(&child, e))?;
        }
        backend.snapshot(&source.join(relative), &child, children_readonly)?;
    }
    backend.set_readonly(target, true)?;
    backend.check_subvolume(target, true)
}
