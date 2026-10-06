//! Verify INI freezing and cache provenance with real configuration readers.
//! 使用真实配置读取接口验证 INI 留档及缓存来源。

use super::*;

/// Freeze multiline INI verbatim and reuse caches on package changes while retaining unknown options.
/// 原样留档多行 INI，包变化复用缓存，未知选项仍参与兼容性判断。
/// # Arguments
/// No parameters.
/// 无参数。
#[test]
fn multiline_ini_freezes_and_preserves_cache_compatibility() {
    let fixture = Fixture::new();
    fixture.system("desktop", &[]);
    let layout = fixture.store.system_layout("desktop").unwrap();
    let path = layout.config.join("build/config.conf");
    let text = "[Config]\nSchemaVersion=1\n[Distribution]\nDistribution=arch\nArchitecture=x86_64\n[Content]\nPackages=discarded\nPackages=\nPackages=\n    old # Initial applications\n\n    kept\nPackages=kept\n[Custom]\nEnabled=true\n";
    fs::write(&path, text).unwrap();
    let backend = SimulatedBackend::default();
    let first = build_with_backend(&fixture.store, Some("desktop"), &backend)
        .unwrap()
        .remove(0);
    assert_eq!(
        fs::read_to_string(
            layout
                .versions
                .join(&first.version)
                .join("config/build/config.conf")
        )
        .unwrap(),
        text
    );
    assert_eq!(
        fixture
            .store
            .read_recorded_build_config("desktop", &first.version)
            .unwrap()
            .packages,
        ["old", "kept", "kept"]
    );
    let updated = text.replace("old # Initial applications", "new # Changed applications");
    fs::write(&path, &updated).unwrap();
    backend.events.borrow_mut().clear();
    let second = build_with_backend(&fixture.store, Some("desktop"), &backend)
        .unwrap()
        .remove(0);
    assert_eq!(backend.count("system_init"), 0);
    assert_eq!(backend.count("update"), 1);
    assert_eq!(
        record(&fixture.cache(&second.version, "base-system.toml"))["source_version"].as_str(),
        Some(first.version.as_str())
    );
    let inventory = SimulatedBackend::inventory(&second.root);
    assert!(inventory.contains_key("new"));
    assert!(!inventory.contains_key("old"));
    fs::write(&path, updated.replace("Enabled=true", "Enabled=false")).unwrap();
    backend.events.borrow_mut().clear();
    let third = build_with_backend(&fixture.store, Some("desktop"), &backend)
        .unwrap()
        .remove(0);
    assert_eq!(backend.count("system_init"), 1);
    assert_eq!(backend.count("update"), 1);
    assert!(
        backend
            .events
            .borrow()
            .iter()
            .filter(|event| event.operation == "update")
            .all(|event| event.root.ends_with("base-root"))
    );
    assert!(
        record(&fixture.cache(&third.version, "base-system.toml"))
            .get("source_version")
            .is_none()
    );
    assert!(
        record(&fixture.cache(&third.version, "packages.toml"))
            .get("source_version")
            .is_none()
    );
}

/// Continue building historical TOML systems and reusing caches with their archived provenance.
/// 继续构建历史 TOML 系统，并根据其留档来源复用缓存。
/// # Arguments
/// No parameters.
/// 无参数。
#[test]
fn legacy_toml_builds_keep_readable_archives_and_reusable_caches() {
    let fixture = Fixture::new();
    fixture.system("desktop", &[]);
    let layout = fixture.store.system_layout("desktop").unwrap();
    fs::remove_file(layout.config.join("build/config.conf")).unwrap();
    let path = layout.config.join("build/config.toml");
    let text = "schema_version=1\nsystem_type='arch'\narchitecture='x86_64'\npackages=['old']\n";
    fs::write(&path, text).unwrap();
    let backend = SimulatedBackend::default();
    let first = build_with_backend(&fixture.store, Some("desktop"), &backend)
        .unwrap()
        .remove(0);
    assert_eq!(
        fixture
            .store
            .read_recorded_build_config("desktop", &first.version)
            .unwrap()
            .packages,
        ["old"]
    );
    fs::write(&path, text.replace("'old'", "'new'")).unwrap();
    backend.events.borrow_mut().clear();
    let second = build_with_backend(&fixture.store, Some("desktop"), &backend)
        .unwrap()
        .remove(0);
    assert_eq!(backend.count("system_init"), 0);
    assert_eq!(backend.count("update"), 1);
    assert_eq!(
        record(&fixture.cache(&second.version, "base-system.toml"))["source_version"].as_str(),
        Some(first.version.as_str())
    );
    assert!(SimulatedBackend::inventory(&second.root).contains_key("new"));
}
