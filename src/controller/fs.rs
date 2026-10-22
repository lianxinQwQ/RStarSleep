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
            "路径必须绝对且不含父级跳转: {0} / Path must be absolute and must not contain parent-directory traversal: {0}",
            path.display()
        )));
    }
    let mut current = std::path::PathBuf::new();
    for part in path.components() {
        current.push(part);
        match fs::symlink_metadata(&current) {
            Ok(info) if info.file_type().is_symlink() => {
                return Err(Error::Invalid(format!(
                    "路径不能经过链接: {0} / Path must not traverse symbolic links: {0}",
                    current.display()
                )));
            }
            Ok(info) if current != path && !info.is_dir() => {
                return Err(Error::Invalid(format!(
                    "祖先必须是目录: {0} / Ancestor must be a directory: {0}",
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

/// Check an entry's ancestors without following or rejecting its final symbolic link.
/// 检查条目的祖先，不跟随或拒绝末段符号链接。
pub(super) fn checked_entry(path: &Path) -> Result<()> {
    if !path.is_absolute() || path.components().any(|c| matches!(c, Component::ParentDir)) {
        return Err(Error::Invalid(format!(
            "路径必须绝对且不含父级跳转: {0} / Path must be absolute and must not contain parent-directory traversal: {0}",
            path.display()
        )));
    }
    checked(path.parent().ok_or_else(|| {
        Error::Invalid("继承条目缺少父目录 / Inheritance entry has no parent directory".into())
    })?)
}

/// Select an inheritance entry literally; directory markers never dereference links.
/// 按字面选择继承条目；目录标记不会导致链接解引用。
pub(super) fn inheritance_source_path(root: &Path, guest: &Path) -> Result<std::path::PathBuf> {
    crate::config::inheritance_name(guest)?;
    checked(root)?;
    let unmarked = Path::new(guest.to_str().unwrap().trim_end_matches('/'));
    let path = root.join(unmarked.strip_prefix("/").unwrap());
    checked_entry(&path)?;
    fs::symlink_metadata(&path).map_err(|e| io(&path, e))?;
    Ok(path)
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
        .ok_or_else(|| Error::Invalid("记录缺少父目录 / Record has no parent directory".into()))?;
    let mut file = tempfile::NamedTempFile::new_in(parent).map_err(|e| io(parent, e))?;
    file.write_all(text.as_bytes())
        .and_then(|_| file.as_file().sync_all())
        .map_err(|e| io(path, e))?;
    file.persist(path).map_err(|e| io(path, e.error))?;
    sync(parent)
}

/// Resolve guest ancestors inside a root, including absolute directory symlinks.
/// The final entry is left unresolved so inheritance can replace a symlink itself.
/// 在系统根内解析目标路径的祖先，包括绝对目录链接；保留末段链接，以便继承替换链接本身。
pub(super) fn guest_path(root: &Path, guest: &Path) -> Result<std::path::PathBuf> {
    rooted_path(root, guest)
}

/// Interpret absolute links relative to the selected root and clamp parent traversal there.
/// 将绝对链接解释为相对于所选根的路径，并将父级跳转限制在该根内。
fn rooted_path(root: &Path, guest: &Path) -> Result<std::path::PathBuf> {
    use std::{collections::VecDeque, ffi::OsString, path::PathBuf};
    checked(root)?;
    if !guest.is_absolute() {
        return Err(Error::Invalid(
            "目标系统路径必须绝对 / Target system path must be absolute".into(),
        ));
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
                    return Err(Error::Invalid(
                        "目标路径符号链接循环 / Symbolic-link loop in target path".into(),
                    ));
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

    /// Source entries preserve every link byte and never traverse an ancestor link.
    /// 来源条目保留链接的每个字节，且不遍历祖先链接。
    #[test]
    fn inheritance_sources_leave_links_unresolved() {
        use std::{ffi::OsString, os::unix::ffi::OsStringExt};
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        fs::create_dir(root.join("data")).unwrap();
        let links = [
            ("absolute", OsString::from("/missing//../target/")),
            ("relative", OsString::from("../missing/./target")),
            ("loop", OsString::from("loop")),
            ("directory", OsString::from("data/")),
            ("native", OsString::from_vec(b"target-\xff".to_vec())),
        ];
        for (name, text) in links {
            symlink(&text, root.join(name)).unwrap();
            let path = inheritance_source_path(root, &Path::new("/").join(name)).unwrap();
            assert_eq!(path, root.join(name));
            assert_eq!(fs::read_link(path).unwrap().as_os_str(), text);
        }
        assert!(inheritance_source_path(root, Path::new("/directory/file")).is_err());
        assert!(inheritance_source_path(root, Path::new("/../data")).is_err());
        assert!(inheritance_source_path(root, Path::new("relative")).is_err());
        assert!(inheritance_source_path(root, Path::new("/missing")).is_err());
        fs::write(root.join("file"), "data").unwrap();
        assert!(inheritance_source_path(root, Path::new("/file/child")).is_err());
    }
}
