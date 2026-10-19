//! Exercise real records and cache selection with isolated simulated subvolumes.
//! 使用隔离模拟子卷验证真实记录和缓存选择。

mod support;

use rstarsleep::controller::{BuildBackend, Error, build_with_backend};
use std::fs;
use support::build::{Fixture, MARKER, SimulatedBackend, record};

/// Verify base and package caches are maintained independently while preserving dependencies.
/// 验证基础及软件缓存独立维护并保留依赖。
/// # Arguments
///
/// No parameters.
/// 无参数。
#[test]
fn cached_base_and_package_steps_both_maintain_and_preserve_dependencies() {
    let fixture = Fixture::new();
    fixture.system("desktop", &["old", "kept"]);
    let backend = SimulatedBackend::default();
    let first = build_with_backend(&fixture.store, Some("desktop"), &backend)
        .unwrap()
        .remove(0);
    assert_eq!(first.version, "v00000001");
    assert_eq!(backend.count("bootstrap"), 1);
    assert_eq!(backend.count("base_install"), 1);
    assert_eq!(backend.count("update"), 1);
    assert!(
        backend
            .events
            .borrow()
            .iter()
            .filter(|event| matches!(event.operation.as_str(), "bootstrap" | "base_install"))
            .all(|event| event.root.ends_with("base-root"))
    );
    assert!(
        backend
            .events
            .borrow()
            .iter()
            .filter(|event| event.operation == "install")
            .all(|event| event.root.ends_with("root"))
    );
    assert!(backend.snapshots.borrow().iter().any(|snapshot| {
        !snapshot.readonly
            && snapshot.source == fixture.cache(&first.version, "base-system")
            && snapshot
                .destination
                .ends_with("work/desktop/v00000001/root")
    }));
    backend.check_subvolume(&first.root, false).unwrap();
    backend
        .check_subvolume(&fixture.cache(&first.version, "base-system"), true)
        .unwrap();
    backend
        .check_subvolume(&fixture.cache(&first.version, "packages"), true)
        .unwrap();
    fs::write(first.root.join("runtime-only"), "runtime change").unwrap();
    for stage in ["base-system", "packages", "script"] {
        assert!(
            !fixture
                .cache(&first.version, stage)
                .join("runtime-only")
                .exists()
        );
    }
    let old_inventory = SimulatedBackend::inventory(&first.root);
    assert!(
        record(&fixture.cache(&first.version, "base-system.toml"))
            .get("source_version")
            .is_none()
    );
    assert!(
        record(&fixture.cache(&first.version, "packages.toml"))
            .get("source_version")
            .is_none()
    );
    fixture.cleaned(&first.version);
    fixture.packages("desktop", &["kept", "new"]);
    backend.events.borrow_mut().clear();
    backend.remove_kept_on_update.set(true);
    let second = build_with_backend(&fixture.store, Some("desktop"), &backend)
        .unwrap()
        .remove(0);
    assert_eq!(backend.count("bootstrap"), 0);
    assert_eq!(backend.count("base_install"), 0);
    assert_eq!(backend.count("update"), 2);
    assert!(
        backend
            .events
            .borrow()
            .iter()
            .filter(|event| event.operation == "update")
            .any(|event| event.root.ends_with("base-root"))
    );
    let inventory = SimulatedBackend::inventory(&second.root);
    assert_eq!(inventory.get("base"), Some(&true));
    assert_eq!(inventory.get("kept"), Some(&true));
    assert_eq!(inventory.get("new"), Some(&true));
    assert_eq!(inventory.get("shared"), Some(&false));
    assert!(!inventory.contains_key("old"));
    assert!(!inventory.contains_key("old-dependency"));
    assert_eq!(SimulatedBackend::inventory(&first.root), old_inventory);
    assert!(
        backend
            .events
            .borrow()
            .iter()
            .any(|event| event.operation == "install" && event.arguments == ["kept", "new"])
    );
    let inherited = record(&fixture.cache(&second.version, "base-system.toml"));
    assert_eq!(inherited["origin_version"].as_str(), Some("v00000002"));
    assert_eq!(inherited["source_version"].as_str(), Some("v00000001"));
    assert_eq!(
        inherited["plugin_sha256"],
        record(&fixture.cache(&first.version, "base-system.toml"))["plugin_sha256"]
    );
    assert_eq!(
        record(&fixture.cache(&second.version, "packages.toml"))["source_version"].as_str(),
        Some("v00000001")
    );
    backend.check_subvolume(&second.root, false).unwrap();
    assert!(first.root.join("runtime-only").exists());
    assert!(!second.root.join("runtime-only").exists());
    fixture.cleaned(&second.version);
}

/// Verify a missing software root selects base maintenance and a first software install.
/// 验证软件根缺失时选择基础维护和首次软件安装。
/// # Arguments
///
/// No parameters.
/// 无参数。
#[test]
fn cached_base_step_maintains_when_package_step_has_no_cache() {
    let fixture = Fixture::new();
    fixture.system("desktop", &["editor"]);
    let backend = SimulatedBackend::default();
    build_with_backend(&fixture.store, Some("desktop"), &backend).unwrap();
    fs::remove_dir_all(fixture.cache("v00000001", "packages")).unwrap();
    backend.events.borrow_mut().clear();
    let outcome = build_with_backend(&fixture.store, Some("desktop"), &backend)
        .unwrap()
        .remove(0);
    assert_eq!(backend.count("bootstrap"), 0);
    assert_eq!(backend.count("update"), 1);
    assert_eq!(backend.count("install"), 1);
    assert_eq!(
        record(&fixture.cache(&outcome.version, "base-system.toml"))["source_version"].as_str(),
        Some("v00000001")
    );
    assert!(
        record(&fixture.cache(&outcome.version, "packages.toml"))
            .get("source_version")
            .is_none()
    );
    assert_eq!(
        SimulatedBackend::inventory(&outcome.root).get("editor"),
        Some(&true)
    );
    fixture.cleaned(&outcome.version);
}

/// Verify missing base cache is installed independently before maintained software.
/// 验证缺失基础缓存独立安装后再维护软件。
/// # Arguments
///
/// No parameters.
/// 无参数。
#[test]
fn uncached_base_step_installs_before_cached_package_step_maintains() {
    let fixture = Fixture::new();
    fixture.system("desktop", &["editor"]);
    let backend = SimulatedBackend::default();
    build_with_backend(&fixture.store, Some("desktop"), &backend).unwrap();
    fs::remove_dir_all(fixture.cache("v00000001", "base-system")).unwrap();
    backend.events.borrow_mut().clear();
    let outcome = build_with_backend(&fixture.store, Some("desktop"), &backend)
        .unwrap()
        .remove(0);
    assert_eq!(backend.count("bootstrap"), 1);
    assert_eq!(backend.count("base_install"), 1);
    assert_eq!(backend.count("update"), 2);
    assert!(
        record(&fixture.cache(&outcome.version, "base-system.toml"))
            .get("source_version")
            .is_none()
    );
    assert_eq!(
        record(&fixture.cache(&outcome.version, "packages.toml"))["source_version"].as_str(),
        Some("v00000001")
    );
    let events = backend.events.borrow();
    assert!(
        events
            .iter()
            .find(|event| event.operation == "system_init")
            .unwrap()
            .root
            .ends_with("base-root")
    );
    assert!(
        events
            .iter()
            .find(|event| event.operation == "update")
            .unwrap()
            .root
            .ends_with("base-root")
    );
    assert_eq!(
        fs::read_to_string(
            fixture
                .cache(&outcome.version, "base-system")
                .join(".base-generation")
        )
        .unwrap(),
        "2"
    );
    assert_eq!(
        fs::read_to_string(outcome.root.join(".base-generation")).unwrap(),
        "1"
    );
    fixture.cleaned(&outcome.version);
}

#[path = "build_workflow/failures.rs"]
mod failures;

#[path = "build_workflow/config.rs"]
mod config;

#[path = "build_workflow/base_system.rs"]
mod base_system;

#[path = "build_workflow/scripts.rs"]
mod scripts;

#[path = "build_workflow/post_maintenance.rs"]
mod post_maintenance;

#[path = "build_workflow/boot_preflight.rs"]
mod boot_preflight;

#[path = "build_workflow/predownload.rs"]
mod predownload;

#[path = "build_workflow/install_logs.rs"]
mod install_logs;

/// Refuse an active cutoff without partial deletion, retain newer versions and persistent data.
/// 拒绝活动截止版本且不部分删除，保留更新版本和持久数据。
/// # Arguments
/// No parameters. 无参数。
#[test]
fn bounded_clean_preserves_running_roots_data_and_monotonic_versions() {
    let fixture = Fixture::new();
    fixture.system("desktop", &["kept"]);
    let backend = SimulatedBackend::default();
    let mut outcomes = Vec::new();
    for _ in 0..3 {
        outcomes.push(
            build_with_backend(&fixture.store, Some("desktop"), &backend)
                .unwrap()
                .remove(0),
        );
    }
    let layout = fixture.store.system_layout("desktop").unwrap();
    fs::write(layout.persistent.join("user-data"), "keep").unwrap();
    *backend.running_root.borrow_mut() = Some(outcomes[1].root.clone());
    assert!(
        rstarsleep::controller::clean_with_backend(
            &fixture.store,
            "desktop",
            &outcomes[1].version,
            &backend
        )
        .is_err()
    );
    assert!(outcomes[0].root.exists());
    *backend.running_root.borrow_mut() = None;
    assert_eq!(
        rstarsleep::controller::clean_with_backend(
            &fixture.store,
            "desktop",
            &outcomes[1].version,
            &backend
        )
        .unwrap(),
        [outcomes[0].version.clone(), outcomes[1].version.clone()]
    );
    assert!(outcomes[2].root.exists());
    assert!(layout.persistent.join("user-data").exists());
    assert!(
        rstarsleep::controller::clean_with_backend(
            &fixture.store,
            "desktop",
            &outcomes[2].version,
            &backend
        )
        .is_err()
    );
    fixture.retire_current("desktop");
    rstarsleep::controller::clean_with_backend(
        &fixture.store,
        "desktop",
        &outcomes[2].version,
        &backend,
    )
    .unwrap();
    let next = build_with_backend(&fixture.store, Some("desktop"), &backend)
        .unwrap()
        .remove(0);
    assert_eq!(next.version, "v00000004");
}

#[path = "build_workflow/deferred.rs"]
mod deferred;
#[path = "build_workflow/inheritance.rs"]
mod inheritance;

/// A failed scratch cleanup never publishes a main-system boot entry.
/// 临时根清理失败时绝不发布主系统启动项。
/// # Arguments
/// No parameters. 无参数。
#[test]
fn main_system_boot_publication_requires_successful_scratch_cleanup() {
    let fixture = Fixture::new();
    fixture.system("desktop", &["linux", "mkinitcpio"]);
    fs::write(
        fixture.store.global_config_path(),
        "schema_version=1\nmain_system='desktop'\n",
    )
    .unwrap();
    let backend = SimulatedBackend::default();
    backend.fail_cleanup.set(true);
    assert!(build_with_backend(&fixture.store, Some("desktop"), &backend).is_err());
    assert_eq!(backend.boot_publications.get(), 0);
    let layout = fixture.store.system_layout("desktop").unwrap();
    assert_eq!(
        record(&layout.versions.join("v00000001/state.toml"))["state"].as_str(),
        Some("failed")
    );
    build_with_backend(&fixture.store, Some("desktop"), &backend).unwrap();
    assert_eq!(backend.boot_publications.get(), 1);
}

/// Cleanup uses version ownership records even after the global main system changes.
/// 即使全局主系统改变，清理也使用版本所有权记录。
/// # Arguments
/// No parameters. 无参数。
#[test]
fn clean_removes_only_owned_boot_files() {
    let fixture = Fixture::new();
    fixture.system("desktop", &[]);
    let backend = SimulatedBackend::default();
    let outcome = build_with_backend(&fixture.store, Some("desktop"), &backend)
        .unwrap()
        .remove(0);
    let boot = fixture.store.root().parent().unwrap().join("esp");
    let directory = boot.join("RStarSleep").join(&outcome.version);
    let entry = boot
        .join("loader/entries")
        .join(format!("RStarSleep-desktop-{}.conf", outcome.version));
    fs::create_dir_all(&directory).unwrap();
    fs::create_dir_all(entry.parent().unwrap()).unwrap();
    fs::write(directory.join("vmlinuz"), "kernel").unwrap();
    fs::write(directory.join("initramfs-linux.img"), "initramfs").unwrap();
    fs::write(directory.join("intel-ucode.img"), "intel-microcode").unwrap();
    fs::write(directory.join("amd-ucode.img"), "amd-microcode").unwrap();
    fs::write(&entry, "entry").unwrap();
    let unrelated = entry.parent().unwrap().join("other.conf");
    fs::write(&unrelated, "keep").unwrap();
    let ownership = toml::Table::from_iter([
        ("system_id".into(), toml::Value::String("desktop".into())),
        (
            "version".into(),
            toml::Value::String(outcome.version.clone()),
        ),
        (
            "directory".into(),
            toml::Value::String(directory.to_str().unwrap().into()),
        ),
        (
            "entry".into(),
            toml::Value::String(entry.to_str().unwrap().into()),
        ),
    ]);
    fs::write(
        outcome.root.parent().unwrap().join("boot.toml"),
        toml::to_string(&ownership).unwrap(),
    )
    .unwrap();
    fixture.retire_current("desktop");
    rstarsleep::controller::clean_with_backend(
        &fixture.store,
        "desktop",
        &outcome.version,
        &backend,
    )
    .unwrap();
    assert!(!directory.exists());
    assert!(!entry.exists());
    assert!(unrelated.exists());
}

#[path = "build_workflow/unified_packages.rs"]
mod unified_packages;

#[path = "build_workflow/reuse.rs"]
mod reuse;

#[path = "build_workflow/dedup.rs"]
mod dedup;
