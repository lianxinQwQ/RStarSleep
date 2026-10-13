//! Check declared tools before invoking plugin entries in their actual execution environment.
//! 在实际执行环境检查声明的工具，再调用插件入口。

use std::{
    ffi::{OsStr, OsString},
    path::Path,
    process::{Command, Output},
};

use super::container::{SYSTEM_PLUGIN, nspawn_from};
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

    /// Invoke a task with frozen inputs, streaming diagnostics and capturing query records.
    /// 使用冻结输入调用任务，即时转发诊断并捕获查询记录。
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
        if operation == "post_maintenance" && !self.metadata.post_maintenance {
            return Err(Error::Invalid(
                "plugin does not advertise post_maintenance".into(),
            ));
        }
        if operation == "maintain_packages" && !self.metadata.package_maintenance {
            return Err(Error::Invalid(
                "plugin does not advertise package_maintenance".into(),
            ));
        }
        if operation == "query_package_files" && !self.metadata.query_package_files {
            return Err(Error::Invalid(
                "plugin does not advertise query_package_files".into(),
            ));
        }
        if operation == "prepare_inheritance_initramfs"
            && self.metadata.initramfs_inheritance.is_none()
        {
            return Err(Error::Invalid(
                "plugin does not advertise initramfs_inheritance".into(),
            ));
        }
        if operation == "collect_packages" && self.metadata.package_plan.is_none() {
            return Err(Error::Invalid(
                "plugin does not advertise package_plan".into(),
            ));
        }
        let standalone = std::collections::BTreeMap::from([(
            "RSTAR_DISTRIBUTION_ARCHITECTURE".into(),
            architecture.into(),
        )]);
        let options = context
            .map(|value| &value.parameters)
            .unwrap_or(&standalone);
        let mut configuration = if matches!(operation, "predownload" | "collect_packages") {
            self.download_environment(context, architecture)?
        } else {
            self.configuration_environment(options, architecture)?
        };
        if let Some(context) = context {
            let parameters = crate::config::PluginParameters {
                values: context.parameters.clone(),
                lists: context.parameter_lists.clone(),
                ..Default::default()
            };
            self.validate_parameter_lists(&parameters)?;
            configuration.insert(
                "RSTAR_PARAMETERS_JSON".into(),
                serde_json::to_string(&parameters.lists)
                    .map_err(|e| Error::Invalid(e.to_string()))?,
            );
        }
        let mappings = self.persistent_directories()?;
        let mounts = context
            .map(|c| c.persistent_mounts.as_slice())
            .unwrap_or(&[]);
        if mappings.len() != mounts.len()
            || mappings.iter().any(|m| {
                !mounts
                    .iter()
                    .any(|b| b.target == m.target && b.source.is_absolute() && b.source.is_dir())
            })
        {
            return Err(Error::Invalid("插件持久目录未由控制器完整分配".into()));
        }
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
        let operation_path = self.operation_path(operation)?;
        let shell_script = operation_path.extension() == Some(OsStr::new("sh"));
        let mandatory: &[&str] = if shell_script { &["bash"] } else { &[] };
        let workspace = if !mounts.is_empty() {
            Some(if operation == "bootstrap" {
                super::persistent::host_workspace(self.root(), mounts)?
            } else {
                super::persistent::container_workspace(self.root(), mounts)?
            })
        } else {
            None
        };
        let visible_root = workspace.as_ref().map(|w| w.path()).unwrap_or(self.root());
        let mut command = if operation == "bootstrap" {
            let tools = required_tools(&self.metadata.tools.host, mandatory);
            check_host(self, operation, &tools, host_path)?;
            let entry = visible_root.join(
                operation_path
                    .strip_prefix(self.root())
                    .expect("operation in resources"),
            );
            let mut command = if shell_script {
                let bash = executable("bash", self.root(), host_path)
                    .ok_or_else(|| missing_tools(self, operation, "宿主", vec!["bash".into()]))?;
                let mut command = Command::new(bash);
                command.arg(&entry);
                command
            } else {
                Command::new(&entry)
            };
            command
                .current_dir(visible_root)
                .env("PATH", host_path)
                .env("RSTAR_PLUGIN_ROOT", visible_root)
                .env("RSTAR_PLUGIN_TMP", self.temporary_path())
                .env("RSTAR_ROOT", root);
            command
        } else {
            check_host(self, operation, &["systemd-nspawn".into()], host_path)?;
            let launcher =
                executable("systemd-nspawn", self.root(), host_path).ok_or_else(|| {
                    missing_tools(self, operation, "宿主", vec!["systemd-nspawn".into()])
                })?;
            let mut declared = self.metadata.tools.system.clone();
            if operation == "predownload" {
                declared.extend(self.metadata.tools.predownload.iter().cloned());
            }
            if operation == "prepare_inheritance_initramfs"
                && let Some(requirements) = &self.metadata.initramfs_inheritance
            {
                declared.extend(requirements.tools.iter().cloned());
            }
            if operation == "collect_packages"
                && let Some(plan) = &self.metadata.package_plan
            {
                declared.extend(plan.tools.iter().cloned());
            }
            let required = required_tools(&declared, mandatory);
            let mut probe = nspawn_from(
                self,
                &launcher,
                root,
                host_path,
                context,
                visible_root,
                &configuration,
            )?;
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
            let mut command = nspawn_from(
                self,
                &launcher,
                root,
                host_path,
                context,
                visible_root,
                &configuration,
            )?;
            command.arg("--");
            if shell_script {
                command.arg("bash");
            }
            command.arg(
                Path::new(SYSTEM_PLUGIN).join(
                    operation_path
                        .strip_prefix(self.root())
                        .expect("operation in resources"),
                ),
            );
            command
        };
        if let Some(context) = context {
            command
                .env("RSTAR_CONFIG_DIR", &context.config)
                .env("RSTAR_SYSTEM_ID", &context.system_id);
        }
        // Optional settings must not accidentally inherit values from the invoking shell.
        // 可选设置不能意外继承调用 shell 中的值。
        for name in self
            .metadata
            .config
            .required
            .iter()
            .chain(&self.metadata.config.optional)
        {
            command.env_remove(super::configuration::environment_name(name)?);
        }
        // Clear inherited plugin/context variables before restoring this invocation's values.
        // 恢复本次调用值之前清除继承的插件及上下文变量。
        for (name, _) in std::env::vars_os() {
            if (name.as_encoded_bytes().starts_with(b"RSTAR_")
                || name.as_encoded_bytes().starts_with(b"RSTAT_"))
                && !matches!(
                    name.to_str(),
                    Some(
                        "RSTAR_PLUGIN_ROOT"
                            | "RSTAR_PLUGIN_TMP"
                            | "RSTAR_ROOT"
                            | "RSTAR_CONFIG_DIR"
                            | "RSTAR_SYSTEM_ID"
                    )
                )
            {
                command.env_remove(name);
            }
        }
        if context.is_none() {
            command
                .env_remove("RSTAR_CONFIG_DIR")
                .env_remove("RSTAR_SYSTEM_ID");
        }
        command.env_remove("RSTAR_ARCH");
        command.envs(configuration);
        if operation == "bootstrap"
            && let Some(context) = context
        {
            crate::controller::container::host_environment(&mut command, &context.environment);
        }
        command.args(arguments);
        let output = if context.is_some() {
            crate::controller::output::stream(
                &mut command,
                super::is_query(operation)
                    || (operation == "predownload" && self.metadata.predownload_plans),
            )
        } else {
            command.output()
        }
        .map_err(|source| io_error(&self.archive, source))?;
        if !output.status.success() {
            return Err(execution_error(self, operation, &output));
        }
        Ok(output)
    }
}

#[cfg(all(test, unix))]
#[path = "runner_tests.rs"]
pub(super) mod tests;

#[cfg(all(test, unix))]
#[path = "context_tests.rs"]
mod context_tests;
