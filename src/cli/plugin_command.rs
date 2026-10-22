//! Discover globally declared plugin commands without consuming their arguments.
//! 查找全局声明的插件命令，不消费其参数。
use crate::{
    config::{ConfigStore, DEFAULT_MOUNT_POINT},
    controller::plugins,
};
use std::{collections::BTreeMap, ffi::OsString, fs};

/// Resolve a unique host plugin command and forward all arguments unchanged.
/// 解析唯一宿主插件命令，并原样透传所有参数。
/// # Arguments
/// - `name`: Requested command name. 请求的命令名。
/// - `arguments`: Unmodified native arguments. 未修改的原生参数。
pub(super) fn run(name: &str, arguments: &[OsString]) -> super::Result<()> {
    let manager =
        std::env::var_os("RSTAR_MANAGER_ROOT").unwrap_or_else(|| DEFAULT_MOUNT_POINT.into());
    let store = ConfigStore::new(manager)?;
    let mut resources = fs::read_dir(store.plugins_path())?
        .map(|entry| entry.map(|e| e.path()))
        .collect::<std::io::Result<Vec<_>>>()?;
    resources.sort();
    let mut plugins_by_command = BTreeMap::new();
    let mut loaded = Vec::new();
    let mut warnings = Vec::new();
    for resource in resources {
        let plugin = match plugins::load(&resource) {
            Ok(value) => value,
            Err(error) => {
                warnings.push(error.to_string());
                continue;
            }
        };
        let index = loaded.len();
        for declaration in &plugin.metadata.commands {
            let command = declaration
                .split_once(':')
                .map(|(name, _)| name)
                .unwrap_or(declaration);
            if plugins_by_command
                .insert(command.to_owned(), index)
                .is_some()
            {
                return Err(format!("多个插件声明同一子命令: {command} / multiple plugins declare the same subcommand: {command}").into());
            }
        }
        loaded.push(plugin);
    }
    let index = plugins_by_command.get(name).ok_or_else(|| {
        format!(
            "未知子命令 {name}；插件诊断：{0} / unknown subcommand {name}; plugin diagnostics: {0}",
            warnings.join("; ")
        )
    })?;
    loaded[*index].invoke_command(name, arguments, store.root())?;
    Ok(())
}
