//! Check declared tools before invoking Bash plugins in their actual execution environment.
//! 在实际执行环境检查声明的工具，再调用 Bash 插件。

use std::{
    ffi::{OsStr, OsString},
    path::Path,
    process::{Command, Output},
};

use super::container::{SYSTEM_PLUGIN, nspawn};
use super::tools::{
    SYSTEM_PROBE, check_host, executable, execution_error, missing_tools, required_tools,
};
use super::{Error, LoadedPlugin, Result, identifier, io_error};

impl LoadedPlugin {
    /// Check all applicable dependencies, then invoke the plugin and capture its output.
    /// 检查全部适用依赖，再调用插件并捕获输出。
    ///
    /// Bootstrap runs on the host; every other task runs through nspawn. No plugin code
    /// is executed when a required tool is missing. This does not create versions or caches.
    /// bootstrap 在宿主运行；其他任务通过 nspawn 运行。缺失工具时不执行插件代码。
    /// 此方法不创建版本或缓存。
    ///
    /// # Arguments
    ///
    /// - `self`: Loaded plugin retaining resources and writable scratch space.
    ///   保留资源和可写临时空间的已加载插件。
    /// - `operation`: Valid task identifier, with `bootstrap` selecting host execution.
    ///   有效任务标识；`bootstrap` 选择宿主执行。
    /// - `arguments`: Independently passed native task arguments, never shell source.
    ///   独立传递的原生任务参数，不作为 shell 源码。
    /// - `root`: Existing absolute system-root directory used by bootstrap or nspawn.
    ///   bootstrap 或 nspawn 使用的已存在系统根绝对目录。
    /// - `architecture`: Target architecture supported by this plugin.
    ///   此插件支持的目标架构。
    ///
    /// # Errors
    ///
    /// Returns an error for invalid context, unavailable dependencies, probe failures,
    /// process startup errors, or a nonzero plugin exit status.
    /// 上下文无效、依赖不可用、探测失败、进程启动失败或插件非零退出时返回错误。
    pub fn invoke(
        &self,
        operation: &str,
        arguments: &[OsString],
        root: &Path,
        architecture: &str,
    ) -> Result<Output> {
        let path = std::env::var_os("PATH").unwrap_or_else(|| {
            OsString::from("/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin")
        });
        self.invoke_environment(operation, arguments, root, architecture, (&path, None))
    }

    /// Invoke a task with frozen configuration and stable system identity.
    /// 使用冻结配置和稳定系统标识调用任务。
    ///
    /// # Arguments
    ///
    /// - `self`: Loaded plugin resources.
    ///   已加载的插件资源。
    /// - `operation`: Task identifier.
    ///   任务标识。
    /// - `arguments`: Native task arguments.
    ///   原生任务参数。
    /// - `root`: Writable system root.
    ///   可写系统根。
    /// - `context`: Archived inputs and identity.
    ///   留档输入和标识。
    ///
    /// # Errors
    ///
    /// Returns validation, preflight, or execution failures.
    /// 返回校验、预检查或执行错误。
    pub fn invoke_for_build(
        &self,
        operation: &str,
        arguments: &[OsString],
        root: &Path,
        context: &crate::controller::BuildContext,
    ) -> Result<Output> {
        identifier(&context.system_id)?;
        if !context.config.is_absolute() || !context.config.is_dir() {
            return Err(Error::Invalid("冻结配置目录无效".into()));
        }
        let path = std::env::var_os("PATH").unwrap_or_else(|| OsString::from("/usr/bin:/bin"));
        self.invoke_environment(
            operation,
            arguments,
            root,
            &context.architecture,
            (&path, Some(context)),
        )
    }

    /// Perform invocation using one fixed host PATH for both checking and execution.
    /// 使用同一个固定宿主 PATH 进行检查和执行。
    ///
    /// # Arguments
    ///
    /// - `self`: Loaded plugin providing metadata and resources.
    ///   提供元数据和资源的已加载插件。
    /// - `operation`: Plugin task determining host or container execution.
    ///   决定宿主或容器执行位置的插件任务。
    /// - `arguments`: Native argument values passed to the task.
    ///   传给任务的原生参数值。
    /// - `root`: Existing absolute target-system root.
    ///   已存在的目标系统根绝对路径。
    /// - `architecture`: Supported target architecture.
    ///   支持的目标架构。
    /// - `host_path`: Host executable search path retained for the spawned process.
    ///   为新进程保留的宿主可执行文件搜索路径。
    #[cfg(test)]
    fn invoke_with_path(
        &self,
        operation: &str,
        arguments: &[OsString],
        root: &Path,
        architecture: &str,
        host_path: &OsStr,
    ) -> Result<Output> {
        self.invoke_environment(operation, arguments, root, architecture, (host_path, None))
    }

    /// Execute with a fixed host path and optional frozen build context.
    /// 使用固定宿主路径和可选冻结构建上下文执行。
    ///
    /// # Arguments
    ///
    /// - `self`: Loaded resources.
    ///   已加载资源。
    /// - `operation`: Task identifier.
    ///   任务标识。
    /// - `arguments`: Native task arguments.
    ///   原生任务参数。
    /// - `root`: Target root.
    ///   目标根。
    /// - `architecture`: Target architecture.
    ///   目标架构。
    /// - `environment`: Host PATH and optional archived inputs.
    ///   宿主 PATH 和可选留档输入。
    fn invoke_environment(
        &self,
        operation: &str,
        arguments: &[OsString],
        root: &Path,
        architecture: &str,
        environment: (&OsStr, Option<&crate::controller::BuildContext>),
    ) -> Result<Output> {
        let (host_path, context) = environment;
        identifier(operation)?;
        identifier(architecture)?;
        if !root.is_absolute() || !root.is_dir() {
            return Err(Error::Invalid("插件任务需要已存在的系统根绝对目录".into()));
        }
        if !self
            .metadata
            .architectures
            .iter()
            .any(|value| value == architecture)
        {
            return Err(Error::Invalid(format!(
                "插件 {} 不支持架构 {architecture}",
                self.metadata.id
            )));
        }
        let mut command = if operation == "bootstrap" {
            let tools = required_tools(&self.metadata.tools.host, &["bash"]);
            check_host(self, operation, &tools, host_path)?;
            let bash = executable("bash", self.root(), host_path)
                .ok_or_else(|| missing_tools(self, operation, "宿主", vec!["bash".into()]))?;
            let mut command = Command::new(bash);
            command
                .current_dir(self.root())
                .env("PATH", host_path)
                .env("RSTAR_PLUGIN_ROOT", self.root())
                .env("RSTAR_PLUGIN_TMP", self.temporary_path())
                .env("RSTAR_ROOT", root)
                .arg(self.entry_path());
            command
        } else {
            check_host(self, operation, &["systemd-nspawn".into()], host_path)?;
            let launcher =
                executable("systemd-nspawn", self.root(), host_path).ok_or_else(|| {
                    missing_tools(self, operation, "宿主", vec!["systemd-nspawn".into()])
                })?;
            let required = required_tools(&self.metadata.tools.system, &["bash"]);
            let mut probe = nspawn(self, &launcher, root, host_path, architecture, context)?;
            probe
                .arg("--")
                .arg("/bin/sh")
                .arg("-c")
                .arg(SYSTEM_PROBE)
                .arg("rstarsleep-tool-check")
                .args(&required);
            let output = probe
                .output()
                .map_err(|source| io_error(&launcher, source))?;
            if !output.status.success() {
                return Err(execution_error(
                    self,
                    &format!("{operation} 工具检查"),
                    &output,
                ));
            }
            let report = String::from_utf8(output.stdout)
                .map_err(|_| Error::Invalid("目标工具检查输出不是 UTF-8".into()))?;
            let missing: Vec<String> = report.lines().map(str::to_owned).collect();
            if missing.iter().any(|tool| !required.contains(tool)) {
                return Err(Error::Invalid("目标工具检查返回了未知工具名称".into()));
            }
            if !missing.is_empty() {
                return Err(missing_tools(self, operation, "目标系统", missing));
            }
            let mut command = nspawn(self, &launcher, root, host_path, architecture, context)?;
            command
                .arg("--")
                .arg("bash")
                .arg(Path::new(SYSTEM_PLUGIN).join(&self.metadata.entry));
            command
        };
        if let Some(context) = context {
            command
                .env("RSTAR_CONFIG_DIR", &context.config)
                .env("RSTAR_SYSTEM_ID", &context.system_id);
        }
        command.env("RSTAR_ARCH", architecture);
        command.arg(operation).args(arguments);
        let output = command
            .output()
            .map_err(|source| io_error(&self.archive, source))?;
        if !output.status.success() {
            return Err(execution_error(self, operation, &output));
        }
        Ok(output)
    }
}

#[cfg(all(test, unix))]
#[path = "runner_tests.rs"]
mod tests;

#[cfg(all(test, unix))]
#[path = "context_tests.rs"]
mod context_tests;
