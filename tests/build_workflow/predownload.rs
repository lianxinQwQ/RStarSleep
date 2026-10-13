//! Verify optional batch preparation and disposable environment/persistent roots.
use super::*;

/// Prevent one system's proxy from being used for a different download batch.
/// 避免一个系统的代理被用于另一下载批次。
/// # Arguments
/// No parameters. 无参数。
#[test]
fn different_environments_keep_download_batches_separate() {
    let fixture = Fixture::with_predownload(true);
    for (id, proxy) in [
        ("desktop", "http://one:10808"),
        ("server", "http://two:10808"),
    ] {
        fixture.system(id, &[id]);
        let layout = fixture.store.system_layout(id).unwrap();
        fs::write(
            layout.config.join("build/environment.conf"),
            format!("[Environment]\nhttp_proxy={proxy}\n"),
        )
        .unwrap();
    }
    let backend = SimulatedBackend::default();
    build_with_backend(&fixture.store, None, &backend).unwrap();
    assert_eq!(backend.count("predownload"), 2);
    let events = backend.events.borrow();
    for (id, proxy) in [
        ("desktop", "http://one:10808"),
        ("server", "http://two:10808"),
    ] {
        let download = events
            .iter()
            .find(|event| event.operation == "predownload" && event.arguments == [id])
            .unwrap();
        assert_eq!(download.environment["http_proxy"], Some(proxy.into()));
        assert!(
            events
                .iter()
                .filter(|event| event.system_id == id)
                .all(|event| event.environment["http_proxy"] == Some(proxy.into()))
        );
    }
}

/// Keep groups with different distribution settings separate and pass their exact frozen options.
/// 分离发行版设置不同的组，并传入各组准确的冻结选项。
/// # Arguments
/// No parameters.
/// 无参数。
#[test]
fn different_distribution_settings_get_independent_download_environments() {
    let fixture = Fixture::with_predownload(true);
    for (id, mirror) in [
        ("desktop", "https://one.invalid/$repo/$arch"),
        ("server", "https://two.invalid/$repo/$arch"),
    ] {
        fixture.system(id, &[id]);
        let path = fixture
            .store
            .system_layout(id)
            .unwrap()
            .config
            .join("build/config.conf");
        let text = fs::read_to_string(&path).unwrap().replace(
            "Architecture=x86_64",
            &format!("Architecture=x86_64\nMirror={mirror}"),
        );
        fs::write(path, text).unwrap();
    }
    let backend = SimulatedBackend::default();
    build_with_backend(&fixture.store, None, &backend).unwrap();
    assert_eq!(backend.count("predownload_install"), 2);
    assert_eq!(backend.count("predownload"), 2);
    let events = backend.events.borrow();
    for (id, mirror) in [
        ("desktop", "https://one.invalid/$repo/$arch"),
        ("server", "https://two.invalid/$repo/$arch"),
    ] {
        let event = events
            .iter()
            .find(|event| event.operation == "predownload" && event.arguments == [id])
            .unwrap();
        assert_eq!(event.distribution_options["Mirror"], mirror);
        assert!(
            events
                .iter()
                .filter(|event| event.system_id == id)
                .all(|event| event.distribution_options["Mirror"] == mirror)
        );
    }
}

#[test]
fn one_batch_union_is_prepared_before_any_target_and_environment_is_reused() {
    let fixture = Fixture::with_predownload(true);
    fixture.system("desktop", &["editor", "shared"]);
    fixture.system("server", &["server", "shared"]);
    let backend = SimulatedBackend::default();
    build_with_backend(&fixture.store, None, &backend).unwrap();
    assert_eq!(backend.count("predownload_install"), 1);
    assert_eq!(backend.count("predownload"), 1);
    let events = backend.events.borrow();
    let pre = events
        .iter()
        .position(|e| e.operation == "predownload")
        .unwrap();
    assert_eq!(events[pre].arguments, ["editor", "server", "shared"]);
    assert!(
        events[..pre]
            .iter()
            .all(|e| e.system_id == "predownload" || e.operation == "collect_packages")
    );
    assert_eq!(events[pre].persistent_mounts.len(), 4);
    assert!(
        events
            .iter()
            .filter(|e| e.operation == "bootstrap")
            .all(|e| e.persistent_mounts.len() == 4)
    );
    let hash = fs::read_dir(fixture.store.predownload_path())
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let env = fs::read_dir(hash.join("x86_64"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    assert!(env.join("completed/root").exists());
    assert!(!env.join("completed/root/.download-work-only").exists());
    assert!(!env.join("work-root").exists());
    drop(events);
    backend.events.borrow_mut().clear();
    build_with_backend(&fixture.store, None, &backend).unwrap();
    assert_eq!(backend.count("predownload_install"), 0);
    assert_eq!(backend.count("predownload_update"), 1);
    assert_eq!(backend.count("predownload"), 1);
}

#[test]
fn failed_preparation_blocks_targets_and_rolls_back_shared_data_but_keeps_environment() {
    let fixture = Fixture::with_predownload(true);
    fixture.system("desktop", &["old"]);
    fixture.system("server", &["old"]);
    let backend = SimulatedBackend::default();
    build_with_backend(&fixture.store, None, &backend).unwrap();
    let persistent = fixture
        .store
        .persistent_path()
        .join("arch-packages/.prepared-batch");
    assert_eq!(fs::read_to_string(&persistent).unwrap(), "old");
    fixture.packages("desktop", &["new"]);
    fixture.packages("server", &["new"]);
    backend.events.borrow_mut().clear();
    backend.fail_operation.borrow_mut().replace("predownload");
    assert!(build_with_backend(&fixture.store, None, &backend).is_err());
    assert!(
        backend
            .events
            .borrow()
            .iter()
            .all(|e| e.system_id == "predownload" || e.operation == "collect_packages")
    );
    assert_eq!(fs::read_to_string(persistent).unwrap(), "old");
    for id in ["desktop", "server"] {
        let status = record(
            &fixture
                .store
                .system_layout(id)
                .unwrap()
                .versions
                .join("v00000002/state.toml"),
        );
        assert_eq!(status["state"].as_str(), Some("failed"));
    }
    assert!(
        !fixture
            .store
            .root()
            .join("persistent-state/transaction")
            .exists()
    );
    backend.events.borrow_mut().clear();
    build_with_backend(&fixture.store, None, &backend).unwrap();
    assert_eq!(backend.count("predownload_install"), 0);
    assert_eq!(backend.count("predownload_update"), 1);
}

#[test]
fn absent_capability_keeps_normal_build_and_single_selection_limits_requirements() {
    let fixture = Fixture::new();
    fixture.system("desktop", &["editor"]);
    let backend = SimulatedBackend::default();
    build_with_backend(&fixture.store, Some("desktop"), &backend).unwrap();
    assert_eq!(backend.count("predownload"), 0);
    let fixture = Fixture::with_predownload(true);
    fixture.system("desktop", &["editor"]);
    fixture.system("server", &["server"]);
    let backend = SimulatedBackend::default();
    build_with_backend(&fixture.store, Some("desktop"), &backend).unwrap();
    assert_eq!(
        backend
            .events
            .borrow()
            .iter()
            .find(|e| e.operation == "predownload")
            .unwrap()
            .arguments,
        ["editor"]
    );
}

#[test]
fn all_batch_inputs_are_frozen_before_environment_bootstrap_can_change_editable_configs() {
    let fixture = Fixture::with_predownload(true);
    fixture.system("desktop", &["editor"]);
    fixture.system("server", &["server"]);
    let backend = SimulatedBackend::default();
    backend.mutate_editable.borrow_mut().replace(
        fixture
            .store
            .system_layout("server")
            .unwrap()
            .config
            .join("build/config.conf"),
    );
    build_with_backend(&fixture.store, None, &backend).unwrap();
    assert_eq!(
        backend
            .events
            .borrow()
            .iter()
            .find(|e| e.operation == "predownload")
            .unwrap()
            .arguments,
        ["editor", "server"]
    );
    assert!(
        backend
            .events
            .borrow()
            .iter()
            .filter(|e| e.system_id == "server")
            .all(|e| e.packages == ["server"])
    );
}

#[test]
fn preparation_includes_installed_packages_that_full_upgrade_will_touch() {
    let fixture = Fixture::with_predownload(true);
    fixture.system("desktop", &["old"]);
    let backend = SimulatedBackend::default();
    build_with_backend(&fixture.store, Some("desktop"), &backend).unwrap();
    fixture.packages("desktop", &["new"]);
    backend.events.borrow_mut().clear();
    build_with_backend(&fixture.store, Some("desktop"), &backend).unwrap();
    let events = backend.events.borrow();
    let requirements = &events
        .iter()
        .find(|e| e.operation == "predownload")
        .unwrap()
        .arguments;
    for name in ["new", "old", "base", "base-dependency"] {
        assert!(requirements.iter().any(|p| p == name));
    }
}
