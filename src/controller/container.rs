//! Shared nspawn isolation flags and native bind argument encoding.
//! 共用 nspawn 隔离参数及原生绑定参数编码。
use super::{Error, Result};
use std::{ffi::OsString, path::Path, process::Command};

/// Configure a non-booting container against an existing system root.
/// 针对已有系统根配置非启动模式容器。
/// # Arguments
/// - `launcher`: Host nspawn executable.
///   宿主 nspawn 可执行文件。
/// - `root`: Writable target system root.
///   可写目标系统根。
pub(super) fn command(launcher: &Path, root: &Path) -> Command {
    let mut command = Command::new(launcher);
    command
        .args([
            "--quiet",
            "--pipe",
            "--settings=no",
            "--register=no",
            "--as-pid2",
            "--link-journal=no",
            // Allow GPG to lock its secure memory pool within a bounded quota.
            // 允许 GPG 在有限额度内锁定安全内存池。
            "--system-call-filter=@memlock",
            "--rlimit=RLIMIT_MEMLOCK=8388608:8388608",
        ])
        .arg("--directory")
        .arg(root);
    command
}

/// Apply resolved host variables without interpreting their native values.
/// 应用已解析的宿主变量，不解释原生值。
/// # Arguments
/// - `command`: Host process being configured. 正在配置的宿主进程。
/// - `environment`: Frozen variable assignments or removals. 冻结的变量赋值或移除请求。
pub(super) fn host_environment(command: &mut Command, environment: &crate::config::Environment) {
    for (name, value) in environment {
        if let Some(value) = value {
            command.env(name, value);
        } else {
            command.env_remove(name);
        }
    }
}

/// Forward explicit guest variables before nspawn's command delimiter.
/// 在 nspawn 命令分隔符之前透传显式容器变量。
/// # Arguments
/// - `command`: nspawn invocation before its command delimiter. 尚未加入命令分隔符的 nspawn 调用。
/// - `environment`: Frozen variable assignments or removals. 冻结的变量赋值或移除请求。
pub(super) fn environment(command: &mut Command, environment: &crate::config::Environment) {
    host_environment(command, environment);
    for (name, value) in environment {
        if let Some(value) = value {
            let mut argument = OsString::from(format!("--setenv={name}="));
            argument.push(value);
            command.arg(argument);
        }
    }
}

/// Encode a bind option without interpreting native path bytes as field separators.
/// 编码绑定选项，不将原生路径字节解释为字段分隔符。
/// # Arguments
/// - `option`: Bind prefix including equals sign.
///   含等号的绑定前缀。
/// - `source`: Host source path.
///   宿主源路径。
/// - `destination`: Fixed guest path.
///   固定容器路径。
pub(super) fn bind(option: &str, source: &Path, destination: &str) -> Result<OsString> {
    if source
        .as_os_str()
        .as_encoded_bytes()
        .iter()
        .any(|byte| b":\\".contains(byte))
    {
        return Err(Error::Invalid(format!(
            "nspawn 绑定源路径不能包含冒号或反斜杠: {0} / nspawn bind source path must not contain colons or backslashes: {0}",
            source.display()
        )));
    }
    let mut argument = OsString::from(option);
    argument.push(source);
    argument.push(":");
    argument.push(destination);
    Ok(argument)
}
