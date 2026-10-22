//! Authenticate the privileged server and preserve command output and exit status.
//! 验证特权服务端并保留命令输出与退出状态。
use super::{
    Error, Result,
    protocol::{self, ClientMessage, Event, Operation, Status},
    terminal,
};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    io::{self, BufReader, Write},
    net::Shutdown,
    os::{
        fd::AsRawFd,
        unix::{
            fs::{FileTypeExt, MetadataExt},
            net::UnixStream,
        },
    },
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::Duration,
};

/// Identify a socket by the canonical manager root.
/// 以规范管理根标识 socket。
/// # Arguments
/// - `root`: Existing manager directory. 已有管理目录。
pub fn socket_for_root(root: &Path) -> Result<PathBuf> {
    let path = fs::canonicalize(root)?;
    let hash = format!("{:x}", Sha256::digest(path.as_os_str().as_encoded_bytes()));
    Ok(Path::new("/run/rstarsleep").join(hash).join("manager.sock"))
}

/// Connect only to a root-owned socket and authenticated root peer.
/// 只连接 root 所属 socket 及经认证 root 对端。
/// # Arguments
/// - `socket`: Expected private service socket. 预期私有服务 socket。
pub(crate) fn connect(socket: &Path) -> Result<UnixStream> {
    super::checked(socket)?;
    let meta = fs::symlink_metadata(socket)?;
    if !meta.file_type().is_socket() || meta.uid() != 0 {
        return Err(Error::Invalid(
            "守护进程 socket 必须属于 root / daemon socket must be owned by root".into(),
        ));
    }
    for ancestor in socket.parent().unwrap().ancestors() {
        let m = fs::symlink_metadata(ancestor)?;
        if !m.is_dir() || m.uid() != 0 || m.mode() & 0o022 != 0 {
            return Err(Error::Invalid(
                "守护进程 socket 目录不可信 / untrusted daemon socket directory".into(),
            ));
        }
    }
    let stream = UnixStream::connect(socket)?;
    let mut cred: libc::ucred = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    if unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut cred as *mut libc::ucred).cast(),
            &mut len,
        )
    } != 0
        || len as usize != std::mem::size_of::<libc::ucred>()
        || cred.uid != 0
        || cred.pid <= 0
    {
        return Err(Error::Invalid(
            "守护进程身份验证失败 / daemon authentication failed".into(),
        ));
    }
    stream.set_write_timeout(Some(Duration::from_secs(10)))?;
    Ok(stream)
}

/// Perform a single management request.
/// 执行单个管理请求。
/// # Arguments
/// - `socket`: Trusted service endpoint. 可信服务端点。
/// - `operation`: Requested management operation. 请求管理操作。
pub fn request(socket: &Path, operation: Operation) -> Result<Vec<Status>> {
    let message = protocol::request(operation);
    if let ClientMessage::Request { request } = &message {
        protocol::validate_request(request)?;
    }
    let mut stream = connect(socket)?;
    stream.set_read_timeout(Some(Duration::from_secs(120)))?;
    protocol::write_frame(&mut stream, &message)?;
    let mut reader = BufReader::new(stream);
    loop {
        match protocol::read_frame::<Event>(&mut reader)? {
            Some(Event::Message { text }) => println!("{text}"),
            Some(Event::Result { statuses }) => return Ok(statuses),
            Some(Event::Error { message }) => return Err(Error::Invalid(message)),
            _ => {
                return Err(Error::Protocol(
                    "管理响应无效 / invalid management response".into(),
                ));
            }
        }
    }
}

/// Receive separate binary output streams until a numeric exit event arrives.
/// 接收分离二进制输出流，直到数值退出事件到达。
/// # Arguments
/// - `stream`: Command response connection. 命令响应连接。
fn receive_execution(stream: UnixStream, ready: &AtomicBool) -> io::Result<i32> {
    let mut reader = BufReader::new(stream);
    loop {
        match protocol::read_frame::<Event>(&mut reader)? {
            Some(Event::Ready { .. }) => {
                ready.store(true, Ordering::SeqCst);
            }
            Some(Event::Stdout { bytes }) => {
                let mut out = io::stdout().lock();
                out.write_all(&bytes)?;
                out.flush()?;
            }
            Some(Event::Stderr { bytes }) => {
                let mut out = io::stderr().lock();
                out.write_all(&bytes)?;
                out.flush()?;
            }
            Some(Event::Message { text }) => println!("{text}"),
            Some(Event::Exit { code }) if (0..=255).contains(&code) => return Ok(code),
            Some(Event::Error { message }) => return Err(io::Error::other(message)),
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "缺少应用退出状态 / missing application exit status",
                ));
            }
        }
    }
}

/// Stream one execution with stdin, resize, signals and true exit status.
/// 传输一次执行的输入、尺寸、信号及真实退出状态。
/// # Arguments
/// - `socket`: Trusted service endpoint. 可信服务端点。
/// - `operation`: Exec or application operation. 执行或应用操作。
/// - `pty`: Whether raw terminal handling is required. 是否需要原始终端处理。
pub fn execute_operation(socket: &Path, operation: Operation, pty: bool) -> Result<i32> {
    let message = protocol::request(operation);
    if let ClientMessage::Request { request } = &message {
        protocol::validate_request(request)?;
    }
    let mut stream = connect(socket)?;
    let _guard = terminal::TerminalGuard::acquire(pty)?;
    protocol::write_frame(&mut stream, &message)?;
    let response = stream.try_clone()?;
    let (tx, rx) = mpsc::channel();
    let ready = Arc::new(AtomicBool::new(false));
    let received_ready = ready.clone();
    let reader = thread::spawn(move || {
        let _ = tx.send(receive_execution(response, &received_ready));
    });
    let result = (|| -> io::Result<i32> {
        let mut input_open = true;
        let mut bytes = [0; protocol::STREAM_CHUNK_BYTES];
        loop {
            match rx.try_recv() {
                Ok(result) => return result,
                Err(mpsc::TryRecvError::Disconnected) => {
                    return Err(io::Error::other(
                        "应用接收线程结束 / application receiver thread ended",
                    ));
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
            if let Some(signal) = terminal::take_signal() {
                if !ready.load(Ordering::SeqCst) {
                    return Ok((128 + signal).min(255));
                }
                protocol::write_frame(&mut stream, &ClientMessage::Signal { signal })?;
            }
            if !ready.load(Ordering::SeqCst) {
                thread::sleep(Duration::from_millis(20));
                continue;
            }
            if pty && terminal::take_resize() {
                protocol::write_frame(
                    &mut stream,
                    &ClientMessage::Resize {
                        window: terminal::window_size(),
                    },
                )?;
            }
            if input_open && terminal::stdin_ready(50)? {
                let n = terminal::read_stdin(&mut bytes)?;
                if n == 0 {
                    input_open = false;
                    protocol::write_frame(&mut stream, &ClientMessage::Eof)?;
                } else {
                    protocol::write_frame(
                        &mut stream,
                        &ClientMessage::Input {
                            bytes: bytes[..n].to_vec(),
                        },
                    )?;
                }
            } else if !input_open {
                thread::sleep(Duration::from_millis(50));
            }
        }
    })();
    let _ = stream.shutdown(Shutdown::Both);
    let _ = reader.join();
    result.map_err(Error::from)
}

/// Execute a literal program vector through the daemon.
/// 通过守护进程执行字面程序数组。
/// # Arguments
/// - `socket`: Trusted service endpoint. 可信服务端点。
/// - `id`: Registered subsystem. 已登记子系统。
/// - `argv`: Program and literal arguments. 程序及字面参数。
/// - `environment`: Caller session variables. 调用者会话变量。
/// - `pty`: Interactive terminal mode. 交互终端模式。
pub fn execute(
    socket: &Path,
    id: &str,
    argv: Vec<String>,
    environment: BTreeMap<String, String>,
    pty: bool,
) -> Result<i32> {
    execute_operation(
        socket,
        Operation::Exec {
            id: id.into(),
            argv,
            environment,
            pty,
            window: pty.then(terminal::window_size),
        },
        pty,
    )
}
