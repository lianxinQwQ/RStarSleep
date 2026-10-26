//! Exercise current-artifact startup using the real records and a simulated CoW backend.
//! 使用真实记录与模拟 CoW 后端验证当前产物启动。
#[allow(dead_code, unused_imports)]
mod support;
use rstarsleep::{
    controller::{self, BuildBackend},
    subsystem,
};
use std::{fs, path::Path};
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

/// Enable the fixture system without permitting arbitrary guest callers.
/// 启用夹具系统，不允许任意容器调用者。
/// # Arguments
/// - `fixture`: Existing isolated manager. 已有隔离管理器。
fn enable(fixture: &Fixture) {
    fs::write(
        fixture.store.global_config_path(),
        "schema_version=1\nsubsystems=['desktop']\n",
    )
    .unwrap();
}

/// Publish only successful artifacts and never substitute history for broken references.
/// 只发布成功产物，绝不以历史替代损坏引用。
/// # Arguments
/// No parameters. 无参数。
#[test]
fn current_artifact_is_atomic_and_failure_preserves_it() {
    let f = Fixture::new();
    f.system("desktop", &[]);
    enable(&f);
    let b = SimulatedBackend::default();
    assert!(controller::read_current_build(&f.store, "desktop").is_err());
    let first = build_with_backend(&f.store, Some("desktop"), &b)
        .unwrap()
        .remove(0);
    *b.fail_operation.borrow_mut() = Some("update");
    assert!(build_with_backend(&f.store, Some("desktop"), &b).is_err());
    assert_eq!(
        controller::read_current_build(&f.store, "desktop")
            .unwrap()
            .version,
        first.version
    );
    fs::write(
        f.store
            .system_layout("desktop")
            .unwrap()
            .root
            .join("build-current.toml"),
        "schema_version=1\nsystem_id='other'\nversion='v1'\n",
    )
    .unwrap();
    assert!(
        subsystem::prepare_with_backend(&f.store, "desktop", &b, Some(Path::new("/"))).is_err()
    );
}

/// Finish a pending artifact at startup once, then replace modified runtime roots automatically.
/// 启动时只完成一次待办产物，之后自动替换已修改运行根。
/// # Arguments
/// No parameters. 无参数。
#[test]
fn deferred_startup_is_once_per_artifact_and_switch_has_no_diff_gate() {
    let f = Fixture::new();
    f.system("desktop", &[]);
    enable(&f);
    let b = SimulatedBackend::default();
    let source = f.store.root().parent().unwrap().join("source");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("value"), "build-time").unwrap();
    let layout = f.store.system_layout("desktop").unwrap();
    fs::write(
        layout.config.join("build/inherit.conf"),
        format!("[Inheritance]\nCopyOnWriteBoot={}/\n", source.display()),
    )
    .unwrap();
    let first = build_with_backend(&f.store, Some("desktop"), &b)
        .unwrap()
        .remove(0);
    assert!(first.root.ends_with("prepared-root"));
    fs::write(source.join("value"), "at-startup").unwrap();
    let runtime =
        subsystem::prepare_with_backend(&f.store, "desktop", &b, Some(Path::new("/"))).unwrap();
    assert_eq!(
        fs::read_to_string(
            runtime
                .root
                .join(source.strip_prefix("/").unwrap())
                .join("value")
        )
        .unwrap(),
        "at-startup"
    );
    b.check_subvolume(&runtime.root, false).unwrap();
    fs::write(runtime.root.join("unsaved"), "runtime change").unwrap();
    fs::write(source.join("value"), "later").unwrap();
    let repeat =
        subsystem::prepare_with_backend(&f.store, "desktop", &b, Some(Path::new("/"))).unwrap();
    assert_eq!(runtime.root, repeat.root);
    assert!(repeat.root.join("unsaved").exists());
    assert_eq!(
        fs::read_to_string(
            repeat
                .root
                .join(source.strip_prefix("/").unwrap())
                .join("value")
        )
        .unwrap(),
        "at-startup"
    );
    let second = build_with_backend(&f.store, Some("desktop"), &b)
        .unwrap()
        .remove(0);
    let new =
        subsystem::prepare_with_backend(&f.store, "desktop", &b, Some(Path::new("/"))).unwrap();
    assert_eq!(new.version, second.version);
    assert_ne!(new.root, repeat.root);
    assert!(!new.root.join("unsaved").exists());
    assert_eq!(
        fs::read_to_string(
            new.root
                .join(source.strip_prefix("/").unwrap())
                .join("value")
        )
        .unwrap(),
        "later"
    );
    assert!(controller::clean_with_backend(&f.store, "desktop", &first.version, &b).is_ok());
    assert!(controller::clean_with_backend(&f.store, "desktop", &second.version, &b).is_err());
}

/// Reject dual roles and conflicting mounts before any artifact mutation.
/// 任何产物修改前拒绝双角色及冲突挂载。
/// # Arguments
/// No parameters. 无参数。
#[test]
fn role_validation_precedes_preparation() {
    let f = Fixture::new();
    f.system("desktop", &[]);
    let b = SimulatedBackend::default();
    assert!(
        subsystem::prepare_with_backend(&f.store, "desktop", &b, Some(Path::new("/"))).is_err()
    );
    for config in [
        "schema_version=1\nsubsystems=['desktop','desktop']",
        "schema_version=1\nmain_system='desktop'\nsubsystems=['desktop']",
    ] {
        fs::write(f.store.global_config_path(), config).unwrap();
        assert!(f.store.runtime_config().is_err());
    }
}

/// Index failure retains the successful artifact, including its immutable candidates.
/// 索引失败保留成功产物及其不可变候选列表。
/// # Arguments
/// No parameters. 无参数。
#[test]
fn package_index_is_part_of_successful_publication() {
    let f = Fixture::new();
    f.system("desktop", &["editor"]);
    enable(&f);
    let b = SimulatedBackend::default();
    let first = build_with_backend(&f.store, Some("desktop"), &b)
        .unwrap()
        .remove(0);
    let layout = f.store.system_layout("desktop").unwrap();
    let index = fs::read_to_string(
        layout
            .versions
            .join(&first.version)
            .join("application-files.tsv"),
    )
    .unwrap();
    assert!(index.contains("editor\t/usr/bin/editor\n"));
    *b.fail_operation.borrow_mut() = Some("query_package_files");
    assert!(build_with_backend(&f.store, Some("desktop"), &b).is_err());
    assert_eq!(
        controller::read_current_build(&f.store, "desktop")
            .unwrap()
            .version,
        first.version
    );
    assert_eq!(
        fs::read_to_string(
            layout
                .versions
                .join(&first.version)
                .join("application-files.tsv")
        )
        .unwrap(),
        index
    );
}

/// Rebuilding runtime storage never rewrites a shared persistent fstab.
/// 重建运行存储绝不改写共享持久 fstab。
/// # Arguments
/// No parameters. 无参数。
#[test]
fn persistent_fstab_is_overlaid_without_modifying_source() {
    let f = Fixture::new();
    f.system("desktop", &[]);
    enable(&f);
    let b = SimulatedBackend::default();
    let layout = f.store.system_layout("desktop").unwrap();
    let source = f.store.root().parent().unwrap().join("etc-source");
    fs::create_dir(&source).unwrap();
    let original=b"# BEGIN RStarSleep inherited mounts\nUUID=host /home btrfs defaults 0 0\n# END RStarSleep inherited mounts\ntmpfs /cache tmpfs size=1M 0 0\n";
    fs::write(source.join("fstab"), original).unwrap();
    fs::write(
        layout.config.join("subsystem/config.toml"),
        format!(
            "schema_version=1\n[[mounts]]\nsource={:?}\ntarget='/etc'\n",
            source.to_str().unwrap()
        ),
    )
    .unwrap();
    build_with_backend(&f.store, Some("desktop"), &b).unwrap();
    let runtime =
        subsystem::prepare_with_backend(&f.store, "desktop", &b, Some(Path::new("/"))).unwrap();
    assert!(
        runtime
            .boot_args
            .iter()
            .any(|v| v.to_string_lossy().ends_with(":/etc/fstab"))
    );
    assert_eq!(fs::read(source.join("fstab")).unwrap(), original);
    assert_eq!(
        fs::read_to_string(layout.root.join("runtime/fstab")).unwrap(),
        "tmpfs /cache tmpfs size=1M 0 0\n"
    );
}

/// Obsolete generations are retired only on explicit readiness acknowledgement.
/// 只在显式确认就绪后退役过时运行代。
/// # Arguments
/// No parameters. 无参数。
#[test]
fn obsolete_runtime_roots_wait_for_successful_boot() {
    let f = Fixture::new();
    f.system("desktop", &[]);
    enable(&f);
    let b = SimulatedBackend::default();
    build_with_backend(&f.store, Some("desktop"), &b).unwrap();
    let first =
        subsystem::prepare_with_backend(&f.store, "desktop", &b, Some(Path::new("/"))).unwrap();
    build_with_backend(&f.store, Some("desktop"), &b).unwrap();
    let second =
        subsystem::prepare_with_backend(&f.store, "desktop", &b, Some(Path::new("/"))).unwrap();
    assert!(first.root.exists());
    subsystem::prune_with_backend(&f.store, "desktop", &b).unwrap();
    assert!(!first.root.exists());
    assert!(second.root.exists());
}

/// A backend's success cannot hide an intact obsolete runtime root.
/// 后端返回成功不能掩盖仍然完整存在的旧运行根。
#[test]
fn runtime_pruning_verifies_root_deletion_before_removing_generation() {
    let f = Fixture::new();
    f.system("desktop", &[]);
    enable(&f);
    let b = SimulatedBackend::default();
    build_with_backend(&f.store, Some("desktop"), &b).unwrap();
    let first =
        subsystem::prepare_with_backend(&f.store, "desktop", &b, Some(Path::new("/"))).unwrap();
    fs::write(first.root.join("retained-data"), "keep runtime contents").unwrap();
    build_with_backend(&f.store, Some("desktop"), &b).unwrap();
    let second =
        subsystem::prepare_with_backend(&f.store, "desktop", &b, Some(Path::new("/"))).unwrap();
    *b.retain_deleted_root.borrow_mut() = Some(first.root.clone());

    let error = subsystem::prune_with_backend(&f.store, "desktop", &b).unwrap_err();
    let message = error.to_string();
    assert!(message.contains("旧运行根删除后仍然存在"));
    assert!(message.contains("obsolete runtime root still exists after deletion"));
    assert!(message.contains(first.root.to_str().unwrap()));
    assert_eq!(
        fs::read_to_string(first.root.join("retained-data")).unwrap(),
        "keep runtime contents"
    );
    assert!(second.root.exists());

    *b.retain_deleted_root.borrow_mut() = None;
    subsystem::prune_with_backend(&f.store, "desktop", &b).unwrap();
    assert!(!first.root.parent().unwrap().exists());
    assert!(second.root.exists());
}

/// Recovery retains its transaction when a failed deletion leaves an uncommitted root intact.
/// 删除失败且未提交运行根仍然完整存在时，恢复过程应保留事务。
#[test]
fn runtime_recovery_preserves_transaction_until_root_deletion_is_verified() {
    let f = Fixture::new();
    f.system("desktop", &[]);
    enable(&f);
    let b = SimulatedBackend::default();
    build_with_backend(&f.store, Some("desktop"), &b).unwrap();
    let current =
        subsystem::prepare_with_backend(&f.store, "desktop", &b, Some(Path::new("/"))).unwrap();
    let generations = current.root.parent().unwrap().parent().unwrap();
    let generation = generations.join("g-interrupted");
    let root = generation.join("root");
    fs::create_dir(&generation).unwrap();
    b.snapshot_recursive(&current.root, &root, false).unwrap();
    fs::write(root.join("retained-data"), "keep uncommitted contents").unwrap();
    let transaction = generations.parent().unwrap().join("transaction.toml");
    let record = format!(
        "schema_version=1\nsystem_id='desktop'\nversion={:?}\ngeneration='g-interrupted'\n",
        current.version
    );
    fs::write(&transaction, &record).unwrap();
    *b.retain_deleted_root.borrow_mut() = Some(root.clone());

    let error =
        subsystem::prepare_with_backend(&f.store, "desktop", &b, Some(Path::new("/"))).unwrap_err();
    assert!(error.to_string().contains(root.to_str().unwrap()));
    assert_eq!(fs::read_to_string(&transaction).unwrap(), record);
    assert_eq!(
        fs::read_to_string(root.join("retained-data")).unwrap(),
        "keep uncommitted contents"
    );
    assert!(current.root.exists());

    *b.retain_deleted_root.borrow_mut() = None;
    let recovered =
        subsystem::prepare_with_backend(&f.store, "desktop", &b, Some(Path::new("/"))).unwrap();
    assert_eq!(recovered.root, current.root);
    assert!(!generation.exists());
    assert!(!transaction.exists());
}

/// Leftover siblings identify the exact failed generation without deleting arbitrary entries.
/// 同级残留应指出失败的具体运行代，且不任意删除条目。
#[test]
fn runtime_pruning_reports_nonempty_generation_path() {
    let f = Fixture::new();
    f.system("desktop", &[]);
    enable(&f);
    let b = SimulatedBackend::default();
    build_with_backend(&f.store, Some("desktop"), &b).unwrap();
    let first =
        subsystem::prepare_with_backend(&f.store, "desktop", &b, Some(Path::new("/"))).unwrap();
    let generation = first.root.parent().unwrap();
    let leftover = generation.join("leftover");
    fs::write(&leftover, "retain unknown entry").unwrap();
    build_with_backend(&f.store, Some("desktop"), &b).unwrap();
    let second =
        subsystem::prepare_with_backend(&f.store, "desktop", &b, Some(Path::new("/"))).unwrap();

    let error = subsystem::prune_with_backend(&f.store, "desktop", &b).unwrap_err();
    assert!(matches!(
        error,
        subsystem::Error::Io { path, source }
            if path == generation && source.kind() == std::io::ErrorKind::DirectoryNotEmpty
    ));
    assert!(!first.root.exists());
    assert_eq!(
        fs::read_to_string(leftover).unwrap(),
        "retain unknown entry"
    );
    assert!(second.root.exists());
}

/// A dangling root must be rejected explicitly instead of being mistaken for an absent root.
/// 悬空运行根链接必须明确报错，不能误判为运行根不存在。
#[test]
fn runtime_pruning_rejects_dangling_root_without_removing_generation() {
    let f = Fixture::new();
    f.system("desktop", &[]);
    enable(&f);
    let b = SimulatedBackend::default();
    build_with_backend(&f.store, Some("desktop"), &b).unwrap();
    let current =
        subsystem::prepare_with_backend(&f.store, "desktop", &b, Some(Path::new("/"))).unwrap();
    let generation = current
        .root
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("g-dangling");
    fs::create_dir(&generation).unwrap();
    let root = generation.join("root");
    std::os::unix::fs::symlink(generation.join("missing"), &root).unwrap();

    let error = subsystem::prune_with_backend(&f.store, "desktop", &b).unwrap_err();
    assert!(error.to_string().contains(root.to_str().unwrap()));
    assert!(matches!(error, subsystem::Error::Invalid(_)));
    assert!(
        fs::symlink_metadata(&root)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert!(current.root.exists());
}

/// Another system's deferred inheritance pins an obsolete runtime source until completion.
/// 其他系统的延后继承在完成前固定过时运行来源。
/// # Arguments
/// No parameters. 无参数。
#[test]
fn runtime_pruning_preserves_cross_system_pending_sources() {
    let f = Fixture::new();
    f.system("desktop", &[]);
    f.system("consumer", &[]);
    enable(&f);
    let b = SimulatedBackend::default();
    build_with_backend(&f.store, Some("desktop"), &b).unwrap();
    let first =
        subsystem::prepare_with_backend(&f.store, "desktop", &b, Some(Path::new("/"))).unwrap();
    let source = first.root.join("data");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("value"), "retained source").unwrap();
    let consumer = f.store.system_layout("consumer").unwrap();
    fs::write(
        consumer.config.join("build/inherit.conf"),
        format!("[Inheritance]\nCopyOnWriteBoot={}/\n", source.display()),
    )
    .unwrap();
    let pending = build_with_backend(&f.store, Some("consumer"), &b)
        .unwrap()
        .remove(0);
    build_with_backend(&f.store, Some("desktop"), &b).unwrap();
    let second =
        subsystem::prepare_with_backend(&f.store, "desktop", &b, Some(Path::new("/"))).unwrap();

    subsystem::prune_with_backend(&f.store, "desktop", &b).unwrap();
    assert!(first.root.exists());
    assert_eq!(
        fs::read_to_string(source.join("value")).unwrap(),
        "retained source"
    );
    let manifest = consumer
        .versions
        .join(&pending.version)
        .join("inheritance-pending.toml");
    controller::finalize_inheritance_with_backend(&manifest, Path::new("/"), &b).unwrap();
    assert_eq!(
        fs::read_to_string(
            consumer
                .versions
                .join(&pending.version)
                .join("root")
                .join(source.strip_prefix("/").unwrap())
                .join("value")
        )
        .unwrap(),
        "retained source"
    );
    subsystem::prune_with_backend(&f.store, "desktop", &b).unwrap();
    assert!(!first.root.exists());
    assert!(second.root.exists());
}

/// A root still reported as running survives cleanup even after the storage current changes.
/// 存储当前引用切换后，仍报告运行中的根也不会被清理。
/// # Arguments
/// No parameters. 无参数。
#[test]
fn runtime_pruning_preserves_running_roots() {
    let f = Fixture::new();
    f.system("desktop", &[]);
    enable(&f);
    let b = SimulatedBackend::default();
    build_with_backend(&f.store, Some("desktop"), &b).unwrap();
    let first =
        subsystem::prepare_with_backend(&f.store, "desktop", &b, Some(Path::new("/"))).unwrap();
    build_with_backend(&f.store, Some("desktop"), &b).unwrap();
    let second =
        subsystem::prepare_with_backend(&f.store, "desktop", &b, Some(Path::new("/"))).unwrap();
    *b.running_root.borrow_mut() = Some(first.root.clone());
    subsystem::prune_with_backend(&f.store, "desktop", &b).unwrap();
    assert!(first.root.exists());
    *b.running_root.borrow_mut() = None;
    subsystem::prune_with_backend(&f.store, "desktop", &b).unwrap();
    assert!(!first.root.exists());
    assert!(second.root.exists());
}
