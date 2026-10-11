//! Initialize the manager workspace through the configuration module.
//! 通过配置模块初始化管理器工作目录。

use std::{ffi::OsString, path::Path};

use crate::config::{initialize, initialize_current_system};

/// Help text describing the init subcommand's argument and storage effects.
/// 说明 init 子命令参数和存储操作的帮助文本。
const INIT_USAGE: &str = "\
用法：RStarSleep init [<Btrfs 顶层绝对路径>] [--update-presets]

参数：
  <Btrfs 顶层绝对路径>  已挂载的 Btrfs 顶层（subvolid=5）中的现有绝对目录。

选项：
  --update-presets  用当前程序内嵌的预制插件原子替换已有同名文件。
                    当前更新 plugins/arch.tar.gz，包括用户修改过的同名包。
                    保留已有配置和其他路径的插件。

省略路径时，自动识别当前系统 / 所在的 Btrfs 文件系统，定位其顶层，
即使当前系统运行于快照中也不会将快照根用作顶层。
优先复用已有可写顶层挂载；否则在 /run/rstarsleep/ 下挂载 subvolid=5，
成功后保留挂载供后续访问（重启后需重新挂载）。
在顶层目录下创建或复用 @RStarSleep 子卷，生成管理器目录及默认全局配置，
将内嵌 Arch 插件压缩包写入 plugins/。默认保留已有配置及插件文件。
需要已安装 btrfs 工具，并具备创建子卷和写入文件的权限。
自动识别还需要 findmnt，自动挂载需要 mount 及挂载权限。

示例：
  RStarSleep init
  RStarSleep init /mnt/btrfs
  RStarSleep init --update-presets
  RStarSleep init /mnt/btrfs --update-presets
  RStarSleep init --help";

/// Handle init arguments and invoke the configuration module's workspace initializer.
/// 处理 init 的参数，并调用配置模块的工作目录初始化接口。
///
/// # Arguments
///
/// - `arguments`: Native arguments after `init`; an optional absolute Btrfs directory
///   and `--update-presets`, or a help flag. Omitting the path selects the current filesystem.
///   `init` 之后的原生参数；可选 Btrfs 绝对目录及 `--update-presets`，或帮助选项。
///   省略路径时自动识别当前文件系统。
///
/// # Errors
///
/// Returns an error for invalid arguments, or propagates initialization/update failures.
/// 参数无效时返回错误，并传递初始化或更新操作中的失败原因。
pub(super) fn run(arguments: &[OsString]) -> super::Result<()> {
    if arguments == ["--help"] || arguments == ["-h"] {
        println!("{INIT_USAGE}");
        return Ok(());
    }
    let mut top_level = None;
    let mut update_presets = false;
    for argument in arguments {
        if argument == "--update-presets" {
            if update_presets {
                return Err(format!("不能重复指定 --update-presets。\n\n{INIT_USAGE}").into());
            }
            update_presets = true;
        } else if argument.as_encoded_bytes().starts_with(b"-") {
            return Err(format!(
                "未知 init 选项：{}\n\n{INIT_USAGE}",
                argument.to_string_lossy()
            )
            .into());
        } else if top_level.replace(Path::new(argument)).is_some() {
            return Err(format!("init 只接受一个 Btrfs 顶层目录参数。\n\n{INIT_USAGE}").into());
        }
    }
    let store = match top_level {
        Some(path) => initialize(path)?,
        None => initialize_current_system()?,
    };
    if update_presets {
        store.update_presets()?;
        println!("预制插件已更新：{}", store.plugins_path().display());
    }
    println!("工作目录已初始化：{}", store.root().display());
    Ok(())
}
