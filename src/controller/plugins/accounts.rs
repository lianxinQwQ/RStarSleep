//! Host-side account operations can also inspect sources before a target exists.
use super::{Error, LoadedPlugin, Result, tools};
use std::{
    ffi::{OsStr, OsString},
    path::Path,
    process::{Command, Output},
};

impl LoadedPlugin {
    pub(super) fn invoke_account(
        &self,
        operation: &str,
        arguments: &[OsString],
        root: &Path,
        architecture: &str,
        host_path: &OsStr,
        context: Option<&crate::controller::BuildContext>,
    ) -> Result<Output> {
        let requirements = self.metadata.account_inheritance.as_ref().ok_or_else(|| {
            Error::Invalid("plugin does not advertise account_inheritance".into())
        })?;
        if !root.is_absolute()
            || !root.is_dir()
            || !self
                .metadata
                .architectures
                .iter()
                .any(|a| a == architecture)
        {
            return Err(Error::Invalid("账户操作的系统根或架构无效".into()));
        }
        let entry = self.operation_path(operation)?;
        let script = entry.extension() == Some(OsStr::new("sh"));
        let mandatory: &[&str] = if script { &["bash"] } else { &[] };
        tools::check_host(
            self,
            operation,
            &tools::required_tools(&requirements.tools, mandatory),
            host_path,
        )?;
        let mut command = if script {
            let bash = tools::executable("bash", self.root(), host_path).unwrap();
            let mut command = Command::new(bash);
            command.arg(&entry);
            command
        } else {
            Command::new(entry)
        };
        for (name, _) in std::env::vars_os() {
            if name.as_encoded_bytes().starts_with(b"RSTAR_")
                || name.as_encoded_bytes().starts_with(b"RSTAT_")
            {
                command.env_remove(name);
            }
        }
        command
            .current_dir(self.root())
            .env("PATH", host_path)
            .env("RSTAR_PLUGIN_ROOT", self.root())
            .env("RSTAR_PLUGIN_TMP", self.temporary_path())
            .env("RSTAR_ROOT", root)
            .args(arguments);
        if let Some(context) = context {
            let raw = crate::config::read_plugin_parameters_at(&context.config)?;
            self.validate_parameter_lists(&raw)?;
            command
                .envs(self.configuration_environment(&context.parameters, architecture)?)
                .env("RSTAR_CONFIG_DIR", &context.config)
                .env("RSTAR_SYSTEM_ID", &context.system_id)
                .env(
                    "RSTAR_PARAMETERS_JSON",
                    serde_json::to_string(&raw.lists).map_err(|e| Error::Invalid(e.to_string()))?,
                );
            crate::controller::container::host_environment(&mut command, &context.environment);
        }
        super::control::Server::clear_environment(&mut command);
        let output = crate::controller::output::stream(&mut command, operation == "account_check")
            .map_err(|e| super::io_error(&self.archive, e))?;
        if !output.status.success() {
            return Err(tools::execution_error(self, operation, &output));
        }
        Ok(output)
    }
}
