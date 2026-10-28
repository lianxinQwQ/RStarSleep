//! Exercise preservation through real build orchestration and first-boot recovery.
//! 通过实际构建编排及首次启动恢复验证例外保留。
use super::*;
use rstarsleep::controller::{
    BuildOptions, BuildOutcome, build_with_options_and_backend, finalize_inheritance_with_backend,
};
use std::{
    os::unix::fs::symlink,
    path::{Path, PathBuf},
};

fn setup(timing: &str, subvolume: bool, reuse: bool) -> (Fixture, SimulatedBackend, PathBuf) {
    let fixture = Fixture::new();
    fixture.system("desktop", &["kept"]);
    let parent = fixture.store.root().parent().unwrap();
    let source = parent.join("source");
    let built = parent.join("built");
    fs::create_dir_all(source.join("etc/machine-id/old-child")).unwrap();
    fs::write(
        source.join("etc/machine-id/old-child").join(MARKER),
        "writable",
    )
    .unwrap();
    fs::write(source.join("etc/machine-id/old-child/value"), "old").unwrap();
    fs::create_dir_all(source.join("etc/generated")).unwrap();
    fs::write(source.join("etc/generated/source-only"), "old").unwrap();
    fs::write(source.join("etc/unrelated"), "source").unwrap();
    fs::create_dir_all(built.join("etc/generated/empty")).unwrap();
    fs::create_dir_all(built.join("etc/generated/nested/deeper")).unwrap();
    for path in ["etc/generated/nested", "etc/generated/nested/deeper"] {
        fs::write(built.join(path).join(MARKER), "writable").unwrap();
    }
    fs::write(built.join("etc/generated/built"), "built").unwrap();
    fs::write(
        built.join("etc/generated/nested/deeper/value"),
        "nested-built",
    )
    .unwrap();
    symlink("/missing/target", built.join("etc/generated/link")).unwrap();
    if subvolume {
        fs::write(built.join("etc/generated").join(MARKER), "writable").unwrap();
    }
    let layout = fixture.store.system_layout("desktop").unwrap();
    fs::write(layout.config.join("build/inherit.conf"), format!("[Inheritance.CopyOnWrite{timing}.etc]\nPath=/etc/\nPreserve=machine-id\nPreserve=generated/\n")).unwrap();
    if reuse {
        fs::write(
            fixture.store.global_config_path(),
            "schema_version=1\ncache_reuse_minutes=10\n",
        )
        .unwrap();
    }
    let backend = SimulatedBackend::default();
    *backend.built_tree.borrow_mut() = Some(built);
    *backend.post_maintenance_probe.borrow_mut() = if timing == "Build" {
        Some("etc/machine-id".into())
    } else {
        None
    };
    (fixture, backend, source)
}

fn build(
    fixture: &Fixture,
    backend: &SimulatedBackend,
    source: &Path,
) -> rstarsleep::controller::Result<BuildOutcome> {
    Ok(build_with_options_and_backend(
        &fixture.store,
        Some("desktop"),
        &BuildOptions {
            inheritance_root: Some(source.into()),
        },
        backend,
    )?
    .remove(0))
}

fn finalize(
    outcome: &BuildOutcome,
    backend: &SimulatedBackend,
) -> rstarsleep::controller::Result<()> {
    finalize_inheritance_with_backend(
        &outcome
            .root
            .parent()
            .unwrap()
            .join("inheritance-pending.toml"),
        Path::new("/"),
        backend,
    )
}

fn assert_output(root: &Path, machine_id: &str, backend: &SimulatedBackend, subvolume: bool) {
    assert_eq!(
        fs::read_to_string(root.join("etc/machine-id")).unwrap(),
        machine_id
    );
    assert_eq!(
        fs::read_to_string(root.join("etc/unrelated")).unwrap(),
        "source"
    );
    assert_eq!(
        fs::read_to_string(root.join("etc/generated/built")).unwrap(),
        "built"
    );
    assert!(!root.join("etc/generated/source-only").exists());
    assert!(root.join("etc/generated/empty").is_dir());
    assert_eq!(
        fs::read_link(root.join("etc/generated/link")).unwrap(),
        Path::new("/missing/target")
    );
    assert_eq!(
        fs::read_to_string(root.join("etc/generated/nested/deeper/value")).unwrap(),
        "nested-built"
    );
    backend
        .check_subvolume(&root.join("etc/generated/nested"), false)
        .unwrap();
    backend
        .check_subvolume(&root.join("etc/generated/nested/deeper"), false)
        .unwrap();
    assert_eq!(root.join("etc/generated").join(MARKER).exists(), subvolume);
}

/// Both source modes preserve entire trees and replace inherited directory leaves with files.
/// 两种来源模式均保留完整目录树，并允许用文件替换继承后的目录末段。
#[test]
fn build_preserves_files_directories_and_nested_subvolumes_with_frozen_sources() {
    for (subvolume, reuse) in [(false, false), (true, false), (false, true), (true, true)] {
        let (fixture, backend, source) = setup("Build", subvolume, reuse);
        if !subvolume && !reuse {
            let config = fixture
                .store
                .system_layout("desktop")
                .unwrap()
                .config
                .join("build/inherit.conf");
            let text = fs::read_to_string(&config).unwrap();
            fs::write(
                config,
                format!("[Inheritance]\nDirectorySubvolume=no\n{text}"),
            )
            .unwrap();
        }
        let first = build(&fixture, &backend, &source).unwrap();
        let machine_id = fs::read_to_string(
            fixture
                .cache(&first.version, "packages")
                .join("etc/machine-id"),
        )
        .unwrap();
        assert_output(&first.root, &machine_id, &backend, subvolume);
        assert_eq!(
            fs::read_to_string(first.root.join(".post-maintenance-inherited")).unwrap(),
            machine_id
        );
        assert!(source.join("etc/machine-id/old-child/value").exists());
        assert!(
            !fixture
                .cache(&first.version, "packages")
                .join("etc/unrelated")
                .exists()
        );
        fixture.cleaned(&first.version);
        if reuse {
            let count = backend.preserve_copies.get();
            let second = build(&fixture, &backend, &source).unwrap();
            assert_output(&second.root, &machine_id, &backend, subvolume);
            assert_eq!(backend.preserve_copies.get(), count);
            fs::write(
                fixture
                    .store
                    .system_layout("desktop")
                    .unwrap()
                    .config
                    .join("build/inherit.conf"),
                "[Inheritance.CopyOnWriteBuild.etc]\nPath=/etc/\nPreserve=machine-id\n",
            )
            .unwrap();
            let third = build(&fixture, &backend, &source).unwrap();
            assert!(backend.preserve_copies.get() > count);
            assert!(third.root.join("etc/generated/source-only").exists());
            assert!(!third.root.join("etc/generated/built").exists());
            assert_eq!(backend.count("bootstrap"), 1);
        }
    }
}

/// Source-only layout errors fail preflight before versions or build operations exist.
/// 仅依赖来源布局即可确定的错误在分配版本或执行构建前失败。
#[test]
fn source_non_directory_paths_fail_before_build_mutations() {
    for link in [false, true] {
        let (fixture, backend, source) = setup("Build", false, false);
        fs::remove_dir_all(source.join("etc/generated")).unwrap();
        if link {
            symlink("machine-id", source.join("etc/generated")).unwrap();
        } else {
            fs::write(source.join("etc/generated"), "file").unwrap();
        }
        assert!(build(&fixture, &backend, &source).is_err());
        assert!(backend.events.borrow().is_empty());
        assert_eq!(
            fixture
                .store
                .system_layout("desktop")
                .unwrap()
                .versions
                .read_dir()
                .unwrap()
                .count(),
            0
        );
    }
    for link in [false, true] {
        let (fixture, backend, source) = setup("Build", false, false);
        let layout = fixture.store.system_layout("desktop").unwrap();
        fs::write(
            layout.config.join("build/inherit.conf"),
            "[Inheritance.CopyOnWriteBuild.etc]\nPath=/etc/\nPreserve=blocked/file\n",
        )
        .unwrap();
        if link {
            symlink("generated", source.join("etc/blocked")).unwrap();
        } else {
            fs::write(source.join("etc/blocked"), "file").unwrap();
        }
        assert!(build(&fixture, &backend, &source).is_err());
        assert!(backend.events.borrow().is_empty());
    }
}

/// Capture failures precede inheritance and restore failures never publish a partial version.
/// 暂存失败发生在继承前，还原失败不会发布半成品版本。
#[test]
fn capture_and_restore_failures_do_not_commit_script_output() {
    for failure in [2, 3] {
        let (fixture, backend, source) = setup("Build", false, false);
        backend.fail_preserve_copy_at.set(failure);
        assert!(build(&fixture, &backend, &source).is_err());
        assert!(fixture.cache("v00000001", "packages").exists());
        assert!(!fixture.cache("v00000001", "script").exists());
        let version = fixture
            .store
            .system_layout("desktop")
            .unwrap()
            .versions
            .join("v00000001");
        assert!(!version.join("root").exists());
        if failure == 2 {
            assert!(backend.inheritance_policies.borrow().is_empty());
        }
        assert!(
            !fixture
                .store
                .root()
                .join("work/desktop/v00000001/script-root-preserve")
                .exists()
        );
    }
}

/// Boot tasks freeze exception configuration and retry from pristine built data.
/// 启动待办固定例外配置，并从原始构建数据重试。
#[test]
fn first_boot_preserves_frozen_build_output_and_retries_without_overwriting_runtime_changes() {
    let (fixture, backend, source) = setup("Boot", false, false);
    let outcome = build(&fixture, &backend, &source).unwrap();
    let version = outcome.root.parent().unwrap();
    let manifest = record(&version.join("inheritance-pending.toml"));
    assert_eq!(manifest["schema_version"].as_integer(), Some(3));
    assert_eq!(
        manifest["sources"][0]["preserve"].as_array().unwrap().len(),
        2
    );
    let machine_id = fs::read_to_string(outcome.root.join("etc/machine-id")).unwrap();
    let built_time = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_234_567);
    fs::File::open(outcome.root.join("etc/generated"))
        .unwrap()
        .set_times(fs::FileTimes::new().set_modified(built_time))
        .unwrap();
    fs::write(
        fixture
            .store
            .system_layout("desktop")
            .unwrap()
            .config
            .join("build/inherit.conf"),
        "[Inheritance]\n",
    )
    .unwrap();
    backend.fail_preserve_copy_at.set(3);
    assert!(finalize(&outcome, &backend).is_err());
    assert!(!version.join("root").exists());
    assert!(!version.join("inheritance-staging-preserve").exists());
    assert_eq!(
        fs::read_to_string(outcome.root.join("etc/machine-id")).unwrap(),
        machine_id
    );
    let stale = version.join("inheritance-staging-preserve");
    backend.create_subvolume(&stale).unwrap();
    fs::write(stale.join("untrusted-stale-data"), "discard").unwrap();
    finalize(&outcome, &backend).unwrap();
    let root = version.join("root");
    assert_output(&root, &machine_id, &backend, false);
    assert_eq!(
        fs::metadata(root.join("etc/generated"))
            .unwrap()
            .modified()
            .unwrap(),
        built_time
    );
    fs::write(root.join("etc/generated/built"), "runtime").unwrap();
    fs::remove_dir_all(&source).unwrap();
    let copies = backend.preserve_copies.get();
    finalize(&outcome, &backend).unwrap();
    assert_eq!(backend.preserve_copies.get(), copies);
    assert_eq!(
        fs::read_to_string(root.join("etc/generated/built")).unwrap(),
        "runtime"
    );
}

/// Tampered relative paths and old schemas cannot silently bypass exception semantics.
/// 篡改的相对路径及旧 schema 不能静默绕过例外语义。
#[test]
fn first_boot_rejects_invalid_exception_manifests() {
    for invalid in ["../escape", "/absolute", "generated/../file"] {
        let (fixture, backend, source) = setup("Boot", false, false);
        let outcome = build(&fixture, &backend, &source).unwrap();
        let path = outcome
            .root
            .parent()
            .unwrap()
            .join("inheritance-pending.toml");
        let mut manifest = record(&path);
        manifest["sources"][0]["preserve"] = toml::Value::Array(vec![invalid.into()]);
        fs::write(&path, toml::to_string(&manifest).unwrap()).unwrap();
        assert!(finalize(&outcome, &backend).is_err());
        assert!(
            !outcome
                .root
                .parent()
                .unwrap()
                .join("inheritance-staging")
                .exists()
        );
    }
    let (fixture, backend, source) = setup("Boot", false, false);
    let outcome = build(&fixture, &backend, &source).unwrap();
    let path = outcome
        .root
        .parent()
        .unwrap()
        .join("inheritance-pending.toml");
    let mut manifest = record(&path);
    manifest["schema_version"] = 2.into();
    fs::write(&path, toml::to_string(&manifest).unwrap()).unwrap();
    assert!(finalize(&outcome, &backend).is_err());
}
