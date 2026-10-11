//! Exercise unprivileged resource and environment boundaries.
//! 验证无需特权的资源和环境边界。
use super::resources::{beneath, runtime_directory, socket};
use super::*;
use std::{
    fs,
    os::{
        fd::AsRawFd,
        unix::{
            fs::{PermissionsExt, symlink},
            net::{UnixListener, UnixStream},
            process::ExitStatusExt,
        },
    },
    process::Command,
};

/// Reject network displays and traversal while retaining normal local hints.
/// 拒绝网络显示和路径穿越，同时保留正常本地提示。
/// # Arguments
/// No parameters. 无参数。
#[test]
fn local_session_hints() {
    assert_eq!(xdisplay(":0.1").unwrap(), "X0");
    for value in ["host:0", ":", ":0.1.2", ":0/../../etc", ":-1"] {
        assert!(xdisplay(value).is_err(), "{value}");
    }
    for value in ["../bus", "foo/bar", ".", "", "wayland-0\n"] {
        assert!(component(value).is_err(), "{value}");
    }
    assert_eq!(component("wayland-0").unwrap(), "wayland-0");
}

/// Descriptor reads reject symlinks and caller-mismatched or public resources.
/// 描述符读取拒绝链接、调用者不匹配或公开资源。
/// # Arguments
/// No parameters. 无参数。
#[test]
fn pinned_session_resources() {
    let directory = tempfile::tempdir().unwrap();
    fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let uid = unsafe { libc::geteuid() };
    let root = runtime_directory(directory.path(), uid).unwrap();
    let socket_path = directory.path().join("bus");
    let _listener = std::os::unix::net::UnixListener::bind(&socket_path).unwrap();
    let source = socket(beneath(&root, Path::new("bus")).unwrap(), uid, false).unwrap();
    let original = source.metadata().unwrap().ino();
    fs::rename(&socket_path, directory.path().join("old-bus")).unwrap();
    let _replacement = std::os::unix::net::UnixListener::bind(&socket_path).unwrap();
    assert_ne!(
        original,
        beneath(&root, Path::new("bus"))
            .unwrap()
            .metadata()
            .unwrap()
            .ino()
    );
    assert_eq!(source.metadata().unwrap().ino(), original);
    symlink("bus", directory.path().join("linked")).unwrap();
    assert!(socket(beneath(&root, Path::new("linked")).unwrap(), uid, false).is_err());
    assert!(beneath(&root, Path::new("../bus")).is_err());
    assert!(runtime_directory(directory.path(), uid.wrapping_add(1)).is_err());
    fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o755)).unwrap();
    assert!(runtime_directory(directory.path(), uid).is_err());
}

/// Headless environment construction neither copies host bus nor loader inputs.
/// 无图形环境构造不复制宿主总线和加载器输入。
/// # Arguments
/// No parameters. 无参数。
#[test]
fn headless_environment_is_filtered() {
    let runtime = PreparedRuntime {
        system_id: "test".into(),
        version: "v1".into(),
        root: "/guest".into(),
        machine: "test-machine".into(),
        config: super::super::SubsystemConfig::default(),
        boot_args: Vec::new(),
    };
    let hints = BTreeMap::from([
        ("LD_PRELOAD".into(), "/host.so".into()),
        ("LD_LIBRARY_PATH".into(), "/host/lib".into()),
        ("PATH".into(), "/host/bin".into()),
        (
            "DBUS_SESSION_BUS_ADDRESS".into(),
            "unix:path=/host/bus".into(),
        ),
        ("TERM".into(), "xterm-256color".into()),
    ]);
    let flags = command_arguments(&runtime, 0, &hints, &[]).unwrap();
    assert!(flags.contains(&"--setenv=TERM=xterm-256color".into()));
    assert!(flags.contains(
        &"--setenv=DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/0/rstarsleep-bus".into()
    ));
    assert!(!flags.iter().any(|flag| flag.contains("host.so")
        || flag.contains("/host/bus")
        || flag.contains("/host/lib")
        || flag.contains("/host/bin")));
    assert!(command_arguments(&runtime, 1000, &hints, &[]).is_err());
    assert!(validate_files(&runtime, &["https://example.com/file".into()]).is_ok());
}

/// A pinned Unix socket address still reaches its inode after its name is replaced.
/// 固定 Unix socket 地址在名称替换后仍连接原 inode。
/// # Arguments
/// No parameters. 无参数。
#[test]
fn native_bus_uses_pinned_socket_and_fixed_policy() {
    let directory = tempfile::tempdir().unwrap();
    fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let uid = unsafe { libc::geteuid() };
    let root = runtime_directory(directory.path(), uid).unwrap();
    let path = directory.path().join("bus");
    let original = UnixListener::bind(&path).unwrap();
    original.set_nonblocking(true).unwrap();
    let bus = socket(beneath(&root, Path::new("bus")).unwrap(), uid, false).unwrap();
    fs::remove_file(&path).unwrap();
    let replacement = UnixListener::bind(&path).unwrap();
    replacement.set_nonblocking(true).unwrap();
    let _stream = UnixStream::connect(format!("/proc/self/fd/{}", bus.as_raw_fd())).unwrap();
    assert!(original.accept().is_ok());
    assert!(replacement.accept().is_err());
    let command = bus::proxy_command(
        &bus,
        uid,
        unsafe { libc::getegid() },
        &directory.path().join("output"),
    );
    let args: Vec<_> = command
        .get_args()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        args[0],
        format!("unix:path=/proc/self/fd/{}", bus.as_raw_fd())
    );
    assert_eq!(
        &args[2..],
        [
            "--filter",
            "--talk=org.freedesktop.IBus",
            "--talk=org.fcitx.Fcitx",
            "--talk=org.fcitx.Fcitx5"
        ]
    );
    assert_eq!(command.get_envs().count(), 1);
    assert_eq!(command.get_envs().next().unwrap().0, "PATH");
}

/// pidfds reject stale lifetime receipts and terminate only the recorded child.
/// pidfd 拒绝过期生命周期记录，仅终止记录的子进程。
/// # Arguments
/// No parameters. 无参数。
#[test]
fn proxy_cleanup_rejects_stale_process_identity() {
    let mut child = Command::new("/bin/sleep").arg("5").spawn().unwrap();
    let mut record = bus::ProxyRecord {
        schema_version: 1,
        uid: unsafe { libc::geteuid() },
        pid: child.id(),
        starttime: bus::starttime(child.id()).unwrap(),
        dev: 0,
        ino: 0,
    };
    assert!(bus::process(&record).unwrap().is_some());
    record.starttime += 1;
    assert!(bus::process(&record).unwrap().is_none());
    bus::terminate(&record).unwrap();
    assert!(child.try_wait().unwrap().is_none());
    record.starttime -= 1;
    record.uid = record.uid.wrapping_add(1);
    assert!(bus::process(&record).unwrap().is_none());
    record.uid = unsafe { libc::geteuid() };
    bus::terminate(&record).unwrap();
    assert_eq!(child.wait().unwrap().signal(), Some(libc::SIGTERM));
    assert!(bus::process(&record).unwrap().is_none());
}
