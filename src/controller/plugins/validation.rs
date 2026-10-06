//! Validate metadata and bounded resource paths.
//! 校验元数据和限定范围的资源路径。
use super::{Error, Metadata, REQUIRED_OPERATIONS, Result, TEMPLATE_OPERATIONS};
use std::{
    fs, io,
    path::{Component, Path, PathBuf},
};
/// Validate compatibility fields and require each same-named operation file at the root.
/// 校验兼容性字段，并要求插件根内存在每个操作的同名文件。
///
/// # Arguments
///
/// - `metadata`: Parsed plugin metadata to validate.
///   待校验的已解析插件元数据。
/// - `root`: Normalized root containing the operation files.
///   包含操作文件的归一化插件根。
pub(super) fn validate_metadata(metadata: &Metadata, root: &Path) -> Result<()> {
    if metadata.schema_version != 1 || !matches!(metadata.api_version, 2 | 3) {
        return Err(Error::Invalid(format!(
            "插件 {} 的元数据或任务接口版本不受支持",
            metadata.id
        )));
    }
    identifier(&metadata.id)?;
    identifier(&metadata.cache_format)?;
    if metadata.version.trim().is_empty() || metadata.architectures.is_empty() {
        return Err(Error::Invalid("插件版本和支持架构不能为空".into()));
    }
    for value in metadata.system_types.iter().chain(&metadata.architectures) {
        identifier(value)?;
    }
    for tool in metadata.tools.host.iter().chain(&metadata.tools.system) {
        if tool.contains('/') {
            relative_path(Path::new(tool))?;
            if !Path::new(tool)
                .components()
                .any(|part| matches!(part, Component::Normal(_)))
            {
                return Err(Error::Invalid(format!(
                    "工具路径必须指向插件内文件: {tool:?}"
                )));
            }
        } else {
            identifier(tool)?;
        }
        if tool.chars().any(char::is_control) || tool.trim() != tool {
            return Err(Error::Invalid(format!("无效工具声明: {tool:?}")));
        }
    }
    for package in &metadata.protected_packages {
        crate::software::valid_name(package).map_err(|error| Error::Invalid(error.to_string()))?;
    }
    for operation in REQUIRED_OPERATIONS {
        operation_path(root, operation)?;
    }
    if metadata.api_version == 3 {
        for operation in TEMPLATE_OPERATIONS {
            operation_path(root, operation)?;
        }
    }
    Ok(())
}

/// Require a regular root-level file named after the operation with a .sh suffix.
/// 要求插件根内存在以操作名加 .sh 后缀命名的普通文件。
///
/// # Arguments
///
/// - `root`: Normalized plugin resource root.
///   归一化插件资源根。
/// - `operation`: Operation identifier, with .sh appended to form the filename.
///   操作标识；附加 .sh 后缀形成文件名。
pub(super) fn operation_path(root: &Path, operation: &str) -> Result<PathBuf> {
    identifier(operation)?;
    let path = root.join(format!("{operation}.sh"));
    let info = fs::symlink_metadata(&path).map_err(|error| {
        if error.kind() == io::ErrorKind::NotFound {
            Error::Invalid(format!(
                "插件操作 {operation} 缺少同名普通文件: {}",
                path.display()
            ))
        } else {
            io_error(&path, error)
        }
    })?;
    if !info.is_file() {
        return Err(Error::Invalid(format!(
            "插件操作 {operation} 必须是同名普通文件: {}",
            path.display()
        )));
    }
    Ok(path)
}

/// Require a relative archive path without parent traversal or platform prefixes.
/// 要求归档路径为相对路径，且没有父级跳转或平台前缀。
///
/// # Arguments
///
/// - `path`: Entry, link target, or script path to constrain within its resource root.
///   待限定在资源根内的条目、链接目标或脚本路径。
pub(super) fn relative_path(path: &Path) -> Result<()> {
    if path.as_os_str().is_empty()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(Error::Invalid(format!(
            "插件路径必须位于资源根内: {}",
            path.display()
        )));
    }
    Ok(())
}

/// Validate a stable ASCII plugin, system-type, or architecture identifier.
/// 校验稳定的 ASCII 插件、系统类型或架构标识。
///
/// # Arguments
///
/// - `value`: Identifier required to start with a letter or digit.
///   要求以字母或数字开头的标识。
pub(crate) fn identifier(value: &str) -> Result<()> {
    let mut bytes = value.bytes();
    if !bytes
        .next()
        .is_some_and(|byte| byte.is_ascii_alphanumeric())
        || !bytes.all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
    {
        return Err(Error::Invalid(format!("无效插件类型或架构标识: {value:?}")));
    }
    Ok(())
}

/// Retain the affected path and original filesystem failure.
/// 保留相关路径和原始文件系统错误。
///
/// # Arguments
///
/// - `path`: Path affected by the failure.
///   失败涉及的路径。
/// - `source`: Original I/O error.
///   原始 I/O 错误。
pub(super) fn io_error(path: impl Into<PathBuf>, source: io::Error) -> Error {
    Error::Io {
        path: path.into(),
        source,
    }
}
