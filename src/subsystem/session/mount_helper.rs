//! Attach descriptor-pinned resources across mount namespaces without path canonicalization.
//! 跨挂载命名空间接入描述符固定的资源，不规范化为可变路径。
use super::{Error, Result};
use std::{
    ffi::{CString, OsString},
    fs::File,
    io,
    os::{
        fd::{AsRawFd, FromRawFd, RawFd},
        unix::fs::FileTypeExt,
    },
};

/// Supported operations accept descriptors only and never acquire additional privileges.
/// 支持的操作仅接受描述符，绝不获取额外权限。
#[derive(Clone, Copy, PartialEq, Eq)]
enum Operation {
    Bind,
    BindReadOnly,
    Attach,
    ReadOnly,
    Unmount,
    Rollback,
}

/// Preserve the failing syscall and its original errno in bilingual diagnostics.
/// 在双语诊断中保留失败的系统调用及其原始 errno。
fn failed(operation: &str) -> Error {
    Error::Invalid(format!(
        "会话挂载系统调用 {operation} 失败 / session mount syscall {operation} failed: {}",
        io::Error::last_os_error()
    ))
}

/// Validate all original descriptors before allocating any duplicates.
/// 分配任何副本之前校验全部原始描述符。
fn descriptor(value: &OsString) -> Result<RawFd> {
    let fd = value
        .to_str()
        .and_then(|s| s.parse::<RawFd>().ok())
        .filter(|fd| *fd >= 3)
        .ok_or_else(|| {
            Error::Invalid(
                "会话挂载描述符必须是至少为 3 的整数 / session mount descriptor must be an integer of at least 3".into(),
            )
        })?;
    if unsafe { libc::fcntl(fd, libc::F_GETFD) } < 0 {
        return Err(failed("fcntl(F_GETFD)"));
    }
    Ok(fd)
}

/// Own a close-on-exec duplicate after all supplied descriptors have been checked.
/// 全部传入描述符检查完成后，持有一个执行时关闭的副本。
fn duplicate(fd: RawFd) -> Result<File> {
    let raw = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 3) };
    if raw < 0 {
        return Err(failed("fcntl(F_DUPFD_CLOEXEC)"));
    }
    Ok(unsafe { File::from_raw_fd(raw) })
}

/// Admit only the regular files and Unix sockets used by caller session resources.
/// 仅允许调用者会话资源使用的普通文件和 Unix 套接字。
fn endpoint(file: &File) -> Result<()> {
    let metadata = file.metadata()?;
    if !metadata.is_file() && !metadata.file_type().is_socket() {
        return Err(Error::Invalid(
            "会话挂载端点必须是普通文件或 socket，不能是目录或符号链接 / session mount endpoint must be a regular file or socket, not a directory or symlink".into(),
        ));
    }
    Ok(())
}

/// Set only the read-only mount attribute, preserving all other source mount flags.
/// 仅设置只读挂载属性，保留来源挂载的其余全部标志。
fn readonly(file: &File) -> Result<()> {
    let attr = libc::mount_attr {
        attr_set: libc::MOUNT_ATTR_RDONLY,
        attr_clr: 0,
        propagation: 0,
        userns_fd: 0,
    };
    let result = unsafe {
        libc::syscall(
            libc::SYS_mount_setattr,
            file.as_raw_fd(),
            c"".as_ptr(),
            libc::AT_EMPTY_PATH,
            &attr,
            std::mem::size_of_val(&attr),
        )
    };
    if result < 0 {
        return Err(failed("mount_setattr"));
    }
    Ok(())
}

/// Clone a pinned host object without changing the caller's mount namespace.
/// 克隆固定的宿主对象，不改变调用者的挂载命名空间。
pub(super) fn prepare(source: &File, read_only: bool) -> Result<File> {
    endpoint(source)?;
    let raw = unsafe {
        libc::syscall(
            libc::SYS_open_tree,
            source.as_raw_fd(),
            c"".as_ptr(),
            libc::AT_EMPTY_PATH as libc::c_uint | libc::OPEN_TREE_CLONE | libc::OPEN_TREE_CLOEXEC,
        )
    };
    if raw < 0 {
        return Err(failed("open_tree"));
    }
    let tree = unsafe { File::from_raw_fd(raw as RawFd) };
    // Apply read-only before publication so failure cannot expose a writable bind.
    // 发布前设置只读，确保失败时不会暴露可写绑定。
    if read_only {
        readonly(&tree)?;
    }
    Ok(tree)
}

/// Run in a fresh helper process; changing its namespace cannot affect daemon threads.
/// 在新辅助进程中执行；切换其命名空间不会影响守护进程线程。
pub(crate) fn entry(arguments: &[OsString]) -> Result<()> {
    let [operation, namespace, source, target] = arguments else {
        return Err(Error::Invalid(
            "内部会话挂载需要操作及三个描述符参数 / internal session mount requires an operation and three descriptor arguments".into(),
        ));
    };
    let operation = match operation.to_str() {
        Some("bind") => Operation::Bind,
        Some("bind-readonly") => Operation::BindReadOnly,
        Some("attach") => Operation::Attach,
        Some("readonly") => Operation::ReadOnly,
        Some("unmount") => Operation::Unmount,
        Some("rollback") => Operation::Rollback,
        _ => {
            return Err(Error::Invalid(
                "内部会话挂载操作无效 / invalid internal session mount operation".into(),
            ));
        }
    };
    let namespace = descriptor(namespace)?;
    let target = descriptor(target)?;
    let source = match operation {
        Operation::Bind | Operation::BindReadOnly | Operation::Attach => Some(descriptor(source)?),
        _ if source == "-" => None,
        _ => {
            return Err(Error::Invalid(
                "非绑定操作的来源描述符必须为 - / non-bind operations require - as the source descriptor".into(),
            ));
        }
    };
    if namespace == target || source.is_some_and(|fd| fd == namespace || fd == target) {
        return Err(Error::Invalid(
            "会话挂载描述符必须互不相同 / session mount descriptors must be distinct".into(),
        ));
    }
    let namespace = duplicate(namespace)?;
    let target = duplicate(target)?;
    endpoint(&target)?;
    let tree = if let Some(source) = source {
        let source = duplicate(source)?;
        endpoint(&source)?;
        Some(if operation == Operation::Attach {
            source
        } else {
            prepare(&source, operation == Operation::BindReadOnly)?
        })
    } else {
        None
    };
    if unsafe { libc::setns(namespace.as_raw_fd(), libc::CLONE_NEWNS) } < 0 {
        return Err(failed("setns"));
    }
    match operation {
        Operation::Bind | Operation::BindReadOnly | Operation::Attach => {
            let result = unsafe {
                libc::syscall(
                    libc::SYS_move_mount,
                    tree.as_ref().unwrap().as_raw_fd(),
                    c"".as_ptr(),
                    target.as_raw_fd(),
                    c"".as_ptr(),
                    libc::MOVE_MOUNT_F_EMPTY_PATH | libc::MOVE_MOUNT_T_EMPTY_PATH,
                )
            };
            if result < 0 {
                return Err(failed("move_mount"));
            }
        }
        Operation::ReadOnly => readonly(&target)?,
        Operation::Unmount | Operation::Rollback => {
            let path = CString::new(format!("/proc/self/fd/{}", target.as_raw_fd())).unwrap();
            // Pinning keeps the old bind busy; detach its tree while open references survive.
            // 固定描述符会占用旧绑定；摘除其挂载树，已打开的引用仍可使用。
            if unsafe { libc::umount2(path.as_ptr(), libc::MNT_DETACH) } < 0 {
                // A self-created tree may never have been attached, or was already detached.
                // 自建挂载树可能从未接入，或已被摘除。
                if operation == Operation::Rollback
                    && io::Error::last_os_error().raw_os_error() == Some(libc::EINVAL)
                {
                    return Ok(());
                }
                return Err(failed("umount2"));
            }
        }
    }
    Ok(())
}
