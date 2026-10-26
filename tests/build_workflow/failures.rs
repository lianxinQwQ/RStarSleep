//! Verify step failures, committed-cache recovery, and rejected build records.
//! 验证步骤失败、已提交缓存恢复和无效构建记录拒绝。
use super::*;

/// Restore test-only directory permissions before Fixture's recursive cleanup.
/// 在 Fixture 递归清理前恢复测试专用目录权限。
struct RestoreSnapshotPermissions(std::path::PathBuf);

impl Drop for RestoreSnapshotPermissions {
    fn drop(&mut self) {
        use std::{os::unix::fs::PermissionsExt, path::Path};

        fn restore(path: &Path) {
            if !fs::symlink_metadata(path).is_ok_and(|entry| entry.is_dir()) {
                return;
            }
            let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o755));
            if let Ok(entries) = fs::read_dir(path) {
                for entry in entries.flatten() {
                    restore(&entry.path());
                }
            }
        }

        restore(&self.0);
    }
}

/// Publish all stage snapshots while VFS rejects moving their roots across parents.
/// 在 VFS 拒绝跨父目录移动只读根时，仍能发布各阶段快照。
#[test]
fn readonly_stage_snapshots_publish_successfully() {
    use std::os::unix::fs::PermissionsExt;

    let fixture = Fixture::new();
    fixture.system("desktop", &["editor"]);
    let _permissions = RestoreSnapshotPermissions(fixture.store.root().into());
    let backend = SimulatedBackend::default();
    backend.protect_readonly_snapshots.set(true);

    let outcome = build_with_backend(&fixture.store, Some("desktop"), &backend)
        .unwrap()
        .remove(0);

    for stage in ["base-system", "packages", "script"] {
        let root = fixture.cache(&outcome.version, stage);
        backend.check_subvolume(&root, true).unwrap();
        assert!(
            fixture
                .cache(&outcome.version, &format!("{stage}.toml"))
                .is_file()
        );
        assert_eq!(fs::metadata(root).unwrap().permissions().mode() & 0o222, 0);
    }
    backend.check_subvolume(&outcome.root, false).unwrap();
    fs::write(outcome.root.join("runtime-only"), "writable root").unwrap();
    assert_eq!(
        fs::read_dir(fixture.cache(&outcome.version, ""))
            .unwrap()
            .count(),
        6
    );
    fixture.cleaned(&outcome.version);
}

/// A partially created stage snapshot is cleaned before a later build retries.
/// 阶段快照创建后失败会先清理暂存根，后续构建可重试。
#[test]
fn snapshot_creation_failure_leaves_no_stage_cache_or_temporary_subvolume() {
    let fixture = Fixture::new();
    fixture.system("desktop", &["editor"]);
    let _permissions = RestoreSnapshotPermissions(fixture.store.root().into());
    let backend = SimulatedBackend::default();
    backend.protect_readonly_snapshots.set(true);
    backend.fail_readonly_snapshot_after_create.set(true);

    let error = build_with_backend(&fixture.store, Some("desktop"), &backend).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("failure after snapshot creation")
    );
    assert!(!fixture.cache("v00000001", "base-system").exists());
    assert!(!fixture.cache("v00000001", "base-system.toml").exists());
    assert_eq!(
        fs::read_dir(fixture.cache("v00000001", ""))
            .unwrap()
            .count(),
        0
    );
    fixture.cleaned("v00000001");

    let outcome = build_with_backend(&fixture.store, Some("desktop"), &backend)
        .unwrap()
        .remove(0);
    backend
        .check_subvolume(&fixture.cache(&outcome.version, "base-system"), true)
        .unwrap();
    fixture.cleaned(&outcome.version);
}

/// Verify a failed software install preserves completed base cache for the next attempt.
/// 验证软件安装失败时保留完成基础缓存供下次尝试使用。
/// # Arguments
///
/// No parameters.
/// 无参数。
#[test]
fn failed_packages_keep_base_cache_and_next_attempt_reuses_it() {
    let fixture = Fixture::new();
    fixture.system("desktop", &["editor"]);
    let backend = SimulatedBackend::default();
    *backend.fail_operation.borrow_mut() = Some("install");
    assert!(build_with_backend(&fixture.store, Some("desktop"), &backend).is_err());
    assert!(fixture.cache("v00000001", "base-system.toml").is_file());
    assert!(!fixture.cache("v00000001", "packages.toml").exists());
    let layout = fixture.store.system_layout("desktop").unwrap();
    assert_eq!(
        record(&layout.versions.join("v00000001/state.toml"))["state"].as_str(),
        Some("failed")
    );
    fixture.cleaned("v00000001");
    backend.events.borrow_mut().clear();
    let outcome = build_with_backend(&fixture.store, Some("desktop"), &backend)
        .unwrap()
        .remove(0);
    assert_eq!(outcome.version, "v00000002");
    assert_eq!(backend.count("bootstrap"), 0);
    assert_eq!(backend.count("update"), 1);
    fixture.cleaned(&outcome.version);
}

/// Recover completed historical stages when the latest attempt failed before committing caches.
/// 最新尝试在提交缓存前失败时，恢复历史已完成阶段。
/// # Arguments
///
/// No parameters.
/// 无参数。
#[test]
fn failed_latest_attempt_reuses_older_completed_caches() {
    let fixture = Fixture::new();
    fixture.system("desktop", &["editor"]);
    let backend = SimulatedBackend::default();
    build_with_backend(&fixture.store, Some("desktop"), &backend).unwrap();
    *backend.fail_operation.borrow_mut() = Some("update");
    assert!(build_with_backend(&fixture.store, Some("desktop"), &backend).is_err());
    backend.events.borrow_mut().clear();
    let outcome = build_with_backend(&fixture.store, Some("desktop"), &backend)
        .unwrap()
        .remove(0);
    assert_eq!(outcome.version, "v00000003");
    assert_eq!(backend.count("bootstrap"), 0);
    assert_eq!(backend.count("system_init"), 0);
    assert_eq!(backend.count("update"), 2);
    for stage in ["base-system", "packages"] {
        assert_eq!(
            record(&fixture.cache(&outcome.version, &format!("{stage}.toml")))["source_version"]
                .as_str(),
            Some("v00000001")
        );
    }
}

/// Verify failed CoW restore aborts before plugins and leaves no copy fallback.
/// 验证 CoW 恢复失败在插件执行前中止且无复制回退。
/// # Arguments
///
/// No parameters.
/// 无参数。
#[test]
fn snapshot_restore_failure_does_not_run_plugins_or_copy_fallback() {
    let fixture = Fixture::new();
    fixture.system("desktop", &["editor"]);
    let backend = SimulatedBackend::default();
    build_with_backend(&fixture.store, Some("desktop"), &backend).unwrap();
    backend.events.borrow_mut().clear();
    backend.fail_restore.set(true);
    let error = build_with_backend(&fixture.store, Some("desktop"), &backend).unwrap_err();
    assert!(error.to_string().contains("CoW restore failure"));
    assert!(backend.events.borrow().is_empty());
    assert!(!fixture.cache("v00000002", "packages.toml").exists());
    fixture.cleaned("v00000002");
}

/// Verify mutable current configuration cannot change an in-progress frozen build.
/// 验证可变当前配置不能改变进行中的冻结构建。
/// # Arguments
///
/// No parameters.
/// 无参数。
#[test]
fn bootstrap_edits_to_current_config_do_not_change_frozen_targets() {
    let fixture = Fixture::new();
    fixture.system("desktop", &["editor"]);
    let backend = SimulatedBackend::default();
    *backend.mutate_editable.borrow_mut() = Some(
        fixture
            .store
            .system_layout("desktop")
            .unwrap()
            .config
            .join("build/config.conf"),
    );
    let outcome = build_with_backend(&fixture.store, Some("desktop"), &backend)
        .unwrap()
        .remove(0);
    assert_eq!(
        fixture.store.read_build_config("desktop").unwrap().packages,
        ["mutated"]
    );
    assert!(
        backend
            .events
            .borrow()
            .iter()
            .all(|event| event.packages == ["editor"])
    );
    let inventory = SimulatedBackend::inventory(&outcome.root);
    assert!(inventory.contains_key("editor"));
    assert!(!inventory.contains_key("mutated"));
}

/// Verify damaged cache completion metadata aborts instead of silently rebuilding.
/// 验证损坏缓存完成元数据时中止，而不静默重建。
/// # Arguments
///
/// No parameters.
/// 无参数。
#[test]
fn malformed_completion_record_is_an_error() {
    let fixture = Fixture::new();
    fixture.system("desktop", &[]);
    let backend = SimulatedBackend::default();
    build_with_backend(&fixture.store, Some("desktop"), &backend).unwrap();
    fs::write(fixture.cache("v00000001", "base-system.toml"), "broken = [").unwrap();
    backend.events.borrow_mut().clear();
    assert!(build_with_backend(&fixture.store, Some("desktop"), &backend).is_err());
    assert!(backend.events.borrow().is_empty());
}

/// Verify a FIFO completion sidecar is rejected without being opened or blocking.
/// 验证 FIFO 完成旁记录被拒绝，而不打开或阻塞。
/// # Arguments
///
/// No parameters.
/// 无参数。
#[cfg(target_os = "linux")]
#[test]
fn fifo_completion_record_is_rejected_without_opening_it() {
    let fixture = Fixture::new();
    fixture.system("desktop", &[]);
    let backend = SimulatedBackend::default();
    build_with_backend(&fixture.store, Some("desktop"), &backend).unwrap();
    let sidecar = fixture.cache("v00000001", "base-system.toml");
    fs::remove_file(&sidecar).unwrap();
    support::build::create_fifo(&sidecar);
    backend.events.borrow_mut().clear();
    let error = build_with_backend(&fixture.store, Some("desktop"), &backend).unwrap_err();
    assert!(error.to_string().contains("缓存完成记录必须是普通文件"));
    assert!(backend.events.borrow().is_empty());
}

/// Verify ordinary directory substitution cannot masquerade as a cache subvolume.
/// 验证普通目录替换不能伪装成缓存子卷。
/// # Arguments
///
/// No parameters.
/// 无参数。
#[test]
fn unmarked_cache_directory_is_rejected_before_plugin_execution() {
    let fixture = Fixture::new();
    fixture.system("desktop", &[]);
    let backend = SimulatedBackend::default();
    build_with_backend(&fixture.store, Some("desktop"), &backend).unwrap();
    fs::remove_file(fixture.cache("v00000001", "packages").join(MARKER)).unwrap();
    backend.events.borrow_mut().clear();
    assert!(build_with_backend(&fixture.store, Some("desktop"), &backend).is_err());
    assert!(backend.events.borrow().is_empty());
}

/// Reject malformed batch inputs before warnings, cache cleanup, or any system build.
/// 批量输入格式错误时，在警告、缓存清理及任何系统构建前退出。
/// # Arguments
///
/// No parameters.
/// 无参数。
#[test]
fn batch_malformed_config_stops_all_systems_before_warning_or_build() {
    use rstarsleep::controller::build_with_backend_and_warning;

    for (broken, good) in [("a-broken", "z-good"), ("z-broken", "a-good")] {
        let fixture = Fixture::with_predownload(true);
        fixture.system(broken, &[]);
        fixture.system(good, &["editor"]);
        let broken_layout = fixture.store.system_layout(broken).unwrap();
        let good_layout = fixture.store.system_layout(good).unwrap();
        let invalid = format!("{}missing-equals\n", "# padding / 填充\n".repeat(23));
        fs::write(broken_layout.config.join("build/config.conf"), invalid).unwrap();
        fs::write(
            good_layout.config.join("build/inherit.conf"),
            "[Inheritance]\nCopyOnWriteBuild=/missing-source\n",
        )
        .unwrap();
        let obsolete = fixture.store.predownload_path().join("arch/obsolete");
        fs::create_dir_all(&obsolete).unwrap();
        fs::write(obsolete.join("sentinel"), "keep").unwrap();
        for layout in [&broken_layout, &good_layout] {
            fs::write(layout.cache.join("sentinel"), "keep").unwrap();
        }

        let backend = SimulatedBackend::default();
        let error = build_with_backend_and_warning(&fixture.store, None, &backend, |_| {
            panic!("配置错误必须在警告回调前退出 / Configuration errors must exit before the warning callback")
        })
        .unwrap_err();
        assert!(
            matches!(error, Error::Batch(ref errors) if errors.len() == 1
                && errors[0].contains(broken)
                && errors[0].contains("config.conf:24")
                && errors[0].contains("设置必须包含 = / setting must contain =")),
            "{error}"
        );
        assert!(backend.events.borrow().is_empty());
        assert!(backend.snapshots.borrow().is_empty());
        assert_eq!(
            fs::read_to_string(obsolete.join("sentinel")).unwrap(),
            "keep"
        );
        for layout in [&broken_layout, &good_layout] {
            assert_eq!(fs::read_dir(&layout.versions).unwrap().count(), 0);
            assert!(!layout.root.join("version-counter").exists());
            assert!(!layout.root.join("build-current.toml").exists());
            assert_eq!(
                fs::read_to_string(layout.cache.join("sentinel")).unwrap(),
                "keep"
            );
        }
    }
}

/// Report every batch configuration failure before the shared warning countdown.
/// 在统一警告倒计时前报告本批全部配置错误。
#[test]
fn batch_semantic_config_errors_exit_before_warning_or_build() {
    use rstarsleep::controller::build_with_backend_and_warning;

    let fixture = Fixture::new();
    for (id, settings) in [
        (
            "a-good",
            "[Inheritance]\nCopyOnWriteBuild=/missing-source\n",
        ),
        ("b-environment", "[Environment]\nA=x\nA=y\n"),
        ("c-plugin", "[Content]\nUndeclaredSetting=value\n"),
    ] {
        fixture.system(id, &[]);
        let layout = fixture.store.system_layout(id).unwrap();
        fs::write(layout.config.join("build/extra.conf"), settings).unwrap();
    }
    let backend = SimulatedBackend::default();
    let error = build_with_backend_and_warning(&fixture.store, None, &backend, |_| {
        panic!("配置错误必须在警告回调前退出 / Configuration errors must exit before the warning callback")
    })
    .unwrap_err();
    assert!(
        matches!(error, Error::Batch(ref errors) if errors.len() == 2
            && errors[0].contains("b-environment")
            && errors[0].contains("must be assigned exactly once")
            && errors[1].contains("c-plugin")
            && errors[1].contains("undeclared settings")),
        "{error}"
    );
    assert!(backend.events.borrow().is_empty());
    assert!(backend.snapshots.borrow().is_empty());
    for id in ["a-good", "b-environment", "c-plugin"] {
        let layout = fixture.store.system_layout(id).unwrap();
        assert_eq!(fs::read_dir(layout.versions).unwrap().count(), 0);
        assert!(!layout.root.join("version-counter").exists());
    }
}

/// Verify lock contention allocates no version and executes no plugin.
/// 验证锁争用不分配版本且不执行插件。
/// # Arguments
///
/// No parameters.
/// 无参数。
#[test]
fn per_system_lock_conflict_prevents_build_start() {
    let fixture = Fixture::new();
    fixture.system("desktop", &[]);
    let layout = fixture.store.system_layout("desktop").unwrap();
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(layout.root.join("build.lock"))
        .unwrap();
    lock.try_lock().unwrap();
    let backend = SimulatedBackend::default();
    let error = build_with_backend(&fixture.store, Some("desktop"), &backend).unwrap_err();
    assert!(error.to_string().contains("构建锁"));
    assert!(backend.events.borrow().is_empty());
    assert_eq!(fs::read_dir(layout.versions).unwrap().count(), 0);
}

/// Verify edits to an archived cache source are detected before maintenance.
/// 验证归档缓存来源的修改在维护前被发现。
/// # Arguments
///
/// No parameters.
/// 无参数。
#[test]
fn modified_archived_config_is_not_reused() {
    let fixture = Fixture::new();
    fixture.system("desktop", &["editor"]);
    let backend = SimulatedBackend::default();
    build_with_backend(&fixture.store, Some("desktop"), &backend).unwrap();
    let archive = fixture
        .store
        .system_layout("desktop")
        .unwrap()
        .versions
        .join("v00000001/config/build/config.conf");
    fs::write(
        archive,
        "[Config]\nSchemaVersion=1\n[Distribution]\nDistribution=arch\nArchitecture=x86_64\n[Content]\nPackages=\n",
    )
    .unwrap();
    backend.events.borrow_mut().clear();
    let error = build_with_backend(&fixture.store, Some("desktop"), &backend).unwrap_err();
    assert!(error.to_string().contains("缓存来源配置已改变"));
    assert!(backend.events.borrow().is_empty());
}

/// Verify a failed base action stops the package step and leaves no completed base cache.
/// 验证基础动作失败阻止软件包步骤且不留下已完成基础缓存。
///
/// # Arguments
///
/// No parameters.
/// 无参数。
#[test]
fn base_step_failure_stops_later_steps() {
    let fixture = Fixture::new();
    fixture.system("desktop", &["editor"]);
    let backend = SimulatedBackend::default();
    *backend.fail_operation.borrow_mut() = Some("base_install");
    assert!(build_with_backend(&fixture.store, Some("desktop"), &backend).is_err());
    assert_eq!(backend.count("bootstrap"), 1);
    assert_eq!(backend.count("install"), 0);
    assert!(
        backend
            .events
            .borrow()
            .iter()
            .all(|event| event.root.ends_with("base-root"))
    );
    assert!(!fixture.cache("v00000001", "base-system.toml").exists());
    assert!(!fixture.cache("v00000001", "packages.toml").exists());
    fixture.cleaned("v00000001");
}

/// Verify uncached software restores the committed base output and retains it on restore failure.
/// 验证无缓存软件恢复已提交基础输出，并在恢复失败时保留该输出。
///
/// # Arguments
///
/// No parameters.
/// 无参数。
#[test]
fn uncached_package_step_requires_cow_restore_of_committed_previous_step() {
    let fixture = Fixture::new();
    fixture.system("desktop", &["editor"]);
    let backend = SimulatedBackend::default();
    *backend.fail_restore_to.borrow_mut() = Some("root");
    let error = build_with_backend(&fixture.store, Some("desktop"), &backend).unwrap_err();
    assert!(error.to_string().contains("CoW restore failure"));
    let base = fixture.cache("v00000001", "base-system");
    assert!(fixture.cache("v00000001", "base-system.toml").is_file());
    backend.check_subvolume(&base, true).unwrap();
    assert!(backend.snapshots.borrow().iter().any(|snapshot| {
        !snapshot.readonly
            && snapshot.source == base
            && snapshot
                .destination
                .ends_with("work/desktop/v00000001/root")
    }));
    assert_eq!(backend.count("install"), 0);
    assert!(!fixture.cache("v00000001", "packages.toml").exists());
    assert!(
        !fixture
            .store
            .system_layout("desktop")
            .unwrap()
            .versions
            .join("v00000001/root")
            .exists()
    );
    fixture.cleaned("v00000001");
}
