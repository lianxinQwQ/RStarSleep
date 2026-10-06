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
        ])
        .arg("--directory")
        .arg(root);
    command
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
            "nspawn 绑定源路径不能包含冒号或反斜杠: {}",
            source.display()
        )));
    }
    let mut argument = OsString::from(option);
    argument.push(source);
    argument.push(":");
    argument.push(destination);
    Ok(argument)
}
