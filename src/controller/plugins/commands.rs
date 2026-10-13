//! Host-side interactive plugin commands with exact native argument forwarding.
//! 在宿主执行交互式插件命令，准确透传原生参数。
use super::{Error, LoadedPlugin, Result, identifier, io_error};
use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    fs,
    path::{Path, PathBuf},
    process::Command,
};

/// Parse command:entry declarations and reject built-in conflicts or duplicates.
/// 解析命令:入口声明，拒绝内置命令冲突或重复。
/// # Arguments
/// - `declarations`: Plugin command declarations. 插件命令声明。
pub(super) fn parse(declarations: &[String]) -> Result<BTreeMap<String, String>> {
    let mut commands = BTreeMap::new();
    for declaration in declarations {
        let (name, entry) = declaration
            .split_once(':')
            .unwrap_or((declaration, declaration));
        identifier(name)?;
        super::validation::relative_path(Path::new(entry))?;
        if crate::cli::BUILTIN_COMMANDS.contains(&name)
            || commands.insert(name.into(), entry.into()).is_some()
        {
            return Err(Error::Invalid(format!(
                "插件命令重复或与内置命令冲突: {name}"
            )));
        }
    }
    Ok(commands)
}

/// Locate a regular bounded shell script or executable entry.
/// 定位资源范围内的普通 shell 脚本或可执行入口。
/// # Arguments
/// - `root`: Frozen plugin root. 冻结插件根。
/// - `entry`: Relative declared entry. 声明的相对入口。
pub(super) fn entry_path(root: &Path, entry: &str) -> Result<PathBuf> {
    let direct = root.join(entry);
    let script = root.join(format!("{entry}.sh"));
    let path = if script.exists() { script } else { direct };
    crate::controller::fs::checked(&path).map_err(|e| Error::Invalid(e.to_string()))?;
    let info = fs::symlink_metadata(&path).map_err(|e| io_error(&path, e))?;
    if !info.is_file() {
        return Err(Error::Invalid(format!(
            "插件命令入口必须为普通文件: {}",
            path.display()
        )));
    }
    if path.extension() != Some(OsStr::new("sh")) {
        use std::os::unix::fs::PermissionsExt;
        if info.permissions().mode() & 0o111 == 0 {
            return Err(Error::Invalid("插件命令入口缺少执行权限".into()));
        }
    }
    Ok(path)
}

/// Execute a declared command with inherited terminal streams and no automatic elevation.
/// 以继承终端流执行声明的命令，不自动提权。
/// # Arguments
/// - `self`: Frozen plugin resources. 冻结插件资源。
/// - `name`: Declared command. 声明的命令。
/// - `arguments`: Native arguments to forward unchanged. 原样透传的原生参数。
/// - `manager`: Manager root exposed to the plugin. 向插件提供的管理根。
impl LoadedPlugin {
    pub fn invoke_command(&self, name: &str, arguments: &[OsString], manager: &Path) -> Result<()> {
        let commands = parse(&self.metadata.commands)?;
        let entry = commands
            .get(name)
            .ok_or_else(|| Error::Invalid(format!("插件未声明命令: {name}")))?;
        let path = entry_path(self.root(), entry)?;
        let mut command = if path.extension() == Some(OsStr::new("sh")) {
            let mut command = Command::new("bash");
            command.arg("--").arg(&path);
            command
        } else {
            Command::new(&path)
        };
        let status = command
            .args(arguments)
            .current_dir(self.root())
            .env("RSTAR_PLUGIN_ROOT", self.root())
            .env("RSTAR_PLUGIN_TMP", self.temporary_path())
            .env("RSTAR_MANAGER_ROOT", manager)
            .status()
            .map_err(|e| io_error(&path, e))?;
        if !status.success() {
            return Err(Error::Execution {
                plugin: self.metadata.id.clone(),
                operation: name.into(),
                status: status.to_string(),
                stdout: String::new(),
                stderr: "插件命令退出失败".into(),
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    /// Preserve whitespace, metacharacters and native argument bytes for interactive commands.
    /// 为交互式命令保留空白、元字符和原生参数字节。
    /// # Arguments
    /// No parameters. 无参数。
    #[test]
    fn forwards_arguments_literally() {
        use std::os::unix::ffi::OsStringExt;
        let mut plugin = super::super::runner::tests::fixture(&[], &[]);
        plugin.metadata.commands = vec!["configure:setup".into()];
        fs::write(
            plugin.root().join("setup.sh"),
            "#!/bin/bash\noutput=$1\nshift\nprintf '%s\\0' \"$@\" > \"$output\"\n",
        )
        .unwrap();
        let temporary = tempfile::tempdir().unwrap();
        let output = temporary.path().join("arguments");
        let native = OsString::from_vec(b"native-\xff".to_vec());
        plugin
            .invoke_command(
                "configure",
                &[
                    output.clone().into(),
                    "space value".into(),
                    "$(touch should-not-exist)".into(),
                    "--root".into(),
                    native,
                ],
                temporary.path(),
            )
            .unwrap();
        assert_eq!(
            fs::read(output).unwrap(),
            b"space value\0$(touch should-not-exist)\0--root\0native-\xff\0"
        );
    }

    /// Reject built-in overrides, duplicate aliases and unsafe entries.
    /// 拒绝覆盖内置命令、重复别名和不安全入口。
    /// # Arguments
    /// No parameters. 无参数。
    #[test]
    fn declarations_are_unambiguous() {
        assert_eq!(
            parse(&["configure:tools/setup".into(), "install-app".into()]).unwrap()["install-app"],
            "install-app"
        );
        for entries in [
            vec!["build:x"],
            vec!["clean"],
            vec!["one", "one:two"],
            vec!["one:../two"],
            vec!["one:/two"],
        ] {
            assert!(parse(&entries.into_iter().map(String::from).collect::<Vec<_>>()).is_err());
        }
    }
}
