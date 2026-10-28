//! Maintain private guest buses and filtered host input-method proxies.
//! 维护容器私有总线及经过滤的宿主输入法代理。
use super::resources::{beneath, runtime_directory, socket};
use super::{Context, Error, PreparedRuntime, Resource, Result, commands, open_at_guest};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io,
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{
            fs::{FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt},
            process::CommandExt,
        },
    },
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::Mutex,
    time::{Duration, Instant},
};
const PROXY: &str = "/usr/bin/xdg-dbus-proxy";
static CHILDREN: Mutex<BTreeMap<(String, u32), Child>> = Mutex::new(BTreeMap::new());

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ProxyRecord {
    pub(super) schema_version: u32,
    pub(super) uid: u32,
    pub(super) pid: u32,
    pub(super) starttime: u64,
    pub(super) dev: u64,
    pub(super) ino: u64,
}
struct StartingProxy(Option<Child>);
impl Drop for StartingProxy {
    /// Reap a newly started proxy whenever publication or readiness fails.
    /// 发布或就绪失败时回收新启动代理。
    /// # Arguments
    /// - `self`: Uncommitted proxy process. 未提交代理进程。
    fn drop(&mut self) {
        if let Some(child) = self.0.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}
struct StartingBus<'a> {
    runtime: &'a PreparedRuntime,
    unit: String,
    armed: bool,
}
impl Drop for StartingBus<'_> {
    /// Stop an unready guest bus on command, socket or timeout failures.
    /// 命令、socket 或超时失败时停止未就绪容器总线。
    /// # Arguments
    /// - `self`: Uncommitted guest bus startup. 未提交容器总线启动。
    fn drop(&mut self) {
        if self.armed {
            let mut stop = Command::new(commands::SYSTEMCTL);
            stop.arg(format!("--machine={}", self.runtime.machine))
                .args(["stop", &self.unit]);
            let _ = commands::output(stop, Duration::from_secs(8));
        }
    }
}

/// Start or reuse a guest-local private session bus.
/// 启动或复用容器本地私有会话总线。
/// # Arguments
/// - `runtime`, `uid`, `context`: Caller machine and live root. 调用者机器及运行根。
pub(super) fn private_bus(runtime: &PreparedRuntime, uid: u32, context: &Context) -> Result<()> {
    let socket_path = PathBuf::from(format!("/run/user/{uid}/rstarsleep-bus"));
    let unit = format!("rstarsleep-dbus-{uid}.service");
    let mut startup = StartingBus {
        runtime,
        unit: unit.clone(),
        armed: false,
    };
    let mut probe = Command::new(commands::SYSTEMCTL);
    probe
        .arg(format!("--machine={}", runtime.machine))
        .args(["is-active", "--quiet", &unit]);
    let active = commands::output(probe, Duration::from_secs(5))?
        .status
        .success();
    let visible = open_at_guest(&context.root, &socket_path, libc::O_PATH | libc::O_NOFOLLOW)
        .ok()
        .is_some_and(|file| socket(file, uid, false).is_ok());
    if active && !visible {
        let mut stop = Command::new(commands::SYSTEMCTL);
        stop.arg(format!("--machine={}", runtime.machine))
            .args(["stop", &unit]);
        if !commands::output(stop, Duration::from_secs(8))?
            .status
            .success()
        {
            return Err(Error::Invalid(
                "无法刷新隐藏的客体会话总线 / cannot refresh a hidden guest session bus".into(),
            ));
        }
    }
    if !active || !visible {
        commands::guest(
            runtime,
            &[
                "/usr/bin/rm".into(),
                "--force".into(),
                "--".into(),
                socket_path.to_string_lossy().into_owned(),
            ],
        )?;
        let mut command = Command::new(commands::SYSTEMD_RUN);
        command
            .arg(format!("--machine={}", runtime.machine))
            .arg(format!("--unit={unit}"))
            .arg(format!("--uid={uid}"))
            .args([
                "--quiet",
                "--collect",
                "--service-type=exec",
                "--expand-environment=no",
                "--property=TimeoutStopSec=5s",
                "--property=KillMode=control-group",
                "--property=TasksAccounting=yes",
                "--",
                "/usr/bin/dbus-daemon",
                "--session",
                "--nofork",
                "--nopidfile",
            ])
            .arg(format!("--address=unix:path={}", socket_path.display()));
        startup.armed = true;
        let output = commands::output(command, Duration::from_secs(10))?;
        if !output.status.success() {
            return Err(Error::Invalid(format!(
                "无法启动客体会话 D-Bus: {0} / cannot start guest session D-Bus: {0}",
                String::from_utf8_lossy(&output.stderr)
            )));
        }
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Ok(file) =
            open_at_guest(&context.root, &socket_path, libc::O_PATH | libc::O_NOFOLLOW)
        {
            socket(file, uid, false)?;
            startup.armed = false;
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(Error::Invalid(
                "客体会话 D-Bus 未就绪 / guest session D-Bus did not become ready".into(),
            ));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Identify a process lifetime separately from its reusable PID.
/// 独立于可复用 PID 标识进程生命周期。
/// # Arguments
/// - `pid`: Proxy process. 代理进程。
pub(super) fn starttime(pid: u32) -> Result<u64> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat"))?;
    stat.rsplit_once(')')
        .and_then(|(_, fields)| fields.split_whitespace().nth(19))
        .and_then(|value| value.parse().ok())
        .ok_or_else(|| Error::Invalid("代理进程身份无效 / invalid proxy process identity".into()))
}

/// Pin an active recorded proxy and reject stale PID reuse or changed ownership.
/// 固定记录中的活跃代理，拒绝过期 PID 复用或所有权变化。
/// # Arguments
/// - `record`: Root-owned process/source receipt. root 所属进程和来源记录。
pub(super) fn process(record: &ProxyRecord) -> Result<Option<File>> {
    let raw = unsafe { libc::syscall(libc::SYS_pidfd_open, record.pid, 0) };
    if raw < 0 {
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ESRCH) {
            return Ok(None);
        }
        return Err(error.into());
    }
    let fd = unsafe { File::from_raw_fd(raw as i32) };
    let mut poll = libc::pollfd {
        fd: fd.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    if unsafe { libc::poll(&mut poll, 1, 0) } < 0 {
        return Err(io::Error::last_os_error().into());
    }
    if poll.revents != 0 {
        return Ok(None);
    }
    let identity = match starttime(record.pid) {
        Ok(identity) => identity,
        Err(Error::Io { source, .. }) if source.kind() == io::ErrorKind::NotFound => {
            return Ok(None);
        }
        Err(error) => return Err(error),
    };
    let owner = match fs::metadata(format!("/proc/{}", record.pid)) {
        Ok(metadata) => metadata.uid(),
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if record.schema_version != 1 || identity != record.starttime || owner != record.uid {
        return Ok(None);
    }
    Ok(Some(fd))
}

/// Send termination through a pinned PID descriptor and bound the wait.
/// 经固定 PID 描述符发送终止并限制等待时长。
/// # Arguments
/// - `record`: Validated proxy receipt. 已校验代理记录。
pub(super) fn terminate(record: &ProxyRecord) -> Result<()> {
    if let Some(fd) = process(record)? {
        if unsafe {
            libc::syscall(
                libc::SYS_pidfd_send_signal,
                fd.as_raw_fd(),
                libc::SIGTERM,
                std::ptr::null::<libc::siginfo_t>(),
                0,
            )
        } < 0
            && io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
        {
            return Err(io::Error::last_os_error().into());
        }
        let mut poll = libc::pollfd {
            fd: fd.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let waited = unsafe { libc::poll(&mut poll, 1, 500) };
        if waited < 0 && io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
            return Err(io::Error::last_os_error().into());
        }
        if waited <= 0
            && unsafe {
                libc::syscall(
                    libc::SYS_pidfd_send_signal,
                    fd.as_raw_fd(),
                    libc::SIGKILL,
                    std::ptr::null::<libc::siginfo_t>(),
                    0,
                )
            } < 0
            && io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
        {
            return Err(io::Error::last_os_error().into());
        }
    }
    Ok(())
}

/// Resolve only the authenticated caller's host primary group.
/// 仅解析已认证调用者的宿主主组。
/// # Arguments
/// - `uid`: Authenticated host user. 已认证宿主用户。
fn primary_group(uid: u32) -> Result<u32> {
    let mut user: libc::passwd = unsafe { std::mem::zeroed() };
    let mut found = std::ptr::null_mut();
    let mut buffer = vec![0u8; 65536];
    let result = unsafe {
        libc::getpwuid_r(
            uid,
            &mut user,
            buffer.as_mut_ptr().cast(),
            buffer.len(),
            &mut found,
        )
    };
    if result != 0 {
        return Err(io::Error::from_raw_os_error(result).into());
    }
    if found.is_null() {
        return Err(Error::Invalid(
            "原生会话调用者没有宿主账户 / native session caller has no host account".into(),
        ));
    }
    Ok(user.pw_gid)
}

/// Execute the fixed proxy as the caller with a genuinely pinned bus address.
/// 以调用者身份运行固定代理，并使用确实固定的总线地址。
/// # Arguments
/// - `bus`, `uid`, `gid`, `output`: Pinned source, exact identity and private output. 固定来源、准确身份及私有输出。
pub(super) fn proxy_command(bus: &File, uid: u32, gid: u32, output: &Path) -> Command {
    let source = bus.as_raw_fd();
    let mut command = Command::new(PROXY);
    command
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .arg(format!("unix:path=/proc/self/fd/{source}"))
        .arg(output)
        .args([
            "--filter",
            "--talk=org.freedesktop.IBus",
            "--talk=org.fcitx.Fcitx",
            "--talk=org.fcitx.Fcitx5",
            // Allow only input-method portals; desktop portals remain filtered.
            // 仅允许输入法 portal；桌面 portal 继续受过滤限制。
            "--talk=org.freedesktop.portal.Fcitx",
            "--talk=org.freedesktop.portal.IBus",
        ]);
    // Credential changes happen here in the required order. Command::uid()
    // would drop privileges before a pre_exec closure could clear groups.
    // 在这里按所需顺序切换凭据；Command::uid() 会在 pre_exec 清空组之前降权。
    unsafe {
        command.pre_exec(move || {
            if libc::setgroups(0, std::ptr::null()) != 0
                || libc::setgid(gid) != 0
                || libc::setuid(uid) != 0
            {
                return Err(io::Error::last_os_error());
            }
            libc::umask(0o077);
            let flags = libc::fcntl(source, libc::F_GETFD);
            if flags < 0 || libc::fcntl(source, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    command
}

/// Stop only the caller's recorded proxy, including daemon-recovered processes.
/// 仅停止调用者记录的代理，包括守护进程恢复后的进程。
/// # Arguments
/// - `runtime`, `uid`: Known machine/caller identity. 已知机器及调用者身份。
pub(super) fn stop_proxy(runtime: &PreparedRuntime, uid: u32) -> Result<()> {
    let path = location(runtime).join(format!("{uid}.proxy.toml"));
    if path.exists() {
        super::super::trust::path(&path)?;
        let record: ProxyRecord = super::super::read_record(&path)?;
        if record.schema_version != 1 || record.uid != uid {
            return Err(Error::Invalid(
                "原生代理回执身份不匹配 / native proxy receipt identity mismatch".into(),
            ));
        }
        terminate(&record)?;
        fs::remove_file(path)?;
    }
    if let Some(mut child) = CHILDREN
        .lock()
        .map_err(|_| Error::Invalid("代理子进程锁损坏 / proxy child lock poisoned".into()))?
        .remove(&(runtime.machine.clone(), uid))
    {
        let _ = child.kill();
        let _ = child.wait();
    }
    Ok(())
}

/// Locate root-owned receipts independently of caller-owned proxy sockets.
/// 独立于调用者所属代理 socket 定位 root 所属记录。
/// # Arguments
/// - `runtime`: Known machine. 已知机器。
fn location(runtime: &PreparedRuntime) -> PathBuf {
    PathBuf::from("/run/rstarsleep/sessions").join(&runtime.machine)
}

/// Fail closed if native input-method integration cannot use a filtered proxy.
/// 原生输入法集成无法使用过滤代理时封闭失败。
/// # Arguments
/// - `runtime`, `uid`, `environment`: Authorized runtime and session hints. 已授权运行态及会话提示。
pub(super) fn native_bus(
    runtime: &PreparedRuntime,
    uid: u32,
    environment: &BTreeMap<String, String>,
) -> Result<Resource> {
    super::super::trust::path(Path::new(PROXY))?;
    let host = PathBuf::from(format!("/run/user/{uid}"));
    let expected = format!("unix:path={}/bus", host.display());
    if environment
        .get("DBUS_SESSION_BUS_ADDRESS")
        .is_some_and(|address| address != &expected)
    {
        return Err(Error::Invalid(
            "原生 D-Bus 必须使用已认证调用者的运行时总线 / native D-Bus must use the authenticated caller's runtime bus".into(),
        ));
    }
    let directory = runtime_directory(&host, uid)?;
    let bus = socket(beneath(&directory, Path::new("bus"))?, uid, false)?;
    let location = location(runtime);
    super::super::checked(&location)?;
    fs::create_dir_all(&location)?;
    super::super::trust::path(&location)?;
    let output_directory = location.join(uid.to_string());
    if !output_directory.exists() {
        fs::create_dir(&output_directory)?;
        let directory = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&output_directory)?;
        if unsafe { libc::fchown(directory.as_raw_fd(), uid, u32::MAX) } != 0 {
            return Err(io::Error::last_os_error().into());
        }
        directory.set_permissions(fs::Permissions::from_mode(0o700))?;
    }
    let output_root = runtime_directory(&output_directory, uid)?;
    let output_path = output_directory.join("bus");
    let receipt = location.join(format!("{uid}.proxy.toml"));
    let source_metadata = bus.metadata()?;
    if receipt.exists() {
        super::super::trust::path(&receipt)?;
        let record: ProxyRecord = super::super::read_record(&receipt)?;
        if record.uid != uid || record.schema_version != 1 {
            return Err(Error::Invalid(
                "原生代理回执身份不匹配 / native proxy receipt identity mismatch".into(),
            ));
        }
        if (record.dev, record.ino) == (source_metadata.dev(), source_metadata.ino())
            && process(&record)?.is_some()
            && let Ok(source) = beneath(&output_root, Path::new("bus"))
        {
            return Ok(Resource {
                source: socket(source, uid, false)?,
                target: host.join("rstarsleep-host-bus"),
                read_only: false,
            });
        }
        stop_proxy(runtime, uid)?;
    }
    if let Ok(metadata) = fs::symlink_metadata(&output_path) {
        if !metadata.file_type().is_socket() || metadata.uid() != uid {
            return Err(Error::Invalid(
                "原生代理套接字不符合预期 / unexpected native proxy socket".into(),
            ));
        }
        fs::remove_file(&output_path)?;
    }
    let child = proxy_command(&bus, uid, primary_group(uid)?, &output_path).spawn()?;
    let mut starting = StartingProxy(Some(child));
    let child = starting.0.as_mut().unwrap();
    let record = ProxyRecord {
        schema_version: 1,
        uid,
        pid: child.id(),
        starttime: starttime(child.id())?,
        dev: source_metadata.dev(),
        ino: source_metadata.ino(),
    };
    super::super::write_record(&receipt, &record)?;
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(status) = starting.0.as_mut().unwrap().try_wait()? {
            return Err(Error::Invalid(format!(
                "经过滤的原生 D-Bus 代理已退出: {status} / filtered native D-Bus proxy exited: {status}"
            )));
        }
        if let Ok(source) = beneath(&output_root, Path::new("bus")) {
            let source = socket(source, uid, false)?;
            CHILDREN
                .lock()
                .map_err(|_| Error::Invalid("代理子进程锁损坏 / proxy child lock poisoned".into()))?
                .insert((runtime.machine.clone(), uid), starting.0.take().unwrap());
            return Ok(Resource {
                source,
                target: host.join("rstarsleep-host-bus"),
                read_only: false,
            });
        }
        if Instant::now() >= deadline {
            return Err(Error::Invalid(
                "经过滤的原生 D-Bus 代理未就绪 / filtered native D-Bus proxy did not become ready"
                    .into(),
            ));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Stop all owned caller proxies after the machine stops, including recovered ones.
/// 机器停止后停止全部自有调用者代理，包括恢复的代理。
/// # Arguments
/// - `runtime`: Known stopped machine. 已知已停止机器。
pub(super) fn cleanup_proxies(runtime: &PreparedRuntime) -> Result<()> {
    let directory = location(runtime);
    if !directory.exists() {
        return Ok(());
    }
    super::super::trust::path(&directory)?;
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        if let Some(uid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.strip_suffix(".proxy.toml"))
            .and_then(|uid| uid.parse::<u32>().ok())
        {
            stop_proxy(runtime, uid)?;
        }
    }
    Ok(())
}
