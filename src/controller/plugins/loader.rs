//! Validate distribution metadata after shared resource preparation.
//! 共用资源准备后校验发行版元数据。
use super::validation::validate_metadata;
use super::{Error, LoadedPlugin, Metadata, Result, io_error};
use crate::controller::resources;
use std::{fs, path::Path};

/// Freeze an archive or directory, normalize its root, and validate its plugin interface.
/// 冻结压缩包或目录，归一化其根并校验插件接口。
/// # Arguments
/// - `path`: Plugin archive or directory containing plugin.toml.
///   含 plugin.toml 的插件压缩包或目录。
/// # Errors
/// Returns errors for unsafe resources, invalid metadata, missing operations, or I/O.
/// 资源不安全、元数据无效、操作缺失或 I/O 失败时返回错误。
pub fn load(path: &Path) -> Result<LoadedPlugin> {
    let prepared =
        resources::prepare(path, "plugin.toml", "plugin").map_err(|error| match error {
            crate::controller::Error::Io { path, source } => Error::Io { path, source },
            crate::controller::Error::Invalid(message) => Error::Invalid(message),
            error => Error::Invalid(error.to_string()),
        })?;
    let metadata_path = prepared.root.join("plugin.toml");
    let text = fs::read_to_string(&metadata_path).map_err(|e| io_error(&metadata_path, e))?;
    let metadata: Metadata = toml::from_str(&text)
        .map_err(|e| Error::Invalid(format!("{}: {e}", metadata_path.display())))?;
    validate_metadata(&metadata, &prepared.root)?;
    let scratch = prepared.temporary.path().join("tmp");
    fs::create_dir(&scratch).map_err(|e| io_error(&scratch, e))?;
    Ok(LoadedPlugin {
        metadata,
        archive: path.into(),
        archive_sha256: prepared.sha256,
        discovery_warnings: Vec::new(),
        root: prepared.root,
        temporary: prepared.temporary,
    })
}
