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
            "[Inheritance]\nDirectorySubvolume={}\nCopyOnWriteBoot={}{}\n",
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
fn build_and_boot_inheritance_capture_independent_sources_at_their_own_times() {
    for (main, subvolumes) in [(false, false), (false, true), (true, true)] {
        let fixture = Fixture::new();
        fixture.system(
            "desktop",
            &[
                "linux",
                "mkinitcpio",
                "btrfs-progs",
                "coreutils",
                "util-linux",
                "bash",
                "findutils",
            ],
        );
        if main {
            fs::write(
                fixture.store.global_config_path(),
                "schema_version=1\nmain_system='desktop'\n",
            )
            .unwrap();
        }
        let live = fixture.store.root().parent().unwrap();
        let build_directory = live.join("build-data");
        let build_file = live.join("build-file");
        let boot_directory = live.join("boot-data");
        let boot_file = live.join("boot-file");
        for directory in [&build_directory, &boot_directory] {
            fs::create_dir(directory).unwrap();
            fs::write(directory.join("value"), "at-build").unwrap();
        }
        for file in [&build_file, &boot_file] {
            fs::write(file, "at-build").unwrap();
        }
        let layout = fixture.store.system_layout("desktop").unwrap();
        fs::write(
            layout.config.join("build/inherit.conf"),
            format!(
                "[Inheritance]\nDirectorySubvolume={}\nCopyOnWriteBuild={}/\nCopyOnWriteBuild={}\nCopyOnWriteBoot={}/\nCopyOnWriteBoot={}\n",
                if subvolumes { "yes" } else { "no" },
                build_directory.display(), build_file.display(),
                boot_directory.display(), boot_file.display(),
            ),
        ).unwrap();
        let backend = SimulatedBackend::default();
        let outcome = build_with_backend(&fixture.store, Some("desktop"), &backend)
            .unwrap()
            .remove(0);
        assert!(outcome.root.ends_with("prepared-root"));
        assert_eq!(
            backend.count("prepare_inheritance_initramfs"),
            usize::from(main)
        );
        let version = outcome.root.parent().unwrap();
        let manifest = record(&version.join("inheritance-pending.toml"));
        let targets: Vec<_> = manifest["sources"]
            .as_array()
            .unwrap()
            .iter()
            .map(|source| Path::new(source["target"].as_str().unwrap()))
            .collect();
        assert_eq!(targets, [&boot_directory, &boot_file]);
        let relative = |path: &Path| path.strip_prefix("/").unwrap().to_path_buf();
        for file in [build_directory.join("value"), build_file.clone()] {
            assert_eq!(
                fs::read_to_string(outcome.root.join(relative(&file))).unwrap(),
                "at-build"
            );
            fs::write(file, "after-build").unwrap();
        }
        fs::remove_dir_all(&build_directory).unwrap();
        fs::remove_file(&build_file).unwrap();
        for file in [boot_directory.join("value"), boot_file.clone()] {
            assert!(!outcome.root.join(relative(&file)).exists());
            fs::write(file, "at-first-boot").unwrap();
        }
        backend.fail_inheritance_copy.set(true);
        assert!(finalize(&outcome, &backend).is_err());
        assert!(!version.join("root").exists());
        backend.fail_inheritance_copy.set(false);
        finalize(&outcome, &backend).unwrap();
        let root = version.join("root");
        for file in [build_directory.join("value"), build_file.clone()] {
            assert_eq!(
                fs::read_to_string(root.join(relative(&file))).unwrap(),
                "at-build"
            );
        }
        for file in [boot_directory.join("value"), boot_file.clone()] {
            assert_eq!(
                fs::read_to_string(root.join(relative(&file))).unwrap(),
                "at-first-boot"
            );
        }
        if subvolumes {
            for directory in [&build_directory, &boot_directory] {
                backend
                    .check_subvolume(&root.join(relative(directory)), false)
                    .unwrap();
            }
        }
        fs::remove_dir_all(&boot_directory).unwrap();
        fs::remove_file(&boot_file).unwrap();
        for file in [&build_file, &boot_file] {
            fs::write(root.join(relative(file)), "runtime-edited").unwrap();
        }
        let snapshots = backend.snapshots.borrow().len();
        finalize(&outcome, &backend).unwrap();
        assert_eq!(backend.snapshots.borrow().len(), snapshots);
        for file in [&build_file, &boot_file] {
            assert_eq!(
                fs::read_to_string(root.join(relative(file))).unwrap(),
                "runtime-edited"
            );
        }
    }
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
    let error = finalize(&outcome, &backend).unwrap_err().to_string();
    let directory = outcome.root.parent().unwrap();
    assert!(!directory.join("root").exists());
    assert!(directory.join("inheritance-staging").exists());
    assert!(directory.join("inheritance-error.txt").exists());
    assert_eq!(
        fs::read_to_string(directory.join("inheritance-error.txt")).unwrap(),
        error
    );
    let failed_log = fs::read_to_string(directory.join("inheritance.log")).unwrap();
    assert!(failed_log.contains(&error));
    assert!(failed_log.contains(source.to_str().unwrap()));
    assert!(failed_log.contains("Starting boot inheritance check"));
    assert!(failed_log.contains("Z]"));
    let install_log = fs::read(directory.join("install.log")).unwrap();
    assert_eq!(
        record(&directory.join("state.toml"))["state"].as_str(),
        Some("pending-inheritance")
    );
    fs::write(&source, "retry").unwrap();
    finalize(&outcome, &backend).unwrap();
    assert!(!directory.join("inheritance-staging").exists());
    assert!(!directory.join("inheritance-error.txt").exists());
    let recovered_log = fs::read_to_string(directory.join("inheritance.log")).unwrap();
    assert!(recovered_log.starts_with(&failed_log));
    assert!(recovered_log.contains("Boot inheritance check completed"));
    assert!(recovered_log.contains("publish the final root"));
    assert_eq!(
        fs::read(directory.join("install.log")).unwrap(),
        install_log
    );
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
fn preflight_failures_are_durable_and_success_keeps_the_retry_history() {
    for failure in [
        "missing-manifest",
        "invalid-manifest",
        "missing-source",
        "uuid",
        "identity",
    ] {
        let fixture = Fixture::new();
        fixture.system("desktop", &[]);
        let source = fixture.store.root().parent().unwrap().join("live-file");
        fs::write(&source, "data").unwrap();
        let backend = SimulatedBackend::default();
        let outcome = pending(&fixture, &backend, &source, true);
        let directory = outcome.root.parent().unwrap();
        let manifest_path = directory.join("inheritance-pending.toml");
        let original = fs::read(&manifest_path).unwrap();
        match failure {
            "missing-manifest" => fs::remove_file(&manifest_path).unwrap(),
            "invalid-manifest" => fs::write(&manifest_path, "not valid TOML [[").unwrap(),
            "missing-source" => fs::remove_file(&source).unwrap(),
            _ => {
                let mut manifest = record(&manifest_path);
                if failure == "uuid" {
                    manifest["filesystem_uuid"] = "another-filesystem".into();
                } else {
                    manifest["source_root"]["subvolume_id"] = "changed-root".into();
                }
                fs::write(&manifest_path, toml::to_string(&manifest).unwrap()).unwrap();
            }
        }
        let error = finalize(&outcome, &backend).unwrap_err().to_string();
        let failed_log = fs::read_to_string(directory.join("inheritance.log")).unwrap();
        assert!(failed_log.contains(&error), "{failure}: {failed_log}");
        assert_eq!(
            fs::read_to_string(directory.join("inheritance-error.txt")).unwrap(),
            error,
            "{failure}"
        );
        assert!(!directory.join("root").exists(), "{failure}");
        assert!(!directory.join("inheritance-staging").exists(), "{failure}");
        assert_eq!(
            record(&directory.join("state.toml"))["state"].as_str(),
            Some("pending-inheritance")
        );

        fs::write(&manifest_path, original).unwrap();
        fs::write(&source, "recovered").unwrap();
        finalize(&outcome, &backend).unwrap();
        assert!(!directory.join("inheritance-error.txt").exists());
        let log = fs::read_to_string(directory.join("inheritance.log")).unwrap();
        assert!(log.starts_with(&failed_log));
        assert!(log.contains("Boot inheritance check completed"));
        assert_eq!(
            fs::read_to_string(
                directory
                    .join("root")
                    .join(source.strip_prefix("/").unwrap())
            )
            .unwrap(),
            "recovered"
        );
    }
}

#[test]
fn error_summary_failure_does_not_hide_the_original_inheritance_error() {
    let fixture = Fixture::new();
    fixture.system("desktop", &[]);
    let source = fixture.store.root().parent().unwrap().join("live-file");
    fs::write(&source, "data").unwrap();
    let backend = SimulatedBackend::default();
    let outcome = pending(&fixture, &backend, &source, true);
    let directory = outcome.root.parent().unwrap();
    fs::remove_file(&source).unwrap();
    fs::create_dir(directory.join("inheritance-error.txt")).unwrap();
    let error = finalize(&outcome, &backend).unwrap_err().to_string();
    assert!(error.contains(source.to_str().unwrap()), "{error}");
    assert!(
        error.contains("Cannot save inheritance diagnostics"),
        "{error}"
    );
    let log = fs::read_to_string(directory.join("inheritance.log")).unwrap();
    assert!(log.contains(source.to_str().unwrap()));
    assert!(log.contains("Cannot save inheritance diagnostics"));
    assert!(!directory.join("root").exists());
}

#[test]
fn inheritance_log_stays_outside_roots_and_is_removed_with_its_version() {
    let fixture = Fixture::new();
    fixture.system("desktop", &[]);
    let source = fixture.store.root().parent().unwrap().join("live-file");
    fs::write(&source, "data").unwrap();
    let backend = SimulatedBackend::default();
    let first = pending(&fixture, &backend, &source, true);
    finalize(&first, &backend).unwrap();
    let directory = first.root.parent().unwrap();
    let log = fs::read(directory.join("inheritance.log")).unwrap();
    assert!(!directory.join("root/inheritance.log").exists());
    assert!(!first.root.join("inheritance.log").exists());
    let second = pending(&fixture, &backend, &source, true);
    assert_eq!(fs::read(directory.join("inheritance.log")).unwrap(), log);
    finalize(&second, &backend).unwrap();
    rstarsleep::controller::clean_with_backend(&fixture.store, "desktop", &first.version, &backend)
        .unwrap();
    assert!(!directory.exists());
    assert!(
        second
            .root
            .parent()
            .unwrap()
            .join("inheritance.log")
            .is_file()
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
        "[Inheritance]\nCopyOnWriteBoot=/boot/\nCopyOnWriteBoot=/usr/lib/modules/\n",
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
    // Legacy manifests carry physical source-to-target mappings.
    value["schema_version"] = toml::Value::Integer(1);
    value.as_table_mut().unwrap().remove("source_root");
    value["sources"][0].as_table_mut().unwrap().insert(
        "subvolume_id".into(),
        toml::Value::String("simulated-id".into()),
    );
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

#[test]
fn boot_nocow_policy_is_frozen_and_legacy_manifests_keep_strict_default() {
    use rstarsleep::config::NocowHandling;
    for legacy in [false, true] {
        let fixture = Fixture::new();
        fixture.system("desktop", &[]);
        let source = fixture.store.root().parent().unwrap().join("source-file");
        fs::write(&source, "data").unwrap();
        let layout = fixture.store.system_layout("desktop").unwrap();
        let config = layout.config.join("build/inherit.conf");
        fs::write(
            &config,
            format!(
                "[Inheritance]\nNocowHandling=copy\nCopyOnWriteBoot={}\n",
                source.display()
            ),
        )
        .unwrap();
        let backend = SimulatedBackend::default();
        let outcome = build_with_backend(&fixture.store, Some("desktop"), &backend)
            .unwrap()
            .remove(0);
        let manifest_path = outcome
            .root
            .parent()
            .unwrap()
            .join("inheritance-pending.toml");
        let mut manifest = record(&manifest_path);
        assert_eq!(manifest["nocow_handling"].as_str(), Some("copy"));
        if legacy {
            manifest["schema_version"] = toml::Value::Integer(1);
            manifest.as_table_mut().unwrap().remove("source_root");
            manifest["sources"][0].as_table_mut().unwrap().insert(
                "subvolume_id".into(),
                toml::Value::String("simulated-id".into()),
            );
            manifest.as_table_mut().unwrap().remove("nocow_handling");
            fs::write(&manifest_path, toml::to_string(&manifest).unwrap()).unwrap();
        }
        // Even an invalid live configuration cannot change an already frozen task.
        fs::write(
            &config,
            "[Inheritance]\nNocowHandling=move-snapshot\nDirectorySubvolume=no\n",
        )
        .unwrap();
        backend.inheritance_policies.borrow_mut().clear();
        finalize(&outcome, &backend).unwrap();
        assert_eq!(
            *backend.inheritance_policies.borrow(),
            [if legacy {
                NocowHandling::Error
            } else {
                NocowHandling::Copy
            }]
        );
    }
}

#[test]
fn pending_source_in_another_system_cache_blocks_invalidation() {
    let fixture = Fixture::new();
    fixture.system("desktop", &[]);
    fixture.system("consumer", &[]);
    let backend = SimulatedBackend::default();
    let first = build_with_backend(&fixture.store, Some("desktop"), &backend)
        .unwrap()
        .remove(0);
    let source = fixture
        .cache(&first.version, "script")
        .join("inherited-file");
    fs::write(&source, "keep pinned data").unwrap();
    let consumer = fixture.store.system_layout("consumer").unwrap();
    fs::write(
        consumer.config.join("build/inherit.conf"),
        format!("[Inheritance]\nCopyOnWriteBoot={}\n", source.display()),
    )
    .unwrap();
    build_with_backend(&fixture.store, Some("consumer"), &backend).unwrap();
    let error = build_with_backend(&fixture.store, Some("desktop"), &backend)
        .unwrap_err()
        .to_string();
    assert!(error.contains("引用，拒绝清理"), "{error}");
    assert_eq!(fs::read(&source).unwrap(), b"keep pinned data");
}

#[test]
fn unfinished_source_migration_pins_the_source_version_during_clean() {
    let fixture = Fixture::new();
    fixture.system("desktop", &[]);
    let backend = SimulatedBackend::default();
    let first = build_with_backend(&fixture.store, Some("desktop"), &backend)
        .unwrap()
        .remove(0);
    let source = first.root.join("data");
    fs::create_dir(&source).unwrap();
    fixture.retire_current("desktop");
    let records = fixture.store.root().join("inheritance-migrations");
    fs::create_dir(&records).unwrap();
    let receipt = format!(
        "schema_version=1\nfilesystem_uuid='simulated-uuid'\nsource='{}'\nold_subvolume_id='simulated-id'\nold_inode=1\nparent_subvolume_id='simulated-id'\nauthorization='build'\nphase='preparing'\n",
        source.display(),
    );
    fs::write(records.join("interrupted.toml"), receipt).unwrap();
    let error = rstarsleep::controller::clean_with_backend(
        &fixture.store,
        "desktop",
        &first.version,
        &backend,
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("继承引用"), "{error}");
    assert!(source.exists());
}

#[test]
fn missing_boot_source_is_warned_once_and_only_scanned_at_first_boot() {
    use rstarsleep::controller::build_with_backend_and_warning;
    let fixture = Fixture::new();
    fixture.system("desktop", &[]);
    let layout = fixture.store.system_layout("desktop").unwrap();
    let source = fixture
        .store
        .root()
        .parent()
        .unwrap()
        .join("not-yet-created");
    let configured = format!("{}/", source.display());
    let config = layout.config.join("build/inherit.conf");
    fs::write(
        &config,
        format!("[Inheritance]\nCopyOnWriteBoot={configured}\n"),
    )
    .unwrap();
    let backend = SimulatedBackend::default();
    let mut reports = 0;
    let outcome =
        build_with_backend_and_warning(&fixture.store, Some("desktop"), &backend, |warnings| {
            reports += 1;
            assert!(
                warnings
                    .iter()
                    .any(|w| w.contains("当前视图检查未通过") && w.contains(&configured))
            );
            assert!(!layout.root.join("version-counter").exists());
            assert!(backend.events.borrow().is_empty());
            // The report and task must both use the frozen configuration.
            fs::write(&config, "[Inheritance]\nNocowHandling=invalid\n").unwrap();
            Ok(())
        })
        .unwrap()
        .remove(0);
    assert_eq!(reports, 1);
    assert!(
        !backend
            .nested_scans
            .borrow()
            .iter()
            .any(|p| p.starts_with(&source))
    );
    let manifest = record(
        &outcome
            .root
            .parent()
            .unwrap()
            .join("inheritance-pending.toml"),
    );
    assert_eq!(manifest["schema_version"].as_integer(), Some(2));
    assert_eq!(manifest["sources"].as_array().unwrap().len(), 1);
    assert_eq!(
        manifest["sources"][0]["path"].as_str(),
        Some(configured.as_str())
    );
    assert!(finalize(&outcome, &backend).is_err());
    assert!(
        !outcome
            .root
            .parent()
            .unwrap()
            .join("inheritance-staging")
            .exists()
    );

    fs::create_dir(&source).unwrap();
    backend.create_subvolume(&source.join("nested")).unwrap();
    backend
        .create_subvolume(&source.join("nested/deep"))
        .unwrap();
    fs::write(source.join("nested/deep/data"), "created after build").unwrap();
    finalize(&outcome, &backend).unwrap();
    let target = outcome
        .root
        .parent()
        .unwrap()
        .join("root")
        .join(source.strip_prefix("/").unwrap());
    backend
        .check_subvolume(&target.join("nested/deep"), false)
        .unwrap();
    assert_eq!(
        fs::read_to_string(target.join("nested/deep/data")).unwrap(),
        "created after build"
    );
}

#[test]
fn boot_uuid_warning_keeps_raw_path_and_reads_below_the_fixed_source_root() {
    use rstarsleep::controller::build_with_backend_and_warning;
    let fixture = Fixture::new();
    fixture.system("desktop", &[]);
    let backend = SimulatedBackend::default();
    let old_root = fixture.store.root().parent().unwrap().join("source-root");
    backend.create_subvolume(&old_root).unwrap();
    fs::create_dir(old_root.join("var")).unwrap();
    fs::write(old_root.join("var/data"), "underlying data").unwrap();
    backend
        .filesystem_locations
        .borrow_mut()
        .insert("/".into(), ("simulated-uuid".into(), old_root.clone()));
    backend.filesystem_locations.borrow_mut().insert(
        "/var".into(),
        ("overlay-uuid".into(), "/mounted-overlay".into()),
    );
    let layout = fixture.store.system_layout("desktop").unwrap();
    fs::write(
        layout.config.join("build/inherit.conf"),
        "[Inheritance]\nCopyOnWriteBoot=/var/\n",
    )
    .unwrap();
    let mut reports = 0;
    let outcome =
        build_with_backend_and_warning(&fixture.store, Some("desktop"), &backend, |warnings| {
            reports += 1;
            assert!(
                warnings
                    .iter()
                    .any(|w| w.contains("/var/") && w.contains("UUID 不一致"))
            );
            Ok(())
        })
        .unwrap()
        .remove(0);
    assert_eq!(reports, 1);
    let manifest = record(
        &outcome
            .root
            .parent()
            .unwrap()
            .join("inheritance-pending.toml"),
    );
    assert_eq!(manifest["sources"].as_array().unwrap().len(), 1);
    assert_eq!(manifest["sources"][0]["path"].as_str(), Some("/var/"));
    assert!(
        !toml::to_string(&manifest)
            .unwrap()
            .contains("mounted-overlay")
    );
    assert_eq!(
        backend
            .filesystem_inspections
            .borrow()
            .iter()
            .filter(|p| **p == Path::new("/var"))
            .count(),
        1
    );
    assert!(
        !backend
            .nested_scans
            .borrow()
            .iter()
            .any(|p| p.starts_with(&old_root))
    );
    finalize(&outcome, &backend).unwrap();
    assert_eq!(
        fs::read_to_string(outcome.root.parent().unwrap().join("root/var/data")).unwrap(),
        "underlying data"
    );
}

#[test]
fn boot_manifest_does_not_expand_existing_descendant_mounts() {
    let fixture = Fixture::new();
    fixture.system("desktop", &[]);
    let backend = SimulatedBackend::default();
    // /proc has real descendant mounts in the test host's current view.
    let outcome = pending(&fixture, &backend, Path::new("/proc"), true);
    let manifest = record(
        &outcome
            .root
            .parent()
            .unwrap()
            .join("inheritance-pending.toml"),
    );
    assert_eq!(manifest["sources"].as_array().unwrap().len(), 1);
    assert_eq!(manifest["sources"][0]["path"].as_str(), Some("/proc/"));
    assert!(
        !backend
            .nested_scans
            .borrow()
            .iter()
            .any(|p| p.starts_with("/proc"))
    );
}

#[test]
fn fixed_boot_root_identity_and_source_symlinks_are_rejected_before_staging() {
    for replace_root in [false, true] {
        let fixture = Fixture::new();
        fixture.system("desktop", &[]);
        let backend = SimulatedBackend::default();
        let old_root = fixture.store.root().parent().unwrap().join("source-root");
        backend.create_subvolume(&old_root).unwrap();
        backend
            .filesystem_locations
            .borrow_mut()
            .insert("/".into(), ("simulated-uuid".into(), old_root.clone()));
        let layout = fixture.store.system_layout("desktop").unwrap();
        fs::write(
            layout.config.join("build/inherit.conf"),
            "[Inheritance]\nCopyOnWriteBoot=/var/\n",
        )
        .unwrap();
        let outcome = build_with_backend(&fixture.store, Some("desktop"), &backend)
            .unwrap()
            .remove(0);
        if replace_root {
            backend
                .source_ids
                .borrow_mut()
                .insert(old_root.clone(), "replacement-id".into());
        } else {
            std::os::unix::fs::symlink("/var", old_root.join("var")).unwrap();
        }
        let error = finalize(&outcome, &backend).unwrap_err().to_string();
        assert!(
            error.contains(if replace_root {
                "来源根身份已改变"
            } else {
                "路径不能经过链接"
            }),
            "{error}"
        );
        assert!(
            !outcome
                .root
                .parent()
                .unwrap()
                .join("inheritance-staging")
                .exists()
        );
    }
}

#[test]
fn cleanup_pins_resolve_configuration_paths_below_the_fixed_source_root() {
    let fixture = Fixture::new();
    fixture.system("desktop", &[]);
    let backend = SimulatedBackend::default();
    let first = build_with_backend(&fixture.store, Some("desktop"), &backend)
        .unwrap()
        .remove(0);
    fs::create_dir(first.root.join("var")).unwrap();
    backend
        .filesystem_locations
        .borrow_mut()
        .insert("/".into(), ("simulated-uuid".into(), first.root.clone()));
    let layout = fixture.store.system_layout("desktop").unwrap();
    fs::write(
        layout.config.join("build/inherit.conf"),
        "[Inheritance]\nCopyOnWriteBoot=/var/\n",
    )
    .unwrap();
    let second = build_with_backend(&fixture.store, Some("desktop"), &backend)
        .unwrap()
        .remove(0);
    let error = rstarsleep::controller::clean_with_backend(
        &fixture.store,
        "desktop",
        &first.version,
        &backend,
    )
    .unwrap_err();
    assert!(error.to_string().contains("继承引用"), "{error}");
    assert!(first.root.exists());
    finalize(&second, &backend).unwrap();
}

#[test]
fn boot_source_root_on_a_different_filesystem_warns_then_refuses_finalization() {
    use rstarsleep::controller::build_with_backend_and_warning;
    let fixture = Fixture::new();
    fixture.system("desktop", &[]);
    let backend = SimulatedBackend::default();
    backend
        .filesystem_locations
        .borrow_mut()
        .insert("/".into(), ("foreign-root-uuid".into(), "/old-root".into()));
    let layout = fixture.store.system_layout("desktop").unwrap();
    fs::write(
        layout.config.join("build/inherit.conf"),
        "[Inheritance]\nCopyOnWriteBoot=/var/\n",
    )
    .unwrap();
    let outcome =
        build_with_backend_and_warning(&fixture.store, Some("desktop"), &backend, |warnings| {
            assert!(
                warnings
                    .iter()
                    .any(|w| w.contains("来源根与目标 Btrfs UUID 不一致"))
            );
            Ok(())
        })
        .unwrap()
        .remove(0);
    // At boot, / models the target filesystem's top-level mount, not the running root.
    backend
        .filesystem_locations
        .borrow_mut()
        .remove(Path::new("/"));
    let error = finalize(&outcome, &backend).unwrap_err().to_string();
    assert!(error.contains("来源根与目标 Btrfs UUID 不一致"), "{error}");
    assert!(
        !outcome
            .root
            .parent()
            .unwrap()
            .join("inheritance-staging")
            .exists()
    );
}
