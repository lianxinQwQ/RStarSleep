//! Durable PTY console recording independent of SSH and daemon lifetimes.
//! 独立于 SSH 和 daemon 生命周期的持久 PTY 控制台记录。
use super::{
    Error, PreparedRuntime, Result,
    execution::Wire,
    protocol::{self, ClientMessage, Event, StartOrigin},
    supervision::{Service, Status, Supervisor, systemd::Systemd},
};
use crate::config::ConfigStore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::{BufReader, Read, Seek, SeekFrom, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{
            fs::{OpenOptionsExt, PermissionsExt},
            net::UnixStream,
            process::CommandExt,
        },
    },
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Spec {
    directory: PathBuf,
    machine: String,
    program: PathBuf,
    arguments: Vec<String>,
    unit: String,
    identity: String,
    task: String,
    origin: StartOrigin,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StopRequest {
    invocation: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ShutdownMarker {
    invocation: Option<String>,
    offset: u64,
}

/// Keep shutdown offsets separate so daemon writes cannot overwrite recorder completion.
/// 单独保存关机偏移，避免 daemon 写入覆盖记录器完成状态。
fn merge_shutdown(directory: &Path, mut run: Run) -> Result<Run> {
    match super::read_record::<ShutdownMarker>(
        &directory.join(format!("{}.shutdown.toml", run.task)),
    ) {
        Ok(marker) if marker.invocation == run.invocation => {
            run.shutdown_offset = Some(marker.offset)
        }
        Ok(_) => {
            return Err(Error::Invalid(
                "关机标记启动身份不匹配 / shutdown marker invocation mismatch".into(),
            ));
        }
        Err(Error::Io { source, .. }) if source.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    Ok(run)
}

/// Persist stop intent independently of whether the recorder has published a run yet.
/// 独立于记录器是否已发布运行记录持久保存停止意图。
fn requested(directory: &Path, invocation: &str) -> Result<bool> {
    match super::read_record::<StopRequest>(&directory.join("stop.toml")) {
        Ok(request) => Ok(!invocation.is_empty() && request.invocation == invocation),
        Err(Error::Io { source, .. }) if source.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
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
    #[serde(default)]
    pub invocation: Option<String>,
    #[serde(default)]
    pub origin: Option<StartOrigin>,
}

/// Resolve persistent console storage, outside the configuration view.
/// 解析配置视图之外的持久控制台存储。
/// # Arguments
/// - `store`, `id`: Fixed manager and subsystem. 固定管理器及子系统。
pub fn directory(store: &ConfigStore, id: &str) -> Result<PathBuf> {
    Ok(store.system_layout(id)?.root.join("runtime/console"))
}

/// Derive a stable service name from the canonical manager root and subsystem.
/// 根据规范管理根和子系统推导稳定服务名。
/// # Arguments
/// - `store`, `id`: Fixed manager and subsystem. 固定管理器及子系统。
pub(crate) fn unit_name(store: &ConfigStore, id: &str) -> Result<String> {
    protocol::validate_id(id)?;
    let root = fs::canonicalize(store.root())?;
    let digest =
        Sha256::digest([root.as_os_str().as_encoded_bytes(), b"\0", id.as_bytes()].concat());
    Ok(format!("rstarsleep-console-{digest:x}.service"))
}

/// Check fixed executable and existing recorder ownership before runtime preparation.
/// 在准备运行环境前检查固定程序及已有记录器所有权。
pub(crate) fn preflight(store: &ConfigStore, id: &str) -> Result<PathBuf> {
    super::checked(&directory(store, id)?)?;
    if store.root().to_str().is_none() {
        return Err(Error::Invalid(
            "服务管理根必须为 UTF-8 / service management root must be UTF-8".into(),
        ));
    }
    let binary = fs::canonicalize(std::env::current_exe()?)?;
    super::trust::path(&binary)?;
    for tool in [
        binary.as_path(),
        Path::new(super::commands::SYSTEMCTL),
        Path::new(super::commands::SYSTEMD_RUN),
        Path::new("/usr/bin/systemd-nspawn"),
    ] {
        let path = fs::canonicalize(tool)?;
        super::trust::path(&path)?;
        let metadata = fs::metadata(&path)?;
        if path.to_str().is_none()
            || !metadata.is_file()
            || metadata.permissions().mode() & 0o111 == 0
        {
            return Err(Error::Invalid(format!(
                "服务程序无效 / invalid service executable: {}",
                path.display()
            )));
        }
    }
    Ok(binary)
}

/// Reject legacy or unexpected writers without deleting their lock file.
/// 拒绝旧版或意外写入者，不删除其锁文件。
pub(crate) fn ensure_idle(store: &ConfigStore, id: &str) -> Result<()> {
    let _guard = capture_lock(&directory(store, id)?, false)?;
    Ok(())
}

/// Acquire a defensive lock, preserving errors unrelated to contention.
/// 获取防御锁，保留与竞争无关的错误。
fn capture_lock(directory: &Path, create: bool) -> Result<Option<File>> {
    let path = directory.join("capture.lock");
    let guard = match OpenOptions::new()
        .read(true)
        .write(true)
        .create(create)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&path)
    {
        Ok(file) => file,
        Err(error) if !create && error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(Error::Io { path, source }),
    };
    if unsafe { libc::flock(guard.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        let source = std::io::Error::last_os_error();
        if source.kind() == std::io::ErrorKind::WouldBlock {
            return Err(Error::Invalid(
                "已有控制台记录器持锁；请先停止旧实例 / an existing console recorder holds the lock; stop the old instance first".into()
            ));
        }
        return Err(Error::Io { path, source });
    }
    Ok(Some(guard))
}

pub(crate) struct Recorder {
    pub unit: String,
    identity: String,
    machine: String,
}

impl Recorder {
    /// Preserve a timed-out stop so reconciliation cannot forget the request.
    /// 保留超时停止意图，避免协调过程忘记请求。
    pub fn request_shutdown(&self, store: &ConfigStore, id: &str, status: &Status) -> Result<()> {
        status.verify(&self.identity)?;
        if !status.live() {
            return Ok(());
        }
        if status.invocation.is_empty() {
            return Err(Error::Invalid(
                "停止请求缺少服务启动身份 / stop request missing service invocation identity"
                    .into(),
            ));
        }
        super::write_record(
            &directory(store, id)?.join("stop.toml"),
            &StopRequest {
                invocation: status.invocation.clone(),
            },
        )
    }

    /// A stop request belongs to exactly one invocation, including pre-registration startup.
    /// 停止请求只属于一次服务运行，包括机器注册前的启动阶段。
    pub fn shutdown_requested(
        &self,
        store: &ConfigStore,
        id: &str,
        status: &Status,
    ) -> Result<bool> {
        requested(&directory(store, id)?, &status.invocation)
    }

    /// Completion requires a stopped machine, an exited service and durable final metadata.
    /// 完成要求机器已停、服务已退出且最终元数据已持久化。
    pub fn drained(
        &self,
        store: &ConfigStore,
        id: &str,
        status: &Status,
        machine_alive: bool,
    ) -> Result<bool> {
        status.verify(&self.identity)?;
        if machine_alive || status.live() {
            return Ok(false);
        }
        if let Some(run) = metadata(store, id)? {
            if run.machine != self.machine
                || (!status.invocation.is_empty()
                    && run.invocation.as_deref() != Some(status.invocation.as_str()))
            {
                return Err(Error::Invalid(
                    "排空记录与控制台服务不匹配 / drain record does not match console service"
                        .into(),
                ));
            }
            if !run.finished {
                return Err(Error::Invalid(format!(
                    "控制台服务退出但日志未完成排空 / console service exited without completing output drain: {} ({}, {})",
                    self.unit, status.result, status.exit_status
                )));
            }
            if let Some(error) = run.error {
                return Err(Error::Invalid(format!(
                    "控制台采集失败 / console capture failed: {error}"
                )));
            }
        }
        Ok(true)
    }
    /// Bind recovery to the exact runtime, independently of the daemon process.
    /// 将恢复绑定到准确运行环境，不依赖 daemon 进程。
    pub fn new(store: &ConfigStore, id: &str, runtime: &PreparedRuntime) -> Result<Self> {
        let unit = unit_name(store, id)?;
        let root = fs::canonicalize(&runtime.root)?;
        let digest = Sha256::digest(
            [
                unit.as_bytes(),
                b"\0",
                runtime.machine.as_bytes(),
                b"\0",
                runtime.version.as_bytes(),
                b"\0",
                root.as_os_str().as_encoded_bytes(),
            ]
            .concat(),
        );
        Ok(Self {
            unit,
            identity: format!("RStarSleep console v1 {digest:x}"),
            machine: runtime.machine.clone(),
        })
    }

    /// Validate ownership for activation, execution and draining alike.
    /// 对激活、运行和排空阶段统一校验所有权。
    pub fn status(&self, supervisor: &dyn Supervisor) -> Result<Status> {
        let status = supervisor.query(&self.unit)?;
        status.verify(&self.identity)?;
        Ok(status)
    }

    /// Match durable metadata to the current service invocation, ignoring older runs.
    /// 将持久元数据匹配到当前服务启动，忽略旧次运行。
    pub fn run(&self, store: &ConfigStore, id: &str, status: &Status) -> Result<Option<Run>> {
        let run = match metadata(store, id)? {
            Some(run)
                if !status.invocation.is_empty()
                    && run.invocation.as_deref() == Some(status.invocation.as_str()) =>
            {
                run
            }
            _ => return Ok(None),
        };
        if run.machine != self.machine {
            return Err(Error::Invalid(
                "控制台机器身份不匹配 / console machine identity mismatch".into(),
            ));
        }
        if run.origin.is_none() {
            return Err(Error::Invalid(
                "控制台启动记录缺少来源 / console invocation record missing start origin".into(),
            ));
        }
        if let Some(error) = &run.error {
            return Err(Error::Invalid(format!(
                "控制台采集失败 / console capture failed: {error}"
            )));
        }
        Ok(Some(run))
    }

    /// Publish an immutable launch specification, then let the supervisor start once.
    /// 发布不可变启动说明，然后由监督器启动一次。
    pub fn launch(
        &self,
        supervisor: &dyn Supervisor,
        store: &ConfigStore,
        id: &str,
        runtime: &PreparedRuntime,
        origin: StartOrigin,
        binary: PathBuf,
    ) -> Result<()> {
        if self.status(supervisor)?.live() {
            return Ok(());
        }
        ensure_idle(store, id)?;
        let directory = directory(store, id)?;
        fs::create_dir_all(&directory)?;
        super::trust::path(&directory)?;
        let task = format!(
            "boot-{}-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|e| Error::Invalid(format!("时钟无效 / invalid clock: {e}")))?
                .as_nanos(),
            std::process::id()
        );
        let mut command = Command::new(fs::canonicalize("/usr/bin/systemd-nspawn")?);
        command
            .args(["--quiet", "--boot", "--register=yes"])
            .arg(format!("--machine={}", runtime.machine))
            .arg("--directory")
            .arg(&runtime.root)
            .args(&runtime.boot_args);
        let spec = Spec {
            directory: directory.clone(),
            machine: runtime.machine.clone(),
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
            unit: self.unit.clone(),
            identity: self.identity.clone(),
            task: task.clone(),
            origin,
        };
        let path = directory.join(format!("{task}.spec.json"));
        let bytes = serde_json::to_vec(&spec).map_err(|e| {
            Error::Protocol(format!("控制台启动协议 / console startup protocol: {e}"))
        })?;
        if bytes.len() > 128 * 1024 {
            return Err(Error::Invalid(
                "控制台启动说明过大 / console launch specification is too large".into(),
            ));
        }
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&path)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        File::open(&directory)?.sync_all()?;
        supervisor.start(&Service {
            name: self.unit.clone(),
            identity: self.identity.clone(),
            executable: binary,
            arguments: vec!["__console-recorder".into(), path.into_os_string()],
        })
    }
}

/// Read optional metadata without hiding corrupt records or access errors.
/// 读取可选元数据，不隐藏记录损坏或访问错误。
pub(crate) fn metadata(store: &ConfigStore, id: &str) -> Result<Option<Run>> {
    let directory = directory(store, id)?;
    match super::read_record(&directory.join("current.toml")) {
        Ok(run) => merge_shutdown(&directory, run).map(Some),
        Err(Error::Io { source, .. }) if source.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

/// Read an archived task with its independently committed shutdown marker.
/// 读取归档任务及其独立提交的关机标记。
pub(crate) fn task(store: &ConfigStore, id: &str, task: &str) -> Result<Run> {
    if task.contains('/') || task.contains("..") || !task.starts_with("boot-") {
        return Err(Error::Invalid(
            "控制台任务标识无效 / invalid console task identity".into(),
        ));
    }
    let directory = directory(store, id)?;
    merge_shutdown(
        &directory,
        super::read_record(&directory.join(format!("{task}.toml")))?,
    )
}

/// Own the PTY and raw append-only log in a process surviving daemon restarts.
/// 在跨 daemon 重启存活的进程中持有 PTY 和原始追加日志。
/// # Arguments
/// - `path`: Trusted immutable launch specification. 可信不可变启动说明。
pub fn recorder_entry(path: &Path) -> Result<()> {
    super::trust::path(path)?;
    let spec: Spec = serde_json::from_reader(File::open(path)?.take(128 * 1024))
        .map_err(|e| Error::Protocol(format!("控制台启动协议 / console startup protocol: {e}")))?;
    super::trust::path(&spec.directory)?;
    super::trust::path(&spec.program)?;
    let invocation = std::env::var("INVOCATION_ID").map_err(|_| {
        Error::Invalid(
            "记录器必须由服务监督器启动 / recorder must be started by its service supervisor"
                .into(),
        )
    })?;
    let service = Systemd.query(&spec.unit)?;
    service.verify(&spec.identity)?;
    if !service.live() || service.invocation != invocation || invocation.is_empty() {
        return Err(Error::Invalid(
            "控制台服务启动身份不匹配 / console service invocation identity mismatch".into(),
        ));
    }
    let _guard = capture_lock(&spec.directory, true)?;
    let log_path = spec.directory.join("console.log");
    let mut log = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&log_path)?;
    let mut run = Run {
        task: spec.task,
        machine: spec.machine,
        offset: log.metadata()?.len(),
        shutdown_offset: None,
        finished: false,
        exit_code: None,
        error: None,
        invocation: Some(invocation.clone()),
        origin: Some(spec.origin),
    };
    super::write_record(&spec.directory.join("current.toml"), &run)?;
    let cancelled = requested(&spec.directory, &invocation)?;
    if cancelled {
        run.shutdown_offset = Some(run.offset);
    }
    let result = (|| -> Result<i32> {
        if cancelled {
            return Ok(0);
        }
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
            if unsafe { libc::fcntl(descriptor, libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
                return Err(std::io::Error::last_os_error().into());
            }
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
    if let Ok(current) = super::read_record::<Run>(&spec.directory.join("current.toml"))
        && current.task == run.task
        && current.invocation == run.invocation
    {
        run.shutdown_offset = current.shutdown_offset.or(run.shutdown_offset);
    }
    run = merge_shutdown(&spec.directory, run)?;
    super::write_record(&spec.directory.join("current.toml"), &run)?;
    super::write_record(&spec.directory.join(format!("{}.toml", run.task)), &run)?;
    result.and_then(|code| {
        if code == 0 {
            Ok(())
        } else {
            Err(Error::Exit(code))
        }
    })
}

/// Record the precise beginning of a graceful shutdown request.
/// 记录优雅关机请求的准确起点。
/// # Arguments
/// - `store`, `id`: Fixed subsystem. 固定子系统。
pub fn mark_shutdown(store: &ConfigStore, id: &str) -> Result<()> {
    let directory = directory(store, id)?;
    let run: Run = super::read_record(&directory.join("current.toml"))?;
    let marker = directory.join(format!("{}.shutdown.toml", run.task));
    if marker.try_exists()? {
        return Ok(());
    }
    super::write_record(
        &marker,
        &ShutdownMarker {
            invocation: run.invocation,
            offset: fs::metadata(directory.join("console.log"))?.len(),
        },
    )
}

/// Read console task metadata and distinguish a lost recorder from a completed run.
/// 读取控制台任务元数据，区分记录器丢失与运行完成。
/// # Arguments
/// - `store`, `id`: Fixed subsystem. 固定子系统。
pub fn current(store: &ConfigStore, id: &str) -> Result<Run> {
    let directory = directory(store, id)?;
    let run = metadata(store, id)?
        .ok_or_else(|| Error::Invalid("控制台运行记录缺失 / console run record missing".into()))?;
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
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::WouldBlock {
            return Err(error.into());
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

#[cfg(test)]
mod tests;
