//! Register pending mounts and publish durable volumes before activation.
//! 登记待处理挂载，并在激活前发布持久卷。
use super::copy::{CopyBudget, copy_object};
use super::*;

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Migration {
    pub(super) schema_version: u32,
    pub(super) system_id: String,
    pub(super) target: PathBuf,
    pub(super) source: PathBuf,
    pub(super) directory: bool,
    pub(super) published: bool,
}
/// Locate owned migration state independent of version generations.
/// 定位独立于版本代的所属迁移状态。
/// # Arguments
/// - `store`, `id`, `target`: Fixed manager, system and validated target. 固定管理器、系统及已校验目标。
fn migration_paths(store: &ConfigStore, id: &str, target: &Path) -> Result<(PathBuf, PathBuf)> {
    let layout = store.system_layout(id)?;
    Ok((
        layout
            .root
            .join("runtime/guest")
            .join(format!("mount-{}.toml", key(target))),
        layout.persistent.join("guest").join(key(target)),
    ))
}

/// Validate persisted state before deriving a host source or trusting publication.
/// 推导宿主来源或信任发布前校验持久状态。
/// # Arguments
/// - `record`: Read migration receipt. 已读取迁移记录。
/// - `id`, `target`, `volume`: Expected fixed identity. 预期固定身份。
pub(super) fn validate_migration(
    record: &Migration,
    id: &str,
    target: &Path,
    volume: &Path,
) -> Result<()> {
    if record.schema_version != 1
        || record.system_id != id
        || record.target != target
        || record.source != volume.join("data")
    {
        return Err(Error::Invalid("guest migration identity mismatch".into()));
    }
    Ok(())
}

/// Register a persistent target while preserving every other runtime option.
/// 登记持久目标，同时保留其他运行选项。
/// # Arguments
/// - `store`, `runtime`, `root`, `target`: Authenticated fixed target. 已认证固定目标。
pub(super) fn add_mount(
    store: &ConfigStore,
    runtime: &PreparedRuntime,
    root: &GuestRoot,
    target: &Path,
) -> Result<String> {
    target_path(target)?;
    let layout = store.system_layout(&runtime.system_id)?;
    let _system = controller::lock_system_directory(&layout.root)?;
    let (record_path, volume) = migration_paths(store, &runtime.system_id, target)?;
    let mut config = SubsystemConfig::read(store, &runtime.system_id)?;
    if let Some(mount) = config.mounts.iter().find(|m| m.target == target) {
        if mount.source != volume.join("data") || mount.read_only {
            return Err(Error::Invalid(
                "target already has another configured mount".into(),
            ));
        }
        let record: Migration = read_record(&record_path)?;
        validate_migration(&record, &runtime.system_id, target, &volume)?;
        return Ok(format!(
            "已登记 {}；运行 guest migrate 完成迁移",
            target.display()
        ));
    }
    if record_path.try_exists()? {
        let record: Migration = read_record(&record_path)?;
        validate_migration(&record, &runtime.system_id, target, &volume)?;
        return Ok(format!(
            "已登记 {}；运行 guest migrate 完成迁移",
            target.display()
        ));
    }
    let inherited = InheritanceConfig::read(&crate::config::read_plugin_parameters_at(
        &layout.versions.join(&runtime.version).join("config"),
    )?)?;
    if inherited
        .mounts
        .iter()
        .any(|m| m.starts_with(target) || target.starts_with(m))
    {
        return Err(Error::Invalid(
            "guest target overlaps a version persistent mount".into(),
        ));
    }
    let source = open_target(&root.root, target)?;
    let metadata = source.metadata()?;
    if !metadata.is_file() && !metadata.is_dir() {
        return Err(Error::Invalid(
            "persistent source must be a regular file or directory".into(),
        ));
    }
    config.mounts.push(RuntimeMount {
        source: volume.join("data"),
        target: target.into(),
        read_only: false,
    });
    config.validate()?;
    fs::create_dir_all(record_path.parent().unwrap())?;
    checked(&volume)?;
    let record = Migration {
        schema_version: 1,
        system_id: runtime.system_id.clone(),
        target: target.into(),
        source: volume.join("data"),
        directory: metadata.is_dir(),
        published: false,
    };
    // A pending registration does not activate a missing source during cold startup.
    // 待处理登记不在冷启动时激活缺失来源。
    write_record(&record_path, &record)?;
    still_running(runtime, root)?;
    Ok(format!(
        "已登记 {}；运行 guest migrate 复制现有数据并立即挂载",
        target.display()
    ))
}

/// Atomically publish without replacing any existing durable volume.
/// 原子发布，绝不替换已有持久卷。
/// # Arguments
/// - `source`, `target`: Owned staging and final paths. 所属暂存与最终路径。
pub(super) fn publish(source: &Path, target: &Path, exchange: bool) -> Result<()> {
    let from = CString::new(source.as_os_str().as_encoded_bytes())
        .map_err(|_| Error::Invalid("invalid publication source".into()))?;
    let to = CString::new(target.as_os_str().as_encoded_bytes())
        .map_err(|_| Error::Invalid("invalid publication target".into()))?;
    if unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            libc::AT_FDCWD,
            from.as_ptr(),
            libc::AT_FDCWD,
            to.as_ptr(),
            if exchange { 2u32 } else { 1u32 },
        )
    } != 0
    {
        return Err(io::Error::last_os_error().into());
    }
    File::open(target.parent().unwrap())?.sync_all()?;
    Ok(())
}

/// Use already published data for every retry; failed bind never triggers recopy.
/// 每次重试使用已发布数据；绑定失败绝不触发重新复制。
/// # Arguments
/// - `store`, `runtime`, `root`, `target`: Authenticated fixed target. 已认证固定目标。
pub(super) fn migrate(
    store: &ConfigStore,
    runtime: &PreparedRuntime,
    root: &GuestRoot,
    target: &Path,
) -> Result<String> {
    target_path(target)?;
    let _activity = super::filesystem::activity(store, &runtime.system_id, true)?;
    if execution::occupied(runtime)? {
        return Err(Error::Invalid(
            "stop managed applications and all other writers before migration".into(),
        ));
    }
    let layout = store.system_layout(&runtime.system_id)?;
    let _system = controller::lock_system_directory(&layout.root)?;
    let (record_path, volume) = migration_paths(store, &runtime.system_id, target)?;
    let mut record: Migration = read_record(&record_path)?;
    validate_migration(&record, &runtime.system_id, target, &volume)?;
    let mut config = SubsystemConfig::read(store, &runtime.system_id)?;
    if let Some(mount) = config.mounts.iter().find(|m| m.target == target)
        && (mount.source != record.source || mount.read_only)
    {
        return Err(Error::Invalid(
            "persistent target now has another configured mount".into(),
        ));
    }
    let inherited = InheritanceConfig::read(&crate::config::read_plugin_parameters_at(
        &layout.versions.join(&runtime.version).join("config"),
    )?)?;
    if inherited
        .mounts
        .iter()
        .any(|m| m.starts_with(target) || target.starts_with(m))
    {
        return Err(Error::Invalid(
            "guest target overlaps a version persistent mount".into(),
        ));
    }
    if !config.mounts.iter().any(|m| m.target == target) {
        config.mounts.push(RuntimeMount {
            source: record.source.clone(),
            target: target.into(),
            read_only: false,
        });
    }
    config.validate()?;
    checked(&volume)?;
    let mounted = mounts_at(root, target)?;
    let mut already_mounted = false;
    if !mounted.is_empty() {
        if mounted.len() == 1
            && mounted[0] == target
            && volume.exists()
            && same_inode(
                &open_target(&root.root, target)?.metadata()?,
                &fs::symlink_metadata(&record.source)?,
            )
        {
            already_mounted = true;
        } else {
            return Err(Error::Invalid(
                "target or descendant is already mounted; refusing migration".into(),
            ));
        }
    }
    if !volume.try_exists()? {
        if record.published {
            return Err(Error::Invalid(
                "published guest volume is missing; refusing to overwrite its receipt".into(),
            ));
        }
        let source = open_target(&root.root, target)?;
        if source.metadata()?.is_dir() != record.directory {
            return Err(Error::Invalid(
                "guest source changed type after registration".into(),
            ));
        }
        let parent = volume.parent().unwrap();
        checked(parent)?;
        fs::create_dir_all(parent)?;
        let stage_parent = tempfile::Builder::new()
            .prefix(".migrating-")
            .tempdir_in(parent)?;
        let stage = stage_parent.path().join("volume");
        SystemBackend.create_subvolume(&stage)?;
        let copied = (|| -> Result<()> {
            copy_object(
                &source,
                &stage.join("data"),
                &mut CopyBudget::new(64 * 1024 * 1024 * 1024),
                0,
            )?;
            still_running(runtime, root)?;
            if !same_inode(
                &source.metadata()?,
                &open_target(&root.root, target)?.metadata()?,
            ) {
                return Err(Error::Invalid(
                    "guest migration source path changed; retry after stopping writers".into(),
                ));
            }
            if execution::occupied(runtime)? || !mounts_at(root, target)?.is_empty() {
                return Err(Error::Invalid(
                    "guest writers or mounts changed during migration".into(),
                ));
            }
            SystemBackend.sync_filesystem(&stage)?;
            publish(&stage, &volume, false)
        })();
        if copied.is_err()
            && stage.exists()
            && let Err(e) = SystemBackend.delete_subvolume_recursive(&stage)
        {
            // Preserve a failed-cleanup subvolume for administrator recovery.
            // 保留清理失败子卷供管理员恢复。
            let path = stage_parent.keep();
            eprintln!("guest migration cleanup {}: {e}", path.display());
        }
        copied?;
    }
    SystemBackend.check_subvolume(&volume, false)?;
    checked(&record.source)?;
    let metadata = fs::symlink_metadata(&record.source)?;
    if metadata.is_dir() != record.directory || (!metadata.is_dir() && !metadata.is_file()) {
        return Err(Error::Invalid(
            "published guest data has invalid type".into(),
        ));
    }
    record.published = true;
    write_record(&record_path, &record)?;
    // Activate only durable data: restart remains possible even if the hot bind fails.
    // 只激活持久数据：热绑定失败时仍可重启。
    write_record(&layout.config.join("subsystem/config.toml"), &config)?;
    still_running(runtime, root)?;
    if already_mounted {
        return Ok(format!("{} 已使用持久卷", target.display()));
    }
    let mut command = Command::new(commands::MACHINECTL);
    command
        .args(["bind", &runtime.machine])
        .arg(&record.source)
        .arg(target);
    let result = commands::output(command, Duration::from_secs(15))?;
    if !result.status.success() {
        return Err(Error::Invalid(format!(
            "数据已保存在 {}；立即挂载失败，重试 guest migrate：{}",
            record.source.display(),
            String::from_utf8_lossy(&result.stderr)
        )));
    }
    if !same_inode(
        &open_target(&root.root, target)?.metadata()?,
        &fs::symlink_metadata(&record.source)?,
    ) {
        return Err(Error::Invalid(
            "volume published but guest bind is not visible; retry guest migrate".into(),
        ));
    }
    Ok(format!("已迁移并挂载 {}", target.display()))
}
