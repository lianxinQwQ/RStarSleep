//! Exercise first-boot publication and recovery without mounting a live system.
use super::*;
use rstarsleep::controller::finalize_inheritance_with_backend;
use std::path::Path;

fn pending(
    fixture: &Fixture,
    backend: &SimulatedBackend,
    source: &Path,
    subvolumes: bool,
) -> rstarsleep::controller::BuildOutcome {
    let layout = fixture.store.system_layout("desktop").unwrap();
    fs::write(
        layout.config.join("build/inherit.conf"),
        format!(
            "[Inheritance]\nTiming=initramfs\nDirectorySubvolume={}\nCopyOnWrite={}{}\n",
            if subvolumes { "yes" } else { "no" },
            source.display(),
            if source.is_dir() { "/" } else { "" }
        ),
    )
    .unwrap();
    build_with_backend(&fixture.store, Some("desktop"), backend)
        .unwrap()
        .remove(0)
}

fn finalize(
    outcome: &rstarsleep::controller::BuildOutcome,
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

#[test]
fn first_boot_rejects_manifest_target_with_mismatched_type_marker() {
    for directory in [true, false] {
        let fixture = Fixture::new();
        fixture.system("desktop", &[]);
        let source = fixture.store.root().parent().unwrap().join("live-source");
        if directory {
            fs::create_dir(&source).unwrap();
        } else {
            fs::write(&source, "file").unwrap();
        }
        let backend = SimulatedBackend::default();
        let outcome = pending(&fixture, &backend, &source, true);
        let version = outcome.root.parent().unwrap();
        let path = version.join("inheritance-pending.toml");
        let mut manifest = record(&path);
        manifest["sources"][0]["target"] = toml::Value::String(format!(
            "{}{}",
            source.display(),
            if directory { "" } else { "/" },
        ));
        fs::write(&path, toml::to_string(&manifest).unwrap()).unwrap();
        let error = finalize(&outcome, &backend).unwrap_err().to_string();
        assert!(
            error.contains(if directory {
                "继承目录路径必须以 / 结尾"
            } else {
                "继承文件路径不能以 / 结尾"
            }),
            "{error}"
        );
        assert!(!version.join("root").exists());
        assert!(!version.join("inheritance-staging").exists());
    }
}

#[test]
fn first_boot_captures_latest_data_once_and_survives_cache_invalidation() {
    let fixture = Fixture::new();
    fixture.system("desktop", &["kept"]);
    let source = fixture.store.root().parent().unwrap().join("live-data");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("value"), "at-build").unwrap();
    let backend = SimulatedBackend::default();
    let first = pending(&fixture, &backend, &source, true);
    assert_eq!(first.root.file_name().unwrap(), "prepared-root");
    backend.check_subvolume(&first.root, true).unwrap();
    assert_eq!(
        record(&first.root.parent().unwrap().join("state.toml"))["state"].as_str(),
        Some("pending-inheritance")
    );
    assert!(!first.root.parent().unwrap().join("root").exists());
    assert!(!first.root.join(source.strip_prefix("/").unwrap()).exists());
    let second = pending(&fixture, &backend, &source, true);
    assert!(!fixture.cache(&first.version, "script").exists());
    assert!(first.root.exists());
    fs::write(source.join("value"), "at-first-boot").unwrap();
    finalize(&first, &backend).unwrap();
    let root = first.root.parent().unwrap().join("root");
    let inherited = root.join(source.strip_prefix("/").unwrap());
    assert_eq!(
        fs::read_to_string(inherited.join("value")).unwrap(),
        "at-first-boot"
    );
    backend.check_subvolume(&root, false).unwrap();
    backend.check_subvolume(&inherited, false).unwrap();
    assert_eq!(
        record(&root.parent().unwrap().join("state.toml"))["state"].as_str(),
        Some("complete")
    );
    fs::write(source.join("value"), "later").unwrap();
    fs::write(inherited.join("value"), "runtime-edited").unwrap();
    let count = backend.snapshots.borrow().len();
    finalize(&first, &backend).unwrap();
    assert_eq!(backend.snapshots.borrow().len(), count);
    assert_eq!(
        fs::read_to_string(inherited.join("value")).unwrap(),
        "runtime-edited"
    );
    assert!(second.root.exists());
}

#[test]
fn failed_copy_never_publishes_root_and_retry_rebuilds_staging() {
    let fixture = Fixture::new();
    fixture.system("desktop", &[]);
    let source = fixture.store.root().parent().unwrap().join("live-file");
    fs::write(&source, "first").unwrap();
    let backend = SimulatedBackend::default();
    let outcome = pending(&fixture, &backend, &source, true);
    backend.fail_inheritance_copy.set(true);
    assert!(finalize(&outcome, &backend).is_err());
    let directory = outcome.root.parent().unwrap();
    assert!(!directory.join("root").exists());
    assert!(directory.join("inheritance-staging").exists());
    assert!(directory.join("inheritance-error.txt").exists());
    assert_eq!(
        record(&directory.join("state.toml"))["state"].as_str(),
        Some("pending-inheritance")
    );
    fs::write(&source, "retry").unwrap();
    finalize(&outcome, &backend).unwrap();
    assert!(!directory.join("inheritance-staging").exists());
    assert!(!directory.join("inheritance-error.txt").exists());
    assert_eq!(
        fs::read_to_string(
            directory
                .join("root")
                .join(source.strip_prefix("/").unwrap())
        )
        .unwrap(),
        "retry"
    );
}

#[test]
fn published_root_can_repair_an_interrupted_status_update_but_not_a_mismatched_receipt() {
    let fixture = Fixture::new();
    fixture.system("desktop", &[]);
    let source = fixture.store.root().parent().unwrap().join("live-file");
    fs::write(&source, "data").unwrap();
    let backend = SimulatedBackend::default();
    let outcome = pending(&fixture, &backend, &source, true);
    let directory = outcome.root.parent().unwrap();
    let before = fs::read_to_string(directory.join("state.toml")).unwrap();
    finalize(&outcome, &backend).unwrap();
    fs::write(directory.join("state.toml"), before).unwrap();
    fs::remove_file(&source).unwrap();
    finalize(&outcome, &backend).unwrap();
    assert_eq!(
        record(&directory.join("state.toml"))["state"].as_str(),
        Some("complete")
    );
    let receipt_path = directory.join("inheritance-commit.toml");
    let receipt = record(&receipt_path);
    for field in ["manifest_sha256", "root_subvolume_id"] {
        let mut mismatched = receipt.clone();
        mismatched[field] = toml::Value::String("wrong".into());
        fs::write(&receipt_path, toml::to_string(&mismatched).unwrap()).unwrap();
        assert!(finalize(&outcome, &backend).is_err());
    }
    fs::write(&receipt_path, "wrong").unwrap();
    assert!(finalize(&outcome, &backend).is_err());
}

#[test]
fn completed_writable_root_preserves_subvolume_changes_on_reboot() {
    let fixture = Fixture::new();
    fixture.system("desktop", &[]);
    let source = fixture.store.root().parent().unwrap().join("live-data");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("value"), "inherited").unwrap();
    let backend = SimulatedBackend::default();
    let outcome = pending(&fixture, &backend, &source, true);
    finalize(&outcome, &backend).unwrap();
    let root = outcome.root.parent().unwrap().join("root");
    let inherited = root.join(source.strip_prefix("/").unwrap());
    backend.delete_subvolume_recursive(&inherited).unwrap();
    let runtime_data = root.join("runtime-data");
    backend.create_subvolume(&runtime_data).unwrap();
    fs::write(runtime_data.join("value"), "runtime change").unwrap();
    fs::remove_dir_all(&source).unwrap();
    let snapshots = backend.snapshots.borrow().len();

    finalize(&outcome, &backend).unwrap();

    assert_eq!(backend.snapshots.borrow().len(), snapshots);
    assert!(!inherited.exists());
    backend.check_subvolume(&root, false).unwrap();
    backend.check_subvolume(&runtime_data, false).unwrap();
    assert_eq!(
        fs::read_to_string(runtime_data.join("value")).unwrap(),
        "runtime change"
    );
}

#[test]
fn interrupted_publication_still_rejects_a_changed_subvolume_tree() {
    for add_subvolume in [false, true] {
        let fixture = Fixture::new();
        fixture.system("desktop", &[]);
        let source = fixture.store.root().parent().unwrap().join("live-data");
        fs::create_dir(&source).unwrap();
        let backend = SimulatedBackend::default();
        let outcome = pending(&fixture, &backend, &source, true);
        let directory = outcome.root.parent().unwrap();
        let state = fs::read_to_string(directory.join("state.toml")).unwrap();
        finalize(&outcome, &backend).unwrap();
        fs::write(directory.join("state.toml"), state).unwrap();
        let root = directory.join("root");
        if add_subvolume {
            backend.create_subvolume(&root.join("unexpected")).unwrap();
        } else {
            backend
                .delete_subvolume_recursive(&root.join(source.strip_prefix("/").unwrap()))
                .unwrap();
        }

        let error = finalize(&outcome, &backend).unwrap_err().to_string();

        assert!(error.contains("最终根与继承提交记录不匹配"), "{error}");
        assert_eq!(
            record(&directory.join("state.toml"))["state"].as_str(),
            Some("pending-inheritance")
        );
    }
}

#[test]
fn source_identity_and_filesystem_are_checked_before_any_destination_is_created() {
    let fixture = Fixture::new();
    fixture.system("desktop", &[]);
    let source = fixture.store.root().parent().unwrap().join("live-file");
    fs::write(&source, "data").unwrap();
    let backend = SimulatedBackend::default();
    let outcome = pending(&fixture, &backend, &source, true);
    let directory = outcome.root.parent().unwrap();
    let manifest = directory.join("inheritance-pending.toml");
    let text = fs::read_to_string(&manifest).unwrap();
    for modified in [
        text.replace("simulated-id", "different-id"),
        text.replace("simulated-uuid", "different-uuid"),
    ] {
        fs::write(&manifest, modified).unwrap();
        assert!(finalize(&outcome, &backend).is_err());
        assert!(!directory.join("inheritance-staging").exists());
        assert!(!directory.join("root").exists());
    }
}

#[test]
fn directory_subvolume_setting_and_recursive_existing_sources_are_honored() {
    let fixture = Fixture::new();
    fixture.system("desktop", &[]);
    let source = fixture.store.root().parent().unwrap().join("source-subvol");
    let backend = SimulatedBackend::default();
    backend.create_subvolume(&source).unwrap();
    let child = source.join("nested with spaces");
    backend.create_subvolume(&child).unwrap();
    backend.create_subvolume(&child.join("deep")).unwrap();
    fs::write(child.join("deep/value"), "nested").unwrap();
    let outcome = pending(&fixture, &backend, &source, true);
    // An existing source subvolume must not invoke the copy path.
    backend.fail_inheritance_copy.set(true);
    finalize(&outcome, &backend).unwrap();
    assert!(backend.fail_inheritance_copy.replace(false));
    let target = outcome
        .root
        .parent()
        .unwrap()
        .join("root")
        .join(source.strip_prefix("/").unwrap());
    for path in [
        &target,
        &target.join("nested with spaces"),
        &target.join("nested with spaces/deep"),
    ] {
        backend.check_subvolume(path, false).unwrap();
    }
    assert_eq!(
        fs::read_to_string(target.join("nested with spaces/deep/value")).unwrap(),
        "nested"
    );
    let ordinary = fixture.store.root().parent().unwrap().join("ordinary");
    fs::create_dir(&ordinary).unwrap();
    fs::write(ordinary.join("value"), "flat").unwrap();
    let flat = pending(&fixture, &backend, &ordinary, false);
    finalize(&flat, &backend).unwrap();
    let target = flat
        .root
        .parent()
        .unwrap()
        .join("root")
        .join(ordinary.strip_prefix("/").unwrap());
    assert!(!target.join(MARKER).exists());
    assert_eq!(fs::read_to_string(target.join("value")).unwrap(), "flat");
}

#[test]
fn cleanup_preserves_sources_of_retained_pending_versions_and_can_cancel_the_task() {
    let fixture = Fixture::new();
    fixture.system("desktop", &[]);
    let backend = SimulatedBackend::default();
    let first = build_with_backend(&fixture.store, Some("desktop"), &backend)
        .unwrap()
        .remove(0);
    let source = first.root.join("data");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("value"), "source").unwrap();
    let second = pending(&fixture, &backend, &source, true);
    let error = rstarsleep::controller::clean_with_backend(
        &fixture.store,
        "desktop",
        &first.version,
        &backend,
    )
    .unwrap_err();
    assert!(error.to_string().contains("继承引用"));
    assert!(first.root.exists());
    assert!(second.root.exists());
    fixture.retire_current("desktop");
    rstarsleep::controller::clean_with_backend(
        &fixture.store,
        "desktop",
        &second.version,
        &backend,
    )
    .unwrap();
    assert!(!first.root.exists());
    assert!(!second.root.exists());
}

#[test]
fn recursive_snapshot_restores_every_child_and_writable_copies_without_changing_source() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source");
    let backend = SimulatedBackend::default();
    backend.create_subvolume(&source).unwrap();
    backend.create_subvolume(&source.join("one")).unwrap();
    backend.create_subvolume(&source.join("one/two")).unwrap();
    fs::write(source.join("one/two/data"), "before").unwrap();
    let readonly = directory.path().join("readonly");
    backend
        .snapshot_recursive(&source, &readonly, true)
        .unwrap();
    for relative in ["", "one", "one/two"] {
        backend
            .check_subvolume(&readonly.join(relative), true)
            .unwrap();
    }
    let writable = directory.path().join("writable");
    backend
        .snapshot_recursive(&readonly, &writable, false)
        .unwrap();
    for relative in ["", "one", "one/two"] {
        backend
            .check_subvolume(&writable.join(relative), false)
            .unwrap();
    }
    fs::write(source.join("one/two/data"), "after").unwrap();
    assert_eq!(
        fs::read_to_string(writable.join("one/two/data")).unwrap(),
        "before"
    );
    backend.delete_subvolume_recursive(&readonly).unwrap();
    assert!(source.join("one/two/data").exists());
}

#[test]
fn build_warning_can_be_cancelled_before_any_version_or_cache_mutation() {
    use std::{
        io::{BufRead, BufReader},
        process::{Command, Stdio},
    };
    let fixture = Fixture::new();
    fixture.system("desktop", &[]);
    let layout = fixture.store.system_layout("desktop").unwrap();
    fs::write(
        layout.config.join("build/inherit.conf"),
        "[Inheritance]\nTiming=initramfs\nCopyOnWrite=/boot/\nCopyOnWrite=/usr/lib/modules/\n",
    )
    .unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_RStarSleep"))
        .args(["build", "desktop", "--root"])
        .arg(fixture.store.root())
        .stderr(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    let mut stderr = BufReader::new(child.stderr.take().unwrap());
    let mut output = String::new();
    loop {
        let mut line = String::new();
        assert_ne!(stderr.read_line(&mut line).unwrap(), 0, "{output}");
        output.push_str(&line);
        if line.contains("3 秒") {
            break;
        }
    }
    assert!(output.contains("/boot") && output.contains("/usr/lib/modules"));
    assert!(
        Command::new("kill")
            .args(["-INT", &child.id().to_string()])
            .status()
            .unwrap()
            .success()
    );
    assert!(!child.wait().unwrap().success());
    assert_eq!(fs::read_dir(&layout.versions).unwrap().count(), 0);
    assert!(!layout.root.join("version-counter").exists());
    assert!(!fixture.store.root().join("RStarSleep").exists());
}

#[test]
fn script_created_subvolumes_survive_cache_publication_and_first_boot_without_inheritance_entries()
{
    let fixture = Fixture::new();
    fixture.system("desktop", &[]);
    let layout = fixture.store.system_layout("desktop").unwrap();
    fs::write(layout.config.join("script/create.sh"), "set -eu\nmkdir -p \"$RSTAR_ROOT/data/nested\"\nprintf writable > \"$RSTAR_ROOT/data/.simulated-subvolume\"\nprintf writable > \"$RSTAR_ROOT/data/nested/.simulated-subvolume\"\nprintf payload > \"$RSTAR_ROOT/data/nested/value\"\n").unwrap();
    let backend = SimulatedBackend::default();
    let outcome = build_with_backend(&fixture.store, Some("desktop"), &backend)
        .unwrap()
        .remove(0);
    for (root, readonly) in [
        (fixture.cache(&outcome.version, "script"), true),
        (outcome.root.clone(), false),
    ] {
        backend
            .check_subvolume(&root.join("data"), readonly)
            .unwrap();
        backend
            .check_subvolume(&root.join("data/nested"), readonly)
            .unwrap();
        assert_eq!(
            fs::read_to_string(root.join("data/nested/value")).unwrap(),
            "payload"
        );
    }
    let source = fixture.store.root().parent().unwrap().join("file");
    fs::write(&source, "additional").unwrap();
    let pending = pending(&fixture, &backend, &source, true);
    finalize(&pending, &backend).unwrap();
    let root = pending.root.parent().unwrap().join("root");
    backend
        .check_subvolume(&root.join("data/nested"), false)
        .unwrap();
    assert_eq!(
        fs::read_to_string(root.join("data/nested/value")).unwrap(),
        "payload"
    );
}

#[test]
fn ordinary_outer_directories_retain_internal_subvolumes_when_subvolume_creation_is_disabled() {
    let fixture = Fixture::new();
    fixture.system("desktop", &[]);
    let backend = SimulatedBackend::default();
    let source = fixture.store.root().parent().unwrap().join("outer");
    fs::create_dir(&source).unwrap();
    backend.create_subvolume(&source.join("inner")).unwrap();
    fs::write(source.join("inner/value"), "inside").unwrap();
    let outcome = pending(&fixture, &backend, &source, false);
    finalize(&outcome, &backend).unwrap();
    let target = outcome
        .root
        .parent()
        .unwrap()
        .join("root")
        .join(source.strip_prefix("/").unwrap());
    assert!(!target.join(MARKER).exists());
    backend
        .check_subvolume(&target.join("inner"), false)
        .unwrap();
    assert_eq!(
        fs::read_to_string(target.join("inner/value")).unwrap(),
        "inside"
    );
}

#[test]
fn deferred_targets_follow_guest_directory_symlinks_inside_the_new_root() {
    let fixture = Fixture::new();
    fixture.system("desktop", &[]);
    let layout = fixture.store.system_layout("desktop").unwrap();
    fs::write(
        layout.config.join("script/links.sh"),
        "set -eu\nmkdir -p \"$RSTAR_ROOT/usr/lib\"\nln -s /usr/lib \"$RSTAR_ROOT/lib\"\n",
    )
    .unwrap();
    let source = fixture.store.root().parent().unwrap().join("file");
    fs::write(&source, "data").unwrap();
    let backend = SimulatedBackend::default();
    let outcome = pending(&fixture, &backend, &source, true);
    let manifest = outcome
        .root
        .parent()
        .unwrap()
        .join("inheritance-pending.toml");
    let mut value: toml::Value = toml::from_str(&fs::read_to_string(&manifest).unwrap()).unwrap();
    value["sources"][0]["target"] = toml::Value::String("/lib/rstarsleep-user-test".into());
    fs::write(&manifest, toml::to_string(&value).unwrap()).unwrap();
    finalize(&outcome, &backend).unwrap();
    assert_eq!(
        fs::read_to_string(
            outcome
                .root
                .parent()
                .unwrap()
                .join("root/usr/lib/rstarsleep-user-test")
        )
        .unwrap(),
        "data"
    );
}
