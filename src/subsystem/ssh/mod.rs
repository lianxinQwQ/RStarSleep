//! Public-key SSH management transport with no host command execution.
//! 不执行宿主命令的公钥 SSH 管理传输。
mod session;
mod sftp;
#[cfg(test)]
mod tests;
use super::{Error, Result};
use crate::config::ConfigStore;
use russh::{
    Channel, ChannelId, Pty,
    keys::{Algorithm, PrivateKey, PublicKey},
    server::{self, Auth, Msg, Server as _, Session},
};
use serde::Deserialize;
use std::{
    collections::HashMap,
    fs,
    net::{SocketAddr, TcpListener},
    path::PathBuf,
    sync::Arc,
    time::Duration,
};
use tokio::sync::{oneshot, watch};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Settings {
    schema_version: u32,
    #[serde(default = "listen_default")]
    listen: SocketAddr,
    #[serde(default = "key_default")]
    host_key: PathBuf,
}
/// Supply a loopback-only management listener.
/// 提供仅回环可访问的管理监听地址。
/// # Arguments
/// No parameters. 无参数。
fn listen_default() -> SocketAddr {
    SocketAddr::from(([127, 0, 0, 1], 2222))
}
/// Locate the persistent host identity outside subsystem-editable inputs.
/// 在子系统可编辑输入之外定位持久主机身份。
/// # Arguments
/// No parameters. 无参数。
fn key_default() -> PathBuf {
    "config/ssh_host_ed25519_key".into()
}

pub struct Service {
    stop: Option<oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl Drop for Service {
    /// Stop the listener and release its runtime when the daemon exits.
    /// daemon 退出时停止监听并释放运行时。
    /// # Arguments
    /// - `self`: Active SSH service. 活动 SSH 服务。
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Load and validate every authorization line; options are deliberately unsupported.
/// 加载并验证全部授权行；明确不支持选项。
/// # Arguments
/// - `text`: OpenSSH public-key lines and comments. OpenSSH 公钥行及注释。
fn parse_keys(text: &str) -> Result<Vec<PublicKey>> {
    let mut keys = Vec::new();
    for (line, value) in text.lines().enumerate() {
        let value = value.trim();
        if value.is_empty() || value.starts_with('#') {
            continue;
        }
        let key = PublicKey::from_openssh(value).map_err(|e| Error::Invalid(format!("authorized_keys 第 {} 行公钥无效或包含不支持的选项 / invalid public key or unsupported options on authorized_keys line {}: {e}",line+1,line+1)))?;
        keys.push(key);
    }
    Ok(keys)
}

/// Reload roles and public keys on every authentication attempt.
/// 每次认证尝试均重载角色和公钥。
/// # Arguments
/// - `store`, `id`, `key`: Fixed manager, requested identity and proved key. 固定管理器、请求身份及已证明公钥。
fn authorized(store: &ConfigStore, id: &str, key: &PublicKey) -> Result<bool> {
    super::protocol::validate_id(id)?;
    super::SubsystemConfig::read(store, id)?;
    let path = store
        .system_layout(id)?
        .config
        .join("subsystem/authorized_keys");
    if !path.try_exists()? {
        return Ok(false);
    }
    super::trust::path(&path)?;
    let text = fs::read_to_string(&path)?;
    if text.len() > 1024 * 1024 {
        return Err(Error::Invalid(
            "公钥文件超出限制 / authorized key file exceeds limit".into(),
        ));
    }
    Ok(parse_keys(&text)?
        .iter()
        .any(|candidate| candidate.key_data() == key.key_data()))
}

/// Start SSH only after static configuration, host key and listener checks succeed.
/// 静态配置、主机密钥和监听检查成功后启动 SSH。
/// # Arguments
/// - `store`: Fixed trusted manager. 固定可信管理器。
/// # Errors
/// Missing ssh.toml disables SSH; invalid supplied configuration fails startup.
/// 缺少 ssh.toml 时禁用 SSH；提供的配置无效时启动失败。
pub fn start(store: &ConfigStore) -> Result<Option<Service>> {
    start_with_builds(store, Arc::new(super::daemon::BuildState::default()))
}

/// Share the daemon's build state across all SSH connections and channels.
/// 在全部 SSH 连接及通道间共享 daemon 的构建状态。
pub(crate) fn start_with_builds(
    store: &ConfigStore,
    builds: Arc<super::daemon::BuildState>,
) -> Result<Option<Service>> {
    let path = store.root().join("config/ssh.toml");
    if !path.try_exists()? {
        return Ok(None);
    }
    super::trust::path(&path)?;
    let settings: Settings = toml::from_str(&fs::read_to_string(&path)?)
        .map_err(|e| Error::Invalid(format!("SSH 配置无效 / invalid SSH configuration: {e}")))?;
    if settings.schema_version != 1 || settings.listen.port() == 0 {
        return Err(Error::Invalid(
            "SSH 配置版本或端口无效 / invalid SSH schema or port".into(),
        ));
    }
    let key_path = if settings.host_key.is_absolute() {
        settings.host_key.clone()
    } else {
        store.root().join(&settings.host_key)
    };
    super::checked(&key_path)?;
    super::trust::path(
        key_path
            .parent()
            .ok_or_else(|| Error::Invalid("主机密钥路径无效 / invalid host key path".into()))?,
    )?;
    for id in store.runtime_config()?.subsystems {
        super::trust::store(store, Some(&id))?;
        super::SubsystemConfig::read(store, &id)?;
        let path = store
            .system_layout(&id)?
            .config
            .join("subsystem/authorized_keys");
        if path.try_exists()? {
            parse_keys(&fs::read_to_string(path)?)?;
        }
    }
    let existing = if key_path.try_exists()? {
        super::trust::path(&key_path)?;
        use std::os::unix::fs::PermissionsExt;
        if fs::metadata(&key_path)?.permissions().mode() & 0o077 != 0 {
            return Err(Error::Invalid("SSH 主机私钥必须仅管理员可读写 / SSH host private key must be accessible only to its owner".into()));
        }
        Some(PrivateKey::from_openssh(fs::read(&key_path)?).map_err(|e| {
            Error::Invalid(format!(
                "SSH 主机私钥无效 / invalid SSH host private key: {e}"
            ))
        })?)
    } else {
        None
    };
    let listener = TcpListener::bind(settings.listen)?;
    listener.set_nonblocking(true)?;
    let key = match existing {
        Some(key) => key,
        None => {
            let key = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).map_err(|e| {
                Error::Invalid(format!("无法生成主机密钥 / cannot generate host key: {e}"))
            })?;
            let encoded = key
                .to_openssh(russh::keys::ssh_key::LineEnding::LF)
                .map_err(|e| {
                    Error::Invalid(format!("无法编码主机密钥 / cannot encode host key: {e}"))
                })?;
            // Never overwrite a host identity created by another process.
            // 绝不覆盖其他进程创建的主机身份。
            use std::{io::Write, os::unix::fs::OpenOptionsExt};
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&key_path)?;
            file.write_all(encoded.as_bytes())?;
            file.sync_all()?;
            fs::File::open(key_path.parent().unwrap())?.sync_all()?;
            key
        }
    };
    let config = Arc::new(server::Config {
        methods: (&[russh::MethodKind::PublicKey][..]).into(),
        keys: vec![key],
        auth_rejection_time: Duration::from_millis(500),
        auth_rejection_time_initial: Some(Duration::from_millis(500)),
        inactivity_timeout: None,
        max_auth_attempts: 6,
        ..Default::default()
    });
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    let (stop, cancelled) = oneshot::channel();
    let store = store.clone();
    let thread = std::thread::Builder::new().name("rstarsleep-ssh".into()).spawn(move || {
        let result = runtime.block_on(async move {
            let listener = tokio::net::TcpListener::from_std(listener)?;
            let mut server = SshServer { store, builds, authorize: authorized };
            let running = server.run_on_socket(config,&listener); let handle = running.handle();
            tokio::pin!(running);
            tokio::select! { result = &mut running => result, _ = cancelled => { handle.shutdown("管理器退出 / manager shutdown".into()); running.await } }
        });
        if let Err(error) = result { eprintln!("SSH 服务失败 / SSH service failed: {error}"); }
    })?;
    Ok(Some(Service {
        stop: Some(stop),
        thread: Some(thread),
    }))
}

struct SshServer {
    store: ConfigStore,
    builds: Arc<super::daemon::BuildState>,
    authorize: fn(&ConfigStore, &str, &PublicKey) -> Result<bool>,
}
impl server::Server for SshServer {
    type Handler = Connection;
    /// Create an unauthenticated connection with no host user identity.
    /// 创建无宿主用户身份的未认证连接。
    /// # Arguments
    /// - `self`, `_peer`: Manager and remote address. 管理器及远端地址。
    fn new_client(&mut self, _peer: Option<SocketAddr>) -> Connection {
        Connection {
            store: self.store.clone(),
            builds: self.builds.clone(),
            authorize: self.authorize,
            id: None,
            channels: HashMap::new(),
        }
    }
    /// Report protocol failures without logging credentials.
    /// 报告协议失败，不记录凭据。
    /// # Arguments
    /// - `self`, `error`: Server and failure. 服务端及失败。
    fn handle_session_error(&mut self, error: Error) {
        eprintln!("SSH 会话失败 / SSH session failed: {error}");
    }
}
struct ChannelState {
    channel: Option<Channel<Msg>>,
    terminal: Option<(String, u32, u32)>,
    resize: watch::Sender<(u32, u32)>,
    sizes: Option<watch::Receiver<(u32, u32)>>,
    _lifetime: oneshot::Sender<()>,
    closed: Option<oneshot::Receiver<()>>,
}
struct Connection {
    store: ConfigStore,
    builds: Arc<super::daemon::BuildState>,
    authorize: fn(&ConfigStore, &str, &PublicKey) -> Result<bool>,
    id: Option<String>,
    channels: HashMap<ChannelId, ChannelState>,
}
impl Connection {
    /// Start exactly one management operation per SSH channel.
    /// 每个 SSH 通道仅启动一个管理操作。
    /// # Arguments
    /// - `self`, `channel`, `command`, `session`: Connection, channel, optional exec and wire session. 连接、通道、可选执行命令及传输会话。
    fn launch(
        &mut self,
        channel: ChannelId,
        command: Option<Vec<u8>>,
        session: &mut Session,
    ) -> Result<()> {
        let id = self
            .id
            .clone()
            .ok_or_else(|| Error::Invalid("SSH 尚未认证 / SSH is not authenticated".into()))?;
        let state = self
            .channels
            .get_mut(&channel)
            .ok_or_else(|| Error::Invalid("SSH 通道不存在 / SSH channel does not exist".into()))?;
        let Some(wire) = state.channel.take() else {
            session.channel_failure(channel)?;
            return Ok(());
        };
        let terminal = state.terminal.clone();
        let sizes = state.sizes.take().unwrap();
        let closed = state.closed.take().unwrap();
        let store = self.store.clone();
        let builds = self.builds.clone();
        let handle = session.handle();
        session.channel_success(channel)?;
        tokio::spawn(async move {
            let result = tokio::select! {
                result = session::run(wire, store, builds, id, terminal, sizes, command, handle.clone()) => result,
                _ = closed => return,
            };
            let code = match result {
                Ok(code) => code,
                Err(error) => {
                    let _ = handle
                        .extended_data(
                            channel,
                            1,
                            format!("操作失败 / operation failed: {error}\r\n").into_bytes(),
                        )
                        .await;
                    1
                }
            };
            let _ = handle.exit_status_request(channel, code).await;
            let _ = handle.eof(channel).await;
            let _ = handle.close(channel).await;
        });
        Ok(())
    }
}
impl server::Handler for Connection {
    type Error = Error;
    /// Check an offered key without marking the connection authenticated.
    /// 检查候选公钥，不将连接标记为已认证。
    /// # Arguments
    /// - `self`, `user`, `key`: Connection, subsystem and offered key. 连接、子系统及候选公钥。
    async fn auth_publickey_offered(&mut self, user: &str, key: &PublicKey) -> Result<Auth> {
        Ok(
            if (self.authorize)(&self.store, user, key).unwrap_or_else(|error| {
                eprintln!("SSH 认证失败 / SSH authentication failed: {error}");
                false
            }) {
                Auth::Accept
            } else {
                Auth::reject()
            },
        )
    }
    /// Bind proved credentials to one subsystem for this connection.
    /// 将已证明凭据绑定到本连接的唯一子系统。
    /// # Arguments
    /// - `self`, `user`, `key`: Connection, subsystem and proved key. 连接、子系统及已证明公钥。
    async fn auth_publickey(&mut self, user: &str, key: &PublicKey) -> Result<Auth> {
        if (self.authorize)(&self.store, user, key).unwrap_or(false) {
            self.id = Some(user.into());
            Ok(Auth::Accept)
        } else {
            Ok(Auth::reject())
        }
    }
    /// Accept only authenticated session channels.
    /// 仅接受已认证会话通道。
    /// # Arguments
    /// - `self`, `channel`, `_session`: Connection, new channel and transport. 连接、新通道及传输。
    async fn channel_open_session(
        &mut self,
        channel: Channel<Msg>,
        _session: &mut Session,
    ) -> Result<bool> {
        if self.id.is_none() || self.channels.len() >= 16 {
            return Ok(false);
        }
        let (resize, sizes) = watch::channel((80, 24));
        let (lifetime, closed) = oneshot::channel();
        self.channels.insert(
            channel.id(),
            ChannelState {
                channel: Some(channel),
                terminal: None,
                resize,
                sizes: Some(sizes),
                _lifetime: lifetime,
                closed: Some(closed),
            },
        );
        Ok(true)
    }
    /// Store terminal metadata without allocating a management root filesystem.
    /// 保存终端元数据，不分配管理根文件系统。
    /// # Arguments
    /// - `self`, `channel`, `term`, `cols`, `rows`, `_width`, `_height`, `_modes`, `session`: Terminal request fields. 终端请求字段。
    async fn pty_request(
        &mut self,
        channel: ChannelId,
        term: &str,
        cols: u32,
        rows: u32,
        _width: u32,
        _height: u32,
        _modes: &[(Pty, u32)],
        session: &mut Session,
    ) -> Result<()> {
        if let Some(state) = self.channels.get_mut(&channel) {
            if cols > 0
                && rows > 0
                && cols <= 1000
                && rows <= 1000
                && term.len() <= 128
                && !term.chars().any(char::is_control)
                && state.channel.is_some()
            {
                state.terminal = Some((term.into(), cols, rows));
                session.channel_success(channel)?;
                return Ok(());
            }
        }
        session.channel_failure(channel)?;
        Ok(())
    }
    /// Start the interactive custom management terminal.
    /// 启动交互式自定义管理终端。
    /// # Arguments
    /// - `self`, `channel`, `session`: Connection, channel and transport. 连接、通道及传输。
    async fn shell_request(&mut self, channel: ChannelId, session: &mut Session) -> Result<()> {
        self.launch(channel, None, session)
    }
    /// Execute a management command without invoking a host shell.
    /// 执行管理命令，不调用宿主 shell。
    /// # Arguments
    /// - `self`, `channel`, `data`, `session`: Connection, channel, command and transport. 连接、通道、命令及传输。
    async fn exec_request(
        &mut self,
        channel: ChannelId,
        data: &[u8],
        session: &mut Session,
    ) -> Result<()> {
        if data.len() > 32768 {
            session.channel_failure(channel)?;
            return Ok(());
        }
        self.launch(channel, Some(data.into()), session)
    }
    /// Attach SFTP to the exact same configuration workspace as the CLI.
    /// 将 SFTP 接入与 CLI 完全相同的配置工作区。
    /// # Arguments
    /// - `self`, `channel`, `name`, `session`: Connection, channel, subsystem request and transport. 连接、通道、子协议请求及传输。
    async fn subsystem_request(
        &mut self,
        channel: ChannelId,
        name: &str,
        session: &mut Session,
    ) -> Result<()> {
        if name != "sftp" {
            session.channel_failure(channel)?;
            return Ok(());
        }
        let id = self
            .id
            .as_ref()
            .ok_or_else(|| Error::Invalid("SSH 尚未认证 / SSH is not authenticated".into()))?;
        let workspace = super::management::Workspace::new(self.store.clone(), id)?;
        let wire = self
            .channels
            .get_mut(&channel)
            .and_then(|state| state.channel.take());
        if let Some(wire) = wire {
            session.channel_success(channel)?;
            tokio::spawn(async move {
                russh_sftp::server::run(wire.into_stream(), sftp::Files::new(workspace)).await;
            });
        } else {
            session.channel_failure(channel)?;
        }
        Ok(())
    }
    /// Forward resize events to whichever terminal mode currently owns input.
    /// 将尺寸变化传给当前拥有输入的终端模式。
    /// # Arguments
    /// - `self`, `channel`, `cols`, `rows`, `_width`, `_height`, `_session`: Resize request fields. 尺寸请求字段。
    async fn window_change_request(
        &mut self,
        channel: ChannelId,
        cols: u32,
        rows: u32,
        _width: u32,
        _height: u32,
        _session: &mut Session,
    ) -> Result<()> {
        if cols > 0 && rows > 0 && cols <= 1000 && rows <= 1000 {
            if let Some(state) = self.channels.get_mut(&channel) {
                state.resize.send_replace((cols, rows));
            }
        }
        Ok(())
    }
    /// Release channel bookkeeping after the client closes it.
    /// 客户端关闭后释放通道记录。
    /// # Arguments
    /// - `self`, `channel`, `_session`: Connection, channel and transport. 连接、通道及传输。
    async fn channel_close(&mut self, channel: ChannelId, _session: &mut Session) -> Result<()> {
        self.channels.remove(&channel);
        Ok(())
    }
}
