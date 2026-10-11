//! Literal guest execution with terminal controls and cgroup lifetime.
//! 字面容器执行、终端控制及 cgroup 生命周期。
use super::{
    Error, PreparedRuntime, Result, commands,
    identity::Peer,
    protocol::{self, ClientMessage, Event, WindowSize},
    session,
};
use std::{
    collections::BTreeMap,
    fs::File,
    io::{self, BufReader, Read, Write},
    net::Shutdown,
    os::{
        fd::{AsRawFd, FromRawFd, IntoRawFd},
        unix::{
            net::UnixStream,
            process::{CommandExt, ExitStatusExt},
        },
    },
    process::{Child, Command, ExitStatus, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

static SEQUENCE: AtomicU64 = AtomicU64::new(1);
pub(crate) struct Wire {
    pub stream: Mutex<UnixStream>,
    pub disconnected: AtomicBool,
}
impl Wire {
    /// Serialize bounded output events and remember transport failure.
    /// 串行传输有界输出事件，记录传输失败。
    /// # Arguments
    /// - `event`: Complete event. 完整事件。
    pub(crate) fn send(&self, event: &Event) -> io::Result<()> {
        let mut stream = self
            .stream
            .lock()
            .map_err(|_| io::Error::other("socket writer poisoned"))?;
        let result = protocol::write_frame(&mut *stream, event);
        if result.is_err() {
            self.disconnected.store(true, Ordering::SeqCst);
        }
        result
    }
}

/// Signal only the authenticated application's guest cgroup.
/// 只向已认证应用的容器 cgroup 发送信号。
/// # Arguments
/// - `machine`: Registered machine. 已注册机器。
/// - `unit`: Server-generated service. 服务端生成服务。
/// - `signal`: Allowed numeric signal. 允许的数字信号。
fn signal(machine: &str, unit: &str, signal: i32) -> Result<()> {
    if !matches!(
        signal,
        libc::SIGINT
            | libc::SIGTERM
            | libc::SIGHUP
            | libc::SIGQUIT
            | libc::SIGKILL
            | libc::SIGCONT
            | libc::SIGTSTP
    ) {
        return Err(Error::Invalid("unsupported signal".into()));
    }
    let mut command = Command::new(commands::SYSTEMCTL);
    command
        .arg(format!("--machine={machine}"))
        .args(["kill", "--kill-whom=all"])
        .arg(format!("--signal={signal}"))
        .arg(unit);
    if !commands::output(command, Duration::from_secs(5))?
        .status
        .success()
    {
        return Err(Error::Invalid("cannot signal guest application".into()));
    }
    Ok(())
}
/// Stop an application's complete cgroup on disconnect or failure.
/// 断连或失败时停止应用完整 cgroup。
/// # Arguments
/// - `machine`: Registered machine. 已注册机器。
/// - `unit`: Application service. 应用服务。
fn cancel(machine: &str, unit: &str) {
    let mut command = Command::new(commands::SYSTEMCTL);
    command
        .arg(format!("--machine={machine}"))
        .args(["stop", unit]);
    let _ = commands::output(command, Duration::from_secs(5));
    let _ = signal(machine, unit, libc::SIGKILL);
}

/// Apply nonblocking I/O to an owned descriptor.
/// 为所属描述符设置非阻塞 I/O。
/// # Arguments
/// - `file`: Pipe or PTY. 管道或 PTY。
fn nonblocking(file: &File) -> io::Result<()> {
    let fd = file.as_raw_fd();
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}
/// Poll briefly so shutdown cannot strand a stream worker.
/// 短暂等待，避免关闭时遗留流线程。
/// # Arguments
/// - `fd`: Stream descriptor. 流描述符。
/// - `events`: Poll flags. 等待标志。
fn poll(fd: i32, events: i16) -> io::Result<()> {
    let mut p = libc::pollfd {
        fd,
        events,
        revents: 0,
    };
    if unsafe { libc::poll(&mut p, 1, 100) } < 0
        && io::Error::last_os_error().kind() != io::ErrorKind::Interrupted
    {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}
/// Allocate a close-on-exec host PTY used by systemd-run.
/// 分配供 systemd-run 使用的执行时关闭宿主 PTY。
/// # Arguments
/// - `window`: Initial dimensions. 初始尺寸。
fn pty(window: WindowSize) -> Result<(File, File)> {
    let (mut a, mut b) = (-1, -1);
    let size = libc::winsize {
        ws_row: window.rows,
        ws_col: window.cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    if unsafe {
        libc::openpty(
            &mut a,
            &mut b,
            std::ptr::null_mut(),
            std::ptr::null(),
            &size,
        )
    } != 0
    {
        return Err(io::Error::last_os_error().into());
    }
    let a = unsafe { File::from_raw_fd(a) };
    let b = unsafe { File::from_raw_fd(b) };
    for f in [&a, &b] {
        if unsafe { libc::fcntl(f.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
            return Err(io::Error::last_os_error().into());
        }
    }
    Ok((a, b))
}
/// Convert a child pipe without duplicating or leaking its descriptor.
/// 转换子进程管道，不复制或泄露描述符。
/// # Arguments
/// - `value`: Owned pipe. 所属管道。
fn file(value: impl IntoRawFd) -> File {
    unsafe { File::from_raw_fd(value.into_raw_fd()) }
}
/// Drain one pipe into binary protocol events.
/// 将一个管道排空到二进制协议事件。
/// # Arguments
/// - `pipe`: Child output. 子进程输出。
/// - `stderr`: Select stderr. 选择 stderr。
/// - `wire`: Serialized transport. 串行传输。
/// - `finished`: Completion flag. 完成标志。
fn pump(
    mut pipe: File,
    stderr: bool,
    wire: Arc<Wire>,
    finished: Arc<AtomicBool>,
) -> io::Result<()> {
    let mut bytes = [0; protocol::STREAM_CHUNK_BYTES];
    loop {
        match pipe.read(&mut bytes) {
            Ok(0) => return Ok(()),
            Ok(n) => {
                let bytes = bytes[..n].to_vec();
                wire.send(&if stderr {
                    Event::Stderr { bytes }
                } else {
                    Event::Stdout { bytes }
                })?;
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                if finished.load(Ordering::SeqCst) {
                    return Ok(());
                }
                poll(pipe.as_raw_fd(), libc::POLLIN)?;
            }
            Err(e) if e.raw_os_error() == Some(libc::EIO) => return Ok(()),
            Err(e) => return Err(e),
        }
    }
}
/// Forward bounded controls until completion or disconnect.
/// 转发有界控制，直到完成或断连。
/// # Arguments
/// - `reader`: Client control channel. 客户端控制通道。
/// - `input`: Child stdin. 子进程 stdin。
/// - `resize`: Optional PTY descriptor. 可选 PTY 描述符。
/// - `finished`: Completion flag. 完成标志。
/// - `runtime`: Target runtime. 目标运行态。
/// - `unit`: Application service. 应用服务。
fn controls(
    mut reader: BufReader<UnixStream>,
    mut input: Option<File>,
    resize: Option<File>,
    finished: Arc<AtomicBool>,
    machine: String,
    unit: String,
) -> Result<()> {
    loop {
        if finished.load(Ordering::SeqCst) {
            return Ok(());
        }
        match protocol::read_frame::<ClientMessage>(&mut reader)? {
            Some(ClientMessage::Input { bytes }) => {
                if bytes.len() > protocol::STREAM_CHUNK_BYTES {
                    return Err(Error::Invalid("oversized stdin frame".into()));
                }
                let pipe = input
                    .as_mut()
                    .ok_or_else(|| Error::Invalid("stdin after EOF".into()))?;
                let mut remaining = bytes.as_slice();
                while !remaining.is_empty() && !finished.load(Ordering::SeqCst) {
                    match pipe.write(remaining) {
                        Ok(0) => return Err(io::Error::from(io::ErrorKind::WriteZero).into()),
                        Ok(n) => remaining = &remaining[n..],
                        Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                        Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                            poll(pipe.as_raw_fd(), libc::POLLOUT)?
                        }
                        // Programs may close stdin before their output/exit is
                        // ready. Preserve that exit instead of cancelling them.
                        // 程序可能在输出或退出就绪前关闭 stdin，应保留其退出而非取消。
                        Err(e) if e.kind() == io::ErrorKind::BrokenPipe => break,
                        Err(e) => return Err(e.into()),
                    }
                }
            }
            Some(ClientMessage::Eof) => {
                if resize.is_some()
                    && let Some(pipe) = input.as_mut()
                {
                    let _ = pipe.write_all(&[4]);
                }
                input.take();
            }
            Some(ClientMessage::Signal { signal: value }) => signal(&machine, &unit, value)?,
            Some(ClientMessage::Resize { window }) => {
                protocol::validate_window(window)?;
                let fd = resize
                    .as_ref()
                    .ok_or_else(|| Error::Invalid("resize requires PTY".into()))?
                    .as_raw_fd();
                let size = libc::winsize {
                    ws_row: window.rows,
                    ws_col: window.cols,
                    ws_xpixel: 0,
                    ws_ypixel: 0,
                };
                if unsafe { libc::ioctl(fd, libc::TIOCSWINSZ, &size) } != 0 {
                    return Err(io::Error::last_os_error().into());
                }
            }
            Some(ClientMessage::Request { .. }) => {
                return Err(Error::Invalid("duplicate request".into()));
            }
            None => return Ok(()),
        }
    }
}

struct ShutdownWorkers {
    finished: Arc<AtomicBool>,
    wake: UnixStream,
}
impl Drop for ShutdownWorkers {
    /// Wake every stream worker on errors, normal completion or unwinding.
    /// 在错误、正常完成或展开时唤醒所有流工作线程。
    /// # Arguments
    /// - `self`: Shared completion and socket read half. 共享完成状态和 socket 读半部。
    fn drop(&mut self) {
        self.finished.store(true, Ordering::SeqCst);
        let _ = self.wake.shutdown(Shutdown::Read);
    }
}

/// Convert normal and signalled child statuses to shell-compatible values.
/// 将正常和信号退出状态转换为 shell 兼容数值。
/// # Arguments
/// - `status`: Collected child status. 收集的子进程状态。
fn exit_code(status: ExitStatus) -> i32 {
    status
        .code()
        .unwrap_or_else(|| 128 + status.signal().unwrap_or(1))
        .clamp(0, 255)
}

/// Join every worker even if child polling or another worker fails.
/// 即使子进程探测或另一工作线程失败也回收所有线程。
/// # Arguments
/// - `child`, `reader`, `wire`: Owned process and authenticated streams. 所属进程及认证流。
/// - `machine`, `unit`, `master`: Guest cancellation identity and optional PTY. 容器取消身份及可选 PTY。
fn stream_child(
    child: &mut Child,
    reader: BufReader<UnixStream>,
    wire: Arc<Wire>,
    machine: &str,
    unit: &str,
    master: Option<File>,
) -> Result<Option<i32>> {
    let (input, stdout, stderr, resize) = if let Some(master) = master {
        (master.try_clone()?, master.try_clone()?, None, Some(master))
    } else {
        (
            file(
                child
                    .stdin
                    .take()
                    .ok_or_else(|| Error::Invalid("missing child stdin".into()))?,
            ),
            file(
                child
                    .stdout
                    .take()
                    .ok_or_else(|| Error::Invalid("missing child stdout".into()))?,
            ),
            Some(file(child.stderr.take().ok_or_else(|| {
                Error::Invalid("missing child stderr".into())
            })?)),
            None,
        )
    };
    nonblocking(&input)?;
    nonblocking(&stdout)?;
    if let Some(stderr) = &stderr {
        nonblocking(stderr)?;
    }
    wire.send(&Event::Ready {
        machine: machine.into(),
    })?;
    thread::scope(|scope| -> Result<Option<i32>> {
        let finished = Arc::new(AtomicBool::new(false));
        let shutdown = ShutdownWorkers {
            finished: finished.clone(),
            wake: reader.get_ref().try_clone()?,
        };
        let out = {
            let w = wire.clone();
            let f = finished.clone();
            scope.spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    pump(stdout, false, w.clone(), f)
                }))
                .unwrap_or_else(|_| Err(io::Error::other("output worker panicked")));
                if result.is_err() {
                    w.disconnected.store(true, Ordering::SeqCst);
                }
                result
            })
        };
        let err = stderr.map(|p| {
            let w = wire.clone();
            let f = finished.clone();
            scope.spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    pump(p, true, w.clone(), f)
                }))
                .unwrap_or_else(|_| Err(io::Error::other("stderr worker panicked")));
                if result.is_err() {
                    w.disconnected.store(true, Ordering::SeqCst);
                }
                result
            })
        });
        let control = {
            let w = wire.clone();
            let f = finished.clone();
            let m = machine.to_owned();
            let u = unit.to_owned();
            scope.spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    controls(reader, Some(input), resize, f.clone(), m, u)
                }))
                .unwrap_or_else(|_| Err(Error::Invalid("control worker panicked".into())));
                if !f.load(Ordering::SeqCst) {
                    w.disconnected.store(true, Ordering::SeqCst);
                }
                result
            })
        };
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break Ok(status),
                Err(error) => break Err(error),
                Ok(None) => {}
            }
            if wire.disconnected.load(Ordering::SeqCst) {
                cancel(machine, unit);
                let _ = child.kill();
                break child.wait();
            }
            thread::sleep(Duration::from_millis(20));
        };
        drop(shutdown);
        // Join first, propagate errors afterwards; an early ? must never leave
        // a sibling waiting on client input or a live pipe.
        // 先回收再传播错误，避免提前 ? 使其他线程仍等待客户端输入或活跃管道。
        let control = control
            .join()
            .map_err(|_| Error::Invalid("control worker panicked".into()));
        let out = out
            .join()
            .map_err(|_| Error::Invalid("output worker panicked".into()));
        let err = err.map(|worker| {
            worker
                .join()
                .map_err(|_| Error::Invalid("stderr worker panicked".into()))
        });
        let status = status?;
        if wire.disconnected.load(Ordering::SeqCst) {
            return Ok(None);
        }
        control??;
        out??;
        if let Some(err) = err {
            err??;
        }
        Ok(Some(exit_code(status)))
    })
}

/// Confirm detached unit readiness before acknowledging a successful launch.
/// 在确认分离服务就绪后才确认启动成功。
/// # Arguments
/// - `child`, `wire`, `machine`, `unit`: Awaited service and launch response. 等待的服务及启动响应。
fn detached_child(child: &mut Child, wire: &Wire, machine: &str, unit: &str) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Some(status) = child.try_wait()? {
            wire.send(&Event::Exit {
                code: exit_code(status),
            })?;
            return Ok(());
        }
        let mut command = Command::new(commands::SYSTEMCTL);
        command
            .arg(format!("--machine={machine}"))
            .args(["is-active", "--quiet", unit]);
        if commands::output(command, Duration::from_secs(5))?
            .status
            .success()
        {
            break;
        }
        if Instant::now() >= deadline {
            return Err(Error::Invalid(
                "detached application did not become ready".into(),
            ));
        }
        thread::sleep(Duration::from_millis(50));
    }
    wire.send(&Event::Exit { code: 0 })?;
    // Keep the caller's lease until ExitType=cgroup reports all descendants gone.
    // 保留调用者租约，直到 ExitType=cgroup 报告所有后代已结束。
    let status = child.wait()?;
    if !status.success() {
        eprintln!("detached guest application {unit}: {status}");
    }
    Ok(())
}

/// Literal application arguments, caller session and lifecycle options.
/// 字面应用参数、调用者会话及生命周期选项。
pub(crate) struct ExecutionRequest {
    pub argv: Vec<String>,
    pub environment: BTreeMap<String, String>,
    pub terminal: bool,
    pub window: Option<WindowSize>,
    pub detached: bool,
}

/// Stream an application while its service tracks every descendant process.
/// 在服务跟踪所有后代进程时传输应用流。
/// # Arguments
/// - `reader`, `wire`: Client transport. 客户端传输。
/// - `runtime`, `peer`: Authorized target and identity. 已授权目标与身份。
/// - `request`: Literal arguments, session inputs and terminal/detach options. 字面参数、会话输入及终端/分离选项。
pub(crate) fn execute(
    reader: BufReader<UnixStream>,
    wire: Arc<Wire>,
    runtime: PreparedRuntime,
    peer: Peer,
    request: ExecutionRequest,
) -> Result<()> {
    let ExecutionRequest {
        argv,
        environment,
        terminal,
        window,
        detached,
    } = request;
    let (name, home) = super::identity::ensure(&runtime, peer)?;
    session::refresh(&runtime, peer.uid, &environment)?;
    let unit = format!(
        "rstarsleep-app-{}-{}-{}.service",
        peer.uid,
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    );
    let mut command = Command::new(commands::SYSTEMD_RUN);
    command
        .arg(format!("--machine={}", runtime.machine))
        .arg(format!("--unit={unit}"))
        .arg(format!("--uid={}", peer.uid))
        .arg(format!("--gid={}", peer.gid))
        .args([
            "--wait",
            "--quiet",
            "--collect",
            "--service-type=exec",
            "--expand-environment=no",
            "--property=ExitType=cgroup",
            "--property=KillMode=control-group",
            "--property=TimeoutStopSec=5s",
        ])
        .arg(if terminal { "--pty" } else { "--pipe" })
        .arg(format!("--working-directory={home}"))
        .arg(format!("--setenv=HOME={home}"))
        .arg(format!("--setenv=USER={name}"))
        .arg(format!("--setenv=LOGNAME={name}"))
        .args(session::command_arguments(
            &runtime,
            peer.uid,
            &environment,
            &argv,
        )?)
        .arg("--")
        .args(argv);
    let mut master = None;
    let mut slave = None;
    if detached {
        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
    } else if terminal {
        let (a, b) = pty(window.unwrap_or(WindowSize { rows: 24, cols: 80 }))?;
        command
            .stdin(Stdio::from(b.try_clone()?))
            .stdout(Stdio::from(b.try_clone()?))
            .stderr(Stdio::from(b.try_clone()?));
        master = Some(a);
        slave = Some(b);
    } else {
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
    }
    unsafe {
        command.pre_exec(move || {
            if libc::setsid() < 0 {
                return Err(io::Error::last_os_error());
            }
            if terminal && !detached && libc::ioctl(0, libc::TIOCSCTTY, 0) < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command.spawn()?;
    drop(slave);
    let result = (|| -> Result<()> {
        if detached {
            return detached_child(&mut child, &wire, &runtime.machine, &unit);
        }
        if let Some(code) = stream_child(
            &mut child,
            reader,
            wire.clone(),
            &runtime.machine,
            &unit,
            master,
        )? {
            wire.send(&Event::Exit { code })?;
        }
        Ok(())
    })();
    if result.is_err() {
        cancel(&runtime.machine, &unit);
        let _ = child.kill();
        let _ = child.wait();
    }
    result
}

#[derive(serde::Deserialize)]
struct OccupancyUnit {
    unit: String,
    active: String,
}

/// Query machine or user units without parsing localized presentation columns.
/// 查询机器或用户服务，不解析本地化展示列。
/// # Arguments
/// - `runtime`, `uid`: Registered machine and optional user manager. 已注册机器及可选用户管理器。
/// - `patterns`: Application and session unit patterns. 应用及会话服务匹配模式。
fn occupancy_units(
    runtime: &PreparedRuntime,
    uid: Option<u32>,
    patterns: &[&str],
) -> Result<Vec<OccupancyUnit>> {
    let mut command = Command::new(commands::SYSTEMCTL);
    command.arg(format!(
        "--machine={}{}",
        uid.map_or_else(String::new, |uid| format!("{uid}@")),
        runtime.machine
    ));
    if uid.is_some() {
        command.arg("--user");
    }
    command
        .args([
            "list-units",
            "--state=active,activating,deactivating",
            "--output=json",
            "--no-pager",
        ])
        .args(patterns);
    let output = commands::output(command, Duration::from_secs(5))?;
    if !output.status.success() {
        return Err(Error::Invalid(
            "cannot recover guest application occupancy".into(),
        ));
    }
    serde_json::from_slice(&output.stdout)
        .map_err(|e| Error::Invalid(format!("invalid guest application occupancy: {e}")))
}

/// Retain applications activated outside the launcher's original service cgroup.
/// 保留在启动器原服务 cgroup 外激活的应用。
/// # Arguments
/// - `units`: Active machine units. 活跃机器服务。
/// - `bus_tasks`: Probe private-bus cgroup tasks. 探测私有总线 cgroup 任务。
/// - `user_apps`: Probe an active user's application units. 探测活跃用户的应用服务。
fn occupied_units(
    units: &[OccupancyUnit],
    mut bus_tasks: impl FnMut(&str) -> Result<u64>,
    mut user_apps: impl FnMut(u32) -> Result<bool>,
) -> Result<bool> {
    for unit in units {
        if (unit.unit.starts_with("rstarsleep-app-") && unit.unit.ends_with(".service"))
            || (unit.unit.starts_with("app-") && unit.unit.ends_with(".scope"))
        {
            return Ok(true);
        }
        // A private dbus-daemon alone has one task. Its activated descendants
        // remain in this cgroup even after the original launcher has exited.
        // 单独的私有 dbus-daemon 有一个任务；启动器退出后，激活的后代仍在此 cgroup。
        if unit.unit.starts_with("rstarsleep-dbus-") && unit.unit.ends_with(".service") {
            if unit.active != "active" || bus_tasks(&unit.unit)? > 1 {
                return Ok(true);
            }
            continue;
        }
        if let Some(uid) = unit
            .unit
            .strip_prefix("user@")
            .and_then(|unit| unit.strip_suffix(".service"))
        {
            let uid = uid
                .parse::<u32>()
                .map_err(|_| Error::Invalid("invalid guest user manager identity".into()))?;
            if unit.active != "active" || user_apps(uid)? {
                return Ok(true);
            }
            continue;
        }
        return Err(Error::Invalid("unexpected guest occupancy unit".into()));
    }
    Ok(false)
}

/// Check launch services, private-bus descendants and user application scopes.
/// 检查启动服务、私有总线后代及用户应用 scope。
/// # Arguments
/// - `runtime`: Registered machine. 已注册机器。
pub(crate) fn occupied(runtime: &PreparedRuntime) -> Result<bool> {
    let units = occupancy_units(
        runtime,
        None,
        &[
            "rstarsleep-app-*.service",
            "rstarsleep-dbus-*.service",
            "app-*.scope",
            "user@*.service",
        ],
    )?;
    occupied_units(
        &units,
        |unit| {
            let mut command = Command::new(commands::SYSTEMCTL);
            command.arg(format!("--machine={}", runtime.machine)).args([
                "show",
                "--property=TasksCurrent",
                "--value",
                unit,
            ]);
            let output = commands::output(command, Duration::from_secs(5))?;
            if !output.status.success() {
                return Err(Error::Invalid("cannot inspect guest session tasks".into()));
            }
            String::from_utf8_lossy(&output.stdout)
                .trim()
                .parse()
                .map_err(|_| Error::Invalid("invalid guest session task count".into()))
        },
        |uid| {
            occupancy_units(runtime, Some(uid), &["app-*.scope", "app-*.service"])
                .map(|units| !units.is_empty())
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A quiet bus is idle, but activated children and user scopes retain occupancy.
    /// 空闲总线不占用，激活后代及用户 scope 保留占用。
    /// # Arguments
    /// No parameters. 无参数。
    #[test]
    fn activated_applications_outlive_the_launcher_occupancy() {
        let units = serde_json::from_str::<Vec<OccupancyUnit>>(
            r#"[{"unit":"rstarsleep-dbus-1000.service","active":"active","load":"loaded"}]"#,
        )
        .unwrap();
        assert!(!occupied_units(&units, |_| Ok(1), |_| panic!("no user manager")).unwrap());
        assert!(occupied_units(&units, |_| Ok(2), |_| panic!("no user manager")).unwrap());
        assert!(
            occupied_units(
                &units,
                |_| Err(Error::Invalid("probe failed".into())),
                |_| panic!("no user manager")
            )
            .is_err()
        );
        let units = [OccupancyUnit {
            unit: "user@1000.service".into(),
            active: "active".into(),
        }];
        assert!(
            occupied_units(
                &units,
                |_| panic!("no bus"),
                |uid| {
                    assert_eq!(uid, 1000);
                    Ok(true)
                }
            )
            .unwrap()
        );
        assert!(!occupied_units(&units, |_| panic!("no bus"), |_| Ok(false)).unwrap());
        assert!(
            occupied_units(
                &units,
                |_| panic!("no bus"),
                |_| Err(Error::Invalid("user manager unavailable".into()))
            )
            .is_err()
        );
    }

    /// Active launch units and session transitions conservatively prevent idle shutdown.
    /// 活跃启动服务及会话转换保守地阻止空闲关机。
    /// # Arguments
    /// No parameters. 无参数。
    #[test]
    fn application_services_and_session_transitions_are_occupied() {
        for unit in [
            "rstarsleep-app-1000-1-1.service",
            "app-org.example.Editor.scope",
            "rstarsleep-dbus-1000.service",
            "user@1000.service",
        ] {
            assert!(
                occupied_units(
                    &[OccupancyUnit {
                        unit: unit.into(),
                        active: "deactivating".into(),
                    }],
                    |_| panic!("transitional bus is occupied"),
                    |_| panic!("transitional user manager is occupied")
                )
                .unwrap()
            );
        }
        assert!(!occupied_units(&[], |_| panic!("no bus"), |_| panic!("no user manager")).unwrap());
    }

    /// Exercise real pipes and socket framing without a privileged machine.
    /// 使用真实管道和 socket 帧，无需特权机器。
    /// # Arguments
    /// - `command`, `input`: Literal test child and optional binary stdin. 字面测试子进程及可选二进制 stdin。
    fn roundtrip(mut command: Command, input: Option<&[u8]>) -> (Vec<u8>, Vec<u8>, i32) {
        let (server, mut client) = UnixStream::pair().unwrap();
        server
            .set_write_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let wire = Arc::new(Wire {
            stream: Mutex::new(server.try_clone().unwrap()),
            disconnected: AtomicBool::new(false),
        });
        let handle = thread::spawn(move || {
            let mut child = command
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            let code = stream_child(
                &mut child,
                BufReader::new(server),
                wire.clone(),
                "test-machine",
                "test-unit.service",
                None,
            )
            .unwrap()
            .unwrap();
            wire.send(&Event::Exit { code }).unwrap();
        });
        if let Some(bytes) = input {
            protocol::write_frame(
                &mut client,
                &ClientMessage::Input {
                    bytes: bytes.into(),
                },
            )
            .unwrap();
            protocol::write_frame(&mut client, &ClientMessage::Eof).unwrap();
        }
        let mut reader = BufReader::new(client);
        let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
        let code = loop {
            match protocol::read_frame::<Event>(&mut reader).unwrap().unwrap() {
                Event::Ready { .. } => {}
                Event::Stdout { bytes } => stdout.extend(bytes),
                Event::Stderr { bytes } => stderr.extend(bytes),
                Event::Exit { code } => break code,
                event => panic!("unexpected test event: {event:?}"),
            }
        };
        handle.join().unwrap();
        (stdout, stderr, code)
    }

    /// Binary stdout/stderr stay separate and nonzero application exits survive.
    /// 二进制 stdout/stderr 保持分离，非零应用退出保持原值。
    /// # Arguments
    /// No parameters. 无参数。
    #[test]
    fn binary_streams_and_exit() {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "printf '\\377\\000ok'; printf error >&2; exit 23"]);
        let (stdout, stderr, code) = roundtrip(command, None);
        assert_eq!(stdout, [255, 0, b'o', b'k']);
        assert_eq!(stderr, b"error");
        assert_eq!(code, 23);
        let (stdout, stderr, code) = roundtrip(Command::new("/bin/cat"), Some(&[0, 255, b'x']));
        assert_eq!(stdout, [0, 255, b'x']);
        assert!(stderr.is_empty());
        assert_eq!(code, 0);
    }

    /// PTYs use requested dimensions and killed children retain their signal code.
    /// PTY 使用请求尺寸，被杀子进程保留信号退出码。
    /// # Arguments
    /// No parameters. 无参数。
    #[test]
    fn terminal_dimensions_and_signal_exit() {
        let (master, _slave) = pty(WindowSize {
            rows: 40,
            cols: 100,
        })
        .unwrap();
        let mut size: libc::winsize = unsafe { std::mem::zeroed() };
        assert_eq!(
            unsafe { libc::ioctl(master.as_raw_fd(), libc::TIOCGWINSZ, &mut size) },
            0
        );
        assert_eq!((size.ws_row, size.ws_col), (40, 100));
        let mut child = Command::new("/bin/sleep").arg("5").spawn().unwrap();
        child.kill().unwrap();
        assert_eq!(exit_code(child.wait().unwrap()), 128 + libc::SIGKILL);
    }
}
