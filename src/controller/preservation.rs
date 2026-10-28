//! Preserve explicitly selected built files and complete directory trees across inheritance.
//! 在继承前后保留显式选择的构建文件及完整目录树。
use super::{BuildBackend, Error, Result, fs as disk, inheritance};
use std::{
    collections::BTreeMap,
    ffi::CString,
    fs,
    os::{
        fd::AsRawFd,
        unix::{
            ffi::OsStrExt,
            fs::{MetadataExt, PermissionsExt},
        },
    },
    path::{Path, PathBuf},
};

fn unmarked(path: &Path) -> &Path {
    Path::new(
        path.as_os_str()
            .as_bytes()
            .strip_suffix(b"/")
            .map(std::ffi::OsStr::from_bytes)
            .unwrap_or(path.as_os_str()),
    )
}

fn directory(path: &Path, required: bool) -> Result<()> {
    disk::checked_entry(path)?;
    match fs::symlink_metadata(path) {
        Ok(info) if info.is_dir() => Ok(()),
        Ok(_) => Err(Error::Invalid(format!(
            "路径必须是真实目录或子卷: {0} / Path must be an actual directory or subvolume: {0}",
            path.display()
        ))),
        Err(error) if !required && error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(disk::io(path, error)),
    }
}

/// Check inherited directory layout without requiring exception entries to exist there.
/// 检查继承来源的目录布局，不要求例外条目在来源中存在。
pub(super) fn validate_source(source: &Path, paths: &[PathBuf]) -> Result<()> {
    if paths.is_empty() {
        return Ok(());
    }
    crate::config::validate_preserve_paths(paths)?;
    directory(unmarked(source), true)?;
    for relative in paths {
        let target = unmarked(source).join(unmarked(relative));
        directory(target.parent().unwrap(), false)?;
        if relative.as_os_str().as_bytes().ends_with(b"/") {
            directory(&target, false)?;
        }
    }
    Ok(())
}

fn target_root(root: &Path, entry: &Path) -> Result<PathBuf> {
    let target = root.join(
        unmarked(entry)
            .strip_prefix("/")
            .map_err(|e| Error::Invalid(e.to_string()))?,
    );
    directory(&target, true)?;
    Ok(target)
}

fn validate_tree(path: &Path) -> Result<()> {
    super::mounts::check_unmounted_tree(path)?;
    let mut pending = vec![path.to_path_buf()];
    while let Some(path) = pending.pop() {
        let info = fs::symlink_metadata(&path).map_err(|e| disk::io(&path, e))?;
        if info.is_dir() {
            for child in fs::read_dir(&path).map_err(|e| disk::io(&path, e))? {
                pending.push(child.map_err(|e| disk::io(&path, e))?.path());
            }
        } else if !info.is_file() && !info.file_type().is_symlink() {
            return Err(Error::Invalid(format!(
                "例外包含不支持的对象: {0} / Preserve entry contains an unsupported object: {0}",
                path.display()
            )));
        }
    }
    Ok(())
}

/// Validate all built exceptions before modifying any inheritance destination.
/// 修改任何继承目标之前校验全部构建例外。
pub(super) fn validate_built(root: &Path, entries: &[(PathBuf, Vec<PathBuf>)]) -> Result<()> {
    for (entry, paths) in entries {
        if paths.is_empty() {
            continue;
        }
        crate::config::validate_preserve_paths(paths)?;
        let base = target_root(root, entry)?;
        for relative in paths {
            let source = base.join(unmarked(relative));
            directory(source.parent().unwrap(), true)?;
            let info = fs::symlink_metadata(&source).map_err(|e| disk::io(&source, e))?;
            crate::config::validate_inheritance_type(relative, info.is_dir())?;
            validate_tree(&source)?;
        }
    }
    Ok(())
}

struct DirectoryMetadata {
    info: fs::Metadata,
    nocow: bool,
    xattrs: Vec<(CString, Vec<u8>)>,
}

impl DirectoryMetadata {
    fn read(path: &Path) -> Result<Self> {
        directory(path, true)?;
        let native =
            CString::new(path.as_os_str().as_bytes()).map_err(|e| Error::Invalid(e.to_string()))?;
        // SAFETY: native is a live NUL-terminated path; null requests the required buffer size.
        // 安全性：native 是有效的 NUL 结尾路径；空指针用于查询缓冲区长度。
        let size = unsafe { libc::llistxattr(native.as_ptr(), std::ptr::null_mut(), 0) };
        if size < 0 {
            return Err(disk::io(path, std::io::Error::last_os_error()));
        }
        let mut names = vec![0u8; size as usize];
        // SAFETY: the allocated names buffer is writable for its entire advertised size.
        // 安全性：names 缓冲区在声明长度内均可写。
        let count =
            unsafe { libc::llistxattr(native.as_ptr(), names.as_mut_ptr().cast(), names.len()) };
        if count < 0 {
            return Err(disk::io(path, std::io::Error::last_os_error()));
        }
        names.truncate(count as usize);
        let mut xattrs = Vec::new();
        for name in names
            .split(|byte| *byte == 0)
            .filter(|name| !name.is_empty())
        {
            let name = CString::new(name).map_err(|e| Error::Invalid(e.to_string()))?;
            // SAFETY: both strings are live and null requests the attribute value size.
            // 安全性：两个字符串均有效，空指针用于查询属性值长度。
            let size =
                unsafe { libc::lgetxattr(native.as_ptr(), name.as_ptr(), std::ptr::null_mut(), 0) };
            if size < 0 {
                return Err(disk::io(path, std::io::Error::last_os_error()));
            }
            let mut value = vec![0u8; size as usize];
            // SAFETY: value has the exact capacity passed to lgetxattr.
            // 安全性：value 的容量与传给 lgetxattr 的长度一致。
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
            xattrs.push((name, value));
        }
        Ok(Self {
            info: fs::symlink_metadata(path).map_err(|e| disk::io(path, e))?,
            nocow: super::nocow::is_nocow(path)?,
            xattrs,
        })
    }

    fn apply(&self, path: &Path) -> Result<()> {
        directory(path, true)?;
        let inherited = Self::read(path)?;
        let file = fs::File::open(path).map_err(|e| disk::io(path, e))?;
        let actual = file.metadata().map_err(|e| disk::io(path, e))?;
        if actual.uid() != self.info.uid() || actual.gid() != self.info.gid() {
            // SAFETY: file is live and the ownership values come from captured metadata.
            // 安全性：文件描述符有效，所有者数值来自已捕获元数据。
            if unsafe { libc::fchown(file.as_raw_fd(), self.info.uid(), self.info.gid()) } < 0 {
                return Err(disk::io(path, std::io::Error::last_os_error()));
            }
        }
        file.set_permissions(self.info.permissions())
            .map_err(|e| disk::io(path, e))?;
        for (name, _) in &inherited.xattrs {
            if self.xattrs.iter().any(|(wanted, _)| wanted == name) {
                continue;
            }
            // SAFETY: file and the captured NUL-terminated attribute name remain live.
            // 安全性：文件及已捕获的 NUL 结尾属性名称均保持有效。
            if unsafe { libc::fremovexattr(file.as_raw_fd(), name.as_ptr()) } < 0 {
                let error = std::io::Error::last_os_error();
                if error.raw_os_error() != Some(libc::ENODATA) {
                    return Err(disk::io(path, error));
                }
            }
        }
        for (name, value) in &self.xattrs {
            // SAFETY: descriptor, attribute name and value remain live throughout the call.
            // 安全性：描述符、属性名称和值在调用期间均保持有效。
            if unsafe {
                libc::fsetxattr(
                    file.as_raw_fd(),
                    name.as_ptr(),
                    value.as_ptr().cast(),
                    value.len(),
                    0,
                )
            } < 0
            {
                return Err(disk::io(path, std::io::Error::last_os_error()));
            }
        }
        file.set_times(
            fs::FileTimes::new()
                .set_accessed(self.info.accessed().map_err(|e| disk::io(path, e))?)
                .set_modified(self.info.modified().map_err(|e| disk::io(path, e))?),
        )
        .map_err(|e| disk::io(path, e))
    }
}

fn snapshot_tree(backend: &impl BuildBackend, source: &Path, target: &Path) -> Result<()> {
    let mut parents = BTreeMap::new();
    for child in backend.nested_subvolumes(source)? {
        if child.as_os_str().is_empty()
            || child
                .components()
                .any(|part| !matches!(part, std::path::Component::Normal(_)))
        {
            return Err(Error::Invalid(
                "例外子卷相对路径无效 / Invalid preserve subvolume relative path".into(),
            ));
        }
        let parent = child.parent().unwrap().to_path_buf();
        if !parents.contains_key(&parent) {
            parents.insert(
                parent.clone(),
                DirectoryMetadata::read(&source.join(parent))?,
            );
        }
    }
    backend.snapshot_recursive(source, target, false)?;
    // Installing child snapshots changes their parents even inside another child subvolume.
    // 安装子卷快照会修改其父目录，即使父目录位于另一嵌套子卷中。
    let mut parents: Vec<_> = parents.into_iter().collect();
    parents.sort_by_key(|(relative, _)| std::cmp::Reverse(relative.components().count()));
    for (relative, metadata) in parents {
        metadata.apply(&target.join(relative))?;
    }
    Ok(())
}

fn copy_tree(backend: &impl BuildBackend, source: &Path, target: &Path) -> Result<()> {
    let info = fs::symlink_metadata(source).map_err(|e| disk::io(source, e))?;
    if info.is_dir() && backend.is_subvolume(source)? {
        return snapshot_tree(backend, source, target);
    }
    if info.is_dir() {
        let mut children = backend.nested_subvolumes(source)?;
        for child in &children {
            if child.as_os_str().is_empty()
                || child
                    .components()
                    .any(|part| !matches!(part, std::path::Component::Normal(_)))
            {
                return Err(Error::Invalid(
                    "例外子卷相对路径无效 / Invalid preserve subvolume relative path".into(),
                ));
            }
        }
        children.sort_by_key(|path| path.components().count());
        let mut restored: Vec<PathBuf> = Vec::new();
        let mut parents = BTreeMap::new();
        for child in &children {
            if restored.iter().any(|parent| child.starts_with(parent)) {
                continue;
            }
            let mut parent = child.parent();
            while let Some(relative) = parent {
                if !parents.contains_key(relative) {
                    parents.insert(
                        relative.to_path_buf(),
                        DirectoryMetadata::read(&source.join(relative))?,
                    );
                }
                parent = relative.parent();
            }
            restored.push(child.clone());
        }
        backend.copy_preserved(source, target)?;
        // Temporarily permit child snapshot installation, then restore all touched directory metadata.
        // 临时允许安装子卷快照，随后恢复全部受影响目录的元数据。
        for (relative, metadata) in &parents {
            let path = target.join(relative);
            fs::set_permissions(
                &path,
                fs::Permissions::from_mode(metadata.info.mode() | 0o700),
            )
            .map_err(|e| disk::io(&path, e))?;
        }
        restored.clear();
        for child in children {
            if restored.iter().any(|parent| child.starts_with(parent)) {
                continue;
            }
            let destination = target.join(&child);
            inheritance::remove_tree(backend, &destination)?;
            snapshot_tree(backend, &source.join(&child), &destination)?;
            restored.push(child);
        }
        let mut parents: Vec<_> = parents.into_iter().collect();
        parents.sort_by_key(|(relative, _)| std::cmp::Reverse(relative.components().count()));
        for (relative, metadata) in parents {
            metadata.apply(&target.join(relative))?;
        }
    } else {
        backend.copy_preserved(source, target)?;
    }
    Ok(())
}

struct SavedEntry {
    entry: PathBuf,
    relative: PathBuf,
    source: PathBuf,
    parents: Vec<(PathBuf, DirectoryMetadata, bool)>,
}

pub(super) struct Saved {
    entries: Vec<SavedEntry>,
}

/// Capture all exceptions in a private owned subvolume outside the destination root.
/// 在目标根之外的私有所属子卷中捕获全部例外。
pub(super) fn capture(
    backend: &impl BuildBackend,
    root: &Path,
    entries: &[(PathBuf, Vec<PathBuf>)],
    storage: &Path,
) -> Result<Saved> {
    validate_built(root, entries)?;
    if entries.iter().all(|(_, paths)| paths.is_empty()) {
        return Ok(Saved {
            entries: Vec::new(),
        });
    }
    if storage.starts_with(root) || root.starts_with(storage) {
        return Err(Error::Invalid(
            "例外暂存区必须位于系统根之外 / Preserve storage must be outside the system root"
                .into(),
        ));
    }
    backend.create_subvolume(storage)?;
    fs::set_permissions(storage, fs::Permissions::from_mode(0o700))
        .map_err(|e| disk::io(storage, e))?;
    let mut saved = Saved {
        entries: Vec::new(),
    };
    for (entry, paths) in entries {
        if paths.is_empty() {
            continue;
        }
        let base = target_root(root, entry)?;
        for relative in paths {
            let mut parents = Vec::new();
            let mut current = PathBuf::new();
            for part in unmarked(relative).parent().unwrap().components() {
                current.push(part);
                let parent = base.join(&current);
                parents.push((
                    current.clone(),
                    DirectoryMetadata::read(&parent)?,
                    backend.is_subvolume(&parent)?,
                ));
            }
            let source = storage.join(saved.entries.len().to_string());
            copy_tree(backend, &base.join(unmarked(relative)), &source)?;
            saved.entries.push(SavedEntry {
                entry: entry.clone(),
                relative: relative.clone(),
                source,
                parents,
            });
        }
    }
    Ok(saved)
}

impl Saved {
    /// Restore one inheritance entry using complete replacement rather than directory merging.
    /// 通过完整替换而非目录合并，还原一个继承项。
    pub(super) fn restore(
        &self,
        backend: &impl BuildBackend,
        root: &Path,
        entry: &Path,
    ) -> Result<()> {
        let selected: Vec<_> = self
            .entries
            .iter()
            .filter(|saved| saved.entry == entry)
            .collect();
        if selected.is_empty() {
            return Ok(());
        }
        let base = target_root(root, entry)?;
        let paths: Vec<_> = selected
            .iter()
            .map(|saved| saved.relative.clone())
            .collect();
        validate_source(&base, &paths)?;
        let mut inherited = BTreeMap::new();
        inherited.insert(base.clone(), DirectoryMetadata::read(&base)?);
        for saved in &selected {
            for (relative, _, _) in &saved.parents {
                let target = base.join(relative);
                match fs::symlink_metadata(&target) {
                    Ok(_) if !inherited.contains_key(&target) => {
                        inherited.insert(target.clone(), DirectoryMetadata::read(&target)?);
                    }
                    Ok(_) => (),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
                    Err(error) => return Err(disk::io(&target, error)),
                }
            }
        }
        let mut created = BTreeMap::new();
        for saved in selected {
            for (relative, metadata, subvolume) in &saved.parents {
                let target = base.join(relative);
                match fs::symlink_metadata(&target) {
                    Ok(_) => directory(&target, true)?,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        if *subvolume {
                            backend.create_subvolume(&target)?;
                        } else {
                            fs::create_dir(&target).map_err(|e| disk::io(&target, e))?;
                        }
                        super::nocow::set_nocow(&target, metadata.nocow)?;
                        created.insert(target, metadata);
                    }
                    Err(error) => return Err(disk::io(&target, error)),
                }
            }
            let target = base.join(unmarked(&saved.relative));
            inheritance::remove_tree(backend, &target)?;
            copy_tree(backend, &saved.source, &target)?;
        }
        let mut metadata: Vec<_> = inherited.iter().collect();
        metadata.extend(created.iter().map(|(path, metadata)| (path, *metadata)));
        metadata.sort_by_key(|(path, _)| std::cmp::Reverse(path.components().count()));
        for (path, metadata) in metadata {
            metadata.apply(path)?;
        }
        Ok(())
    }
}

/// Discard owned temporary data, including nested subvolumes, on success or retry.
/// 在成功或重试时丢弃所属暂存数据，包括内部子卷。
pub(super) fn cleanup(backend: &impl BuildBackend, storage: &Path) -> Result<()> {
    disk::checked(storage)?;
    match fs::symlink_metadata(storage) {
        Ok(_) => {
            backend.delete_subvolume_recursive(storage)?;
            match fs::symlink_metadata(storage) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(error) => Err(disk::io(storage, error)),
                Ok(_) => Err(Error::Invalid(format!(
                    "例外暂存区清理后仍存在: {0} / Preserve storage still exists after cleanup: {0}",
                    storage.display()
                ))),
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(disk::io(storage, error)),
    }
}

/// Scope temporary captures to a single operation and clean partial captures on failure.
/// 将暂存限制于一次操作，并在失败时清理未完成的捕获。
pub(super) fn with_saved<T>(
    backend: &impl BuildBackend,
    root: &Path,
    entries: &[(PathBuf, Vec<PathBuf>)],
    operation: impl FnOnce(&Saved) -> Result<T>,
) -> Result<T> {
    with_saved_from(backend, root, root, entries, operation)
}

/// Read boot exceptions directly from the immutable preparation root, not its working snapshot.
/// 从不可变准备根直接读取启动例外，避免读取其工作快照。
pub(super) fn with_saved_from<T>(
    backend: &impl BuildBackend,
    root: &Path,
    capture_root: &Path,
    entries: &[(PathBuf, Vec<PathBuf>)],
    operation: impl FnOnce(&Saved) -> Result<T>,
) -> Result<T> {
    let mut name = root
        .file_name()
        .ok_or_else(|| {
            Error::Invalid("继承工作根缺少名称 / Inheritance working root has no name".into())
        })?
        .to_os_string();
    name.push("-preserve");
    let storage = root.with_file_name(name);
    cleanup(backend, &storage)?;
    let result =
        capture(backend, capture_root, entries, &storage).and_then(|saved| operation(&saved));
    let cleaned = cleanup(backend, &storage);
    match (result, cleaned) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), Ok(())) | (Ok(_), Err(error)) => Err(error),
        (Err(error), Err(cleanup)) => Err(Error::Invalid(format!(
            "{error}; 暂存清理也失败 / Preserve cleanup also failed: {cleanup}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    const SUBVOLUME: &str = ".preserve-test-subvolume";

    struct Files;
    impl BuildBackend for Files {
        fn create_subvolume(&self, path: &Path) -> Result<()> {
            fs::create_dir(path).map_err(|e| disk::io(path, e))
        }
        fn check_subvolume(&self, path: &Path, readonly: bool) -> Result<()> {
            if !readonly && path.join(SUBVOLUME).is_file() {
                Ok(())
            } else {
                Err(Error::Invalid(
                    "测试普通目录 / Test ordinary directory".into(),
                ))
            }
        }
        fn snapshot(&self, source: &Path, target: &Path, readonly: bool) -> Result<()> {
            assert!(!readonly);
            self.check_subvolume(source, false)?;
            self.copy_preserved(source, target)?;
            let mut children = self.nested_subvolumes(source)?;
            children.sort_by_key(|path| path.components().count());
            let mut omitted: Vec<PathBuf> = Vec::new();
            for child in children {
                if omitted.iter().any(|parent| child.starts_with(parent)) {
                    continue;
                }
                let stub = target.join(&child);
                fs::remove_dir_all(&stub).map_err(|e| disk::io(&stub, e))?;
                fs::create_dir(&stub).map_err(|e| disk::io(&stub, e))?;
                omitted.push(child);
            }
            Ok(())
        }
        fn delete_subvolume(&self, path: &Path) -> Result<()> {
            fs::remove_dir_all(path).map_err(|e| disk::io(path, e))
        }
        fn delete_subvolume_recursive(&self, path: &Path) -> Result<()> {
            self.delete_subvolume(path)
        }
        fn nested_subvolumes(&self, path: &Path) -> Result<Vec<PathBuf>> {
            let mut children = Vec::new();
            let mut pending = vec![path.to_path_buf()];
            while let Some(parent) = pending.pop() {
                for child in fs::read_dir(&parent).map_err(|e| disk::io(&parent, e))? {
                    let child = child.map_err(|e| disk::io(&parent, e))?;
                    if child
                        .file_type()
                        .map_err(|e| disk::io(&child.path(), e))?
                        .is_dir()
                    {
                        let child = child.path();
                        if child.join(SUBVOLUME).is_file() {
                            children.push(child.strip_prefix(path).unwrap().to_path_buf());
                        }
                        pending.push(child);
                    }
                }
            }
            Ok(children)
        }
        fn invoke(
            &self,
            _plugin: &super::super::plugins::LoadedPlugin,
            _operation: &str,
            _args: &[String],
            _root: &Path,
            _context: &super::super::BuildContext,
        ) -> Result<String> {
            unreachable!()
        }
    }

    fn set_attribute(path: &Path, value: &[u8]) {
        let native = CString::new(path.as_os_str().as_bytes()).unwrap();
        let name = CString::new("user.rstarsleep-preserve").unwrap();
        // SAFETY: both strings and the byte buffer live for the complete call.
        // 安全性：两个字符串和字节缓冲区在整个调用期间均保持有效。
        assert_eq!(
            unsafe {
                libc::lsetxattr(
                    native.as_ptr(),
                    name.as_ptr(),
                    value.as_ptr().cast(),
                    value.len(),
                    0,
                )
            },
            0
        );
    }

    /// Preserve bytes, hardlinks, empty directories and metadata without merging old contents.
    /// 保留数据、硬链接、空目录及元数据，不合并旧内容。
    #[test]
    fn full_tree_and_missing_parent_metadata_survive_replacement() {
        let tree = tempfile::tempdir().unwrap();
        let root = tree.path().join("root");
        let base = root.join("data");
        fs::create_dir_all(base.join("keep/empty")).unwrap();
        fs::create_dir_all(base.join("missing/deep")).unwrap();
        fs::create_dir_all(base.join("existing")).unwrap();
        fs::write(base.join("file"), "built-file").unwrap();
        fs::write(base.join("keep/a"), "hardlinked").unwrap();
        fs::hard_link(base.join("keep/a"), base.join("keep/b")).unwrap();
        symlink("/missing/target", base.join("keep/link")).unwrap();
        symlink("../missing-target", base.join("link")).unwrap();
        fs::write(base.join("missing/deep/file"), "built-new").unwrap();
        fs::write(base.join("existing/file"), "built-existing").unwrap();
        fs::set_permissions(base.join("missing"), fs::Permissions::from_mode(0o750)).unwrap();
        set_attribute(&base.join("missing"), b"built-parent");
        set_attribute(&base.join("keep/a"), b"built-file");
        set_attribute(&base.join("keep"), b"built-directory");
        let parent = DirectoryMetadata::read(&base.join("missing")).unwrap();
        let keep = DirectoryMetadata::read(&base.join("keep")).unwrap();
        let paths = [
            "file",
            "keep/",
            "link",
            "missing/deep/file",
            "existing/file",
        ]
        .map(PathBuf::from)
        .to_vec();
        with_saved(&Files, &root, &[("/data/".into(), paths)], |saved| {
            fs::remove_dir_all(&base).unwrap();
            fs::create_dir_all(base.join("file/old-directory")).unwrap();
            fs::create_dir_all(base.join("keep")).unwrap();
            fs::write(base.join("keep/old-extra"), "remove").unwrap();
            fs::create_dir_all(base.join("existing")).unwrap();
            fs::set_permissions(base.join("existing"), fs::Permissions::from_mode(0o755)).unwrap();
            set_attribute(&base.join("existing"), b"source-parent");
            saved.restore(&Files, &root, Path::new("/data/"))
        })
        .unwrap();
        assert_eq!(fs::read_to_string(base.join("file")).unwrap(), "built-file");
        assert!(!base.join("keep/old-extra").exists());
        assert!(base.join("keep/empty").is_dir());
        assert_eq!(
            fs::metadata(base.join("keep/a")).unwrap().ino(),
            fs::metadata(base.join("keep/b")).unwrap().ino()
        );
        assert_eq!(
            fs::read_link(base.join("keep/link")).unwrap(),
            Path::new("/missing/target")
        );
        assert_eq!(
            fs::read_link(base.join("link")).unwrap(),
            Path::new("../missing-target")
        );
        let restored = DirectoryMetadata::read(&base.join("missing")).unwrap();
        assert_eq!(restored.info.mode(), parent.info.mode());
        assert_eq!(
            restored.info.modified().unwrap(),
            parent.info.modified().unwrap()
        );
        assert_eq!(restored.xattrs, parent.xattrs);
        assert_eq!(
            DirectoryMetadata::read(&base.join("keep")).unwrap().xattrs,
            keep.xattrs
        );
        assert_eq!(
            DirectoryMetadata::read(&base.join("existing"))
                .unwrap()
                .xattrs[0]
                .1,
            b"source-parent"
        );
        assert!(!tree.path().join("root-preserve").exists());
    }

    /// A trailing slash never turns a link into an accepted directory.
    /// 末尾斜杠不能将链接变成可接受的目录。
    #[test]
    fn literal_directory_checks_reject_links_files_and_non_directory_parents() {
        let tree = tempfile::tempdir().unwrap();
        let base = tree.path();
        fs::create_dir(base.join("real")).unwrap();
        symlink("real", base.join("linked")).unwrap();
        symlink("missing", base.join("broken")).unwrap();
        fs::write(base.join("file"), "data").unwrap();
        for relative in ["linked/", "broken/", "file/", "linked/value", "file/value"] {
            assert!(
                validate_source(base, &[relative.into()]).is_err(),
                "{relative}"
            );
        }
        assert!(validate_source(base, &["missing/deep/file".into()]).is_ok());
        assert!(validate_source(base, &["linked".into()]).is_ok());
    }

    /// Replacing omitted nested snapshots must not change preserved parent timestamps.
    /// 替换被省略的嵌套快照不得改变保留父目录的时间戳。
    #[test]
    fn nested_snapshot_installation_preserves_directory_metadata() {
        for subvolume in [false, true] {
            let tree = tempfile::tempdir().unwrap();
            let source = tree.path().join("source");
            fs::create_dir_all(source.join("ordinary/volume/deep/nested")).unwrap();
            for path in ["ordinary/volume", "ordinary/volume/deep/nested"] {
                fs::write(source.join(path).join(SUBVOLUME), "writable").unwrap();
            }
            if subvolume {
                fs::write(source.join(SUBVOLUME), "writable").unwrap();
            }
            fs::write(source.join("ordinary/volume/deep/nested/file"), "built").unwrap();
            let paths = ["", "ordinary", "ordinary/volume", "ordinary/volume/deep"];
            let time = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_234_567);
            for path in paths {
                let directory = source.join(path);
                fs::File::open(&directory)
                    .unwrap()
                    .set_times(fs::FileTimes::new().set_modified(time))
                    .unwrap();
                set_attribute(&directory, b"built-parent");
            }
            let target = tree.path().join("target");
            copy_tree(&Files, &source, &target).unwrap();
            for path in paths {
                let actual = DirectoryMetadata::read(&target.join(path)).unwrap();
                assert_eq!(actual.info.modified().unwrap(), time, "{subvolume}: {path}");
                assert_eq!(actual.xattrs[0].1, b"built-parent");
            }
            assert_eq!(
                fs::read_to_string(target.join("ordinary/volume/deep/nested/file")).unwrap(),
                "built"
            );
        }
    }

    /// Built exceptions must exist, match their declaration and contain only supported objects.
    /// 构建例外必须存在、符合声明，且只能包含支持的对象。
    #[test]
    fn built_errors_precede_capture_and_inheritance() {
        let tree = tempfile::tempdir().unwrap();
        let root = tree.path().join("root");
        fs::create_dir_all(root.join("data/directory")).unwrap();
        fs::write(root.join("data/file"), "built").unwrap();
        symlink("directory", root.join("data/link")).unwrap();
        let _socket =
            std::os::unix::net::UnixListener::bind(root.join("data/directory/socket")).unwrap();
        for relative in ["missing", "directory", "file/", "link/", "directory/"] {
            let entries = [("/data/".into(), vec![PathBuf::from(relative)])];
            assert!(
                with_saved::<()>(&Files, &root, &entries, |_| panic!(
                    "不应开始继承 / Inheritance must not begin"
                ))
                .is_err(),
                "{relative}"
            );
            assert!(!tree.path().join("root-preserve").exists());
        }
    }

    /// Run explicitly on a writable Btrfs directory; no actual subvolumes are allocated.
    /// 在可写 Btrfs 目录显式执行，不分配真实子卷。
    #[test]
    #[ignore = "需要可写 Btrfs 工作区 / Requires a writable Btrfs workspace"]
    fn native_nocow_full_copy_preserves_data_and_attributes() {
        let tree = tempfile::Builder::new()
            .prefix(".preserve-test-")
            .tempdir_in(env!("CARGO_MANIFEST_DIR"))
            .unwrap();
        let source = tree.path().join("source");
        fs::write(&source, "").unwrap();
        super::super::nocow::set_nocow(&source, true).unwrap();
        fs::write(&source, "nocow-built").unwrap();
        let target = tree.path().join("target");
        Files.copy_preserved(&source, &target).unwrap();
        assert_eq!(fs::read_to_string(&target).unwrap(), "nocow-built");
        assert!(super::super::nocow::is_nocow(&target).unwrap());
        assert_ne!(
            fs::metadata(&source).unwrap().ino(),
            fs::metadata(&target).unwrap().ino()
        );
    }
}
