//! Initialize the manager workspace through the configuration module.
//! 通过配置模块初始化管理器工作目录。

use std::{ffi::OsString, path::Path};

use crate::config::{initialize, initialize_current_system};

/// Help text describing the init subcommand's argument and storage effects.
/// 说明 init 子命令参数和存储操作的帮助文本。
const INIT_USAGE: &str = "\
用法：RStarSleep init [<Btrfs 顶层绝对路径>]

参数：
  <Btrfs 顶层绝对路径>  已挂载的 Btrfs 顶层（subvolid=5）中的现有绝对目录。

省略路径时，自动识别当前系统 / 所在的 Btrfs 文件系统，定位其顶层，
即使当前系统运行于快照中也不会将快照根用作顶层。
优先复用已有可写顶层挂载；否则在 /run/rstarsleep/ 下挂载 subvolid=5，
成功后保留挂载供后续访问（重启后需重新挂载）。
在顶层目录下创建或复用 @RStarSleep 子卷，生成管理器目录及默认全局配置，
将内嵌 Arch 插件压缩包写入 plugins/。保留已有配置及插件文件。
需要已安装 btrfs 工具，并具备创建子卷和写入文件的权限。
自动识别还需要 findmnt，自动挂载需要 mount 及挂载权限。

示例：
  RStarSleep init
  RStarSleep init /mnt/btrfs
  RStarSleep init --help";

/// Handle init arguments and invoke the configuration module's workspace initializer.
/// 处理 init 的参数，并调用配置模块的工作目录初始化接口。
///
/// # Arguments
///
/// - `arguments`: Native command-line arguments after `init`; one absolute Btrfs directory
///   or a help flag is accepted; no argument selects the current system's filesystem.
///   `init` 之后的原生命令行参数；接受一个 Btrfs 绝对目录或一个帮助选项，省略时自动识别。
///
/// # Errors
///
/// Returns an error for extra arguments, or propagates initialization failures.
/// 参数过多时返回错误，并传递初始化操作中的失败原因。
pub(super) fn run(arguments: &[OsString]) -> super::Result<()> {
    match arguments {
        [help] if help == "--help" || help == "-h" => println!("{INIT_USAGE}"),
        [top_level] => {
            let store = initialize(Path::new(top_level))?;
            println!("工作目录已初始化：{}", store.root().display());
        }
        [] => {
            let store = initialize_current_system()?;
            println!("工作目录已初始化：{}", store.root().display());
        }
        _ => return Err(format!("init 只接受一个 Btrfs 顶层目录参数。\n\n{INIT_USAGE}").into()),
    }
    Ok(())
}
