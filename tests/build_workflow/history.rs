//! Exercise independent stage selection across descending numeric build history.
//! 验证各阶段独立按构建数字历史倒序选择缓存。
use super::*;

/// Skip multiple incomplete attempts independently for base and software stages.
/// 基础与软件阶段分别跳过多个未完成尝试。
#[test]
fn stages_find_different_sources_across_multiple_incomplete_attempts() {
    let fixture = Fixture::new();
    fixture.system("desktop", &["editor"]);
    let backend = SimulatedBackend::default();
    for _ in 0..3 {
        build_with_backend(&fixture.store, Some("desktop"), &backend).unwrap();
    }
    for version in ["v00000002", "v00000003"] {
        fs::remove_dir_all(fixture.cache(version, "packages")).unwrap();
        fs::remove_file(fixture.cache(version, "packages.toml")).unwrap();
    }
    backend.events.borrow_mut().clear();
    let outcome = build_with_backend(&fixture.store, Some("desktop"), &backend)
        .unwrap()
        .remove(0);
    assert_eq!(backend.count("bootstrap"), 0);
    assert_eq!(backend.count("update"), 2);
    for (stage, source) in [("base-system", "v00000003"), ("packages", "v00000001")] {
        assert_eq!(
            record(&fixture.cache(&outcome.version, &format!("{stage}.toml")))["source_version"]
                .as_str(),
            Some(source)
        );
    }
    fixture.cleaned(&outcome.version);
}

/// Prefer the nearest numeric version, including the transition beyond eight digits.
/// 优先选择数字序号最近的版本，包括超过八位数的边界。
#[test]
fn cache_search_uses_descending_numbers_instead_of_directory_names() {
    let fixture = Fixture::new();
    fixture.system("desktop", &["editor"]);
    let layout = fixture.store.system_layout("desktop").unwrap();
    fs::write(layout.root.join("version-counter"), "99999998").unwrap();
    let backend = SimulatedBackend::default();
    for _ in 0..2 {
        build_with_backend(&fixture.store, Some("desktop"), &backend).unwrap();
    }
    backend.fail_operation.borrow_mut().replace("update");
    assert!(build_with_backend(&fixture.store, Some("desktop"), &backend).is_err());
    backend.events.borrow_mut().clear();
    let outcome = build_with_backend(&fixture.store, Some("desktop"), &backend)
        .unwrap()
        .remove(0);
    assert_eq!(outcome.version, "v100000002");
    for stage in ["base-system", "packages"] {
        assert_eq!(
            record(&fixture.cache(&outcome.version, &format!("{stage}.toml")))["source_version"]
                .as_str(),
            Some("v100000000")
        );
    }
}

/// Skip incompatible stages but never conceal corrupted completion metadata.
/// 跳过不兼容阶段，但不能掩盖损坏的完成元数据。
#[test]
fn incompatible_stages_are_skipped_but_corrupt_candidates_abort() {
    for corrupt in [false, true] {
        let fixture = Fixture::new();
        fixture.system("desktop", &["editor"]);
        let backend = SimulatedBackend::default();
        for _ in 0..2 {
            build_with_backend(&fixture.store, Some("desktop"), &backend).unwrap();
        }
        let path = fixture.cache("v00000002", "packages.toml");
        let mut value = record(&path);
        value["identity"]["cache_format"] = "incompatible-v2".into();
        fs::write(&path, toml::to_string(&value).unwrap()).unwrap();
        if corrupt {
            fs::write(&path, "broken = [").unwrap();
        }
        backend.events.borrow_mut().clear();
        let result = build_with_backend(&fixture.store, Some("desktop"), &backend);
        if corrupt {
            let error = result.unwrap_err();
            assert!(error.to_string().contains("Cache record"));
            assert!(backend.events.borrow().is_empty());
        } else {
            let outcome = result.unwrap().remove(0);
            assert_eq!(
                record(&fixture.cache(&outcome.version, "packages.toml"))["source_version"]
                    .as_str(),
                Some("v00000001")
            );
            assert_eq!(
                record(&fixture.cache(&outcome.version, "base-system.toml"))["source_version"]
                    .as_str(),
                Some("v00000002")
            );
        }
    }
}

/// A changed latest attempt must not delete a compatible earlier donor during invalidation.
/// 最新尝试配置变化时，失效清理不得删除兼容的更早来源。
#[test]
fn changed_incomplete_attempt_preserves_compatible_historical_donors() {
    let fixture = Fixture::new();
    fixture.system("desktop", &["editor"]);
    let backend = SimulatedBackend::default();
    build_with_backend(&fixture.store, Some("desktop"), &backend).unwrap();
    let layout = fixture.store.system_layout("desktop").unwrap();
    let failed = layout.versions.join("v00000002");
    fs::create_dir_all(failed.join("config/build")).unwrap();
    fs::create_dir(failed.join("config/script")).unwrap();
    let path = failed.join("config/build/config.conf");
    let text = fs::read_to_string(layout.config.join("build/config.conf")).unwrap();
    fs::write(
        &path,
        text.replace(
            "Architecture=x86_64",
            "Architecture=x86_64\nMirror=https://example.invalid/$repo/os/$arch",
        ),
    )
    .unwrap();
    backend.events.borrow_mut().clear();
    let outcome = build_with_backend(&fixture.store, Some("desktop"), &backend)
        .unwrap()
        .remove(0);
    for stage in ["base-system", "packages"] {
        assert!(fixture.cache("v00000001", stage).exists());
        assert_eq!(
            record(&fixture.cache(&outcome.version, &format!("{stage}.toml")))["source_version"]
                .as_str(),
            Some("v00000001")
        );
    }
    assert_eq!(backend.count("bootstrap"), 0);
}
