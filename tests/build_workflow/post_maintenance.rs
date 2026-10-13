//! Verify optional distribution behavior runs after software, on an uncached writable root.
//! 验证可选发行版行为在软件维护后、不复用旧执行结果的可写根上运行。
use super::*;

#[test]
fn plugin_behavior_precedes_user_scripts_without_changing_software_caches() {
    let fixture = Fixture::with_unified_packages_without_predownload();
    fixture.system("desktop", &["editor"]);
    let layout = fixture.store.system_layout("desktop").unwrap();
    fs::write(
        layout.config.join("script/check.sh"),
        "set -eu\ntest -f \"$RSTAR_ROOT/.post-maintenance\"\nprintf user-script >> \"$RSTAR_ROOT/.post-maintenance\"\n",
    )
    .unwrap();
    let backend = SimulatedBackend::default();
    let outcome = build_with_backend(&fixture.store, Some("desktop"), &backend)
        .unwrap()
        .remove(0);
    assert_eq!(
        fs::read_to_string(outcome.root.join(".post-maintenance")).unwrap(),
        "1user-script"
    );
    for stage in ["base-system", "packages"] {
        assert!(
            !fixture
                .cache(&outcome.version, stage)
                .join(".post-maintenance")
                .exists()
        );
    }
    assert_eq!(
        fs::read_to_string(
            fixture
                .cache(&outcome.version, "script")
                .join(".post-maintenance")
        )
        .unwrap(),
        "1user-script"
    );
    let events = backend.events.borrow();
    let maintenance = events
        .iter()
        .position(|event| event.operation == "maintain_packages")
        .unwrap();
    let hook = events
        .iter()
        .position(|event| event.operation == "post_maintenance")
        .unwrap();
    let script = events
        .iter()
        .position(|event| event.operation == "script")
        .unwrap();
    assert!(maintenance < hook && hook < script);
    assert!(events[hook].arguments.is_empty());
    assert!(events[hook].root.ends_with("script-root"));
    fixture.cleaned(&outcome.version);
}

#[test]
fn behavior_reapplies_with_changed_configuration_and_cached_software_without_user_scripts() {
    // Empty package targets exercise software supplied by the maintained base as well.
    for packages in [&[][..], &["editor"][..]] {
        let fixture = Fixture::with_unified_packages_without_predownload();
        fixture.system("desktop", packages);
        let layout = fixture.store.system_layout("desktop").unwrap();
        let settings = layout.config.join("build/content.conf");
        fs::write(&settings, "[Content]\nRootPassword=first-value\n").unwrap();
        let backend = SimulatedBackend::default();
        let first = build_with_backend(&fixture.store, Some("desktop"), &backend)
            .unwrap()
            .remove(0);
        let bootstraps = backend.count("bootstrap");
        let base_installs = backend.count("base_install");
        fs::write(&settings, "[Content]\nRootPassword=second-value\n").unwrap();
        let second = build_with_backend(&fixture.store, Some("desktop"), &backend)
            .unwrap()
            .remove(0);
        assert_eq!(backend.count("post_maintenance"), 2);
        assert_eq!(backend.count("script"), 0);
        assert_eq!(backend.count("bootstrap"), bootstraps);
        assert_eq!(backend.count("base_install"), base_installs);
        assert_eq!(
            fs::read_to_string(first.root.join(".post-maintenance")).unwrap(),
            "1"
        );
        assert_eq!(
            fs::read_to_string(second.root.join(".post-maintenance")).unwrap(),
            "2"
        );
        for stage in ["base-system", "packages"] {
            assert!(
                !fixture
                    .cache(&second.version, stage)
                    .join(".post-maintenance")
                    .exists()
            );
        }
        assert_eq!(
            record(&fixture.cache(&second.version, "base-system.toml"))["source_version"].as_str(),
            Some(first.version.as_str())
        );
        if !packages.is_empty() {
            assert_eq!(
                record(&fixture.cache(&second.version, "packages.toml"))["source_version"].as_str(),
                Some(first.version.as_str())
            );
        }
        let events = backend.events.borrow();
        let hooks: Vec<_> = events
            .iter()
            .filter(|event| event.operation == "post_maintenance")
            .collect();
        assert_eq!(
            hooks[0].parameters["RSTAR_CONTENT_ROOTPASSWORD"],
            "first-value"
        );
        assert_eq!(
            hooks[1].parameters["RSTAR_CONTENT_ROOTPASSWORD"],
            "second-value"
        );
        let last_hook = events
            .iter()
            .rposition(|event| event.operation == "post_maintenance")
            .unwrap();
        assert!(
            events[last_hook + 1..]
                .iter()
                .any(|event| event.operation == "query_packages")
        );
        fixture.cleaned(&second.version);
    }
}

#[test]
fn hook_failure_blocks_scripts_and_publication_but_keeps_software_for_retry() {
    let fixture = Fixture::new();
    fixture.system("desktop", &["editor"]);
    let layout = fixture.store.system_layout("desktop").unwrap();
    fs::write(
        layout.config.join("script/after.sh"),
        "printf done > \"$RSTAR_ROOT/done\"\n",
    )
    .unwrap();
    let backend = SimulatedBackend::default();
    *backend.fail_operation.borrow_mut() = Some("post_maintenance");
    let error = build_with_backend(&fixture.store, Some("desktop"), &backend).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("injected post_maintenance failure")
    );
    assert_eq!(backend.count("post_maintenance"), 1);
    assert_eq!(backend.count("script"), 0);
    assert!(fixture.cache("v00000001", "packages.toml").is_file());
    assert!(!fixture.cache("v00000001", "script.toml").exists());
    assert!(!layout.versions.join("v00000001/root").exists());
    assert_eq!(
        record(&layout.versions.join("v00000001/state.toml"))["state"].as_str(),
        Some("failed")
    );
    fixture.cleaned("v00000001");
    backend.events.borrow_mut().clear();
    let retried = build_with_backend(&fixture.store, Some("desktop"), &backend)
        .unwrap()
        .remove(0);
    assert_eq!(backend.count("bootstrap"), 0);
    assert_eq!(backend.count("base_install"), 0);
    assert_eq!(backend.count("post_maintenance"), 1);
    assert_eq!(backend.count("script"), 1);
    assert_eq!(
        fs::read_to_string(retried.root.join("done")).unwrap(),
        "done"
    );
    fixture.cleaned(&retried.version);
}

#[test]
fn plugins_without_the_capability_skip_the_hook_even_when_an_entry_exists() {
    let fixture = Fixture::new();
    fixture.system("desktop", &["editor"]);
    let temporary = tempfile::tempdir().unwrap();
    let path = fixture.store.plugins_path().join("arch.tar");
    tar::Archive::new(fs::File::open(&path).unwrap())
        .unpack(temporary.path())
        .unwrap();
    let metadata_path = temporary.path().join("plugin.toml");
    let metadata = fs::read_to_string(&metadata_path)
        .unwrap()
        .replace("post_maintenance = true\n", "");
    fs::write(metadata_path, metadata).unwrap();
    let mut archive = tar::Builder::new(fs::File::create(path).unwrap());
    archive.append_dir_all(".", temporary.path()).unwrap();
    archive.finish().unwrap();
    let backend = SimulatedBackend::default();
    let outcome = build_with_backend(&fixture.store, Some("desktop"), &backend)
        .unwrap()
        .remove(0);
    assert_eq!(backend.count("post_maintenance"), 0);
    assert!(!outcome.root.join(".post-maintenance").exists());
    fixture.cleaned(&outcome.version);
}
