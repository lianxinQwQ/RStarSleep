//! Verify script payload copying, ordered execution, and isolated stage recovery.
//! 验证脚本资源复制、有序执行及隔离阶段恢复。
use super::*;
use std::{io::Write, path::Path};

/// Build a script archive with an optional single wrapper.
/// 构造含可选单层包装目录的脚本归档。
/// # Arguments
/// - `path`: Supported archive destination.
///   支持的归档目标路径。
/// - `wrapper`: Optional top-level directory.
///   可选顶层目录。
fn script_archive(path: &Path, wrapper: Option<&str>) {
    let mut builder = tar::Builder::new(Vec::new());
    let files = [
        (
            "main.sh",
            b"set -eu\ncat .data >> \"$RSTAR_ROOT/order\"\nprintf modified > .data\n".as_slice(),
        ),
        (".data", b"archive\n".as_slice()),
    ];
    for (name, bytes) in files {
        let name = wrapper.map_or_else(|| name.into(), |wrapper| format!("{wrapper}/{name}"));
        let mut header = tar::Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder.append_data(&mut header, name, bytes).unwrap();
    }
    let bytes = builder.into_inner().unwrap();
    if path.extension().unwrap() == "gz" {
        let mut encoder = flate2::write::GzEncoder::new(
            fs::File::create(path).unwrap(),
            flate2::Compression::default(),
        );
        encoder.write_all(&bytes).unwrap();
        encoder.finish().unwrap();
    } else {
        fs::write(path, bytes).unwrap();
    }
}

/// Exercise all input forms with real Bash, relative resources, order, and frozen inputs.
/// 使用真实 Bash 验证所有输入形式、相对资源、顺序及冻结输入。
/// # Arguments
/// No parameters.
/// 无参数。
#[test]
fn executes_mixed_script_inputs_after_software_with_private_writable_cwd() {
    let fixture = Fixture::new();
    fixture.system("desktop", &["editor"]);
    let scripts = fixture
        .store
        .system_layout("desktop")
        .unwrap()
        .config
        .join("script");
    assert!(scripts.is_dir());
    fs::write(scripts.join("01-loose.sh"), "set -eu\ntest -f \"$RSTAR_ROOT/.simulated-packages\"\ntest \"$RSTAR_SYSTEM_ID\" = desktop\ntest \"$(find . -mindepth 1 | wc -l)\" -eq 1\nprintf 'loose\\n' >> \"$RSTAR_ROOT/order\"\n").unwrap();
    let directory = scripts.join("02-directory");
    fs::create_dir(&directory).unwrap();
    fs::write(
        directory.join("main.sh"),
        "set -eu\ncat .data >> \"$RSTAR_ROOT/order\"\nprintf changed > .data\n",
    )
    .unwrap();
    fs::write(directory.join(".data"), "directory\n").unwrap();
    script_archive(&scripts.join("03-direct.tar"), None);
    script_archive(&scripts.join("04-wrapped.tar.gz"), Some("wrapper"));
    let backend = SimulatedBackend::default();
    let outcome = build_with_backend(&fixture.store, Some("desktop"), &backend)
        .unwrap()
        .remove(0);
    assert_eq!(
        fs::read_to_string(outcome.root.join("order")).unwrap(),
        "loose\ndirectory\narchive\narchive\n"
    );
    assert!(
        !fixture
            .cache(&outcome.version, "packages")
            .join("order")
            .exists()
    );
    assert_eq!(
        fs::read_to_string(directory.join(".data")).unwrap(),
        "directory\n"
    );
    let frozen = fixture
        .store
        .system_layout("desktop")
        .unwrap()
        .versions
        .join(&outcome.version)
        .join("config/script");
    assert_eq!(
        fs::read_to_string(frozen.join("02-directory/.data")).unwrap(),
        "directory\n"
    );
    assert!(fs::read_dir(&outcome.root).unwrap().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".rstarsleep-script-")
    }));
    let events = backend.events.borrow();
    let first_script = events
        .iter()
        .position(|event| event.operation == "script")
        .unwrap();
    let install = events
        .iter()
        .position(|event| event.operation == "install")
        .unwrap();
    assert!(first_script > install);
    assert_eq!(backend.count("script"), 4);
    assert!(
        events
            .iter()
            .filter(|event| event.operation == "script")
            .all(|event| event.root.ends_with("script-root"))
    );
    assert_eq!(
        record(&fixture.cache(&outcome.version, "script.toml"))["stage"].as_str(),
        Some("script")
    );
    backend.check_subvolume(&outcome.root, true).unwrap();
    fixture.cleaned(&outcome.version);
}

/// Reapply changed scripts from clean software caches without inheriting prior script effects.
/// 从干净软件缓存重新应用变更脚本，不继承先前脚本效果。
/// # Arguments
/// No parameters.
/// 无参数。
#[test]
fn script_edits_and_removal_preserve_software_cache_reuse() {
    let fixture = Fixture::new();
    fixture.system("desktop", &["editor"]);
    let scripts = fixture
        .store
        .system_layout("desktop")
        .unwrap()
        .config
        .join("script");
    let path = scripts.join("configure.sh");
    fs::write(&path, "printf first > \"$RSTAR_ROOT/obsolete\"\n").unwrap();
    let backend = SimulatedBackend::default();
    let first = build_with_backend(&fixture.store, Some("desktop"), &backend)
        .unwrap()
        .remove(0);
    // Prior script caches are deliberately unread: even corrupted entries cannot select a branch.
    // 明确不读取过往脚本缓存：即使条目损坏，也不能影响步骤分支。
    fs::write(
        fixture.cache(&first.version, "script.toml"),
        "invalid record",
    )
    .unwrap();
    fs::write(&path, "printf second > \"$RSTAR_ROOT/current\"\n").unwrap();
    backend.events.borrow_mut().clear();
    let second = build_with_backend(&fixture.store, Some("desktop"), &backend)
        .unwrap()
        .remove(0);
    assert!(!fixture.cache(&first.version, "script").exists());
    assert!(!fixture.cache(&first.version, "script.toml").exists());
    assert!(fixture.cache(&first.version, "base-system").exists());
    assert!(fixture.cache(&first.version, "packages").exists());
    assert!(!second.root.join("obsolete").exists());
    assert_eq!(
        fs::read_to_string(second.root.join("current")).unwrap(),
        "second"
    );
    assert_eq!(
        fs::read_to_string(first.root.join("obsolete")).unwrap(),
        "first"
    );
    assert_eq!(backend.count("system_init"), 0);
    assert_eq!(backend.count("update"), 2);
    assert_eq!(
        record(&fixture.cache(&second.version, "packages.toml"))["source_version"].as_str(),
        Some(first.version.as_str())
    );
    fs::remove_dir_all(scripts).unwrap();
    backend.events.borrow_mut().clear();
    let third = build_with_backend(&fixture.store, Some("desktop"), &backend)
        .unwrap()
        .remove(0);
    assert!(!third.root.join("current").exists());
    assert_eq!(backend.count("script"), 0);
    assert_eq!(
        record(&fixture.cache(&third.version, "packages.toml"))["source_version"].as_str(),
        Some(second.version.as_str())
    );
}

/// Retain completed software caches after a script failure and stop later scripts/publication.
/// 脚本失败后保留已完成软件缓存，停止后续脚本和发布。
/// # Arguments
/// No parameters.
/// 无参数。
#[test]
fn script_failure_keeps_software_cache_and_retry_starts_from_clean_software() {
    let fixture = Fixture::new();
    fixture.system("desktop", &["editor"]);
    let layout = fixture.store.system_layout("desktop").unwrap();
    let scripts = layout.config.join("script");
    fs::write(
        scripts.join("01-failure.sh"),
        "printf partial > \"$RSTAR_ROOT/partial\"\necho intentional-failure >&2\nexit 23\n",
    )
    .unwrap();
    fs::write(
        scripts.join("02-later.sh"),
        "printf done > \"$RSTAR_ROOT/done\"\n",
    )
    .unwrap();
    let backend = SimulatedBackend::default();
    let error = build_with_backend(&fixture.store, Some("desktop"), &backend).unwrap_err();
    assert!(error.to_string().contains("intentional-failure"));
    assert_eq!(backend.count("script"), 1);
    assert!(fixture.cache("v00000001", "packages.toml").is_file());
    assert!(!fixture.cache("v00000001", "script.toml").exists());
    assert!(!layout.versions.join("v00000001/root").exists());
    assert_eq!(
        record(&layout.versions.join("v00000001/state.toml"))["state"].as_str(),
        Some("failed")
    );
    fixture.cleaned("v00000001");
    fs::remove_file(scripts.join("01-failure.sh")).unwrap();
    backend.events.borrow_mut().clear();
    let retried = build_with_backend(&fixture.store, Some("desktop"), &backend)
        .unwrap()
        .remove(0);
    assert!(!retried.root.join("partial").exists());
    assert_eq!(
        fs::read_to_string(retried.root.join("done")).unwrap(),
        "done"
    );
    assert_eq!(backend.count("system_init"), 0);
    assert_eq!(backend.count("update"), 2);
    fixture.cleaned(&retried.version);
}

/// Reject missing main.sh and unsupported entries before running any scripts.
/// 在执行任何脚本前拒绝缺少 main.sh 及不支持的条目。
/// # Arguments
/// No parameters.
/// 无参数。
#[test]
fn malformed_script_inputs_stop_the_stage() {
    for entry in ["directory", "unsupported.txt"] {
        let fixture = Fixture::new();
        fixture.system("desktop", &[]);
        let path = fixture
            .store
            .system_layout("desktop")
            .unwrap()
            .config
            .join("script")
            .join(entry);
        if entry == "directory" {
            fs::create_dir(path).unwrap();
        } else {
            fs::write(path, "ignored?").unwrap();
        }
        let backend = SimulatedBackend::default();
        assert!(build_with_backend(&fixture.store, Some("desktop"), &backend).is_err());
        assert_eq!(backend.count("script"), 0);
        assert!(fixture.cache("v00000001", "packages.toml").exists());
        fixture.cleaned("v00000001");
    }
}

/// Require a writable CoW root restored from software before running any script.
/// 执行任何脚本前，要求从软件阶段恢复可写 CoW 根。
/// # Arguments
/// No parameters.
/// 无参数。
#[test]
fn script_step_requires_cow_restore_and_never_falls_back_to_copying_the_system() {
    let fixture = Fixture::new();
    fixture.system("desktop", &[]);
    let layout = fixture.store.system_layout("desktop").unwrap();
    fs::write(layout.config.join("script/test.sh"), "exit 0\n").unwrap();
    let backend = SimulatedBackend::default();
    *backend.fail_restore_to.borrow_mut() = Some("script-root");
    assert!(build_with_backend(&fixture.store, Some("desktop"), &backend).is_err());
    assert_eq!(backend.count("script"), 0);
    assert!(fixture.cache("v00000001", "packages.toml").is_file());
    assert!(!fixture.cache("v00000001", "script.toml").exists());
    assert!(!layout.versions.join("v00000001/root").exists());
    assert!(backend.snapshots.borrow().iter().any(|snapshot| {
        snapshot.source == fixture.cache("v00000001", "packages")
            && snapshot.destination.ends_with("script-root")
            && !snapshot.readonly
    }));
    fixture.cleaned("v00000001");
}

/// Detect script changes in archived configuration before reusing software caches.
/// 复用软件缓存前检测留档配置内的脚本修改。
/// # Arguments
/// No parameters.
/// 无参数。
#[test]
fn modified_archived_scripts_invalidate_the_recorded_provenance() {
    let fixture = Fixture::new();
    fixture.system("desktop", &[]);
    let layout = fixture.store.system_layout("desktop").unwrap();
    fs::write(layout.config.join("script/test.sh"), "exit 0\n").unwrap();
    let backend = SimulatedBackend::default();
    let first = build_with_backend(&fixture.store, Some("desktop"), &backend)
        .unwrap()
        .remove(0);
    fs::write(
        layout
            .versions
            .join(&first.version)
            .join("config/script/test.sh"),
        "exit 1\n",
    )
    .unwrap();
    backend.events.borrow_mut().clear();
    assert!(
        build_with_backend(&fixture.store, Some("desktop"), &backend)
            .unwrap_err()
            .to_string()
            .contains("缓存来源配置已改变")
    );
    assert!(backend.events.borrow().is_empty());
}
