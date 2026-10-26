//! Bounded authenticated Unix connections for one fixed manager.
//! 单个固定管理器的有界认证 Unix 连接。
use super::*;

struct Connection {
    manager: Arc<Manager>,
    uid: u32,
}
impl Drop for Connection {
    fn drop(&mut self) {
        if let Ok(mut counts) = self.manager.connections.lock() {
            if counts.get(&self.uid).is_some_and(|count| *count <= 1) {
                counts.remove(&self.uid);
            } else if let Some(count) = counts.get_mut(&self.uid) {
                *count -= 1;
            }
        }
    }
}

/// Apply an absolute deadline so partial-frame progress cannot keep a connection open.
/// 应用绝对截止时间，防止部分帧进展使连接无限保持。
/// # Arguments
/// - `reader`: Incoming buffered transport. 进入的缓冲传输。
/// - `timeout`: Total first-frame allowance. 首帧总时限。
pub(super) fn read_initial(
    reader: &mut BufReader<UnixStream>,
    timeout: Duration,
) -> Result<ClientMessage> {
    let expiry = reader.get_ref().try_clone()?;
    let initial = Arc::new(AtomicBool::new(false));
    let done = initial.clone();
    let deadline = Instant::now() + timeout;
    thread::Builder::new()
        .name("rstarsleep-frame-deadline".into())
        .spawn(move || {
            while !done.load(Ordering::Acquire) {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    let _ = expiry.shutdown(std::net::Shutdown::Both);
                    return;
                }
                thread::sleep(remaining.min(Duration::from_millis(100)));
            }
        })?;
    let first = protocol::read_frame::<ClientMessage>(reader);
    initial.store(true, Ordering::Release);
    first?.ok_or_else(|| Error::Protocol("缺少请求 / missing request".into()))
}

/// Run the privileged manager for a fixed trusted root.
/// 为固定可信管理根运行特权管理器。
/// # Arguments
/// - `store`: Administrator-controlled manager. 管理员控制管理器。
/// # Errors
/// Fails closed for untrusted paths, credentials or duplicate daemons.
/// 路径、凭据不可信或守护进程重复时封闭失败。
pub fn run(store: ConfigStore) -> Result<()> {
    identity::admin(Peer {
        uid: unsafe { libc::geteuid() },
        gid: unsafe { libc::getegid() },
        pid: std::process::id() as i32,
    })?;
    trust::store(&store, None)?;
    let socket = client::socket_for_root(store.root())?;
    let directory = socket.parent().unwrap().to_owned();
    fs::create_dir_all(&directory)?;
    trust::path(&directory)?;
    let guard = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(directory.join("daemon.lock"))?;
    trust::path(&directory.join("daemon.lock"))?;
    if unsafe { libc::flock(guard.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err(Error::Invalid(
            "守护进程已在运行 / daemon already running".into(),
        ));
    }
    if let Ok(m) = fs::symlink_metadata(&socket) {
        if !m.file_type().is_socket() || m.uid() != 0 {
            return Err(Error::Invalid(
                "守护进程套接字路径不符合预期 / unexpected daemon socket path".into(),
            ));
        }
        fs::remove_file(&socket)?;
    }
    let listener = UnixListener::bind(&socket)?;
    fs::set_permissions(&socket, fs::Permissions::from_mode(0o666))?;
    let manager = Arc::new(Manager {
        store,
        directory,
        slots: Mutex::new(BTreeMap::new()),
        connections: Mutex::new(BTreeMap::new()),
    });
    let _ssh = super::super::ssh::start(&manager.store)?;
    let monitor = manager.clone();
    thread::spawn(move || monitor.monitor());
    let _guard = guard;
    for stream in listener.incoming() {
        let stream = match stream {
            Ok(stream) => stream,
            Err(error) => {
                eprintln!("子系统接受连接 / subsystem accept: {error}");
                thread::sleep(Duration::from_millis(50));
                continue;
            }
        };
        let peer = match identity::credentials(stream.as_raw_fd()) {
            Ok(peer) => peer,
            Err(error) => {
                eprintln!("子系统对端 / subsystem peer: {error}");
                continue;
            }
        };
        {
            let mut counts = lock(&manager.connections)?;
            let total: usize = counts.values().sum();
            let count = counts.get(&peer.uid).copied().unwrap_or(0);
            if total >= 128 || (peer.uid != 0 && (total >= 120 || count >= 16)) {
                continue;
            }
            *counts.entry(peer.uid).or_default() += 1;
        }
        let connection = Connection {
            manager: manager.clone(),
            uid: peer.uid,
        };
        let manager = manager.clone();
        if let Err(error) = thread::Builder::new()
            .name(format!("rstarsleep-client-{}", peer.uid))
            .spawn(move || {
                let _connection = connection;
                let result = (|| -> Result<()> {
                    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
                    stream.set_write_timeout(Some(Duration::from_secs(10)))?;
                    let wire = Arc::new(Wire {
                        stream: Mutex::new(stream.try_clone()?),
                        disconnected: AtomicBool::new(false),
                    });
                    let mut reader = BufReader::new(stream);
                    // Replay the parsed frame to the handler without reading unbounded data twice.
                    // 将已解析帧交给处理器，不重复读取无界数据。
                    let request = read_initial(&mut reader, Duration::from_secs(5))?;
                    let result = manager.serve_request(reader, wire.clone(), peer, request);
                    if let Err(e) = &result {
                        let _ = wire.send(&Event::Error {
                            message: e.to_string(),
                        });
                    }
                    result
                })();
                if let Err(e) = result {
                    eprintln!(
                        "子系统 uid={0} pid={1} / subsystem uid={0} pid={1}: {e}",
                        peer.uid, peer.pid
                    );
                }
            })
        {
            eprintln!("子系统客户端工作线程 / subsystem client worker: {error}");
        }
    }
    Ok(())
}
