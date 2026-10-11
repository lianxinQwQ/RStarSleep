//! Mount pinned resource descriptors in the pinned machine namespace.
//! 在固定机器命名空间中挂载固定资源描述符。
use super::{
    BindIdentity, Context, Error, PreparedRuntime, Resource, Result, commands, open_at_guest,
};
use std::{
    fs::{File, OpenOptions},
    io,
    os::{
        fd::AsRawFd,
        unix::{
            fs::{MetadataExt, OpenOptionsExt},
            process::CommandExt,
        },
    },
    path::PathBuf,
    process::Command,
    time::Duration,
};
const NSENTER: &str = "/usr/bin/nsenter";

/// Pin the running guest root and mount namespace before handling resources.
/// 在处理资源前固定运行容器的根和挂载命名空间。
/// # Arguments
/// - `runtime`: Registered machine. 已注册机器。
pub(super) fn context(runtime: &PreparedRuntime) -> Result<Context> {
    let pid = commands::leader(runtime)?;
    let base = PathBuf::from(format!("/proc/{pid}"));
    let root = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_PATH | libc::O_DIRECTORY | libc::O_CLOEXEC)
        .open(base.join("root"))?;
    let namespace = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC)
        .open(base.join("ns/mnt"))?;
    if commands::leader(runtime)? != pid {
        return Err(Error::Invalid(
            "machine leader changed during session preparation".into(),
        ));
    }
    Ok(Context { root, namespace })
}

/// Pass pinned descriptors only to the child entering the pinned namespace.
/// 仅向进入固定命名空间的子进程传递固定描述符。
/// # Arguments
/// - `context`, `source`, `target`: Pinned namespace and mount endpoints. 固定命名空间及挂载端点。
/// - `operation`: Bind, remount or unmount operation. 绑定、重新挂载或卸载操作。
pub(super) fn mount_command(
    context: &Context,
    source: Option<&File>,
    target: &File,
    operation: &str,
) -> Result<()> {
    let namespace_fd = context.namespace.as_raw_fd();
    let target_fd = target.as_raw_fd();
    let source_fd = source.map(AsRawFd::as_raw_fd);
    // nsenter changes only the mount namespace and retains the host root and
    // PID namespace. Host /proc/self/fd remains valid; disabling canonicalization
    // prevents util-linux from converting descriptors back to mutable paths.
    // nsenter 仅切换挂载命名空间，保留宿主根和 PID 命名空间；宿主 /proc/self/fd 仍有效，
    // 禁用规范化可防止 util-linux 将描述符转换回可变路径。
    let mut command = Command::new(NSENTER);
    command
        .arg(format!("--mount=/proc/self/fd/{namespace_fd}"))
        .arg("--");
    match operation {
        "bind" => {
            command
                .args(["/usr/bin/mount", "--no-canonicalize", "--bind", "--"])
                .arg(format!("/proc/self/fd/{}", source_fd.unwrap()))
                .arg(format!("/proc/self/fd/{target_fd}"));
        }
        "readonly" => {
            command
                .args([
                    "/usr/bin/mount",
                    "--no-canonicalize",
                    "--options=remount,bind,ro",
                    "--",
                ])
                .arg(format!("/proc/self/fd/{target_fd}"));
        }
        "unmount" => {
            command
                .args(["/usr/bin/umount", "--no-canonicalize", "--"])
                .arg(format!("/proc/self/fd/{target_fd}"));
        }
        _ => {
            return Err(Error::Invalid(
                "invalid internal session mount operation".into(),
            ));
        }
    }
    unsafe {
        command.pre_exec(move || {
            for fd in [Some(namespace_fd), Some(target_fd), source_fd]
                .into_iter()
                .flatten()
            {
                let flags = libc::fcntl(fd, libc::F_GETFD);
                if flags < 0 || libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0 {
                    return Err(io::Error::last_os_error());
                }
            }
            Ok(())
        });
    }
    let output = commands::output(command, Duration::from_secs(10))?;
    if !output.status.success() {
        return Err(Error::Invalid(format!(
            "cannot {operation} caller session resource: {}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    Ok(())
}

/// Recheck both source identity and the actual guest-visible mount each time.
/// 每次重新检查来源身份和容器实际可见挂载。
/// # Arguments
/// - `runtime`, `context`, `uid`, `resource`: Authorized machine and pinned resource. 已授权机器及固定资源。
pub(super) fn bind(
    runtime: &PreparedRuntime,
    context: &Context,
    uid: u32,
    resource: &Resource,
    previous: Option<BindIdentity>,
) -> Result<()> {
    let mut target = match open_at_guest(
        &context.root,
        &resource.target,
        libc::O_PATH | libc::O_NOFOLLOW,
    ) {
        Ok(target) => target,
        Err(Error::Io { source, .. }) if source.kind() == io::ErrorKind::NotFound => {
            let parent = resource
                .target
                .parent()
                .unwrap()
                .to_string_lossy()
                .into_owned();
            commands::guest(
                runtime,
                &[
                    "/usr/bin/install".into(),
                    "--directory".into(),
                    "--mode=0700".into(),
                    format!("--owner={uid}"),
                    parent,
                ],
            )?;
            commands::guest(
                runtime,
                &[
                    "/usr/bin/install".into(),
                    "--mode=0600".into(),
                    format!("--owner={uid}"),
                    "/dev/null".into(),
                    resource.target.to_string_lossy().into_owned(),
                ],
            )?;
            open_at_guest(
                &context.root,
                &resource.target,
                libc::O_PATH | libc::O_NOFOLLOW,
            )?
        }
        Err(error) => return Err(error),
    };
    let source_metadata = resource.source.metadata()?;
    let mut target_metadata = target.metadata()?;
    if target_metadata.file_type().is_symlink() {
        return Err(Error::Invalid(
            "guest session mount target is a symlink".into(),
        ));
    }
    if (source_metadata.dev(), source_metadata.ino())
        != (target_metadata.dev(), target_metadata.ino())
    {
        // Remove a still-visible previous bind before replacing it. A fresh
        // logind directory has another inode and needs no old-mount removal.
        // 替换前移除仍可见的旧绑定；logind 新目录具有不同 inode，无需移除旧挂载。
        if previous == Some((target_metadata.dev(), target_metadata.ino())) {
            mount_command(context, None, &target, "unmount")?;
            target = open_at_guest(
                &context.root,
                &resource.target,
                libc::O_PATH | libc::O_NOFOLLOW,
            )?;
            target_metadata = target.metadata()?;
            if target_metadata.file_type().is_symlink() {
                return Err(Error::Invalid(
                    "replaced guest session mount target is a symlink".into(),
                ));
            }
        }
        mount_command(context, Some(&resource.source), &target, "bind")?;
        target = open_at_guest(
            &context.root,
            &resource.target,
            libc::O_PATH | libc::O_NOFOLLOW,
        )?;
    }
    if resource.read_only {
        mount_command(context, None, &target, "readonly")?;
    }
    let visible = target.metadata()?;
    if (source_metadata.dev(), source_metadata.ino()) != (visible.dev(), visible.ino()) {
        return Err(Error::Invalid(
            "guest session bind is not visible after publication".into(),
        ));
    }
    Ok(())
}
