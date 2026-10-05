//! Display the package list from a managed system's current build configuration.
//! 显示一个被管理系统当前构建配置中的软件包列表。

use std::{ffi::OsString, path::PathBuf};

use crate::config::ConfigStore;

/// Validate arguments and print each configured package on a separate line.
/// 校验参数，并逐行输出配置中的各个软件包。
///
/// # Arguments
///
/// - `arguments`: Arguments after `packages`: the absolute workspace path followed
///   by the UTF-8 identifier of the managed system whose configuration is read.
///   `packages` 之后的参数：工作目录绝对路径，以及需要读取配置的被管理系统的 UTF-8 标识。
///
/// # Errors
///
/// Returns an error for invalid arguments or propagates package configuration read failures.
/// 参数无效时返回错误，并传递软件包配置读取过程中的错误。
pub(super) fn run(arguments: &[OsString]) -> super::Result<()> {
    let [root, id] = arguments else {
        return Err(super::USAGE.into());
    };
    let id = id.to_str().ok_or("系统标识必须是 UTF-8 文本")?;
    let store = ConfigStore::new(PathBuf::from(root))?;
    for package in store.read_packages(id)?.packages {
        println!("{package}");
    }
    Ok(())
}
