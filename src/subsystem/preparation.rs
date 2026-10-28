//! Complete deferred version tasks and boot directly from the writable version root.
//! 完成延后版本任务，并直接从可写版本根启动。
use super::{
    Error, PreparedRuntime, Result, RuntimeMount, RuntimeRecord, SubsystemConfig, checked,
    filesystem::exists, guest_path, read_record, write_record,
};
use crate::{
    config::{ConfigStore, InheritanceConfig, inheritance_name, read_plugin_parameters_at},
    controller::{self, BuildBackend, SystemBackend},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{ffi::OsString, fs, path::Path, process::Command};

/// Legacy snapshot transaction retained solely for interrupted-upgrade recovery.
/// 仅为升级后恢复旧中断操作而保留的快照事务。
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Transaction {
    schema_version: u32,
    system_id: String,
    version: String,
    generation: String,
    previous_version: Option<String>,
}

/// Produce a deterministic machine name bounded by systemd's naming limit.
/// 生成满足 systemd 长度限制的确定机器名称。
/// # Arguments
/// - `store`: Manager identity. 管理器身份。
/// - `id`: Validated managed-system identifier. 经校验的被管理系统标识。
pub fn machine_name(store: &ConfigStore, id: &str) -> String {
    let digest = Sha256::digest(
        [
            store.root().as_os_str().as_encoded_bytes(),
            b"\0",
            id.as_bytes(),
        ]
        .concat(),
    );
    format!(
        "rs-{}-{}",
        &id[..id.len().min(32)],
        &format!("{digest:x}")[..16]
    )
}

/// Validate a current runtime record before deriving any owned path.
/// 推导所属路径前校验当前运行记录。
/// # Arguments
/// - `record`: Persisted version or legacy generation reference. 持久版本或旧运行代引用。
/// - `store`: Owning manager. 所属管理器。
/// - `id`: Expected system. 预期系统。
fn validate_record(record: &RuntimeRecord, store: &ConfigStore, id: &str) -> Result<()> {
    crate::config::validate_identifier(&record.version)?;
    let valid_storage = match record.schema_version {
        1 => {
            crate::config::validate_identifier(&record.generation)?;
            record.generation.starts_with("g-")
                && record.config_sha256.len() == 64
                && record.config_sha256.bytes().all(|v| v.is_ascii_hexdigit())
        }
        2 => record.generation.is_empty() && record.config_sha256.is_empty(),
        _ => false,
    };
    if !valid_storage || record.system_id != id || record.machine != machine_name(store, id) {
        return Err(Error::Invalid(
            "运行记录身份或格式不匹配 / runtime record identity or format mismatch".into(),
        ));
    }
    Ok(())
}

/// Remove a generation only after its runtime root has actually disappeared.
/// 仅在运行根确实消失后删除运行代目录。
/// # Arguments
/// - `directory`: Owned uncommitted legacy generation. 所属未提交旧运行代。
fn remove_generation(directory: &Path) -> Result<()> {
    let root = directory.join("root");
    if exists(&root)? {
        return Err(Error::Invalid(format!(
            "旧运行根删除后仍然存在: {0} / obsolete runtime root still exists after deletion: {0}",
            root.display()
        )));
    }
    fs::remove_dir(directory).map_err(|source| Error::Io {
        path: directory.into(),
        source,
    })
}

/// Recover an interrupted legacy preparation without deleting a committed current root.
/// 恢复旧中断准备，不删除已提交的当前根。
/// # Arguments
/// - `store`: Owning manager. 所属管理器。
/// - `id`: Registered system. 已登记系统。
/// - `backend`: Recursive storage backend. 递归存储后端。
fn recover(store: &ConfigStore, id: &str, backend: &impl BuildBackend) -> Result<()> {
    let runtime = store.system_layout(id)?.root.join("runtime");
    let path = runtime.join("transaction.toml");
    if !exists(&path)? {
        return Ok(());
    }
    let tx: Transaction = read_record(&path)?;
    crate::config::validate_identifier(&tx.generation)?;
    crate::config::validate_identifier(&tx.version)?;
    if tx.schema_version != 1 || tx.system_id != id || !tx.generation.starts_with("g-") {
        return Err(Error::Invalid(
            "启动事务身份不匹配 / startup transaction identity mismatch".into(),
        ));
    }
    let current_path = runtime.join("current.toml");
    let committed = if exists(&current_path)? {
        let current: RuntimeRecord = read_record(&current_path)?;
        validate_record(&current, store, id)?;
        current.schema_version == 1 && current.generation == tx.generation
    } else {
        false
    };
    if !committed {
        let generation = runtime.join("generations").join(&tx.generation);
        checked(&generation)?;
        let root = generation.join("root");
        if exists(&root)? {
            backend.delete_subvolume_recursive(&root)?;
        }
        if exists(&generation)? {
            remove_generation(&generation)?;
        }
    }
    fs::remove_file(&path).map_err(|source| Error::Io { path, source })?;
    fs::File::open(&runtime)
        .and_then(|file| file.sync_all())
        .map_err(|source| Error::Io {
            path: runtime,
            source,
        })?;
    Ok(())
}

/// Read the already prepared root while retaining current policy.
/// 保留当前策略读取已经准备的根。
/// # Arguments
/// - `store`: Manager configuration. 管理器配置。
/// - `id`: Registered subsystem. 已登记子系统。
/// # Errors
/// Rejects missing or corrupt owned records. 拒绝缺失或损坏的所属记录。
pub fn load_prepared(store: &ConfigStore, id: &str) -> Result<PreparedRuntime> {
    let config = SubsystemConfig::read(store, id)?;
    load_with_config(store, id, config)
}

/// Recover an owned registration for draining a system whose role was removed.
/// 为排空已移除角色的系统恢复所属注册。
/// # Arguments
/// - `store`, `id`: Fixed manager and formerly registered system. 固定管理器及此前登记系统。
pub(crate) fn load_for_drain(store: &ConfigStore, id: &str) -> Result<PreparedRuntime> {
    let path = store
        .system_layout(id)?
        .config
        .join("subsystem/config.toml");
    let config: SubsystemConfig = read_record(&path)?;
    config.validate()?;
    load_with_config(store, id, config)
}

/// Resolve a validated owned root using already checked policy.
/// 使用已检查策略解析经校验所属根。
/// # Arguments
/// - `store`, `id`, `config`: Fixed manager, identity and policy. 固定管理器、身份及策略。
fn load_with_config(
    store: &ConfigStore,
    id: &str,
    config: SubsystemConfig,
) -> Result<PreparedRuntime> {
    let layout = store.system_layout(id)?;
    let record: RuntimeRecord = read_record(&layout.root.join("runtime/current.toml"))?;
    validate_record(&record, store, id)?;
    // Legacy roots are resolved only to recover and drain machines created before the upgrade.
    // 旧根仅用于恢复并排空升级前创建的机器。
    let root = if record.schema_version == 1 {
        layout
            .root
            .join("runtime/generations")
            .join(&record.generation)
            .join("root")
    } else {
        layout.versions.join(&record.version).join("root")
    };
    checked(&root)?;
    if !root.is_dir() {
        return Err(Error::Invalid(
            "运行根缺失 / runtime root is missing".into(),
        ));
    }
    Ok(PreparedRuntime {
        system_id: id.into(),
        version: record.version,
        root,
        machine: record.machine,
        config,
        boot_args: Vec::new(),
    })
}

/// Mount a verified Btrfs top-level view for the existing offline finalizer.
/// 为已有离线收尾器挂载经校验的 Btrfs 顶层视图。
struct TopLevel {
    directory: tempfile::TempDir,
    mounted: bool,
}
impl TopLevel {
    /// Mount only the known manager filesystem under a private temporary directory.
    /// 只在私有临时目录中挂载已知管理器文件系统。
    /// # Arguments
    /// - `uuid`: Verified Btrfs filesystem UUID. 经校验的 Btrfs 文件系统 UUID。
    fn mount(uuid: &str) -> Result<Self> {
        if uuid.is_empty() || !uuid.bytes().all(|v| v.is_ascii_hexdigit() || v == b'-') {
            return Err(Error::Invalid(
                "Btrfs UUID 无效 / invalid Btrfs UUID".into(),
            ));
        }
        let parent = Path::new("/run/rstarsleep/storage");
        checked(parent)?;
        fs::create_dir_all(parent)?;
        let directory = tempfile::Builder::new().prefix("top-").tempdir_in(parent)?;
        let output = Command::new("mount")
            .args(["-t", "btrfs", "-o", "rw,subvolid=5", "--"])
            .arg(format!("UUID={uuid}"))
            .arg(directory.path())
            .output()?;
        if !output.status.success() {
            return Err(Error::Invalid(format!(
                "挂载 Btrfs 顶层失败: {0} / failed to mount Btrfs top level: {0}",
                String::from_utf8_lossy(&output.stderr)
            )));
        }
        Ok(Self {
            directory,
            mounted: true,
        })
    }
}
impl Drop for TopLevel {
    /// Unmount before dropping the private ordinary directory.
    /// 删除私有普通目录前解除挂载。
    /// # Arguments
    /// - `self`: Mounted top-level view. 已挂载顶层视图。
    fn drop(&mut self) {
        if self.mounted {
            let result = Command::new("umount")
                .arg("--")
                .arg(self.directory.path())
                .status();
            if !result.is_ok_and(|s| s.success()) {
                // Retain an unremoved mount rather than recursively deleting through it.
                // 保留无法解除的挂载，避免经挂载递归删除。
                self.directory.disable_cleanup(true);
            }
        }
    }
}

/// Prepare the current version root using production storage.
/// 使用生产存储准备当前版本根。
/// # Arguments
/// - `store`: Initialized manager. 已初始化管理器。
/// - `id`: Registered subsystem; the old machine must be stopped. 已登记且旧机器已停止的子系统。
/// # Errors
/// Returns artifact, deferred-task or runtime publication failures. 返回产物、延后任务或运行发布错误。
pub fn prepare(store: &ConfigStore, id: &str) -> Result<PreparedRuntime> {
    prepare_with_backend(store, id, &SystemBackend, None)
}

/// Prepare the version root with injectable storage and an optional top-level view.
/// 使用可注入存储及可选顶层视图准备版本根。
/// # Arguments
/// - `store`: Initialized manager. 已初始化管理器。
/// - `id`: Registered subsystem. 已登记子系统。
/// - `backend`: Checked recursive snapshot/finalization backend. 经检查的递归快照及收尾后端。
/// - `filesystem_root`: Existing subvolid=5 view, or production temporary mount. 已有顶层视图，省略则生产临时挂载。
/// # Errors
/// Returns any incomplete storage or task error without switching to another version.
/// 存储或任务错误直接返回，不切换到其他版本。
pub fn prepare_with_backend(
    store: &ConfigStore,
    id: &str,
    backend: &impl BuildBackend,
    filesystem_root: Option<&Path>,
) -> Result<PreparedRuntime> {
    let config = SubsystemConfig::read(store, id)?;
    let layout = store.system_layout(id)?;
    let artifact = controller::read_current_build(store, id)?;
    let runtime = layout.root.join("runtime");
    checked(&runtime)?;
    let current_path = runtime.join("current.toml");
    if exists(&current_path)? {
        let record: RuntimeRecord = read_record(&current_path)?;
        validate_record(&record, store, id)?;
    }
    let mounts = runtime_mounts(store, id, &artifact.version, &config)?;
    recover(store, id, backend)?;
    let version_directory = layout.versions.join(&artifact.version);
    let manifest = version_directory.join("inheritance-pending.toml");
    checked(&manifest)?;
    if manifest.exists() {
        let (uuid, location) = backend.filesystem_location(&version_directory)?;
        let mount = if filesystem_root.is_none() {
            Some(TopLevel::mount(&uuid)?)
        } else {
            None
        };
        let top = filesystem_root.unwrap_or_else(|| mount.as_ref().unwrap().directory.path());
        let relocated = top
            .join(
                location
                    .strip_prefix("/")
                    .map_err(|e| Error::Invalid(e.to_string()))?,
            )
            .join("inheritance-pending.toml");
        controller::finalize_inheritance_with_backend(&relocated, top, backend)?;
    }
    let root = version_directory.join("root");
    checked(&root)?;
    backend.check_subvolume(&root, false)?;
    fs::create_dir_all(&runtime)?;
    let boot_args = mount_arguments(store, id, &root, &config, &mounts)?;
    backend.sync_filesystem(&root)?;
    let record = RuntimeRecord {
        schema_version: 2,
        system_id: id.into(),
        version: artifact.version.clone(),
        generation: String::new(),
        config_sha256: String::new(),
        machine: machine_name(store, id),
    };
    write_record(&current_path, &record)?;
    Ok(PreparedRuntime {
        system_id: id.into(),
        version: artifact.version,
        root,
        machine: record.machine,
        config,
        boot_args,
    })
}

/// Encode a native bind without allowing field separator injection.
/// 编码原生绑定，不允许字段分隔符注入。
/// # Arguments
/// - `mount`: Validated mapping. 经校验映射。
pub(crate) fn bind_argument(mount: &RuntimeMount) -> Result<OsString> {
    super::config::absolute(&mount.source, false)?;
    super::config::absolute(&mount.target, false)?;
    let mut result = OsString::from(if mount.read_only {
        "--bind-ro="
    } else {
        "--bind="
    });
    result.push(&mount.source);
    result.push(":");
    result.push(&mount.target);
    Ok(result)
}

/// Validate configured mounts and their sources before finalizing the version.
/// 版本收尾前校验配置挂载及其来源。
/// # Arguments
/// - `store`, `id`, `version`, `config`: Fixed manager, artifact and launch policy. 固定管理器、产物及启动策略。
fn runtime_mounts(
    store: &ConfigStore,
    id: &str,
    version: &str,
    config: &SubsystemConfig,
) -> Result<Vec<RuntimeMount>> {
    let layout = store.system_layout(id)?;
    let parameters = read_plugin_parameters_at(&layout.versions.join(version).join("config"))?;
    let inheritance = InheritanceConfig::read(&parameters)?;
    let mut mounts = config.mounts.clone();
    for target in inheritance.mounts {
        let source = layout.persistent.join(inheritance_name(&target)?);
        checked(&source)?;
        if !source.exists() {
            return Err(Error::Invalid(format!(
                "持久对象不存在: {0} / persistent object does not exist: {0}",
                source.display()
            )));
        }
        crate::config::validate_inheritance_type(&target, fs::metadata(&source)?.is_dir())?;
        mounts.push(RuntimeMount {
            source,
            target,
            read_only: false,
        });
    }
    SubsystemConfig {
        mounts: mounts.clone(),
        ..config.clone()
    }
    .validate()?;
    for mount in &mounts {
        checked(&mount.source)?;
        let meta = fs::metadata(&mount.source)?;
        if !meta.is_dir() && !meta.is_file() {
            return Err(Error::Invalid(
                "挂载来源必须是文件或目录 / mount source must be a file or directory".into(),
            ));
        }
    }
    Ok(mounts)
}

/// Prepare mount targets and launch arguments in the finalized version root.
/// 在已收尾版本根内准备挂载目标及启动参数。
/// # Arguments
/// - `store`, `id`, `root`, `config`, `mounts`: Fixed manager, version root and checked launch inputs.
///   固定管理器、版本根及已检查的启动输入。
fn mount_arguments(
    store: &ConfigStore,
    id: &str,
    root: &Path,
    config: &SubsystemConfig,
    mounts: &[RuntimeMount],
) -> Result<Vec<OsString>> {
    let layout = store.system_layout(id)?;
    let mut args = vec![
        OsString::from("--settings=no"),
        OsString::from("--link-journal=auto"),
    ];
    for mount in mounts {
        checked(&mount.source)?;
        let meta = fs::metadata(&mount.source)?;
        let target = guest_path(root, &mount.target)?;
        let resolved = Path::new("/").join(
            target
                .strip_prefix(root)
                .map_err(|e| Error::Invalid(e.to_string()))?,
        );
        for reserved in ["/run", "/dev", "/proc", "/sys", "/rstarsleep"] {
            if resolved.starts_with(reserved) || Path::new(reserved).starts_with(&resolved) {
                return Err(Error::Invalid(format!(
                    "挂载目标解析后位于保留的客体路径内: {0} / mount target resolves into a reserved guest path: {0}",
                    mount.target.display()
                )));
            }
        }
        if meta.is_dir() {
            fs::create_dir_all(&target)?;
        } else if meta.is_file() {
            fs::create_dir_all(target.parent().ok_or_else(|| {
                Error::Invalid("挂载目标缺少父目录 / mount target has no parent directory".into())
            })?)?;
            if !target.exists() {
                fs::File::create(&target)?;
            }
        } else {
            return Err(Error::Invalid(
                "挂载来源必须是文件或目录 / mount source must be a file or directory".into(),
            ));
        }
        args.push(bind_argument(mount)?);
    }
    // Overlay only an adapted fstab; persistent /etc and its source remain untouched.
    // 仅覆盖适配后的 fstab；持久 /etc 及其来源不被修改。
    let mut text = String::new();
    use std::io::Read;
    use std::os::unix::fs::OpenOptionsExt;
    let fstab = Path::new("/etc/fstab");
    let source = if let Some(mount) = mounts.iter().find(|m| fstab.starts_with(&m.target)) {
        if mount.source.is_file() {
            Some(
                fs::OpenOptions::new()
                    .read(true)
                    .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
                    .open(&mount.source)?,
            )
        } else {
            let relative = fstab
                .strip_prefix(&mount.target)
                .map_err(|e| Error::Invalid(e.to_string()))?;
            let guest = Path::new("/").join(relative);
            match super::open_guest(&mount.source, &guest, libc::O_RDONLY | libc::O_NONBLOCK) {
                Ok(f) => Some(f),
                Err(Error::Io { source, .. }) if source.kind() == std::io::ErrorKind::NotFound => {
                    None
                }
                Err(e) => return Err(e),
            }
        }
    } else {
        match super::open_guest(root, fstab, libc::O_RDONLY | libc::O_NONBLOCK) {
            Ok(f) => Some(f),
            Err(Error::Io { source, .. }) if source.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(e),
        }
    };
    if let Some(file) = source {
        if !file.metadata()?.is_file() {
            return Err(Error::Invalid(
                "fstab 必须为普通文件 / fstab must be a regular file".into(),
            ));
        }
        file.take(1024 * 1024 + 1).read_to_string(&mut text)?;
    }
    let adapted = adapt_fstab(&text)?;
    let path = layout.root.join("runtime/fstab");
    super::atomic_write(&path, adapted.as_bytes(), 0o644)?;
    args.push(bind_argument(&RuntimeMount {
        source: path,
        target: fstab.into(),
        read_only: true,
    })?);
    if config.tmp_size_bytes > 0 {
        args.push(format!("--tmpfs=/tmp:mode=1777,size={}", config.tmp_size_bytes).into());
    }
    if config.gpu && Path::new("/dev/dri").is_dir() {
        args.push("--bind=/dev/dri".into());
    }
    Ok(args)
}

/// Remove managed host mounts and diagnose unsupported host-dependent guest rows.
/// 移除受管宿主挂载，并诊断依赖宿主的非受管容器条目。
/// # Arguments
/// - `text`: Existing fstab, possibly from shared persistent /etc. 已有 fstab，可能来自共享持久 /etc。
fn adapt_fstab(text: &str) -> Result<String> {
    if text.len() > 1024 * 1024 {
        return Err(Error::Invalid("fstab 过大 / fstab too large".into()));
    }
    let mut output = String::new();
    let mut inside = false;
    let mut seen = false;
    for line in text.lines() {
        let value = line.trim();
        if value == "# BEGIN RStarSleep inherited mounts" {
            if inside || seen {
                return Err(Error::Invalid(
                    "受管 fstab 区块格式错误 / malformed managed fstab block".into(),
                ));
            }
            inside = true;
            seen = true;
            continue;
        }
        if value == "# END RStarSleep inherited mounts" {
            if !inside {
                return Err(Error::Invalid(
                    "受管 fstab 区块格式错误 / malformed managed fstab block".into(),
                ));
            }
            inside = false;
            continue;
        }
        if inside {
            continue;
        }
        if value.is_empty() || value.starts_with('#') {
            output.push_str(line);
            output.push('\n');
            continue;
        }
        let fields: Vec<_> = value.split_whitespace().collect();
        if fields.len() < 4 {
            return Err(Error::Invalid(
                "客体 fstab 行格式错误 / malformed guest fstab row".into(),
            ));
        }
        if fields[1] == "/" || fields[2] == "swap" {
            continue;
        }
        if !matches!(fields[2], "tmpfs" | "proc" | "sysfs" | "devpts")
            && !fields[3].split(',').any(|o| matches!(o, "bind" | "rbind"))
        {
            return Err(Error::Invalid(format!(
                "客体 fstab 行需要宿主挂载: {line}；请声明子系统运行时挂载 / guest fstab row requires a host mount: {line}; declare a subsystem runtime mount"
            )));
        }
        output.push_str(line);
        output.push('\n');
    }
    if inside {
        return Err(Error::Invalid(
            "受管 fstab 区块未终止 / unterminated managed fstab block".into(),
        ));
    }
    Ok(output)
}

#[cfg(test)]
mod fstab_tests {
    use super::*;
    /// Managed mounts are injected by nspawn while guest tmpfs rows survive.
    /// 受管挂载由 nspawn 注入，容器 tmpfs 条目保留。
    /// # Arguments
    /// No parameters. 无参数。
    #[test]
    fn preserves_guest_rows() {
        let result=adapt_fstab("UUID=host / btrfs defaults 0 0\n# BEGIN RStarSleep inherited mounts\nUUID=host /home btrfs subvol=data 0 0\n# END RStarSleep inherited mounts\ntmpfs /cache tmpfs size=1M 0 0\n").unwrap();
        assert_eq!(result, "tmpfs /cache tmpfs size=1M 0 0\n");
        assert!(adapt_fstab("UUID=host /data ext4 defaults 0 0\n").is_err());
        assert!(adapt_fstab("# BEGIN RStarSleep inherited mounts\n").is_err());
    }
}
