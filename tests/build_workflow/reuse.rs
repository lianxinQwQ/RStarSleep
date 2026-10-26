//! Verify direct reuse skips whole stages without extending execution lifetimes.
//! 验证直接复用跳过整个阶段且不延长执行有效期。
use super::*;
use rstarsleep::controller::BuildOutcome;
use std::{os::unix::fs::PermissionsExt, path::Path};

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

/// Enable a ten-minute global execution lifetime in an isolated manager.
/// 在隔离管理器中启用十分钟全局执行有效期。
/// # Arguments
/// - `fixture`: Isolated manager owning global policy. 拥有全局策略的隔离管理器。
fn enable(fixture: &Fixture) {
    fs::write(
        fixture.store.global_config_path(),
        "schema_version=1\ncache_reuse_minutes=10\n",
    )
    .unwrap();
}

/// Build one fixed system and return its published artifact.
/// 构建固定系统并返回发布产物。
/// # Arguments
/// - `fixture`: Isolated manager. 隔离管理器。
/// - `backend`: Recording simulated storage and operations. 记录存储及操作的模拟后端。
fn build(fixture: &Fixture, backend: &SimulatedBackend) -> BuildOutcome {
    build_with_backend(&fixture.store, Some("desktop"), backend)
        .unwrap()
        .remove(0)
}

/// Simulate an older receipt without changing its protected stage root.
/// 模拟旧执行凭据，不修改受保护阶段根。
/// # Arguments
/// - `fixture`: Isolated manager. 隔离管理器。
/// - `version`: Owning version. 所属版本。
/// - `stage`: Completed stage name. 已完成阶段名。
/// - `edit`: Mutation of the serialized test record. 序列化测试记录的修改。
fn edit_record(fixture: &Fixture, version: &str, stage: &str, edit: impl FnOnce(&mut toml::Value)) {
    let path = fixture.cache(version, &format!("{stage}.toml"));
    let mut value = record(&path);
    edit(&mut value);
    fs::write(path, toml::to_string(&value).unwrap()).unwrap();
}

/// Install a deterministic script whose executions are observable.
/// 安装可观察执行结果的确定性脚本。
/// # Arguments
/// - `fixture`: Isolated manager owning editable scripts. 拥有可编辑脚本的隔离管理器。
fn script(fixture: &Fixture) -> std::path::PathBuf {
    let path = fixture
        .store
        .system_layout("desktop")
        .unwrap()
        .config
        .join("script/test.sh");
    fs::write(&path, "printf original > \"$RSTAR_ROOT/script-result\"\n").unwrap();
    path
}

/// Check whole-stage skips for legacy and unified maintenance backends.
/// 验证旧版及统一维护后端完整跳过阶段。
/// # Arguments
/// No parameters. 无参数。
#[test]
fn fresh_caches_skip_all_three_stages_and_keep_time_and_inventory() {
    for fixture in [
        Fixture::new(),
        Fixture::with_unified_packages(),
        Fixture::with_unified_packages_without_predownload(),
    ] {
        fixture.system("desktop", &["editor"]);
        enable(&fixture);
        script(&fixture);
        let backend = SimulatedBackend::default();
        let first = build(&fixture, &backend);
        fs::write(first.root.join("runtime-change"), "must not leak").unwrap();
        for _ in 0..2 {
            backend.events.borrow_mut().clear();
            let hit = build(&fixture, &backend);
            for operation in [
                "bootstrap",
                "base_install",
                "update",
                "install",
                "maintain_packages",
                "post_maintenance",
                "script",
                "query_package_files",
            ] {
                // Independent preparation can maintain its own environment; target stages must not run.
                assert!(
                    !backend
                        .events
                        .borrow()
                        .iter()
                        .any(|event| event.operation == operation && event.system_id == "desktop"),
                    "executed {operation}"
                );
            }
            for stage in ["base-system", "packages", "script"] {
                let original = record(&fixture.cache(&first.version, &format!("{stage}.toml")));
                let reused = record(&fixture.cache(&hit.version, &format!("{stage}.toml")));
                assert_eq!(original["reuse"], reused["reuse"]);
                assert_eq!(
                    reused["origin_version"].as_str(),
                    Some(first.version.as_str())
                );
                backend
                    .check_subvolume(&fixture.cache(&hit.version, stage), true)
                    .unwrap();
            }
            assert_eq!(
                fs::read_to_string(hit.root.join("script-result")).unwrap(),
                "original"
            );
            assert!(!hit.root.join("runtime-change").exists());
            assert!(
                hit.root
                    .parent()
                    .unwrap()
                    .join("application-files.tsv")
                    .is_file()
            );
            fixture.cleaned(&hit.version);
        }
    }
}

/// Continue through expired or input-mismatched script candidates to a fresh matching history.
/// 脚本候选过期或输入不匹配时，继续倒序查找未过期且匹配的历史缓存。
#[test]
fn script_reuse_searches_past_expired_and_mismatched_candidates() {
    for expired in [false, true] {
        let fixture = Fixture::new();
        fixture.system("desktop", &["editor"]);
        enable(&fixture);
        script(&fixture);
        let backend = SimulatedBackend::default();
        let first = build(&fixture, &backend);
        let second = build(&fixture, &backend);
        edit_record(&fixture, &second.version, "script", |r| {
            if expired {
                r["reuse"]["executed_at_unix"] = 1.into();
            } else {
                r["reuse"]["inputs_sha256"] = "0".repeat(64).into();
            }
        });
        backend.events.borrow_mut().clear();
        let outcome = build(&fixture, &backend);
        assert_eq!(backend.count("script"), 0);
        let original = record(&fixture.cache(&first.version, "script.toml"));
        let reused = record(&fixture.cache(&outcome.version, "script.toml"));
        assert_eq!(
            reused["source_version"].as_str(),
            Some(first.version.as_str())
        );
        assert_eq!(reused["reuse"], original["reuse"]);
        assert_eq!(
            fs::read_to_string(outcome.root.join("script-result")).unwrap(),
            "original"
        );
        fixture.cleaned(&outcome.version);
    }
}

/// Reject expired or future executions and downstream outputs from an older base.
/// 拒绝过期或未来执行记录及来自旧基础的下游产物。
/// # Arguments
/// No parameters. 无参数。
#[test]
fn expiration_and_upstream_execution_prevent_downstream_hits() {
    let fixture = Fixture::new();
    fixture.system("desktop", &["editor"]);
    enable(&fixture);
    script(&fixture);
    let backend = SimulatedBackend::default();
    let first = build(&fixture, &backend);
    edit_record(&fixture, &first.version, "base-system", |r| {
        r["reuse"]["executed_at_unix"] = 1.into()
    });
    backend.events.borrow_mut().clear();
    let second = build(&fixture, &backend);
    assert_eq!(backend.count("update"), 2);
    assert_eq!(backend.count("script"), 1);
    for stage in ["base-system", "packages", "script"] {
        assert_eq!(
            record(&fixture.cache(&second.version, &format!("{stage}.toml")))["origin_version"]
                .as_str(),
            Some(second.version.as_str())
        );
    }
    // A future timestamp cannot turn a cache into an indefinitely fresh donor.
    edit_record(&fixture, &second.version, "packages", |r| {
        r["reuse"]["executed_at_unix"] = i64::MAX.into()
    });
    backend.events.borrow_mut().clear();
    build(&fixture, &backend);
    assert_eq!(backend.count("update"), 1);
    assert_eq!(backend.count("script"), 1);
}

/// Maintain legacy receipts and honor an explicit per-system disable.
/// 维护旧执行记录并遵守系统显式禁用配置。
/// # Arguments
/// No parameters. 无参数。
#[test]
fn old_receipts_and_system_zero_override_keep_normal_maintenance() {
    let fixture = Fixture::new();
    fixture.system("desktop", &[]);
    enable(&fixture);
    let backend = SimulatedBackend::default();
    let first = build(&fixture, &backend);
    for stage in ["base-system", "packages", "script"] {
        edit_record(&fixture, &first.version, stage, |r| {
            r.as_table_mut().unwrap().remove("reuse");
        });
    }
    backend.events.borrow_mut().clear();
    build(&fixture, &backend);
    assert_eq!(backend.count("update"), 2);
    let config = fixture.store.system_layout("desktop").unwrap().config;
    fs::write(
        config.join("build/reuse.conf"),
        "[Config]\nCacheReuseMinutes=0\n",
    )
    .unwrap();
    backend.events.borrow_mut().clear();
    build(&fixture, &backend);
    assert_eq!(backend.count("update"), 2);
}

/// Invalidate only stages affected by script or package changes.
/// 只使受脚本或软件目标变化影响的阶段失效。
/// # Arguments
/// No parameters. 无参数。
#[test]
fn script_resources_only_invalidate_script_but_package_targets_invalidate_software() {
    let fixture = Fixture::new();
    fixture.system("desktop", &["editor"]);
    enable(&fixture);
    let path = script(&fixture);
    let backend = SimulatedBackend::default();
    build(&fixture, &backend);
    fs::write(path, "printf changed > \"$RSTAR_ROOT/script-result\"\n").unwrap();
    backend.events.borrow_mut().clear();
    let changed = build(&fixture, &backend);
    assert_eq!(backend.count("update"), 0);
    assert_eq!(backend.count("script"), 1);
    assert_eq!(
        fs::read_to_string(changed.root.join("script-result")).unwrap(),
        "changed"
    );
    fixture.packages("desktop", &["new"]);
    backend.events.borrow_mut().clear();
    let updated = build(&fixture, &backend);
    assert_eq!(backend.count("script"), 1);
    assert!(SimulatedBackend::inventory(&updated.root).contains_key("new"));
    assert!(!SimulatedBackend::inventory(&updated.root).contains_key("editor"));
    // The package change need not execute the unchanged base step.
    assert!(
        !backend
            .events
            .borrow()
            .iter()
            .any(|e| e.root.ends_with("base-root"))
    );
}

/// Detect inherited bytes and permissions while keeping software stages reusable.
/// 检测继承内容及权限变化，同时保留软件阶段复用。
/// # Arguments
/// No parameters. 无参数。
#[test]
fn inherited_content_and_permissions_invalidate_script_only() {
    let fixture = Fixture::new();
    fixture.system("desktop", &["editor"]);
    enable(&fixture);
    script(&fixture);
    let source = fixture.store.root().parent().unwrap().join("live");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("value"), "first").unwrap();
    let file = fixture.store.root().parent().unwrap().join("live-file");
    fs::write(&file, "file").unwrap();
    let config = fixture.store.system_layout("desktop").unwrap().config;
    fs::write(
        config.join("build/inherit.conf"),
        format!(
            "[Inheritance]\nCopyOnWriteBuild={}/\nCopyOnWriteBuild={}\n",
            source.display(),
            file.display()
        ),
    )
    .unwrap();
    let backend = SimulatedBackend::default();
    build(&fixture, &backend);
    backend.events.borrow_mut().clear();
    let hit = build(&fixture, &backend);
    assert_eq!(backend.count("script"), 0);
    assert!(
        !fixture
            .store
            .root()
            .join("work/desktop")
            .join(&hit.version)
            .join("cow-inputs")
            .exists()
    );
    fs::write(source.join("value"), "other").unwrap();
    backend.events.borrow_mut().clear();
    let updated = build(&fixture, &backend);
    assert_eq!(backend.count("update"), 0);
    assert_eq!(backend.count("script"), 1);
    assert_eq!(
        fs::read_to_string(
            updated
                .root
                .join(source.strip_prefix(Path::new("/")).unwrap())
                .join("value")
        )
        .unwrap(),
        "other"
    );
    fs::set_permissions(&file, fs::Permissions::from_mode(0o700)).unwrap();
    backend.events.borrow_mut().clear();
    build(&fixture, &backend);
    assert_eq!(backend.count("script"), 1);
}

/// Keep actual inherited bytes consistent with the previously hashed frozen input.
/// 保证实际继承内容与已散列冻结输入一致。
/// # Arguments
/// No parameters. 无参数。
#[test]
fn execution_uses_frozen_inheritance_even_if_live_source_changes_after_hashing() {
    let fixture = Fixture::new();
    fixture.system("desktop", &[]);
    enable(&fixture);
    let source = fixture.store.root().parent().unwrap().join("changing-file");
    fs::write(&source, "captured").unwrap();
    let config = fixture.store.system_layout("desktop").unwrap().config;
    fs::write(
        config.join("build/inherit.conf"),
        format!("[Inheritance]\nCopyOnWriteBuild={}\n", source.display()),
    )
    .unwrap();
    let backend = SimulatedBackend::default();
    backend
        .mutate_on_script_restore
        .replace(Some((source.clone(), "changed".into())));
    let first = build(&fixture, &backend);
    let relative = source.strip_prefix("/").unwrap();
    assert_eq!(
        fs::read_to_string(first.root.join(relative)).unwrap(),
        "captured"
    );
    assert_eq!(fs::read_to_string(&source).unwrap(), "changed");
    backend.events.borrow_mut().clear();
    let second = build(&fixture, &backend);
    assert_eq!(backend.count("update"), 0);
    assert_eq!(backend.count("post_maintenance"), 1);
    assert_eq!(
        fs::read_to_string(second.root.join(relative)).unwrap(),
        "changed"
    );
}

/// Reject direct reuse after plugin resources or execution environment change.
/// 插件资源或执行环境变化后拒绝直接复用。
/// # Arguments
/// No parameters. 无参数。
#[test]
fn plugin_bytes_and_resolved_environment_prevent_stale_hits() {
    let fixture = Fixture::new();
    fixture.system("desktop", &["editor"]);
    enable(&fixture);
    let backend = SimulatedBackend::default();
    build(&fixture, &backend);
    let config = fixture.store.system_layout("desktop").unwrap().config;
    fs::write(
        config.join("build/environment.conf"),
        "[Environment]\nFOO=one\n",
    )
    .unwrap();
    backend.events.borrow_mut().clear();
    let environment = build(&fixture, &backend);
    assert_eq!(backend.count("update"), 2);
    assert_eq!(backend.count("post_maintenance"), 1);
    // Alter an arbitrary plugin resource while keeping its compatibility format unchanged.
    let path = fixture.store.plugins_path().join("arch.tar");
    let directory = tempfile::tempdir().unwrap();
    tar::Archive::new(fs::File::open(&path).unwrap())
        .unpack(directory.path())
        .unwrap();
    fs::write(directory.path().join("extra-resource"), "new bytes").unwrap();
    let mut archive = tar::Builder::new(fs::File::create(&path).unwrap());
    archive.append_dir_all(".", directory.path()).unwrap();
    archive.finish().unwrap();
    backend.events.borrow_mut().clear();
    let plugin = build(&fixture, &backend);
    assert_eq!(backend.count("bootstrap"), 0);
    assert_eq!(backend.count("update"), 2);
    assert_eq!(backend.count("post_maintenance"), 1);
    assert_ne!(
        record(&fixture.cache(&environment.version, "base-system.toml"))["plugin_sha256"],
        record(&fixture.cache(&plugin.version, "base-system.toml"))["plugin_sha256"]
    );
}

/// Recover failed scripts using earlier successfully completed stage receipts.
/// 通过前序成功阶段执行凭据恢复失败脚本。
/// # Arguments
/// No parameters. 无参数。
#[test]
fn failure_during_scripts_preserves_reusable_completed_stages() {
    let fixture = Fixture::new();
    fixture.system("desktop", &["editor"]);
    enable(&fixture);
    let path = script(&fixture);
    let backend = SimulatedBackend::default();
    let first = build(&fixture, &backend);
    fs::write(&path, "exit 1\n").unwrap();
    assert!(build_with_backend(&fixture.store, Some("desktop"), &backend).is_err());
    assert!(!fixture.cache("v00000002", "script.toml").exists());
    for stage in ["base-system", "packages"] {
        assert_eq!(
            record(&fixture.cache(&first.version, &format!("{stage}.toml")))["reuse"],
            record(&fixture.cache("v00000002", &format!("{stage}.toml")))["reuse"]
        );
    }
    fs::write(&path, "printf recovered > \"$RSTAR_ROOT/script-result\"\n").unwrap();
    backend.events.borrow_mut().clear();
    let recovered = build(&fixture, &backend);
    assert_eq!(backend.count("update"), 0);
    assert_eq!(backend.count("script"), 1);
    assert_eq!(
        fs::read_to_string(recovered.root.join("script-result")).unwrap(),
        "recovered"
    );
}

/// Frozen nested subvolumes remain reusable with either outer-directory inheritance policy.
/// 两种外层目录继承策略下，冻结的内部子卷均保持可复用。
/// # Arguments
/// No parameters. 无参数。
#[test]
fn frozen_nested_subvolumes_are_reused_and_cleaned_with_both_directory_modes() {
    for directory_subvolume in ["yes", "no"] {
        let fixture = Fixture::new();
        fixture.system("desktop", &[]);
        enable(&fixture);
        let backend = SimulatedBackend::default();
        let source = fixture.store.root().parent().unwrap().join("nested-source");
        backend.create_subvolume(&source).unwrap();
        backend.create_subvolume(&source.join("child")).unwrap();
        fs::write(source.join("child/value"), "nested bytes").unwrap();
        let config = fixture.store.system_layout("desktop").unwrap().config;
        fs::write(
            config.join("build/inherit.conf"),
            format!(
                "[Inheritance]\nDirectorySubvolume={directory_subvolume}\nCopyOnWriteBuild={}/\n",
                source.display()
            ),
        )
        .unwrap();
        build(&fixture, &backend);
        backend.events.borrow_mut().clear();
        let hit = build(&fixture, &backend);
        assert_eq!(backend.count("post_maintenance"), 0);
        let relative = source.strip_prefix("/").unwrap().join("child");
        assert_eq!(
            fs::read_to_string(hit.root.join(&relative).join("value")).unwrap(),
            "nested bytes"
        );
        backend
            .check_subvolume(&hit.root.join(&relative), false)
            .unwrap();
        backend
            .check_subvolume(&fixture.cache(&hit.version, "script").join(&relative), true)
            .unwrap();
        assert!(
            !fixture
                .store
                .root()
                .join("work/desktop")
                .join(&hit.version)
                .join("cow-inputs")
                .exists()
        );
    }
}

/// Failed source capture leaves earlier successful stages reusable and no partial frozen inputs.
/// 来源捕获失败仍保留可复用的前序成功阶段，且不残留冻结输入。
/// # Arguments
/// No parameters. 无参数。
#[test]
fn failed_source_capture_cleans_partial_inputs_and_keeps_completed_stages() {
    let fixture = Fixture::new();
    fixture.system("desktop", &[]);
    enable(&fixture);
    let source = fixture.store.root().parent().unwrap().join("source-file");
    fs::write(&source, "data").unwrap();
    let config = fixture.store.system_layout("desktop").unwrap().config;
    fs::write(
        config.join("build/inherit.conf"),
        format!("[Inheritance]\nCopyOnWriteBuild={}\n", source.display()),
    )
    .unwrap();
    let backend = SimulatedBackend::default();
    backend.fail_inheritance_copy.set(true);
    assert!(build_with_backend(&fixture.store, Some("desktop"), &backend).is_err());
    assert!(
        !fixture
            .store
            .root()
            .join("work/desktop/v00000001/cow-inputs")
            .exists()
    );
    assert!(fixture.cache("v00000001", "packages.toml").exists());
    assert!(!fixture.cache("v00000001", "script.toml").exists());
    backend.events.borrow_mut().clear();
    let recovered = build(&fixture, &backend);
    assert_eq!(backend.count("update"), 0);
    assert_eq!(
        fs::read_to_string(recovered.root.join(source.strip_prefix("/").unwrap())).unwrap(),
        "data"
    );
}

#[test]
fn changing_nocow_policy_invalidates_script_cache_and_reuses_packages() {
    let fixture = Fixture::new();
    fixture.system("desktop", &["editor"]);
    enable(&fixture);
    script(&fixture);
    let config = fixture
        .store
        .system_layout("desktop")
        .unwrap()
        .config
        .join("build/inherit.conf");
    fs::write(&config, "[Inheritance]\nNocowHandling=error\n").unwrap();
    let backend = SimulatedBackend::default();
    build(&fixture, &backend);
    backend.events.borrow_mut().clear();
    build(&fixture, &backend);
    assert_eq!(backend.count("script"), 0);
    fs::write(&config, "[Inheritance]\nNocowHandling=copy\n").unwrap();
    backend.events.borrow_mut().clear();
    build(&fixture, &backend);
    assert_eq!(backend.count("script"), 1);
    assert_eq!(backend.count("post_maintenance"), 1);
    assert_eq!(backend.count("update"), 0);
    assert_eq!(backend.count("install"), 0);
}
