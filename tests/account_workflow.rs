#[allow(dead_code, unused_imports)]
mod support;
use rstarsleep::{
    config::AccountDriftPolicy,
    controller::{Error, build_with_backend, build_with_backend_and_warning},
};
use std::fs;
use support::build::{Fixture, SimulatedBackend};

const EMPTY: &str = r#"{"schema_version":1,"consistent":true}"#;
const DROPPED: &str = r#"{"schema_version":1,"consistent":false,"dropped_users":["alice"]}"#;

#[test]
fn preflight_combines_account_and_inheritance_warnings_once_before_any_build() {
    let fixture = Fixture::new();
    fixture.system("desktop", &["editor"]);
    let source = fixture.store.root().parent().unwrap().join("source");
    fs::write(&source, "source account data").unwrap();
    fixture.account_plugin(&source, DROPPED, EMPTY);
    let layout = fixture.store.system_layout("desktop").unwrap();
    fs::write(
        layout.config.join("build/inherit.conf"),
        format!("[Inheritance]\nCopyOnWriteBoot={}\n", source.display()),
    )
    .unwrap();
    let backend = SimulatedBackend::default();
    let mut count = 0;
    let error =
        build_with_backend_and_warning(&fixture.store, Some("desktop"), &backend, |messages| {
            count += 1;
            assert!(messages.iter().any(|m| m.contains("account-selection")));
            assert!(messages.iter().any(|m| m.contains("account-override")));
            assert!(messages.iter().any(|m| m.contains("inheritance")));
            assert_eq!(backend.count("bootstrap"), 0);
            assert_eq!(fs::read_dir(&layout.versions).unwrap().count(), 0);
            Err(Error::Invalid("cancelled before build".into()))
        })
        .unwrap_err();
    assert!(error.to_string().contains("cancelled"));
    assert_eq!(count, 1);
}

#[test]
fn source_errors_fail_before_allocation_and_no_warning_wait() {
    let fixture = Fixture::new();
    fixture.system("desktop", &["editor"]);
    let source = fixture.store.root().parent().unwrap().join("source");
    fs::write(&source, "data").unwrap();
    fixture.account_plugin(
        &source,
        r#"{"schema_version":1,"consistent":false,"differences":["missing selected user"]}"#,
        EMPTY,
    );
    let backend = SimulatedBackend::default();
    let error = build_with_backend_and_warning(&fixture.store, Some("desktop"), &backend, |_| {
        panic!("must fail before warning callback")
    })
    .unwrap_err();
    assert!(error.to_string().contains("missing selected user"));
    assert_eq!(backend.count("bootstrap"), 0);
    assert_eq!(
        fs::read_dir(fixture.store.system_layout("desktop").unwrap().versions)
            .unwrap()
            .count(),
        0
    );
}

#[test]
fn account_write_precedes_password_hook_and_source_changes_invalidate_script_reuse() {
    let fixture = Fixture::new();
    fixture.system("desktop", &["editor"]);
    fs::write(
        fixture.store.root().join("config/rstarsleep.toml"),
        "schema_version = 1\ncache_reuse_minutes = 60\n",
    )
    .unwrap();
    let source = fixture.store.root().parent().unwrap().join("source");
    fs::write(&source, "first").unwrap();
    fixture.account_plugin(&source, EMPTY, EMPTY);
    let layout = fixture.store.system_layout("desktop").unwrap();
    fs::write(
        layout.config.join("script/check.sh"),
        "set -eu\ntest -f \"$RSTAR_ROOT/.accounts\"\ntest -f \"$RSTAR_ROOT/.post-maintenance\"\n",
    )
    .unwrap();
    let backend = SimulatedBackend::default();
    let first = build_with_backend(&fixture.store, Some("desktop"), &backend)
        .unwrap()
        .remove(0);
    assert_eq!(
        fs::read_to_string(first.root.join(".accounts")).unwrap(),
        "inherited"
    );
    let posts = backend.count("post_maintenance");
    build_with_backend(&fixture.store, Some("desktop"), &backend).unwrap();
    assert_eq!(backend.count("post_maintenance"), posts);
    fs::write(&source, "second").unwrap();
    build_with_backend(&fixture.store, Some("desktop"), &backend).unwrap();
    assert_eq!(backend.count("post_maintenance"), posts + 1);
    assert_eq!(backend.count("bootstrap"), 1);
}

#[test]
fn drift_policy_controls_publication_without_repair() {
    for policy in [
        AccountDriftPolicy::Silent,
        AccountDriftPolicy::Warn,
        AccountDriftPolicy::Error,
        AccountDriftPolicy::Assess,
    ] {
        let fixture = Fixture::new();
        fixture.system("desktop", &["editor"]);
        let source = fixture.store.root().parent().unwrap().join("source");
        fs::write(&source, "data").unwrap();
        fixture.account_plugin(&source, EMPTY, r#"{"schema_version":1,"consistent":false,"drifts":[{"kind":"uid","name":"alice","old_id":1000,"new_id":2000,"new_name":"alice"}]}"#);
        let name = match policy {
            AccountDriftPolicy::Silent => "silent",
            AccountDriftPolicy::Warn => "warn",
            AccountDriftPolicy::Error => "error",
            AccountDriftPolicy::Assess => "assess",
        };
        let layout = fixture.store.system_layout("desktop").unwrap();
        fs::write(
            layout.config.join("build/accounts.conf"),
            format!("[AccountInheritance]\nDriftPolicy={name}\n"),
        )
        .unwrap();
        let result = build_with_backend(
            &fixture.store,
            Some("desktop"),
            &SimulatedBackend::default(),
        );
        assert_eq!(result.is_ok(), policy != AccountDriftPolicy::Error);
        if policy == AccountDriftPolicy::Error {
            assert!(!layout.versions.join("v00000001/root").exists());
        }
    }
}

#[test]
fn selected_source_root_supplies_account_preflight_environment_and_frozen_inputs() {
    use rstarsleep::controller::{BuildOptions, build_with_options_and_backend_and_warning};
    use std::path::Path;

    let fixture = Fixture::new();
    fixture.system("desktop", &["editor"]);
    let parent = fixture.store.root().parent().unwrap();
    let selected = parent.join("selected-source");
    fs::create_dir_all(selected.join("rstar-test")).unwrap();
    fs::write(
        selected.join("rstar-test/account"),
        "frozen-before-preflight",
    )
    .unwrap();
    fixture.account_plugin(Path::new("/rstar-test/account"), EMPTY, EMPTY);
    let plugin_source = parent.join("plugin-source");
    let quoted_root = format!("'{}'", selected.to_str().unwrap().replace('\'', "'\\''"));
    fs::write(
        plugin_source.join("account_check.sh"),
        format!(
            "set -eu\nif [ \"${{3:-}}\" = --preflight ]; then\n  test \"$RSTAR_ROOT\" = {quoted_root}\n  test \"$(cat \"$2/old/rstar-test/account\")\" = frozen-before-preflight\n  test \"$(cat \"$RSTAR_ROOT/rstar-test/account\")\" = frozen-before-preflight\n  printf changed-after-preflight > \"$RSTAR_ROOT/rstar-test/account\"\n  cat \"$RSTAR_PLUGIN_ROOT/account-preflight.json\"\nelse\n  cat \"$RSTAR_PLUGIN_ROOT/account-result.json\"\nfi\n"
        ),
    )
    .unwrap();
    fs::write(
        plugin_source.join("account_prepare.sh"),
        "set -eu\ntest \"$(cat \"$2/old/rstar-test/account\")\" = frozen-before-preflight\ncat \"$2/old/rstar-test/account\" > \"$RSTAR_ROOT/.account-frozen-source\"\n",
    )
    .unwrap();
    let archive = fs::File::create(fixture.store.plugins_path().join("arch.tar")).unwrap();
    let mut archive = tar::Builder::new(archive);
    archive.append_dir_all(".", &plugin_source).unwrap();
    archive.finish().unwrap();

    let backend = SimulatedBackend::default();
    let outcome = build_with_options_and_backend_and_warning(
        &fixture.store,
        Some("desktop"),
        &BuildOptions {
            inheritance_root: selected.clone(),
        },
        &backend,
        |_| Ok(()),
    )
    .unwrap()
    .remove(0);
    assert_eq!(
        fs::read_to_string(outcome.root.join(".account-frozen-source")).unwrap(),
        "frozen-before-preflight"
    );
    assert_eq!(
        fs::read_to_string(selected.join("rstar-test/account")).unwrap(),
        "changed-after-preflight"
    );
    assert_eq!(
        fs::read_to_string(outcome.root.join(".accounts")).unwrap(),
        "inherited"
    );
}
