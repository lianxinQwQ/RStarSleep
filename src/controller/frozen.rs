//! Freeze and fingerprint all configuration files for reproducible stage provenance.
//! 冻结并指纹化所有配置文件，确保阶段来源可重现。
use super::{Error, Result, fs as disk};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File},
    io::Read,
    path::Path,
};
/// Copy only ordinary files and directories, refusing configuration links or special entries.
/// 仅复制普通文件和目录，拒绝配置链接或特殊条目。
/// # Arguments
///
/// - `source`: Editable configuration directory.
///   可编辑配置目录。
/// - `destination`: Private staging directory.
///   私有暂存目录。
pub(super) fn copy_config(source: &Path, destination: &Path) -> Result<()> {
    disk::checked(source)?;
    for entry in entries(source)? {
        let info = entry.file_type().map_err(|e| disk::io(&entry.path(), e))?;
        let target = destination.join(entry.file_name());
        if info.is_dir() {
            fs::create_dir(&target).map_err(|e| disk::io(&target, e))?;
            copy_config(&entry.path(), &target)?;
        } else if info.is_file() {
            fs::copy(entry.path(), &target).map_err(|e| disk::io(&target, e))?;
            File::open(&target)
                .and_then(|f| f.sync_all())
                .map_err(|e| disk::io(&target, e))?;
        } else {
            return Err(Error::Invalid(format!(
                "配置仅允许普通文件及目录: {}",
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

/// Hash archived files including unknown options; optionally exclude only the package table.
/// 散列留档文件及未知选项；可选仅排除包表。
/// # Arguments
///
/// - `config`: Archived configuration directory.
///   留档配置目录。
/// - `exclude_packages`: Whether package changes remain maintenance-compatible.
///   包变化是否保持维护兼容。
pub(super) fn config_hash(config: &Path, exclude_packages: bool) -> Result<String> {
    disk::checked(config)?;
    let mut digest = Sha256::new();
    hash_directory(config, Path::new(""), exclude_packages, &mut digest)?;
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
/// - `exclude_packages`: Omit only the main package array.
///   仅省略主包数组。
/// - `digest`: Accumulated SHA-256 state.
///   累积 SHA-256 状态。
fn hash_directory(
    root: &Path,
    relative: &Path,
    exclude_packages: bool,
    digest: &mut Sha256,
) -> Result<()> {
    for entry in entries(&root.join(relative))? {
        let path = relative.join(entry.file_name());
        let name = path.as_os_str().as_encoded_bytes();
        digest.update((name.len() as u64).to_le_bytes());
        digest.update(name);
        let kind = entry.file_type().map_err(|e| disk::io(&entry.path(), e))?;
        if kind.is_dir() {
            digest.update(b"D");
            hash_directory(root, &path, exclude_packages, digest)?;
        } else if kind.is_file() {
            digest.update(b"F");
            let mut content = Vec::new();
            File::open(entry.path())
                .and_then(|mut f| f.read_to_end(&mut content))
                .map_err(|e| disk::io(&entry.path(), e))?;
            if path == Path::new("build/config.toml") {
                let text =
                    std::str::from_utf8(&content).map_err(|e| Error::Invalid(e.to_string()))?;
                let mut value: toml::Value =
                    toml::from_str(text).map_err(|e| Error::Invalid(e.to_string()))?;
                if exclude_packages {
                    value
                        .as_table_mut()
                        .ok_or_else(|| Error::Invalid("构建配置不是表".into()))?
                        .remove("packages");
                }
                content = toml::to_string(&value)
                    .map_err(|e| Error::Invalid(e.to_string()))?
                    .into_bytes();
            }
            digest.update((content.len() as u64).to_le_bytes());
            digest.update(content);
        } else {
            return Err(Error::Invalid(format!(
                "留档配置含特殊条目: {}",
                entry.path().display()
            )));
        }
    }
    Ok(())
}
