//! Durable management jobs which survive SSH and daemon disconnects.
//! 跨 SSH 及 daemon 断连存活的持久管理任务。
use super::{Error, Result, client, daemon::BuildState, protocol::Operation};
use crate::{config::ConfigStore, controller};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::Read,
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{fs::OpenOptionsExt, process::CommandExt},
    },
    path::PathBuf,
    process::{Child, Command, ExitStatus, Stdio},
    sync::{Arc, mpsc},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
#[cfg(test)]
#[path = "admin_tests.rs"]
mod tests;

#[derive(Debug, Serialize, Deserialize)]
pub struct Job {
    pub task: String,
    pub operation: String,
    pub state: String,
    pub started: u64,
    pub finished: Option<u64>,
    pub exit_code: Option<i32>,
    pub error: Option<String>,
}

/// Resolve a subsystem's durable management job directory.
/// 解析子系统的持久管理任务目录。
/// # Arguments
/// - `store`, `id`: Fixed manager and subsystem. 固定管理器及子系统。
pub fn directory(store: &ConfigStore, id: &str) -> Result<PathBuf> {
    Ok(store.system_layout(id)?.root.join("runtime/jobs"))
}

/// Return the current Unix timestamp without ignoring clock errors.
/// 返回当前 Unix 时间戳，不忽略时钟错误。
/// # Arguments
/// No parameters. 无参数。
fn now() -> Result<u64> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| Error::Invalid(format!("时钟无效 / invalid clock: {e}")))?
        .as_secs())
}

/// Submit a fixed operation after configuration-only preflight.
/// 仅依赖配置的预检完成后提交固定操作。
/// # Arguments
/// - `store`, `id`, `operation`: Fixed manager, identity and operation. 固定管理器、身份及操作。
/// - `builds`: Shared SSH build state owned by this daemon. 此 daemon 所属的共享 SSH 构建状态。
pub fn submit(
    store: &ConfigStore,
    id: &str,
    operation: &str,
    builds: &Arc<BuildState>,
) -> Result<Job> {
    super::trust::store(store, Some(id))?;
    super::SubsystemConfig::read(store, id)?;
    let task = format!(
        "{operation}-{}-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| Error::Invalid(format!("时钟无效 / invalid clock: {e}")))?
            .as_nanos(),
        std::process::id()
    );
    let registration = if operation == "build" {
        Some(builds.begin(id, &task)?)
    } else {
        None
    };
    match operation {
        "build" => {
            controller::check_configuration(store, id)?;
        }
        "start" | "restart" => {
            controller::read_current_build(store, id)?;
        }
        "stop" => {}
        _ => {
            return Err(Error::Invalid(
                "未知管理操作 / unknown management operation".into(),
            ));
        }
    }
    let binary = fs::canonicalize(std::env::current_exe()?)?;
    super::trust::path(&binary)?;
    let directory = directory(store, id)?;
    super::checked(&directory)?;
    fs::create_dir_all(&directory)?;
    let lock = if operation == "build" {
        None
    } else {
        Some(
            OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
                .open(directory.join("lifecycle.lock"))?,
        )
    };
    if lock.as_ref().is_some_and(
        |lock| unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0,
    ) {
        return Err(Error::Invalid(
            "同类管理任务正在执行 / a management job of this kind is active".into(),
        ));
    }
    let mut job = Job {
        task: task.clone(),
        operation: operation.into(),
        state: "queued".into(),
        started: now()?,
        finished: None,
        exit_code: None,
        error: None,
    };
    let log = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(directory.join(format!("{task}.log")))?;
    let live = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_CLOEXEC)
        .open(directory.join(format!("{task}.lock")))?;
    if unsafe { libc::flock(live.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err(Error::Invalid("任务锁冲突 / job lock conflict".into()));
    }
    super::write_record(&directory.join(format!("{task}.toml")), &job)?;
    if let Some(registration) = &registration {
        registration.recorded()?;
    }
    let descriptors = lock
        .iter()
        .chain(std::iter::once(&live))
        .map(AsRawFd::as_raw_fd)
        .collect::<Vec<_>>();
    let mut command = Command::new(binary);
    command
        .args(["__management-job"])
        .arg(store.root())
        .arg(id)
        .arg(&task)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log.try_clone()?))
        .stderr(Stdio::from(log));
    for descriptor in &descriptors {
        command.arg(descriptor.to_string());
    }
    unsafe {
        command.pre_exec(move || {
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            for descriptor in &descriptors {
                if libc::fcntl(*descriptor, libc::F_SETFD, 0) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
            }
            Ok(())
        });
    }
    // Create the waiter before the child so thread failure cannot leave an untracked job.
    // 先创建等待线程再启动子进程，避免线程创建失败后遗留未跟踪任务。
    let (sender, receiver) = mpsc::channel::<Child>();
    let waiter = std::thread::Builder::new()
        .name(format!("rstarsleep-job-{id}"))
        .spawn(move || {
            let _lock = lock;
            let _live = live;
            let _registration = registration;
            if let Ok(mut child) = receiver.recv() {
                let _ = child.wait();
            }
        })?;
    match command.spawn() {
        Ok(child) => {
            if let Err(mpsc::SendError(mut child)) = sender.send(child) {
                let _ = child.kill();
                let _ = child.wait();
                return Err(Error::Invalid(
                    "任务等待线程已退出 / job waiter exited".into(),
                ));
            }
        }
        Err(error) => {
            drop(sender);
            let _ = waiter.join();
            job.state = "failed".into();
            job.finished = Some(now()?);
            job.error = Some(format!("任务启动失败 / job launch failed: {error}"));
            super::write_record(&directory.join(format!("{task}.toml")), &job)?;
            return Err(error.into());
        }
    }
    Ok(job)
}

/// Execute an independently supervised job with durable completion status.
/// 执行独立监督任务，并持久记录完成状态。
/// # Arguments
/// - `arguments`: Manager root, subsystem and fixed task identifier. 管理根、子系统及固定任务标识。
pub fn job_entry(arguments: &[std::ffi::OsString]) -> Result<()> {
    if !matches!(arguments.len(), 4 | 5) {
        return Err(Error::Invalid(
            "内部任务参数无效 / invalid internal job arguments".into(),
        ));
    }
    let store = ConfigStore::new(&arguments[0])?;
    let id = arguments[1]
        .to_str()
        .ok_or_else(|| Error::Invalid("系统标识无效 / invalid system identity".into()))?;
    let task = arguments[2]
        .to_str()
        .ok_or_else(|| Error::Invalid("任务标识无效 / invalid task identity".into()))?;
    let path = directory(&store, id)?.join(format!("{task}.toml"));
    if task.contains('/') || task.contains("..") {
        return Err(Error::Invalid(
            "任务标识无效 / invalid task identity".into(),
        ));
    }
    super::trust::store(&store, Some(id))?;
    let locks = inherited_locks(&arguments[3..])?;
    let supervisor = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(directory(&store, id)?.join(format!("{task}.supervisor")))?;
    if unsafe { libc::flock(supervisor.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err(Error::Invalid(
            "监督器锁冲突 / supervisor lock conflict".into(),
        ));
    }
    let mut job: Job = super::read_record(&path)?;
    job.state = "running".into();
    super::write_record(&path, &job)?;
    let mut cancelled = false;
    let result = (|| -> Result<()> {
        if job.operation == "build" {
            let mut command = Command::new(fs::canonicalize(std::env::current_exe()?)?);
            command.arg("__management-build").arg(store.root()).arg(id);
            for lock in &locks {
                command.arg(lock.as_raw_fd().to_string());
            }
            let descriptors: Vec<_> = locks.iter().map(AsRawFd::as_raw_fd).collect();
            command.process_group(0);
            unsafe {
                command.pre_exec(move || {
                    for descriptor in &descriptors {
                        if libc::fcntl(*descriptor, libc::F_SETFD, 0) < 0 {
                            return Err(std::io::Error::last_os_error());
                        }
                    }
                    Ok(())
                });
            }
            let cancel = directory(&store, id)?.join(format!("{task}.cancel"));
            let (status, stopped) = supervise(command.spawn()?, &cancel)?;
            cancelled = stopped;
            if stopped {
                return Err(Error::Invalid("构建已停止 / build stopped".into()));
            }
            if !status.success() {
                return Err(Error::Invalid(format!("构建失败 / build failed: {status}")));
            }
        } else {
            let operation = match job.operation.as_str() {
                "start" => Operation::Start { id: id.into() },
                "stop" => Operation::Stop { id: id.into() },
                "restart" => Operation::Restart { id: id.into() },
                _ => {
                    return Err(Error::Invalid(
                        "未知任务操作 / unknown job operation".into(),
                    ));
                }
            };
            let statuses = client::request(&client::socket_for_root(store.root())?, operation)?;
            println!(
                "{}",
                serde_json::to_string(&statuses).map_err(|e| Error::Protocol(format!(
                    "状态编码失败 / status encoding failed: {e}"
                )))?
            );
        }
        Ok(())
    })();
    job.finished = Some(now()?);
    job.exit_code = Some(if cancelled {
        130
    } else if result.is_ok() {
        0
    } else {
        1
    });
    job.state = if cancelled {
        "cancelled"
    } else if result.is_ok() {
        "complete"
    } else {
        "failed"
    }
    .into();
    if let Err(error) = &result {
        eprintln!("{error}");
        job.error = Some(error.to_string());
    }
    super::write_record(&path, &job)?;
    // Completion is authoritative; do not emit more output after publishing it.
    // 完成记录为最终状态；发布后不再输出诊断。
    Ok(())
}

/// Hold inherited job locks, but never leak them into plugin processes.
/// 持有继承的任务锁，但绝不将其泄露给插件进程。
fn inherited_locks(arguments: &[std::ffi::OsString]) -> Result<Vec<File>> {
    let mut locks = Vec::new();
    for argument in arguments {
        let fd = argument
            .to_str()
            .and_then(|s| s.parse::<i32>().ok())
            .filter(|fd| *fd > 2)
            .ok_or_else(|| {
                Error::Invalid("任务锁描述符无效 / invalid job lock descriptor".into())
            })?;
        if locks.iter().any(|file: &File| file.as_raw_fd() == fd) {
            return Err(Error::Invalid(
                "任务锁描述符重复 / duplicate job lock descriptor".into(),
            ));
        }
        if unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        locks.push(unsafe { File::from_raw_fd(fd) });
    }
    Ok(locks)
}

/// Execute the build worker while retaining its liveness independently of its supervisor.
/// 执行构建工作进程，并独立于监督进程保留存活标记。
pub fn build_entry(arguments: &[std::ffi::OsString]) -> Result<()> {
    if arguments.len() != 3 {
        return Err(Error::Invalid(
            "构建工作进程参数无效 / invalid build worker arguments".into(),
        ));
    }
    let store = ConfigStore::new(&arguments[0])?;
    let id = arguments[1]
        .to_str()
        .ok_or_else(|| Error::Invalid("系统标识无效 / invalid system identity".into()))?;
    super::trust::store(&store, Some(id))?;
    let _locks = inherited_locks(&arguments[2..])?;
    controller::with_terminal_progress(|| controller::build(&store, Some(id)))?;
    Ok(())
}

/// Observe exit without reaping, so process-group signals cannot target a reused PID.
/// 不回收地观察退出，避免进程组信号误中复用的 PID。
fn exited(child: &Child) -> std::io::Result<bool> {
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    if unsafe {
        libc::waitid(
            libc::P_PID,
            child.id(),
            &mut info,
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
        )
    } < 0
    {
        return Err(std::io::Error::last_os_error());
    }
    Ok(unsafe { info.si_pid() } != 0)
}

fn signal_group(child: &Child, signal: i32) -> std::io::Result<()> {
    if unsafe { libc::kill(-(child.id() as i32), signal) } < 0 {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::ESRCH) {
            return Err(error);
        }
    }
    Ok(())
}

/// Keep cancellation outside SSH and wait for worker cleanup before reporting completion.
/// 在 SSH 之外处理取消，等待工作进程清理后才报告完成。
fn supervise(mut child: Child, cancel: &std::path::Path) -> Result<(ExitStatus, bool)> {
    let result = (|| -> Result<bool> {
        loop {
            if exited(&child)? {
                return Ok(false);
            }
            if cancel.try_exists()? {
                signal_group(&child, libc::SIGTERM)?;
                let deadline = Instant::now() + Duration::from_secs(2);
                while !exited(&child)? && Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(20));
                }
                return Ok(true);
            }
            std::thread::sleep(Duration::from_millis(30));
        }
    })();
    // Keep the leader unreaped until all remaining members have received SIGKILL.
    // 在其余成员收到 SIGKILL 前保留未回收的组长。
    let cleanup = signal_group(&child, libc::SIGKILL);
    let status = child.wait();
    cleanup?;
    let status = status?;
    Ok((status, result?))
}

/// Find the SSH build tracked by this daemon, with durable worker diagnostics.
/// 查找此 daemon 跟踪的 SSH 构建，并附带持久工作进程诊断。
pub fn active_build(store: &ConfigStore, id: &str, builds: &BuildState) -> Result<Option<Job>> {
    let Some(build) = builds.active(id)? else {
        return Ok(None);
    };
    if !build.recorded {
        return Ok(Some(Job {
            task: build.task,
            operation: "build".into(),
            state: "preparing".into(),
            // No durable start timestamp exists until preflight completes.
            // 预检完成前尚无持久化的开始时间戳。
            started: 0,
            finished: None,
            exit_code: None,
            error: None,
        }));
    }
    let job = jobs(store, id)?
        .into_iter()
        .find(|job| job.task == build.task)
        .ok_or_else(|| Error::Invalid("构建任务记录丢失 / build job record was lost".into()))?;
    Ok(Some(job))
}

/// Request cancellation of exactly the observed build; never signal a recorded PID.
/// 只请求取消已观察到的构建，绝不向记录的 PID 发信号。
pub fn stop_build(store: &ConfigStore, id: &str, builds: &BuildState) -> Result<Option<Job>> {
    let Some(job) = active_build(store, id, builds)? else {
        return Ok(None);
    };
    if job.state == "preparing" {
        return Err(Error::Invalid("正在检查构建配置，请在任务启动后停止 / build configuration is being checked; stop the job after it starts".into()));
    }
    if !matches!(job.state.as_str(), "queued" | "running") {
        return Ok(None);
    }
    request_stop(store, id, &job)?;
    Ok(Some(job))
}

/// Bind a cancellation request to one immutable task identity.
/// 将取消请求绑定到一个不可变的任务标识。
pub fn request_stop(store: &ConfigStore, id: &str, job: &Job) -> Result<()> {
    super::protocol::validate_id(&job.task)?;
    if job.operation != "build" {
        return Err(Error::Invalid(
            "只能停止构建任务 / only build jobs can be stopped".into(),
        ));
    }
    if job.state == "running" {
        let supervisor = OpenOptions::new().read(true).write(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(directory(store, id)?.join(format!("{}.supervisor", job.task)))
            .map_err(|e| Error::Invalid(format!("构建监督器不可用，无法安全停止 / build supervisor is unavailable; cannot stop safely: {e}")))?;
        if !held(&supervisor)? {
            return Err(Error::Invalid(
                "构建监督器已退出，无法安全停止 / build supervisor has exited; cannot stop safely"
                    .into(),
            ));
        }
    }
    let path = directory(store, id)?.join(format!("{}.cancel", job.task));
    super::checked(&path)?;
    super::write_record(&path, job)?;
    Ok(())
}

fn held(file: &File) -> Result<bool> {
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
        return Ok(false);
    }
    let error = std::io::Error::last_os_error();
    if error.kind() == std::io::ErrorKind::WouldBlock {
        Ok(true)
    } else {
        Err(error.into())
    }
}

/// Enumerate durable tasks with explicit incomplete-worker detection.
/// 枚举持久任务，并显式检测未完成工作进程。
/// # Arguments
/// - `store`, `id`: Fixed subsystem. 固定子系统。
pub fn jobs(store: &ConfigStore, id: &str) -> Result<Vec<Job>> {
    let directory = directory(store, id)?;
    if !directory.try_exists()? {
        return Ok(Vec::new());
    }
    let mut jobs = Vec::new();
    for entry in fs::read_dir(&directory)? {
        let entry = entry?;
        if entry.path().extension().is_some_and(|e| e == "toml") {
            let mut job: Job = super::read_record(&entry.path())?;
            if matches!(job.state.as_str(), "queued" | "running") {
                let file = OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(directory.join(format!("{}.lock", job.task)))?;
                if !held(&file)? {
                    job.state = "interrupted".into();
                    job.error = Some("任务未记录完成且工作进程已结束 / worker ended without recording completion".into());
                } else if job.operation == "build" && job.state == "running" {
                    let supervisor = directory.join(format!("{}.supervisor", job.task));
                    if supervisor.try_exists()?
                        && !held(
                            &OpenOptions::new()
                                .read(true)
                                .write(true)
                                .custom_flags(libc::O_NOFOLLOW)
                                .open(supervisor)?,
                        )?
                    {
                        job.error = Some("构建监督器已丢失 / build supervisor was lost".into());
                    }
                }
            }
            jobs.push(job);
        }
    }
    jobs.sort_by(|a, b| a.started.cmp(&b.started).then_with(|| a.task.cmp(&b.task)));
    Ok(jobs)
}

/// Read bounded task output without exposing a host filesystem path.
/// 有界读取任务输出，不暴露宿主文件系统路径。
/// # Arguments
/// - `store`, `id`, `task`: Fixed subsystem and listed task. 固定子系统及已列出任务。
pub fn log(store: &ConfigStore, id: &str, task: &str) -> Result<Vec<u8>> {
    if !jobs(store, id)?.iter().any(|job| job.task == task) {
        return Err(Error::Invalid("任务不存在 / task does not exist".into()));
    }
    let mut bytes = Vec::new();
    File::open(directory(store, id)?.join(format!("{task}.log")))?
        .take(16 * 1024 * 1024)
        .read_to_end(&mut bytes)?;
    Ok(bytes)
}
