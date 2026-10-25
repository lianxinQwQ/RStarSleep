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

/// Archive the checked snapshot across filesystems and publish only a complete configuration.
/// 跨文件系统归档已经检查的副本，仅发布完整配置。
pub(super) fn publish_config(source: &Path, directory: &Path) -> Result<()> {
    let staging = tempfile::Builder::new()
        .prefix(".config-pending-")
        .tempdir_in(directory)
        .map_err(|error| disk::io(directory, error))?;
    let temporary = staging.path().join("config");
    let destination = directory.join("config");
    let result = (|| {
        fs::create_dir(&temporary).map_err(|error| disk::io(&temporary, error))?;
        copy_config(source, &temporary)?;
        fs::rename(&temporary, &destination).map_err(|error| disk::io(&destination, error))?;
        disk::sync(directory)
    })();
    let path = staging.path().to_owned();
    let cleanup = staging.close().map_err(|error| disk::io(&path, error));
    match (result, cleanup) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(error), Err(cleanup)) => Err(Error::Invalid(format!(
            "{error}; 配置暂存目录清理失败: {cleanup} / {error}; configuration staging cleanup failed: {cleanup}"
        ))),
    }
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

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn incomplete_configuration_copies_are_removed_without_publishing() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source");
        let version = directory.path().join("version");
        fs::create_dir(&source).unwrap();
        fs::create_dir(&version).unwrap();
        fs::write(source.join("a.conf"), b"copied before failure").unwrap();
        std::os::unix::fs::symlink("missing", source.join("z.conf")).unwrap();
        let error = publish_config(&source, &version).unwrap_err().to_string();
        assert!(
            error.contains("Configuration permits only regular files"),
            "{error}"
        );
        assert_eq!(fs::read_dir(&version).unwrap().count(), 0);
        assert_eq!(
            fs::read(source.join("a.conf")).unwrap(),
            b"copied before failure"
        );
    }
}
