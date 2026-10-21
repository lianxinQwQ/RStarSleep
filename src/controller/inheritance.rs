//! Inherit build-time data before scripts and prepare persistent mounts after scripts.
//! 脚本前继承构建时数据，脚本后准备持久挂载。
use super::{
    BuildBackend, BuildContext, Error, Result, fs as disk, plugins::LoadedPlugin, progress,
};
use crate::config::{
    DEFAULT_MOUNT_POINT, InheritanceConfig, NocowHandling, inheritance_name,
    validate_inheritance_type,
};
use std::{
    fs,
    path::{Path, PathBuf},
};

/// Persistent bindings used during the final build-time package query.
/// 构建阶段最终包查询所用的持久绑定。
pub(super) struct Inherited {
    pub mounts: Vec<super::PersistentMount>,
}

/// Inherit live data into a writable software-cache copy before hooks and scripts.
/// 在后处理及脚本前，向软件缓存的可写副本继承运行数据。
/// # Arguments
/// - `backend`: CoW and plugin backend. CoW 及插件后端。
/// - `context`: Frozen current build inputs. 冻结的当前构建输入。
/// - `root`: Writable pre-script root. 脚本前的可写根。
/// - `scratch`: Current attempt scratch directory. 当前尝试的临时目录。
/// - `system`: Owning managed-system directory. 所属被管理系统目录。
pub(super) fn apply_build(
    backend: &impl BuildBackend,
    context: &BuildContext,
    root: &Path,
    scratch: &Path,
    system: &Path,
) -> Result<bool> {
    let parameters = crate::config::read_plugin_parameters_at(&context.config)?;
    let config = InheritanceConfig::read(&parameters)?;
    let manager = system
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| Error::Invalid("继承缺少管理根".into()))?;
    let _migration_lock = if config.nocow_handling == NocowHandling::MoveSnapshot {
        Some(super::records::lock_inheritance(manager)?)
    } else {
        None
    };
    for source in &config.copy_on_write_build {
        progress::message(
            &context.system_id,
            format!("继承写时复制路径：{}", source.display()),
            format!("Inherit copy-on-write path: {}", source.display()),
        );
        let unmarked = Path::new(source.to_str().unwrap().trim_end_matches('/'));
        let resolved = fs::canonicalize(unmarked).map_err(|e| disk::io(source, e))?;
        let directory = check_source(&resolved, system)?;
        validate_inheritance_type(source, directory)?;
        backend.check_cow_source(&resolved, scratch)?;
        if directory && config.nocow_handling == NocowHandling::MoveSnapshot {
            super::inheritance_migration::prepare(backend, &resolved, manager, "build", None)?;
        }
        inherit_path(
            backend,
            &resolved,
            &disk::guest_path(root, source)?,
            config.directory_subvolume,
            config.nocow_handling,
        )?;
    }
    Ok(!config.copy_on_write_build.is_empty())
}

/// Prepare persistent mount declarations on the completed script output.
/// 在已完成脚本的根中准备持久挂载声明。
/// # Arguments
/// - `backend`: CoW and plugin backend. CoW 及插件后端。
/// - `plugin`: Selected distribution plugin. 所选发行版插件。
/// - `context`: Frozen current build inputs. 冻结的当前构建输入。
/// - `root`: Writable post-script root. 脚本后的可写根。
/// - `system`: Owning managed-system directory. 所属被管理系统目录。
pub(super) fn apply_mounts(
    backend: &impl BuildBackend,
    plugin: &LoadedPlugin,
    context: &BuildContext,
    root: &Path,
    system: &Path,
) -> Result<Inherited> {
    let parameters = crate::config::read_plugin_parameters_at(&context.config)?;
    let config = InheritanceConfig::read(&parameters)?;
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
        progress::message(
            &context.system_id,
            format!("准备持久继承挂载：{}", source.display()),
            format!("Prepare persistent inherited mount: {}", source.display()),
        );
        if !disk::exists(&persistent)? {
            // Remove the marker before inspecting a file so a mismatched trailing slash
            // produces a type error instead of ENOTDIR, and links remain rejected.
            let unmarked = Path::new(source.to_str().unwrap().trim_end_matches('/'));
            let directory = check_source(unmarked, system)?;
            validate_inheritance_type(source, directory)?;
            if backend.check_subvolume(unmarked, false).is_ok()
                || backend.check_subvolume(unmarked, true).is_ok()
            {
                super::logging::diagnostic(format!(
                    "警告：Mount 继承来源是子卷，使用完整复制 {} / Warning: mount inheritance source is a subvolume; copying its full contents",
                    source.display(),
                ));
            }
            if let Err(error) = backend.check_cow_source(unmarked, &pool) {
                super::logging::diagnostic(format!(
                    "警告：Mount 继承使用完整复制 {} / Warning: copying mount inheritance across filesystems, subvolumes or mount points: {error}",
                    source.display()
                ));
            }
            seed_persistent(backend, unmarked, &persistent, &pool)?;
        }
        let info = fs::symlink_metadata(&persistent).map_err(|e| disk::io(&persistent, e))?;
        validate_inheritance_type(source, info.is_dir())?;
        let relative = source
            .strip_prefix("/")
            .map_err(|e| Error::Invalid(e.to_string()))?;
        let target = root.join(relative);
        if info.is_dir() {
            if let Err(error) = backend.check_cow_source(&persistent, &pool) {
                super::logging::diagnostic(format!(
                    "警告：复用含子卷或挂载点的 Mount 持久目录 {} / Warning: reusing a persistent mount directory containing subvolumes or mount points: {error}",
                    persistent.display()
                ));
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
pub(super) fn check_source(source: &Path, system: &Path) -> Result<bool> {
    disk::checked(source)?;
    let manager = system
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| Error::Invalid("系统缺少管理根".into()))?;
    if manager.starts_with(source) {
        return Err(Error::Invalid("继承来源不能与管理器目录重叠".into()));
    }
    let info = fs::symlink_metadata(source).map_err(|e| disk::io(source, e))?;
    if !info.is_dir() && !info.is_file() {
        return Err(Error::Invalid("继承来源必须为普通文件或目录".into()));
    }
    Ok(info.is_dir())
}

/// Replace a target only after checking paths and descendant storage.
/// 检查路径及后代存储后替换目标。
/// # Arguments
/// - `backend`: Checked storage backend. 经检查的存储后端。
/// - `target`: Owned destination inside the writable build root. 可写构建根内的所属目标。
fn replace_target(backend: &impl BuildBackend, target: &Path) -> Result<()> {
    disk::checked(
        target
            .parent()
            .ok_or_else(|| Error::Invalid("继承目标没有父目录".into()))?,
    )?;
    super::mounts::check_unmounted_tree(target)?;
    if fs::symlink_metadata(target).is_ok() {
        let info = fs::symlink_metadata(target).map_err(|e| disk::io(target, e))?;
        if info.is_dir() {
            if backend.is_subvolume(target)? {
                backend.delete_subvolume_recursive(target)?;
            } else {
                let mut children = backend.nested_subvolumes(target)?;
                children.sort_by_key(|p| p.components().count());
                for relative in &children {
                    backend.set_readonly(&target.join(relative), false)?;
                }
                for relative in children.iter().rev() {
                    backend.delete_subvolume(&target.join(relative))?;
                }
                fs::remove_dir_all(target).map_err(|e| disk::io(target, e))?;
            }
        } else if info.is_file() || info.file_type().is_symlink() {
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

/// Remove only a validated owned tree, including any nested subvolumes.
pub(super) fn remove_tree(backend: &impl BuildBackend, target: &Path) -> Result<()> {
    replace_target(backend, target)
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
/// - `children_readonly`: Whether child snapshots are immutable cache data. 子快照是否为不可变缓存数据。
pub(super) fn snapshot_tree(
    backend: &impl BuildBackend,
    source: &Path,
    target: &Path,
    children_readonly: bool,
) -> Result<()> {
    backend.snapshot_recursive(source, target, children_readonly)
}

/// Reuse existing source subvolumes; ordinary directories are seeded only once.
pub(super) fn inherit_path(
    backend: &impl BuildBackend,
    source: &Path,
    target: &Path,
    directory_subvolume: bool,
    nocow_handling: NocowHandling,
) -> Result<()> {
    let info = fs::symlink_metadata(source).map_err(|e| disk::io(source, e))?;
    if !info.is_dir() && !info.is_file() {
        return Err(Error::Invalid("继承来源必须为普通文件或目录".into()));
    }
    if nocow_handling == NocowHandling::MoveSnapshot
        && ((!info.is_dir() && super::nocow::is_nocow(source)?)
            || (info.is_dir()
                && !backend.is_subvolume(source)?
                && super::nocow::contains_nocow(source)?))
    {
        return Err(Error::Invalid(
            "move-snapshot 需要先安全迁移来源目录；不能扩大单个 NOCOW 文件的继承范围".into(),
        ));
    }
    replace_target(backend, target)?;
    if !info.is_dir() {
        return backend.copy_inherited_with_policy(source, target, nocow_handling);
    }
    if directory_subvolume && backend.is_subvolume(source)? {
        return backend.snapshot_recursive(source, target, false);
    }
    if directory_subvolume {
        backend.create_subvolume(target)?;
    } else {
        disk::directory(target)?;
    }
    copy_directory(backend, source, target, nocow_handling)
}

/// Populate an owned empty directory without changing its already recorded identity.
pub(super) fn copy_directory(
    backend: &impl BuildBackend,
    source: &Path,
    target: &Path,
    nocow_handling: NocowHandling,
) -> Result<()> {
    backend.copy_inherited_with_policy(source, target, nocow_handling)?;
    {
        // Preserve existing child subvolumes independently of the outer-directory option.
        let mut children = backend.nested_subvolumes(source)?;
        children.sort_by_key(|p| p.components().count());
        let mut restored: Vec<PathBuf> = Vec::new();
        for relative in children {
            if restored.iter().any(|p| relative.starts_with(p)) {
                continue;
            }
            let child = target.join(&relative);
            replace_target(backend, &child)?;
            backend.snapshot_recursive(&source.join(&relative), &child, false)?;
            restored.push(relative);
        }
    }
    Ok(())
}
