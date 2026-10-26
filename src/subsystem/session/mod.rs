//! Connect only the authenticated caller's graphical and audio resources.
//! 仅接入已认证调用者的图形与音频资源。
mod bus;
mod mount_helper;
mod mounts;
mod resources;
#[cfg(test)]
mod tests;

use super::{Error, PreparedRuntime, Result, commands, filesystem::open_at_guest};
use bus::{native_bus, private_bus};
use mounts::{bind, context, mount_command};
use resources::{component, resources, xdisplay};
use std::{
    collections::BTreeMap,
    fs::File,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    sync::Mutex,
};

pub(crate) use mount_helper::entry as mount_entry;

type BindIdentity = (u64, u64);
static SESSIONS: Mutex<BTreeMap<(String, u32), BTreeMap<PathBuf, BindIdentity>>> =
    Mutex::new(BTreeMap::new());

struct Resource {
    source: File,
    target: PathBuf,
    read_only: bool,
}
struct Context {
    root: File,
    namespace: File,
}

/// Reject missing authorization at every privileged session entry.
/// 在每个特权会话入口拒绝缺失的授权。
/// # Arguments
/// - `runtime`, `uid`: Runtime policy and kernel-authenticated UID. 运行策略和内核认证 UID。
fn authorize(runtime: &PreparedRuntime, uid: u32) -> Result<()> {
    if !runtime.config.allows(uid) {
        return Err(Error::Invalid(
            "子系统会话访问被拒绝 / subsystem session access denied".into(),
        ));
    }
    Ok(())
}

/// Refresh caller resources, including binds hidden by guest logind.
/// 刷新调用者资源，包括被容器 logind 遮蔽的绑定。
/// # Arguments
/// - `runtime`, `uid`, `environment`: Authorized runtime, caller and session hints. 已授权运行态、调用者及会话提示。
/// # Errors
/// Rejects mismatched identities, unsafe paths, missing dependencies and failed mounts.
/// 拒绝身份不匹配、不安全路径、缺失依赖和挂载失败。
pub fn refresh(
    runtime: &PreparedRuntime,
    uid: u32,
    environment: &BTreeMap<String, String>,
) -> Result<()> {
    authorize(runtime, uid)?;
    if runtime.config.session_mode == "off" {
        return Ok(());
    }
    let mut resources = resources(uid, environment)?;
    let mut sessions = SESSIONS
        .lock()
        .map_err(|_| Error::Invalid("会话锁损坏 / session lock poisoned".into()))?;
    let context = context(runtime)?;
    let result = (|| -> Result<()> {
        private_bus(runtime, uid, &context)?;
        if runtime.config.session_mode == "native" {
            resources.push(native_bus(runtime, uid, environment)?);
        }
        let desired: BTreeMap<_, _> = resources
            .iter()
            .map(|r| {
                let metadata = r.source.metadata()?;
                Ok((r.target.clone(), (metadata.dev(), metadata.ino())))
            })
            .collect::<Result<_>>()?;
        let key = (runtime.machine.clone(), uid);
        if let Some(previous) = sessions.get(&key) {
            for (path, identity) in previous
                .iter()
                .filter(|(path, _)| !desired.contains_key(*path))
            {
                if let Ok(target) =
                    open_at_guest(&context.root, path, libc::O_PATH | libc::O_NOFOLLOW)
                {
                    let metadata = target.metadata()?;
                    if (metadata.dev(), metadata.ino()) == *identity {
                        mount_command(&context, None, &target, path, "unmount")?;
                    }
                }
            }
        }
        for resource in resources {
            let previous = sessions
                .get(&key)
                .and_then(|bindings| bindings.get(&resource.target))
                .copied();
            bind(runtime, &context, uid, &resource, previous)?;
        }
        sessions.insert(key, desired);
        Ok(())
    })();
    if result.is_err() && runtime.config.session_mode == "native" {
        let _ = bus::stop_proxy(runtime, uid);
    }
    result
}

/// Construct systemd-run environment flags without interpreting shell text.
/// 构造 systemd-run 环境参数，不解释 shell 文本。
/// # Arguments
/// - `runtime`, `uid`, `environment`, `argv`: Authorized caller, session hints and literal application. 已授权调用者、会话提示及字面应用。
/// # Errors
/// Rejects unauthorized callers and malformed display/socket hints. 拒绝未授权调用者和无效显示/socket 提示。
pub fn command_arguments(
    runtime: &PreparedRuntime,
    uid: u32,
    environment: &BTreeMap<String, String>,
    _argv: &[String],
) -> Result<Vec<String>> {
    authorize(runtime, uid)?;
    let mut values = BTreeMap::from([(
        "PATH".to_owned(),
        "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin".to_owned(),
    )]);
    for (key, value) in environment {
        if matches!(key.as_str(), "LANG" | "LANGUAGE" | "TERM" | "COLORTERM")
            || (key.starts_with("LC_") && key.bytes().all(|b| b.is_ascii_uppercase() || b == b'_'))
        {
            values.insert(key.clone(), value.clone());
        }
    }
    if runtime.config.session_mode != "off" {
        let host = PathBuf::from(format!("/run/user/{uid}"));
        values.insert(
            "XDG_RUNTIME_DIR".into(),
            host.to_string_lossy().into_owned(),
        );
        values.insert(
            "DBUS_SESSION_BUS_ADDRESS".into(),
            format!(
                "unix:path={}/{}",
                host.display(),
                if runtime.config.session_mode == "native" {
                    "rstarsleep-host-bus"
                } else {
                    "rstarsleep-bus"
                }
            ),
        );
        if let Some(value) = environment.get("WAYLAND_DISPLAY").filter(|s| !s.is_empty()) {
            let name = if Path::new(value).is_absolute() {
                Path::new(value)
                    .strip_prefix(&host)
                    .ok()
                    .and_then(|p| p.to_str())
                    .ok_or_else(|| {
                        Error::Invalid("Wayland 路径无效 / invalid Wayland path".into())
                    })?
            } else {
                value
            };
            values.insert("WAYLAND_DISPLAY".into(), component(name)?.into());
            values.insert("XDG_SESSION_TYPE".into(), "wayland".into());
        }
        if let Some(value) = environment.get("DISPLAY").filter(|s| !s.is_empty()) {
            xdisplay(value)?;
            values.insert("DISPLAY".into(), value.clone());
            if environment.get("XAUTHORITY").is_some_and(|s| !s.is_empty()) {
                values.insert(
                    "XAUTHORITY".into(),
                    format!("{}/rstarsleep/Xauthority", host.display()),
                );
            }
            values
                .entry("XDG_SESSION_TYPE".into())
                .or_insert_with(|| "x11".into());
        }
        let sessions = SESSIONS
            .lock()
            .map_err(|_| Error::Invalid("会话锁损坏 / session lock poisoned".into()))?;
        if let Some(paths) = sessions.get(&(runtime.machine.clone(), uid)) {
            if paths.contains_key(&host.join("pulse/native")) {
                values.insert(
                    "PULSE_SERVER".into(),
                    format!("unix:{}/pulse/native", host.display()),
                );
            }
            let pipewire = environment
                .get("PIPEWIRE_REMOTE")
                .map(String::as_str)
                .unwrap_or("pipewire-0");
            if paths.contains_key(&host.join(pipewire)) {
                values.insert("PIPEWIRE_REMOTE".into(), pipewire.into());
            }
        }
        if runtime.config.session_mode == "native" {
            for key in ["GTK_IM_MODULE", "QT_IM_MODULE", "XMODIFIERS"] {
                if let Some(value) = environment.get(key) {
                    values.insert(key.into(), value.clone());
                }
            }
        }
    }
    Ok(values
        .into_iter()
        .map(|(key, value)| format!("--setenv={key}={value}"))
        .collect())
}

/// Remove host proxies even after daemon recovery; guest binds die with the machine.
/// 即使守护进程恢复后也移除宿主代理；容器绑定随机器结束。
/// # Arguments
/// - `runtime`: Stopped machine identity. 已停止机器身份。
/// # Errors
/// Returns invalid proxy receipts or process termination failures. 返回无效代理记录或进程终止失败。
pub fn cleanup(runtime: &PreparedRuntime) -> Result<()> {
    SESSIONS
        .lock()
        .map_err(|_| Error::Invalid("会话锁损坏 / session lock poisoned".into()))?
        .retain(|(machine, _), _| machine != &runtime.machine);
    bus::cleanup_proxies(runtime)
}

/// Explain unavailable local file arguments before launching a desktop application.
/// 在启动桌面应用前明确说明不可见的本地文件参数。
/// # Arguments
/// - `runtime`, `args`: Running machine and desktop file arguments. 运行机器及桌面文件参数。
/// # Errors
/// Returns inaccessible guest paths or unsupported encoded local file URIs.
/// 返回不可访问容器路径或不支持的编码本地文件 URI。
pub fn validate_files(runtime: &PreparedRuntime, args: &[String]) -> Result<()> {
    let paths: Vec<_> = args
        .iter()
        .filter_map(|value| {
            if value.starts_with('/') {
                Some(value.as_str())
            } else {
                value.strip_prefix("file://")
            }
        })
        .collect();
    if paths.is_empty() {
        return Ok(());
    }
    let context = context(runtime)?;
    for path in paths {
        if !path.starts_with('/') || path.contains('%') {
            return Err(Error::Invalid(
                "本地文件 URL 必须使用未编码的客体绝对路径 / local file URLs must use an absolute unencoded guest path".into(),
            ));
        }
        for mount in &runtime.config.mounts {
            if let Ok(relative) = Path::new(path).strip_prefix(&mount.source) {
                let mapped = mount.target.join(relative);
                if mapped != Path::new(path) {
                    return Err(Error::Invalid(format!(
                        "宿主文件已挂载到其他客体路径，请传入 {0} 而不是 {path} / host file is mounted under another guest path; pass {0} instead of {path}",
                        mapped.display()
                    )));
                }
            }
        }
        open_at_guest(&context.root, Path::new(path), libc::O_PATH).map_err(|_|Error::Invalid(format!("文件在子系统 {0} 中不可见，请配置运行时挂载或使用其客体路径: {path} / file is not visible in subsystem {0}; configure a runtime mount or use its guest path: {path}", runtime.system_id)))?;
    }
    Ok(())
}
