//! Bounded systemd operations shared by runtime integrations.
//! 运行集成共用的有界 systemd 操作。
use super::{Error, PreparedRuntime, Result};
use std::{
    io,
    os::unix::process::CommandExt,
    process::{Child, Command, Output, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::{Duration, Instant},
};

static CALL_SEQUENCE: AtomicU64 = AtomicU64::new(1);
struct ManagedChild(Child);
impl Drop for ManagedChild {
    /// Stop an unfinished management process group on every error path.
    /// 在每个错误路径停止未完成的管理进程组。
    /// # Arguments
    /// - `self`: Session-leader child. 会话首子进程。
    fn drop(&mut self) {
        if !matches!(self.0.try_wait(), Ok(Some(_))) {
            unsafe {
                libc::kill(-(self.0.id() as i32), libc::SIGKILL);
            }
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

pub(crate) const MACHINECTL: &str = "/usr/bin/machinectl";
pub(crate) const SYSTEMD_RUN: &str = "/usr/bin/systemd-run";
pub(crate) const SYSTEMCTL: &str = "/usr/bin/systemctl";

/// Execute a small management command with bounded output and time.
/// 有界执行小型管理命令，限制输出与时间。
/// # Arguments
/// - `command`: Literal command. 字面命令。
/// - `timeout`: Maximum duration. 最长时长。
pub(crate) fn output(mut command: Command, timeout: Duration) -> Result<Output> {
    use std::io::{Read, Seek};
    let mut stdout = tempfile::tempfile()?;
    let mut stderr = tempfile::tempfile()?;
    command
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout.try_clone()?))
        .stderr(Stdio::from(stderr.try_clone()?));
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = ManagedChild(command.spawn()?);
    let deadline = Instant::now() + timeout;
    let status = loop {
        if let Some(status) = child.0.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline
            || stdout.metadata()?.len() > 1024 * 1024
            || stderr.metadata()?.len() > 1024 * 1024
        {
            return Err(Error::Invalid(
                "management command exceeded time/output limit".into(),
            ));
        }
        thread::sleep(Duration::from_millis(20));
    };
    stdout.rewind()?;
    stderr.rewind()?;
    let mut out = Vec::new();
    let mut err = Vec::new();
    stdout.take(1024 * 1024 + 1).read_to_end(&mut out)?;
    stderr.take(1024 * 1024 + 1).read_to_end(&mut err)?;
    if out.len() > 1024 * 1024 || err.len() > 1024 * 1024 {
        return Err(io::Error::other("management output exceeded limit").into());
    }
    Ok(Output {
        status,
        stdout: out,
        stderr: err,
    })
}

/// Probe registration without treating tool errors as a stopped machine.
/// 探测注册，不将工具错误视作机器停止。
/// # Arguments
/// - `machine`: Deterministic registered name. 确定注册名称。
pub(crate) fn alive(machine: &str) -> Result<bool> {
    let mut command = Command::new(MACHINECTL);
    command.args(["show", "--property=Leader", "--value", machine]);
    let result = output(command, Duration::from_secs(5))?;
    if result.status.success() {
        return Ok(String::from_utf8_lossy(&result.stdout)
            .trim()
            .parse::<u32>()
            .is_ok_and(|p| p > 0));
    }
    let mut probe = Command::new(MACHINECTL);
    probe.args(["list", "--no-legend", "--no-pager"]);
    if !output(probe, Duration::from_secs(5))?.status.success() {
        return Err(Error::Invalid("systemd-machined unavailable".into()));
    }
    Ok(false)
}

/// Execute a privileged literal argument vector in a running machine.
/// 在运行机器中执行特权字面参数数组。
/// # Arguments
/// - `runtime`: Registered runtime. 已注册运行态。
/// - `argv`: Program and arguments. 程序和参数。
pub(crate) fn guest(runtime: &PreparedRuntime, argv: &[String]) -> Result<Output> {
    let unit = format!(
        "rstarsleep-call-{}-{}.service",
        std::process::id(),
        CALL_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    );
    let mut command = Command::new(SYSTEMD_RUN);
    command
        .arg(format!("--machine={}", runtime.machine))
        .arg(format!("--unit={unit}"))
        .args([
            "--uid=0",
            "--gid=0",
            "--wait",
            "--pipe",
            "--quiet",
            "--collect",
            "--service-type=exec",
            "--expand-environment=no",
            "--property=ExitType=cgroup",
            "--property=TimeoutStopSec=5s",
            "--",
        ])
        .args(argv);
    let result = match output(command, Duration::from_secs(15)) {
        Ok(result) => result,
        Err(error) => {
            let mut stop = Command::new(SYSTEMCTL);
            stop.arg(format!("--machine={}", runtime.machine))
                .args(["stop", &unit]);
            let _ = output(stop, Duration::from_secs(8));
            return Err(error);
        }
    };
    if !result.status.success() {
        return Err(Error::Invalid(format!(
            "guest command failed: {}",
            String::from_utf8_lossy(&result.stderr)
        )));
    }
    Ok(result)
}

/// Read the machine leader PID from machined.
/// 从 machined 读取机器首进程 PID。
/// # Arguments
/// - `runtime`: Registered runtime. 已注册运行态。
pub(crate) fn leader(runtime: &PreparedRuntime) -> Result<u32> {
    let mut command = Command::new(MACHINECTL);
    command.args(["show", "--property=Leader", "--value", &runtime.machine]);
    let result = output(command, Duration::from_secs(5))?;
    if !result.status.success() {
        return Err(Error::Invalid("cannot query machine leader".into()));
    }
    String::from_utf8_lossy(&result.stdout)
        .trim()
        .parse::<u32>()
        .ok()
        .filter(|p| *p > 0)
        .ok_or_else(|| Error::Invalid("invalid machine leader".into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    /// Capture independent streams and stop timed-out commands without waiting forever.
    /// 捕获独立流并停止超时命令，不无限等待。
    /// # Arguments
    /// No parameters. 无参数。
    #[test]
    fn bounded_management_commands() {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "printf output; printf error >&2; exit 23"]);
        let result = output(command, Duration::from_secs(2)).unwrap();
        assert_eq!(result.status.code(), Some(23));
        assert_eq!(result.stdout, b"output");
        assert_eq!(result.stderr, b"error");
        let mut command = Command::new("/bin/sleep");
        command.arg("5");
        let started = Instant::now();
        assert!(output(command, Duration::from_millis(40)).is_err());
        assert!(started.elapsed() < Duration::from_secs(2));
    }
}
