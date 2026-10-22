//! Verify missing sources are diagnosed before building and never feed stale inheritance.
//! 验证缺失来源在构建前诊断，且不会使用过期继承内容。
use super::*;
use rstarsleep::controller::{BuildOptions, build_with_backend_and_warning};

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

#[test]
fn no_source_warns_before_operations_and_skips_all_inheritance_but_keeps_fstab() {
    let fixture = Fixture::new();
    fixture.system("desktop", &["linux", "mkinitcpio"]);
    fs::write(
        fixture.store.global_config_path(),
        "schema_version=1\nmain_system='desktop'\ncache_reuse_minutes=60\n",
    )
    .unwrap();
    let missing = fixture
        .store
        .root()
        .parent()
        .unwrap()
        .join("missing-account-input");
    fixture.account_plugin(
        &missing,
        "invalid protocol must never be read",
        "invalid protocol",
    );
    let text = "[Inheritance]\nCopyOnWriteBuild=/missing-build\nCopyOnWriteBoot=/missing-boot/\nMount=/missing-mount/\n[AccountInheritance]\nUsers=alice\n[Content]\nFstabLines=tmpfs /tmp tmpfs defaults 0 0\n";
    configure(&fixture, text);
    let layout = fixture.store.system_layout("desktop").unwrap();
    let backend = SimulatedBackend::default();
    let mut reports = 0;
    let outcome =
        build_with_backend_and_warning(&fixture.store, Some("desktop"), &backend, |warnings| {
            reports += 1;
            assert_eq!(warnings.len(), 1);
            assert!(warnings[0].contains("系统 desktop 具有继承项"));
            assert!(warnings[0].contains("inheritance will be skipped"));
            assert!(backend.events.borrow().is_empty());
            assert!(backend.snapshots.borrow().is_empty());
            assert!(backend.filesystem_inspections.borrow().is_empty());
            assert_eq!(fs::read_dir(&layout.versions).unwrap().count(), 0);
            assert!(!layout.root.join("version-counter").exists());
            Ok(())
        })
        .unwrap()
        .remove(0);
    assert_eq!(reports, 1);
    assert_eq!(outcome.root.file_name().unwrap(), "root");
    assert_eq!(
        record(&outcome.root.parent().unwrap().join("state.toml"))["state"].as_str(),
        Some("complete")
    );
    assert!(!outcome.root.join("missing-build").exists());
    assert!(!outcome.root.join("missing-boot").exists());
    assert!(!outcome.root.join("missing-mount").exists());
    assert!(!outcome.root.join(".accounts").exists());
    assert!(outcome.root.join(".post-maintenance").is_file());
    assert!(outcome.root.join("etc/fstab").is_file());
    assert_eq!(backend.count("mount_inheritance"), 1);
    assert_eq!(backend.count("prepare_inheritance_initramfs"), 0);
    assert!(
        !outcome
            .root
            .parent()
            .unwrap()
            .join("inheritance-pending.toml")
            .exists()
    );
    assert_eq!(fs::read_dir(&layout.persistent).unwrap().count(), 0);
    assert_eq!(
        fs::read_to_string(layout.config.join("build/inherit.conf")).unwrap(),
        text
    );
    assert_eq!(
        fs::read_to_string(
            outcome
                .root
                .parent()
                .unwrap()
                .join("config/build/inherit.conf")
        )
        .unwrap(),
        text
    );
}

#[test]
fn switching_between_absent_and_present_sources_invalidates_script_reuse() {
    let fixture = Fixture::new();
    fixture.system("desktop", &[]);
    fs::write(
        fixture.store.global_config_path(),
        "schema_version=1\ncache_reuse_minutes=60\n",
    )
    .unwrap();
    configure(&fixture, "[Inheritance]\nCopyOnWriteBuild=/saved\n");
    let backend = SimulatedBackend::default();
    let first =
        build_with_backend_and_warning(&fixture.store, Some("desktop"), &backend, |_| Ok(()))
            .unwrap()
            .remove(0);
    assert!(!first.root.join("saved").exists());
    fs::write(first.root.join("saved"), "runtime data").unwrap();
    let second =
        build_with_backend_and_warning(&fixture.store, Some("desktop"), &backend, |_| Ok(()))
            .unwrap()
            .remove(0);
    assert_eq!(
        fs::read_to_string(second.root.join("saved")).unwrap(),
        "runtime data"
    );
    assert_eq!(backend.count("post_maintenance"), 2);
    for outcome in [&first, &second] {
        let state = outcome.root.parent().unwrap().join("state.toml");
        let mut value = record(&state);
        value["state"] = "cancelled".into();
        fs::write(state, toml::to_string(&value).unwrap()).unwrap();
    }
    let third =
        build_with_backend_and_warning(&fixture.store, Some("desktop"), &backend, |_| Ok(()))
            .unwrap()
            .remove(0);
    assert!(!third.root.join("saved").exists());
    assert_eq!(backend.count("post_maintenance"), 3);
}

#[test]
fn corrupt_latest_completed_source_fails_before_build_without_using_older_root() {
    for corruption in [
        "missing-root",
        "wrong-owner",
        "malformed-state",
        "missing-path",
    ] {
        let fixture = Fixture::new();
        fixture.system("desktop", &[]);
        configure(&fixture, "[Inheritance]\nCopyOnWriteBuild=/saved\n");
        let backend = SimulatedBackend::default();
        let layout = fixture.store.system_layout("desktop").unwrap();
        for version in ["v1", "v2"] {
            let directory = layout.versions.join(version);
            fs::create_dir(&directory).unwrap();
            let root = directory.join("root");
            if version == "v1" || corruption != "missing-root" {
                backend.create_subvolume(&root).unwrap();
                if version == "v1" || corruption != "missing-path" {
                    fs::write(root.join("saved"), version).unwrap();
                }
            }
            let owner = if version == "v2" && corruption == "wrong-owner" {
                "other"
            } else {
                "desktop"
            };
            let state = if version == "v2" && corruption == "malformed-state" {
                "invalid = [".to_string()
            } else {
                format!(
                    "schema_version=1\nsystem_id='{owner}'\nversion='{version}'\nstate='complete'\n"
                )
            };
            fs::write(directory.join("state.toml"), state).unwrap();
        }
        let error =
            build_with_backend_and_warning(&fixture.store, Some("desktop"), &backend, |_| {
                panic!("无效来源必须在警告前失败 / Invalid source must fail before warnings")
            })
            .unwrap_err();
        assert!(!error.to_string().is_empty());
        assert!(backend.events.borrow().is_empty());
        assert!(backend.snapshots.borrow().is_empty());
        assert_eq!(fs::read_dir(&layout.versions).unwrap().count(), 2);
        assert!(!layout.root.join("version-counter").exists());
    }
}

#[test]
fn inheritance_override_without_system_fails_before_inspecting_store_or_source() {
    let fixture = Fixture::new();
    let backend = SimulatedBackend::default();
    let error = rstarsleep::controller::build_with_options_and_backend(
        &fixture.store,
        None,
        &BuildOptions {
            inheritance_root: Some("relative-and-missing".into()),
        },
        &backend,
    )
    .unwrap_err()
    .to_string();
    assert!(
        error.contains("具体构建系统") && error.contains("explicitly selected"),
        "{error}"
    );
    assert!(backend.events.borrow().is_empty());
}

#[test]
fn finalized_boot_copy_becomes_the_automatic_source_for_the_next_boot_copy() {
    use rstarsleep::controller::{
        build_with_options_and_backend, finalize_inheritance_with_backend,
    };
    use std::path::Path;

    let fixture = Fixture::new();
    fixture.system("desktop", &[]);
    configure(&fixture, "[Inheritance]\nCopyOnWriteBoot=/saved\n");
    let selected = fixture
        .store
        .root()
        .parent()
        .unwrap()
        .join("selected-source");
    fs::create_dir(&selected).unwrap();
    fs::write(selected.join("saved"), "initial").unwrap();
    let backend = SimulatedBackend::default();
    let first = build_with_options_and_backend(
        &fixture.store,
        Some("desktop"),
        &BuildOptions {
            inheritance_root: Some(selected),
        },
        &backend,
    )
    .unwrap()
    .remove(0);
    let first_directory = first.root.parent().unwrap();
    assert_eq!(
        record(&first_directory.join("state.toml"))["state"].as_str(),
        Some("pending-inheritance")
    );
    finalize_inheritance_with_backend(
        &first_directory.join("inheritance-pending.toml"),
        Path::new("/"),
        &backend,
    )
    .unwrap();
    let first_final = first_directory.join("root");
    assert_eq!(
        record(&first_directory.join("state.toml"))["state"].as_str(),
        Some("complete")
    );
    fs::write(first_final.join("saved"), "after first boot").unwrap();

    let second =
        build_with_backend_and_warning(&fixture.store, Some("desktop"), &backend, |_| Ok(()))
            .unwrap()
            .remove(0);
    let manifest = second
        .root
        .parent()
        .unwrap()
        .join("inheritance-pending.toml");
    assert_eq!(
        record(&manifest)["source_root"]["path"].as_str(),
        first_final.to_str()
    );
    fs::write(first_final.join("saved"), "latest at second boot").unwrap();
    finalize_inheritance_with_backend(&manifest, Path::new("/"), &backend).unwrap();
    assert_eq!(
        fs::read_to_string(second.root.parent().unwrap().join("root/saved")).unwrap(),
        "latest at second boot"
    );
}
