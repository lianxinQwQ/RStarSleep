//! Construct container mounts and environment before the command delimiter.
//! 在命令分隔符之前构造容器挂载及环境。

use super::{Error, LoadedPlugin, Result};
use std::{
    ffi::{OsStr, OsString},
    path::Path,
    process::Command,
};

/// Container mount point for the complete plugin environment.
/// 完整插件环境在容器内的挂载点。
pub(super) const SYSTEM_PLUGIN: &str = "/run/rstarsleep/plugin";
/// Writable scratch directory independent of read-only plugin resources.
/// 独立于只读插件资源的可写临时目录。
const SYSTEM_TMP: &str = "/run/rstarsleep/plugin-tmp";
/// Read-only mount point for the frozen configuration archive.
/// 冻结配置留档的只读挂载点。
const SYSTEM_CONFIG: &str = "/run/rstarsleep/config";

/// Configure nspawn with the complete plugin and separate writable scratch space.
/// 配置 nspawn，提供完整插件和独立可写临时空间。
///
/// # Arguments
///
/// - `plugin`: Loaded resources and scratch space to bind into the container.
///   待绑定进容器的已加载资源和临时空间。
/// - `launcher`: Resolved host systemd-nspawn executable.
///   已定位的宿主 systemd-nspawn 可执行文件。
/// - `root`: Target-system root directory.
///   目标系统根目录。
/// - `host_path`: Host PATH for launching nspawn and its host-side dependencies.
///   启动 nspawn 及其宿主依赖所用的 PATH。
/// - `architecture`: Target architecture exposed to plugin operations.
///   向插件操作提供的目标架构。
/// - `context`: Optional frozen inputs.
///   可选冻结输入。
#[cfg(test)]
pub(super) fn nspawn(
    plugin: &LoadedPlugin,
    launcher: &Path,
    root: &Path,
    host_path: &OsStr,
    architecture: &str,
    context: Option<&crate::controller::BuildContext>,
) -> Result<Command> {
    nspawn_from(
        plugin,
        launcher,
        root,
        host_path,
        context,
        plugin.root(),
        &plugin.configuration_environment(
            context
                .map(|value| &value.parameters)
                .unwrap_or(&std::collections::BTreeMap::from([(
                    "RSTAR_DISTRIBUTION_ARCHITECTURE".into(),
                    architecture.into(),
                )])),
            architecture,
        )?,
    )
}

pub(super) fn nspawn_from(
    plugin: &LoadedPlugin,
    launcher: &Path,
    root: &Path,
    host_path: &OsStr,
    context: Option<&crate::controller::BuildContext>,
    resources: &Path,
    configuration: &std::collections::BTreeMap<String, String>,
) -> Result<Command> {
    let mut command = crate::controller::container::command(launcher, root);
    command
        .current_dir(resources)
        .env("PATH", host_path)
        .arg(bind("--bind-ro=", resources, SYSTEM_PLUGIN)?)
        .arg(bind("--bind=", &plugin.temporary_path(), SYSTEM_TMP)?)
        .arg(format!("--chdir={SYSTEM_PLUGIN}"))
        .arg(format!("--setenv=RSTAR_PLUGIN_ROOT={SYSTEM_PLUGIN}"))
        .arg(format!("--setenv=RSTAR_PLUGIN_TMP={SYSTEM_TMP}"))
        .arg("--setenv=RSTAR_ROOT=/");
    for (name, value) in configuration {
        command.arg(format!("--setenv={name}={value}"));
    }
    if let Some(context) = context {
        for mount in &context.persistent_mounts {
            command.arg(bind(
                "--bind=",
                &mount.source,
                &format!("{SYSTEM_PLUGIN}/{}", mount.target.display()),
            )?);
        }
        command
            .arg(bind("--bind-ro=", &context.config, SYSTEM_CONFIG)?)
            .arg("--setenv=RSTAR_CONFIG_DIR=/run/rstarsleep/config")
            .arg(format!("--setenv=RSTAR_SYSTEM_ID={}", context.system_id));
    }
    Ok(command)
}

/// Encode a mount argument using native path bytes without ambiguous field separators.
/// 使用原生路径字节编码挂载参数，禁止有歧义的字段分隔符。
///
/// # Arguments
///
/// - `option`: Bind option prefix including its equals sign.
///   包括等号的绑定选项前缀。
/// - `source`: Absolute host path; colons and backslashes are unsupported.
///   宿主绝对路径；不支持冒号和反斜杠。
/// - `destination`: Fixed absolute guest mount point without field separators.
///   不含字段分隔符的固定容器绝对挂载点。
fn bind(option: &str, source: &Path, destination: &str) -> Result<OsString> {
    crate::controller::container::bind(option, source, destination)
        .map_err(|e| Error::Invalid(e.to_string()))
}
