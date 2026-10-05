//! Initialize the directory layout and templates for one managed system.
//! 初始化一个被管理系统的目录布局和配置模板。

use std::{ffi::OsString, path::PathBuf};

use crate::config::ConfigStore;

/// Validate arguments and initialize a system within an existing workspace.
/// 校验参数，并在已有工作目录内初始化一个系统。
///
/// # Arguments
///
/// - `arguments`: Arguments after `init-system`: the absolute workspace path followed
///   by a stable UTF-8 system identifier.
///   `init-system` 之后的参数：工作目录绝对路径，以及稳定的 UTF-8 系统标识。
///
/// # Errors
///
/// Returns an error for invalid arguments or propagates configuration initialization failures.
/// 参数无效时返回错误，并传递配置初始化过程中的错误。
pub(super) fn run(arguments: &[OsString]) -> super::Result<()> {
    let [root, id] = arguments else {
        return Err(super::USAGE.into());
    };
    let id = id.to_str().ok_or("系统标识必须是 UTF-8 文本")?;
    let store = ConfigStore::new(PathBuf::from(root))?;
    let layout = store.initialize_system(id)?;
    println!("系统目录已初始化：{}", layout.root.display());
    Ok(())
}
