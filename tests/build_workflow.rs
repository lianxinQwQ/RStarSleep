//! Exercise real records and cache selection with isolated simulated subvolumes.
//! 使用隔离模拟子卷验证真实记录和缓存选择。

mod support;

use rstarsleep::controller::{BuildBackend, Error, build_with_backend};
use std::fs;
use support::build::{Fixture, MARKER, SimulatedBackend, record};

/// Verify cached package maintenance covers the base update and preserves dependencies.
/// 验证有缓存软件维护覆盖基础更新并保留依赖。
/// # Arguments
///
/// No parameters.
/// 无参数。
#[test]
fn cached_package_step_takes_over_maintenance_and_preserves_dependencies() {
    let fixture = Fixture::new();
    fixture.system("desktop", &["old", "kept"]);
    let backend = SimulatedBackend::default();
    let first = build_with_backend(&fixture.store, Some("desktop"), &backend)
        .unwrap()
        .remove(0);
    assert_eq!(first.version, "v00000001");
    assert_eq!(backend.count("bootstrap"), 1);
    assert_eq!(backend.count("base_install"), 1);
    assert_eq!(backend.count("update"), 0);
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
    backend.check_subvolume(&first.root, true).unwrap();
    backend
        .check_subvolume(&fixture.cache(&first.version, "base-system"), true)
        .unwrap();
    backend
        .check_subvolume(&fixture.cache(&first.version, "packages"), true)
        .unwrap();
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
    assert_eq!(backend.count("update"), 1);
    assert!(
        backend
            .events
            .borrow()
            .iter()
            .all(|event| event.root.ends_with("root"))
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
    assert_eq!(inherited["origin_version"].as_str(), Some("v00000001"));
    assert_eq!(inherited["source_version"].as_str(), Some("v00000001"));
    assert_eq!(
        inherited["plugin_sha256"],
        record(&fixture.cache(&first.version, "base-system.toml"))["plugin_sha256"]
    );
    assert_eq!(
        record(&fixture.cache(&second.version, "packages.toml"))["source_version"].as_str(),
        Some("v00000001")
    );
    backend.check_subvolume(&second.root, true).unwrap();
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

/// Verify missing base cache is regenerated independently from maintained software.
/// 验证缺失基础缓存独立于被维护软件重新生成。
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
    assert_eq!(backend.count("update"), 1);
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
            .find(|event| event.operation == "bootstrap")
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
            .ends_with("root")
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
