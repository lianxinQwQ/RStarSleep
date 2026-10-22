//! Instance-bound administration, captured build inputs and durable persistence.
//! 实例绑定的管理、构建输入保存及持久化迁移。
use super::{
    Error, PreparedRuntime, Result, RuntimeMount, SubsystemConfig, atomic_write, checked, commands,
    execution, filesystem, identity, load_prepared,
    protocol::{self, ClientMessage, Event, Operation},
    read_record, trust, write_record,
};
use crate::{
    config::{ConfigStore, InheritanceConfig},
    controller::{self, BuildBackend, SystemBackend},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    ffi::CString,
    fs::{self, File, OpenOptions},
    io::{self, BufReader},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{
            fs::{FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt},
            net::{UnixListener, UnixStream},
            process::CommandExt,
        },
    },
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

/// The guest sees only its fixed endpoint and the separately bound executable.
/// 容器只看到固定端点及单独绑定的可执行程序。
pub const SOCKET: &str = "/run/rstarsleep/guest.sock";
mod authentication;
mod build;
mod copy;
mod migration;
mod mutation;
#[cfg(test)]
mod tests;
#[cfg(test)]
use authentication::mount_path;
use authentication::{
    authenticate, key, mounts_at, open_target, same_inode, still_running, target_path,
};
use build::{install_packages, rebuild};
use migration::{add_mount, migrate};
use mutation::add_copy;

static ENDPOINTS: OnceLock<Mutex<BTreeMap<PathBuf, Arc<Endpoint>>>> = OnceLock::new();

struct Endpoint {
    store: ConfigStore,
    id: String,
    mutation: Mutex<()>,
    connections: AtomicUsize,
}
struct Connection(Arc<Endpoint>);
impl Drop for Connection {
    fn drop(&mut self) {
        self.0.connections.fetch_sub(1, Ordering::SeqCst);
    }
}
struct GuestRoot {
    leader: u32,
    namespace: File,
    root: File,
}

impl Endpoint {
    /// Authenticate each request before interpreting its fixed-instance mutation.
    /// 解释固定实例变更前认证每个请求。
    /// # Arguments
    /// - `stream`: Untrusted incoming connection. 不可信进入连接。
    fn serve(&self, mut stream: UnixStream) -> Result<()> {
        stream.set_read_timeout(Some(Duration::from_secs(5)))?;
        stream.set_write_timeout(Some(Duration::from_secs(10)))?;
        let peer = identity::credentials(stream.as_raw_fd())?;
        let runtime = load_prepared(&self.store, &self.id)?;
        let root = authenticate(&runtime, peer)?;
        let expiry = stream.try_clone()?;
        let complete = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let done = complete.clone();
        thread::spawn(move || {
            for _ in 0..50 {
                if done.load(Ordering::SeqCst) {
                    return;
                }
                thread::sleep(Duration::from_millis(100));
            }
            let _ = expiry.shutdown(std::net::Shutdown::Both);
        });
        let first = protocol::read_frame::<ClientMessage>(&mut BufReader::new(stream.try_clone()?));
        complete.store(true, Ordering::SeqCst);
        let request = match first? {
            Some(ClientMessage::Request { request }) => request,
            _ => {
                return Err(Error::Protocol(
                    "需要初始客体请求 / initial guest request required".into(),
                ));
            }
        };
        protocol::validate_request(&request)?;
        let (action, path, packages) = match request.operation {
            Operation::Guest {
                action,
                path,
                packages,
            } => (action, path, packages),
            _ => {
                return Err(Error::Invalid(
                    "客体端点仅接受固定实例的客体操作 / guest endpoint accepts only fixed-instance guest operations".into(),
                ));
            }
        };
        trust::store(&self.store, Some(&self.id))?;
        let _mutation = self.mutation.try_lock().map_err(|_| {
            Error::Invalid(
                "另一个客体管理操作正在进行 / another guest management operation is active".into(),
            )
        })?;
        let target = path.as_deref().map(Path::new);
        let result = match action.as_str() {
            "add-copy" | "add-mount" | "migrate" => {
                if !packages.is_empty() {
                    return Err(Error::Invalid(
                        "路径操作不接受软件包名称 / path operation does not accept package names"
                            .into(),
                    ));
                }
                let target = target
                    .ok_or_else(|| Error::Invalid("需要客体路径 / guest path required".into()))?;
                match action.as_str() {
                    "add-copy" => add_copy(&self.store, &runtime, &root, target),
                    "add-mount" => add_mount(&self.store, &runtime, &root, target),
                    _ => migrate(&self.store, &runtime, &root, target),
                }
            }
            "install" => {
                if target.is_some() {
                    return Err(Error::Invalid(
                        "客体安装不接受路径 / guest install does not accept a path".into(),
                    ));
                }
                install_packages(&self.store, &self.id, &packages)?;
                rebuild(&self.store, &self.id)
            }
            "rebuild" => {
                if target.is_some() || !packages.is_empty() {
                    return Err(Error::Invalid(
                        "客体重建不接受额外参数 / guest rebuild accepts no extra arguments".into(),
                    ));
                }
                rebuild(&self.store, &self.id)
            }
            "restart" => {
                if target.is_some() || !packages.is_empty() {
                    return Err(Error::Invalid(
                        "客体重启不接受额外参数 / guest restart accepts no extra arguments".into(),
                    ));
                }
                SubsystemConfig::read(&self.store, &self.id)?;
                still_running(&runtime, &root)?;
                protocol::write_frame(
                    &mut stream,
                    &Event::Message {
                        text: "已安排重启；使用当前成功产物并完成启动前任务".into(),
                    },
                )?;
                protocol::write_frame(
                    &mut stream,
                    &Event::Result {
                        statuses: Vec::new(),
                    },
                )?;
                let store = self.store.clone();
                let id = self.id.clone();
                thread::spawn(move || {
                    thread::sleep(Duration::from_millis(100));
                    let result = super::client::socket_for_root(store.root()).and_then(|socket| {
                        super::client::request(&socket, Operation::Restart { id }).map(|_| ())
                    });
                    if let Err(error) = result {
                        eprintln!("客体重启 / guest restart: {error}");
                    }
                });
                return Ok(());
            }
            _ => Err(Error::Invalid("未知客体操作 / unknown guest action".into())),
        }?;
        protocol::write_frame(&mut stream, &Event::Message { text: result })?;
        protocol::write_frame(
            &mut stream,
            &Event::Result {
                statuses: Vec::new(),
            },
        )?;
        Ok(())
    }
}

/// Create one root-only endpoint whose identity is never selected by guest input.
/// 创建单个 root 专用端点，身份绝不由容器输入选择。
/// # Arguments
/// - `store`: Trusted fixed manager. 可信固定管理器。
/// - `runtime`: Fixed registered runtime. 固定已注册运行态。
/// # Errors
/// Rejects duplicate/untrusted endpoint paths and listener failures.
/// 拒绝重复或不可信端点路径以及监听失败。
pub fn listen(store: &ConfigStore, runtime: &PreparedRuntime) -> Result<PathBuf> {
    trust::store(store, Some(&runtime.system_id))?;
    let path = endpoint_path(store, &runtime.system_id)?;
    let parent = path.parent().unwrap().to_owned();
    checked(&path)?;
    fs::create_dir_all(&parent)?;
    trust::path(&parent)?;
    let endpoints = ENDPOINTS.get_or_init(|| Mutex::new(BTreeMap::new()));
    let mut endpoints = endpoints
        .lock()
        .map_err(|_| Error::Invalid("客体端点锁损坏 / guest endpoint lock poisoned".into()))?;
    if let Some(endpoint) = endpoints.get(&path) {
        if endpoint.id != runtime.system_id || endpoint.store.root() != store.root() {
            return Err(Error::Invalid(
                "客体端点身份冲突 / guest endpoint identity conflict".into(),
            ));
        }
        return Ok(path);
    }
    if let Ok(metadata) = fs::symlink_metadata(&path) {
        if !metadata.file_type().is_socket() || metadata.uid() != 0 {
            return Err(Error::Invalid(
                "客体端点对象不符合预期 / unexpected guest endpoint object".into(),
            ));
        }
        trust::path(&path)?;
        fs::remove_file(&path)?;
    }
    let listener = UnixListener::bind(&path)?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
    let endpoint = Arc::new(Endpoint {
        store: store.clone(),
        id: runtime.system_id.clone(),
        mutation: Mutex::new(()),
        connections: AtomicUsize::new(0),
    });
    endpoints.insert(path.clone(), endpoint.clone());
    thread::spawn(move || {
        for stream in listener.incoming() {
            let stream = match stream {
                Ok(stream) => stream,
                Err(error) => {
                    eprintln!("客体监听器 / guest listener: {error}");
                    continue;
                }
            };
            if endpoint.connections.fetch_add(1, Ordering::SeqCst) >= 16 {
                endpoint.connections.fetch_sub(1, Ordering::SeqCst);
                continue;
            }
            let endpoint = endpoint.clone();
            thread::spawn(move || {
                let _connection = Connection(endpoint.clone());
                let response = stream.try_clone();
                if let Err(error) = endpoint.serve(stream) {
                    if let Ok(mut response) = response {
                        let _ = protocol::write_frame(
                            &mut response,
                            &Event::Error {
                                message: error.to_string(),
                            },
                        );
                    }
                    eprintln!("客体 {0} / guest {0}: {error}", endpoint.id);
                }
            });
        }
    });
    Ok(path)
}

/// Keep long identifiers within Linux's 108-byte Unix socket address limit.
/// 使长标识保持在 Linux 的 108 字节 Unix socket 地址限制内。
/// # Arguments
/// - `store`, `id`: Fixed endpoint identity. 固定端点身份。
fn endpoint_path(store: &ConfigStore, id: &str) -> Result<PathBuf> {
    let parent = super::client::socket_for_root(store.root())?
        .parent()
        .unwrap()
        .to_owned();
    let digest = format!("{:x}", Sha256::digest(id.as_bytes()));
    Ok(parent.join(format!("g-{}", &digest[..24])))
}

/// Restore the dedicated endpoint when a registered machine survives daemon restart.
/// 已注册机器跨 daemon 重启存活时恢复专用端点。
/// # Arguments
/// - `store`, `runtime`: Trusted manager and surviving machine. 可信管理器及存活机器。
/// # Errors
/// Rejects changed machine identities or failed/hidden live binds.
/// 拒绝变化的机器身份或失败、不可见的运行绑定。
pub(crate) fn reconnect(store: &ConfigStore, runtime: &PreparedRuntime) -> Result<()> {
    let endpoint = listen(store, runtime)?;
    let root = filesystem::open_runtime_root(runtime)?;
    let visible =
        filesystem::open_at_guest(&root, Path::new(SOCKET), libc::O_PATH | libc::O_NOFOLLOW);
    if let Ok(file) = visible
        && same_inode(&file.metadata()?, &fs::symlink_metadata(&endpoint)?)
    {
        return Ok(());
    }
    let mut command = Command::new(commands::MACHINECTL);
    command
        .args(["bind", "--mkdir", &runtime.machine])
        .arg(&endpoint)
        .arg(SOCKET);
    let output = commands::output(command, Duration::from_secs(15))?;
    if !output.status.success() {
        return Err(Error::Invalid(format!(
            "无法恢复客体端点: {0} / cannot restore guest endpoint: {0}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    let visible =
        filesystem::open_at_guest(&root, Path::new(SOCKET), libc::O_PATH | libc::O_NOFOLLOW)?;
    if !same_inode(&visible.metadata()?, &fs::symlink_metadata(&endpoint)?) {
        return Err(Error::Invalid(
            "恢复的客体端点在机器内不可见 / restored guest endpoint is not visible in the machine"
                .into(),
        ));
    }
    Ok(())
}
