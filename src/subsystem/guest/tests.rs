//! Guest boundary and publication regression tests.
//! 容器边界与发布回归测试。
use super::*;
use super::{
    build::install_packages,
    copy::{CopyBudget, capture_script, child_object, copy_object},
    migration::{Migration, publish, validate_migration},
};
/// Reject guest traversal/control targets and decode actual mount escapes.
/// 拒绝容器跳转及控制目标，并解码实际挂载转义。
/// # Arguments
/// No parameters. 无参数。
#[test]
fn validates_paths_and_receipts() {
    for path in [
        "/",
        "relative",
        "/etc/../root",
        "/run/user/1000",
        "/dev/null",
        "/proc/self/root",
        "/sys",
        "/rstarsleep",
    ] {
        assert!(target_path(Path::new(path)).is_err(), "{path}");
    }
    assert!(target_path(Path::new("/etc/my config'file")).is_ok());
    assert_eq!(
        mount_path(r"/my\040data\134file").unwrap(),
        PathBuf::from("/my data\\file")
    );
    assert!(mount_path(r"/bad\777").is_err());
    assert!(mount_path(r"/bad\00").is_err());
    let volume = Path::new("/manager/systems/s/persistent/guest/k");
    let mut record = Migration {
        schema_version: 1,
        system_id: "s".into(),
        target: "/data".into(),
        source: volume.join("data"),
        directory: true,
        published: true,
    };
    assert!(validate_migration(&record, "s", Path::new("/data"), volume).is_ok());
    record.source = "/etc".into();
    assert!(validate_migration(&record, "s", Path::new("/data"), volume).is_err());
}
/// Pin symlinks as data and refuse special nodes or exhausted copy limits.
/// 将链接固定为数据，并拒绝特殊节点或耗尽的复制限制。
/// # Arguments
/// No parameters. 无参数。
#[test]
fn captured_objects_cannot_escape_or_block() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("regular"), b"hello").unwrap();
    fs::hard_link(source.join("regular"), source.join("hardlink")).unwrap();
    std::os::unix::fs::symlink("/etc/shadow", source.join("link")).unwrap();
    let pinned = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_PATH | libc::O_DIRECTORY)
        .open(&source)
        .unwrap();
    let destination = root.path().join("copied");
    copy_object(&pinned, &destination, &mut CopyBudget::new(100), 0).unwrap();
    assert_eq!(fs::read(destination.join("regular")).unwrap(), b"hello");
    assert!(same_inode(
        &fs::metadata(destination.join("regular")).unwrap(),
        &fs::metadata(destination.join("hardlink")).unwrap()
    ));
    assert_eq!(
        fs::read_link(destination.join("link")).unwrap(),
        PathBuf::from("/etc/shadow")
    );
    let small = root.path().join("small");
    assert!(copy_object(&pinned, &small, &mut CopyBudget::new(1), 0).is_err());
    let fifo = CString::new(source.join("fifo").as_os_str().as_encoded_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
    let object = child_object(&pinned, std::ffi::OsStr::new("fifo")).unwrap();
    assert!(
        copy_object(
            &object,
            &root.path().join("fifo-copy"),
            &mut CopyBudget::new(100),
            0
        )
        .is_err()
    );
    assert!(child_object(&pinned, std::ffi::OsStr::new("../outside")).is_err());
}
/// Preserve a published destination when retrying a failed migration publication.
/// 重试失败的迁移发布时保留已发布目标。
/// # Arguments
/// No parameters. 无参数。
#[test]
fn retry_never_replaces_published_data() {
    let root = tempfile::tempdir().unwrap();
    let stage = root.path().join("stage");
    let target = root.path().join("published");
    fs::create_dir(&stage).unwrap();
    fs::write(stage.join("data"), b"first").unwrap();
    publish(&stage, &target, false).unwrap();
    fs::create_dir(&stage).unwrap();
    fs::write(stage.join("data"), b"second").unwrap();
    assert!(publish(&stage, &target, false).is_err());
    assert_eq!(fs::read(target.join("data")).unwrap(), b"first");
    assert_eq!(fs::read(stage.join("data")).unwrap(), b"second");
}
/// Keep package declarations independent and fail closed on legacy or unmanaged data.
/// 保持包声明独立，并在旧格式或非所属内容上封闭失败。
/// # Arguments
/// No parameters. 无参数。
#[test]
fn package_requests_preserve_other_inputs() {
    let root = tempfile::tempdir().unwrap();
    let store = ConfigStore::new(root.path()).unwrap();
    let layout = store.system_layout("app").unwrap();
    fs::create_dir_all(layout.config.join("build")).unwrap();
    let base =
        "[Config]\nSchemaVersion=1\n[Distribution]\nDistribution=arch\n[Content]\nPackages=base\n";
    fs::write(layout.config.join("build/config.conf"), base).unwrap();
    let inheritance = "# retain formatting\n[Inheritance]\nCopyOnWriteBoot=/var/lib/myapp\n";
    fs::write(layout.config.join("build/inherit.conf"), inheritance).unwrap();
    install_packages(&store, "app", &["firefox".into(), "git".into()]).unwrap();
    install_packages(&store, "app", &["git".into(), "vim".into()]).unwrap();
    assert_eq!(
        store.read_packages("app").unwrap().packages,
        ["base", "firefox", "git", "vim"]
    );
    assert_eq!(
        fs::read_to_string(layout.config.join("build/config.conf")).unwrap(),
        base
    );
    assert_eq!(
        fs::read_to_string(layout.config.join("build/inherit.conf")).unwrap(),
        inheritance
    );
    let before = fs::read(layout.config.join("build/guest-packages.conf")).unwrap();
    for token in [
        "bad name",
        "--root",
        "pkg\n[Distribution]",
        "$(touch marker)",
    ] {
        assert!(install_packages(&store, "app", &[token.into()]).is_err());
    }
    assert_eq!(
        fs::read(layout.config.join("build/guest-packages.conf")).unwrap(),
        before
    );
    fs::write(
        layout.config.join("build/guest-packages.conf"),
        "[Distribution]\nDistribution=evil\n",
    )
    .unwrap();
    assert!(install_packages(&store, "app", &["nano".into()]).is_err());
    fs::remove_file(layout.config.join("build/guest-packages.conf")).unwrap();
    fs::remove_file(layout.config.join("build/config.conf")).unwrap();
    fs::write(
        layout.config.join("build/config.toml"),
        "schema_version=1\ndistribution='arch'\npackages=['base']\n",
    )
    .unwrap();
    assert!(install_packages(&store, "app", &["nano".into()]).is_err());
    assert!(!layout.config.join("build/guest-packages.conf").exists());
}

/// Execute literal generated scripts against private test targets with shell metacharacters.
/// 对含 shell 元字符的私有测试目标执行字面生成脚本。
/// # Arguments
/// No parameters. 无参数。
#[test]
fn captured_build_script_keeps_targets_literal() {
    let root = tempfile::tempdir().unwrap();
    let resource = root.path().join("resource");
    fs::create_dir(&resource).unwrap();
    let staging = root.path().join("staging");
    fs::create_dir(&staging).unwrap();
    fs::write(staging.join("data"), b"saved configuration").unwrap();
    fs::set_permissions(staging.join("data"), fs::Permissions::from_mode(0o640)).unwrap();
    let archive = resource.join("data.tar");
    assert!(
        Command::new("/usr/bin/tar")
            .arg("--create")
            .arg("--file")
            .arg(&archive)
            .arg("--directory")
            .arg(&staging)
            .args(["--", "data"])
            .status()
            .unwrap()
            .success()
    );
    let target = root.path().join("target '$(touch attempted) file");
    let script = capture_script(&target, false).unwrap();
    fs::write(resource.join("main.sh"), script).unwrap();
    let output = Command::new("/usr/bin/bash")
        .arg("--")
        .arg("main.sh")
        .env("TMPDIR", root.path())
        .current_dir(&resource)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(fs::read(&target).unwrap(), b"saved configuration");
    assert_eq!(fs::metadata(&target).unwrap().mode() & 0o777, 0o640);
    assert!(!resource.join("attempted").exists());
}

/// Reject symlink aliases of control paths while preserving ordinary source paths.
/// 拒绝控制路径的符号链接别名，同时保留普通来源路径。
/// # Arguments
/// No parameters. 无参数。
#[test]
fn source_selection_requires_real_guest_paths() {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir_all(root.path().join("etc/data")).unwrap();
    fs::write(root.path().join("etc/data/config"), b"configuration").unwrap();
    std::os::unix::fs::symlink("/etc/data", root.path().join("alias")).unwrap();
    std::os::unix::fs::symlink("/proc", root.path().join("pseudo")).unwrap();
    let directory = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_PATH | libc::O_DIRECTORY)
        .open(root.path())
        .unwrap();
    assert!(
        open_target(&directory, Path::new("/etc/data/config"))
            .unwrap()
            .metadata()
            .unwrap()
            .is_file()
    );
    assert!(open_target(&directory, Path::new("/alias/config")).is_err());
    assert!(open_target(&directory, Path::new("/pseudo/version")).is_err());
    assert!(open_target(&directory, Path::new("/etc/data/../data/config")).is_err());
    let process_file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_PATH)
        .open("/proc/version")
        .unwrap();
    assert!(
        copy_object(
            &process_file,
            &root.path().join("kernel-copy"),
            &mut CopyBudget::new(1024 * 1024),
            0
        )
        .is_err()
    );
    assert!(!root.path().join("kernel-copy").exists());
}

/// Valid long system IDs still fit the kernel's Unix socket address limit.
/// 有效的长系统标识仍满足内核 Unix socket 地址限制。
/// # Arguments
/// No parameters. 无参数。
#[test]
fn endpoint_addresses_fit_the_kernel() {
    let root = tempfile::tempdir().unwrap();
    let store = ConfigStore::new(root.path()).unwrap();
    let long = "s".repeat(64);
    let path = endpoint_path(&store, &long).unwrap();
    assert!(path.as_os_str().len() < 108);
    assert_eq!(endpoint_path(&store, &long).unwrap(), path);
    assert_ne!(endpoint_path(&store, "other").unwrap(), path);
}
