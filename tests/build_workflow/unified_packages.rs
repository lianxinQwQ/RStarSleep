//! Verify preparation plans and distribution-owned software maintenance.
//! 验证准备阶段计划以及发行版自行编排的软件维护。
use super::*;

fn download_systems(backend: &SimulatedBackend) -> serde_json::Value {
    let events = backend.events.borrow();
    let download = events
        .iter()
        .find(|event| event.operation == "predownload")
        .unwrap();
    assert_eq!(download.arguments.len(), 2);
    assert_eq!(download.arguments[0], "--systems-json");
    let request: serde_json::Value = serde_json::from_str(&download.arguments[1]).unwrap();
    assert_eq!(request["schema_version"], 1);
    request["systems"].clone()
}

#[test]
fn one_download_produces_all_plans_without_separate_resolution_or_environment_updates() {
    let fixture = Fixture::with_unified_packages();
    fixture.system("desktop", &["editor"]);
    fixture.system("server", &["server"]);
    let backend = SimulatedBackend::default();
    let outcomes = build_with_backend(&fixture.store, None, &backend).unwrap();
    assert_eq!(outcomes.len(), 2);
    assert_eq!(backend.count("predownload"), 1);
    assert_eq!(backend.count("collect_packages"), 0);
    assert_eq!(backend.count("predownload_install"), 1);
    assert_eq!(backend.count("update"), 0);
    assert_eq!(backend.count("install"), 0);
    assert_eq!(backend.count("maintain_packages"), 2);
    assert_eq!(
        download_systems(&backend),
        serde_json::json!({"desktop":["editor"],"server":["server"]})
    );
    let events = backend.events.borrow();
    let download_index = events
        .iter()
        .position(|event| event.operation == "predownload")
        .unwrap();
    assert!(
        events[..download_index]
            .iter()
            .all(|event| event.system_id == "predownload")
    );
    assert!(
        events
            .iter()
            .filter(|event| event.operation == "maintain_packages")
            .all(|event| event.root.ends_with("root") && event.system_id != "predownload")
    );
    let editor = outcomes
        .iter()
        .find(|outcome| outcome.system_id == "desktop")
        .unwrap();
    assert_eq!(
        SimulatedBackend::inventory(&editor.root),
        std::collections::BTreeMap::from([
            ("base".into(), true),
            ("base-dependency".into(), false),
            ("editor".into(), true),
            ("shared".into(), false),
        ])
    );
}

#[test]
fn reused_package_cache_does_not_add_removed_packages_to_the_download_request() {
    let fixture = Fixture::with_unified_packages();
    fixture.system("desktop", &["old", "kept"]);
    let backend = SimulatedBackend::default();
    let first = build_with_backend(&fixture.store, Some("desktop"), &backend)
        .unwrap()
        .remove(0);
    fixture.packages("desktop", &["new", "kept"]);
    backend.events.borrow_mut().clear();
    let second = build_with_backend(&fixture.store, Some("desktop"), &backend)
        .unwrap()
        .remove(0);
    assert_eq!(backend.count("predownload"), 1);
    assert_eq!(backend.count("collect_packages"), 0);
    assert_eq!(backend.count("predownload_update"), 1);
    assert_eq!(backend.count("update"), 0);
    assert_eq!(backend.count("maintain_packages"), 1);
    assert_eq!(
        download_systems(&backend),
        serde_json::json!({"desktop":["new","kept"]})
    );
    let inventory = SimulatedBackend::inventory(&second.root);
    assert!(!inventory.contains_key("old"));
    assert!(!inventory.contains_key("old-dependency"));
    assert_eq!(inventory.get("new"), Some(&true));
    assert_eq!(inventory.get("kept"), Some(&true));
    assert_eq!(inventory.get("shared"), Some(&false));
    assert_eq!(
        record(&fixture.cache(&second.version, "packages.toml"))["source_version"].as_str(),
        Some(first.version.as_str())
    );
    assert!(
        backend
            .events
            .borrow()
            .iter()
            .filter(|event| event.operation == "maintain_packages")
            .all(|event| event.root.ends_with("root"))
    );
}

#[test]
fn clearing_targets_updates_the_base_cache_once_and_drops_old_package_cache_contents() {
    let fixture = Fixture::with_unified_packages();
    fixture.system("desktop", &["old"]);
    let backend = SimulatedBackend::default();
    let first = build_with_backend(&fixture.store, Some("desktop"), &backend)
        .unwrap()
        .remove(0);
    fixture.packages("desktop", &[]);
    backend.events.borrow_mut().clear();
    let second = build_with_backend(&fixture.store, Some("desktop"), &backend)
        .unwrap()
        .remove(0);
    assert_eq!(backend.count("maintain_packages"), 1);
    assert_eq!(backend.count("update"), 0);
    assert_eq!(backend.count("collect_packages"), 0);
    assert_eq!(
        download_systems(&backend),
        serde_json::json!({"desktop":[]})
    );
    assert!(
        backend
            .events
            .borrow()
            .iter()
            .filter(|event| event.operation == "maintain_packages")
            .all(|event| event.root.ends_with("base-root"))
    );
    let expected = std::collections::BTreeMap::from([
        ("base".into(), true),
        ("base-dependency".into(), false),
    ]);
    assert_eq!(SimulatedBackend::inventory(&second.root), expected);
    assert_eq!(
        SimulatedBackend::inventory(&fixture.cache(&second.version, "base-system")),
        expected
    );
    assert_eq!(
        record(&fixture.cache(&second.version, "base-system.toml"))["source_version"].as_str(),
        Some(first.version.as_str())
    );
}

#[test]
fn legacy_plugin_keeps_the_original_package_operation_protocol() {
    let fixture = Fixture::with_predownload(true);
    fixture.system("desktop", &["old"]);
    let backend = SimulatedBackend::default();
    build_with_backend(&fixture.store, Some("desktop"), &backend).unwrap();
    assert_eq!(backend.count("maintain_packages"), 0);
    assert_eq!(backend.count("collect_packages"), 1);
    assert_eq!(backend.count("install"), 1);
    assert!(backend.count("update") > 0);
    assert_eq!(
        backend
            .events
            .borrow()
            .iter()
            .find(|event| event.operation == "predownload")
            .unwrap()
            .arguments,
        ["old"]
    );
}

#[test]
fn disabled_downloads_still_collect_and_apply_the_current_complete_plan() {
    let fixture = Fixture::with_unified_packages_without_predownload();
    fixture.system("desktop", &["old"]);
    let backend = SimulatedBackend::default();
    build_with_backend(&fixture.store, Some("desktop"), &backend).unwrap();
    fixture.packages("desktop", &["new"]);
    backend.events.borrow_mut().clear();
    let outcome = build_with_backend(&fixture.store, Some("desktop"), &backend)
        .unwrap()
        .remove(0);
    assert_eq!(backend.count("predownload"), 0);
    assert_eq!(backend.count("collect_packages"), 1);
    assert_eq!(backend.count("predownload_update"), 1);
    assert_eq!(backend.count("update"), 0);
    assert_eq!(backend.count("maintain_packages"), 1);
    let events = backend.events.borrow();
    assert_eq!(
        events
            .iter()
            .find(|event| event.operation == "collect_packages")
            .unwrap()
            .arguments,
        ["base", "new"]
    );
    assert_eq!(
        events
            .iter()
            .find(|event| event.operation == "maintain_packages")
            .unwrap()
            .arguments,
        [
            "--plan",
            "base",
            "base-dependency",
            "new",
            "shared",
            "--targets",
            "new"
        ]
    );
    assert_eq!(
        SimulatedBackend::inventory(&outcome.root),
        std::collections::BTreeMap::from([
            ("base".into(), true),
            ("base-dependency".into(), false),
            ("new".into(), true),
            ("shared".into(), false),
        ])
    );
}

#[test]
fn failed_combined_preparation_leaves_completed_targets_and_current_reference_untouched() {
    let fixture = Fixture::with_unified_packages();
    fixture.system("desktop", &["old"]);
    let backend = SimulatedBackend::default();
    let first = build_with_backend(&fixture.store, Some("desktop"), &backend)
        .unwrap()
        .remove(0);
    let layout = fixture.store.system_layout("desktop").unwrap();
    let current = fs::read(layout.root.join("build-current.toml")).unwrap();
    let packages = fs::read(fixture.cache(&first.version, "packages.toml")).unwrap();
    let published = SimulatedBackend::inventory(&first.root);
    fixture.packages("desktop", &["new"]);
    backend.events.borrow_mut().clear();
    backend.fail_operation.borrow_mut().replace("predownload");
    assert!(build_with_backend(&fixture.store, Some("desktop"), &backend).is_err());
    assert_eq!(backend.count("predownload"), 1);
    assert_eq!(backend.count("maintain_packages"), 0);
    assert!(
        backend
            .events
            .borrow()
            .iter()
            .all(|event| event.system_id == "predownload")
    );
    assert!(!layout.versions.join("v00000002").exists());
    assert!(!layout.cache.join("v00000002").exists());
    assert_eq!(
        fs::read(layout.root.join("build-current.toml")).unwrap(),
        current
    );
    assert_eq!(
        fs::read(fixture.cache(&first.version, "packages.toml")).unwrap(),
        packages
    );
    assert_eq!(SimulatedBackend::inventory(&first.root), published);
}

#[test]
fn malformed_combined_plans_stop_before_allocating_target_attempts() {
    let valid_plan = serde_json::json!({
        "schema_version": 1,
        "packages": ["base", "base-dependency", "editor", "shared"]
    });
    let mut responses = vec![
        "not json".to_owned(),
        serde_json::json!({"schema_version":2,"systems":{"desktop":valid_plan}}).to_string(),
        serde_json::json!({"schema_version":1,"systems":{}}).to_string(),
        serde_json::json!({"schema_version":1,"systems":{"other":valid_plan}}).to_string(),
        serde_json::json!({"schema_version":1,"systems":{"desktop":valid_plan,"extra":valid_plan}})
            .to_string(),
        serde_json::json!({"schema_version":1,"systems":{"desktop":valid_plan},"unknown":true})
            .to_string(),
    ];
    for invalid_plan in [
        serde_json::Value::Null,
        serde_json::json!({"schema_version":2,"packages":["base"]}),
        serde_json::json!({"schema_version":1,"packages":["base","base"]}),
        serde_json::json!({"schema_version":1,"packages":["bad name"]}),
        serde_json::json!({"schema_version":1,"packages":["base"],"unknown":true}),
    ] {
        responses.push(
            serde_json::json!({"schema_version":1,"systems":{"desktop":invalid_plan}}).to_string(),
        );
    }
    for response in responses {
        let fixture = Fixture::with_unified_packages();
        fixture.system("desktop", &["editor"]);
        let backend = SimulatedBackend::default();
        *backend.prepared_package_plans_override.borrow_mut() = Some(response.clone());
        assert!(
            build_with_backend(&fixture.store, Some("desktop"), &backend).is_err(),
            "accepted response: {response}"
        );
        let layout = fixture.store.system_layout("desktop").unwrap();
        assert!(!layout.root.join("version-counter").exists());
        assert!(!layout.versions.join("v00000001").exists());
        assert!(!layout.cache.join("v00000001").exists());
        assert!(!layout.root.join("build-current.toml").exists());
        assert_eq!(backend.count("maintain_packages"), 0);
        assert!(
            backend
                .events
                .borrow()
                .iter()
                .all(|event| event.system_id == "predownload")
        );
    }
}

#[test]
fn one_invalid_system_plan_still_allows_other_systems_in_the_batch_to_build() {
    let fixture = Fixture::with_unified_packages();
    fixture.system("desktop", &["editor"]);
    fixture.system("server", &["server"]);
    let backend = SimulatedBackend::default();
    *backend.prepared_package_plans_override.borrow_mut() = Some(
        serde_json::json!({
            "schema_version":1,
            "systems":{
                "desktop":{"schema_version":1,"packages":["bad name"]},
                "server":{"schema_version":1,"packages":["base","base-dependency","server"]}
            }
        })
        .to_string(),
    );
    let error = build_with_backend(&fixture.store, None, &backend)
        .unwrap_err()
        .to_string();
    assert!(error.contains("desktop"), "{error}");
    let desktop = fixture.store.system_layout("desktop").unwrap();
    let server = fixture.store.system_layout("server").unwrap();
    assert!(!desktop.root.join("version-counter").exists());
    assert!(!desktop.root.join("build-current.toml").exists());
    assert!(server.root.join("build-current.toml").exists());
    assert!(server.cache.join("v00000001/packages.toml").exists());
    assert_eq!(backend.count("predownload"), 1);
    assert_eq!(backend.count("maintain_packages"), 1);
}

#[test]
fn invalid_maintenance_states_cannot_commit_or_publish_the_new_package_stage() {
    let valid = serde_json::json!({
        "requested":["editor"],
        "installed":[
            {"name":"base","explicit":true},
            {"name":"base-dependency","explicit":false},
            {"name":"editor","explicit":true},
            {"name":"shared","explicit":false}
        ]
    });
    let mut missing_package = valid.clone();
    missing_package["installed"].as_array_mut().unwrap().pop();
    let mut extra_package = valid.clone();
    extra_package["installed"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!({"name":"unexpected","explicit":false}));
    let mut duplicate_package = valid.clone();
    duplicate_package["installed"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!({"name":"base","explicit":true}));
    let mut base_demoted = valid.clone();
    base_demoted["installed"][0]["explicit"] = serde_json::json!(false);
    let mut root_demoted = valid.clone();
    root_demoted["installed"][2]["explicit"] = serde_json::json!(false);
    let mut requested_omitted = valid.clone();
    requested_omitted["requested"] = serde_json::json!([]);
    requested_omitted["installed"][2]["explicit"] = serde_json::json!(false);
    let mut requested_added = valid.clone();
    requested_added["requested"] = serde_json::json!(["editor", "shared"]);
    requested_added["installed"][3]["explicit"] = serde_json::json!(true);
    for response in std::iter::once("not json".to_owned()).chain(
        [
            missing_package,
            extra_package,
            duplicate_package,
            base_demoted,
            root_demoted,
            requested_omitted,
            requested_added,
        ]
        .into_iter()
        .map(|state| state.to_string()),
    ) {
        let fixture = Fixture::with_unified_packages();
        fixture.system("desktop", &["old"]);
        let backend = SimulatedBackend::default();
        let first = build_with_backend(&fixture.store, Some("desktop"), &backend)
            .unwrap()
            .remove(0);
        let layout = fixture.store.system_layout("desktop").unwrap();
        let current = fs::read(layout.root.join("build-current.toml")).unwrap();
        let published = SimulatedBackend::inventory(&first.root);
        fixture.packages("desktop", &["editor"]);
        *backend.maintained_software_state_override.borrow_mut() = Some(response.clone());
        assert!(
            build_with_backend(&fixture.store, Some("desktop"), &backend).is_err(),
            "accepted maintenance state: {response}"
        );
        assert!(!layout.cache.join("v00000002/packages.toml").exists());
        assert_eq!(
            fs::read(layout.root.join("build-current.toml")).unwrap(),
            current
        );
        assert_eq!(SimulatedBackend::inventory(&first.root), published);
        assert_eq!(
            record(&layout.versions.join("v00000002/state.toml"))["state"].as_str(),
            Some("failed")
        );
    }
}

#[test]
fn a_configured_base_target_is_kept_protected_without_becoming_a_managed_target() {
    let fixture = Fixture::with_unified_packages();
    fixture.system("desktop", &["base", "editor"]);
    let backend = SimulatedBackend::default();
    let outcome = build_with_backend(&fixture.store, Some("desktop"), &backend)
        .unwrap()
        .remove(0);
    let packages = record(&fixture.cache(&outcome.version, "packages.toml"));
    assert_eq!(
        packages["software"]["requested"].as_array().unwrap(),
        &[toml::Value::String("editor".into())]
    );
    assert_eq!(
        SimulatedBackend::inventory(&outcome.root).get("base"),
        Some(&true)
    );
}
