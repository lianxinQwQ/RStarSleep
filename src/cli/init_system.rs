//! Initialize the directory layout and templates for one managed system.
//! 初始化一个被管理系统的目录布局和配置模板。

use std::{ffi::OsString, path::PathBuf};

use crate::config::{ConfigStore, current_system_workspace};

const INIT_SYSTEM_USAGE: &str = "\
用法：RStarSleep init-system [<工作目录绝对路径>] <系统标识>

省略工作目录时，自动定位当前系统所在 Btrfs 文件系统顶层的 @RStarSleep 工作目录，
兼容已有 @rstarsleep；即使当前系统运行于快照中也能识别。
请先运行 RStarSleep init 初始化管理器工作目录。

示例：
  RStarSleep init-system desktop
  RStarSleep init-system /rstarsleep desktop";

/// Validate arguments and initialize a system within an existing workspace.
/// 校验参数，并在已有工作目录内初始化一个系统。
///
/// # Arguments
///
/// - `arguments`: Optional workspace path followed by a stable UTF-8 system identifier.
///   `init-system` 之后的参数：可选的工作目录绝对路径，以及稳定的 UTF-8 系统标识。
///
/// # Errors
///
/// Returns an error for invalid arguments or propagates configuration initialization failures.
/// 参数无效时返回错误，并传递配置初始化过程中的错误。
pub(super) fn run(arguments: &[OsString]) -> super::Result<()> {
    let (root, id) = match arguments {
        [help] if help == "--help" || help == "-h" => {
            println!("{INIT_SYSTEM_USAGE}");
            return Ok(());
        }
        [id] => (None, id),
        [root, id] => (Some(root), id),
        _ => return Err(INIT_SYSTEM_USAGE.into()),
    };
    let id = id.to_str().ok_or("系统标识必须是 UTF-8 文本")?;
    crate::config::validate_identifier(id)?;
    let store = match root {
        Some(root) => ConfigStore::new(PathBuf::from(root))?,
        None => current_system_workspace()?,
    };
    let layout = store.initialize_system(id)?;
    println!("系统目录已初始化：{}", layout.root.display());
    Ok(())
}
