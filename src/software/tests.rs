//! Verify package maintenance and refuse unsafe or malformed plugin results.
//! 验证软件维护并拒绝不安全或格式错误的插件结果。

use super::{
    InstalledPackage,
    test_support::{Fixture, PackageBackend, previous},
};

/// Keep a removed configured target when another retained target still needs it.
/// 另一保留目标仍需要已移除配置目标时保留该包。
///
/// # Arguments
///
/// No parameters.
/// 无参数。
#[test]
fn maintenance_demotes_removed_target_but_preserves_needed_dependency() {
    let installed = [("base", true), ("editor", true), ("library", true)];
    let fixture = Fixture::new(PackageBackend::new(&installed, &[("editor", &["library"])]));
    let state = fixture
        .session()
        .maintain(
            &["editor".into()],
            &previous(&["editor", "library"], &installed),
        )
        .unwrap();
    assert_eq!(state.requested, ["editor"]);
    assert_eq!(
        fixture.backend.packages.borrow().get("library"),
        Some(&false)
    );
    assert_eq!(fixture.backend.count("remove_orphans"), 0);
    assert_eq!(fixture.backend.count("update"), 1);
    assert_eq!(fixture.backend.calls.borrow()[0].0, "update");
}

/// Delete a removed unneeded target and its dependency chain after demoting the target.
/// 降低已移除且不再需要的目标安装原因后，删除该包和依赖链。
///
/// # Arguments
///
/// No parameters.
/// 无参数。
#[test]
fn maintenance_removes_orphans_after_marking_removed_targets_as_dependencies() {
    let installed = [
        ("base", true),
        ("editor", true),
        ("old-app", true),
        ("old-library", false),
    ];
    let fixture = Fixture::new(PackageBackend::new(
        &installed,
        &[("old-app", &["old-library"])],
    ));
    fixture
        .session()
        .maintain(
            &["editor".into()],
            &previous(&["editor", "old-app"], &installed),
        )
        .unwrap();
    assert!(!fixture.backend.packages.borrow().contains_key("old-app"));
    assert!(
        !fixture
            .backend
            .packages
            .borrow()
            .contains_key("old-library")
    );
    let calls = fixture.backend.calls.borrow();
    let demotion = calls
        .iter()
        .position(|(operation, args)| {
            operation == "mark_reason" && args == &["dependency", "old-app"]
        })
        .unwrap();
    let removal = calls
        .iter()
        .position(|(operation, _)| operation == "remove_orphans")
        .unwrap();
    assert!(demotion < removal);
    assert_eq!(calls[removal].1, ["old-app"]);
}

/// Reinstall an unchanged desired target when the actual database loses it during update.
/// 更新期间实际数据库丢失未改变的所需目标时重新安装该目标。
///
/// # Arguments
///
/// No parameters.
/// 无参数。
#[test]
fn maintenance_installs_unchanged_targets_missing_from_actual_inventory() {
    let installed = [("base", true), ("editor", true), ("library", false)];
    let mut backend = PackageBackend::new(&installed, &[("editor", &["library"])]);
    backend.remove_on_update = vec!["editor".into()];
    let fixture = Fixture::new(backend);
    let state = fixture
        .session()
        .maintain(&["editor".into()], &previous(&["editor"], &installed))
        .unwrap();
    assert!(state.installed.contains(&InstalledPackage {
        name: "editor".into(),
        explicit: true,
    }));
    assert!(
        fixture
            .backend
            .calls
            .borrow()
            .iter()
            .any(|(operation, args)| operation == "install" && args == &["editor"])
    );
    assert_eq!(fixture.backend.count("update"), 1);
}

/// Remove an unneeded replacement introduced by update while preserving unchanged manual roots.
/// 删除更新引入但不再需要的替代包，同时保留未改变的手动安装根。
///
/// # Arguments
///
/// No parameters.
/// 无参数。
#[test]
fn maintenance_demotes_replacement_names_and_preserves_existing_unmanaged_explicit_packages() {
    let installed = [
        ("base", true),
        ("editor", true),
        ("old-app", true),
        ("old-library", false),
        ("manual", true),
    ];
    let mut backend = PackageBackend::new(&installed, &[("new-app", &["old-library"])]);
    backend.replace_on_update = vec![("old-app".into(), "new-app".into())];
    let fixture = Fixture::new(backend);
    fixture
        .session()
        .maintain(
            &["editor".into()],
            &previous(&["editor", "old-app"], &installed),
        )
        .unwrap();
    assert!(!fixture.backend.packages.borrow().contains_key("old-app"));
    assert!(!fixture.backend.packages.borrow().contains_key("new-app"));
    assert!(
        !fixture
            .backend
            .packages
            .borrow()
            .contains_key("old-library")
    );
    assert_eq!(fixture.backend.packages.borrow().get("manual"), Some(&true));
    assert!(
        fixture
            .backend
            .calls
            .borrow()
            .iter()
            .any(|(operation, args)| operation == "mark_reason"
                && args == &["dependency", "new-app"])
    );
}

/// Retain a replacement as a dependency when a current target still requires the new name.
/// 当前目标仍需要替代包的新名称时，以依赖原因保留该包。
///
/// # Arguments
///
/// No parameters.
/// 无参数。
#[test]
fn maintenance_retains_needed_replacement_packages_as_dependencies() {
    let installed = [("base", true), ("editor", true), ("old-library", true)];
    let mut backend = PackageBackend::new(&installed, &[("editor", &["new-library"])]);
    backend.replace_on_update = vec![("old-library".into(), "new-library".into())];
    let fixture = Fixture::new(backend);
    fixture
        .session()
        .maintain(
            &["editor".into()],
            &previous(&["editor", "old-library"], &installed),
        )
        .unwrap();
    assert_eq!(
        fixture.backend.packages.borrow().get("new-library"),
        Some(&false)
    );
    assert_eq!(fixture.backend.count("remove_orphans"), 0);
}

/// First installation must preserve unrelated explicit packages already present in the base root.
/// 首次安装必须保留基础根中已存在且无关的显式安装包。
///
/// # Arguments
///
/// No parameters.
/// 无参数。
#[test]
fn first_install_preserves_unrelated_explicit_packages() {
    let fixture = Fixture::new(PackageBackend::new(
        &[("base", true), ("manual", true)],
        &[],
    ));
    fixture.session().install(&[]).unwrap();
    assert_eq!(fixture.backend.packages.borrow().get("manual"), Some(&true));
    assert_eq!(fixture.backend.count("remove_orphans"), 0);
    assert!(
        !fixture
            .backend
            .calls
            .borrow()
            .iter()
            .any(|(operation, args)| {
                operation == "mark_reason"
                    && args.first().is_some_and(|reason| reason == "dependency")
            })
    );
}

/// First software installation restores protected targets without repeating the system update.
/// 首次软件安装恢复受保护目标且不重复系统更新。
///
/// # Arguments
///
/// No parameters.
/// 无参数。
#[test]
fn first_install_has_no_update_and_restores_missing_protected_target() {
    let fixture = Fixture::new(PackageBackend::new(&[], &[("editor", &["library"])]));
    let state = fixture
        .session()
        .install(&["editor".into(), "editor".into()])
        .unwrap();
    assert_eq!(state.requested, ["editor"]);
    assert_eq!(fixture.backend.count("update"), 0);
    assert_eq!(fixture.backend.packages.borrow().get("base"), Some(&true));
    assert_eq!(fixture.backend.packages.borrow().get("editor"), Some(&true));
    assert_eq!(
        fixture.backend.packages.borrow().get("library"),
        Some(&false)
    );
}

/// Removing base from configured targets must preserve its protected explicit installation.
/// 从配置目标移除 base 后仍必须保留其受保护显式安装状态。
///
/// # Arguments
///
/// No parameters.
/// 无参数。
#[test]
fn maintenance_never_demotes_protected_base_target() {
    let installed = [("base", true)];
    let fixture = Fixture::new(PackageBackend::new(&installed, &[]));
    let state = fixture
        .session()
        .maintain(&[], &previous(&["base"], &installed))
        .unwrap();
    assert!(state.requested.is_empty());
    assert_eq!(fixture.backend.packages.borrow().get("base"), Some(&true));
    assert!(
        !fixture
            .backend
            .calls
            .borrow()
            .iter()
            .any(|(operation, args)| {
                operation == "mark_reason"
                    && args.first().is_some_and(|reason| reason == "dependency")
            })
    );
}

/// Reject corrupt installed inventory before any package mutation is attempted.
/// 尝试任何包变更前拒绝损坏的已安装清单。
///
/// # Arguments
///
/// No parameters.
/// 无参数。
#[test]
fn malformed_inventory_cannot_trigger_installation_or_cleanup() {
    for output in [
        "base explicit\n",
        "base\tunknown\n",
        "base\texplicit\nbase\tdependency\n",
        "../escape\texplicit\n",
    ] {
        let mut backend = PackageBackend::new(&[("base", true)], &[]);
        backend.inventory_override = Some(output.into());
        let fixture = Fixture::new(backend);
        assert!(fixture.session().install(&["editor".into()]).is_err());
        assert_eq!(fixture.backend.count("install"), 0);
        assert_eq!(fixture.backend.count("mark_reason"), 0);
        assert_eq!(fixture.backend.count("remove_orphans"), 0);
    }
}

/// Never delete protected, explicit, absent, duplicate, or malformed alleged orphans.
/// 不删除被错误报告为孤儿的受保护包、显式包、缺失包、重复包或格式错误包。
///
/// # Arguments
///
/// No parameters.
/// 无参数。
#[test]
fn invalid_orphan_results_are_rejected_before_any_removal() {
    for output in [
        "base\n",
        "manual\n",
        "missing\n",
        "unused\nunused\n",
        "../escape\n",
    ] {
        let installed = [("base", true), ("manual", true), ("unused", false)];
        let mut backend = PackageBackend::new(&installed, &[]);
        backend.orphan_override = Some(output.into());
        let fixture = Fixture::new(backend);
        assert!(
            fixture.session().install(&[]).is_err(),
            "accepted {output:?}"
        );
        assert_eq!(fixture.backend.count("remove_orphans"), 0);
        assert_eq!(fixture.backend.packages.borrow().len(), installed.len());
    }
}

/// Reject incomplete or ambiguous target resolution before mutating the actual package database.
/// 变更实际包数据库前拒绝不完整或不明确的目标解析结果。
///
/// # Arguments
///
/// No parameters.
/// 无参数。
#[test]
fn target_resolution_requires_one_concrete_result_per_target() {
    for output in ["", "editor\nlibrary\n", "editor\neditor\n", "bad name\n"] {
        let mut backend = PackageBackend::new(&[("base", true)], &[]);
        backend.resolution_override = Some(output.into());
        let fixture = Fixture::new(backend);
        assert!(fixture.session().install(&["editor".into()]).is_err());
        assert_eq!(fixture.backend.count("query_packages"), 0);
        assert_eq!(fixture.backend.count("install"), 0);
    }
}

/// Refuse invalid previous cache state before even performing its maintenance update.
/// 连维护更新也尚未执行前拒绝无效过往缓存状态。
///
/// # Arguments
///
/// No parameters.
/// 无参数。
#[test]
fn invalid_previous_targets_do_not_trigger_maintenance_update() {
    for state in [
        previous(&["editor"], &[("base", true)]),
        previous(&["editor"], &[("base", true), ("editor", false)]),
        previous(&["editor", "editor"], &[("editor", true)]),
    ] {
        let fixture = Fixture::new(PackageBackend::new(&[("base", true)], &[]));
        assert!(fixture.session().maintain(&[], &state).is_err());
        assert!(fixture.backend.calls.borrow().is_empty());
    }
}
