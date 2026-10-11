//! Kernel-authenticated fixed guest roots and bounded mount-table checks.
//! 内核认证的固定容器根与有界挂载表检查。
use super::*;

/// Derive a collision-resistant component from a normalized guest path.
/// 从规范容器路径推导抗碰撞分量。
/// # Arguments
/// - `target`: Validated guest path. 已校验容器路径。
pub(super) fn key(target: &Path) -> String {
    format!(
        "{:x}",
        Sha256::digest(target.as_os_str().as_encoded_bytes())
    )
}

/// Bound and reject special control paths before any privileged file lookup.
/// 特权文件查找前限制并拒绝特殊控制路径。
/// # Arguments
/// - `target`: Requested guest path. 请求容器路径。
pub(super) fn target_path(target: &Path) -> Result<()> {
    crate::subsystem::config::absolute(target, false)?;
    if target.as_os_str().len() > 4096 {
        return Err(Error::Invalid("guest path exceeds limit".into()));
    }
    for reserved in ["/run", "/proc", "/sys", "/dev", "/rstarsleep"] {
        if target.starts_with(reserved) || Path::new(reserved).starts_with(target) {
            return Err(Error::Invalid(format!(
                "reserved guest path: {}",
                target.display()
            )));
        }
    }
    Ok(())
}

/// Select a real guest path with no symbolic-link aliases in any component.
/// 选择真实容器路径，所有分量均不允许符号链接别名。
/// # Arguments
/// - `root`: Pinned authenticated guest root. 固定已认证容器根。
/// - `target`: Normalized unreserved guest path. 规范非保留容器路径。
pub(super) fn open_target(root: &File, target: &Path) -> Result<File> {
    target_path(target)?;
    let path = CString::new(target.as_os_str().as_encoded_bytes())
        .map_err(|_| Error::Invalid("invalid guest target".into()))?;
    #[repr(C)]
    struct How {
        flags: u64,
        mode: u64,
        resolve: u64,
    }
    let how = How {
        flags: (libc::O_PATH | libc::O_CLOEXEC) as u64,
        mode: 0,
        resolve: 0x10 | 0x02 | 0x04, // IN_ROOT | NO_MAGICLINKS | NO_SYMLINKS
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
        return Err(Error::Io {
            path: target.into(),
            source: io::Error::last_os_error(),
        });
    }
    Ok(unsafe { File::from_raw_fd(fd as i32) })
}

/// Pin the live machine root and require a root peer in its mount namespace.
/// 固定运行机器根，并要求 root 对端属于该机器挂载命名空间。
/// # Arguments
/// - `runtime`: Endpoint's fixed machine identity. 端点固定机器身份。
/// - `peer`: Kernel-authenticated caller. 内核认证调用者。
pub(super) fn authenticate(runtime: &PreparedRuntime, peer: identity::Peer) -> Result<GuestRoot> {
    identity::admin(peer)?;
    let leader = commands::leader(runtime)?;
    let namespace = File::open(format!("/proc/{leader}/ns/mnt"))?;
    let caller = File::open(format!("/proc/{}/ns/mnt", peer.pid))?;
    if !same_inode(&namespace.metadata()?, &caller.metadata()?) {
        return Err(Error::Invalid(
            "guest socket is restricted to its machine mount namespace".into(),
        ));
    }
    let root = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_PATH | libc::O_DIRECTORY | libc::O_CLOEXEC)
        .open(format!("/proc/{leader}/root"))?;
    if !same_inode(&root.metadata()?, &fs::metadata(&runtime.root)?)
        || commands::leader(runtime)? != leader
    {
        return Err(Error::Invalid(
            "guest machine root changed during authentication".into(),
        ));
    }
    Ok(GuestRoot {
        leader,
        namespace,
        root,
    })
}

/// Recheck leader identity before publishing data or changing a live mount.
/// 发布数据或改变运行挂载前重新检查首进程身份。
/// # Arguments
/// - `runtime`, `root`: Fixed runtime and pinned namespace/root. 固定运行态与命名空间/根。
pub(super) fn still_running(runtime: &PreparedRuntime, root: &GuestRoot) -> Result<()> {
    if commands::leader(runtime)? != root.leader
        || !same_inode(
            &root.namespace.metadata()?,
            &fs::metadata(format!("/proc/{}/ns/mnt", root.leader))?,
        )
    {
        return Err(Error::Invalid(
            "guest restarted during operation; retry".into(),
        ));
    }
    Ok(())
}

/// Compare pinned kernel objects without relying on textual paths.
/// 比较固定内核对象，不依赖文字路径。
/// # Arguments
/// - `a`, `b`: Metadata to compare. 待比较元数据。
pub(super) fn same_inode(a: &fs::Metadata, b: &fs::Metadata) -> bool {
    a.dev() == b.dev() && a.ino() == b.ino()
}

/// Decode mountinfo's octal escaping without accepting malformed fields.
/// 解码 mountinfo 八进制转义并拒绝无效字段。
/// # Arguments
/// - `value`: Mountpoint field. 挂载点字段。
pub(super) fn mount_path(value: &str) -> Result<PathBuf> {
    use std::os::unix::ffi::OsStringExt;
    let bytes = value.as_bytes();
    let mut output = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' {
            let octal = bytes
                .get(i + 1..i + 4)
                .ok_or_else(|| Error::Invalid("invalid mountinfo escape".into()))?;
            if !octal.iter().all(|v| (b'0'..=b'7').contains(v)) {
                return Err(Error::Invalid("invalid mountinfo escape".into()));
            }
            let n = ((octal[0] - b'0') as u16) * 64
                + ((octal[1] - b'0') as u16) * 8
                + (octal[2] - b'0') as u16;
            if n == 0 || n > 255 {
                return Err(Error::Invalid("invalid mountinfo byte".into()));
            }
            output.push(n as u8);
            i += 4;
        } else {
            output.push(bytes[i]);
            i += 1;
        }
    }
    Ok(std::ffi::OsString::from_vec(output).into())
}

/// Detect a target mount and all descendants, so copies never omit hidden volumes.
/// 检测目标及后代挂载，防止复制遗漏被隐藏卷。
/// # Arguments
/// - `root`: Authenticated namespace. 已认证命名空间。
/// - `target`: Guest target. 容器目标。
pub(super) fn mounts_at(root: &GuestRoot, target: &Path) -> Result<Vec<PathBuf>> {
    let data = fs::read_to_string(format!("/proc/{}/mountinfo", root.leader))?;
    if data.len() > 4 * 1024 * 1024 {
        return Err(Error::Invalid("guest mount table exceeds limit".into()));
    }
    let mut mounts = Vec::new();
    for line in data.lines() {
        let field = line
            .split_ascii_whitespace()
            .nth(4)
            .ok_or_else(|| Error::Invalid("invalid guest mount table".into()))?;
        let mount = mount_path(field)?;
        if mount.starts_with(target) {
            mounts.push(mount);
        }
    }
    Ok(mounts)
}
