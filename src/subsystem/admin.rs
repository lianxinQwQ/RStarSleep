//! Durable management jobs which survive SSH and daemon disconnects.
//! 跨 SSH 及 daemon 断连存活的持久管理任务。
use super::{Error, Result, client, protocol::Operation};
use crate::{config::ConfigStore, controller};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::Read,
    os::{
        fd::AsRawFd,
        unix::{fs::OpenOptionsExt, process::CommandExt},
    },
    path::PathBuf,
    process::{Command, Stdio},
    time::{SystemTime, UNIX_EPOCH},
};

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
pub fn submit(store: &ConfigStore, id: &str, operation: &str) -> Result<Job> {
    super::trust::store(store, Some(id))?;
    super::SubsystemConfig::read(store, id)?;
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
    let directory = directory(store, id)?;
    super::checked(&directory)?;
    fs::create_dir_all(&directory)?;
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(directory.join(if operation == "build" {
            "build.lock"
        } else {
            "lifecycle.lock"
        }))?;
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err(Error::Invalid(
            "同类管理任务正在执行 / a management job of this kind is active".into(),
        ));
    }
    let task = format!(
        "{operation}-{}-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| Error::Invalid(format!("时钟无效 / invalid clock: {e}")))?
            .as_nanos(),
        std::process::id()
    );
    let mut job = Job {
        task: task.clone(),
        operation: operation.into(),
        state: "queued".into(),
        started: now()?,
        finished: None,
        exit_code: None,
        error: None,
    };
    super::write_record(&directory.join(format!("{task}.toml")), &job)?;
    let log = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(directory.join(format!("{task}.log")))?;
    let binary = fs::canonicalize(std::env::current_exe()?)?;
    super::trust::path(&binary)?;
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
    let descriptor = lock.as_raw_fd();
    let live_descriptor = live.as_raw_fd();
    let mut command = Command::new(binary);
    command
        .args(["__management-job"])
        .arg(store.root())
        .arg(id)
        .arg(&task)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log.try_clone()?))
        .stderr(Stdio::from(log));
    unsafe {
        command.pre_exec(move || {
            if libc::setsid() < 0
                || libc::fcntl(descriptor, libc::F_SETFD, 0) < 0
                || libc::fcntl(live_descriptor, libc::F_SETFD, 0) < 0
            {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    match command.spawn() {
        Ok(mut child) => {
            std::thread::spawn(move || {
                let _lock = lock;
                let _live = live;
                let _ = child.wait();
            });
        }
        Err(error) => {
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
    if arguments.len() != 3 {
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
    let mut job: Job = super::read_record(&path)?;
    job.state = "running".into();
    super::write_record(&path, &job)?;
    let result = (|| -> Result<()> {
        if job.operation == "build" {
            controller::build(&store, Some(id))?;
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
    job.exit_code = Some(if result.is_ok() { 0 } else { 1 });
    job.state = if result.is_ok() { "complete" } else { "failed" }.into();
    if let Err(error) = &result {
        eprintln!("{error}");
        job.error = Some(error.to_string());
    }
    super::write_record(&path, &job)?;
    result
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
                if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
                    job.state = "interrupted".into();
                    job.error = Some("任务未记录完成且工作进程已结束 / worker ended without recording completion".into());
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
