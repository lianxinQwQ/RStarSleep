//! Verify selected inheritance roots preserve guest paths and stage boundaries.
//! 验证选定继承来源根保留客体路径与阶段边界。
use super::*;
use rstarsleep::{
    config::inheritance_name,
    controller::{
        BuildOptions, BuildOutcome, build_with_options_and_backend_and_warning,
        finalize_inheritance_with_backend,
    },
};
use std::path::{Path, PathBuf};

fn source_root(fixture: &Fixture, name: &str) -> PathBuf {
    let root = fixture.store.root().parent().unwrap().join(name);
    fs::create_dir(&root).unwrap();
    root
}

fn configure(fixture: &Fixture, text: &str) {
    fs::write(
        fixture
            .store
            .system_layout("desktop")
            .unwrap()
            .config
            .join("build/inherit.conf"),
        text,
    )
    .unwrap();
}

fn build(
    fixture: &Fixture,
    backend: &SimulatedBackend,
    root: &Path,
) -> rstarsleep::controller::Result<BuildOutcome> {
    build_with_options_and_backend_and_warning(
        &fixture.store,
        Some("desktop"),
        &BuildOptions {
            inheritance_root: root.to_path_buf(),
        },
        backend,
        |_| Ok(()),
    )
    .map(|mut outcomes| outcomes.remove(0))
}

fn assert_no_build(fixture: &Fixture, backend: &SimulatedBackend) {
    assert!(backend.events.borrow().is_empty());
    assert!(backend.snapshots.borrow().is_empty());
    assert_eq!(
        fs::read_dir(fixture.store.system_layout("desktop").unwrap().versions)
            .unwrap()
            .count(),
        0
    );
}

#[test]
fn build_inheritance_resolves_guest_paths_and_invalidates_only_script_on_source_changes() {
    let fixture = Fixture::new();
    fixture.system("desktop", &["editor"]);
    fs::write(
        fixture.store.global_config_path(),
        "schema_version=1\ncache_reuse_minutes=60\n",
    )
    .unwrap();
    let first_root = source_root(&fixture, "first-source");
    let second_root = source_root(&fixture, "second-source");
    for (root, value) in [(&first_root, "first"), (&second_root, "second")] {
        fs::create_dir_all(root.join("rstar-test/data")).unwrap();
        fs::write(root.join("rstar-test/data/value"), value).unwrap();
        fs::write(root.join("rstar-test/file"), value).unwrap();
    }
    configure(
        &fixture,
        "[Inheritance]\nCopyOnWriteBuild=/rstar-test/data/\nCopyOnWriteBuild=/rstar-test/file\n",
    );
    let backend = SimulatedBackend::default();
    let first = build(&fixture, &backend, &first_root).unwrap();
    for path in ["rstar-test/data/value", "rstar-test/file"] {
        assert_eq!(fs::read_to_string(first.root.join(path)).unwrap(), "first");
    }
    assert!(
        !first
            .root
            .join(first_root.strip_prefix("/").unwrap())
            .exists()
    );
    backend.events.borrow_mut().clear();
    build(&fixture, &backend, &first_root).unwrap();
    assert_eq!(backend.count("post_maintenance"), 0);

    let second = build(&fixture, &backend, &second_root).unwrap();
    assert_eq!(backend.count("bootstrap"), 0);
    assert_eq!(backend.count("base_install"), 0);
    assert_eq!(backend.count("update"), 0);
    assert_eq!(backend.count("install"), 0);
    assert_eq!(backend.count("post_maintenance"), 1);
    for path in ["rstar-test/data/value", "rstar-test/file"] {
        assert_eq!(
            fs::read_to_string(second.root.join(path)).unwrap(),
            "second"
        );
        assert_eq!(fs::read_to_string(first.root.join(path)).unwrap(), "first");
    }
    for stage in ["base-system", "packages"] {
        assert_eq!(
            record(&fixture.cache(&second.version, &format!("{stage}.toml")))["origin_version"]
                .as_str(),
            Some(first.version.as_str())
        );
    }
    fs::write(second_root.join("rstar-test/file"), "third").unwrap();
    let third = build(&fixture, &backend, &second_root).unwrap();
    assert_eq!(backend.count("post_maintenance"), 2);
    assert_eq!(backend.count("update"), 0);
    assert_eq!(
        fs::read_to_string(third.root.join("rstar-test/file")).unwrap(),
        "third"
    );
}

#[test]
fn persistent_mounts_seed_from_selected_root_once_and_keep_guest_targets() {
    let fixture = Fixture::new();
    fixture.system("desktop", &[]);
    let first_root = source_root(&fixture, "first-source");
    let second_root = source_root(&fixture, "second-source");
    for (root, value) in [(&first_root, "first"), (&second_root, "second")] {
        fs::create_dir_all(root.join("rstar-test/data")).unwrap();
        fs::write(root.join("rstar-test/data/value"), value).unwrap();
        fs::write(root.join("rstar-test/file"), value).unwrap();
    }
    configure(
        &fixture,
        "[Inheritance]\nMount=/rstar-test/data/\nMount=/rstar-test/file\n",
    );
    let backend = SimulatedBackend::default();
    let first = build(&fixture, &backend, &first_root).unwrap();
    let layout = fixture.store.system_layout("desktop").unwrap();
    let directory = layout
        .persistent
        .join(inheritance_name(Path::new("/rstar-test/data/")).unwrap());
    let file = layout
        .persistent
        .join(inheritance_name(Path::new("/rstar-test/file")).unwrap());
    assert_eq!(
        fs::read_to_string(directory.join("value")).unwrap(),
        "first"
    );
    assert_eq!(fs::read_to_string(&file).unwrap(), "first");
    fs::write(directory.join("value"), "runtime-directory").unwrap();
    fs::write(&file, "runtime-file").unwrap();
    let second = build(&fixture, &backend, &second_root).unwrap();
    assert_eq!(
        fs::read_to_string(directory.join("value")).unwrap(),
        "runtime-directory"
    );
    assert_eq!(fs::read_to_string(&file).unwrap(), "runtime-file");
    for outcome in [first, second] {
        let fstab = fs::read_to_string(outcome.root.join("etc/fstab")).unwrap();
        assert!(fstab.contains(" /rstar-test/data/ "), "{fstab}");
        assert!(fstab.contains(" /rstar-test/file "), "{fstab}");
        assert!(!fstab.contains(first_root.to_str().unwrap()));
        assert!(!fstab.contains(second_root.to_str().unwrap()));
        assert!(
            !outcome
                .root
                .join(first_root.strip_prefix("/").unwrap())
                .exists()
        );
    }
}

#[test]
fn boot_manifest_pins_selected_directory_and_reads_its_latest_guest_data() {
    let fixture = Fixture::new();
    fixture.system("desktop", &[]);
    let selected = source_root(&fixture, "selected-source");
    let host = source_root(&fixture, "host-source");
    for (root, value) in [(&selected, "selected-at-build"), (&host, "host-value")] {
        fs::create_dir_all(root.join("rstar-test/data")).unwrap();
        fs::write(root.join("rstar-test/data/value"), value).unwrap();
    }
    configure(
        &fixture,
        "[Inheritance]\nCopyOnWriteBoot=/rstar-test/data/\n",
    );
    let backend = SimulatedBackend::default();
    backend
        .filesystem_locations
        .borrow_mut()
        .insert(PathBuf::from("/"), ("host-uuid".into(), host.clone()));
    backend.filesystem_locations.borrow_mut().insert(
        selected.clone(),
        ("simulated-uuid".into(), selected.clone()),
    );
    backend
        .source_ids
        .borrow_mut()
        .insert(selected.clone(), "selected-subvolume-id".into());
    let outcome = build(&fixture, &backend, &selected).unwrap();
    let manifest_path = outcome
        .root
        .parent()
        .unwrap()
        .join("inheritance-pending.toml");
    let manifest = record(&manifest_path);
    assert_eq!(
        manifest["source_root"]["filesystem_uuid"].as_str(),
        Some("simulated-uuid")
    );
    assert_eq!(manifest["source_root"]["path"].as_str(), selected.to_str());
    assert_eq!(
        manifest["source_root"]["subvolume_id"].as_str(),
        Some("selected-subvolume-id")
    );
    assert_eq!(
        manifest["sources"][0]["path"].as_str(),
        Some("/rstar-test/data/")
    );
    assert_eq!(
        manifest["sources"][0]["target"].as_str(),
        Some("/rstar-test/data/")
    );
    assert!(!selected.join(MARKER).exists());
    assert!(!outcome.root.join("rstar-test/data").exists());
    fs::write(selected.join("rstar-test/data/value"), "selected-at-boot").unwrap();
    backend
        .filesystem_locations
        .borrow_mut()
        .remove(Path::new("/"));
    finalize_inheritance_with_backend(&manifest_path, Path::new("/"), &backend).unwrap();
    assert_eq!(
        fs::read_to_string(
            outcome
                .root
                .parent()
                .unwrap()
                .join("root/rstar-test/data/value")
        )
        .unwrap(),
        "selected-at-boot"
    );
    assert_eq!(
        fs::read_to_string(host.join("rstar-test/data/value")).unwrap(),
        "host-value"
    );
}

#[test]
fn invalid_roots_fail_before_allocating_versions_or_running_plugins() {
    for invalid in ["missing", "file", "relative"] {
        let fixture = Fixture::new();
        fixture.system("desktop", &["editor"]);
        let root = if invalid == "relative" {
            PathBuf::from("relative-source")
        } else {
            fixture.store.root().parent().unwrap().join(invalid)
        };
        if invalid == "file" {
            fs::write(&root, "not a directory").unwrap();
        }
        let backend = SimulatedBackend::default();
        let error = build_with_options_and_backend_and_warning(
            &fixture.store,
            Some("desktop"),
            &BuildOptions {
                inheritance_root: root,
            },
            &backend,
            |_| {
                panic!(
                    "无效根必须在警告回调前失败 / Invalid root must fail before warning callback"
                )
            },
        )
        .unwrap_err();
        assert!(!error.to_string().is_empty());
        assert_no_build(&fixture, &backend);
        assert!(backend.filesystem_inspections.borrow().is_empty());
    }
}

#[test]
fn invalid_build_sources_under_selected_root_fail_preflight_without_host_fallback() {
    for invalid in ["missing", "file-marked-directory", "directory-marked-file"] {
        let fixture = Fixture::new();
        fixture.system("desktop", &["editor"]);
        let selected = source_root(&fixture, "selected-source");
        let guest = match invalid {
            "file-marked-directory" => {
                fs::write(selected.join("var"), "not a directory").unwrap();
                "/var/"
            }
            "directory-marked-file" => {
                fs::create_dir(selected.join("var")).unwrap();
                "/var"
            }
            _ => "/var/",
        };
        configure(
            &fixture,
            &format!("[Inheritance]\nCopyOnWriteBuild={guest}\n"),
        );
        let backend = SimulatedBackend::default();
        let error = build(&fixture, &backend, &selected).unwrap_err();
        assert!(!error.to_string().is_empty());
        assert_no_build(&fixture, &backend);
        assert!(
            !backend
                .cow_checks
                .borrow()
                .iter()
                .any(|path| path == Path::new("/var"))
        );
    }
}

#[test]
fn explicit_boot_source_on_another_filesystem_fails_before_build() {
    let fixture = Fixture::new();
    fixture.system("desktop", &["editor"]);
    let selected = source_root(&fixture, "selected-source");
    configure(&fixture, "[Inheritance]\nCopyOnWriteBoot=/var/\n");
    let backend = SimulatedBackend::default();
    backend.filesystem_locations.borrow_mut().insert(
        selected.clone(),
        ("foreign-source-uuid".into(), selected.clone()),
    );
    let error = build_with_options_and_backend_and_warning(
        &fixture.store,
        Some("desktop"),
        &BuildOptions {
            inheritance_root: selected,
        },
        &backend,
        |_| panic!("跨文件系统来源必须提前拒绝 / Cross-filesystem source must be rejected early"),
    )
    .unwrap_err();
    assert!(error.to_string().contains("UUID"), "{error}");
    assert_no_build(&fixture, &backend);
}

#[test]
fn invalid_boot_root_identity_fails_before_warning_callback_and_allocation() {
    use std::{ffi::OsString, os::unix::ffi::OsStringExt};

    for invalid in [
        "non-utf8-path",
        "relative-path",
        "parent-component",
        "empty-uuid",
        "empty-id",
    ] {
        let fixture = Fixture::new();
        fixture.system("desktop", &["editor"]);
        let selected = source_root(&fixture, "selected-source");
        configure(
            &fixture,
            "[Inheritance]\nCopyOnWriteBoot=/rstar-test/data/\n",
        );
        let backend = SimulatedBackend::default();
        let location = match invalid {
            "non-utf8-path" => PathBuf::from(OsString::from_vec(b"/source-\xff".to_vec())),
            "relative-path" => PathBuf::from("relative-source"),
            "parent-component" => PathBuf::from("/old-source/../source"),
            _ => selected.clone(),
        };
        let uuid = if invalid == "empty-uuid" {
            String::new()
        } else {
            "simulated-uuid".into()
        };
        backend
            .filesystem_locations
            .borrow_mut()
            .insert(selected.clone(), (uuid, location));
        if invalid == "empty-id" {
            backend
                .source_ids
                .borrow_mut()
                .insert(selected.clone(), String::new());
        }
        let error = build_with_options_and_backend_and_warning(
            &fixture.store,
            Some("desktop"),
            &BuildOptions {
                inheritance_root: selected,
            },
            &backend,
            |_| {
                panic!(
                    "无效来源身份必须在警告回调前失败 / Invalid source identity must fail before warning callback"
                )
            },
        )
        .unwrap_err()
        .to_string();
        assert!(
            error.contains("Cannot pin the selected boot inheritance source root"),
            "{invalid}: {error}"
        );
        assert_no_build(&fixture, &backend);
    }
}

#[test]
fn invalid_new_mount_sources_fail_before_warning_callback_or_any_build_work() {
    for invalid in ["missing", "file-marked-directory", "directory-marked-file"] {
        let fixture = Fixture::new();
        fixture.system("desktop", &["editor"]);
        let selected = source_root(&fixture, "selected-source");
        let guest = match invalid {
            "file-marked-directory" => {
                fs::write(selected.join("var"), "not a directory").unwrap();
                "/var/"
            }
            "directory-marked-file" => {
                fs::create_dir(selected.join("var")).unwrap();
                "/var"
            }
            _ => "/var/",
        };
        configure(&fixture, &format!("[Inheritance]\nMount={guest}\n"));
        let backend = SimulatedBackend::default();
        let error = build_with_options_and_backend_and_warning(
            &fixture.store,
            Some("desktop"),
            &BuildOptions {
                inheritance_root: selected,
            },
            &backend,
            |_| {
                panic!(
                    "无效挂载来源必须在警告回调前失败 / Invalid mount source must fail before warning callback"
                )
            },
        )
        .unwrap_err();
        assert!(!error.to_string().is_empty());
        assert_no_build(&fixture, &backend);
    }
}

#[test]
fn selected_system_caches_cannot_supply_inheritance_roots_that_build_would_retire() {
    for directive in ["CopyOnWriteBuild", "CopyOnWriteBoot"] {
        for selection in [Some("desktop"), None] {
            let fixture = Fixture::new();
            fixture.system("desktop", &["editor"]);
            fixture.system("secondary", &["editor"]);
            let owner = if selection.is_some() {
                "desktop"
            } else {
                "secondary"
            };
            let backend = SimulatedBackend::default();
            let first = build_with_backend(&fixture.store, Some(owner), &backend)
                .unwrap()
                .remove(0);
            let layout = fixture.store.system_layout(owner).unwrap();
            let selected = layout.cache.join(&first.version).join("script");
            let original = fs::read(selected.join(".post-maintenance")).unwrap();
            for system in ["desktop", "secondary"] {
                fs::write(
                    fixture
                        .store
                        .system_layout(system)
                        .unwrap()
                        .config
                        .join("build/inherit.conf"),
                    format!("[Inheritance]\n{directive}=/etc/\n"),
                )
                .unwrap();
            }
            backend.events.borrow_mut().clear();
            backend.snapshots.borrow_mut().clear();
            let error = build_with_options_and_backend_and_warning(
                &fixture.store,
                selection,
                &BuildOptions {
                    inheritance_root: selected.clone(),
                },
                &backend,
                |_| {
                    panic!(
                        "缓存来源根必须在警告回调前拒绝 / Cache source roots must be rejected before warning callback"
                    )
                },
            )
            .unwrap_err()
            .to_string();
            assert!(error.contains("缓存") && error.contains("cache"), "{error}");
            assert!(backend.events.borrow().is_empty());
            assert!(backend.snapshots.borrow().is_empty());
            assert!(selected.is_dir());
            assert_eq!(
                fs::read(selected.join(".post-maintenance")).unwrap(),
                original
            );
            for system in ["desktop", "secondary"] {
                assert_eq!(
                    fs::read_dir(fixture.store.system_layout(system).unwrap().versions)
                        .unwrap()
                        .count(),
                    usize::from(system == owner)
                );
            }
        }
    }
}
