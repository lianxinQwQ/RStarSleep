//! Own frozen resource sessions and normalize their direct or single-wrapper roots.
//! 持有冻结资源会话，并归一化直接根或单层包装根。
use super::{archive, copy_tree, tree_hash};
use crate::controller::{Error, Result, fs as disk};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{self, Read, Seek, Write},
    path::{Component, Path, PathBuf},
};

/// An owned resource snapshot that is removed when its session ends.
/// 会话结束时自动删除的所属资源副本。
pub(in crate::controller) struct Prepared {
    /// Normalized frozen resource root.
    /// 归一化冻结资源根。
    pub root: PathBuf,
    /// Exact archive digest or deterministic directory fingerprint.
    /// 准确归档摘要或确定性目录指纹。
    pub sha256: String,
    /// Session lifetime owning all extracted/copied resources.
    /// 持有所解包或复制资源的会话生命周期。
    pub temporary: tempfile::TempDir,
}

/// Copy or extract a resource and select its direct or single-wrapper root.
/// 复制或解包资源，并选择直接根或单层包装根。
/// # Arguments
/// - `path`: Source archive or ordinary directory.
///   源归档或普通目录。
/// - `entrypoint`: Required root-level regular file.
///   必需的根级普通文件。
/// - `name`: Private normalized directory name.
///   私有归一化目录名。
pub(in crate::controller) fn prepare(
    path: &Path,
    entrypoint: &str,
    name: &str,
) -> Result<Prepared> {
    let info = fs::symlink_metadata(path).map_err(|e| disk::io(path, e))?;
    if !(info.is_file() || info.is_dir()) {
        return Err(Error::Invalid(format!(
            "资源必须是普通文件或目录: {0} / Resource must be a regular file or directory: {0}",
            path.display()
        )));
    }
    let temporary = tempfile::Builder::new()
        .prefix("rstarsleep-resource-")
        .tempdir()
        .map_err(|e| disk::io(&std::env::temp_dir(), e))?;
    let staging = temporary.path().join("unpacked");
    fs::create_dir(&staging).map_err(|e| disk::io(&staging, e))?;
    let sha256 = if info.is_dir() {
        copy_tree(path, &staging)?;
        tree_hash(&staging)?
    } else {
        let frozen = temporary.path().join("archive");
        let mut source = fs::File::open(path).map_err(|e| disk::io(path, e))?;
        let mut file = fs::File::options()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&frozen)
            .map_err(|e| disk::io(&frozen, e))?;
        let mut digest = Sha256::new();
        let mut buffer = [0u8; 65536];
        loop {
            let count = source.read(&mut buffer).map_err(|e| disk::io(path, e))?;
            if count == 0 {
                break;
            }
            digest.update(&buffer[..count]);
            file.write_all(&buffer[..count])
                .map_err(|e| disk::io(&frozen, e))?;
        }
        file.rewind().map_err(|e| disk::io(&frozen, e))?;
        archive::unpack(archive::reader(file, path)?, &staging, path)?;
        format!("{:x}", digest.finalize())
    };
    let source = normalize_root(&staging, entrypoint)?;
    let root = temporary.path().join(name);
    fs::rename(&source, &root).map_err(|e| disk::io(&source, e))?;
    Ok(Prepared {
        root,
        sha256,
        temporary,
    })
}

/// Require a regular root entry, optionally lifting exactly one wrapper directory.
/// 要求普通根入口，可提升恰好一个包装目录。
/// # Arguments
/// - `staging`: Complete resource tree, including hidden entries.
///   包含隐藏条目的完整资源树。
/// - `entrypoint`: Fixed required filename.
///   固定的必需文件名。
fn normalize_root(staging: &Path, entrypoint: &str) -> Result<PathBuf> {
    match fs::symlink_metadata(staging.join(entrypoint)) {
        Ok(info) if info.is_file() => return Ok(staging.into()),
        Ok(_) => {
            return Err(Error::Invalid(format!(
                "{entrypoint} 必须是根中的普通文件 / {entrypoint} must be a regular file in the root"
            )));
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => (),
        Err(e) => return Err(disk::io(&staging.join(entrypoint), e)),
    }
    let entries = fs::read_dir(staging)
        .map_err(|e| disk::io(staging, e))?
        .collect::<io::Result<Vec<_>>>()
        .map_err(|e| disk::io(staging, e))?;
    if entries.len() != 1
        || !entries[0]
            .file_type()
            .map_err(|e| disk::io(staging, e))?
            .is_dir()
    {
        return Err(Error::Invalid(format!(
            "资源根缺少 {entrypoint}，且顶层不是单个包装目录 / Resource root has no {entrypoint}, and its top level is not a single wrapper directory"
        )));
    }
    let wrapper = entries[0].path();
    let entry = wrapper.join(entrypoint);
    if !fs::symlink_metadata(&entry)
        .map_err(|e| disk::io(&entry, e))?
        .is_file()
    {
        return Err(Error::Invalid(format!(
            "包装目录内必须直接包含普通 {entrypoint} 文件 / Wrapper directory must directly contain a regular {entrypoint} file"
        )));
    }
    Ok(wrapper)
}

/// Reject absolute paths, parent traversal, and empty resource/link names.
/// 拒绝绝对路径、父级跳转及空资源或链接名称。
/// # Arguments
/// - `path`: Entry or link target constrained within its resource root.
///   限于资源根内的条目或链接目标。
pub(super) fn relative_path(path: &Path) -> Result<()> {
    if path.as_os_str().is_empty()
        || path.components().any(|c| {
            matches!(
                c,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(Error::Invalid(format!(
            "资源路径必须位于资源根内: {0} / Resource path must remain within the resource root: {0}",
            path.display()
        )));
    }
    Ok(())
}
