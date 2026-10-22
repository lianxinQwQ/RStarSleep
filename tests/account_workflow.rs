#[allow(dead_code, unused_imports)]
mod support;
use rstarsleep::{config::AccountDriftPolicy, controller::Error};
use std::fs;
use support::build::{Fixture, SimulatedBackend};

/// Keep these stage tests tied to their explicit host-side source paths.
/// 将这些阶段测试固定到其显式宿主来源路径。
fn build_with_backend(
    store: &rstarsleep::config::ConfigStore,
    system: Option<&str>,
    backend: &impl rstarsleep::controller::BuildBackend,
) -> rstarsleep::controller::Result<Vec<rstarsleep::controller::BuildOutcome>> {
    if system.is_none() {
        return rstarsleep::controller::build_with_backend(store, system, backend);
    }
    rstarsleep::controller::build_with_options_and_backend(
        store,
        system,
        &rstarsleep::controller::BuildOptions {
            inheritance_root: Some("/".into()),
        },
        backend,
    )
}

/// Exercise the shared preflight callback with an explicitly selected source root.
/// 使用显式选择的来源根验证统一预检回调。
fn build_with_backend_and_warning(
    store: &rstarsleep::config::ConfigStore,
    system: Option<&str>,
    backend: &impl rstarsleep::controller::BuildBackend,
    warning: impl FnMut(&[String]) -> rstarsleep::controller::Result<()>,
) -> rstarsleep::controller::Result<Vec<rstarsleep::controller::BuildOutcome>> {
    rstarsleep::controller::build_with_options_and_backend_and_warning(
        store,
        system,
        &rstarsleep::controller::BuildOptions {
            inheritance_root: Some("/".into()),
        },
        backend,
        warning,
    )
}

const EMPTY: &str = r#"{"schema_version":1,"consistent":true}"#;
const DROPPED: &str = r#"{"schema_version":1,"consistent":false,"dropped_users":["alice"]}"#;

/// Edit an isolated plugin's metadata and resources, then replace its test archive.
/// 编辑隔离插件的元数据和资源，然后替换测试归档。
fn configure_plugin(fixture: &Fixture, edit: impl FnOnce(&std::path::Path, &mut toml::Value)) {
    let path = fixture.store.plugins_path().join("arch.tar");
    let plugin = rstarsleep::controller::plugins::load(&path).unwrap();
    let metadata = plugin.root().join("plugin.toml");
    let mut parsed = toml::from_str(&fs::read_to_string(&metadata).unwrap()).unwrap();
    edit(plugin.root(), &mut parsed);
    fs::write(metadata, toml::to_string(&parsed).unwrap()).unwrap();
    let mut archive = tar::Builder::new(fs::File::create(path).unwrap());
    archive.append_dir_all(".", plugin.root()).unwrap();
    archive.finish().unwrap();
}

/// Advertising account actions must not implicitly accept any account configuration.
/// 声明账户能力不应自动接受任何账户配置。
#[test]
fn account_capability_requires_explicit_configuration_declarations() {
    for (field, value) in [
        ("Users", "alice"),
        ("Groups", "wheel"),
        ("DriftPolicy", "warn"),
    ] {
        let fixture = Fixture::new();
        fixture.system("desktop", &[]);
        let source = fixture.store.root().parent().unwrap().join("source");
        fs::write(&source, "data").unwrap();
        fixture.account_plugin(&source, EMPTY, EMPTY);
        let name = format!("RSTAR_ACCOUNTINHERITANCE_{}", field.to_ascii_uppercase());
        configure_plugin(&fixture, |_, metadata| {
            metadata["config"]["optional"]
                .as_array_mut()
                .unwrap()
                .retain(|entry| entry.as_str() != Some(name.as_str()));
            metadata["config"]["constraints"]
                .as_table_mut()
                .unwrap()
                .remove(&name);
        });
        let layout = fixture.store.system_layout("desktop").unwrap();
        fs::write(
            layout.config.join("build/accounts.conf"),
            format!("[AccountInheritance]\n{field}={value}\n"),
        )
        .unwrap();
        let backend = SimulatedBackend::default();
        let error = build_with_backend(&fixture.store, Some("desktop"), &backend).unwrap_err();
        let message = error.to_string();
        assert!(message.contains("undeclared settings"), "{message}");
        assert!(message.contains(&name), "{message}");
        assert!(backend.events.borrow().is_empty());
        assert_eq!(fs::read_dir(&layout.versions).unwrap().count(), 0);
    }
}

/// Account constraints and required fields fail before account checks or build allocation.
/// 账户约束和必填字段错误应早于账户检查和构建分配。
#[test]
fn account_metadata_rejects_invalid_configuration_before_operations() {
    for (settings, expected) in [
        ("DriftPolicy=unknown\n", "enum candidate values"),
        (
            "DriftPolicy=warn\nDriftPolicy=error\n",
            "requires a single item",
        ),
        ("Custom=undeclared\n", "undeclared settings"),
        ("", "missing or empty required settings"),
    ] {
        let fixture = Fixture::new();
        fixture.system("desktop", &[]);
        let source = fixture.store.root().parent().unwrap().join("source");
        fs::write(&source, "data").unwrap();
        fixture.account_plugin(&source, "invalid check result", EMPTY);
        if settings.is_empty() {
            configure_plugin(&fixture, |_, metadata| {
                let name = "RSTAR_ACCOUNTINHERITANCE_USERS";
                metadata["config"]["optional"]
                    .as_array_mut()
                    .unwrap()
                    .retain(|entry| entry.as_str() != Some(name));
                metadata["config"]["required"]
                    .as_array_mut()
                    .unwrap()
                    .push(name.into());
            });
        }
        let layout = fixture.store.system_layout("desktop").unwrap();
        fs::write(
            layout.config.join("build/accounts.conf"),
            format!("[AccountInheritance]\n{settings}"),
        )
        .unwrap();
        let backend = SimulatedBackend::default();
        let message = build_with_backend(&fixture.store, Some("desktop"), &backend)
            .unwrap_err()
            .to_string();
        assert!(message.contains(expected), "{message}");
        assert!(backend.events.borrow().is_empty());
        assert_eq!(fs::read_dir(&layout.versions).unwrap().count(), 0);
    }
}

/// Declared fields remain ordinary plugin inputs without enabling account operations.
/// 已声明字段在未开启账户能力时仍是普通插件输入。
#[test]
fn declared_account_fields_do_not_enable_account_operations() {
    let fixture = Fixture::new();
    fixture.system("desktop", &[]);
    configure_plugin(&fixture, |_, metadata| {
        metadata["config"]["constraints"]["RSTAR_ACCOUNTINHERITANCE_DRIFTPOLICY"]["values"] =
            toml::Value::Array(vec!["plugin-policy".into()]);
    });
    let layout = fixture.store.system_layout("desktop").unwrap();
    fs::write(
        layout.config.join("build/accounts.conf"),
        "[AccountInheritance]\nUsers=plugin-specific name\nGroups=plugin-specific group\nDriftPolicy=plugin-policy\n",
    )
    .unwrap();
    let backend = SimulatedBackend::default();
    let outcome = build_with_backend(&fixture.store, Some("desktop"), &backend)
        .unwrap()
        .remove(0);
    assert!(!outcome.root.join(".accounts").exists());
    let events = backend.events.borrow();
    assert!(!events.is_empty());
    assert!(events.iter().all(|event| {
        !event.operation.starts_with("account_")
            && event.parameters["RSTAR_ACCOUNTINHERITANCE_USERS"] == "plugin-specific name"
            && event.parameters["RSTAR_ACCOUNTINHERITANCE_GROUPS"] == "plugin-specific group"
            && event.parameters["RSTAR_ACCOUNTINHERITANCE_DRIFTPOLICY"] == "plugin-policy"
    }));
}

/// Declared extensions and original lists reach account scripts alongside standard requests.
/// 已声明扩展字段及原始列表应与标准请求一同传入账户脚本。
#[test]
fn declared_account_extensions_reach_plugin_environment_and_requests() {
    let fixture = Fixture::new();
    fixture.system("desktop", &[]);
    let source = fixture.store.root().parent().unwrap().join("source");
    fs::write(&source, "data").unwrap();
    fixture.account_plugin(&source, EMPTY, EMPTY);
    configure_plugin(&fixture, |root, metadata| {
        metadata["config"]["optional"]
            .as_array_mut()
            .unwrap()
            .push("RSTAR_ACCOUNTINHERITANCE_CUSTOM".into());
        fs::write(
            root.join("account_prepare.sh"),
            "set -eu\ntest \"$RSTAR_ACCOUNTINHERITANCE_CUSTOM\" = plugin-choice\ntest \"$RSTAR_ACCOUNTINHERITANCE_GROUPS\" = wheel\nprintf '%s' \"$RSTAR_PARAMETERS_JSON\" > \"$RSTAR_ROOT/.account-parameters.json\"\ncat \"$2/request.json\" > \"$RSTAR_ROOT/.account-request.json\"\n",
        )
        .unwrap();
    });
    let layout = fixture.store.system_layout("desktop").unwrap();
    fs::write(
        layout.config.join("build/accounts.conf"),
        "[AccountInheritance]\nUsers=alice\nUsers=bob\nUsers=alice\nGroups=wheel\nDriftPolicy=warn\nCustom=plugin-choice\n",
    )
    .unwrap();
    let outcome = build_with_backend(
        &fixture.store,
        Some("desktop"),
        &SimulatedBackend::default(),
    )
    .unwrap()
    .remove(0);
    let request: serde_json::Value =
        serde_json::from_slice(&fs::read(outcome.root.join(".account-request.json")).unwrap())
            .unwrap();
    assert_eq!(request["users"], serde_json::json!(["alice", "bob"]));
    assert_eq!(request["groups"], serde_json::json!(["wheel"]));
    let parameters: serde_json::Value =
        serde_json::from_slice(&fs::read(outcome.root.join(".account-parameters.json")).unwrap())
            .unwrap();
    assert_eq!(
        parameters["RSTAR_ACCOUNTINHERITANCE_USERS"],
        serde_json::json!(["alice", "bob", "alice"])
    );
    assert_eq!(
        parameters["RSTAR_ACCOUNTINHERITANCE_CUSTOM"],
        serde_json::json!(["plugin-choice"])
    );
}

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
            inheritance_root: Some(selected.clone()),
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
