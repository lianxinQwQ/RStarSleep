//! Mount pinned resource descriptors in the pinned machine namespace.
//! 在固定机器命名空间中挂载固定资源描述符。
use super::{
    BindIdentity, Context, Error, PreparedRuntime, Resource, Result, commands, mount_helper,
    open_at_guest,
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
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

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
            "会话准备期间机器主进程已改变 / machine leader changed during session preparation"
                .into(),
        ));
    }
    Ok(Context { root, namespace })
}

/// Pass pinned descriptors only to the child entering the pinned namespace.
/// 仅向进入固定命名空间的子进程传递固定描述符。
/// # Arguments
/// - `context`, `source`, `target`: Pinned namespace and mount endpoints. 固定命名空间及挂载端点。
/// - `path`, `operation`: Guest resource path and attach, read-only, unmount or rollback operation.
///   容器资源路径及接入、只读、卸载或回滚操作。
pub(super) fn mount_command(
    context: &Context,
    source: Option<&File>,
    target: &File,
    path: &Path,
    operation: &str,
) -> Result<()> {
    let namespace_fd = context.namespace.as_raw_fd();
    let target_fd = target.as_raw_fd();
    let source_fd = source.map(AsRawFd::as_raw_fd);
    if !matches!(operation, "attach" | "readonly" | "unmount" | "rollback")
        || (operation == "attach") != source.is_some()
    {
        return Err(Error::Invalid(
            "内部会话挂载操作或来源无效 / invalid internal session mount operation or source"
                .into(),
        ));
    }
    // Re-exec the running binary, avoiding both executable replacement and libmount path parsing.
    // 重新执行正在运行的程序，避免可执行文件替换及 libmount 的路径解析。
    let mut command = Command::new("/proc/self/exe");
    command
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .args(["__session-mount", operation])
        .arg(namespace_fd.to_string())
        .arg(source_fd.map_or_else(|| "-".into(), |fd| fd.to_string()))
        .arg(target_fd.to_string());
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
    let source_path = source_fd.map(|fd| {
        let descriptor = PathBuf::from(format!("/proc/self/fd/{fd}"));
        std::fs::read_link(&descriptor).unwrap_or(descriptor)
    });
    let context = format!(
        "调用者会话资源操作 {operation}，目标 {0}，来源 {1} / caller session resource operation {operation}, target {0}, source {1}",
        path.display(),
        source_path.as_deref().unwrap_or(Path::new("-")).display()
    );
    let output = commands::output(command, Duration::from_secs(10))
        .map_err(|error| Error::Invalid(format!("{context}: {error}")))?;
    if !output.status.success() {
        return Err(Error::Invalid(format!(
            "{context}: 挂载辅助进程失败 ({0}) / mount helper failed ({0}): {1}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    Ok(())
}

/// Remove only our retained mount on failure, preserving the original error as well.
/// 失败时仅摘除本方保留的挂载，同时保留原始错误。
fn rollback(context: &Context, tree: &File, path: &Path, error: Error) -> Error {
    match mount_command(context, None, tree, path, "rollback") {
        Ok(()) => error,
        Err(cleanup) => Error::Invalid(format!(
            "{error}; 清理新会话挂载也失败 / cleanup of the new session mount also failed: {cleanup}"
        )),
    }
}

/// Retain the cloned mount until both publication and guest-visible validation succeed.
/// 保留克隆挂载，直至发布及容器可见性校验均成功。
fn publish(context: &Context, resource: &Resource, target: &File) -> Result<()> {
    let tree = mount_helper::prepare(&resource.source, resource.read_only).map_err(|error| {
        Error::Invalid(format!(
            "无法准备会话资源 {0} / cannot prepare session resource {0}: {error}",
            resource.target.display()
        ))
    })?;
    if let Err(error) = mount_command(context, Some(&tree), target, &resource.target, "attach") {
        // output() kills and reaps an unfinished helper before returning an error.
        // output() 返回错误前会终止并回收尚未结束的辅助进程。
        return Err(rollback(context, &tree, &resource.target, error));
    }
    let verification = (|| -> Result<()> {
        let visible = open_at_guest(
            &context.root,
            &resource.target,
            libc::O_PATH | libc::O_NOFOLLOW,
        )?
        .metadata()?;
        let expected = tree.metadata()?;
        if (expected.dev(), expected.ino()) != (visible.dev(), visible.ino()) {
            return Err(Error::Invalid(format!(
                "发布后客体会话绑定挂载不可见: {0} / guest session bind is not visible after publication: {0}",
                resource.target.display()
            )));
        }
        Ok(())
    })();
    if let Err(error) = verification {
        return Err(rollback(context, &tree, &resource.target, error));
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
            "客体会话挂载目标是符号链接 / guest session mount target is a symlink".into(),
        ));
    }
    if (source_metadata.dev(), source_metadata.ino())
        != (target_metadata.dev(), target_metadata.ino())
    {
        // Remove a still-visible previous bind before replacing it. A fresh
        // logind directory has another inode and needs no old-mount removal.
        // 替换前移除仍可见的旧绑定；logind 新目录具有不同 inode，无需移除旧挂载。
        if previous == Some((target_metadata.dev(), target_metadata.ino())) {
            mount_command(context, None, &target, &resource.target, "unmount")?;
            target = open_at_guest(
                &context.root,
                &resource.target,
                libc::O_PATH | libc::O_NOFOLLOW,
            )?;
            target_metadata = target.metadata()?;
            if target_metadata.file_type().is_symlink() {
                return Err(Error::Invalid(
                    "替换后的客体会话挂载目标是符号链接 / replaced guest session mount target is a symlink".into(),
                ));
            }
        }
        publish(context, resource, &target)?;
    } else if resource.read_only {
        mount_command(context, None, &target, &resource.target, "readonly")?;
    }
    Ok(())
}
