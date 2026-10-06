//! Dispatch command-line requests to independent subcommand modules.
//! 将命令行请求分派到各自独立的子命令模块。

mod build;
mod init;
mod init_system;
mod packages;

use std::ffi::OsString;

/// Result type used to propagate command failures to the process entry point.
/// 用于将命令执行错误传递给进程入口的结果类型。
pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// Usage text shared by help output and invalid-command errors.
/// 帮助输出与无效命令错误共用的用法说明。
const USAGE: &str = "\
用法：
  RStarSleep init <Btrfs 顶层绝对路径>
  RStarSleep init-system <工作目录绝对路径> <系统标识>
  RStarSleep packages <工作目录绝对路径> <系统标识>
  RStarSleep build [<系统标识> | --all] [--root <管理根绝对路径>]

init 创建 @rstarsleep 子卷；工作目录可使用该子卷路径或其挂载点 /rstarsleep。";

/// Read process arguments and delegate command handling to the dispatcher.
/// 读取进程参数，并将命令处理委托给分派函数。
///
/// # Arguments
///
/// No parameters; command-line arguments are read from the current process.
/// 无参数；命令行参数从当前进程读取。
///
/// # Errors
///
/// Propagates command selection, argument validation, and execution failures.
/// 传递命令选择、参数校验和执行过程中的错误。
pub fn run() -> Result<()> {
    // Preserve native filesystem paths; individual commands validate text identifiers.
    // 保留原生文件系统路径，文本标识由各子命令自行校验。
    let arguments: Vec<_> = std::env::args_os().skip(1).collect();
    dispatch(&arguments)
}

/// Select a subcommand without implementing its storage or configuration operations.
/// 选择子命令，不在此处实现子命令的存储或配置操作。
///
/// # Arguments
///
/// - `arguments`: Native command-line arguments excluding the executable name;
///   the first argument selects a command and the remaining arguments are passed to it.
///   不含可执行程序名称的原生命令行参数；首个参数选择子命令，其余参数传递给该命令。
///
/// # Errors
///
/// Returns an error for an unknown command or propagates the selected command's failure.
/// 命令未知时返回错误，并传递选定子命令的执行错误。
fn dispatch(arguments: &[OsString]) -> Result<()> {
    match arguments {
        [] => println!("{USAGE}"),
        [help] if help == "--help" || help == "-h" => println!("{USAGE}"),
        [command, rest @ ..] if command == "init" => init::run(rest)?,
        [command, rest @ ..] if command == "init-system" => init_system::run(rest)?,
        [command, rest @ ..] if command == "packages" => packages::run(rest)?,
        [command, rest @ ..] if command == "build" => build::run(rest)?,
        _ => return Err(USAGE.into()),
    }
    Ok(())
}
