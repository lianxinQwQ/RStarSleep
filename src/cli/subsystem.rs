//! Subsystem lifecycle, literal execution and user application integration.
//! 子系统生命周期、字面执行及用户应用集成。
use crate::{
    config::{ConfigStore, DEFAULT_MOUNT_POINT},
    subsystem::{
        self, Error, client,
        protocol::{Operation, Status},
    },
};
use std::{collections::BTreeMap, ffi::OsString, path::PathBuf};

const USAGE: &str = "用法：RStarSleep subsystem list|status [ID]|start ID|stop ID|restart ID|shell ID|exec [--pty] ID -- COMMAND [ARG...]|open [--wait] ID TARGET [-- ARG...]|run-desktop ID PATH [-- ARG...]|sync-exports [ID] [--root PATH] / Usage: RStarSleep subsystem list|status [ID]|start ID|stop ID|restart ID|shell ID|exec [--pty] ID -- COMMAND [ARG...]|open [--wait] ID TARGET [-- ARG...]|run-desktop ID PATH [-- ARG...]|sync-exports [ID] [--root PATH]";

/// Extract fixed manager options before the application's delimiter.
/// 在应用参数分隔符前提取固定管理器选项。
/// # Arguments
/// - `arguments`: Native command-group arguments. 原生命令组参数。
pub(super) fn parse(arguments: &[OsString]) -> super::Result<(ConfigStore, Vec<String>)> {
    let mut root = None;
    let mut rest = Vec::new();
    let mut iter = arguments.iter();
    let mut application = false;
    while let Some(value) = iter.next() {
        if !application && value == "--root" {
            if root.is_some() {
                return Err("--root 只能指定一次 / --root may only be specified once".into());
            }
            root = Some(PathBuf::from(
                iter.next()
                    .ok_or("--root 缺少路径 / --root is missing a path")?,
            ));
        } else {
            let text = value
                .to_str()
                .ok_or("子系统命令参数必须为 UTF-8 / subsystem command arguments must be UTF-8")?;
            if text == "--" {
                application = true;
            }
            rest.push(text.to_owned());
        }
    }
    Ok((
        ConfigStore::new(root.unwrap_or_else(|| DEFAULT_MOUNT_POINT.into()))?,
        rest,
    ))
}
/// Capture only allowlisted caller session variables.
/// 只获取允许列表中的调用者会话变量。
/// # Arguments
/// No parameters. 无参数。
fn environment() -> BTreeMap<String, String> {
    std::env::vars()
        .filter(|(key, _)| {
            matches!(
                key.as_str(),
                "XDG_RUNTIME_DIR"
                    | "DISPLAY"
                    | "WAYLAND_DISPLAY"
                    | "XAUTHORITY"
                    | "DBUS_SESSION_BUS_ADDRESS"
                    | "PULSE_SERVER"
                    | "PIPEWIRE_REMOTE"
                    | "LANG"
                    | "TERM"
                    | "COLORTERM"
                    | "GTK_IM_MODULE"
                    | "QT_IM_MODULE"
                    | "XMODIFIERS"
            ) || key.starts_with("LC_")
        })
        .collect()
}
/// Print machine records without introducing a version-selection interface.
/// 输出机器记录，不引入版本选择接口。
/// # Arguments
/// - `statuses`: Authorized machine records. 已授权机器记录。
fn statuses(statuses: &[Status]) {
    for status in statuses {
        println!(
            "{}\t{}\tmachine={}\torigin={}\tleases={}",
            status.id,
            if status.running { "running" } else { "stopped" },
            status.machine.as_deref().unwrap_or("-"),
            match status.origin {
                Some(subsystem::protocol::StartOrigin::Explicit) => "explicit",
                Some(subsystem::protocol::StartOrigin::Autostart) => "autostart",
                Some(subsystem::protocol::StartOrigin::Demand) => "demand",
                None => "-",
            },
            status.leases
        );
    }
}
/// Return an application exit status through the common CLI error boundary.
/// 通过共用 CLI 错误边界返回应用退出状态。
/// # Arguments
/// - `code`: Actual application status. 实际应用状态。
fn exited(code: i32) -> super::Result<()> {
    if code == 0 {
        Ok(())
    } else {
        Err(Box::new(Error::Exit(code)))
    }
}
/// Execute the selected subsystem operation.
/// 执行所选子系统操作。
/// # Arguments
/// - `arguments`: Arguments after subsystem. subsystem 后参数。
/// # Errors
/// Returns protocol, authorization or actual application-exit errors.
/// 返回协议、授权或实际应用退出错误。
pub(super) fn run(arguments: &[OsString]) -> super::Result<()> {
    if arguments == [OsString::from("--help")] || arguments == [OsString::from("-h")] {
        println!("{USAGE}");
        return Ok(());
    }
    let (store, args) = parse(arguments)?;
    let socket = client::socket_for_root(store.root())?;
    let command = args.first().map(String::as_str).ok_or(USAGE)?;
    let rest = &args[1..];
    match (command, rest) {
        ("list", []) | ("status", []) => statuses(&client::request(&socket, Operation::List)?),
        (command @ ("status" | "start" | "stop" | "restart"), [id]) => {
            let id = id.clone();
            let op = match command {
                "status" => Operation::Status { id },
                "start" => Operation::Start { id },
                "stop" => Operation::Stop { id },
                _ => Operation::Restart { id },
            };
            statuses(&client::request(&socket, op)?);
        }
        ("shell", [id]) => exited(client::execute(
            &socket,
            id,
            vec!["/bin/sh".into(), "-l".into()],
            environment(),
            true,
        )?)?,
        ("exec", rest) => {
            let (pty, rest) = if rest.first().is_some_and(|v| v == "--pty") {
                (true, &rest[1..])
            } else {
                (false, rest)
            };
            if rest.len() < 3 || rest[1] != "--" {
                return Err(USAGE.into());
            }
            subsystem::protocol::validate_id(&rest[0])?;
            exited(client::execute(
                &socket,
                &rest[0],
                rest[2..].to_vec(),
                environment(),
                pty,
            )?)?;
        }
        (command @ ("open" | "run-desktop"), rest) => {
            let (wait, rest) = if rest.first().is_some_and(|v| v == "--wait") {
                (true, &rest[1..])
            } else {
                (false, rest)
            };
            if rest.len() < 2 {
                return Err(USAGE.into());
            }
            subsystem::protocol::validate_id(&rest[0])?;
            let launch_args = if rest.get(2).is_some_and(|v| v == "--") {
                rest[3..].to_vec()
            } else {
                rest[2..].to_vec()
            };
            let pty = command == "run-desktop" && unsafe { libc::isatty(0) } == 1;
            exited(client::execute_operation(
                &socket,
                Operation::Open {
                    id: rest[0].clone(),
                    target: rest[1].clone(),
                    args: launch_args,
                    environment: environment(),
                    desktop: command == "run-desktop",
                    detach: !wait,
                },
                pty,
            )?)?;
        }
        ("sync-exports", []) => {
            for status in client::request(&socket, Operation::List)? {
                client::request(&socket, Operation::SyncExports { id: status.id })?;
            }
        }
        ("sync-exports", [id]) => {
            client::request(&socket, Operation::SyncExports { id: id.clone() })?;
        }
        _ => return Err(USAGE.into()),
    }
    Ok(())
}

/// Run or install a daemon for the same fixed manager selection syntax.
/// 以相同固定管理器选择语法运行或安装守护进程。
/// # Arguments
/// - `arguments`: Arguments after daemon. daemon 后参数。
pub(super) fn daemon(arguments: &[OsString]) -> super::Result<()> {
    let (store, args) = parse(arguments)?;
    match args.as_slice() {
        [command] if command == "run" => subsystem::daemon::run(store)?,
        [command] if command == "install" => subsystem::daemon::install(&store)?,
        [command] if command == "--help" || command == "-h" => {
            println!("用法：RStarSleep daemon install|run [--root PATH] / Usage: RStarSleep daemon install|run [--root PATH]")
        }
        _ => return Err("用法：RStarSleep daemon install|run [--root PATH] / Usage: RStarSleep daemon install|run [--root PATH]".into()),
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    /// Manager flags after the delimiter are passed literally to the application.
    /// 分隔符后的管理器标志原样传给应用。
    /// # Arguments
    /// No parameters. 无参数。
    #[test]
    fn preserves_application_options() {
        let (store, args) = parse(
            &[
                "exec", "--root", "/manager", "apps", "--", "app", "--root", "/guest",
            ]
            .map(OsString::from),
        )
        .unwrap();
        assert_eq!(store.root(), std::path::Path::new("/manager"));
        assert_eq!(args, ["exec", "apps", "--", "app", "--root", "/guest"]);
        assert!(parse(&["--root", "/a", "--root", "/b"].map(OsString::from)).is_err());
    }
}
