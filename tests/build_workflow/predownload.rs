//! Verify optional batch preparation and disposable environment/persistent roots.
use super::*;
use std::path::Path;

/// Rewrite the installed archive while keeping unrelated plugin inputs unchanged.
/// 重写已安装归档，保留其他插件输入。
fn rewrite_plugin(fixture: &Fixture, edit: impl FnOnce(&Path)) {
    let archive = fixture.store.plugins_path().join("arch.tar");
    let source = tempfile::tempdir().unwrap();
    tar::Archive::new(fs::File::open(&archive).unwrap())
        .unpack(source.path())
        .unwrap();
    edit(source.path());
    let mut output = tar::Builder::new(fs::File::create(archive).unwrap());
    output.append_dir_all(".", source.path()).unwrap();
    output.finish().unwrap();
}

/// Install another valid plugin without adding a system that selects it.
/// 安装另一个有效插件，不添加选择它的系统。
fn install_unused_plugin(fixture: &Fixture, version: &str) {
    let path = fixture
        .store
        .plugins_path()
        .join(format!("unused-{version}"));
    fs::create_dir(&path).unwrap();
    tar::Archive::new(fs::File::open(fixture.store.plugins_path().join("arch.tar")).unwrap())
        .unpack(&path)
        .unwrap();
    let metadata = path.join("plugin.toml");
    let mut data: toml::Value = toml::from_str(&fs::read_to_string(&metadata).unwrap()).unwrap();
    data["id"] = "unused".into();
    data["system_types"] = toml::Value::Array(vec!["unused".into()]);
    data["predownload_version"] = version.into();
    fs::write(metadata, toml::to_string(&data).unwrap()).unwrap();
}

/// Plugin byte and release changes keep the independently versioned resolver reusable.
/// 插件字节及发布版本变化后，独立版本控制的解析环境仍可复用。
#[test]
fn plugin_edits_reuse_the_declared_predownload_version() {
    let fixture = Fixture::with_predownload(true);
    fixture.system("desktop", &["editor"]);
    let backend = SimulatedBackend::default();
    build_with_backend(&fixture.store, Some("desktop"), &backend).unwrap();
    rewrite_plugin(&fixture, |root| {
        fs::write(root.join("new-resource"), "changed bytes").unwrap();
        let metadata = root.join("plugin.toml");
        let mut data: toml::Value =
            toml::from_str(&fs::read_to_string(&metadata).unwrap()).unwrap();
        data["version"] = "99.0.0".into();
        fs::write(metadata, toml::to_string(&data).unwrap()).unwrap();
    });
    backend.events.borrow_mut().clear();
    build_with_backend(&fixture.store, Some("desktop"), &backend).unwrap();
    assert_eq!(backend.count("predownload_install"), 0);
    assert_eq!(backend.count("predownload_update"), 1);
    assert_eq!(
        fs::read_dir(fixture.store.predownload_path().join("arch"))
            .unwrap()
            .count(),
        1
    );
}

/// Version changes retire all prior architectures and configs, including interrupted roots.
/// 版本变化清理全部旧架构及配置，包括中断后遗留的工作根。
#[test]
fn changed_predownload_version_retires_old_environments_and_rebuilds() {
    let fixture = Fixture::with_predownload(true);
    fixture.system("desktop", &["editor"]);
    let backend = SimulatedBackend::default();
    build_with_backend(&fixture.store, Some("desktop"), &backend).unwrap();
    let old = fixture.store.predownload_path().join("arch/1");
    for path in [
        "other-architecture/config/pending/work-root",
        "x86_64/interrupted/work-root",
    ] {
        let root = old.join(path);
        fs::create_dir_all(root.parent().unwrap()).unwrap();
        backend.create_subvolume(&root).unwrap();
        backend.create_subvolume(&root.join("nested")).unwrap();
    }
    rewrite_plugin(&fixture, |root| {
        let metadata = root.join("plugin.toml");
        let mut data: toml::Value =
            toml::from_str(&fs::read_to_string(&metadata).unwrap()).unwrap();
        data["predownload_version"] = "2".into();
        fs::write(metadata, toml::to_string(&data).unwrap()).unwrap();
    });
    backend.events.borrow_mut().clear();
    build_with_backend(&fixture.store, Some("desktop"), &backend).unwrap();
    assert!(!old.exists());
    assert!(
        fixture
            .store
            .predownload_path()
            .join("arch/2/x86_64")
            .is_dir()
    );
    assert_eq!(backend.count("predownload_install"), 1);
}

/// Scan plugins outside the selected system, preserve all installed versions and unknown owners.
/// 扫描所选系统之外的插件，保留全部已安装版本及未知归属。
#[test]
fn all_installed_plugins_are_cleaned_but_unknown_and_declared_versions_are_retained() {
    let fixture = Fixture::with_predownload(true);
    fixture.system("desktop", &["editor"]);
    install_unused_plugin(&fixture, "1");
    install_unused_plugin(&fixture, "2");
    let pool = fixture.store.predownload_path();
    let legacy = format!("{}/x86_64/config", "a".repeat(64));
    for path in ["unused/0", "unused/1", "unused/2", "unknown/old", &legacy] {
        fs::create_dir_all(pool.join(path)).unwrap();
        fs::write(pool.join(path).join("sentinel"), "keep if retained").unwrap();
    }
    let backend = SimulatedBackend::default();
    let root = pool.join("unused/0/x86_64/config/completed/root");
    fs::create_dir_all(root.parent().unwrap()).unwrap();
    backend.create_subvolume(&root).unwrap();
    build_with_backend(&fixture.store, Some("desktop"), &backend).unwrap();
    assert!(!pool.join("unused/0").exists());
    for path in ["unused/1", "unused/2", "unknown/old", &legacy] {
        assert!(pool.join(path).join("sentinel").is_file());
    }
}

/// Invalid unselected metadata prevents both deletion and build-attempt allocation.
/// 未选中插件的无效元数据同时阻止清理及构建尝试分配。
#[test]
fn invalid_installed_version_fails_before_cache_cleanup_or_attempts() {
    let fixture = Fixture::with_predownload(true);
    fixture.system("desktop", &["editor"]);
    install_unused_plugin(&fixture, "2");
    let metadata = fixture.store.plugins_path().join("unused-2/plugin.toml");
    let text = fs::read_to_string(&metadata).unwrap().replace(
        "predownload_version = \"2\"",
        "predownload_version = \"../escape\"",
    );
    fs::write(metadata, text).unwrap();
    let old = fixture.store.predownload_path().join("arch/0");
    fs::create_dir_all(&old).unwrap();
    let backend = SimulatedBackend::default();
    let error = build_with_backend(&fixture.store, Some("desktop"), &backend).unwrap_err();
    assert!(error.to_string().contains("predownload_version"));
    assert!(old.exists());
    assert!(backend.events.borrow().is_empty());
    assert_eq!(
        fs::read_dir(fixture.store.system_layout("desktop").unwrap().versions)
            .unwrap()
            .count(),
        0
    );
}

/// An in-use cache namespace must never be cleaned by a second build.
/// 正在使用的缓存命名空间不得被第二次构建清理。
#[test]
fn build_cache_lock_prevents_concurrent_retirement() {
    let fixture = Fixture::with_predownload(true);
    fixture.system("desktop", &["editor"]);
    let pool = fixture.store.predownload_path();
    fs::create_dir_all(pool.join("arch/old")).unwrap();
    let lock = fs::File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(pool.join(".build.lock"))
        .unwrap();
    lock.try_lock().unwrap();
    let backend = SimulatedBackend::default();
    let error = build_with_backend(&fixture.store, Some("desktop"), &backend).unwrap_err();
    assert!(error.to_string().contains("another build"));
    assert!(pool.join("arch/old").is_dir());
    assert!(backend.events.borrow().is_empty());
}

/// Reject symlink versions before removing any other obsolete environment.
/// 删除任何其他废弃环境前，拒绝符号链接版本。
#[test]
fn unsafe_retired_version_aborts_all_cleanup() {
    use std::os::unix::fs::symlink;
    let fixture = Fixture::with_predownload(true);
    fixture.system("desktop", &["editor"]);
    let pool = fixture.store.predownload_path();
    fs::create_dir_all(pool.join("arch/0")).unwrap();
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("sentinel"), "keep").unwrap();
    symlink(outside.path(), pool.join("arch/old-link")).unwrap();
    let backend = SimulatedBackend::default();
    assert!(build_with_backend(&fixture.store, Some("desktop"), &backend).is_err());
    assert!(pool.join("arch/0").exists());
    assert!(outside.path().join("sentinel").exists());
    assert!(backend.events.borrow().is_empty());
}

/// Different AUR policies remain separate even when the requested packages match.
/// 即使包目标相同，不同 AUR 策略仍分别准备，避免丢失强制重建要求。
#[test]
fn different_aur_policies_keep_preparation_groups_separate() {
    let fixture = Fixture::with_predownload(true);
    for (id, rebuild) in [("desktop", "none"), ("server", "all")] {
        fixture.system(id, &["editor"]);
        let directory = fixture.store.system_layout(id).unwrap().config;
        fs::write(
            directory.join("build/aur.conf"),
            format!("[Distribution]\nAurDevel=true\nAurRebuild={rebuild}\n"),
        )
        .unwrap();
    }
    let backend = SimulatedBackend::default();
    build_with_backend(&fixture.store, None, &backend).unwrap();
    let events = backend.events.borrow();
    let downloads: Vec<_> = events
        .iter()
        .filter(|event| event.operation == "predownload")
        .collect();
    assert_eq!(downloads.len(), 2);
    let policies: std::collections::BTreeSet<_> = downloads
        .iter()
        .map(|event| event.distribution_options["AurRebuild"].as_str())
        .collect();
    assert_eq!(policies, std::collections::BTreeSet::from(["all", "none"]));
}

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
    assert_eq!(events[pre].persistent_mounts.len(), 5);
    assert!(
        events[pre]
            .persistent_mounts
            .iter()
            .any(|mount| mount.target == Path::new("data"))
    );
    assert!(
        events
            .iter()
            .filter(|e| e.operation == "bootstrap")
            .all(|e| e.persistent_mounts.len() == 5)
    );
    let env = fs::read_dir(fixture.store.predownload_path().join("arch/1/x86_64"))
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
