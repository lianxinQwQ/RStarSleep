//! Verify opt-in, batch-local software-stage sharing without changing export indices.
use super::*;
use std::path::PathBuf;

fn enable(f: &Fixture, id: &str) {
    let path = f
        .store
        .system_layout(id)
        .unwrap()
        .config
        .join("build/config.conf");
    let text = fs::read_to_string(&path).unwrap().replace(
        "SchemaVersion=1",
        "SchemaVersion=1\nDeduplicatePackages=yes",
    );
    fs::write(path, text).unwrap();
}
fn index(f: &Fixture, id: &str, version: &str) -> PathBuf {
    f.store
        .system_layout(id)
        .unwrap()
        .cache
        .join(version)
        .join("packages-files.tsv")
}
fn script(f: &Fixture, id: &str, text: &str) {
    fs::write(
        f.store
            .system_layout(id)
            .unwrap()
            .config
            .join("script/change.sh"),
        text,
    )
    .unwrap();
}

#[test]
fn disabled_has_no_extra_query_or_software_index() {
    let f = Fixture::new();
    f.system("desktop", &["editor"]);
    let b = SimulatedBackend::default();
    let outcome = build_with_backend(&f.store, Some("desktop"), &b)
        .unwrap()
        .remove(0);
    assert!(!index(&f, "desktop", &outcome.version).exists());
    assert_eq!(b.count("query_package_files"), 1);
    assert!(
        outcome
            .root
            .parent()
            .unwrap()
            .join("application-files.tsv")
            .is_file()
    );
    assert!(b.dedup_attempts.borrow().is_empty());
}

#[test]
fn multiple_donors_and_mismatching_files_fall_back_to_earlier_sources() {
    let f = Fixture::with_unified_packages_without_predownload();
    for (id, packages) in [
        ("a", vec!["editor", "a-only"]),
        ("b", vec!["editor", "b-only"]),
        ("c", vec!["editor", "a-only", "b-only"]),
    ] {
        f.system(id, &packages);
        enable(&f, id);
    }
    // Changes to the published root must not become deduplication sources.
    script(&f, "a", "printf changed > \"$RSTAR_ROOT/usr/bin/editor\"\n");
    let b = SimulatedBackend::default();
    b.generate_package_files.set(true);
    // Keep the differing file the same length so the backend performs a content comparison.
    b.package_file_contents.borrow_mut().insert(
        ("b".into(), "editor".into()),
        vec![b'x'; "editor".len() * 4096],
    );
    let outcomes = build_with_backend(&f.store, None, &b).unwrap();
    assert_eq!(outcomes.len(), 3);
    assert_eq!(b.count("query_package_files"), 6);
    let attempts = b.dedup_attempts.borrow();
    let c_editor: Vec<_> = attempts
        .iter()
        .filter(|(_, target)| target.ends_with("work/c/v00000001/root/usr/bin/editor"))
        .collect();
    assert_eq!(c_editor.len(), 2);
    assert!(
        c_editor[0]
            .0
            .ends_with("systems/b/cache/v00000001/packages/usr/bin/editor")
    );
    assert!(
        c_editor[1]
            .0
            .ends_with("systems/a/cache/v00000001/packages/usr/bin/editor")
    );
    for (package, donor) in [("a-only", "a"), ("b-only", "b")] {
        assert!(
            attempts
                .iter()
                .any(|(source, target)| source.ends_with(format!(
                    "systems/{donor}/cache/v00000001/packages/usr/bin/{package}"
                )) && target
                    .ends_with(format!("work/c/v00000001/root/usr/bin/{package}")))
        );
    }
    assert!(
        attempts
            .iter()
            .all(|(source, _)| source.to_string_lossy().contains("/packages/usr/bin/"))
    );
}

#[test]
fn stage_three_index_still_captures_packages_added_by_scripts() {
    let f = Fixture::new();
    f.system("desktop", &["editor"]);
    enable(&f, "desktop");
    script(
        &f,
        "desktop",
        "printf 'extra\\texplicit\\n' >> \"$RSTAR_ROOT/.simulated-packages\"\n",
    );
    let b = SimulatedBackend::default();
    let outcome = build_with_backend(&f.store, Some("desktop"), &b)
        .unwrap()
        .remove(0);
    assert!(
        !fs::read_to_string(index(&f, "desktop", &outcome.version))
            .unwrap()
            .contains("extra\t")
    );
    assert!(
        fs::read_to_string(outcome.root.parent().unwrap().join("application-files.tsv"))
            .unwrap()
            .contains("extra\t")
    );
}

#[test]
fn enabled_failed_system_does_not_become_a_donor() {
    let f = Fixture::new();
    for id in ["a", "b", "c"] {
        f.system(id, &["editor"]);
        enable(&f, id);
    }
    script(&f, "b", "exit 1\n");
    let b = SimulatedBackend::default();
    b.generate_package_files.set(true);
    assert!(build_with_backend(&f.store, None, &b).is_err());
    let attempts = b.dedup_attempts.borrow();
    assert!(attempts.iter().any(
        |(source, target)| source.to_string_lossy().contains("/a/cache/")
            && target.to_string_lossy().contains("/work/c/")
    ));
    assert!(
        !attempts
            .iter()
            .any(|(source, _)| source.to_string_lossy().contains("/b/cache/"))
    );
}

#[test]
fn reuse_copies_stage_index_and_legacy_cache_is_queried_in_a_writable_copy() {
    let f = Fixture::new();
    f.system("desktop", &["editor"]);
    enable(&f, "desktop");
    fs::write(
        f.store.global_config_path(),
        "schema_version=1\ncache_reuse_minutes=10\n",
    )
    .unwrap();
    let b = SimulatedBackend::default();
    let first = build_with_backend(&f.store, Some("desktop"), &b)
        .unwrap()
        .remove(0);
    b.events.borrow_mut().clear();
    let second = build_with_backend(&f.store, Some("desktop"), &b)
        .unwrap()
        .remove(0);
    assert_eq!(b.count("query_package_files"), 0);
    assert_eq!(
        fs::read(index(&f, "desktop", &first.version)).unwrap(),
        fs::read(index(&f, "desktop", &second.version)).unwrap()
    );
    fs::remove_file(index(&f, "desktop", &second.version)).unwrap();
    b.events.borrow_mut().clear();
    let third = build_with_backend(&f.store, Some("desktop"), &b)
        .unwrap()
        .remove(0);
    assert!(index(&f, "desktop", &third.version).is_file());
    let events = b.events.borrow();
    let queries: Vec<_> = events
        .iter()
        .filter(|event| event.operation == "query_package_files")
        .collect();
    assert_eq!(queries.len(), 1);
    assert!(queries[0].root.ends_with("packages-index-root"));
    assert!(b.dedup_attempts.borrow().is_empty());
}

#[test]
fn missing_plugin_capability_fails_before_allocating_a_version() {
    let f = Fixture::new();
    f.system("desktop", &["editor"]);
    enable(&f, "desktop");
    let source = f.store.root().parent().unwrap().join("plugin-source");
    let metadata = source.join("plugin.toml");
    fs::write(
        &metadata,
        fs::read_to_string(&metadata)
            .unwrap()
            .replace("query_package_files = true", "query_package_files = false"),
    )
    .unwrap();
    let mut archive =
        tar::Builder::new(fs::File::create(f.store.plugins_path().join("arch.tar")).unwrap());
    archive.append_dir_all(".", source).unwrap();
    archive.finish().unwrap();
    let b = SimulatedBackend::default();
    let error = build_with_backend(&f.store, Some("desktop"), &b)
        .err()
        .unwrap()
        .to_string();
    assert!(
        error.contains("DeduplicatePackages") && error.contains("query_package_files"),
        "{error}"
    );
    let layout = f.store.system_layout("desktop").unwrap();
    assert!(!layout.versions.join("v00000001").exists());
    assert!(!layout.root.join("version-counter").exists());
    assert!(b.events.borrow().is_empty());
}

#[test]
fn historical_builds_and_disabled_systems_are_not_donors() {
    let f = Fixture::new();
    f.system("a", &["editor"]);
    enable(&f, "a");
    f.system("b", &["editor"]);
    let b = SimulatedBackend::default();
    b.generate_package_files.set(true);
    build_with_backend(&f.store, Some("a"), &b).unwrap();
    enable(&f, "b");
    build_with_backend(&f.store, Some("b"), &b).unwrap();
    assert!(b.dedup_attempts.borrow().is_empty());
    let path = f
        .store
        .system_layout("a")
        .unwrap()
        .config
        .join("build/config.conf");
    fs::write(
        &path,
        fs::read_to_string(&path)
            .unwrap()
            .replace("DeduplicatePackages=yes", "DeduplicatePackages=no"),
    )
    .unwrap();
    build_with_backend(&f.store, None, &b).unwrap();
    assert!(b.dedup_attempts.borrow().is_empty());
    assert!(!index(&f, "a", "v00000002").exists());
}

#[test]
fn legacy_toml_can_enable_deduplication_without_plugin_configuration_fields() {
    let f = Fixture::new();
    f.system("desktop", &["editor"]);
    let config = f.store.system_layout("desktop").unwrap().config;
    fs::remove_file(config.join("build/config.conf")).unwrap();
    fs::write(config.join("build/config.toml"), "schema_version=1\nsystem_type='arch'\narchitecture='x86_64'\npackages=['editor']\ndeduplicate_packages=true\n").unwrap();
    let b = SimulatedBackend::default();
    let outcome = build_with_backend(&f.store, Some("desktop"), &b)
        .unwrap()
        .remove(0);
    assert!(index(&f, "desktop", &outcome.version).is_file());
    assert_eq!(b.count("query_package_files"), 2);
}

#[test]
fn deduplication_io_failure_does_not_commit_the_software_stage_or_publish_the_target() {
    let f = Fixture::new();
    for id in ["a", "b"] {
        f.system(id, &["editor"]);
        enable(&f, id);
    }
    let b = SimulatedBackend::default();
    b.generate_package_files.set(true);
    b.fail_deduplication.set(true);
    let error = build_with_backend(&f.store, None, &b)
        .err()
        .unwrap()
        .to_string();
    assert!(
        error.contains("文件去重失败") && error.contains("/a/cache/") && error.contains("/work/b/"),
        "{error}"
    );
    let layout = f.store.system_layout("b").unwrap();
    assert!(!layout.cache.join("v00000001/packages.toml").exists());
    assert!(!layout.cache.join("v00000001/packages").exists());
    assert!(rstarsleep::controller::read_current_build(&f.store, "b").is_err());
    assert_eq!(
        rstarsleep::controller::read_current_build(&f.store, "a")
            .unwrap()
            .version,
        "v00000001"
    );
}
