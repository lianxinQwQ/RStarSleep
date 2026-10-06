//! Copy bounded directory trees and fingerprint paths, bytes, links, and permissions.
//! 复制限定范围的目录树，并计算路径、内容、链接及权限的指纹。
use super::relative_path;
use crate::controller::{Error, Result, fs as disk};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Write},
    path::Path,
};

/// List entries in stable native filename order.
/// 按稳定的原生文件名顺序列出条目。
/// # Arguments
/// - `path`: Ordinary directory to enumerate.
///   待列举的普通目录。
fn entries(path: &Path) -> Result<Vec<fs::DirEntry>> {
    if !fs::symlink_metadata(path)
        .map_err(|e| disk::io(path, e))?
        .is_dir()
    {
        return Err(Error::Invalid(format!(
            "资源根必须是普通目录: {}",
            path.display()
        )));
    }
    let mut entries = fs::read_dir(path)
        .map_err(|e| disk::io(path, e))?
        .collect::<std::io::Result<Vec<_>>>()
        .map_err(|e| disk::io(path, e))?;
    entries.sort_by_key(|entry| entry.file_name());
    Ok(entries)
}

/// Physically copy regular files and safe relative links without following links.
/// 实际复制普通文件及安全相对链接，不跟随链接。
/// # Arguments
/// - `source`: Source resource directory.
///   源资源目录。
/// - `destination`: Existing empty private directory.
///   已存在的空私有目录。
pub(crate) fn copy_tree(source: &Path, destination: &Path) -> Result<()> {
    for entry in entries(source)? {
        let path = entry.path();
        let target = destination.join(entry.file_name());
        let info = fs::symlink_metadata(&path).map_err(|e| disk::io(&path, e))?;
        if info.is_dir() {
            fs::create_dir(&target).map_err(|e| disk::io(&target, e))?;
            copy_tree(&path, &target)?;
        } else if info.is_file() {
            let mut input = fs::File::open(&path).map_err(|e| disk::io(&path, e))?;
            let mut output = fs::File::options()
                .write(true)
                .create_new(true)
                .open(&target)
                .map_err(|e| disk::io(&target, e))?;
            let mut buffer = [0u8; 65536];
            loop {
                let count = input.read(&mut buffer).map_err(|e| disk::io(&path, e))?;
                if count == 0 {
                    break;
                }
                output
                    .write_all(&buffer[..count])
                    .map_err(|e| disk::io(&target, e))?;
            }
            output.sync_all().map_err(|e| disk::io(&target, e))?;
        } else if info.file_type().is_symlink() {
            let link = fs::read_link(&path).map_err(|e| disk::io(&path, e))?;
            relative_path(&link)?;
            #[cfg(unix)]
            std::os::unix::fs::symlink(link, &target).map_err(|e| disk::io(&target, e))?;
            #[cfg(not(unix))]
            return Err(Error::Invalid("此平台不支持资源链接".into()));
            continue;
        } else {
            return Err(Error::Invalid(format!(
                "不支持的资源条目: {}",
                path.display()
            )));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(
                &target,
                fs::Permissions::from_mode(info.permissions().mode() & 0o777),
            )
            .map_err(|e| disk::io(&target, e))?;
        }
    }
    Ok(())
}

/// Fingerprint a frozen tree without depending on source location or timestamps.
/// 计算冻结树指纹，不依赖源位置或时间戳。
/// # Arguments
/// - `root`: Frozen resource tree.
///   冻结资源树。
pub(crate) fn tree_hash(root: &Path) -> Result<String> {
    let mut digest = Sha256::new();
    digest.update(b"rstarsleep-directory-v1");
    hash(root, Path::new(""), &mut digest)?;
    Ok(format!("{:x}", digest.finalize()))
}

/// Feed a length-delimited byte string to a digest.
/// 向摘要输入带长度边界的字节串。
/// # Arguments
/// - `digest`: Accumulated digest.
///   累积摘要。
/// - `bytes`: One unambiguous field.
///   一个明确字段。
fn field(digest: &mut Sha256, bytes: &[u8]) {
    digest.update((bytes.len() as u64).to_le_bytes());
    digest.update(bytes);
}

/// Hash sorted entries recursively, including empty directories and executable bits.
/// 递归散列有序条目，包括空目录和可执行位。
/// # Arguments
/// - `root`: Complete frozen tree.
///   完整冻结树。
/// - `relative`: Current directory relative to root.
///   相对根的当前目录。
/// - `digest`: Accumulated digest.
///   累积摘要。
fn hash(root: &Path, relative: &Path, digest: &mut Sha256) -> Result<()> {
    for entry in entries(&root.join(relative))? {
        let path = entry.path();
        let name = relative.join(entry.file_name());
        field(digest, name.as_os_str().as_encoded_bytes());
        let info = fs::symlink_metadata(&path).map_err(|e| disk::io(&path, e))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            digest.update((info.permissions().mode() & 0o777).to_le_bytes());
        }
        if info.is_dir() {
            digest.update(b"D");
            hash(root, &name, digest)?;
        } else if info.is_file() {
            digest.update(b"F");
            digest.update(info.len().to_le_bytes());
            let mut file = fs::File::open(&path).map_err(|e| disk::io(&path, e))?;
            let mut buffer = [0u8; 65536];
            loop {
                let count = file.read(&mut buffer).map_err(|e| disk::io(&path, e))?;
                if count == 0 {
                    break;
                }
                digest.update(&buffer[..count]);
            }
        } else if info.file_type().is_symlink() {
            digest.update(b"L");
            let target = fs::read_link(&path).map_err(|e| disk::io(&path, e))?;
            relative_path(&target)?;
            field(digest, target.as_os_str().as_encoded_bytes());
        } else {
            return Err(Error::Invalid(format!(
                "不支持的资源条目: {}",
                path.display()
            )));
        }
    }
    Ok(())
}
