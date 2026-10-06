//! Checked filesystem paths and durable atomic records.
//! 经检查的文件系统路径和持久化原子记录。
use super::{Error, Result};
use serde::Serialize;
use std::{
    fs,
    io::Write,
    path::{Component, Path},
};

/// Attach a filesystem path to its original error.
/// 为原始错误附加文件系统路径。
/// # Arguments
///
/// - `path`: Affected path.
///   相关路径。
/// - `source`: Original failure.
///   原始错误。
pub(super) fn io(path: &Path, source: std::io::Error) -> Error {
    Error::Io {
        path: path.into(),
        source,
    }
}

/// Reject relative paths, parent traversal, links, and non-directory ancestors.
/// 拒绝相对路径、父级跳转、链接及非目录祖先。
/// # Arguments
///
/// - `path`: Absolute owned path to inspect.
///   待检查的所属绝对路径。
pub(super) fn checked(path: &Path) -> Result<()> {
    if !path.is_absolute() || path.components().any(|c| matches!(c, Component::ParentDir)) {
        return Err(Error::Invalid(format!(
            "路径必须绝对且不含父级跳转: {}",
            path.display()
        )));
    }
    let mut current = std::path::PathBuf::new();
    for part in path.components() {
        current.push(part);
        match fs::symlink_metadata(&current) {
            Ok(info) if info.file_type().is_symlink() => {
                return Err(Error::Invalid(format!(
                    "路径不能经过链接: {}",
                    current.display()
                )));
            }
            Ok(info) if current != path && !info.is_dir() => {
                return Err(Error::Invalid(format!(
                    "祖先必须是目录: {}",
                    current.display()
                )));
            }
            Ok(_) => (),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(error) => return Err(io(&current, error)),
        }
    }
    Ok(())
}

/// Create an ordinary directory after validating its ancestors.
/// 校验祖先后创建普通目录。
/// # Arguments
///
/// - `path`: Directory to create.
///   待创建目录。
pub(super) fn directory(path: &Path) -> Result<()> {
    checked(path)?;
    fs::create_dir_all(path).map_err(|e| io(path, e))
}

/// Distinguish missing paths from unreadable or substituted entries.
/// 区分路径缺失与不可读取或被替换的条目。
/// # Arguments
///
/// - `path`: Entry to inspect.
///   待检查条目。
pub(super) fn exists(path: &Path) -> Result<bool> {
    checked(path)?;
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(io(path, e)),
    }
}

/// Flush a directory after installing durable entries.
/// 安装持久化条目后同步目录。
/// # Arguments
///
/// - `path`: Directory whose entries changed.
///   条目发生变化的目录。
pub(super) fn sync(path: &Path) -> Result<()> {
    fs::File::open(path)
        .and_then(|f| f.sync_all())
        .map_err(|e| io(path, e))
}

/// Atomically replace a TOML record only after flushing its complete contents.
/// 完整内容同步后原子替换 TOML 记录。
/// # Arguments
///
/// - `path`: Record destination.
///   记录目标。
/// - `value`: Serializable record.
///   可序列化记录。
pub(super) fn write<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let text = toml::to_string(value).map_err(|e| Error::Invalid(e.to_string()))?;
    write_text(path, &text)
}

/// Atomically replace UTF-8 text after flushing its complete contents.
/// 完整内容同步后原子替换 UTF-8 文本。
/// # Arguments
/// - `path`: Record or configuration destination.
///   记录或配置目标。
/// - `text`: Complete serialized contents.
///   完整序列化内容。
pub(super) fn write_text(path: &Path, text: &str) -> Result<()> {
    checked(path)?;
    let parent = path
        .parent()
        .ok_or_else(|| Error::Invalid("记录缺少父目录".into()))?;
    let mut file = tempfile::NamedTempFile::new_in(parent).map_err(|e| io(parent, e))?;
    file.write_all(text.as_bytes())
        .and_then(|_| file.as_file().sync_all())
        .map_err(|e| io(path, e))?;
    file.persist(path).map_err(|e| io(path, e.error))?;
    sync(parent)
}
