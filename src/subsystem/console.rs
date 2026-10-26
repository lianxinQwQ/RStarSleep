//! Durable PTY console recording independent of SSH and daemon lifetimes.
//! 独立于 SSH 和 daemon 生命周期的持久 PTY 控制台记录。
use super::{
    Error, Result,
    execution::Wire,
    protocol::{self, ClientMessage, Event},
};
use crate::config::ConfigStore;
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::{BufReader, Read, Seek, SeekFrom, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{fs::OpenOptionsExt, net::UnixStream, process::CommandExt},
    },
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

#[derive(Serialize, Deserialize)]
struct Spec {
    directory: PathBuf,
    machine: String,
    program: PathBuf,
    arguments: Vec<String>,
}
#[derive(Debug, Serialize, Deserialize)]
pub struct Run {
    pub task: String,
    pub machine: String,
    pub offset: u64,
    pub shutdown_offset: Option<u64>,
    pub finished: bool,
    pub exit_code: Option<i32>,
    pub error: Option<String>,
}

/// Resolve persistent console storage, outside the configuration view.
/// 解析配置视图之外的持久控制台存储。
/// # Arguments
/// - `store`, `id`: Fixed manager and subsystem. 固定管理器及子系统。
pub fn directory(store: &ConfigStore, id: &str) -> Result<PathBuf> {
    Ok(store.system_layout(id)?.root.join("runtime/console"))
}

/// Start a detached recorder; readiness is still checked by the lifecycle manager.
/// 启动独立记录器；生命周期管理器仍负责检查就绪。
/// # Arguments
/// - `store`, `id`, `machine`, `command`: Fixed identity and literal nspawn invocation. 固定身份及字面 nspawn 调用。
pub fn spawn(store: &ConfigStore, id: &str, machine: &str, command: &Command) -> Result<Child> {
    let directory = directory(store, id)?;
    super::checked(&directory)?;
    fs::create_dir_all(&directory)?;
    super::trust::path(&directory)?;
    let binary = fs::canonicalize(std::env::current_exe()?)?;
    super::trust::path(&binary)?;
    let spec = Spec {
        directory,
        machine: machine.into(),
        program: command.get_program().into(),
        arguments: command
            .get_args()
            .map(|a| {
                a.to_str().map(str::to_owned).ok_or_else(|| {
                    Error::Invalid(
                        "控制台参数必须为 UTF-8 / console arguments must be UTF-8".into(),
                    )
                })
            })
            .collect::<Result<_>>()?,
    };
    let mut recorder = Command::new(binary);
    recorder
        .arg("__console-recorder")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit());
    unsafe {
        recorder.pre_exec(|| {
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = recorder.spawn()?;
    if let Err(error) = (|| -> Result<()> {
        let mut input = child.stdin.take().unwrap();
        serde_json::to_writer(&mut input, &spec).map_err(|e| {
            Error::Protocol(format!("控制台启动协议 / console startup protocol: {e}"))
        })?;
        input.flush()?;
        Ok(())
    })() {
        let _ = child.kill();
        let _ = child.wait();
        return Err(error);
    }
    Ok(child)
}

/// Own the PTY and raw append-only log in a process surviving daemon restarts.
/// 在跨 daemon 重启存活的进程中持有 PTY 和原始追加日志。
/// # Arguments
/// No parameters; the fixed spawn specification arrives on stdin. 无参数；固定启动说明由标准输入传入。
pub fn recorder_entry() -> Result<()> {
    let spec: Spec = serde_json::from_reader(std::io::stdin().take(128 * 1024))
        .map_err(|e| Error::Protocol(format!("控制台启动协议 / console startup protocol: {e}")))?;
    super::trust::path(&spec.directory)?;
    let guard = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(spec.directory.join("capture.lock"))?;
    if unsafe { libc::flock(guard.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err(Error::Invalid(
            "控制台记录器已运行 / console recorder already running".into(),
        ));
    }
    let log_path = spec.directory.join("console.log");
    let mut log = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&log_path)?;
    let mut run = Run {
        task: format!(
            "boot-{}-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|e| Error::Invalid(format!("时钟无效 / invalid clock: {e}")))?
                .as_nanos(),
            std::process::id()
        ),
        machine: spec.machine,
        offset: log.metadata()?.len(),
        shutdown_offset: None,
        finished: false,
        exit_code: None,
        error: None,
    };
    super::write_record(&spec.directory.join("current.toml"), &run)?;
    let result = (|| -> Result<i32> {
        let (mut master, mut slave) = (-1, -1);
        let size = libc::winsize {
            ws_row: 40,
            ws_col: 120,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        if unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null(),
                &size,
            )
        } != 0
        {
            return Err(std::io::Error::last_os_error().into());
        }
        let mut master = unsafe { File::from_raw_fd(master) };
        let slave = unsafe { File::from_raw_fd(slave) };
        for descriptor in [master.as_raw_fd(), slave.as_raw_fd()] {
            if unsafe { libc::fcntl(descriptor, libc::F_SETFD, libc::FD_CLOEXEC) } < 0 { return Err(std::io::Error::last_os_error().into()); }
        }
        let mut command = Command::new(spec.program);
        command
            .args(spec.arguments)
            .arg("--console=interactive")
            .env("TERM", "xterm-256color")
            .stdin(Stdio::from(slave.try_clone()?))
            .stdout(Stdio::from(slave.try_clone()?))
            .stderr(Stdio::from(slave.try_clone()?));
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() < 0 || libc::ioctl(0, libc::TIOCSCTTY, 0) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut child = command.spawn()?;
        drop(slave);
        let mut bytes = [0; 8192];
        let capture = loop {
            match master.read(&mut bytes) {
                Ok(0) => break Ok(()),
                Ok(n) => {
                    if let Err(e) = log.write_all(&bytes[..n]) {
                        break Err(e);
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) if e.raw_os_error() == Some(libc::EIO) => break Ok(()),
                Err(e) => break Err(e),
            }
        };
        if let Err(error) = capture {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error.into());
        }
        log.sync_all()?;
        Ok(child.wait()?.code().unwrap_or(1))
    })();
    run.finished = true;
    match &result {
        Ok(code) => run.exit_code = Some(*code),
        Err(error) => run.error = Some(error.to_string()),
    }
    // Preserve shutdown markers written by the daemon while this process captured bytes.
    // 保留本进程采集字节期间由 daemon 写入的关机标记。
    if let Ok(current) = super::read_record::<Run>(&spec.directory.join("current.toml")) {
        run.shutdown_offset = current.shutdown_offset;
    }
    super::write_record(&spec.directory.join("current.toml"), &run)?;
    super::write_record(&spec.directory.join(format!("{}.toml", run.task)), &run)?;
    result.map(|_| ())
}

/// Record the precise beginning of a graceful shutdown request.
/// 记录优雅关机请求的准确起点。
/// # Arguments
/// - `store`, `id`: Fixed subsystem. 固定子系统。
pub fn mark_shutdown(store: &ConfigStore, id: &str) -> Result<()> {
    let directory = directory(store, id)?;
    let mut run: Run = super::read_record(&directory.join("current.toml"))?;
    run.shutdown_offset = Some(fs::metadata(directory.join("console.log"))?.len());
    super::write_record(&directory.join("current.toml"), &run)
}

/// Read console task metadata and distinguish a lost recorder from a completed run.
/// 读取控制台任务元数据，区分记录器丢失与运行完成。
/// # Arguments
/// - `store`, `id`: Fixed subsystem. 固定子系统。
pub fn current(store: &ConfigStore, id: &str) -> Result<Run> {
    let directory = directory(store, id)?;
    let run: Run = super::read_record(&directory.join("current.toml"))?;
    if let Some(error) = &run.error {
        return Err(Error::Invalid(format!(
            "控制台采集失败 / console capture failed: {error}"
        )));
    }
    if !run.finished {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(directory.join("capture.lock"))?;
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            return Err(Error::Invalid(
                "控制台记录器已丢失 / console recorder was lost".into(),
            ));
        }
    }
    Ok(run)
}

/// Read exact raw console bytes from an independent byte offset.
/// 从独立字节偏移读取准确的原始控制台字节。
/// # Arguments
/// - `store`, `id`, `offset`: Fixed subsystem and validated byte cursor. 固定子系统及有效字节游标。
pub fn read(store: &ConfigStore, id: &str, offset: &mut u64) -> Result<Vec<u8>> {
    let mut file = File::open(directory(store, id)?.join("console.log"))?;
    if *offset > file.metadata()?.len() {
        return Err(Error::Invalid(
            "控制台偏移超出文件长度 / console offset exceeds file length".into(),
        ));
    }
    file.seek(SeekFrom::Start(*offset))?;
    let mut bytes = vec![0; 8192];
    let n = file.read(&mut bytes)?;
    bytes.truncate(n);
    *offset += n as u64;
    Ok(bytes)
}

/// Stream raw console bytes; disconnects and Ctrl+C only cancel this subscription.
/// 传输原始控制台字节；断连和 Ctrl+C 仅取消此订阅。
/// # Arguments
/// - `store`, `id`, `offset`, `follow`, `reader`, `wire`: Fixed source, cursor and subscription transport. 固定来源、游标及订阅传输。
pub(crate) fn serve(
    store: &ConfigStore,
    id: &str,
    mut offset: u64,
    follow: bool,
    mut reader: BufReader<UnixStream>,
    wire: Arc<Wire>,
) -> Result<()> {
    current(store, id)?;
    let cancelled = Arc::new(AtomicBool::new(false));
    let done = cancelled.clone();
    reader
        .get_ref()
        .set_read_timeout(Some(Duration::from_millis(200)))?;
    let input = thread::spawn(move || {
        while !done.load(Ordering::Relaxed) {
            match protocol::read_frame::<ClientMessage>(&mut reader) {
                Ok(Some(ClientMessage::Resize { .. })) => {}
                Ok(Some(_)) | Ok(None) => break,
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                    ) => {}
                Err(_) => break,
            }
        }
        done.store(true, Ordering::Relaxed);
    });
    let result = (|| -> Result<()> {
        loop {
            let bytes = read(store, id, &mut offset)?;
            if !bytes.is_empty() {
                wire.send(&Event::Stdout { bytes })?;
                continue;
            }
            if !follow || cancelled.load(Ordering::Relaxed) || current(store, id)?.finished {
                break;
            }
            thread::sleep(Duration::from_millis(50));
        }
        wire.send(&Event::Exit { code: 0 })?;
        Ok(())
    })();
    cancelled.store(true, Ordering::Relaxed);
    let _ = input.join();
    result
}
