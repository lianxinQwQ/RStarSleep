//! Batch-local package-file candidates tied to immutable software-stage caches.
//! 与软件阶段只读缓存绑定的批次内包文件候选项。
mod storage;
use super::{BuildBackend, Error, Result, fs as disk, plugins::LoadedPlugin, progress};
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::CString,
    fs::{self, File},
    io::Read,
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::fs::OpenOptionsExt,
    },
    path::{Path, PathBuf},
};
pub use storage::DedupOutcome;
pub(super) use storage::share;

pub(super) const INDEX: &str = "packages-files.tsv";
pub(super) type Index = BTreeSet<(String, String)>;

/// One successful, enabled system retained by the batch's existing build lock.
/// 由批次现有构建锁保护的已成功且开启去重的系统。
pub(super) struct Source {
    root: PathBuf,
    plugin: String,
    architecture: String,
    packages: BTreeMap<String, BTreeSet<String>>,
}

impl Source {
    pub(super) fn load(
        backend: &impl BuildBackend,
        root: PathBuf,
        plugin: &LoadedPlugin,
        architecture: &str,
    ) -> Result<Self> {
        backend.check_subvolume(&root, true)?;
        let path = root.parent().unwrap().join(INDEX);
        let index = read_index(&path)?.ok_or_else(|| {
            Error::Invalid(format!(
                "缺少二阶段包文件索引: {0} / Missing phase-two package file index: {0}",
                path.display()
            ))
        })?;
        let mut packages = BTreeMap::<String, BTreeSet<String>>::new();
        for (owner, path) in index {
            packages.entry(owner).or_default().insert(path);
        }
        Ok(Self {
            root,
            plugin: plugin.metadata.id.clone(),
            architecture: architecture.into(),
            packages,
        })
    }
}

/// Read a bounded ordinary index; a missing legacy index is distinguishable from corruption.
/// 读取有界普通索引，区分旧缓存缺失索引和损坏索引。
pub(super) fn read_index(path: &Path) -> Result<Option<Index>> {
    disk::checked(path)?;
    let file = match fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(disk::io(path, error)),
    };
    if !file.metadata().map_err(|e| disk::io(path, e))?.is_file() {
        return Err(Error::Invalid(format!(
            "包文件索引必须为普通文件: {0} / Package file index must be a regular file: {0}",
            path.display()
        )));
    }
    let mut bytes = Vec::new();
    file.take(crate::subsystem::package_files::MAX_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| disk::io(path, e))?;
    parse(&bytes).map(Some)
}

pub(super) fn parse(bytes: &[u8]) -> Result<Index> {
    crate::subsystem::package_files::parse(bytes).map_err(|e| Error::Invalid(e.to_string()))
}

pub(super) fn write_index(path: &Path, index: &Index) -> Result<()> {
    disk::directory(path.parent().unwrap())?;
    let bytes: String = index
        .iter()
        .map(|(owner, path)| format!("{owner}\t{path}\n"))
        .collect();
    crate::subsystem::atomic_write(path, bytes.as_bytes(), 0o600)
        .map_err(|e| Error::Invalid(e.to_string()))
}

fn open_root(root: &Path) -> Result<File> {
    disk::checked(root)?;
    fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_PATH | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(root)
        .map_err(|e| disk::io(root, e))
}

/// Pin first with O_PATH, reject special files, then open that exact inode for I/O.
/// 先用 O_PATH 固定对象并排除特殊文件，再打开同一 inode 读写。
fn open_file(root: &File, guest: &str, writable: bool) -> Result<Option<File>> {
    #[repr(C)]
    struct How {
        flags: u64,
        mode: u64,
        resolve: u64,
    }
    let path = CString::new(guest.trim_start_matches('/'))
        .map_err(|_| Error::Invalid("无效包文件路径 / Invalid package file path".into()))?;
    let how = How {
        flags: (libc::O_PATH | libc::O_CLOEXEC) as u64,
        mode: 0,
        resolve: 0x08 | 0x04 | 0x02, // BENEATH | NO_SYMLINKS | NO_MAGICLINKS
    };
    let fd = unsafe {
        libc::syscall(
            libc::SYS_openat2,
            root.as_raw_fd(),
            path.as_ptr(),
            &how,
            std::mem::size_of::<How>(),
        )
    };
    if fd < 0 {
        let error = std::io::Error::last_os_error();
        if matches!(
            error.raw_os_error(),
            Some(libc::ENOENT | libc::ENOTDIR | libc::ELOOP | libc::EXDEV)
        ) {
            return Ok(None);
        }
        return Err(disk::io(Path::new(guest), error));
    }
    let pinned = unsafe { File::from_raw_fd(fd as i32) };
    if !pinned
        .metadata()
        .map_err(|e| disk::io(Path::new(guest), e))?
        .is_file()
    {
        return Ok(None);
    }
    let proc = PathBuf::from(format!("/proc/self/fd/{}", pinned.as_raw_fd()));
    fs::OpenOptions::new()
        .read(true)
        .write(writable)
        .custom_flags(libc::O_CLOEXEC)
        .open(proc)
        .map(Some)
        .map_err(|e| disk::io(Path::new(guest), e))
}

/// Try newest donors first, retaining unmatched files for earlier systems.
/// 优先尝试最近来源，未匹配文件继续向更早系统查找。
pub(super) fn run(
    backend: &impl BuildBackend,
    plugin: &LoadedPlugin,
    architecture: &str,
    system: &str,
    root: &Path,
    index: &Index,
    sources: &[Source],
) -> Result<()> {
    let donors: Vec<_> = sources
        .iter()
        .rev()
        .filter(|s| s.plugin == plugin.metadata.id && s.architecture == architecture)
        .collect();
    if donors.is_empty() {
        progress::message(
            system,
            "无可用去重来源，保留二阶段包文件索引",
            "No deduplication donors; retain the software-stage file index",
        );
        return Ok(());
    }
    let target_root = open_root(root)?;
    let mut matched = BTreeSet::new();
    let mut shared_files = 0u64;
    let mut shared_bytes = 0u64;
    let mut attempted = 0u64;
    let mut unsupported = 0u64;
    for source in donors {
        backend.check_subvolume(&source.root, true)?;
        let source_root = open_root(&source.root)?;
        // Deduplicate a path once per donor even if multiple packages claim it.
        // 即使多个包声明同一路径，每个来源也只比较一次。
        let mut visited = BTreeSet::new();
        for (owner, path) in index {
            if matched.contains(path)
                || !source
                    .packages
                    .get(owner)
                    .is_some_and(|paths| paths.contains(path))
                || !visited.insert(path)
            {
                continue;
            }
            let Some(source_file) = open_file(&source_root, path, false)? else {
                continue;
            };
            let Some(target_file) = open_file(&target_root, path, true)? else {
                continue;
            };
            let source_size = source_file
                .metadata()
                .map_err(|e| disk::io(&source.root, e))?
                .len();
            if source_size == 0
                || source_size != target_file.metadata().map_err(|e| disk::io(root, e))?.len()
            {
                continue;
            }
            attempted += 1;
            let outcome = backend
                .deduplicate_file(&source_file, &target_file)
                .map_err(|error| {
                    Error::Invalid(format!(
                        "软件包 {owner} 文件去重失败 {0} -> {1} / File deduplication for package {owner} failed {0} -> {1}: {error}",
                        source.root.join(path.trim_start_matches('/')).display(),
                        root.join(path.trim_start_matches('/')).display()
                    ))
                })?;
            match outcome {
                DedupOutcome::Shared(bytes) => {
                    matched.insert(path.clone());
                    shared_files += 1;
                    shared_bytes += bytes;
                }
                DedupOutcome::Different => (),
                DedupOutcome::Unsupported => unsupported += 1,
            }
        }
    }
    progress::message(
        system,
        format!(
            "软件包去重：比较 {attempted} 次，共享 {shared_files} 个文件、{shared_bytes} 字节，{unsupported} 次不支持；共享量不等于释放空间"
        ),
        format!(
            "Package deduplication: {attempted} comparisons, {shared_files} files, {shared_bytes} shared bytes, {unsupported} unsupported; shared bytes are not freed space"
        ),
    );
    Ok(())
}

#[cfg(test)]
mod tests;
