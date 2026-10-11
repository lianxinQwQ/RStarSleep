//! Durable owned records and root-contained guest path resolution.
//! 持久所属记录和根内受限的容器路径解析。
use super::{Error, Result};
use serde::{Serialize, de::DeserializeOwned};
use std::os::unix::fs::PermissionsExt;
use std::{
    collections::VecDeque,
    ffi::OsString,
    fs,
    io::Write,
    path::{Component, Path, PathBuf},
};

/// Gate persistent migration against machine operations and application leases.
/// 使用锁协调持久迁移、机器操作与应用租约。
/// # Arguments
/// - `store`, `id`: Fixed manager and registered system. 固定管理器及已登记系统。
/// - `exclusive`: Migration writer versus runtime reader. 迁移写者或运行读者。
pub(crate) fn activity(
    store: &crate::config::ConfigStore,
    id: &str,
    exclusive: bool,
) -> Result<fs::File> {
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::OpenOptionsExt;
    let directory = store.system_layout(id)?.root.join("runtime");
    checked(&directory)?;
    fs::create_dir_all(&directory)?;
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(directory.join("activity.lock"))?;
    if unsafe {
        libc::flock(
            file.as_raw_fd(),
            (if exclusive {
                libc::LOCK_EX
            } else {
                libc::LOCK_SH
            }) | libc::LOCK_NB,
        )
    } != 0
    {
        return Err(Error::Invalid(
            "subsystem has an active execution or persistent migration".into(),
        ));
    }
    Ok(file)
}

/// Open a guest resource with kernel-enforced containment, including racing links.
/// 使用内核强制边界打开容器资源，包括并发变化的链接。
/// # Arguments
/// - `root`: Actual root directory. 实际根目录。
/// - `guest`: Absolute guest path. 绝对容器路径。
/// - `flags`: Read-only or O_PATH flags. 只读或 O_PATH 标志。
pub(crate) fn open_guest(root: &Path, guest: &Path, flags: i32) -> Result<fs::File> {
    super::config::absolute(guest, true)?;
    checked(root)?;
    let directory = open_root(root)?;
    open_at_guest(&directory, guest, flags)
}

/// Pin an owned artifact directory without following a mutable root link.
/// 固定所属产物目录，不跟随可变根链接。
/// # Arguments
/// - `root`: Owned actual root path. 所属实际根路径。
pub(crate) fn open_root(root: &Path) -> Result<fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    checked(root)?;
    Ok(fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_PATH | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(root)?)
}

/// Pin the live machine root, including all actual persistent/runtime bindings.
/// 固定包含实际持久及运行绑定的运行机器根。
/// # Arguments
/// - `runtime`: Expected owned machine. 预期所属机器。
pub(crate) fn open_runtime_root(runtime: &super::PreparedRuntime) -> Result<fs::File> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    let leader = super::commands::leader(runtime)?;
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_PATH | libc::O_DIRECTORY | libc::O_CLOEXEC)
        .open(format!("/proc/{leader}/root"))?;
    let actual = file.metadata()?;
    let expected = fs::metadata(&runtime.root)?;
    if actual.dev() != expected.dev()
        || actual.ino() != expected.ino()
        || super::commands::leader(runtime)? != leader
    {
        return Err(Error::Invalid("live machine root identity changed".into()));
    }
    Ok(file)
}

/// Resolve a path relative to a pinned guest-root descriptor.
/// 相对于已固定容器根描述符解析路径。
/// # Arguments
/// - `directory`: Pinned root directory. 固定根目录。
/// - `guest`: Guest-contained absolute path. 根内绝对路径。
/// - `flags`: Open flags. 打开标志。
pub(crate) fn open_at_guest(directory: &fs::File, guest: &Path, flags: i32) -> Result<fs::File> {
    use std::os::fd::{AsRawFd, FromRawFd};
    super::config::absolute(guest, true)?;
    let path = std::ffi::CString::new(guest.as_os_str().as_encoded_bytes())
        .map_err(|_| Error::Invalid("invalid guest path".into()))?;
    #[repr(C)]
    struct How {
        flags: u64,
        mode: u64,
        resolve: u64,
    }
    let how = How {
        flags: (flags | libc::O_CLOEXEC) as u64,
        mode: 0,
        resolve: 0x10 | 0x02,
    }; // IN_ROOT | NO_MAGICLINKS
    let fd = unsafe {
        libc::syscall(
            libc::SYS_openat2,
            directory.as_raw_fd(),
            path.as_ptr(),
            &how,
            std::mem::size_of::<How>(),
        )
    };
    if fd < 0 {
        return Err(Error::Io {
            path: guest.into(),
            source: std::io::Error::last_os_error(),
        });
    }
    Ok(unsafe { fs::File::from_raw_fd(fd as i32) })
}

/// Reject owned paths passing through symlinks or parent traversal.
/// 拒绝经过链接或父级跳转的所属路径。
/// # Arguments
/// - `path`: Absolute manager-owned path. 管理器所属绝对路径。
pub(crate) fn checked(path: &Path) -> Result<()> {
    if !path.is_absolute() || path.components().any(|p| matches!(p, Component::ParentDir)) {
        return Err(Error::Invalid(format!("所属路径无效: {}", path.display())));
    }
    let mut current = PathBuf::new();
    for part in path.components() {
        current.push(part);
        match fs::symlink_metadata(&current) {
            Ok(meta) if meta.file_type().is_symlink() || (current != path && !meta.is_dir()) => {
                return Err(Error::Invalid(format!(
                    "所属路径祖先无效: {}",
                    current.display()
                )));
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(Error::Io {
                    path: current,
                    source,
                });
            }
        }
    }
    Ok(())
}

/// Synchronize bytes before replacing an owned regular file.
/// 替换所属普通文件前同步字节内容。
/// # Arguments
/// - `path`: Destination with an existing parent. 父目录已存在的目标。
/// - `bytes`: Complete desired content. 完整目标内容。
/// - `mode`: Final Unix permission bits. 最终 Unix 权限位。
pub(crate) fn atomic_write(path: &Path, bytes: &[u8], mode: u32) -> Result<()> {
    checked(path)?;
    let parent = path
        .parent()
        .ok_or_else(|| Error::Invalid("文件缺少父目录".into()))?;
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    file.as_file()
        .set_permissions(fs::Permissions::from_mode(mode))?;
    file.write_all(bytes)?;
    file.as_file().sync_all()?;
    file.persist(path).map_err(|e| Error::Io {
        path: path.into(),
        source: e.error,
    })?;
    fs::File::open(parent)?.sync_all()?;
    Ok(())
}

/// Read a strict typed TOML record from an owned path.
/// 从所属路径读取严格类型 TOML 记录。
/// # Arguments
/// - `path`: Existing record file. 已有记录文件。
pub(crate) fn read_record<T: DeserializeOwned>(path: &Path) -> Result<T> {
    checked(path)?;
    let text = fs::read_to_string(path).map_err(|source| Error::Io {
        path: path.into(),
        source,
    })?;
    toml::from_str(&text).map_err(|e| Error::Invalid(format!("{}: {e}", path.display())))
}

/// Persist a typed TOML record atomically.
/// 原子持久化类型化 TOML 记录。
/// # Arguments
/// - `path`: Owned record destination. 所属记录目标。
/// - `record`: Complete replacement value. 完整替换值。
pub(crate) fn write_record<T: Serialize>(path: &Path, record: &T) -> Result<()> {
    let text = toml::to_string(record).map_err(|e| Error::Invalid(e.to_string()))?;
    atomic_write(path, text.as_bytes(), 0o600)
}

/// Resolve absolute guest links inside the root without escaping to host paths.
/// 在系统根内解析绝对容器链接，不越界到宿主路径。
/// # Arguments
/// - `root`: Owned actual system root. 所属实际系统根。
/// - `guest`: Normalized absolute guest path. 规范绝对容器路径。
pub(crate) fn guest_path(root: &Path, guest: &Path) -> Result<PathBuf> {
    super::config::absolute(guest, true)?;
    checked(root)?;
    let mut queue: VecDeque<OsString> = guest
        .components()
        .filter_map(|p| match p {
            Component::Normal(p) => Some(p.to_owned()),
            _ => None,
        })
        .collect();
    let mut relative = PathBuf::new();
    let mut links = 0;
    while let Some(part) = queue.pop_front() {
        if part == ".." {
            relative.pop();
            continue;
        }
        if part == "." {
            continue;
        }
        let current = root.join(&relative).join(&part);
        match fs::symlink_metadata(&current) {
            Ok(meta) if meta.file_type().is_symlink() => {
                links += 1;
                if links > 40 {
                    return Err(Error::Invalid("容器路径链接循环".into()));
                }
                let target = fs::read_link(&current)?;
                if target.is_absolute() {
                    relative.clear();
                }
                let parts: Vec<_> = target
                    .components()
                    .filter_map(|p| match p {
                        Component::Normal(v) => Some(v.to_owned()),
                        Component::ParentDir => Some("..".into()),
                        _ => None,
                    })
                    .collect();
                for p in parts.into_iter().rev() {
                    queue.push_front(p);
                }
            }
            Ok(_) => relative.push(part),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => relative.push(part),
            Err(source) => {
                return Err(Error::Io {
                    path: current,
                    source,
                });
            }
        }
    }
    Ok(root.join(relative))
}
