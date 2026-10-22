//! Exercise automatic inheritance selection through the default build entry points.
//! 通过默认构建入口验证自动继承来源选择。

use super::*;
use rstarsleep::controller::build_with_backend_and_warning;
use std::path::{Path, PathBuf};

/// Select the fixture's primary system without changing per-system build inputs.
/// 选择测试夹具的主系统，不修改各系统的构建输入。
fn primary_system(fixture: &Fixture) {
    fs::write(
        fixture.store.global_config_path(),
        "schema_version=1\nmain_system='desktop'\n",
    )
    .unwrap();
}

/// Configure one guest file whose contents identify the selected inheritance root.
/// 配置一个客体文件，以其内容识别所选继承来源根。
fn inherit_file(fixture: &Fixture, id: &str, guest: &Path) {
    fs::write(
        fixture
            .store
            .system_layout(id)
            .unwrap()
            .config
            .join("build/inherit.conf"),
        format!("[Inheritance]\nCopyOnWriteBuild={}\n", guest.display()),
    )
    .unwrap();
}

/// Seed a historical version without a cache donor, archived config, or boot manifest.
/// 创建历史版本，不提供缓存来源、留档配置或启动清单。
fn historical_root(
    fixture: &Fixture,
    backend: &SimulatedBackend,
    id: &str,
    version: &str,
    state: &str,
    guest: &Path,
    contents: &str,
) -> PathBuf {
    let directory = fixture
        .store
        .system_layout(id)
        .unwrap()
        .versions
        .join(version);
    fs::create_dir(&directory).unwrap();
    fs::write(
        directory.join("state.toml"),
        format!("schema_version=1\nsystem_id='{id}'\nversion='{version}'\nstate='{state}'\n"),
    )
    .unwrap();
    let root = directory.join("root");
    backend.create_subvolume(&root).unwrap();
    let saved = root.join(guest.strip_prefix("/").unwrap());
    fs::create_dir_all(saved.parent().unwrap()).unwrap();
    fs::write(saved, contents).unwrap();
    root
}

/// Prefer the live primary-system view over every newer completed historical root.
/// 主系统优先使用当前运行视图，即使存在更新的已完成历史根。
#[test]
fn primary_system_uses_running_root_when_its_older_version_is_running() {
    let fixture = Fixture::new();
    fixture.system("desktop", &["linux", "mkinitcpio"]);
    primary_system(&fixture);
    let guest = fixture.store.root().parent().unwrap().join("live-saved");
    fs::write(&guest, "running view").unwrap();
    inherit_file(&fixture, "desktop", &guest);
    let backend = SimulatedBackend::default();
    let running = historical_root(
        &fixture,
        &backend,
        "desktop",
        "v9",
        "complete",
        &guest,
        "older historical view",
    );
    historical_root(
        &fixture,
        &backend,
        "desktop",
        "v10",
        "complete",
        &guest,
        "newer historical view",
    );
    *backend.running_root.borrow_mut() = Some(running);

    let outcome =
        build_with_backend_and_warning(&fixture.store, Some("desktop"), &backend, |_| Ok(()))
            .unwrap()
            .remove(0);

    assert_eq!(
        fs::read_to_string(outcome.root.join(guest.strip_prefix("/").unwrap())).unwrap(),
        "running view"
    );
    assert!(backend.cow_checks.borrow().contains(&guest));
}

/// Use the primary system's completed history when a different system is running.
/// 当前运行其他系统时，使用主系统自己的已完成历史版本。
#[test]
fn primary_system_uses_own_latest_complete_root_when_another_system_is_running() {
    let fixture = Fixture::new();
    fixture.system("desktop", &["linux", "mkinitcpio"]);
    fixture.system("other", &[]);
    primary_system(&fixture);
    let guest = Path::new("/saved");
    inherit_file(&fixture, "desktop", guest);
    let backend = SimulatedBackend::default();
    historical_root(
        &fixture,
        &backend,
        "desktop",
        "v9",
        "complete",
        guest,
        "older desktop",
    );
    let selected = historical_root(
        &fixture,
        &backend,
        "desktop",
        "v10",
        "complete",
        guest,
        "latest desktop",
    );
    *backend.running_root.borrow_mut() = Some(historical_root(
        &fixture,
        &backend,
        "other",
        "v99",
        "complete",
        guest,
        "running other",
    ));

    let outcome = build_with_backend(&fixture.store, Some("desktop"), &backend)
        .unwrap()
        .remove(0);

    assert_eq!(
        fs::read_to_string(outcome.root.join("saved")).unwrap(),
        "latest desktop"
    );
    assert!(
        backend
            .cow_checks
            .borrow()
            .contains(&selected.join("saved"))
    );
}

/// A secondary system uses completed history even if its older root is marked running.
/// 即使子系统的旧根标记为运行中，仍使用其已完成历史版本。
#[test]
fn secondary_system_uses_latest_complete_root_even_when_its_older_root_is_running() {
    let fixture = Fixture::new();
    fixture.system("desktop", &["linux", "mkinitcpio"]);
    fixture.system("worker", &[]);
    primary_system(&fixture);
    let guest = Path::new("/saved");
    inherit_file(&fixture, "worker", guest);
    let backend = SimulatedBackend::default();
    *backend.running_root.borrow_mut() = Some(historical_root(
        &fixture,
        &backend,
        "worker",
        "v9",
        "complete",
        guest,
        "running worker",
    ));
    let selected = historical_root(
        &fixture,
        &backend,
        "worker",
        "v10",
        "complete",
        guest,
        "latest worker",
    );

    let outcome = build_with_backend(&fixture.store, Some("worker"), &backend)
        .unwrap()
        .remove(0);

    assert_eq!(
        fs::read_to_string(outcome.root.join("saved")).unwrap(),
        "latest worker"
    );
    assert!(
        backend
            .cow_checks
            .borrow()
            .contains(&selected.join("saved"))
    );
}

/// Select numeric completed versions independently and skip every unfinished newer state.
/// 独立选择各系统按数字排序的已完成版本，跳过所有更新的未完成状态。
#[test]
fn all_systems_use_own_latest_numeric_complete_version_without_boot_manifests() {
    let fixture = Fixture::new();
    fixture.system("desktop", &["linux", "mkinitcpio"]);
    fixture.system("worker", &[]);
    primary_system(&fixture);
    let guest = Path::new("/saved");
    let backend = SimulatedBackend::default();
    for id in ["desktop", "worker"] {
        inherit_file(&fixture, id, guest);
        for (version, state) in [
            ("v9", "complete"),
            ("v10", "complete"),
            ("v11", "failed"),
            ("v12", "pending-inheritance"),
            ("v13", "cancelled"),
            ("v14", "building"),
        ] {
            let root = historical_root(
                &fixture,
                &backend,
                id,
                version,
                state,
                guest,
                &format!("{id} {version}"),
            );
            assert!(
                !root
                    .parent()
                    .unwrap()
                    .join("inheritance-pending.toml")
                    .exists()
            );
            assert!(!root.parent().unwrap().join("boot.toml").exists());
        }
    }

    let outcomes = build_with_backend(&fixture.store, None, &backend).unwrap();

    assert_eq!(outcomes.len(), 2);
    for outcome in outcomes {
        assert_eq!(
            fs::read_to_string(outcome.root.join("saved")).unwrap(),
            format!("{} v10", outcome.system_id)
        );
        assert!(
            backend.cow_checks.borrow().contains(
                &fixture
                    .store
                    .system_layout(&outcome.system_id)
                    .unwrap()
                    .versions
                    .join("v10/root/saved")
            )
        );
    }
}
