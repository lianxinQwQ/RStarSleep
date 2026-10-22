//! Process entry point delegating command handling to the CLI module.
//! 将命令处理委托给 CLI 模块的进程入口。

/// Report failures on stderr and return a nonzero exit code.
/// 将失败原因输出到标准错误，并返回非零退出码。
///
/// # Arguments
///
/// No parameters.
/// 无参数。
fn main() -> std::process::ExitCode {
    match rstarsleep::cli::run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            if let Some(rstarsleep::subsystem::Error::Exit(code)) =
                error.downcast_ref::<rstarsleep::subsystem::Error>()
            {
                return std::process::ExitCode::from((*code).clamp(0, 255) as u8);
            }
            eprintln!("操作失败 / Operation failed: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}
