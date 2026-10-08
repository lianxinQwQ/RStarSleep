//! Verify live data inheritance and per-system persistent mount seeding.
//! 验证运行数据继承及每系统持久挂载初始化。
use super::*;
use rstarsleep::config::inheritance_name;
use std::path::Path;

/// CoW directories are independently snapshotted from live data on every version and cleaned safely.
/// 每个版本从运行数据独立快照 CoW 目录并安全清理。
/// # Arguments
/// No parameters. 无参数。
#[test]
fn cow_directories_and_files_come_from_live_sources_every_time() {
    let fixture = Fixture::new();
    fixture.system("desktop", &["kept"]);
    let source = fixture.store.root().parent().unwrap().join("running-data");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("value"), "first").unwrap();
    let file = fixture.store.root().parent().unwrap().join("running-file");
    fs::write(&file, "file-first").unwrap();
    let layout = fixture.store.system_layout("desktop").unwrap();
    fs::write(
        layout.config.join("build/inherit.conf"),
        format!(
            "[Inheritance]\nCopyOnWrite={}\nCopyOnWrite={}\n",
            source.display(),
            file.display()
        ),
    )
    .unwrap();
    let backend = SimulatedBackend::default();
    let first = build_with_backend(&fixture.store, Some("desktop"), &backend)
        .unwrap()
        .remove(0);
    let relative = source.strip_prefix("/").unwrap();
    let relative_file = file.strip_prefix("/").unwrap();
    assert_eq!(
        fs::read_to_string(first.root.join(relative).join("value")).unwrap(),
        "first"
    );
    backend
        .check_subvolume(&first.root.join(relative), true)
        .unwrap();
    backend
        .check_subvolume(
            &fixture.cache(&first.version, "inherit").join(relative),
            true,
        )
        .unwrap();
    assert!(layout.persistent.read_dir().unwrap().next().is_none());
    fs::write(source.join("value"), "second").unwrap();
    fs::write(&file, "file-second").unwrap();
    let second = build_with_backend(&fixture.store, Some("desktop"), &backend)
        .unwrap()
        .remove(0);
    assert_eq!(
        fs::read_to_string(second.root.join(relative).join("value")).unwrap(),
        "second"
    );
    assert_eq!(
        fs::read_to_string(second.root.join(relative_file)).unwrap(),
        "file-second"
    );
    assert_eq!(
        fs::read_to_string(first.root.join(relative).join("value")).unwrap(),
        "first"
    );
    assert!(!fixture.cache(&first.version, "inherit").exists());
    rstarsleep::controller::clean_with_backend(&fixture.store, "desktop", &first.version, &backend)
        .unwrap();
    assert!(!first.root.exists());
    assert!(second.root.exists());
}

/// Persistent mount data is seeded once and is reused even if the current source disappears.
/// 持久挂载数据只初始化一次，即使当前来源消失也复用。
/// # Arguments
/// No parameters. 无参数。
#[test]
fn mount_data_is_seeded_once_and_reused_without_overwriting_changes() {
    let fixture = Fixture::new();
    fixture.system("desktop", &[]);
    let source = fixture.store.root().parent().unwrap().join("mount-data");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("value"), "live").unwrap();
    let file = fixture.store.root().parent().unwrap().join("mount-file");
    fs::write(&file, "live-file").unwrap();
    let layout = fixture.store.system_layout("desktop").unwrap();
    fs::write(
        layout.config.join("build/inherit.conf"),
        format!(
            "[Inheritance]\nMount={}\nMount={}\n",
            source.display(),
            file.display()
        ),
    )
    .unwrap();
    let backend = SimulatedBackend::default();
    let first = build_with_backend(&fixture.store, Some("desktop"), &backend)
        .unwrap()
        .remove(0);
    let persistent = layout.persistent.join(inheritance_name(&source).unwrap());
    fs::write(persistent.join("value"), "persistent-edited").unwrap();
    fs::remove_dir_all(&source).unwrap();
    let second = build_with_backend(&fixture.store, Some("desktop"), &backend)
        .unwrap()
        .remove(0);
    assert_eq!(
        fs::read_to_string(persistent.join("value")).unwrap(),
        "persistent-edited"
    );
    assert!(
        fs::read_to_string(second.root.join("etc/fstab"))
            .unwrap()
            .contains("UUID=simulated-uuid")
    );
    assert!(
        fs::read_to_string(first.root.join("etc/fstab"))
            .unwrap()
            .contains("bind,x-systemd.requires-mounts-for=/rstarsleep")
    );
    rstarsleep::controller::clean_with_backend(
        &fixture.store,
        "desktop",
        &second.version,
        &backend,
    )
    .unwrap();
    assert!(persistent.exists());
    assert_eq!(
        inheritance_name(Path::new("/home/user")).unwrap(),
        "-home-user"
    );
}

/// Copy failures stop publication but preserve software/script caches and remove scratch subvolumes.
/// 复制失败阻止发布，但保留软件及脚本缓存并删除临时子卷。
/// # Arguments
/// No parameters. 无参数。
#[test]
fn failed_inheritance_keeps_completed_software_and_script_caches() {
    let fixture = Fixture::new();
    fixture.system("desktop", &[]);
    let source = fixture.store.root().parent().unwrap().join("running-data");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("value"), "live").unwrap();
    let layout = fixture.store.system_layout("desktop").unwrap();
    fs::write(
        layout.config.join("build/inherit.conf"),
        format!("[Inheritance]\nCopyOnWrite={}\n", source.display()),
    )
    .unwrap();
    let backend = SimulatedBackend::default();
    backend.fail_inheritance_copy.set(true);
    assert!(build_with_backend(&fixture.store, Some("desktop"), &backend).is_err());
    assert!(fixture.cache("v00000001", "packages").exists());
    assert!(fixture.cache("v00000001", "script").exists());
    assert!(!layout.versions.join("v00000001/root").exists());
    let scratch = fixture.store.root().join("work/desktop/v00000001");
    assert!(!scratch.join("inherit-root").exists());
    assert!(
        !scratch
            .join(format!("inherit-{}", inheritance_name(&source).unwrap()))
            .exists()
    );
}
