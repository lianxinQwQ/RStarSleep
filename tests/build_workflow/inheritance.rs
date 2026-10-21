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
            "[Inheritance]\nCopyOnWriteBuild={}/\nCopyOnWriteBuild={}\n",
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
        .check_subvolume(&first.root.join(relative), false)
        .unwrap();
    backend
        .check_subvolume(
            &fixture.cache(&first.version, "script").join(relative),
            true,
        )
        .unwrap();
    assert!(layout.persistent.read_dir().unwrap().next().is_none());
    fs::write(first.root.join(relative).join("value"), "runtime-edited").unwrap();
    assert_eq!(
        fs::read_to_string(
            fixture
                .cache(&first.version, "script")
                .join(relative)
                .join("value")
        )
        .unwrap(),
        "first"
    );
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
        "runtime-edited"
    );
    assert!(!fixture.cache(&first.version, "script").exists());
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
            "[Inheritance]\nMount={}/\nMount={}\n",
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
    fixture.retire_current("desktop");
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

/// Copy failures preserve software caches, block hooks/scripts, and remove scratch subvolumes.
/// 复制失败保留软件缓存，阻止后处理及脚本执行，并删除临时子卷。
/// # Arguments
/// No parameters. 无参数。
#[test]
fn failed_build_inheritance_keeps_software_cache_and_blocks_hooks_and_scripts() {
    let fixture = Fixture::with_unified_packages_without_predownload();
    fixture.system("desktop", &["editor"]);
    let source = fixture.store.root().parent().unwrap().join("running-data");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("value"), "live").unwrap();
    let layout = fixture.store.system_layout("desktop").unwrap();
    fs::write(
        layout.config.join("build/inherit.conf"),
        format!("[Inheritance]\nCopyOnWriteBuild={}/\n", source.display()),
    )
    .unwrap();
    fs::write(
        layout.config.join("script/check.sh"),
        "printf executed > \"$RSTAR_ROOT/script-ran\"\n",
    )
    .unwrap();
    let backend = SimulatedBackend::default();
    backend.fail_inheritance_copy.set(true);
    assert!(build_with_backend(&fixture.store, Some("desktop"), &backend).is_err());
    assert!(fixture.cache("v00000001", "packages").exists());
    assert!(!fixture.cache("v00000001", "script").exists());
    assert!(!fixture.cache("v00000001", "script.toml").exists());
    assert_eq!(backend.count("post_maintenance"), 0);
    assert_eq!(backend.count("script"), 0);
    assert!(!layout.versions.join("v00000001/root").exists());
    let scratch = fixture.store.root().join("work/desktop/v00000001");
    assert!(!scratch.join("script-root").exists());
    let retried = build_with_backend(&fixture.store, Some("desktop"), &backend)
        .unwrap()
        .remove(0);
    assert_eq!(backend.count("post_maintenance"), 1);
    assert_eq!(backend.count("script"), 1);
    assert_eq!(
        fs::read_to_string(retried.root.join("script-ran")).unwrap(),
        "executed"
    );
    assert_eq!(
        fs::read_to_string(
            retried
                .root
                .join(source.strip_prefix("/").unwrap())
                .join("value")
        )
        .unwrap(),
        "live"
    );
}

#[test]
fn build_inheritance_precedes_hooks_and_scripts_without_entering_software_caches() {
    let fixture = Fixture::with_unified_packages_without_predownload();
    fixture.system("desktop", &["editor"]);
    let layout = fixture.store.system_layout("desktop").unwrap();
    let source = fixture
        .store
        .root()
        .parent()
        .unwrap()
        .join("pre-script-data");
    fs::create_dir(&source).unwrap();
    let relative = source.strip_prefix("/").unwrap();
    fs::write(
        layout.config.join("build/inherit.conf"),
        format!("[Inheritance]\nCopyOnWriteBuild={}/\n", source.display()),
    )
    .unwrap();
    fs::write(
        layout.config.join("script/check.sh"),
        format!(
            "set -eu\ntest \"$(cat \"$RSTAR_ROOT/.post-maintenance-inherited\")\" = \"$(cat \"$RSTAR_ROOT{0}/value\")\"\nprintf script >> \"$RSTAR_ROOT{0}/value\"\n",
            source.display(),
        ),
    ).unwrap();
    let backend = SimulatedBackend::default();
    *backend.post_maintenance_probe.borrow_mut() = Some(relative.join("value"));
    for content in ["first", "second"] {
        fs::write(source.join("value"), content).unwrap();
        let outcome = build_with_backend(&fixture.store, Some("desktop"), &backend)
            .unwrap()
            .remove(0);
        for stage in ["base-system", "packages"] {
            let cache = fixture.cache(&outcome.version, stage);
            assert!(!cache.join(relative).exists());
            assert!(!cache.join(".post-maintenance-inherited").exists());
        }
        for root in [&outcome.root, &fixture.cache(&outcome.version, "script")] {
            assert_eq!(
                fs::read_to_string(root.join(".post-maintenance-inherited")).unwrap(),
                content
            );
            assert_eq!(
                fs::read_to_string(root.join(relative).join("value")).unwrap(),
                format!("{content}script")
            );
        }
        assert!(!fixture.cache(&outcome.version, "inherit").exists());
        assert_eq!(fs::read_to_string(source.join("value")).unwrap(), content);
    }
    let events = backend.events.borrow();
    for operation in ["bootstrap", "base_install"] {
        assert_eq!(
            events
                .iter()
                .filter(|event| event.system_id == "desktop" && event.operation == operation)
                .count(),
            1
        );
    }
    assert_eq!(backend.count("post_maintenance"), 2);
    assert_eq!(backend.count("script"), 2);
}

#[test]
fn inheritance_rejects_mismatched_directory_markers_before_publication() {
    for key in ["CopyOnWriteBuild", "CopyOnWriteBoot", "Mount"] {
        for directory in [true, false] {
            let fixture = Fixture::new();
            fixture.system("desktop", &[]);
            let source = fixture.store.root().parent().unwrap().join("source");
            if directory {
                fs::create_dir(&source).unwrap();
            } else {
                fs::write(&source, "file").unwrap();
            }
            let layout = fixture.store.system_layout("desktop").unwrap();
            fs::write(
                layout.config.join("build/inherit.conf"),
                format!(
                    "[Inheritance]\n{key}={}{}\n",
                    source.display(),
                    if directory { "" } else { "/" },
                ),
            )
            .unwrap();
            let backend = SimulatedBackend::default();
            let result = build_with_backend(&fixture.store, Some("desktop"), &backend);
            let error = if key == "CopyOnWriteBoot" {
                let outcome = result.unwrap().remove(0);
                rstarsleep::controller::finalize_inheritance_with_backend(
                    &outcome
                        .root
                        .parent()
                        .unwrap()
                        .join("inheritance-pending.toml"),
                    std::path::Path::new("/"),
                    &backend,
                )
                .unwrap_err()
                .to_string()
            } else {
                result.unwrap_err().to_string()
            };
            assert!(
                error.contains(if directory {
                    "继承目录路径必须以 / 结尾"
                } else {
                    "继承文件路径不能以 / 结尾"
                }),
                "{key}: {error}"
            );
            assert!(!layout.versions.join("v00000001/root").exists());
        }
    }
}

#[test]
fn reused_mount_checks_persistent_type_even_when_original_source_is_missing() {
    for directory in [true, false] {
        let fixture = Fixture::new();
        fixture.system("desktop", &[]);
        let source = fixture.store.root().parent().unwrap().join("mount-source");
        if directory {
            fs::create_dir(&source).unwrap();
            fs::write(source.join("value"), "kept").unwrap();
        } else {
            fs::write(&source, "kept").unwrap();
        }
        let layout = fixture.store.system_layout("desktop").unwrap();
        let config = layout.config.join("build/inherit.conf");
        fs::write(
            &config,
            format!(
                "[Inheritance]\nMount={}{}\n",
                source.display(),
                if directory { "/" } else { "" },
            ),
        )
        .unwrap();
        let backend = SimulatedBackend::default();
        build_with_backend(&fixture.store, Some("desktop"), &backend).unwrap();
        let persistent = layout.persistent.join(inheritance_name(&source).unwrap());
        if directory {
            fs::remove_dir_all(&source).unwrap();
        } else {
            fs::remove_file(&source).unwrap();
        }
        fs::write(
            &config,
            format!(
                "[Inheritance]\nMount={}{}\n",
                source.display(),
                if directory { "" } else { "/" },
            ),
        )
        .unwrap();
        let error = build_with_backend(&fixture.store, Some("desktop"), &backend)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains(if directory {
                "继承目录路径必须以 / 结尾"
            } else {
                "继承文件路径不能以 / 结尾"
            }),
            "{error}"
        );
        let data = if directory {
            persistent.join("value")
        } else {
            persistent
        };
        assert_eq!(fs::read_to_string(data).unwrap(), "kept");
        assert!(!layout.versions.join("v00000002/root").exists());
    }
}
