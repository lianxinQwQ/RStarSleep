//! Exercise unprivileged resource and environment boundaries.
//! 验证无需特权的资源和环境边界。
use super::resources::{beneath, runtime_directory, socket, xauthority};
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

/// X11 cookie files are optional, but explicit files retain ownership and privacy checks.
/// X11 cookie 文件可选，但显式文件仍须通过属主和私有权限检查。
/// # Arguments
/// No parameters. 无参数。
#[test]
fn optional_xauthority_retains_explicit_file_validation() {
    let uid = unsafe { libc::geteuid() };
    let mut hints = BTreeMap::new();
    assert!(xauthority(uid, &hints).unwrap().is_none());
    hints.insert("XAUTHORITY".into(), String::new());
    assert!(xauthority(uid, &hints).unwrap().is_none());

    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("Xauthority");
    hints.insert("XAUTHORITY".into(), path.to_str().unwrap().into());
    assert!(xauthority(uid, &hints).is_err());
    fs::write(&path, b"cookie").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    let resource = xauthority(uid, &hints).unwrap().unwrap();
    assert_eq!(
        resource.target,
        PathBuf::from(format!("/run/user/{uid}/rstarsleep/Xauthority"))
    );
    assert!(resource.read_only);
    assert_eq!(
        resource.source.metadata().unwrap().ino(),
        fs::metadata(&path).unwrap().ino()
    );
    assert!(xauthority(uid.wrapping_add(1), &hints).is_err());
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(xauthority(uid, &hints).is_err());
    fs::rename(&path, directory.path().join("original")).unwrap();
    symlink("original", &path).unwrap();
    assert!(xauthority(uid, &hints).is_err());
}

/// Wayland and X11 coexist without a fabricated cookie path in either session mode.
/// 两种会话模式下 Wayland 与 X11 可并存，且不会凭空生成 cookie 路径。
/// # Arguments
/// No parameters. 无参数。
#[test]
fn wayland_and_x11_only_forward_explicit_authority() {
    let mut runtime = PreparedRuntime {
        system_id: "test".into(),
        version: "v1".into(),
        root: "/guest".into(),
        machine: "test-graphical-environment".into(),
        config: super::super::SubsystemConfig::default(),
        boot_args: Vec::new(),
    };
    for mode in ["isolated", "native"] {
        runtime.config.session_mode = mode.into();
        for wayland in [false, true] {
            let mut hints = BTreeMap::from([("DISPLAY".into(), ":42".into())]);
            if wayland {
                hints.insert("WAYLAND_DISPLAY".into(), "wayland-1".into());
            }
            for authority in [None, Some(""), Some("/host/session/Xauthority")] {
                if let Some(path) = authority {
                    hints.insert("XAUTHORITY".into(), path.into());
                }
                let flags = command_arguments(&runtime, 0, &hints, &[]).unwrap();
                assert!(flags.contains(&"--setenv=DISPLAY=:42".into()));
                assert_eq!(
                    flags.contains(&"--setenv=WAYLAND_DISPLAY=wayland-1".into()),
                    wayland
                );
                assert!(flags.contains(&format!(
                    "--setenv=XDG_SESSION_TYPE={}",
                    if wayland { "wayland" } else { "x11" }
                )));
                let authority_flags: Vec<_> = flags
                    .iter()
                    .filter(|flag| flag.starts_with("--setenv=XAUTHORITY="))
                    .map(String::as_str)
                    .collect();
                assert_eq!(
                    authority_flags,
                    if authority.is_some_and(|path| !path.is_empty()) {
                        vec!["--setenv=XAUTHORITY=/run/user/0/rstarsleep/Xauthority"]
                    } else {
                        vec![]
                    }
                );
            }
            hints.remove("DISPLAY");
            let flags = command_arguments(&runtime, 0, &hints, &[]).unwrap();
            assert!(
                !flags
                    .iter()
                    .any(|flag| flag.starts_with("--setenv=XAUTHORITY="))
            );
        }
    }
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
