//! Select a unique plugin by metadata.
//! 按元数据选择唯一插件。
use super::{Error, LoadedPlugin, Result, identifier, io_error, load};
use crate::config::ConfigStore;
use crate::controller::resources::archive_format;
use std::{fs, io, path::Path};
/// Select a plugin using a managed system's current type and optional architecture.
/// 使用被管理系统的当前类型和可选架构选择插件。
///
/// # Arguments
///
/// - `store`: Workspace containing system configuration and the shared plugins directory.
///   包含系统配置和共享插件目录的工作目录。
/// - `id`: Managed-system identifier whose build configuration selects the plugin.
///   构建配置用于选择插件的被管理系统标识。
///
/// # Errors
///
/// Returns an error for invalid configuration, archive failures, no match, or multiple matches.
/// 配置无效、压缩包处理失败、无匹配或存在多个匹配时返回错误。
pub fn for_system(store: &ConfigStore, id: &str) -> Result<LoadedPlugin> {
    let config = store.read_system_type(id)?;
    let plugin = find(
        &store.plugins_path(),
        &config.system_type,
        config
            .architecture
            .as_deref()
            .unwrap_or(std::env::consts::ARCH),
    )?;
    plugin.validate_parameter_lists(&crate::config::read_plugin_parameters_at(
        &store.system_layout(id)?.config,
    )?)?;
    Ok(plugin)
}

/// Search archive and directory metadata and require one matching plugin.
/// 搜索压缩包及目录元数据，并要求恰好一个匹配插件。
///
/// # Arguments
///
/// - `directory`: Existing shared directory containing plugin archives or directories.
///   存放插件压缩包或目录的现有共享目录。
/// - `system_type`: Valid type matched against metadata system_types or the plugin identity.
///   有效系统类型，与元数据 system_types 或插件标识匹配。
/// - `architecture`: Target architecture required by the current system.
///   当前系统要求的目标架构。
///
/// # Errors
///
/// Returns an error for inaccessible directories or an ambiguous/missing match. Invalid
/// resources are retained as diagnostics instead of blocking an otherwise unique valid match.
/// 目录无法访问、匹配不明确或缺失时返回错误。无效资源保留为诊断信息，
/// 不阻止其他有效插件的唯一匹配。
pub fn find(directory: &Path, system_type: &str, architecture: &str) -> Result<LoadedPlugin> {
    identifier(system_type)?;
    identifier(architecture)?;
    let mut paths = fs::read_dir(directory)
        .map_err(|source| io_error(directory, source))?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<io::Result<Vec<_>>>()
        .map_err(|source| io_error(directory, source))?;
    paths.sort();
    let mut matches = Vec::new();
    let mut unsupported = Vec::new();
    let mut warnings = Vec::new();
    for path in paths
        .into_iter()
        .filter(|path| path.is_dir() || archive_format(path).is_some())
    {
        let plugin = match load(&path) {
            Ok(plugin) => plugin,
            Err(error) => {
                warnings.push(format!("跳过插件 {}: {error}", path.display()));
                continue;
            }
        };
        let types = &plugin.metadata.system_types;
        let matches_type = if types.is_empty() {
            plugin.metadata.id == system_type
        } else {
            types.iter().any(|value| value == system_type)
        };
        if matches_type {
            if plugin
                .metadata
                .architectures
                .iter()
                .any(|value| value == architecture)
            {
                matches.push(plugin);
            } else {
                unsupported.push(path);
            }
        }
    }
    if matches.len() > 1 {
        let paths = matches
            .iter()
            .map(|plugin| plugin.archive.display().to_string())
            .collect::<Vec<_>>()
            .join(", ");
        return Err(Error::Invalid(format!(
            "系统类型 {system_type}、架构 {architecture} 匹配多个插件: {paths}"
        )));
    }
    if let Some(mut plugin) = matches.pop() {
        plugin.discovery_warnings = warnings;
        return Ok(plugin);
    }
    Err({
        Error::Invalid(format!(
            "{} 中没有匹配系统类型 {system_type}、架构 {architecture} 的插件{}{}",
            directory.display(),
            if unsupported.is_empty() {
                String::new()
            } else {
                "（存在该类型但不支持目标架构的插件）".into()
            },
            if warnings.is_empty() {
                String::new()
            } else {
                format!("\n{}", warnings.join("\n"))
            }
        ))
    })
}
