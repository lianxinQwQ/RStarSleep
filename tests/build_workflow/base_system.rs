//! Verify independent base roots, unconditional maintenance, and cache invalidation.
//! 验证独立基础根、固定维护和缓存失效清理。
use super::*;
use std::{io::Read, path::Path};

/// Replace or add one plugin resource, retaining all other archive entries.
/// 替换或添加一个插件资源，保留所有其他归档条目。
/// # Arguments
///
/// - `fixture`: Manager containing the preset tar archive.
///   包含预设 tar 归档的管理器。
/// - `name`: Resource path relative to the plugin root.
///   相对于插件根的资源路径。
/// - `contents`: New complete file contents.
///   新的完整文件内容。
pub(super) fn replace_resource(fixture: &Fixture, name: &str, contents: &[u8]) {
    let adjusted;
    let contents = if name == "plugin.toml" {
        adjusted = String::from_utf8(contents.to_vec())
            .unwrap()
            .replace("predownload = true", "predownload = false");
        adjusted.as_bytes()
    } else {
        contents
    };
    let path = fixture.store.plugins_path().join("arch.tar");
    let mut input = tar::Archive::new(fs::File::open(&path).unwrap());
    let mut entries = Vec::new();
    for entry in input.entries().unwrap() {
        let mut entry = entry.unwrap();
        let name = entry.path().unwrap().into_owned();
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes).unwrap();
        entries.push((name, entry.header().clone(), bytes));
    }
    drop(input);
    let mut output = tar::Builder::new(fs::File::create(&path).unwrap());
    let mut replaced = false;
    for (path, mut header, mut bytes) in entries {
        if path.strip_prefix(".").unwrap_or(&path) == Path::new(name) {
            bytes = contents.to_vec();
            header.set_size(bytes.len() as u64);
            replaced = true;
        }
        output
            .append_data(&mut header, path, bytes.as_slice())
            .unwrap();
    }
    if !replaced {
        let mut header = tar::Header::new_gnu();
        header.set_mode(0o644);
        header.set_size(contents.len() as u64);
        output.append_data(&mut header, name, contents).unwrap();
    }
    output.finish().unwrap();
}

/// Verify new systems install independent roots and always maintain after installation.
/// 验证新系统安装独立根，并始终在安装后执行维护。
/// # Arguments
/// No parameters.
/// 无参数。
#[test]
fn systems_install_independently_and_preserve_their_own_identity() {
    let fixture = Fixture::new();
    fixture.system("desktop", &["editor"]);
    fixture.system("server", &["server"]);
    let backend = SimulatedBackend::default();
    let outcomes = build_with_backend(&fixture.store, None, &backend).unwrap();
    assert_eq!(backend.count("bootstrap"), 2);
    assert_eq!(backend.count("base_install"), 2);
    assert_eq!(backend.count("system_init"), 2);
    assert_eq!(backend.count("update"), 2);
    assert!(!fixture.store.root().join("templates").exists());
    let identities: Vec<_> = outcomes
        .iter()
        .map(|outcome| fs::read_to_string(outcome.root.join("etc/machine-id")).unwrap())
        .collect();
    assert_ne!(identities[0], identities[1]);
    assert_ne!(
        fs::read_to_string(outcomes[0].root.join(".base-generation")).unwrap(),
        fs::read_to_string(outcomes[1].root.join(".base-generation")).unwrap()
    );
    for id in ["desktop", "server"] {
        let events = backend.events.borrow();
        let operations: Vec<_> = events
            .iter()
            .filter(|event| event.system_id == id)
            .map(|event| event.operation.as_str())
            .collect();
        assert_eq!(
            &operations[..4],
            ["bootstrap", "base_install", "system_init", "update"]
        );
    }
    backend.events.borrow_mut().clear();
    let second = build_with_backend(&fixture.store, Some("desktop"), &backend)
        .unwrap()
        .remove(0);
    assert_eq!(backend.count("bootstrap"), 0);
    assert_eq!(backend.count("update"), 2);
    assert_eq!(
        fs::read_to_string(second.root.join("etc/machine-id")).unwrap(),
        identities[0]
    );
}

/// Verify changed base inputs delete every historical downstream cache before a fresh attempt.
/// 验证基础输入变化在重新尝试前删除所有历史后续缓存。
/// # Arguments
/// No parameters.
/// 无参数。
#[test]
fn base_configuration_change_clears_all_stage_caches_but_keeps_versions() {
    let fixture = Fixture::new();
    fixture.system("desktop", &["editor"]);
    let backend = SimulatedBackend::default();
    let first = build_with_backend(&fixture.store, Some("desktop"), &backend)
        .unwrap()
        .remove(0);
    let second = build_with_backend(&fixture.store, Some("desktop"), &backend)
        .unwrap()
        .remove(0);
    let layout = fixture.store.system_layout("desktop").unwrap();
    let path = layout.config.join("build/config.conf");
    let text = fs::read_to_string(&path).unwrap().replace(
        "Architecture=x86_64",
        "Architecture=x86_64\nMirror=https://example.invalid/$repo/os/$arch",
    );
    fs::write(path, text).unwrap();
    backend.events.borrow_mut().clear();
    backend.fail_operation.borrow_mut().replace("bootstrap");
    assert!(build_with_backend(&fixture.store, Some("desktop"), &backend).is_err());
    assert_eq!(backend.count("bootstrap"), 1);
    for version in [&first.version, &second.version] {
        for stage in ["base-system", "packages", "script"] {
            assert!(!fixture.cache(version, stage).exists());
            assert!(!fixture.cache(version, &format!("{stage}.toml")).exists());
        }
    }
    assert!(first.root.exists());
    assert!(second.root.exists());
    backend.events.borrow_mut().clear();
    let retried = build_with_backend(&fixture.store, Some("desktop"), &backend)
        .unwrap()
        .remove(0);
    assert_eq!(backend.count("bootstrap"), 1);
    assert_eq!(backend.count("update"), 1);
    assert_eq!(
        fs::read_to_string(retried.root.join("etc/machine-id")).unwrap(),
        fs::read_to_string(first.root.join("etc/machine-id")).unwrap()
    );
}

/// Verify base maintenance failure blocks software installation even after successful bootstrap.
/// 验证引导成功后基础维护失败仍阻止软件安装。
/// # Arguments
/// No parameters.
/// 无参数。
#[test]
fn newly_installed_base_must_complete_maintenance_before_caching() {
    let fixture = Fixture::new();
    fixture.system("desktop", &["editor"]);
    let backend = SimulatedBackend::default();
    backend.fail_operation.borrow_mut().replace("update");
    assert!(build_with_backend(&fixture.store, Some("desktop"), &backend).is_err());
    assert_eq!(backend.count("base_install"), 1);
    assert_eq!(backend.count("update"), 1);
    assert_eq!(backend.count("install"), 0);
    assert!(!fixture.cache("v00000001", "base-system.toml").exists());
    fixture.cleaned("v00000001");
}

/// Verify missing required and undeclared options abort before any plugin or download operation.
/// 验证必选设置缺失和未声明设置均在插件或下载操作前中止。
/// # Arguments
/// No parameters.
/// 无参数。
#[test]
fn plugin_configuration_contract_is_checked_before_work() {
    for settings in ["", "Channel=", "Channel=stable\nChanel=typo"] {
        let fixture = Fixture::with_predownload(true);
        fixture.system("desktop", &[]);
        let metadata = fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("presets/plugins/arch/plugin.toml"),
        )
        .unwrap();
        replace_resource(
            &fixture,
            "plugin.toml",
            metadata
                .replace(
                    "required = []",
                    "required = [\"RSTAR_DISTRIBUTION_CHANNEL\"]",
                )
                .as_bytes(),
        );
        let layout = fixture.store.system_layout("desktop").unwrap();
        let path = layout.config.join("build/config.conf");
        let text = fs::read_to_string(&path).unwrap().replace(
            "Architecture=x86_64",
            &format!("Architecture=x86_64\n{settings}"),
        );
        fs::write(path, text).unwrap();
        let backend = SimulatedBackend::default();
        let error = build_with_backend(&fixture.store, Some("desktop"), &backend).unwrap_err();
        assert!(error.to_string().contains("配置不满足声明"));
        assert!(backend.events.borrow().is_empty());
        assert!(
            !fixture
                .store
                .persistent_path()
                .join("arch-bootstrap")
                .exists()
        );
    }
}

/// Verify an API 2 plugin can still maintain existing roots but needs API 3 to install a new one.
/// 验证 API 2 插件仍可维护已有根，而安装新根需要 API 3。
/// # Arguments
/// No parameters.
/// 无参数。
#[test]
fn api_two_keeps_existing_cache_maintenance() {
    let fixture = Fixture::new();
    fixture.system("desktop", &[]);
    let backend = SimulatedBackend::default();
    build_with_backend(&fixture.store, Some("desktop"), &backend).unwrap();
    let metadata = fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("presets/plugins/arch/plugin.toml"),
    )
    .unwrap();
    replace_resource(
        &fixture,
        "plugin.toml",
        metadata
            .replace("api_version = 3", "api_version = 2")
            .as_bytes(),
    );
    backend.events.borrow_mut().clear();
    build_with_backend(&fixture.store, Some("desktop"), &backend).unwrap();
    assert_eq!(backend.count("bootstrap"), 0);
    assert_eq!(backend.count("update"), 2);
    fixture.system("server", &[]);
    backend.events.borrow_mut().clear();
    let error = build_with_backend(&fixture.store, Some("server"), &backend).unwrap_err();
    assert!(error.to_string().contains("API 3"));
    assert!(backend.events.borrow().is_empty());
}
