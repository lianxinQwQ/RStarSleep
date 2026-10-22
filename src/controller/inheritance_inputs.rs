//! Freeze inherited inputs once so cache validation and execution observe the same data.
//! 只冻结一次继承输入，确保缓存校验与实际执行读取相同数据。
use super::{BuildBackend, BuildContext, Error, Result, fs as disk, inheritance};
use crate::config::{InheritanceConfig, NocowHandling, validate_inheritance_type};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    ffi::CString,
    fs,
    io::Read,
    os::unix::{
        ffi::OsStrExt,
        fs::{FileTypeExt, MetadataExt},
    },
    path::{Path, PathBuf},
};

/// Sealed sources and the fingerprint of their inherited output.
/// 已封存来源及其继承产物的指纹。
pub(super) struct FrozenBuild {
    /// Frozen source and declared guest destination pairs.
    /// 冻结来源与声明容器目标的配对。
    entries: Vec<(PathBuf, PathBuf)>,
    /// Preserve the configured outer-directory subvolume policy.
    /// 保留配置中的外层目录子卷策略。
    directory_subvolume: bool,
    nocow_handling: NocowHandling,
    /// Complete captured input fingerprint.
    /// 完整捕获输入指纹。
    pub(super) sha256: String,
}

impl FrozenBuild {
    /// Inherit the same immutable sources used to compute the cache fingerprint.
    /// 继承与缓存指纹计算相同的不可变来源。
    /// # Arguments
    /// - `self`: Sealed inheritance sources. 已封存继承来源。
    /// - `backend`: Required CoW copy and snapshot operations. 必需的 CoW 复制与快照操作。
    /// - `root`: Writable script-stage root. 可写脚本阶段根。
    pub(super) fn apply(&self, backend: &impl BuildBackend, root: &Path) -> Result<bool> {
        for (source, target) in &self.entries {
            inheritance::inherit_path(
                backend,
                source,
                &disk::guest_path(root, target)?,
                self.directory_subvolume,
                self.nocow_handling,
            )?;
        }
        Ok(!self.entries.is_empty())
    }
}

/// Capture only declared paths in an owned CoW subvolume, then seal every nested subvolume.
/// 只将声明路径捕获到所属 CoW 子卷中，再封存每个内部子卷。
/// # Arguments
/// - `backend`: Required CoW and read-only operations. 必需的 CoW 及只读操作。
/// - `context`: Frozen system configuration. 冻结系统配置。
/// - `scratch`: Existing owned scratch directory. 已有所属临时目录。
/// - `system`: Managed-system directory excluded from live sources. 从运行来源排除的被管理系统目录。
/// - `source_root`: Selected source system root. 选定来源系统根。
/// - `owned`: Cleanup roots registered before any mutations. 在修改前登记的清理根。
pub(super) fn freeze(
    backend: &impl BuildBackend,
    context: &BuildContext,
    scratch: &Path,
    system: &Path,
    source_root: &Path,
    owned: &mut Vec<PathBuf>,
) -> Result<FrozenBuild> {
    let parameters = crate::config::read_plugin_parameters_at(&context.config)?;
    let config = InheritanceConfig::read(&parameters)?;
    let manager = system.parent().and_then(Path::parent).ok_or_else(|| {
        Error::Invalid("继承缺少管理根 / Inheritance has no management root".into())
    })?;
    let _migration_lock = if config.nocow_handling == NocowHandling::MoveSnapshot {
        Some(super::records::lock_inheritance(manager)?)
    } else {
        None
    };
    let mut digest = Sha256::new();
    field(&mut digest, b"rstarsleep-inherited-inputs-v1");
    let mut entries = Vec::new();
    if !config.copy_on_write_build.is_empty() {
        super::progress::message(
            &context.system_id,
            "冻结并校验构建继承内容摘要",
            "Freeze and fingerprint build-time inheritance inputs",
        );
        let container = scratch.join("cow-inputs");
        owned.push(container.clone());
        backend.create_subvolume(&container)?;
        for (index, target) in config.copy_on_write_build.iter().enumerate() {
            let unmarked = Path::new(target.to_str().unwrap().trim_end_matches('/'));
            let source = disk::inheritance_source_path(source_root, unmarked)?;
            let directory = inheritance::check_source(&source, system)?;
            validate_inheritance_type(target, directory)?;
            if directory && config.nocow_handling == NocowHandling::MoveSnapshot {
                super::inheritance_migration::prepare(backend, &source, manager, "build", None)?;
            }
            let frozen = container.join(index.to_string());
            inheritance::inherit_path(
                backend,
                &source,
                &frozen,
                config.directory_subvolume,
                config.nocow_handling,
            )?;
            entries.push((frozen, target.clone()));
        }
        let mut children = backend.nested_subvolumes(&container)?;
        children.sort_by_key(|p| p.components().count());
        for child in children.iter().rev() {
            backend.set_readonly(&container.join(child), true)?;
        }
        backend.set_readonly(&container, true)?;
        backend.check_subvolume(&container, true)?;
        for child in &children {
            backend.check_subvolume(&container.join(child), true)?;
        }
        for (source, target) in &entries {
            field(&mut digest, target.as_os_str().as_bytes());
            field(&mut digest, tree_hash(backend, source)?.as_bytes());
        }
    }
    Ok(FrozenBuild {
        entries,
        directory_subvolume: config.directory_subvolume,
        nocow_handling: config.nocow_handling,
        sha256: format!("{:x}", digest.finalize()),
    })
}

/// Include preserved metadata and stable hardlink relationships, never raw inode identities.
/// 包含所保留的元数据及稳定硬链接关系，不包含原始 inode 标识。
/// # Arguments
/// - `backend`: Subvolume topology inspector. 子卷结构检查器。
/// - `root`: Sealed file or directory. 已封存文件或目录。
fn tree_hash(backend: &impl BuildBackend, root: &Path) -> Result<String> {
    hash_tree(backend, root, false)
}

pub(super) fn migration_hash(backend: &impl BuildBackend, root: &Path) -> Result<String> {
    hash_tree(backend, root, true)
}

fn hash_tree(
    backend: &impl BuildBackend,
    root: &Path,
    ignore_root_subvolume: bool,
) -> Result<String> {
    let mut digest = Sha256::new();
    let mut links = BTreeMap::new();
    hash(
        backend,
        root,
        Path::new(""),
        &mut links,
        &mut digest,
        ignore_root_subvolume,
    )?;
    Ok(format!("{:x}", digest.finalize()))
}

/// Append a length-delimited fingerprint field.
/// 追加带长度边界的指纹字段。
/// # Arguments
/// - `digest`: Accumulated hash state. 累积摘要状态。
/// - `value`: Field bytes. 字段字节。
fn field(digest: &mut Sha256, value: &[u8]) {
    digest.update((value.len() as u64).to_le_bytes());
    digest.update(value);
}

/// Hash one entry without following links or opening special files.
/// 散列单个条目，不跟随链接或打开特殊文件。
/// # Arguments
/// - `backend`: Subvolume topology inspector. 子卷结构检查器。
/// - `root`: Sealed tree or file root. 已封存树或文件根。
/// - `relative`: Current entry relative to root. 当前条目相对根的路径。
/// - `links`: Canonical first path for each inode within this source. 此来源内各 inode 的首个规范路径。
/// - `digest`: Accumulated hash state. 累积摘要状态。
fn hash(
    backend: &impl BuildBackend,
    root: &Path,
    relative: &Path,
    links: &mut BTreeMap<(u64, u64), PathBuf>,
    digest: &mut Sha256,
    ignore_root_subvolume: bool,
) -> Result<()> {
    let path = if relative.as_os_str().is_empty() {
        root.to_path_buf()
    } else {
        root.join(relative)
    };
    let info = fs::symlink_metadata(&path).map_err(|e| disk::io(&path, e))?;
    field(digest, relative.as_os_str().as_bytes());
    digest.update(info.mode().to_le_bytes());
    digest.update(info.uid().to_le_bytes());
    digest.update(info.gid().to_le_bytes());
    if ignore_root_subvolume {
        // Migration must preserve modification times; cache reuse ignores touches.
        digest.update(info.mtime().to_le_bytes());
        digest.update(info.mtime_nsec().to_le_bytes());
    }
    // Timestamps are deliberately excluded from cache reuse: touching a file does not change its inherited content.
    // 有意排除时间戳：仅 touch 文件不会改变所继承内容。
    hash_xattrs(&path, digest)?;
    if info.is_file() || info.is_dir() {
        digest.update([u8::from(super::nocow::is_nocow(&path)?)]);
    }
    let kind = info.file_type();
    if kind.is_dir() {
        digest.update([u8::from(
            !(ignore_root_subvolume && relative.as_os_str().is_empty())
                && backend.is_subvolume(&path)?,
        )]);
        let mut entries = fs::read_dir(&path)
            .map_err(|e| disk::io(&path, e))?
            .collect::<std::io::Result<Vec<_>>>()
            .map_err(|e| disk::io(&path, e))?;
        entries.sort_by_key(|e| e.file_name());
        digest.update((entries.len() as u64).to_le_bytes());
        for entry in entries {
            hash(
                backend,
                root,
                &relative.join(entry.file_name()),
                links,
                digest,
                ignore_root_subvolume,
            )?;
        }
    } else if kind.is_symlink() {
        field(
            digest,
            fs::read_link(&path)
                .map_err(|e| disk::io(&path, e))?
                .as_os_str()
                .as_bytes(),
        );
    } else if kind.is_file() {
        let key = (info.dev(), info.ino());
        if let Some(first) = links.get(&key) {
            digest.update(b"hardlink");
            field(digest, first.as_os_str().as_bytes());
        } else {
            links.insert(key, relative.into());
            digest.update(b"contents");
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
        }
    } else if kind.is_block_device() || kind.is_char_device() || kind.is_fifo() || kind.is_socket()
    {
        digest.update(info.rdev().to_le_bytes());
    } else {
        return Err(Error::Invalid(format!(
            "不能缓存继承条目: {0} / Cannot cache inheritance entry: {0}",
            path.display()
        )));
    }
    Ok(())
}

/// Use no-follow xattr calls so a symlink's target is never read as metadata.
/// 使用不跟随链接的 xattr 调用，避免读取符号链接目标的元数据。
/// # Arguments
/// - `path`: Sealed entry, possibly a symbolic link. 已封存条目，可为符号链接。
/// - `digest`: Accumulated hash state. 累积摘要状态。
fn hash_xattrs(path: &Path, digest: &mut Sha256) -> Result<()> {
    let native = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| Error::Invalid("继承路径包含 NUL / Inheritance path contains NUL".into()))?;
    // SAFETY: native is NUL-terminated and the first call passes no output buffer.
    let size = unsafe { libc::llistxattr(native.as_ptr(), std::ptr::null_mut(), 0) };
    if size < 0 {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ENOTSUP) {
            digest.update(0u64.to_le_bytes());
            return Ok(());
        }
        return Err(disk::io(path, error));
    }
    let mut names = vec![0u8; size as usize];
    // SAFETY: names has the length supplied to llistxattr.
    let count =
        unsafe { libc::llistxattr(native.as_ptr(), names.as_mut_ptr().cast(), names.len()) };
    if count < 0 {
        return Err(disk::io(path, std::io::Error::last_os_error()));
    }
    names.truncate(count as usize);
    let mut names: Vec<_> = names.split(|b| *b == 0).filter(|n| !n.is_empty()).collect();
    names.sort();
    digest.update((names.len() as u64).to_le_bytes());
    for name in names {
        field(digest, name);
        let name = CString::new(name).unwrap();
        // SAFETY: both strings are NUL-terminated; this only queries required size.
        let size =
            unsafe { libc::lgetxattr(native.as_ptr(), name.as_ptr(), std::ptr::null_mut(), 0) };
        if size < 0 {
            return Err(disk::io(path, std::io::Error::last_os_error()));
        }
        let mut value = vec![0u8; size as usize];
        // SAFETY: value owns the entire writable buffer supplied to lgetxattr.
        let count = unsafe {
            libc::lgetxattr(
                native.as_ptr(),
                name.as_ptr(),
                value.as_mut_ptr().cast(),
                value.len(),
            )
        };
        if count < 0 {
            return Err(disk::io(path, std::io::Error::last_os_error()));
        }
        value.truncate(count as usize);
        field(digest, &value);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        cell::Cell,
        os::unix::fs::{PermissionsExt, symlink},
    };

    struct MetadataBackend {
        subvolume: Cell<bool>,
    }
    impl BuildBackend for MetadataBackend {
        fn create_subvolume(&self, _: &Path) -> Result<()> {
            unreachable!()
        }
        fn check_subvolume(&self, _: &Path, _: bool) -> Result<()> {
            unreachable!()
        }
        fn snapshot(&self, _: &Path, _: &Path, _: bool) -> Result<()> {
            unreachable!()
        }
        fn delete_subvolume(&self, _: &Path) -> Result<()> {
            unreachable!()
        }
        fn invoke(
            &self,
            _: &super::super::plugins::LoadedPlugin,
            _: &str,
            _: &[String],
            _: &Path,
            _: &BuildContext,
        ) -> Result<String> {
            unreachable!()
        }
        fn is_subvolume(&self, _: &Path) -> Result<bool> {
            Ok(self.subvolume.get())
        }
    }

    /// Verify semantic changes invalidate, while timestamp-only changes remain reusable.
    /// 验证语义变化失效，而仅时间戳变化仍可复用。
    /// # Arguments
    /// No parameters. 无参数。
    #[test]
    fn fingerprints_contents_links_permissions_xattrs_and_topology_without_timestamps() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path();
        let backend = MetadataBackend {
            subvolume: Cell::new(false),
        };
        let file = root.join("file");
        fs::write(&file, "first").unwrap();
        symlink("file", root.join("link")).unwrap();
        let original = tree_hash(&backend, root).unwrap();
        // File times can change while bytes stay identical; only reading also cannot invalidate.
        let times = fs::FileTimes::new().set_modified(std::time::UNIX_EPOCH);
        fs::File::options()
            .write(true)
            .open(&file)
            .unwrap()
            .set_times(times)
            .unwrap();
        assert_eq!(tree_hash(&backend, root).unwrap(), original);
        fs::write(&file, "other").unwrap();
        assert_ne!(tree_hash(&backend, root).unwrap(), original);
        fs::write(&file, "first").unwrap();
        assert_eq!(tree_hash(&backend, root).unwrap(), original);
        fs::set_permissions(&file, fs::Permissions::from_mode(0o700)).unwrap();
        let mode = tree_hash(&backend, root).unwrap();
        assert_ne!(mode, original);
        fs::remove_file(root.join("link")).unwrap();
        symlink("elsewhere", root.join("link")).unwrap();
        let link = tree_hash(&backend, root).unwrap();
        assert_ne!(link, mode);
        fs::create_dir(root.join("empty")).unwrap();
        let added = tree_hash(&backend, root).unwrap();
        assert_ne!(added, link);
        backend.subvolume.set(true);
        assert_ne!(tree_hash(&backend, root).unwrap(), added);
        backend.subvolume.set(false);
        let native = CString::new(file.as_os_str().as_bytes()).unwrap();
        let name = CString::new("user.rstarsleep-test").unwrap();
        // SAFETY: both strings and the four-byte value remain alive for this call.
        let status = unsafe {
            libc::lsetxattr(
                native.as_ptr(),
                name.as_ptr(),
                b"test".as_ptr().cast(),
                4,
                0,
            )
        };
        assert_eq!(status, 0, "{}", std::io::Error::last_os_error());
        assert_ne!(tree_hash(&backend, root).unwrap(), added);
    }

    /// Compare hardlink topology across separately allocated inode numbers.
    /// 在分别分配的 inode 编号之间比较硬链接结构。
    /// # Arguments
    /// No parameters. 无参数。
    #[test]
    fn hardlinks_are_compared_by_relationship_instead_of_inode_number() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let backend = MetadataBackend {
            subvolume: Cell::new(false),
        };
        for root in [first.path(), second.path()] {
            fs::write(root.join("a"), "bytes").unwrap();
            fs::hard_link(root.join("a"), root.join("b")).unwrap();
        }
        let expected = tree_hash(&backend, first.path()).unwrap();
        assert_eq!(expected, tree_hash(&backend, second.path()).unwrap());
        fs::remove_file(second.path().join("b")).unwrap();
        fs::write(second.path().join("b"), "bytes").unwrap();
        assert_ne!(expected, tree_hash(&backend, second.path()).unwrap());
    }
}
