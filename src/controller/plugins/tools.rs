//! Validate tool availability without executing plugin code.
//! 不执行插件代码，校验工具可用性。

use super::{Error, LoadedPlugin, Result};
use std::{
    ffi::OsStr,
    fs,
    path::{Path, PathBuf},
    process::Output,
};

/// Fixed POSIX probe returning only missing executables and never running plugin code.
/// 固定 POSIX 探测脚本；仅返回缺失工具，不运行插件代码。
pub(super) const SYSTEM_PROBE: &str = r#"
for required_tool do
    available=false
    case "$required_tool" in
        */*)
            if [ -f "$required_tool" ] && [ -x "$required_tool" ]; then available=true; fi
            ;;
        *)
            remaining_path=${PATH-}
            while :; do
                directory=${remaining_path%%:*}
                [ -n "$directory" ] || directory=.
                if [ -f "$directory/$required_tool" ] && [ -x "$directory/$required_tool" ]; then
                    available=true
                    break
                fi
                case "$remaining_path" in
                    *:*) remaining_path=${remaining_path#*:} ;;
                    *) break ;;
                esac
            done
            ;;
    esac
    if [ "$available" = false ]; then printf '%s\n' "$required_tool"; fi
done
"#;

/// Deduplicate declared requirements and include the executor's mandatory tools.
/// 去重声明的需求，并加入执行器必需的工具。
///
/// # Arguments
///
/// - `declared`: Executables explicitly required by the plugin.
///   插件显式要求的可执行工具。
/// - `mandatory`: Executables needed to invoke the plugin itself.
///   调用插件本身所需的可执行工具。
pub(super) fn required_tools(declared: &[String], mandatory: &[&str]) -> Vec<String> {
    let mut result: Vec<String> = mandatory.iter().map(|tool| (*tool).to_owned()).collect();
    for tool in declared {
        if !result.contains(tool) {
            result.push(tool.clone());
        }
    }
    result
}

/// Check host executables without launching any plugin script or dependency.
/// 不启动插件脚本或依赖程序，检查宿主可执行工具。
///
/// # Arguments
///
/// - `plugin`: Plugin whose identity is included in an error.
///   标识将包含在错误中的插件。
/// - `operation`: Task blocked when requirements are unavailable.
///   需求不可用时被阻止的任务。
/// - `tools`: Executables to check in the host environment.
///   需要在宿主环境检查的可执行工具。
/// - `path`: Search path identical to the one used for execution.
///   与实际执行相同的搜索路径。
pub(super) fn check_host(
    plugin: &LoadedPlugin,
    operation: &str,
    tools: &[String],
    path: &OsStr,
) -> Result<()> {
    let missing = tools
        .iter()
        .filter(|tool| executable(tool, plugin.root(), path).is_none())
        .cloned()
        .collect::<Vec<_>>();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(missing_tools(plugin, operation, "宿主", missing))
    }
}

/// Resolve a real executable using the task's working directory and PATH.
/// 使用任务工作目录和 PATH 定位真实可执行文件。
///
/// # Arguments
///
/// - `tool`: Bare executable name or plugin-relative resource path.
///   可执行工具名或插件资源相对路径。
/// - `directory`: Plugin working directory used for relative paths and empty PATH entries.
///   用于相对路径和空 PATH 项的插件工作目录。
/// - `path`: Executable search path.
///   可执行文件搜索路径。
pub(super) fn executable(tool: &str, directory: &Path, path: &OsStr) -> Option<PathBuf> {
    let candidates = if tool.contains('/') {
        vec![directory.join(tool)]
    } else {
        std::env::split_paths(path)
            .map(|item| {
                if item.is_absolute() {
                    item.join(tool)
                } else {
                    directory.join(item).join(tool)
                }
            })
            .collect()
    };
    candidates.into_iter().find(|candidate| {
        let Ok(metadata) = fs::metadata(candidate) else {
            return false;
        };
        if !metadata.is_file() {
            return false;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            metadata.permissions().mode() & 0o111 != 0
        }
        #[cfg(not(unix))]
        {
            true
        }
    })
}

/// Construct a dependency error before a plugin can perform side effects.
/// 在插件执行副作用之前生成依赖错误。
///
/// # Arguments
///
/// - `plugin`: Plugin identity included in the error.
///   错误中包含其标识的插件。
/// - `operation`: Task prevented from running.
///   被阻止运行的任务。
/// - `environment`: Host or target environment checked.
///   被检查的宿主或目标环境。
/// - `tools`: Missing executables to report together.
///   一并报告的缺失可执行工具。
pub(super) fn missing_tools(
    plugin: &LoadedPlugin,
    operation: &str,
    environment: &str,
    tools: Vec<String>,
) -> Error {
    Error::MissingTools {
        plugin: plugin.metadata.id.clone(),
        operation: operation.into(),
        environment: environment.into(),
        tools,
    }
}

/// Preserve a failed probe or operation's status and diagnostics.
/// 保留失败探测或操作的退出状态和诊断信息。
///
/// # Arguments
///
/// - `plugin`: Plugin owning the task.
///   任务所属的插件。
/// - `operation`: Failed task or dependency probe label.
///   失败任务或依赖探测的标签。
/// - `output`: Captured process output and exit status.
///   捕获的进程输出和退出状态。
pub(super) fn execution_error(plugin: &LoadedPlugin, operation: &str, output: &Output) -> Error {
    Error::Execution {
        plugin: plugin.metadata.id.clone(),
        operation: operation.into(),
        status: output.status.to_string(),
        stdout: String::from_utf8_lossy(&output.stdout).trim().into(),
        stderr: String::from_utf8_lossy(&output.stderr).trim().into(),
    }
}
