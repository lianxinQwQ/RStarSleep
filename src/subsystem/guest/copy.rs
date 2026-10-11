//! Bounded descriptor-based data capture and generated container scripts.
//! 有界描述符数据保存与生成容器脚本。
use super::*;

pub(super) struct CopyBudget {
    bytes: u64,
    entries: usize,
    hardlinks: BTreeMap<(u64, u64), PathBuf>,
}
impl CopyBudget {
    pub(super) fn new(bytes: u64) -> Self {
        Self {
            bytes,
            entries: 100_000,
            hardlinks: BTreeMap::new(),
        }
    }
    /// Consume actual data and extended-attribute bytes after bounded reads.
    /// 有界读取后计入实际数据与扩展属性字节。
    /// # Arguments
    /// - `bytes`: Captured byte count. 保存的字节数量。
    fn consume(&mut self, bytes: u64) -> Result<()> {
        self.bytes = self
            .bytes
            .checked_sub(bytes)
            .ok_or_else(|| Error::Invalid("guest copy data limit exceeded".into()))?;
        Ok(())
    }
}

/// Never let a privileged copier read process/kernel control files through an alias.
/// 防止特权复制器通过别名读取进程或内核控制文件。
/// # Arguments
/// - `source`: Pinned guest-visible object. 固定容器可见对象。
fn data_filesystem(source: &File) -> Result<()> {
    let mut info: libc::statfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstatfs(source.as_raw_fd(), &mut info) } != 0 {
        return Err(io::Error::last_os_error().into());
    }
    if matches!(
        info.f_type as u64,
        0x9fa0
            | 0x62656572
            | 0x64626720
            | 0x74726163
            | 0x73636673
            | 0xcafe4a11
            | 0x27e0eb
            | 0x63677270
            | 0x1cd1
            | 0x19800202
            | 0xf97cff8c
            | 0x62656570
    ) {
        return Err(Error::Invalid(
            "guest capture refuses kernel and process pseudo-filesystems".into(),
        ));
    }
    Ok(())
}

/// Open one child as an object, rejecting traversal and never following its symlink.
/// 打开单个子对象，拒绝跳转且不跟随链接。
/// # Arguments
/// - `directory`, `name`: Pinned parent and single native component. 固定父目录及单个原生分量。
pub(super) fn child_object(directory: &File, name: &std::ffi::OsStr) -> Result<File> {
    let bytes = name.as_encoded_bytes();
    if bytes.is_empty() || bytes == b"." || bytes == b".." || bytes.contains(&b'/') {
        return Err(Error::Invalid("invalid guest directory component".into()));
    }
    let name = CString::new(bytes).map_err(|_| Error::Invalid("invalid guest filename".into()))?;
    let fd = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            name.as_ptr(),
            libc::O_PATH | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error().into());
    }
    Ok(unsafe { File::from_raw_fd(fd) })
}

/// Copy bounded regular data and links from pinned objects without guest path races.
/// 从固定对象复制有界普通数据及链接，避开容器路径竞态。
/// # Arguments
/// - `source`, `target`: Pinned source and private destination. 固定来源及私有目标。
/// - `budget`, `depth`: Remaining data/entry limits and depth. 剩余数据/条目限制与深度。
pub(super) fn copy_object(
    source: &File,
    target: &Path,
    budget: &mut CopyBudget,
    depth: usize,
) -> Result<()> {
    use std::io::{Read, Write};
    if depth > 64 || budget.entries == 0 {
        return Err(Error::Invalid(
            "guest copy depth/entry limit exceeded".into(),
        ));
    }
    budget.entries -= 1;
    let metadata = source.metadata()?;
    data_filesystem(source)?;
    if metadata.file_type().is_symlink() {
        let mut link = [0u8; 4097];
        let n = unsafe {
            libc::readlinkat(
                source.as_raw_fd(),
                c"".as_ptr(),
                link.as_mut_ptr().cast(),
                link.len(),
            )
        };
        if n < 0 {
            return Err(io::Error::last_os_error().into());
        }
        if n as usize == link.len() {
            return Err(Error::Invalid("guest link exceeds limit".into()));
        }
        budget.consume(n as u64)?;
        use std::os::unix::ffi::OsStrExt;
        std::os::unix::fs::symlink(std::ffi::OsStr::from_bytes(&link[..n as usize]), target)?;
        let path = CString::new(target.as_os_str().as_encoded_bytes())
            .map_err(|_| Error::Invalid("invalid captured path".into()))?;
        if unsafe { libc::lchown(path.as_ptr(), metadata.uid(), metadata.gid()) } != 0 {
            return Err(io::Error::last_os_error().into());
        }
        return Ok(());
    }
    if !metadata.is_dir() && !metadata.is_file() {
        return Err(Error::Invalid(
            "guest capture refuses devices, sockets and FIFOs".into(),
        ));
    }
    let mut input = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(format!("/proc/self/fd/{}", source.as_raw_fd()))?;
    if metadata.is_dir() {
        fs::create_dir(target)?;
        fs::set_permissions(target, fs::Permissions::from_mode(0o700))?;
        for entry in fs::read_dir(format!("/proc/self/fd/{}", source.as_raw_fd()))? {
            let name = entry?.file_name();
            let child = child_object(source, &name)?;
            copy_object(&child, &target.join(name), budget, depth + 1)?;
        }
        let output = File::open(target)?;
        attributes(&input, &output, &metadata, budget)?;
        output.sync_all()?;
    } else {
        let identity = (metadata.dev(), metadata.ino());
        if metadata.nlink() > 1
            && let Some(first) = budget.hardlinks.get(&identity)
        {
            fs::hard_link(first, target)?;
            return Ok(());
        }
        if metadata.len() > budget.bytes {
            return Err(Error::Invalid("guest copy data limit exceeded".into()));
        }
        let mut output = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(target)?;
        if unsafe { libc::ioctl(output.as_raw_fd(), 0x40049409u64, input.as_raw_fd()) } != 0 {
            let mut limited = (&mut input).take(budget.bytes.saturating_add(1));
            let mut bytes = [0u8; 64 * 1024];
            let mut copied = 0u64;
            loop {
                let n = limited.read(&mut bytes)?;
                if n == 0 {
                    break;
                }
                copied += n as u64;
                if copied > budget.bytes {
                    return Err(Error::Invalid("guest copy data limit exceeded".into()));
                }
                output.write_all(&bytes[..n])?;
            }
        }
        let actual = output.metadata()?.len();
        if actual > budget.bytes {
            return Err(Error::Invalid("guest copy data limit exceeded".into()));
        }
        budget.consume(actual)?;
        attributes(&input, &output, &metadata, budget)?;
        output.sync_all()?;
        if metadata.nlink() > 1 {
            budget.hardlinks.insert(identity, target.into());
        }
    }
    Ok(())
}

/// Preserve data ownership, modes, times and bounded Linux extended attributes.
/// 保留数据所有权、模式、时间及有界 Linux 扩展属性。
/// # Arguments
/// - `source`, `target`, `metadata`: Open data objects and original attributes. 已打开数据对象及原属性。
/// - `budget`: Remaining captured byte allowance. 剩余保存字节额度。
fn attributes(
    source: &File,
    target: &File,
    metadata: &fs::Metadata,
    budget: &mut CopyBudget,
) -> Result<()> {
    if unsafe { libc::fchown(target.as_raw_fd(), metadata.uid(), metadata.gid()) } != 0 {
        return Err(io::Error::last_os_error().into());
    }
    target.set_permissions(fs::Permissions::from_mode(metadata.mode() & 0o7777))?;
    let times = [
        libc::timespec {
            tv_sec: metadata.atime(),
            tv_nsec: metadata.atime_nsec(),
        },
        libc::timespec {
            tv_sec: metadata.mtime(),
            tv_nsec: metadata.mtime_nsec(),
        },
    ];
    if unsafe { libc::futimens(target.as_raw_fd(), times.as_ptr()) } != 0 {
        return Err(io::Error::last_os_error().into());
    }
    let count = unsafe { libc::flistxattr(source.as_raw_fd(), std::ptr::null_mut(), 0) };
    if count < 0 {
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::EOPNOTSUPP) {
            return Ok(());
        }
        return Err(error.into());
    }
    if count > 1024 * 1024 {
        return Err(Error::Invalid("guest xattr list exceeds limit".into()));
    }
    budget.consume(count as u64)?;
    let mut names = vec![0u8; count as usize];
    let read =
        unsafe { libc::flistxattr(source.as_raw_fd(), names.as_mut_ptr().cast(), names.len()) };
    if read < 0 {
        return Err(io::Error::last_os_error().into());
    }
    if read as usize > names.len() {
        return Err(Error::Invalid("guest xattrs changed during copy".into()));
    }
    names.truncate(read as usize);
    for name in names.split(|b| *b == 0).filter(|s| !s.is_empty()) {
        let name = CString::new(name).map_err(|_| Error::Invalid("invalid xattr name".into()))?;
        let count =
            unsafe { libc::fgetxattr(source.as_raw_fd(), name.as_ptr(), std::ptr::null_mut(), 0) };
        if count < 0 {
            return Err(io::Error::last_os_error().into());
        }
        if count > 1024 * 1024 {
            return Err(Error::Invalid("guest xattr exceeds limit".into()));
        }
        if count as u64 > budget.bytes {
            return Err(Error::Invalid("guest xattrs exceed data limit".into()));
        }
        let mut value = vec![0u8; count as usize];
        let n = unsafe {
            libc::fgetxattr(
                source.as_raw_fd(),
                name.as_ptr(),
                value.as_mut_ptr().cast(),
                value.len(),
            )
        };
        if n < 0 {
            return Err(io::Error::last_os_error().into());
        }
        if n as usize > value.len() {
            return Err(Error::Invalid("guest xattr changed during copy".into()));
        }
        budget.consume(n as u64)?;
        if unsafe {
            libc::fsetxattr(
                target.as_raw_fd(),
                name.as_ptr(),
                value.as_ptr().cast(),
                n as usize,
                0,
            )
        } != 0
        {
            return Err(io::Error::last_os_error().into());
        }
    }
    Ok(())
}

/// Quote a validated path as literal shell data in the generated build script.
/// 在生成构建脚本中把已校验路径引用为 shell 字面数据。
/// # Arguments
/// - `target`: Validated target. 已校验目标。
fn shell_path(target: &Path) -> Result<String> {
    Ok(format!(
        "'{}'",
        target
            .to_str()
            .ok_or_else(|| Error::Invalid("guest target must be UTF-8".into()))?
            .replace('\'', "'\"'\"'")
    ))
}

/// Apply a captured payload with literal targets inside the next build container.
/// 在下一构建容器内以字面目标应用已保存资源。
/// # Arguments
/// - `target`: Validated absolute target. 已校验绝对目标。
/// - `directory`: Whether the payload is a directory. 资源是否为目录。
pub(super) fn capture_script(target: &Path, directory: bool) -> Result<String> {
    let target = shell_path(target)?;
    let copy = if directory {
        "mkdir -p -- \"$target\"\ncp -a --reflink=auto -- \"$payload/data/.\" \"$target\""
    } else {
        "cp -a --reflink=auto --remove-destination -- \"$payload/data\" \"$target\""
    };
    Ok(format!(
        "#!/usr/bin/env bash\nset -euo pipefail\ntarget={target}\npayload=$(mktemp -d)\ntrap 'rm -rf -- \"$payload\"' EXIT\ntar --extract --same-owner --same-permissions --xattrs --acls --file=./data.tar --directory=\"$payload\"\nmkdir -p -- \"$(dirname -- \"$target\")\"\n{copy}\n"
    ))
}
