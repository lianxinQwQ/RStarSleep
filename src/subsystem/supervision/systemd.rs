//! Supervise recorders in independent systemd transient services.
//! 使用独立 systemd 临时服务监督记录器。
use super::{Phase, Service, Status, Supervisor};
use crate::subsystem::{Error, Result, commands};
use std::{
    collections::BTreeMap,
    process::{Command, Output},
    time::{Duration, Instant},
};

pub(crate) struct Systemd;

/// Parse named properties without depending on localized diagnostics or output order.
/// 解析具名属性，不依赖本地化诊断或输出顺序。
fn parse(bytes: &[u8]) -> Result<Status> {
    let text = std::str::from_utf8(bytes).map_err(|_| {
        Error::Invalid("服务状态必须为 UTF-8 / service status must be UTF-8".into())
    })?;
    let fields: BTreeMap<_, _> = text
        .lines()
        .filter_map(|line| line.split_once('='))
        .collect();
    let field = |name| {
        fields.get(name).copied().ok_or_else(|| {
            Error::Invalid(format!(
                "服务状态缺少 {name} / service status missing {name}"
            ))
        })
    };
    let phase = if field("LoadState")? == "not-found" {
        Phase::Missing
    } else {
        if field("LoadState")? != "loaded" {
            return Err(Error::Invalid(format!(
                "控制台服务加载失败 / console service load failed: {}",
                field("LoadState")?
            )));
        }
        match field("ActiveState")? {
            "inactive" => Phase::Inactive,
            "activating" => Phase::Starting,
            "active" | "reloading" | "refreshing" => Phase::Running,
            "deactivating" => Phase::Stopping,
            "failed" => Phase::Failed,
            state => {
                return Err(Error::Invalid(format!(
                    "未知服务状态 / unknown service state: {state}"
                )));
            }
        }
    };
    let status = Status {
        phase,
        identity: fields
            .get("Description")
            .copied()
            .unwrap_or_default()
            .into(),
        invocation: fields
            .get("InvocationID")
            .copied()
            .unwrap_or_default()
            .into(),
        result: fields.get("Result").copied().unwrap_or_default().into(),
        exit_status: fields
            .get("ExecMainStatus")
            .copied()
            .unwrap_or_default()
            .into(),
    };
    if status.phase == Phase::Running {
        let pid = field("MainPID")?.parse::<u32>().unwrap_or(0);
        if pid == 0
            || status.invocation.len() != 32
            || !status.invocation.bytes().all(|c| c.is_ascii_hexdigit())
        {
            return Err(Error::Invalid(
                "控制台服务缺少进程或启动身份 / console service missing process or invocation identity".into()
            ));
        }
    }
    Ok(status)
}

/// Build literal argv; transient services have no dependency on the daemon unit.
/// 构造字面参数；临时服务不依赖 daemon 服务生命周期。
fn launch(service: &Service) -> Command {
    let mut command = Command::new(commands::SYSTEMD_RUN);
    command
        .args(["--quiet", "--expand-environment=no", "--service-type=exec"])
        .arg(format!("--unit={}", service.name))
        .arg(format!("--description={}", service.identity))
        .args([
            "--property=User=root",
            "--property=Group=root",
            "--property=Restart=no",
            "--property=Delegate=yes",
            "--property=KillMode=control-group",
            "--property=StandardInput=null",
            "--property=StandardOutput=null",
            "--property=StandardError=journal",
            "--setenv=PATH=/usr/sbin:/usr/bin:/sbin:/bin",
            "--",
        ])
        .arg(&service.executable)
        .args(&service.arguments);
    command
}

/// Query through a bounded runner so transport failures remain distinct from absent units.
/// 通过有界执行器查询，区分传输失败和服务不存在。
fn query(name: &str, execute: &impl Fn(Command, Duration) -> Result<Output>) -> Result<Status> {
    let mut command = Command::new(commands::SYSTEMCTL);
    command.args([
        "show",
        "--all",
        "--no-pager",
        "--property=LoadState,ActiveState,Description,InvocationID,MainPID,Result,ExecMainStatus",
        "--",
        name,
    ]);
    let output = execute(command, Duration::from_secs(5))?;
    let status = parse(&output.stdout);
    if !output.status.success()
        && !(output.status.code() == Some(4)
            && status.as_ref().is_ok_and(|s| s.phase == Phase::Missing))
    {
        return Err(Error::Invalid(format!(
            "无法查询控制台服务 {name} / cannot query console service {name}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    status
}

/// Converge concurrent starts without replacing an existing invocation.
/// 收敛并发启动，不替换已有运行。
fn start(service: &Service, execute: &impl Fn(Command, Duration) -> Result<Output>) -> Result<()> {
    let before = query(&service.name, execute)?;
    before.verify(&service.identity)?;
    if before.live() {
        return Ok(());
    }
    if before.phase == Phase::Failed {
        let mut reset = Command::new(commands::SYSTEMCTL);
        reset.args(["reset-failed", "--", &service.name]);
        let output = execute(reset, Duration::from_secs(5))?;
        if !output.status.success() {
            return Err(Error::Invalid(format!(
                "无法重置控制台失败状态 / cannot reset failed console service: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
    }
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let output = execute(launch(service), Duration::from_secs(10))?;
        if output.status.success() {
            return Ok(());
        }
        // A competing start may have won; never replace or stop that invocation.
        // 并发启动可能已成功；绝不替换或停止该次运行。
        let after = query(&service.name, execute)?;
        after.verify(&service.identity)?;
        if after.live() {
            return Ok(());
        }
        // A completed transient unit can briefly remain loaded while systemd collects it.
        // 已完成临时服务在 systemd 回收期间可能短暂保持加载状态。
        if after.phase == Phase::Inactive && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
            continue;
        }
        return Err(Error::Invalid(format!(
            "无法启动控制台服务 {} / cannot start console service {}: {}",
            service.name,
            service.name,
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
}

impl Supervisor for Systemd {
    fn query(&self, name: &str) -> Result<Status> {
        query(name, &commands::output)
    }
    fn start(&self, service: &Service) -> Result<()> {
        start(service, &commands::output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Absence is distinct from malformed or unsupported manager responses.
    /// 服务不存在区别于管理器响应损坏或不受支持。
    #[test]
    fn named_states_and_failures() {
        assert_eq!(
            parse(b"LoadState=not-found\n").unwrap().phase,
            Phase::Missing
        );
        assert!(parse(b"").is_err());
        assert!(parse(b"LoadState=error\n").is_err());
        for (name, expected) in [
            ("activating", Phase::Starting),
            ("deactivating", Phase::Stopping),
            ("failed", Phase::Failed),
            ("inactive", Phase::Inactive),
        ] {
            let status = parse(
                format!(
                    "ActiveState={name}\nResult=exit-code\nExecMainStatus=7\nLoadState=loaded\n"
                )
                .as_bytes(),
            )
            .unwrap();
            assert_eq!(status.phase, expected);
            assert_eq!(status.exit_status, "7");
        }
        assert!(parse(b"LoadState=loaded\nActiveState=active\nMainPID=0\n").is_err());
    }

    /// Service arguments preserve shell characters and never attach daemon lifecycle dependencies.
    /// 服务参数保留 shell 字符，且不绑定 daemon 生命周期依赖。
    #[test]
    fn independent_literal_service() {
        let service = Service {
            name: "test.service".into(),
            identity: "identity".into(),
            executable: "/opt/a b/$BIN%tool".into(),
            arguments: vec!["/a b/$(literal)".into()],
        };
        let command = launch(&service);
        let args = command.get_args().collect::<Vec<_>>();
        assert_eq!(args[args.len() - 2], service.executable.as_os_str());
        assert_eq!(args[args.len() - 1], service.arguments[0]);
        assert!(args.contains(&std::ffi::OsStr::new("--property=Restart=no")));
        assert!(
            !args.iter().any(|a| a.to_string_lossy().contains("PartOf=")
                || a.to_string_lossy().contains("BindsTo="))
        );
    }
    fn output(code: i32, stdout: &str) -> Output {
        use std::os::unix::process::ExitStatusExt;
        Output {
            status: std::process::ExitStatus::from_raw(code << 8),
            stdout: stdout.as_bytes().into(),
            stderr: b"test diagnostic".to_vec(),
        }
    }

    fn service() -> Service {
        Service {
            name: "test.service".into(),
            identity: "owned".into(),
            executable: "/bin/true".into(),
            arguments: Vec::new(),
        }
    }

    /// A race lost to the matching invocation is recovered; foreign services are never adopted.
    /// 并发竞争失败后恢复匹配运行；绝不接管其他服务。
    #[test]
    fn competing_start_checks_identity_without_stopping_any_service() {
        use std::cell::RefCell;
        use std::collections::VecDeque;
        for (identity, success) in [("owned", true), ("foreign", false)] {
            let replies = RefCell::new(VecDeque::from([
                output(0, "LoadState=not-found\n"),
                output(1, ""),
                output(
                    0,
                    &format!("LoadState=loaded\nActiveState=activating\nDescription={identity}\n"),
                ),
            ]));
            let commands = RefCell::new(Vec::new());
            let runner = |command: Command, _: Duration| {
                commands.borrow_mut().push(
                    command
                        .get_args()
                        .map(|a| a.to_string_lossy().into_owned())
                        .collect::<Vec<_>>(),
                );
                Ok(replies.borrow_mut().pop_front().unwrap())
            };
            assert_eq!(start(&service(), &runner).is_ok(), success);
            assert!(replies.borrow().is_empty());
            assert_eq!(commands.borrow().len(), 3);
            assert!(
                !commands
                    .borrow()
                    .iter()
                    .flatten()
                    .any(|a| a == "stop" || a == "restart" || a == "kill")
            );
        }
    }

    /// Failed services reset before retry; delayed transient collection is bounded and retried.
    /// 失败服务先重置再重试；临时服务延后回收会有界重试。
    #[test]
    fn failed_unit_can_start_after_delayed_collection() {
        use std::cell::RefCell;
        use std::collections::VecDeque;
        let replies = RefCell::new(VecDeque::from([
            output(0, "LoadState=loaded\nActiveState=failed\n"),
            output(0, ""),
            output(1, ""),
            output(0, "LoadState=loaded\nActiveState=inactive\n"),
            output(0, ""),
        ]));
        let calls = RefCell::new(Vec::new());
        start(&service(), &|command: Command, _| {
            calls
                .borrow_mut()
                .push(command.get_args().next().unwrap().to_owned());
            Ok(replies.borrow_mut().pop_front().unwrap())
        })
        .unwrap();
        assert!(replies.borrow().is_empty());
        assert_eq!(calls.borrow()[1], "reset-failed");
    }

    /// Manager access failures must never be interpreted as permission to start another recorder.
    /// 管理器访问失败绝不能被解释为允许启动另一个记录器。
    #[test]
    fn query_failure_is_not_missing_service() {
        assert!(
            query("test.service", &|_, _| Ok(output(
                1,
                "LoadState=not-found\n"
            )))
            .is_err()
        );
        assert_eq!(
            query("test.service", &|_, _| Ok(output(
                4,
                "LoadState=not-found\n"
            )))
            .unwrap()
            .phase,
            Phase::Missing
        );
        assert!(query("test.service", &|_, _| Ok(output(0, "malformed"))).is_err());
    }

    /// Exercise real atomic unit creation; the short-lived fixture exits without forced cleanup.
    /// 验证真实服务原子创建；短生命周期夹具自然退出，不强制清理。
    #[test]
    #[ignore = "需要 root 和运行中的系统 systemd / requires root and a running system systemd"]
    fn real_systemd_concurrent_start() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let service = Service {
            name: format!(
                "rstarsleep-console-test-{}-{unique}.service",
                std::process::id()
            ),
            identity: "RStarSleep console supervision integration test".into(),
            executable: "/usr/bin/sleep".into(),
            arguments: vec!["10".into()],
        };
        let barrier = std::sync::Barrier::new(4);
        std::thread::scope(|scope| {
            let workers = (0..4)
                .map(|_| {
                    let barrier = &barrier;
                    let service = &service;
                    scope.spawn(move || {
                        barrier.wait();
                        Systemd.start(service).unwrap();
                        Systemd.query(&service.name).unwrap().invocation
                    })
                })
                .collect::<Vec<_>>();
            let invocations = workers
                .into_iter()
                .map(|worker| worker.join().unwrap())
                .collect::<Vec<_>>();
            assert_eq!(invocations[0].len(), 32);
            assert!(invocations.iter().all(|value| value == &invocations[0]));
        });
        let deadline = Instant::now() + Duration::from_secs(15);
        while Systemd.query(&service.name).unwrap().live() {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}
