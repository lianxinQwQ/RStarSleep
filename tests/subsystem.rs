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

/// Finalize once, retain edits in the version root, and switch to the next version on restart.
/// 仅收尾一次，在版本根保留修改，并在重启时切换下一版本。
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
    let version_root = layout.versions.join(&first.version).join("root");
    assert_eq!(runtime.root, version_root);
    fs::write(runtime.root.join("unsaved"), "runtime change").unwrap();
    assert_eq!(
        fs::read_to_string(version_root.join("unsaved")).unwrap(),
        "runtime change"
    );
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
    assert_eq!(new.root, layout.versions.join(&second.version).join("root"));
    assert_eq!(
        fs::read_to_string(version_root.join("unsaved")).unwrap(),
        "runtime change"
    );
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

/// Preparing version roots never rewrites a shared persistent fstab.
/// 准备版本根绝不改写共享持久 fstab。
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

/// Launch-policy changes preserve edits and nested subvolumes in the same version root.
/// 启动策略变化保留同一版本根内的修改及内部子卷。
#[test]
fn direct_version_root_survives_policy_changes_without_snapshots() {
    let f = Fixture::new();
    f.system("desktop", &[]);
    enable(&f);
    let b = SimulatedBackend::default();
    let build = build_with_backend(&f.store, Some("desktop"), &b)
        .unwrap()
        .remove(0);
    let layout = f.store.system_layout("desktop").unwrap();
    let snapshots = b.snapshots.borrow().len();
    let first =
        subsystem::prepare_with_backend(&f.store, "desktop", &b, Some(Path::new("/"))).unwrap();
    assert_eq!(first.root, build.root);
    fs::write(first.root.join("edited"), "version edit").unwrap();
    b.create_subvolume(&first.root.join("nested")).unwrap();
    fs::write(first.root.join("nested/value"), "nested edit").unwrap();
    let bind_source = f.store.root().parent().unwrap().join("bind-source");
    fs::create_dir(&bind_source).unwrap();
    fs::write(
        layout.config.join("subsystem/config.toml"),
        format!(
            "schema_version=1\ntmp_size_bytes=4096\n[[mounts]]\nsource={:?}\ntarget='/mnt/data'\n",
            bind_source.to_str().unwrap()
        ),
    )
    .unwrap();
    let second =
        subsystem::prepare_with_backend(&f.store, "desktop", &b, Some(Path::new("/"))).unwrap();
    assert_eq!(second.root, first.root);
    assert_eq!(
        fs::read_to_string(second.root.join("edited")).unwrap(),
        "version edit"
    );
    assert_eq!(
        fs::read_to_string(second.root.join("nested/value")).unwrap(),
        "nested edit"
    );
    assert!(
        second
            .boot_args
            .iter()
            .any(|v| v == "--tmpfs=/tmp:mode=1777,size=4096")
    );
    assert!(
        second
            .boot_args
            .iter()
            .any(|v| v.to_string_lossy().ends_with(":/mnt/data"))
    );
    assert_eq!(b.snapshots.borrow().len(), snapshots);
    assert!(!layout.root.join("runtime/generations").exists());
    assert!(!layout.root.join("runtime/transaction.toml").exists());
    let record: toml::Value =
        toml::from_str(&fs::read_to_string(layout.root.join("runtime/current.toml")).unwrap())
            .unwrap();
    assert_eq!(record["schema_version"].as_integer(), Some(2));
    assert!(record.get("generation").is_none());
    assert_eq!(
        subsystem::load_prepared(&f.store, "desktop").unwrap().root,
        build.root
    );
}

/// A new build leaves the recorded running version protected until the next preparation.
/// 新构建保留已登记运行版本的清理保护，直到下一次启动准备。
#[test]
fn version_switch_preserves_old_edits_until_explicit_clean() {
    let f = Fixture::new();
    f.system("desktop", &[]);
    enable(&f);
    let b = SimulatedBackend::default();
    build_with_backend(&f.store, Some("desktop"), &b).unwrap();
    let first =
        subsystem::prepare_with_backend(&f.store, "desktop", &b, Some(Path::new("/"))).unwrap();
    fs::write(first.root.join("edited"), "old version edit").unwrap();
    build_with_backend(&f.store, Some("desktop"), &b).unwrap();
    assert_eq!(
        subsystem::load_prepared(&f.store, "desktop").unwrap().root,
        first.root
    );
    assert!(controller::clean_with_backend(&f.store, "desktop", &first.version, &b).is_err());
    let second =
        subsystem::prepare_with_backend(&f.store, "desktop", &b, Some(Path::new("/"))).unwrap();
    assert_ne!(second.root, first.root);
    assert_eq!(
        fs::read_to_string(first.root.join("edited")).unwrap(),
        "old version edit"
    );
    assert!(!second.root.join("edited").exists());
    *b.running_root.borrow_mut() = Some(first.root.clone());
    assert!(controller::clean_with_backend(&f.store, "desktop", &first.version, &b).is_err());
    *b.running_root.borrow_mut() = None;
    controller::clean_with_backend(&f.store, "desktop", &first.version, &b).unwrap();
    assert!(!first.root.exists());
    assert!(second.root.exists());
    assert!(controller::clean_with_backend(&f.store, "desktop", &second.version, &b).is_err());
}

/// Legacy machines can be drained; their modified roots survive switching to direct version use.
/// 旧机器可被排空；切换为直接使用版本根时保留其已修改的旧根。
#[test]
fn legacy_runtime_is_readable_and_retained_when_switching_to_version_root() {
    let f = Fixture::new();
    f.system("desktop", &[]);
    enable(&f);
    let b = SimulatedBackend::default();
    let build = build_with_backend(&f.store, Some("desktop"), &b)
        .unwrap()
        .remove(0);
    let layout = f.store.system_layout("desktop").unwrap();
    let legacy_root = layout.root.join("runtime/generations/g-legacy/root");
    fs::create_dir_all(legacy_root.parent().unwrap()).unwrap();
    b.snapshot_recursive(&build.root, &legacy_root, false)
        .unwrap();
    fs::write(legacy_root.join("edited"), "legacy edit").unwrap();
    let record = subsystem::RuntimeRecord {
        schema_version: 1,
        system_id: "desktop".into(),
        version: build.version.clone(),
        generation: "g-legacy".into(),
        config_sha256: "a".repeat(64),
        machine: subsystem::machine_name(&f.store, "desktop"),
    };
    fs::write(
        layout.root.join("runtime/current.toml"),
        toml::to_string(&record).unwrap(),
    )
    .unwrap();
    assert_eq!(
        subsystem::load_prepared(&f.store, "desktop").unwrap().root,
        legacy_root
    );
    let runtime =
        subsystem::prepare_with_backend(&f.store, "desktop", &b, Some(Path::new("/"))).unwrap();
    assert_eq!(runtime.root, build.root);
    assert_eq!(
        subsystem::load_prepared(&f.store, "desktop").unwrap().root,
        build.root
    );
    assert_eq!(
        fs::read_to_string(legacy_root.join("edited")).unwrap(),
        "legacy edit"
    );
    assert!(!build.root.join("edited").exists());
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
    let generations = f
        .store
        .system_layout("desktop")
        .unwrap()
        .root
        .join("runtime/generations");
    fs::create_dir_all(&generations).unwrap();
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

/// Invalid runtime references fail before deferred inheritance can mutate the version.
/// 无效运行引用在延后继承修改版本前失败。
#[test]
fn corrupt_runtime_record_is_rejected_before_finalization() {
    let f = Fixture::new();
    f.system("desktop", &[]);
    enable(&f);
    let b = SimulatedBackend::default();
    let source = f.store.root().parent().unwrap().join("source");
    fs::create_dir(&source).unwrap();
    let layout = f.store.system_layout("desktop").unwrap();
    fs::write(
        layout.config.join("build/inherit.conf"),
        format!("[Inheritance]\nCopyOnWriteBoot={}/\n", source.display()),
    )
    .unwrap();
    let build = build_with_backend(&f.store, Some("desktop"), &b)
        .unwrap()
        .remove(0);
    fs::create_dir_all(layout.root.join("runtime")).unwrap();
    let record = subsystem::RuntimeRecord {
        schema_version: 2,
        system_id: "other".into(),
        version: build.version.clone(),
        generation: String::new(),
        config_sha256: String::new(),
        machine: subsystem::machine_name(&f.store, "desktop"),
    };
    fs::write(
        layout.root.join("runtime/current.toml"),
        toml::to_string(&record).unwrap(),
    )
    .unwrap();
    let snapshots = b.snapshots.borrow().len();
    let error =
        subsystem::prepare_with_backend(&f.store, "desktop", &b, Some(Path::new("/"))).unwrap_err();
    assert!(error.to_string().contains("运行记录身份或格式不匹配"));
    assert!(
        error
            .to_string()
            .contains("runtime record identity or format mismatch")
    );
    assert_eq!(b.snapshots.borrow().len(), snapshots);
    assert!(!layout.versions.join(&build.version).join("root").exists());
    assert!(build.root.exists());
}

/// Missing mount sources fail before finalization and can be corrected without rebuilding.
/// 挂载来源缺失在版本收尾前失败，修正后无需重新构建即可重试。
#[test]
fn mount_source_preflight_precedes_finalization() {
    let f = Fixture::new();
    f.system("desktop", &[]);
    enable(&f);
    let b = SimulatedBackend::default();
    let source = f.store.root().parent().unwrap().join("source");
    let mount_source = f.store.root().parent().unwrap().join("mount-source");
    fs::create_dir(&source).unwrap();
    let layout = f.store.system_layout("desktop").unwrap();
    fs::write(
        layout.config.join("build/inherit.conf"),
        format!("[Inheritance]\nCopyOnWriteBoot={}/\n", source.display()),
    )
    .unwrap();
    fs::write(
        layout.config.join("subsystem/config.toml"),
        format!(
            "schema_version=1\n[[mounts]]\nsource={:?}\ntarget='/mnt/data'\n",
            mount_source.to_str().unwrap()
        ),
    )
    .unwrap();
    let build = build_with_backend(&f.store, Some("desktop"), &b)
        .unwrap()
        .remove(0);
    let root = layout.versions.join(&build.version).join("root");
    let snapshots = b.snapshots.borrow().len();
    let error =
        subsystem::prepare_with_backend(&f.store, "desktop", &b, Some(Path::new("/"))).unwrap_err();
    assert!(
        matches!(error, subsystem::Error::Io { source, .. } if source.kind() == std::io::ErrorKind::NotFound)
    );
    assert!(!root.exists());
    assert_eq!(b.snapshots.borrow().len(), snapshots);
    assert!(!layout.root.join("runtime/current.toml").exists());
    fs::create_dir(&mount_source).unwrap();
    let runtime =
        subsystem::prepare_with_backend(&f.store, "desktop", &b, Some(Path::new("/"))).unwrap();
    assert_eq!(runtime.root, root);
}

/// Pending inheritance protects old version roots until its source has been consumed.
/// 待办继承在读取完成前保护旧版本来源根。
#[test]
fn version_clean_preserves_cross_system_pending_sources() {
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
    assert!(controller::clean_with_backend(&f.store, "desktop", &first.version, &b).is_err());
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
    controller::clean_with_backend(&f.store, "desktop", &first.version, &b).unwrap();
    assert!(!first.root.exists());
    assert!(second.root.exists());
}
