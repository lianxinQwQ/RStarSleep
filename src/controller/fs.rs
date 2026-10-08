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

/// Resolve guest ancestors inside a root, including absolute directory symlinks.
/// The final entry is left unresolved so inheritance can replace a symlink itself.
pub(super) fn guest_path(root: &Path, guest: &Path) -> Result<std::path::PathBuf> {
    use std::{collections::VecDeque, ffi::OsString, path::PathBuf};
    checked(root)?;
    if !guest.is_absolute() {
        return Err(Error::Invalid("目标系统路径必须绝对".into()));
    }
    let mut pending: VecDeque<OsString> = guest
        .components()
        .filter_map(|p| match p {
            Component::Normal(p) => Some(p.to_owned()),
            _ => None,
        })
        .collect();
    let mut relative = PathBuf::new();
    let mut links = 0;
    while let Some(part) = pending.pop_front() {
        if part == "." {
            continue;
        }
        if part == ".." {
            relative.pop();
            continue;
        }
        let path = root.join(&relative).join(&part);
        match fs::symlink_metadata(&path) {
            Ok(info) if info.file_type().is_symlink() && !pending.is_empty() => {
                links += 1;
                if links > 40 {
                    return Err(Error::Invalid("目标路径符号链接循环".into()));
                }
                let link = fs::read_link(&path).map_err(|e| io(&path, e))?;
                if link.is_absolute() {
                    relative.clear();
                }
                let parts: Vec<_> = link
                    .components()
                    .filter_map(|p| match p {
                        Component::Normal(p) => Some(p.to_owned()),
                        Component::ParentDir => Some(OsString::from("..")),
                        _ => None,
                    })
                    .collect();
                for part in parts.into_iter().rev() {
                    pending.push_front(part);
                }
            }
            Ok(_) => relative.push(part),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => relative.push(part),
            Err(error) => return Err(io(&path, error)),
        }
    }
    Ok(root.join(relative))
}

#[cfg(test)]
mod guest_path_tests {
    use super::*;
    use std::os::unix::fs::symlink;
    #[test]
    fn guest_symlinks_stay_inside_root_and_final_links_can_be_replaced() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        fs::create_dir_all(root.join("usr/lib")).unwrap();
        symlink("/usr/lib", root.join("lib")).unwrap();
        assert_eq!(
            guest_path(root, Path::new("/lib/data")).unwrap(),
            root.join("usr/lib/data")
        );
        symlink("../../../../usr/lib", root.join("alias")).unwrap();
        assert_eq!(
            guest_path(root, Path::new("/alias/data")).unwrap(),
            root.join("usr/lib/data")
        );
        assert_eq!(
            guest_path(root, Path::new("/lib")).unwrap(),
            root.join("lib")
        );
        symlink("/loop", root.join("loop")).unwrap();
        assert!(guest_path(root, Path::new("/loop/data")).is_err());
    }
}
