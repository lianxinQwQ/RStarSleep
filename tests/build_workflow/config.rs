//! Verify INI freezing and cache provenance with real configuration readers.
//! 使用真实配置读取接口验证 INI 留档及缓存来源。

use super::*;

/// Freeze environment assignments from merged INI files before execution.
/// 在执行前冻结合并 INI 文件中的环境赋值。
/// # Arguments
/// No parameters. 无参数。
#[test]
fn environment_group_uses_merged_ini_and_frozen_execution_inputs() {
    let fixture = Fixture::new();
    fixture.system("desktop", &["editor"]);
    let layout = fixture.store.system_layout("desktop").unwrap();
    let path = layout.config.join("build/environment.conf");
    let original = "# Same INI parser, including comments and continuation lines.\n[Environment]\nhttp_proxy=http://127.0.0.1:10808 # comment\nHTTP_PROXY=%PATH\nRAW=a=b ; $(touch unexpected)\n    continued\nEMPTY=\nPERCENT=%%literal\n";
    fs::write(&path, original).unwrap();
    fs::write(
        layout.config.join("build/extra.conf"),
        "[Environment]\nno_proxy=localhost,127.0.0.1\n",
    )
    .unwrap();
    let backend = SimulatedBackend::default();
    backend.mutate_environment.borrow_mut().replace((
        path.clone(),
        "[Environment]\nhttp_proxy=http://changed:8080\n".into(),
    ));
    let first = build_with_backend(&fixture.store, Some("desktop"), &backend)
        .unwrap()
        .remove(0);
    assert_eq!(
        fs::read_to_string(
            layout
                .versions
                .join(&first.version)
                .join("config/build/environment.conf")
        )
        .unwrap(),
        original
    );
    let events = backend.events.borrow();
    assert!(!events.is_empty());
    for event in events.iter() {
        assert_eq!(
            event.environment["http_proxy"],
            Some("http://127.0.0.1:10808".into())
        );
        assert_eq!(event.environment["HTTP_PROXY"], std::env::var_os("PATH"));
        assert_eq!(
            event.environment["no_proxy"],
            Some("localhost,127.0.0.1".into())
        );
        assert_eq!(
            event.environment["RAW"],
            Some("a=b ; $(touch unexpected)\ncontinued".into())
        );
        assert_eq!(event.environment["EMPTY"], Some("".into()));
        assert_eq!(event.environment["PERCENT"], Some("%literal".into()));
        assert!(
            !event
                .parameters
                .keys()
                .any(|name| name.starts_with("RSTAR_ENVIRONMENT_"))
        );
    }
    drop(events);
    backend.events.borrow_mut().clear();
    build_with_backend(&fixture.store, Some("desktop"), &backend).unwrap();
    assert_eq!(backend.count("bootstrap"), 0);
    assert!(
        backend
            .events
            .borrow()
            .iter()
            .all(|event| event.environment["http_proxy"] == Some("http://changed:8080".into()))
    );
}

/// Reject invalid environment settings before any plugin operation.
/// 在任何插件操作前拒绝无效环境设置。
/// # Arguments
/// No parameters. 无参数。
#[test]
fn invalid_environment_group_blocks_plugin_execution() {
    for settings in [
        "bad-name=value",
        "A=%bad-name",
        "A=x\nA=y",
        "RSTAR_ROOT=/",
        "RSTAT_ROOT=/",
        "A=bad\0value",
    ] {
        let fixture = Fixture::new();
        fixture.system("desktop", &[]);
        let layout = fixture.store.system_layout("desktop").unwrap();
        fs::write(
            layout.config.join("build/environment.conf"),
            format!("[Environment]\n{settings}\n"),
        )
        .unwrap();
        let backend = SimulatedBackend::default();
        assert!(
            build_with_backend(&fixture.store, Some("desktop"), &backend).is_err(),
            "{settings}"
        );
        assert!(backend.events.borrow().is_empty(), "{settings}");
    }
}

/// Preserve legacy fallback while accepting the same environment group in TOML.
/// 保留旧格式回退，同时接受 TOML 中的同一环境组。
/// # Arguments
/// No parameters. 无参数。
#[test]
fn legacy_toml_accepts_environment_group_and_default_ini_template() {
    let fixture = Fixture::new();
    fixture.system("desktop", &[]);
    let layout = fixture.store.system_layout("desktop").unwrap();
    fs::remove_file(layout.config.join("build/config.conf")).unwrap();
    fs::write(layout.config.join("build/config.toml"), "schema_version=1\nsystem_type='arch'\narchitecture='x86_64'\npackages=[]\n[Environment]\nhttp_proxy='http://localhost:10808'\nHTTP_PROXY='%PATH'\n").unwrap();
    fixture.store.initialize_system("desktop").unwrap();
    let backend = SimulatedBackend::default();
    build_with_backend(&fixture.store, Some("desktop"), &backend).unwrap();
    assert!(
        backend
            .events
            .borrow()
            .iter()
            .all(
                |event| event.environment["http_proxy"] == Some("http://localhost:10808".into())
                    && event.environment["HTTP_PROXY"] == std::env::var_os("PATH")
            )
    );
}

/// Freeze multiline INI verbatim and reuse caches on package changes while retaining unknown options.
/// 原样留档多行 INI，包变化复用缓存，未知选项仍参与兼容性判断。
/// # Arguments
/// No parameters.
/// 无参数。
#[test]
fn multiline_ini_freezes_and_preserves_cache_compatibility() {
    let fixture = Fixture::new();
    fixture.system("desktop", &[]);
    let metadata = fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("presets/plugins/arch/plugin.toml"),
    )
    .unwrap();
    super::base_system::replace_resource(
        &fixture,
        "plugin.toml",
        metadata
            .replace(
                "\"RSTAR_CONTENT_PACKAGES\",",
                "\"RSTAR_CONTENT_PACKAGES\", \"RSTAR_CUSTOM_ENABLED\",",
            )
            .replace(
                "base_system = [\"RSTAR_CONFIG\", \"RSTAR_DISTRIBUTION\"]",
                "base_system = [\"RSTAR_CONFIG\", \"RSTAR_DISTRIBUTION\", \"RSTAR_CUSTOM\"]",
            )
            .as_bytes(),
    );
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
        ["discarded", "old", "kept", "kept"]
    );
    let updated = text.replace("old # Initial applications", "new # Changed applications");
    fs::write(&path, &updated).unwrap();
    backend.events.borrow_mut().clear();
    let second = build_with_backend(&fixture.store, Some("desktop"), &backend)
        .unwrap()
        .remove(0);
    assert_eq!(backend.count("system_init"), 0);
    assert_eq!(backend.count("update"), 2);
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
    assert_eq!(backend.count("update"), 2);
    assert_eq!(
        record(&fixture.cache(&second.version, "base-system.toml"))["source_version"].as_str(),
        Some(first.version.as_str())
    );
    assert!(SimulatedBackend::inventory(&second.root).contains_key("new"));
}

/// A software-only dependency clears software donors without reinstalling the base.
/// 仅软件依赖变化清理软件来源，基础无需重新安装。
#[test]
fn software_dependencies_invalidate_only_software_and_downstream_caches() {
    let fixture = Fixture::new();
    fixture.system("desktop", &["editor"]);
    let metadata = fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("presets/plugins/arch/plugin.toml"),
    )
    .unwrap();
    super::base_system::replace_resource(
        &fixture,
        "plugin.toml",
        metadata
            .replace(
                "\"RSTAR_CONTENT_PACKAGES\",",
                "\"RSTAR_CONTENT_PACKAGES\", \"RSTAR_CUSTOM_MODE\", \"RSTAR_CUSTOM_IGNORED\",",
            )
            .replace("software = []", "software = [\"RSTAR_*CUSTOM_MODE\"]")
            .as_bytes(),
    );
    let layout = fixture.store.system_layout("desktop").unwrap();
    let path = layout.config.join("build/config.conf");
    let text = format!(
        "{}[Custom]\nMode=stable\nIgnored=one\n",
        fs::read_to_string(&path).unwrap()
    );
    fs::write(&path, &text).unwrap();
    let backend = SimulatedBackend::default();
    let first = build_with_backend(&fixture.store, Some("desktop"), &backend)
        .unwrap()
        .remove(0);
    fs::write(&path, text.replace("Ignored=one", "Ignored=two")).unwrap();
    backend.events.borrow_mut().clear();
    let second = build_with_backend(&fixture.store, Some("desktop"), &backend)
        .unwrap()
        .remove(0);
    assert_eq!(backend.count("bootstrap"), 0);
    assert_eq!(backend.count("update"), 2);
    assert_eq!(
        record(&fixture.cache(&second.version, "packages.toml"))["source_version"].as_str(),
        Some(first.version.as_str())
    );
    fs::write(&path, text.replace("Mode=stable", "Mode=next")).unwrap();
    backend.events.borrow_mut().clear();
    backend.fail_operation.borrow_mut().replace("install");
    assert!(build_with_backend(&fixture.store, Some("desktop"), &backend).is_err());
    assert_eq!(backend.count("bootstrap"), 0);
    assert_eq!(backend.count("update"), 1);
    for version in [&first.version, &second.version] {
        assert!(fixture.cache(version, "base-system").exists());
        assert!(fixture.cache(version, "base-system.toml").exists());
        assert!(!fixture.cache(version, "packages").exists());
        assert!(!fixture.cache(version, "packages.toml").exists());
        assert!(layout.versions.join(version).join("root").exists());
    }
    let base_record = record(&fixture.cache("v00000003", "base-system.toml"));
    assert_eq!(
        base_record["source_version"].as_str(),
        Some(second.version.as_str())
    );
}

#[test]
fn every_single_system_operation_receives_all_frozen_parameters() {
    let fixture = Fixture::new();
    fixture.system("desktop", &["editor"]);
    let metadata = fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("presets/plugins/arch/plugin.toml"),
    )
    .unwrap();
    super::base_system::replace_resource(
        &fixture,
        "plugin.toml",
        metadata
            .replace(
                "\"RSTAR_CONTENT_PACKAGES\",",
                "\"RSTAR_CONTENT_PACKAGES\", \"RSTAR_CHANNEL\", \"RSTAR_CUSTOM_CHANNEL\",",
            )
            .as_bytes(),
    );
    let path = fixture
        .store
        .system_layout("desktop")
        .unwrap()
        .config
        .join("build/config.conf");
    fs::write(
        &path,
        format!(
            "Channel=bare\n{}[Custom]\nChannel=section\n",
            fs::read_to_string(&path).unwrap()
        ),
    )
    .unwrap();
    let backend = SimulatedBackend::default();
    build_with_backend(&fixture.store, Some("desktop"), &backend).unwrap();
    for event in backend.events.borrow().iter() {
        assert_eq!(event.parameters["RSTAR_CHANNEL"], "bare");
        assert_eq!(event.parameters["RSTAR_CUSTOM_CHANNEL"], "section");
        assert_eq!(event.parameters["RSTAR_CONTENT_PACKAGES"], "editor");
        assert_eq!(event.parameters["RSTAR_DISTRIBUTION_DISTRIBUTION"], "arch");
    }
}

/// Plugins can opt into detecting package changes that previously always kept caches compatible.
/// 插件可选择检测此前始终保持缓存兼容的软件包变化。
#[test]
fn plugins_can_select_package_targets_as_a_software_dependency() {
    let fixture = Fixture::new();
    fixture.system("desktop", &["old"]);
    let metadata = fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("presets/plugins/arch/plugin.toml"),
    )
    .unwrap();
    super::base_system::replace_resource(
        &fixture,
        "plugin.toml",
        metadata
            .replace("software = []", "software = [\"RSTAR_CONTENT_PACKAGES\"]")
            .as_bytes(),
    );
    let backend = SimulatedBackend::default();
    let first = build_with_backend(&fixture.store, Some("desktop"), &backend)
        .unwrap()
        .remove(0);
    fixture.packages("desktop", &["new"]);
    backend.events.borrow_mut().clear();
    let second = build_with_backend(&fixture.store, Some("desktop"), &backend)
        .unwrap()
        .remove(0);
    assert_eq!(backend.count("bootstrap"), 0);
    assert_eq!(backend.count("update"), 1);
    assert!(fixture.cache(&first.version, "base-system").exists());
    assert!(!fixture.cache(&first.version, "packages").exists());
    assert_eq!(
        record(&fixture.cache(&second.version, "base-system.toml"))["source_version"].as_str(),
        Some(first.version.as_str())
    );
    assert!(
        record(&fixture.cache(&second.version, "packages.toml"))
            .get("source_version")
            .is_none()
    );
    let inventory = SimulatedBackend::inventory(&second.root);
    assert!(inventory.contains_key("new"));
    assert!(!inventory.contains_key("old"));
}
