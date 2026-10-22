//! Freeze and fingerprint all configuration files for reproducible stage provenance.
//! 冻结并指纹化所有配置文件，确保阶段来源可重现。
use super::{Error, Result, fs as disk};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File},
    io::Read,
    path::Path,
};
/// Freeze ordinary configuration and bounded script resources without following links.
/// 冻结普通配置及限定范围的脚本资源，不跟随链接。
/// # Arguments
///
/// - `source`: Editable configuration directory.
///   可编辑配置目录。
/// - `destination`: Private staging directory.
///   私有暂存目录。
pub(super) fn copy_config(source: &Path, destination: &Path) -> Result<()> {
    disk::checked(source)?;
    copy_directory(source, destination, true)
}

/// Copy configuration recursively while reserving resource semantics for the top-level script tree.
/// 递归复制配置，仅为顶层 script 树保留资源语义。
/// # Arguments
/// - `source`: Current source directory.
///   当前源目录。
/// - `destination`: Private target directory.
///   私有目标目录。
/// - `config_root`: Whether this is the top-level configuration directory.
///   是否为顶层配置目录。
fn copy_directory(source: &Path, destination: &Path, config_root: bool) -> Result<()> {
    for entry in entries(source)? {
        let info = entry.file_type().map_err(|e| disk::io(&entry.path(), e))?;
        let target = destination.join(entry.file_name());
        if info.is_dir() {
            fs::create_dir(&target).map_err(|e| disk::io(&target, e))?;
            if config_root && entry.file_name() == "script" {
                super::resources::copy_tree(&entry.path(), &target)?;
                disk::sync(&target)?;
            } else {
                copy_directory(&entry.path(), &target, false)?;
            }
        } else if info.is_file() {
            fs::copy(entry.path(), &target).map_err(|e| disk::io(&target, e))?;
            File::open(&target)
                .and_then(|f| f.sync_all())
                .map_err(|e| disk::io(&target, e))?;
        } else {
            return Err(Error::Invalid(format!(
                "配置仅允许普通文件及目录: {0} / Configuration permits only regular files and directories: {0}",
                entry.path().display()
            )));
        }
    }
    disk::sync(destination)
}

/// Enumerate configuration entries in stable native-name order.
/// 以稳定原生名称顺序列举配置条目。
/// # Arguments
///
/// - `path`: Configuration directory.
///   配置目录。
fn entries(path: &Path) -> Result<Vec<fs::DirEntry>> {
    let mut result = fs::read_dir(path)
        .map_err(|e| disk::io(path, e))?
        .collect::<std::io::Result<Vec<_>>>()
        .map_err(|e| disk::io(path, e))?;
    result.sort_by_key(|e| e.file_name());
    Ok(result)
}

/// Hash all archived inputs to detect provenance changes.
/// 散列全部留档输入以检测来源变化。
/// # Arguments
///
/// - `config`: Archived configuration directory.
///   留档配置目录。
pub(super) fn config_hash(config: &Path) -> Result<String> {
    disk::checked(config)?;
    let mut digest = Sha256::new();
    hash_directory(config, Path::new(""), &mut digest)?;
    Ok(format!("{:x}", digest.finalize()))
}

/// Feed sorted configuration entries with unambiguous length boundaries into the digest.
/// 将带明确长度边界的有序配置条目输入摘要。
/// # Arguments
///
/// - `root`: Configuration root.
///   配置根。
/// - `relative`: Current relative directory.
///   当前相对目录。
/// - `digest`: Accumulated SHA-256 state.
///   累积 SHA-256 状态。
fn hash_directory(root: &Path, relative: &Path, digest: &mut Sha256) -> Result<()> {
    for entry in entries(&root.join(relative))? {
        let path = relative.join(entry.file_name());
        if path == Path::new("script") {
            let hash = super::resources::tree_hash(&entry.path())?;
            digest.update(b"script-tree");
            digest.update(hash.as_bytes());
            continue;
        }
        let name = path.as_os_str().as_encoded_bytes();
        digest.update((name.len() as u64).to_le_bytes());
        digest.update(name);
        let kind = entry.file_type().map_err(|e| disk::io(&entry.path(), e))?;
        if kind.is_dir() {
            digest.update(b"D");
            hash_directory(root, &path, digest)?;
        } else if kind.is_file() {
            digest.update(b"F");
            let mut content = Vec::new();
            File::open(entry.path())
                .and_then(|mut f| f.read_to_end(&mut content))
                .map_err(|e| disk::io(&entry.path(), e))?;
            if path.starts_with("build") {
                let text =
                    std::str::from_utf8(&content).map_err(|e| Error::Invalid(e.to_string()))?;
                content = crate::config::normalized_build_config(text, &entry.path())?;
            }
            digest.update((content.len() as u64).to_le_bytes());
            digest.update(content);
        } else {
            return Err(Error::Invalid(format!(
                "留档配置含特殊条目: {0} / Archived configuration contains special entries: {0}",
                entry.path().display()
            )));
        }
    }
    Ok(())
}
