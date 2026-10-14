//! Verify installation output follows each version through success, failure, and cleanup.
//! 验证安装输出在成功、失败及清理时随各版本留档。
use super::*;
use std::path::PathBuf;

fn log_path(fixture: &Fixture, system: &str, version: &str) -> PathBuf {
    fixture
        .store
        .system_layout(system)
        .unwrap()
        .versions
        .join(version)
        .join("install.log")
}

fn log(fixture: &Fixture, system: &str, version: &str) -> String {
    fs::read_to_string(log_path(fixture, system, version)).unwrap()
}

/// Preserve each build's preparation and final progress without rewriting earlier logs.
/// 各次构建保存准备及最终进度，不改写先前日志。
#[test]
fn each_version_keeps_its_own_complete_installation_log() {
    let fixture = Fixture::new();
    fixture.system("desktop", &["editor"]);
    let backend = SimulatedBackend::default();
    let first = build_with_backend(&fixture.store, Some("desktop"), &backend)
        .unwrap()
        .remove(0);
    let first_path = log_path(&fixture, "desktop", &first.version);
    let first_bytes = fs::read(&first_path).unwrap();
    let first_log = String::from_utf8(first_bytes.clone()).unwrap();
    for text in [
        "Prepare configuration and check caches",
        "Configuration checks passed",
        "Build version v00000001",
        "No reusable base cache",
        "Clean temporary work roots",
        "Completed: Build version v00000001",
    ] {
        assert!(first_log.contains(text), "missing {text:?}: {first_log}");
    }
    let second = build_with_backend(&fixture.store, Some("desktop"), &backend)
        .unwrap()
        .remove(0);
    assert_eq!(fs::read(&first_path).unwrap(), first_bytes);
    assert!(!first_log.contains("Build version v00000002"));
    let second_log = log(&fixture, "desktop", &second.version);
    assert!(second_log.contains("Build version v00000002"));
    assert!(second_log.contains("Reuse base system cache v00000001"));
    assert!(!second_log.contains("No reusable base cache"));

    rstarsleep::controller::clean_with_backend(&fixture.store, "desktop", &first.version, &backend)
        .unwrap();
    assert!(!first_path.exists());
    assert!(log_path(&fixture, "desktop", &second.version).is_file());
}

/// Cache invalidation leaves the historical installation logs alongside published roots.
/// 缓存失效时，历史安装日志与已发布系统根一同保留。
#[test]
fn clearing_all_stage_caches_preserves_historical_installation_logs() {
    let fixture = Fixture::new();
    fixture.system("desktop", &["editor"]);
    let backend = SimulatedBackend::default();
    let mut history = Vec::new();
    for _ in 0..2 {
        let outcome = build_with_backend(&fixture.store, Some("desktop"), &backend)
            .unwrap()
            .remove(0);
        let bytes = fs::read(log_path(&fixture, "desktop", &outcome.version)).unwrap();
        history.push((outcome, bytes));
    }
    let config = fixture
        .store
        .system_layout("desktop")
        .unwrap()
        .config
        .join("build/config.conf");
    let changed = fs::read_to_string(&config).unwrap().replace(
        "Architecture=x86_64",
        "Architecture=x86_64\nMirror=https://example.invalid/$repo/os/$arch",
    );
    fs::write(config, changed).unwrap();
    let latest = build_with_backend(&fixture.store, Some("desktop"), &backend)
        .unwrap()
        .remove(0);
    for (outcome, bytes) in history {
        assert_eq!(
            fs::read(log_path(&fixture, "desktop", &outcome.version)).unwrap(),
            bytes
        );
        assert!(outcome.root.is_dir());
        for stage in ["base-system", "packages", "script"] {
            assert!(!fixture.cache(&outcome.version, stage).exists());
            assert!(
                !fixture
                    .cache(&outcome.version, &format!("{stage}.toml"))
                    .exists()
            );
        }
    }
    assert!(log(&fixture, "desktop", &latest.version).contains("Base configuration changed"));
}

/// A failed package installation still records its error and scratch cleanup.
/// 软件安装失败仍保存错误与临时工作根清理过程。
#[test]
fn failed_installation_keeps_diagnostics_and_cleanup_after_retry() {
    let fixture = Fixture::new();
    fixture.system("desktop", &["editor"]);
    let backend = SimulatedBackend::default();
    backend.fail_operation.borrow_mut().replace("install");
    let error = build_with_backend(&fixture.store, Some("desktop"), &backend).unwrap_err();
    assert!(error.to_string().contains("injected install failure"));
    let failed_path = log_path(&fixture, "desktop", "v00000001");
    let failed_bytes = fs::read(&failed_path).unwrap();
    let failed_log = String::from_utf8(failed_bytes.clone()).unwrap();
    for text in [
        "Failed: Software packages",
        "injected install failure",
        "Starting: Clean temporary work roots",
        "Completed: Clean temporary work roots",
        "Failed: Build version v00000001",
    ] {
        assert!(failed_log.contains(text), "missing {text:?}: {failed_log}");
    }
    fixture.cleaned("v00000001");
    let outcome = build_with_backend(&fixture.store, Some("desktop"), &backend)
        .unwrap()
        .remove(0);
    assert_eq!(outcome.version, "v00000002");
    assert_eq!(fs::read(failed_path).unwrap(), failed_bytes);
    assert!(!log(&fixture, "desktop", &outcome.version).contains("injected install failure"));
}

/// Cleanup failures and their error text also survive in the failed version's log.
/// 清理失败及其错误文本也保留在失败版本日志中。
#[test]
fn scratch_cleanup_failure_is_archived() {
    let fixture = Fixture::new();
    fixture.system("desktop", &["editor"]);
    let backend = SimulatedBackend::default();
    backend.fail_cleanup.set(true);
    assert!(build_with_backend(&fixture.store, Some("desktop"), &backend).is_err());
    let failed_log = log(&fixture, "desktop", "v00000001");
    assert!(failed_log.contains("Failed: Clean temporary work roots"));
    assert!(failed_log.contains("injected cleanup failure"));
    assert!(failed_log.contains("Failed: Build version v00000001"));
}

/// Keep errors raised after stage completion, including current-reference publication.
/// 保留阶段完成后发生的错误，包括当前版本引用发布失败。
#[test]
fn publication_failure_is_archived_and_marks_the_version_failed() {
    let fixture = Fixture::new();
    fixture.system("desktop", &["editor"]);
    let layout = fixture.store.system_layout("desktop").unwrap();
    fs::create_dir(layout.root.join("build-current.toml")).unwrap();
    let backend = SimulatedBackend::default();
    let error = build_with_backend(&fixture.store, Some("desktop"), &backend).unwrap_err();
    let content = log(&fixture, "desktop", "v00000001");
    assert!(content.contains("Publish current build version reference"));
    assert!(content.contains("Failed: Build version v00000001"));
    assert!(content.contains(&error.to_string()));
    assert_eq!(
        record(&layout.versions.join("v00000001/state.toml"))["state"].as_str(),
        Some("failed")
    );
}

/// Direct system progress never leaks into another system's installation archive.
/// 单系统进度不会混入其他系统安装存档。
#[test]
fn all_system_builds_keep_system_progress_isolated() {
    let fixture = Fixture::new();
    for system in ["desktop", "server"] {
        fixture.system(system, &["editor"]);
    }
    let backend = SimulatedBackend::default();
    build_with_backend(&fixture.store, None, &backend).unwrap();
    for (system, other) in [("desktop", "server"), ("server", "desktop")] {
        let content = log(&fixture, system, "v00000001");
        assert!(content.contains(&format!("[RStarSleep][{system}]")));
        assert!(!content.contains(&format!("[RStarSleep][{other}]")));
        assert!(content.contains("Completed: Build version v00000001"));
    }
}

/// Shared preparation is copied to its participating versions, including early collection.
/// 共享准备输出写入参与版本，包括版本分配前的软件收集。
#[test]
fn shared_download_progress_is_archived_only_for_participating_systems() {
    let fixture = Fixture::with_predownload(true);
    for system in ["desktop", "server", "worker"] {
        fixture.system(system, &["editor"]);
    }
    fs::write(
        fixture
            .store
            .system_layout("worker")
            .unwrap()
            .config
            .join("build/environment.conf"),
        "[Environment]\nhttp_proxy=http://worker.invalid:8080\n",
    )
    .unwrap();
    let backend = SimulatedBackend::default();
    build_with_backend(&fixture.store, None, &backend).unwrap();
    assert_eq!(backend.count("predownload"), 2);
    for (system, other) in [("desktop", "server"), ("server", "desktop")] {
        let content = log(&fixture, system, "v00000001");
        assert!(content.contains("Collect package groups, base packages and runtime dependencies"));
        assert!(content.contains("systems desktop, server"));
        assert!(content.contains("Check the predownload environment cache"));
        assert!(content.contains("Completed: Clean temporary resolver work roots"));
        assert!(!content.contains("systems worker"));
        assert!(!content.contains(&format!("[RStarSleep][{other}]")));
        assert!(!content.contains("[RStarSleep][worker]"));
    }
    let worker = log(&fixture, "worker", "v00000001");
    assert!(worker.contains("systems worker"));
    assert!(!worker.contains("systems desktop, server"));
    assert!(!worker.contains("[RStarSleep][desktop]"));
    assert!(!worker.contains("[RStarSleep][server]"));
}

/// Failed shared downloads leave the same diagnostics in every affected version.
/// 共享下载失败时，每个受影响版本都保留诊断输出。
#[test]
fn failed_shared_download_retains_each_versions_installation_log() {
    let fixture = Fixture::with_predownload(true);
    for system in ["desktop", "server"] {
        fixture.system(system, &["editor"]);
    }
    let backend = SimulatedBackend::default();
    backend.fail_operation.borrow_mut().replace("predownload");
    assert!(build_with_backend(&fixture.store, None, &backend).is_err());
    assert_eq!(backend.count("predownload"), 1);
    for system in ["desktop", "server"] {
        let content = log(&fixture, system, "v00000001");
        for text in [
            "systems desktop, server",
            "injected predownload failure",
            "Completed: Clean temporary resolver work roots",
            "Failed: Predownload batch",
        ] {
            assert!(content.contains(text), "missing {text:?}: {content}");
        }
        assert!(!content.contains("Starting: Build version"));
        assert_eq!(
            record(
                &fixture
                    .store
                    .system_layout(system)
                    .unwrap()
                    .versions
                    .join("v00000001/state.toml")
            )["state"]
                .as_str(),
            Some("failed")
        );
    }
}

/// New-protocol downloads performed before version allocation remain in both archives.
/// 新协议在版本分配前完成的共享下载仍保存在双方存档中。
#[test]
fn unified_preflight_download_output_is_kept_after_version_allocation() {
    let fixture = Fixture::with_unified_packages();
    for system in ["desktop", "server"] {
        fixture.system(system, &["editor"]);
    }
    let backend = SimulatedBackend::default();
    build_with_backend(&fixture.store, None, &backend).unwrap();
    assert_eq!(backend.count("predownload"), 1);
    for (system, other) in [("desktop", "server"), ("server", "desktop")] {
        let content = log(&fixture, system, "v00000001");
        assert!(content.contains("Check the predownload environment cache"));
        assert!(content.contains("Create an independent predownload environment"));
        assert!(content.contains("Completed: Clean temporary resolver work roots"));
        assert!(content.contains("Completed: Build version v00000001"));
        assert!(!content.contains(&format!("[RStarSleep][{other}]")));
    }
}
