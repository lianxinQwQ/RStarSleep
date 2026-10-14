//! Validate role-specific declarations before any build attempt or cache mutation.
use super::*;
use std::path::Path;

const BOOT: &[&str] = &["linux", "mkinitcpio"];
const COW: &[&str] = &[
    "linux",
    "mkinitcpio",
    "btrfs-progs",
    "coreutils",
    "util-linux",
    "bash",
    "findutils",
];

fn main_system(fixture: &Fixture) {
    fs::write(
        fixture.store.global_config_path(),
        "schema_version=1\nmain_system='desktop'\n",
    )
    .unwrap();
}

fn inheritance(fixture: &Fixture, timing: &str, cow: bool) {
    let source = fixture.store.root().parent().unwrap().join("live-file");
    fs::write(&source, "live").unwrap();
    let layout = fixture.store.system_layout("desktop").unwrap();
    fs::write(
        layout.config.join("build/inherit.conf"),
        format!(
            "[Inheritance]\nTiming={timing}\n{}",
            if cow {
                format!("CopyOnWrite={}\n", source.display())
            } else {
                String::new()
            }
        ),
    )
    .unwrap();
}

fn change_plugin(fixture: &Fixture, boot: bool, cow: bool, action: bool) {
    let temporary = tempfile::tempdir().unwrap();
    let path = fixture.store.plugins_path().join("arch.tar");
    tar::Archive::new(fs::File::open(&path).unwrap())
        .unpack(temporary.path())
        .unwrap();
    let metadata_path = temporary.path().join("plugin.toml");
    let mut metadata: toml::Value =
        toml::from_str(&fs::read_to_string(&metadata_path).unwrap()).unwrap();
    if !boot {
        metadata.as_table_mut().unwrap().remove("boot");
    }
    if !cow {
        metadata
            .as_table_mut()
            .unwrap()
            .remove("initramfs_inheritance");
    }
    fs::write(metadata_path, toml::to_string(&metadata).unwrap()).unwrap();
    if !action {
        fs::remove_file(temporary.path().join("prepare_inheritance_initramfs.sh")).unwrap();
    }
    let mut archive = tar::Builder::new(fs::File::create(path).unwrap());
    archive.append_dir_all(".", temporary.path()).unwrap();
    archive.finish().unwrap();
}

fn no_attempt(fixture: &Fixture, backend: &SimulatedBackend) {
    let layout = fixture.store.system_layout("desktop").unwrap();
    assert!(!layout.root.join("version-counter").exists());
    assert_eq!(fs::read_dir(&layout.versions).unwrap().count(), 0);
    assert_eq!(fs::read_dir(&layout.cache).unwrap().count(), 0);
    assert!(
        backend
            .events
            .borrow()
            .iter()
            .all(|event| event.system_id == "predownload" || event.operation == "collect_packages")
    );
    assert_eq!(backend.count("predownload"), 0);
    assert!(backend.snapshots.borrow().iter().all(|snapshot| {
        !snapshot.destination.starts_with(&layout.root)
            && !snapshot
                .destination
                .starts_with(fixture.store.root().join("work/desktop"))
    }));
    assert_eq!(backend.boot_publications.get(), 0);
    assert!(!fixture.store.root().join("RStarSleep").exists());
}

#[test]
fn missing_boot_packages_fail_before_version_download_or_cache_work() {
    let fixture = Fixture::with_predownload(true);
    fixture.system("desktop", &["base"]);
    main_system(&fixture);
    let backend = SimulatedBackend::default();
    let error = build_with_backend(&fixture.store, Some("desktop"), &backend)
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("通用启动") && error.contains("linux") && error.contains("mkinitcpio"),
        "{error}"
    );
    no_attempt(&fixture, &backend);
    assert_eq!(backend.count("collect_packages"), 1);
}

#[test]
fn missing_cow_package_is_reported_from_metadata_before_build() {
    let fixture = Fixture::new();
    fixture.system(
        "desktop",
        &COW.iter()
            .copied()
            .filter(|p| *p != "btrfs-progs")
            .collect::<Vec<_>>(),
    );
    main_system(&fixture);
    inheritance(&fixture, "initramfs", true);
    let backend = SimulatedBackend::default();
    let error = build_with_backend(&fixture.store, Some("desktop"), &backend)
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("启动时 CoW 继承") && error.contains("btrfs-progs"),
        "{error}"
    );
    no_attempt(&fixture, &backend);
}

#[test]
fn main_system_requires_boot_declaration_even_without_inheritance() {
    let fixture = Fixture::new();
    fixture.system("desktop", BOOT);
    main_system(&fixture);
    change_plugin(&fixture, false, true, true);
    let backend = SimulatedBackend::default();
    let error = build_with_backend(&fixture.store, Some("desktop"), &backend)
        .unwrap_err()
        .to_string();
    assert!(error.contains("[boot]"), "{error}");
    no_attempt(&fixture, &backend);
}

#[test]
fn active_main_system_cow_requires_declaration_and_action() {
    for (declared, action, diagnostic) in [
        (false, true, "[initramfs_inheritance]"),
        (true, false, "prepare_inheritance_initramfs"),
    ] {
        let fixture = Fixture::new();
        fixture.system("desktop", COW);
        main_system(&fixture);
        inheritance(&fixture, "initramfs", true);
        change_plugin(&fixture, true, declared, action);
        let backend = SimulatedBackend::default();
        let error = build_with_backend(&fixture.store, Some("desktop"), &backend)
            .unwrap_err()
            .to_string();
        assert!(error.contains(diagnostic), "{error}");
        no_attempt(&fixture, &backend);
    }
}

#[test]
fn build_time_cow_empty_cow_and_mount_only_skip_initramfs_requirements() {
    for (timing, cow, mounts) in [
        ("build", true, false),
        ("initramfs", false, false),
        ("initramfs", false, true),
    ] {
        let fixture = Fixture::new();
        fixture.system("desktop", BOOT);
        main_system(&fixture);
        inheritance(&fixture, timing, cow);
        if mounts {
            let source = fixture.store.root().parent().unwrap().join("live-mount");
            fs::create_dir(&source).unwrap();
            let path = fixture
                .store
                .system_layout("desktop")
                .unwrap()
                .config
                .join("build/inherit.conf");
            fs::write(
                &path,
                format!(
                    "[Inheritance]\nTiming=initramfs\nMount={}/\n",
                    source.display()
                ),
            )
            .unwrap();
        }
        change_plugin(&fixture, true, false, false);
        let backend = SimulatedBackend::default();
        build_with_backend(&fixture.store, Some("desktop"), &backend).unwrap();
        assert_eq!(backend.count("prepare_inheritance_initramfs"), 0);
        assert_eq!(backend.boot_publications.get(), 1);
    }
}

#[test]
fn subsystem_deferred_cow_needs_no_main_boot_support() {
    let fixture = Fixture::new();
    fixture.system("desktop", &[]);
    inheritance(&fixture, "initramfs", true);
    change_plugin(&fixture, false, false, false);
    let backend = SimulatedBackend::default();
    let outcome = build_with_backend(&fixture.store, Some("desktop"), &backend)
        .unwrap()
        .remove(0);
    assert!(outcome.root.ends_with("prepared-root"));
    assert_eq!(backend.count("prepare_inheritance_initramfs"), 0);
    assert_eq!(backend.boot_publications.get(), 0);
    assert!(
        !outcome
            .root
            .parent()
            .unwrap()
            .join("boot-spec.toml")
            .exists()
    );
}

#[test]
fn preflight_failure_preserves_previous_version_current_reference_and_all_caches() {
    let fixture = Fixture::new();
    fixture.system("desktop", &["editor"]);
    let backend = SimulatedBackend::default();
    build_with_backend(&fixture.store, Some("desktop"), &backend).unwrap();
    let layout = fixture.store.system_layout("desktop").unwrap();
    let current = fs::read(layout.root.join("build-current.toml")).unwrap();
    main_system(&fixture);
    backend.events.borrow_mut().clear();
    assert!(build_with_backend(&fixture.store, Some("desktop"), &backend).is_err());
    assert_eq!(
        fs::read_to_string(layout.root.join("version-counter"))
            .unwrap()
            .trim(),
        "1"
    );
    assert_eq!(fs::read_dir(&layout.versions).unwrap().count(), 1);
    assert_eq!(
        fs::read(layout.root.join("build-current.toml")).unwrap(),
        current
    );
    for stage in ["base-system", "packages", "script"] {
        assert!(fixture.cache("v00000001", stage).is_dir());
        assert!(
            fixture
                .cache("v00000001", &format!("{stage}.toml"))
                .is_file()
        );
    }
    assert!(
        backend
            .events
            .borrow()
            .iter()
            .all(|event| event.system_id == "predownload" || event.operation == "collect_packages")
    );
    assert_eq!(backend.count("predownload"), 0);
}

#[test]
fn active_cow_action_runs_after_scripts_and_failure_blocks_boot_publication() {
    for fail in [false, true] {
        let fixture = Fixture::new();
        fixture.system("desktop", COW);
        main_system(&fixture);
        inheritance(&fixture, "initramfs", true);
        let layout = fixture.store.system_layout("desktop").unwrap();
        fs::write(layout.config.join("script/prepare.sh"), "exit 0\n").unwrap();
        let backend = SimulatedBackend::default();
        if fail {
            *backend.fail_operation.borrow_mut() = Some("prepare_inheritance_initramfs");
        }
        let result = build_with_backend(&fixture.store, Some("desktop"), &backend);
        assert_eq!(result.is_err(), fail);
        assert_eq!(backend.count("prepare_inheritance_initramfs"), 1);
        let events = backend.events.borrow();
        let script = events.iter().position(|e| e.operation == "script").unwrap();
        let action = events
            .iter()
            .position(|e| e.operation == "prepare_inheritance_initramfs")
            .unwrap();
        assert!(script < action);
        assert!(fixture.cache("v00000001", "packages").exists());
        assert!(fixture.cache("v00000001", "script").exists());
        assert_eq!(backend.boot_publications.get(), usize::from(!fail));
        let spec = layout.versions.join("v00000001/boot-spec.toml");
        let _: rstarsleep::controller::plugins::BootRequirements =
            toml::from_str(&fs::read_to_string(spec).unwrap()).unwrap();
    }
}

#[test]
fn validated_main_configuration_stays_frozen_during_build() {
    let fixture = Fixture::new();
    fixture.system("desktop", BOOT);
    main_system(&fixture);
    let backend = SimulatedBackend::default();
    *backend.mutate_editable.borrow_mut() = Some(
        fixture
            .store
            .system_layout("desktop")
            .unwrap()
            .config
            .join("build/config.conf"),
    );
    let outcome = build_with_backend(&fixture.store, Some("desktop"), &backend)
        .unwrap()
        .remove(0);
    assert_eq!(outcome.version, "v00000001");
    assert_eq!(backend.boot_publications.get(), 1);
    assert!(
        backend
            .events
            .borrow()
            .iter()
            .filter(|event| event.system_id == "desktop")
            .all(|event| event.packages == BOOT)
    );
    assert!(Path::new(&outcome.root).exists());
}

#[test]
fn manager_binary_sync_failure_does_not_allocate_attempt_or_change_caches() {
    let fixture = Fixture::new();
    fixture.system("desktop", BOOT);
    main_system(&fixture);
    fs::create_dir(fixture.store.root().join("RStarSleep")).unwrap();
    let backend = SimulatedBackend::default();
    let error = build_with_backend(&fixture.store, Some("desktop"), &backend)
        .unwrap_err()
        .to_string();
    assert!(error.contains("管理器二进制目标必须为普通文件"), "{error}");
    let layout = fixture.store.system_layout("desktop").unwrap();
    assert!(!layout.root.join("version-counter").exists());
    assert_eq!(fs::read_dir(&layout.versions).unwrap().count(), 0);
    assert_eq!(fs::read_dir(&layout.cache).unwrap().count(), 0);
    assert!(
        backend
            .events
            .borrow()
            .iter()
            .all(|event| event.system_id == "predownload" || event.operation == "collect_packages")
    );
    assert_eq!(backend.count("predownload"), 0);
}

fn arch_base_dependencies(backend: &SimulatedBackend) {
    backend.runtime_dependencies.borrow_mut().insert(
        "base".into(),
        ["coreutils", "util-linux", "bash", "findutils"]
            .map(String::from)
            .to_vec(),
    );
}

#[test]
fn groups_base_packages_and_runtime_dependencies_satisfy_requirements_without_duplicate_configuration()
 {
    for use_group in [true, false] {
        let fixture = Fixture::with_predownload(true);
        fixture.system(
            "desktop",
            &[if use_group {
                "boot-group"
            } else {
                "desktop-suite"
            }],
        );
        main_system(&fixture);
        inheritance(&fixture, "initramfs", true);
        let backend = SimulatedBackend::default();
        arch_base_dependencies(&backend);
        let packages = ["linux", "mkinitcpio", "btrfs-progs"]
            .map(String::from)
            .to_vec();
        if use_group {
            backend
                .package_groups
                .borrow_mut()
                .insert("boot-group".into(), packages);
        } else {
            backend
                .runtime_dependencies
                .borrow_mut()
                .insert("desktop-suite".into(), packages);
        }
        let outcome = build_with_backend(&fixture.store, Some("desktop"), &backend)
            .unwrap()
            .remove(0);
        assert_eq!(backend.count("collect_packages"), 1);
        assert_eq!(backend.boot_publications.get(), 1);
        let plan: rstarsleep::software::PackagePlan = toml::from_str(
            &fs::read_to_string(outcome.root.parent().unwrap().join("package-plan.toml")).unwrap(),
        )
        .unwrap();
        for name in COW {
            assert!(plan.packages.iter().any(|package| package == name));
        }
        let events = backend.events.borrow();
        let collect = events
            .iter()
            .position(|e| e.operation == "collect_packages")
            .unwrap();
        let download = events
            .iter()
            .position(|e| e.operation == "predownload")
            .unwrap();
        let build = events
            .iter()
            .position(|e| e.system_id == "desktop" && e.operation == "bootstrap")
            .unwrap();
        assert!(collect < download && download < build);
        assert_eq!(
            events[collect].arguments,
            [
                "base",
                if use_group {
                    "boot-group"
                } else {
                    "desktop-suite"
                }
            ]
        );
    }
}

#[test]
fn another_systems_package_does_not_satisfy_the_main_system_requirement() {
    let fixture = Fixture::with_predownload(true);
    fixture.system("desktop", BOOT);
    fixture.system("server", &["btrfs-progs"]);
    main_system(&fixture);
    inheritance(&fixture, "initramfs", true);
    let backend = SimulatedBackend::default();
    arch_base_dependencies(&backend);
    let error = build_with_backend(&fixture.store, None, &backend)
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("desktop") && error.contains("btrfs-progs"),
        "{error}"
    );
    let desktop = fixture.store.system_layout("desktop").unwrap();
    assert!(!desktop.root.join("version-counter").exists());
    assert_eq!(fs::read_dir(&desktop.versions).unwrap().count(), 0);
    assert_eq!(fs::read_dir(&desktop.cache).unwrap().count(), 0);
    assert!(
        backend
            .events
            .borrow()
            .iter()
            .filter(|e| e.system_id == "desktop")
            .all(|e| e.operation == "collect_packages")
    );
    let server = fixture.store.system_layout("server").unwrap();
    assert!(server.versions.join("v00000001/root").is_dir());
    assert_eq!(backend.count("collect_packages"), 2);
    let events = backend.events.borrow();
    let collect = events
        .iter()
        .find(|e| e.operation == "collect_packages" && e.system_id == "desktop")
        .unwrap();
    assert!(!collect.arguments.iter().any(|p| p == "btrfs-progs"));
    let download = events
        .iter()
        .find(|e| e.operation == "predownload")
        .unwrap();
    assert_eq!(download.arguments, ["btrfs-progs"]);
}

#[test]
fn old_installed_packages_are_not_counted_as_current_configuration_requirements() {
    let fixture = Fixture::with_predownload(true);
    fixture.system("desktop", COW);
    main_system(&fixture);
    inheritance(&fixture, "initramfs", true);
    let backend = SimulatedBackend::default();
    arch_base_dependencies(&backend);
    build_with_backend(&fixture.store, Some("desktop"), &backend).unwrap();
    fixture.packages("desktop", BOOT);
    backend.events.borrow_mut().clear();
    let error = build_with_backend(&fixture.store, Some("desktop"), &backend)
        .unwrap_err()
        .to_string();
    assert!(error.contains("btrfs-progs"), "{error}");
    let layout = fixture.store.system_layout("desktop").unwrap();
    assert_eq!(
        fs::read_to_string(layout.root.join("version-counter"))
            .unwrap()
            .trim(),
        "1"
    );
    assert!(fixture.cache("v00000001", "script").is_dir());
    assert_eq!(backend.count("predownload"), 0);
    let events = backend.events.borrow();
    let collect = events
        .iter()
        .find(|e| e.operation == "collect_packages")
        .unwrap();
    assert_eq!(collect.arguments, ["base", "linux", "mkinitcpio"]);
}

#[test]
fn invalid_collection_results_stop_before_target_build_changes() {
    for output in [
        "not json",
        r#"{"schema_version":2,"packages":["linux","mkinitcpio"]}"#,
        r#"{"schema_version":1,"packages":["linux","linux"]}"#,
        r#"{"schema_version":1,"packages":["bad name"]}"#,
        r#"{"schema_version":1,"packages":["linux"],"unknown":1}"#,
    ] {
        let fixture = Fixture::new();
        fixture.system("desktop", BOOT);
        main_system(&fixture);
        let backend = SimulatedBackend::default();
        *backend.collected_package_plan_override.borrow_mut() = Some(output.into());
        assert!(build_with_backend(&fixture.store, Some("desktop"), &backend).is_err());
        no_attempt(&fixture, &backend);
    }
}

#[test]
fn main_system_without_package_collection_capability_fails_before_environment_setup() {
    let fixture = Fixture::new();
    fixture.system("desktop", BOOT);
    main_system(&fixture);
    let metadata = fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("presets/plugins/arch/plugin.toml"),
    )
    .unwrap();
    let mut metadata: toml::Value = toml::from_str(&metadata).unwrap();
    metadata.as_table_mut().unwrap().remove("package_plan");
    super::base_system::replace_resource(
        &fixture,
        "plugin.toml",
        toml::to_string(&metadata).unwrap().as_bytes(),
    );
    let backend = SimulatedBackend::default();
    let error = build_with_backend(&fixture.store, Some("desktop"), &backend)
        .unwrap_err()
        .to_string();
    assert!(error.contains("[package_plan]"), "{error}");
    no_attempt(&fixture, &backend);
    assert!(backend.events.borrow().is_empty());
}
