//! Exercise the real session-mount helper with inherited, pinned file descriptors.
//! 使用继承的固定文件描述符验证实际会话挂载辅助进程。
use std::{
    fs::{self, File, OpenOptions},
    io::{BufRead, BufReader},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{
            fs::{MetadataExt, OpenOptionsExt, symlink},
            net::{UnixListener, UnixStream},
            process::CommandExt,
        },
    },
    path::{Path, PathBuf},
    process::{Child, Command, Output, Stdio},
};

/// Open socket and file identities without following the final component.
/// 打开 socket 和文件身份，不跟随最后一级链接。
fn pin(path: &Path) -> File {
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_PATH | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .unwrap()
}

/// Keep a detached mount reference in the parent while the real helper attaches it.
/// 在父进程中保留分离挂载引用，由实际辅助进程接入。
fn clone_mount(source: &File) -> File {
    let fd = unsafe {
        libc::syscall(
            libc::SYS_open_tree,
            source.as_raw_fd(),
            c"".as_ptr(),
            libc::AT_EMPTY_PATH as libc::c_uint | libc::OPEN_TREE_CLONE | libc::OPEN_TREE_CLOEXEC,
        )
    };
    assert!(
        fd >= 0,
        "无法克隆测试挂载 / Cannot clone test mount: {}",
        std::io::Error::last_os_error()
    );
    unsafe { File::from_raw_fd(fd as i32) }
}

/// Invoke the installed command dispatcher with only the required descriptors inherited.
/// 调用实际命令分派，仅继承必需的描述符。
fn invoke(operation: &str, namespace: &File, source: Option<&File>, target: &File) -> Output {
    let fds = [
        Some(namespace.as_raw_fd()),
        source.map(AsRawFd::as_raw_fd),
        Some(target.as_raw_fd()),
    ];
    let mut command = Command::new(env!("CARGO_BIN_EXE_RStarSleep"));
    command
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .args(["__session-mount", operation])
        .arg(namespace.as_raw_fd().to_string())
        .arg(source.map_or_else(|| "-".into(), |file| file.as_raw_fd().to_string()))
        .arg(target.as_raw_fd().to_string());
    unsafe {
        command.pre_exec(move || {
            for fd in fds.into_iter().flatten() {
                let flags = libc::fcntl(fd, libc::F_GETFD);
                if flags < 0 || libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
            }
            Ok(())
        });
    }
    command.output().unwrap()
}

/// Require a successful helper, retaining its bilingual diagnostics on failure.
/// 要求辅助进程成功，失败时保留双语诊断。
fn success(output: Output) {
    assert!(
        output.status.success(),
        "挂载辅助进程失败 / Mount helper failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Invalid operations and symlink endpoints fail before namespace or mount changes.
/// 无效操作和链接端点在命名空间或挂载变化前失败。
#[test]
fn helper_rejects_invalid_requests_before_mounting() {
    for arguments in [
        vec![],
        vec!["unknown", "3", "4", "5"],
        vec!["bind", "0", "1", "2"],
        vec!["bind", "-1", "-1", "-1"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_RStarSleep"))
            .arg("__session-mount")
            .args(arguments)
            .output()
            .unwrap();
        assert!(!output.status.success());
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(
            error.contains("会话挂载") && error.contains("session mount"),
            "{error}"
        );
    }
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("file");
    fs::write(&path, b"original").unwrap();
    let link = temporary.path().join("link");
    symlink(&path, &link).unwrap();
    let namespace = File::open("/proc/self/ns/mnt").unwrap();
    let file = pin(&path);
    let linked = pin(&link);
    for (source, target) in [(&linked, &file), (&file, &linked)] {
        let output = invoke("bind", &namespace, Some(source), target);
        assert!(!output.status.success());
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(
            error.contains("符号链接") && error.contains("symlink"),
            "{error}"
        );
    }
    let output = invoke("bind", &namespace, Some(&file), &file);
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success() && error.contains("distinct"),
        "{error}"
    );
    assert_eq!(fs::read(&path).unwrap(), b"original");
}

/// Hold an independent guest mount namespace whose runtime directory is a fresh tmpfs.
/// 保持一个独立容器挂载命名空间，其运行目录使用新的 tmpfs。
struct Guest {
    child: Child,
    namespace: File,
    directory: PathBuf,
}

impl Guest {
    fn new(directory: &Path) -> Self {
        fs::create_dir(directory).unwrap();
        let mut child = Command::new("unshare")
            .args(["--mount", "--propagation", "private", "--", "/bin/sh", "-eu", "-c"])
            .arg("mount -t tmpfs -o size=1m,nosuid,nodev tmpfs \"$1\"\ntouch \"$1/socket\" \"$1/auth\" \"$1/readonly\" \"$1/replaced\"\nprintf 'ready\\n'\nread -r done\n")
            .arg("session-mount-test")
            .arg(directory)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut ready = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut ready)
            .unwrap();
        if ready != "ready\n" {
            let _ = child.kill();
            let _ = child.wait();
            panic!("容器测试命名空间未就绪 / Guest test namespace did not become ready");
        }
        let namespace = File::open(format!("/proc/{}/ns/mnt", child.id())).unwrap();
        let directory = PathBuf::from(format!("/proc/{}/root", child.id()))
            .join(directory.strip_prefix("/").unwrap());
        Self {
            child,
            namespace,
            directory,
        }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.directory.join(name)
    }
}

impl Drop for Guest {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Prove cross-namespace socket identity, atomic read-only publication and precise removal.
/// 验证跨命名空间 socket 身份、原子只读发布及准确摘除。
#[test]
#[ignore = "需要非特权 user/mount 命名空间 / requires unprivileged user/mount namespaces"]
fn pinned_session_mounts_cross_namespaces() {
    if std::env::var_os("RSTAR_SESSION_MOUNT_WORKER").is_none() {
        let output = Command::new("unshare")
            .args([
                "--user",
                "--map-root-user",
                "--mount",
                "--propagation",
                "private",
                "--",
            ])
            .arg(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "pinned_session_mounts_cross_namespaces",
                "--ignored",
                "--nocapture",
            ])
            .env("RSTAR_SESSION_MOUNT_WORKER", "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "隔离挂载测试失败 / Isolated mount test failed:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let temporary = tempfile::tempdir().unwrap();
    let original_namespace = fs::metadata("/proc/self/ns/mnt").unwrap().ino();
    let guest = Guest::new(&temporary.path().join("guest"));
    assert_ne!(
        original_namespace,
        guest.namespace.metadata().unwrap().ino()
    );

    let source_path = temporary.path().join("wayland-test");
    let listener = UnixListener::bind(&source_path).unwrap();
    listener.set_nonblocking(true).unwrap();
    let source = pin(&source_path);
    fs::rename(&source_path, temporary.path().join("old-wayland")).unwrap();
    let replacement = UnixListener::bind(&source_path).unwrap();
    replacement.set_nonblocking(true).unwrap();
    let target_path = guest.path("socket");
    let target = pin(&target_path);
    let placeholder = target.metadata().unwrap().ino();
    success(invoke("bind", &guest.namespace, Some(&source), &target));
    assert_eq!(
        fs::metadata(&target_path).unwrap().ino(),
        source.metadata().unwrap().ino()
    );
    let _connection = UnixStream::connect(&target_path).unwrap();
    assert!(listener.accept().is_ok());
    assert_eq!(
        replacement.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
    success(invoke(
        "unmount",
        &guest.namespace,
        None,
        &pin(&target_path),
    ));
    assert_eq!(fs::metadata(&target_path).unwrap().ino(), placeholder);

    let authority_path = temporary.path().join("Xauthority");
    fs::write(&authority_path, b"cookie").unwrap();
    let authority = pin(&authority_path);
    let readonly_path = guest.path("auth");
    success(invoke(
        "bind-readonly",
        &guest.namespace,
        Some(&authority),
        &pin(&readonly_path),
    ));
    assert_eq!(fs::read(&readonly_path).unwrap(), b"cookie");
    assert_eq!(
        fs::write(&readonly_path, b"bad")
            .unwrap_err()
            .raw_os_error(),
        Some(libc::EROFS)
    );
    fs::write(&authority_path, b"source stays writable").unwrap();
    assert_eq!(fs::read(&readonly_path).unwrap(), b"source stays writable");
    success(invoke(
        "unmount",
        &guest.namespace,
        None,
        &pin(&readonly_path),
    ));

    let rebound_path = guest.path("readonly");
    success(invoke(
        "bind",
        &guest.namespace,
        Some(&authority),
        &pin(&rebound_path),
    ));
    success(invoke(
        "readonly",
        &guest.namespace,
        None,
        &pin(&rebound_path),
    ));
    assert_eq!(
        fs::write(&rebound_path, b"bad").unwrap_err().raw_os_error(),
        Some(libc::EROFS)
    );
    success(invoke(
        "unmount",
        &guest.namespace,
        None,
        &pin(&rebound_path),
    ));

    let replaced_path = guest.path("replaced");
    let stale = pin(&replaced_path);
    fs::remove_file(&replaced_path).unwrap();
    fs::write(&replaced_path, b"replacement").unwrap();
    let output = invoke("bind", &guest.namespace, Some(&source), &stale);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("move_mount"));
    assert_eq!(fs::read(&replaced_path).unwrap(), b"replacement");

    let output = invoke(
        "bind-readonly",
        &authority,
        Some(&source),
        &pin(&replaced_path),
    );
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("setns"));
    assert_eq!(fs::read(&replaced_path).unwrap(), b"replacement");
    fs::write(&authority_path, b"still writable").unwrap();
    let output = invoke("unmount", &guest.namespace, None, &pin(&replaced_path));
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("umount2"));
    let output = invoke("readonly", &guest.namespace, None, &pin(&replaced_path));
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("mount_setattr"));
    fs::write(&replaced_path, b"parent stays writable").unwrap();
    fs::write(guest.path("after-failed-readonly"), b"writable tmpfs").unwrap();

    // Retain the exact mount even when the guest moves its parent and replaces the path.
    // 即使容器移动其父目录并替换路径，仍保留准确的挂载对象。
    let moving = guest.path("moving");
    let moved = guest.path("moved");
    fs::create_dir(&moving).unwrap();
    let moving_path = moving.join("socket");
    fs::write(&moving_path, b"original placeholder").unwrap();
    let tree = clone_mount(&source);
    success(invoke(
        "attach",
        &guest.namespace,
        Some(&tree),
        &pin(&moving_path),
    ));
    fs::rename(&moving, &moved).unwrap();
    fs::create_dir(&moving).unwrap();
    fs::write(&moving_path, b"replacement placeholder").unwrap();
    let replacement_tree = clone_mount(&authority);
    success(invoke(
        "attach",
        &guest.namespace,
        Some(&replacement_tree),
        &pin(&moving_path),
    ));
    success(invoke("rollback", &guest.namespace, None, &tree));
    assert_eq!(
        fs::read(moved.join("socket")).unwrap(),
        b"original placeholder"
    );
    assert_eq!(fs::read(&moving_path).unwrap(), b"still writable");
    success(invoke("rollback", &guest.namespace, None, &tree));
    success(invoke(
        "rollback",
        &guest.namespace,
        None,
        &replacement_tree,
    ));
    assert_eq!(fs::read(&moving_path).unwrap(), b"replacement placeholder");

    // Failed publication and never-published trees can be cleaned without touching replacements.
    // 发布失败及从未发布的挂载树可被清理，不会影响替换对象。
    let tree = clone_mount(&source);
    let output = invoke("attach", &guest.namespace, Some(&tree), &stale);
    assert!(!output.status.success());
    success(invoke("rollback", &guest.namespace, None, &tree));
    assert_eq!(fs::read(&replaced_path).unwrap(), b"parent stays writable");
    assert_eq!(
        fs::metadata("/proc/self/ns/mnt").unwrap().ino(),
        original_namespace
    );
}
