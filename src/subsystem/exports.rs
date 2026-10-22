//! User-level command, desktop and icon exports with ownership reconciliation.
//! 带所有权协调的用户级命令、desktop 和图标导出。
use super::{
    Error, PreparedRuntime, Result, SubsystemConfig, checked, desktop, package_files, read_record,
    write_record,
};
use crate::{config::ConfigStore, controller};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::{CStr, OsString},
    fs,
    io::{Read, Seek, Write},
    os::unix::{
        ffi::OsStringExt,
        fs::{OpenOptionsExt, PermissionsExt},
        process::CommandExt,
    },
    path::{Component, Path, PathBuf},
    process::{Child, Command, Output, Stdio},
    thread,
    time::{Duration, Instant},
};

const MAX_MANIFEST_BYTES: usize = 64 * 1024 * 1024;
const MAX_HELPER_STDERR_BYTES: u64 = 1024 * 1024;

struct ExportChild(Child);
impl Drop for ExportChild {
    /// Stop an unfinished helper process group on every error path.
    /// 在每个错误路径停止未完成的辅助程序进程组。
    /// # Arguments
    /// - `self`: Isolated helper process. 隔离的辅助子进程。
    fn drop(&mut self) {
        if !matches!(self.0.try_wait(), Ok(Some(_))) {
            unsafe {
                libc::kill(-(self.0.id() as i32), libc::SIGKILL);
            }
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Item {
    bytes: Vec<u8>,
    mode: u32,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Manifest {
    schema_version: u32,
    manager: String,
    id: String,
    home: PathBuf,
    entries: BTreeMap<String, Item>,
}
#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Ownership {
    files: BTreeMap<String, String>,
}
#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Users {
    uids: BTreeSet<u32>,
}

/// Retrieve a caller's home and primary group through reentrant host NSS.
/// 通过可重入宿主 NSS 获取调用者 home 及主要组。
/// # Arguments
/// - `uid`: Kernel caller identity. 内核调用者身份。
fn account(uid: u32) -> Result<(PathBuf, u32)> {
    let mut pwd: libc::passwd = unsafe { std::mem::zeroed() };
    let mut result = std::ptr::null_mut();
    let mut buffer = vec![0u8; 64 * 1024];
    let error = unsafe {
        libc::getpwuid_r(
            uid,
            &mut pwd,
            buffer.as_mut_ptr().cast(),
            buffer.len(),
            &mut result,
        )
    };
    if error != 0 || result.is_null() {
        return Err(Error::Invalid(
            "宿主用户没有 NSS 账户 / host user has no NSS account".into(),
        ));
    }
    let home = PathBuf::from(OsString::from_vec(
        unsafe { CStr::from_ptr(pwd.pw_dir) }.to_bytes().to_vec(),
    ));
    if !home.is_absolute() {
        return Err(Error::Invalid(
            "用户主目录必须为绝对路径 / user home must be absolute".into(),
        ));
    }
    Ok((home, pwd.pw_gid))
}
/// Quote a literal POSIX argument for generated command wrappers.
/// 为生成的命令包装器引用字面 POSIX 参数。
/// # Arguments
/// - `value`: Literal UTF-8 argument. 字面 UTF-8 参数。
fn shell(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}
/// Quote one desktop Exec argument, preserving literal specifiers.
/// 引用一个 desktop Exec 参数，保留字面说明符。
/// # Arguments
/// - `value`: Literal UTF-8 argument. 字面 UTF-8 参数。
fn exec_word(value: &str) -> String {
    format!(
        "\"{}\"",
        value
            .replace('\\', "\\\\\\\\")
            .replace('"', r#"\\""#)
            .replace('$', "\\\\$")
            .replace('`', "\\\\`")
            .replace('%', "%%")
    )
}
/// Read a bounded regular guest resource by a contained descriptor.
/// 通过根内描述符读取有界普通容器资源。
/// # Arguments
/// - `root`, `path`: Actual root and guest path. 实际根及容器路径。
/// - `limit`: Maximum content bytes. 内容字节上限。
fn resource(root: &fs::File, path: &str, limit: u64) -> Result<Vec<u8>> {
    let file =
        super::filesystem::open_at_guest(root, Path::new(path), libc::O_RDONLY | libc::O_NONBLOCK)?;
    if !file.metadata()?.is_file() || file.metadata()?.len() > limit {
        return Err(Error::Invalid(
            "导出资源不是大小受限的普通文件 / export resource is not a bounded regular file".into(),
        ));
    }
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(Error::Invalid(
            "导出资源增长后超出限制 / export resource grew beyond limit".into(),
        ));
    }
    Ok(bytes)
}

/// Build namespaced entry candidates for the selected actual artifact.
/// 为所选实际产物构建命名空间入口候选项。
/// # Arguments
/// - `store`, `runtime`, `uid`: Fixed manager, artifact and caller. 固定管理器、产物及调用者。
fn desired(store: &ConfigStore, runtime: &PreparedRuntime, uid: u32) -> Result<Manifest> {
    let files = if super::commands::alive(&runtime.machine)? {
        super::filesystem::open_runtime_root(runtime)?
    } else {
        super::filesystem::open_root(&runtime.root)?
    };
    let id = &runtime.system_id;
    let config = SubsystemConfig::read(store, id)?;
    let (home, _) = account(uid)?;
    let manager = format!(
        "{:x}",
        Sha256::digest(store.root().as_os_str().as_encoded_bytes())
    );
    let binary = fs::canonicalize(std::env::current_exe()?)?;
    let text = |path: &Path| {
        path.to_str().map(str::to_owned).ok_or_else(|| {
            Error::Invalid("导出路径必须为 UTF-8 / export paths must be UTF-8".into())
        })
    };
    let binary = text(&binary)?;
    let root = text(store.root())?;
    let mut commands: BTreeSet<String> = config.exports.commands.iter().cloned().collect();
    let mut desktops: BTreeSet<String> = config.exports.desktop_files.iter().cloned().collect();
    let mut index = BTreeSet::new();
    if !config.exports.packages.is_empty() {
        let path = store
            .system_layout(id)?
            .versions
            .join(&runtime.version)
            .join("application-files.tsv");
        checked(&path)?;
        let file = fs::File::open(&path).map_err(|e| {
            Error::Invalid(format!(
                "包导出需要 query_package_files 索引 / package exports require a query_package_files index: {e}"
            ))
        })?;
        let mut bytes = Vec::new();
        file.take(package_files::MAX_BYTES as u64 + 1)
            .read_to_end(&mut bytes)?;
        index = package_files::parse(&bytes)?;
        for (owner, path) in &index {
            if !config.exports.packages.contains(owner) {
                continue;
            }
            if path.starts_with("/usr/share/applications/") && path.ends_with(".desktop") {
                desktops.insert(path.clone());
            } else if path.starts_with("/usr/bin/")
                && !path[9..].contains('/')
                && desktop::executable_at(&files, path).is_ok()
            {
                commands.insert(path.clone());
            }
        }
        for package in &config.exports.packages {
            if !index.iter().any(|(owner, _)| owner == package) {
                return Err(Error::Invalid(format!(
                    "导出包 {package} 没有已索引文件 / no indexed files for exported package {package}"
                )));
            }
        }
    }
    let mut entries = BTreeMap::new();
    for target in commands {
        desktop::executable_at(&files, &target)?;
        let basename = Path::new(&target)
            .file_name()
            .and_then(|v| v.to_str())
            .ok_or_else(|| Error::Invalid("命令文件名无效 / invalid command basename".into()))?;
        crate::config::validate_identifier(basename)?;
        let wrapper = format!(
            "#!/bin/sh\nexec {} subsystem exec --root {} {} -- {} \"$@\"\n",
            shell(&binary),
            shell(&root),
            shell(id),
            shell(&target)
        );
        let path = format!(".local/bin/{id}-{basename}");
        if entries
            .insert(
                path,
                Item {
                    bytes: wrapper.into_bytes(),
                    mode: 0o755,
                },
            )
            .is_some()
        {
            return Err(Error::Invalid(
                "命令导出名称冲突 / command export name collision".into(),
            ));
        }
    }
    for target in desktops {
        let entry = desktop::read_at(&files, &target)?;
        if entry.hidden || entry.no_display {
            continue;
        }
        desktop::expand(&entry, &target, &[])?;
        let basename = Path::new(&target)
            .file_name()
            .and_then(|v| v.to_str())
            .ok_or_else(|| {
                Error::Invalid("desktop 文件名无效 / invalid desktop basename".into())
            })?;
        crate::config::validate_identifier(basename)?;
        let mut contents =
            String::from("[Desktop Entry]\nType=Application\nDBusActivatable=false\n");
        for (key, value) in &entry.values {
            if matches!(
                key.as_str(),
                "Name"
                    | "GenericName"
                    | "Comment"
                    | "Categories"
                    | "MimeType"
                    | "Keywords"
                    | "Terminal"
                    | "StartupNotify"
                    | "StartupWMClass"
                    | "OnlyShowIn"
                    | "NotShowIn"
            ) || ["Name[", "GenericName[", "Comment[", "Keywords["]
                .iter()
                .any(|prefix| key.starts_with(prefix))
            {
                if key.contains(['\n', '\r', '=']) || value.contains(['\n', '\r']) {
                    return Err(Error::Invalid(
                        "desktop 元数据必须位于单行 / desktop metadata must occupy one line".into(),
                    ));
                }
                contents.push_str(&format!("{key}={value}\n"));
            }
        }
        let mut argv = vec![
            binary.clone(),
            "subsystem".into(),
            "run-desktop".into(),
            "--root".into(),
            root.clone(),
            id.clone(),
            target.clone(),
            "--".into(),
        ];
        let field = desktop::file_field(&entry)?;
        let mut exec = argv
            .drain(..)
            .map(|a| exec_word(&a))
            .collect::<Vec<_>>()
            .join(" ");
        if let Some(field) = field {
            exec.push(' ');
            exec.push_str(field);
        }
        contents.push_str(&format!("Exec={exec}\n"));
        if let Some(icon) = entry.icon.as_deref() {
            let candidates = if icon.starts_with('/') {
                vec![icon.to_owned()]
            } else {
                let mut paths = index
                    .iter()
                    .filter_map(|(_, path)| {
                        let stem = Path::new(path).file_stem()?.to_str()?;
                        if stem == icon
                            && (path.starts_with("/usr/share/icons/")
                                || path.starts_with("/usr/share/pixmaps/"))
                        {
                            Some(path.clone())
                        } else {
                            None
                        }
                    })
                    .collect::<Vec<_>>();
                for ext in ["png", "svg", "xpm"] {
                    paths.extend([
                        format!("/usr/share/icons/hicolor/48x48/apps/{icon}.{ext}"),
                        format!("/usr/share/icons/hicolor/scalable/apps/{icon}.{ext}"),
                        format!("/usr/share/pixmaps/{icon}.{ext}"),
                    ]);
                }
                paths
            };
            for path in candidates {
                let ext = Path::new(&path)
                    .extension()
                    .and_then(|v| v.to_str())
                    .unwrap_or("");
                if !matches!(ext, "png" | "svg" | "xpm") {
                    continue;
                }
                if let Ok(bytes) = resource(&files, &path, 16 * 1024 * 1024) {
                    let name = format!(
                        "rs-{id}-{}.{ext}",
                        &format!("{:x}", Sha256::digest(&bytes))[..16]
                    );
                    let destination = format!(".local/share/icons/rstarsleep/{name}");
                    contents.push_str(&format!("Icon={}\n", home.join(&destination).display()));
                    entries.insert(destination, Item { bytes, mode: 0o644 });
                    break;
                }
            }
        }
        let path = format!(".local/share/applications/rstarsleep-{id}-{basename}");
        if entries
            .insert(
                path,
                Item {
                    bytes: contents.into_bytes(),
                    mode: 0o644,
                },
            )
            .is_some()
        {
            return Err(Error::Invalid(
                "desktop 导出名称冲突 / desktop export name collision".into(),
            ));
        }
    }
    Ok(Manifest {
        schema_version: 1,
        manager,
        id: id.clone(),
        home,
        entries,
    })
}

/// Apply exports only after exec has permanently dropped host privileges.
/// 只有 exec 永久放弃宿主特权后才应用导出。
/// # Arguments
/// - `manifest`: Complete desired user entries. 完整期望用户入口。
/// - `uid`: Authenticated destination user. 已认证目标用户。
fn user_helper(manifest: &Manifest, uid: u32) -> Result<()> {
    let bytes = serde_json::to_vec(manifest).map_err(|e| Error::Invalid(e.to_string()))?;
    let (_, gid) = account(uid)?;
    let binary = fs::canonicalize(std::env::current_exe()?)?;
    super::trust::path(&binary)?;
    let mut command = Command::new(binary);
    command
        .arg("__export-user")
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", &manifest.home);
    unsafe {
        command.pre_exec(move || {
            if libc::setgroups(0, std::ptr::null()) != 0
                || libc::setresgid(gid, gid, gid) != 0
                || libc::setresuid(uid, uid, uid) != 0
            {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let output = helper_output(command, &bytes, Duration::from_secs(30))?;
    if !output.status.success() {
        return Err(Error::Invalid(format!(
            "用户导出状态协调失败: {0} / user export reconciliation failed: {0}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    Ok(())
}

/// Bound helper execution even when the destination home stops responding.
/// 即使目标 home 停止响应，也限制辅助程序执行时间。
/// # Arguments
/// - `command`: Helper with permanent privilege dropping configured. 已配置永久降权的辅助命令。
/// - `bytes`, `timeout`: Manifest and execution budget. 清单及执行时限。
fn helper_output(mut command: Command, bytes: &[u8], timeout: Duration) -> Result<Output> {
    if bytes.len() > MAX_MANIFEST_BYTES {
        return Err(Error::Invalid(
            "导出清单过大 / export manifest too large".into(),
        ));
    }
    // A file avoids blocking the manager on pipe writes before its timeout loop.
    // 文件避免管理器在进入超时循环前因管道写入而阻塞。
    let mut input = tempfile::tempfile()?;
    input.write_all(bytes)?;
    input.rewind()?;
    let mut stderr = tempfile::tempfile()?;
    command
        .stdin(Stdio::from(input))
        .stdout(Stdio::null())
        .stderr(Stdio::from(stderr.try_clone()?));
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = ExportChild(command.spawn()?);
    let deadline = Instant::now() + timeout;
    let status = loop {
        if stderr.metadata()?.len() > MAX_HELPER_STDERR_BYTES {
            return Err(Error::Invalid(
                "用户导出辅助程序超出输出限制 / user export helper exceeded output limit".into(),
            ));
        }
        if let Some(status) = child.0.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            return Err(Error::Invalid(
                "用户导出辅助程序超出时间限制 / user export helper exceeded time limit".into(),
            ));
        }
        thread::sleep(Duration::from_millis(20));
    };
    stderr.rewind()?;
    let mut errors = Vec::new();
    stderr
        .take(MAX_HELPER_STDERR_BYTES + 1)
        .read_to_end(&mut errors)?;
    if errors.len() as u64 > MAX_HELPER_STDERR_BYTES {
        return Err(Error::Invalid(
            "用户导出辅助程序超出输出限制 / user export helper exceeded output limit".into(),
        ));
    }
    Ok(Output {
        status,
        stdout: Vec::new(),
        stderr: errors,
    })
}

/// Generate entries from a running version or the exact current built artifact.
/// 从运行版本或准确当前构建产物生成入口。
/// # Arguments
/// - `store`, `id`, `uid`: Fixed manager, subsystem and caller. 固定管理器、子系统及调用者。
/// # Errors
/// Returns authorization, artifact/index or ownership reconciliation failures.
/// 返回授权、产物索引或所有权协调失败。
pub fn sync(store: &ConfigStore, id: &str, uid: u32) -> Result<()> {
    let config = SubsystemConfig::read(store, id)?;
    if !config.allows(uid) {
        return Err(Error::Invalid(
            "导出访问被拒绝 / export access denied".into(),
        ));
    }
    let layout = store.system_layout(id)?;
    let _lock = controller::lock_system_directory(&layout.root)?;
    let prepared_path = layout.root.join("runtime/current.toml");
    checked(&prepared_path)?;
    let prepared = if prepared_path.exists() {
        let runtime = super::load_prepared(store, id)?;
        if super::commands::alive(&runtime.machine)? {
            Some(runtime)
        } else {
            None
        }
    } else {
        None
    };
    let runtime = if let Some(runtime) = prepared {
        runtime
    } else {
        let current = controller::read_current_build(store, id)?;
        let dir = layout.versions.join(&current.version);
        let root = if dir.join("root").is_dir() {
            dir.join("root")
        } else {
            dir.join("prepared-root")
        };
        PreparedRuntime {
            system_id: id.into(),
            version: current.version,
            root,
            machine: super::machine_name(store, id),
            config,
            boot_args: Vec::new(),
        }
    };
    let manifest = desired(store, &runtime, uid)?;
    user_helper(&manifest, uid)?;
    let directory = layout.root.join("runtime");
    fs::create_dir_all(&directory)?;
    let path = directory.join("exports-users.toml");
    let mut users: Users = if path.exists() {
        read_record(&path)?
    } else {
        Users::default()
    };
    users.uids.insert(uid);
    write_record(&path, &users)
}

/// Refresh previously opted-in users after a successful artifact or runtime switch.
/// 成功产物或运行态切换后刷新此前主动集成用户。
/// # Arguments
/// - `store`, `id`: Fixed manager and registered system. 固定管理器及已登记系统。
pub(crate) fn sync_registered(store: &ConfigStore, id: &str) -> Result<()> {
    let path = store
        .system_layout(id)?
        .root
        .join("runtime/exports-users.toml");
    if !path.exists() {
        return Ok(());
    }
    let users: Users = read_record(&path)?;
    let config = SubsystemConfig::read(store, id)?;
    for uid in users.uids {
        if config.allows(uid) {
            sync(store, id, uid)?;
        }
    }
    Ok(())
}

/// Withdraw only owned, unmodified files for previously integrated users.
/// 只撤销此前集成用户的自有未修改文件。
/// # Arguments
/// - `store`, `id`: Removed or disabled subsystem. 已移除或禁用子系统。
/// # Errors
/// Returns controlled record or user-helper failures.
/// 返回受控记录或用户辅助程序失败。
pub fn withdraw_all(store: &ConfigStore, id: &str) -> Result<()> {
    let layout = store.system_layout(id)?;
    let path = layout.root.join("runtime/exports-users.toml");
    if !path.exists() {
        return Ok(());
    }
    let _lock = controller::lock_system_directory(&layout.root)?;
    let users: Users = read_record(&path)?;
    for uid in users.uids {
        let (home, _) = account(uid)?;
        user_helper(
            &Manifest {
                schema_version: 1,
                manager: format!(
                    "{:x}",
                    Sha256::digest(store.root().as_os_str().as_encoded_bytes())
                ),
                id: id.into(),
                home,
                entries: BTreeMap::new(),
            },
            uid,
        )?;
    }
    fs::remove_file(path)?;
    Ok(())
}

/// Withdraw stale authorizations without touching still-authorized users.
/// 撤销过时授权，不修改仍获授权用户的入口。
/// # Arguments
/// - `store`, `id`: Fixed manager and current subsystem. 固定管理器及当前子系统。
/// # Errors
/// Returns controlled record, configuration or user-helper failures.
/// 返回受控记录、配置或用户辅助程序失败。
pub fn withdraw_disallowed(store: &ConfigStore, id: &str) -> Result<()> {
    let layout = store.system_layout(id)?;
    let _lock = controller::lock_system_directory(&layout.root)?;
    let path = layout.root.join("runtime/exports-users.toml");
    if !path.exists() {
        return Ok(());
    }
    let mut users: Users = read_record(&path)?;
    let config = SubsystemConfig::read(store, id)?;
    let stale = users
        .uids
        .iter()
        .copied()
        .filter(|uid| !config.allows(*uid))
        .collect::<Vec<_>>();
    for uid in &stale {
        let (home, _) = account(*uid)?;
        user_helper(
            &Manifest {
                schema_version: 1,
                manager: format!(
                    "{:x}",
                    Sha256::digest(store.root().as_os_str().as_encoded_bytes())
                ),
                id: id.into(),
                home,
                entries: BTreeMap::new(),
            },
            *uid,
        )?;
        users.uids.remove(uid);
    }
    if !stale.is_empty() {
        write_record(&path, &users)?;
    }
    Ok(())
}

/// Validate manager-owned relative entry namespaces before mutation.
/// 修改前校验管理器所属相对入口命名空间。
/// # Arguments
/// - `path`, `id`: Relative entry and subsystem ID. 相对入口及子系统标识。
fn entry_path(path: &str, id: &str) -> Result<()> {
    let p = Path::new(path);
    if p.is_absolute()
        || p.components().any(|c| !matches!(c, Component::Normal(_)))
        || !([
            format!(".local/bin/{id}-"),
            format!(".local/share/applications/rstarsleep-{id}-"),
            format!(".local/share/icons/rstarsleep/rs-{id}-"),
        ]
        .iter()
        .any(|prefix| path.starts_with(prefix)))
    {
        return Err(Error::Invalid(
            "导出命名空间无效 / invalid export namespace".into(),
        ));
    }
    Ok(())
}
/// Hash only a bounded ordinary file; links are never owned exports.
/// 只摘要有界普通文件；链接绝不视为自有导出。
/// # Arguments
/// - `path`: User output path. 用户输出路径。
fn hash(path: &Path) -> Option<String> {
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .ok()?;
    if !file.metadata().ok()?.is_file() || file.metadata().ok()?.len() > 16 * 1024 * 1024 {
        return None;
    }
    let mut bytes = Vec::new();
    file.take(16 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() > 16 * 1024 * 1024 {
        return None;
    }
    Some(format!("{:x}", Sha256::digest(bytes)))
}
/// Reconcile a user manifest without overwriting external or edited files.
/// 协调用户清单，不覆盖外部或已编辑文件。
/// # Arguments
/// - `manifest`: Validated user-specific desired state. 已校验用户所属期望状态。
fn apply(manifest: Manifest) -> Result<()> {
    if manifest.schema_version != 1
        || manifest.manager.len() != 64
        || !manifest.manager.bytes().all(|c| c.is_ascii_hexdigit())
    {
        return Err(Error::Invalid(
            "导出清单无效 / invalid export manifest".into(),
        ));
    }
    protocol_id(&manifest.id)?;
    let state = manifest
        .home
        .join(".local/state/rstarsleep")
        .join(&manifest.manager);
    fs::create_dir_all(&state)?;
    let record = state.join(format!("{}.toml", manifest.id));
    let previous: Ownership = if record.exists() {
        toml::from_str(&fs::read_to_string(&record)?).map_err(|e| Error::Invalid(e.to_string()))?
    } else {
        Ownership::default()
    };
    let mut next = Ownership::default();
    for (path, digest) in &previous.files {
        entry_path(path, &manifest.id)?;
        if !manifest.entries.contains_key(path)
            && hash(&manifest.home.join(path)).as_ref() == Some(digest)
        {
            fs::remove_file(manifest.home.join(path))?;
        }
    }
    for (path, item) in manifest.entries {
        entry_path(&path, &manifest.id)?;
        if !matches!(item.mode, 0o644 | 0o755) || item.bytes.len() > 16 * 1024 * 1024 {
            return Err(Error::Invalid(
                "导出载荷无效 / invalid export payload".into(),
            ));
        }
        let destination = manifest.home.join(&path);
        let digest = format!("{:x}", Sha256::digest(&item.bytes));
        let current = hash(&destination);
        let exists = fs::symlink_metadata(&destination).is_ok();
        if exists && !(current.is_some() && previous.files.get(&path) == current.as_ref()) {
            eprintln!(
                "保留用户所有的导出 {0} / preserving user-owned export {0}",
                destination.display()
            );
            continue;
        }
        if current.as_ref() != Some(&digest) {
            let parent = destination.parent().unwrap();
            fs::create_dir_all(parent)?;
            let mut file = tempfile::NamedTempFile::new_in(parent)?;
            file.as_file()
                .set_permissions(fs::Permissions::from_mode(item.mode))?;
            file.write_all(&item.bytes)?;
            file.as_file().sync_all()?;
            file.persist(&destination).map_err(|e| e.error)?;
            fs::File::open(parent)?.sync_all()?;
        }
        next.files.insert(path, digest);
    }
    let bytes = toml::to_string(&next).map_err(|e| Error::Invalid(e.to_string()))?;
    let mut file = tempfile::NamedTempFile::new_in(&state)?;
    file.write_all(bytes.as_bytes())?;
    file.as_file().sync_all()?;
    file.persist(record).map_err(|e| e.error)?;
    fs::File::open(state)?.sync_all()?;
    Ok(())
}
/// Validate the subsystem component used in user filenames.
/// 校验用户文件名使用的子系统分量。
/// # Arguments
/// - `id`: Untrusted manifest ID. 不可信清单标识。
fn protocol_id(id: &str) -> Result<()> {
    super::protocol::validate_id(id)?;
    Ok(())
}

/// Internal helper entry, invoked after permanent UID/GID dropping.
/// 永久放弃 UID/GID 特权后调用的内部辅助入口。
/// # Arguments
/// No parameters; receives its bounded manifest through stdin. 无参数；从 stdin 接收有界清单。
/// # Errors
/// Rejects a home other than the effective user's NSS home.
/// 拒绝不同于有效用户 NSS home 的目标。
pub fn user_entry() -> Result<()> {
    let mut bytes = Vec::new();
    std::io::stdin()
        .take(MAX_MANIFEST_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_MANIFEST_BYTES {
        return Err(Error::Invalid(
            "导出清单过大 / export manifest too large".into(),
        ));
    }
    let manifest: Manifest =
        serde_json::from_slice(&bytes).map_err(|e| Error::Invalid(e.to_string()))?;
    let (home, _) = account(unsafe { libc::geteuid() })?;
    if manifest.home != home {
        return Err(Error::Invalid(
            "导出主目录与调用者不匹配 / export home does not match caller".into(),
        ));
    }
    apply(manifest)
}

#[cfg(test)]
mod tests {
    use super::*;
    /// Large manifests do not block before waiting, and helper errors stay bounded.
    /// 大型清单不会在等待前阻塞，辅助程序错误保持有界。
    /// # Arguments
    /// No parameters. 无参数。
    #[test]
    fn bounded_helper_preserves_input_and_status() {
        let input = vec![b'x'; 256 * 1024];
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "cat >&2; exit 23"]);
        let output = helper_output(command, &input, Duration::from_secs(2)).unwrap();
        assert_eq!(output.status.code(), Some(23));
        assert_eq!(output.stderr, input);
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "head -c 1048577 /dev/zero >&2"]);
        assert!(helper_output(command, &[], Duration::from_secs(2)).is_err());
    }
    /// Stalled user-home operations are interrupted without an unbounded pipe wait.
    /// 停滞的用户 home 操作被中断，不产生无界管道等待。
    /// # Arguments
    /// No parameters. 无参数。
    #[test]
    fn bounded_helper_interrupts_nonreading_child() {
        let mut command = Command::new("/bin/sleep");
        command.arg("5");
        let started = Instant::now();
        assert!(
            helper_output(command, &vec![b'x'; 256 * 1024], Duration::from_millis(40)).is_err()
        );
        assert!(started.elapsed() < Duration::from_secs(2));
    }
    /// Synchronization and withdrawal preserve user edits and foreign links.
    /// 同步和撤销保留用户编辑及外部链接。
    /// # Arguments
    /// No parameters. 无参数。
    #[test]
    fn ownership_preserves_user_files() {
        let directory = tempfile::tempdir().unwrap();
        let manifest = |entries| Manifest {
            schema_version: 1,
            manager: "a".repeat(64),
            id: "apps".into(),
            home: directory.path().into(),
            entries,
        };
        let path = ".local/bin/apps-test".to_owned();
        apply(manifest(BTreeMap::from([(
            path.clone(),
            Item {
                bytes: b"original".to_vec(),
                mode: 0o755,
            },
        )])))
        .unwrap();
        fs::write(directory.path().join(&path), "edited").unwrap();
        apply(manifest(BTreeMap::new())).unwrap();
        assert_eq!(fs::read(directory.path().join(path)).unwrap(), b"edited");
        assert!(entry_path(".local/bin/apps-../../other", "apps").is_err());
    }
    /// Foreign symlinks survive synchronization while untouched owned files are withdrawn.
    /// 外部符号链接在同步后保留，未改动自有文件被撤销。
    /// # Arguments
    /// No parameters. 无参数。
    #[test]
    fn does_not_replace_external_links() {
        let directory = tempfile::tempdir().unwrap();
        let bin = directory.path().join(".local/bin");
        fs::create_dir_all(&bin).unwrap();
        let foreign = directory.path().join("foreign");
        fs::write(&foreign, "keep").unwrap();
        std::os::unix::fs::symlink(&foreign, bin.join("apps-link")).unwrap();
        let manifest = |entries| Manifest {
            schema_version: 1,
            manager: "b".repeat(64),
            id: "apps".into(),
            home: directory.path().into(),
            entries,
        };
        apply(manifest(BTreeMap::from([
            (
                ".local/bin/apps-link".into(),
                Item {
                    bytes: b"replace".to_vec(),
                    mode: 0o755,
                },
            ),
            (
                ".local/bin/apps-owned".into(),
                Item {
                    bytes: b"owned".to_vec(),
                    mode: 0o755,
                },
            ),
        ])))
        .unwrap();
        assert!(
            fs::symlink_metadata(bin.join("apps-link"))
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(fs::read(foreign).unwrap(), b"keep");
        apply(manifest(BTreeMap::new())).unwrap();
        assert!(bin.join("apps-link").exists());
        assert!(!bin.join("apps-owned").exists());
    }
    /// Generated desktop arguments remain literal through both quoting layers.
    /// 生成的 desktop 参数经两层引用后仍保持字面内容。
    /// # Arguments
    /// No parameters. 无参数。
    #[test]
    fn desktop_wrapper_quotes_literal_paths() {
        let args = [
            "/a path/program",
            "/manager/$literal;%value",
            "apps",
            "/foo\\bar\".desktop",
            "`literal`",
            "",
            r#"\\followed-by-""#,
        ];
        let exec = args
            .iter()
            .map(|s| exec_word(s))
            .collect::<Vec<_>>()
            .join(" ");
        let entry = desktop::parse(&format!(
            "[Desktop Entry]\nType=Application\nName=App\nExec={exec}\n"
        ))
        .unwrap();
        assert_eq!(desktop::expand(&entry, "/x.desktop", &[]).unwrap(), args);
        assert_eq!(exec_word("\"").as_bytes(), b"\"\\\\\"\"");
    }
}
