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

const USAGE: &str = "用法 / Usage: RStarSleep subsystem <COMMAND> [ARGS...] [--root PATH]
运行 RStarSleep subsystem --help 查看命令与示例。
Run RStarSleep subsystem --help for commands and examples.";

const HELP: &str = "子系统管理与应用运行 / Subsystem management and application execution

用法 / Usage:
  RStarSleep subsystem <COMMAND> [ARGS...] [--root PATH]

状态查询 / Status:
  list                              列出有权访问的子系统 / List accessible subsystems
  status [ID]                       查看指定或全部子系统状态 / Show one or all statuses

生命周期（需要 root） / Lifecycle (requires root):
  start ID                          显式启动子系统 / Start a subsystem explicitly
  stop ID                           正常关闭子系统 / Shut down a subsystem gracefully
  restart ID                        重启并使用当前成功构建 / Restart with the current build

终端与应用 / Terminals and applications:
  shell ID
    以调用者身份打开交互式 /bin/sh -l，分配伪终端。
    Open an interactive /bin/sh -l as the caller with a pseudo-terminal.
  exec [--pty] ID -- COMMAND [ARG...]
    按字面参数执行命令并等待退出；--pty 分配伪终端。
    Execute literal arguments and wait for exit; --pty allocates a pseudo-terminal.
  open [--wait] ID TARGET [-- ARG...]
    启动程序名、客体可执行文件绝对路径或 .desktop 入口。
    Launch a program name, absolute guest executable path, or .desktop entry.
  run-desktop [--wait] ID PATH [-- ARG...]
    运行客体中的 .desktop 文件；文件名相对于 /usr/share/applications 解析。
    Run a guest .desktop file; filenames resolve under /usr/share/applications.

用户入口 / User exports:
  sync-exports [ID]
    为当前用户同步命令和桌面入口；省略 ID 时同步全部有权访问的子系统。
    Sync command and desktop entries for the caller; omit ID for all accessible subsystems.

选项与参数 / Options and arguments:
  --root PATH                       管理根绝对路径，默认 /rstarsleep。
                                    Absolute management root; default: /rstarsleep.
  -h, --help                        显示此帮助 / Show this help
  ID                                全局配置中 subsystems 登记的系统标识。
                                    System ID registered in the global subsystems list.
  --                                结束管理器参数，后续参数原样传给应用。
                                    End manager arguments; pass the rest to the application.

说明 / Notes:
  --root 可用于所有命令，必须放在 -- 之前；--pty、--wait 放在 ID 之前。
  --root applies to every command and must precede --; put --pty and --wait before ID.
  open 和 run-desktop 默认后台运行；--wait 等待退出，Terminal=true 的入口始终等待。
  open and run-desktop detach by default; --wait or Terminal=true waits for exit.
  shell、exec、open、run-desktop 会按需启动子系统；需要已运行的 daemon。
  shell, exec, open and run-desktop start subsystems on demand; a running daemon is required.
  普通用户须在 allowed_uids 中获授权；应用和用户入口应以目标用户身份运行。
  Non-root callers need allowed_uids authorization; run apps and exports as the intended user.
  exec 不解析 shell 语法；需要管道或重定向时，请显式执行 /bin/sh -c。
  exec does not interpret shell syntax; invoke /bin/sh -c explicitly for pipes or redirection.

示例（apps 为子系统标识） / Examples (apps is a subsystem ID):
  RStarSleep subsystem list
  RStarSleep subsystem status apps --root /rstarsleep
  sudo RStarSleep subsystem start apps
  RStarSleep subsystem shell apps
  RStarSleep subsystem exec --pty apps -- /usr/bin/top
  RStarSleep subsystem open --wait apps firefox -- https://example.org
  RStarSleep subsystem run-desktop apps /usr/share/applications/firefox.desktop
  RStarSleep subsystem sync-exports apps";

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
        println!("{HELP}");
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
