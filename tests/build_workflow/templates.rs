//! Verify shared template creation without changing cached-system maintenance.
//! 验证共享模板创建，同时保持有缓存系统的维护行为。
use super::*;
use std::{
    io::Read,
    path::{Path, PathBuf},
};

/// Find this fixture's published templates without inspecting per-system caches.
/// 不检查每系统缓存，查找此夹具已发布的模板。
/// # Arguments
///
/// - `fixture`: Isolated manager owning templates.
///   持有模板的隔离管理器。
fn completed(fixture: &Fixture) -> Vec<PathBuf> {
    let mut result = Vec::new();
    let directory = fixture.store.templates_path();
    if !directory.exists() {
        return result;
    }
    for hash in fs::read_dir(directory).unwrap() {
        for arch in fs::read_dir(hash.unwrap().path()).unwrap() {
            let path = arch.unwrap().path().join("completed");
            if path.exists() {
                result.push(path);
            }
        }
    }
    result.sort();
    result
}

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
fn replace_resource(fixture: &Fixture, name: &str, contents: &[u8]) {
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

/// Verify different systems share a neutral base while retaining independent machine identities.
/// 验证不同系统共享无身份基础，同时保留独立机器身份。
/// # Arguments
///
/// No parameters.
/// 无参数。
#[test]
fn systems_share_template_but_not_identity_or_user_packages() {
    let fixture = Fixture::new();
    fixture.system("desktop", &["old"]);
    fixture.system("server", &["new"]);
    let server_config = fixture.store.system_layout("server").unwrap().config;
    fs::write(
        server_config.join("subsystem/config.toml"),
        "schema_version=1\ncustom=true\n",
    )
    .unwrap();
    let backend = SimulatedBackend::default();
    let roots = build_with_backend(&fixture.store, None, &backend).unwrap();
    assert_eq!(backend.count("bootstrap"), 1);
    assert_eq!(backend.count("base_install"), 1);
    assert_eq!(backend.count("template_clean"), 1);
    assert_eq!(backend.count("system_init"), 2);
    assert_eq!(backend.count("update"), 1);
    let templates = completed(&fixture);
    assert_eq!(templates.len(), 1);
    let template = &templates[0];
    backend
        .check_subvolume(&template.join("root"), true)
        .unwrap();
    assert_eq!(
        fs::read_to_string(template.join("root/etc/machine-id")).unwrap(),
        ""
    );
    assert_eq!(SimulatedBackend::inventory(&template.join("root")).len(), 2);
    let marker = record(&template.join("template.toml"));
    assert_eq!(
        marker["plugin_sha256"].as_str().unwrap(),
        template
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .file_name()
            .unwrap()
    );
    assert_eq!(marker["architecture"].as_str(), Some("x86_64"));
    assert!(
        backend
            .events
            .borrow()
            .iter()
            .filter(|event| event.system_id == "template")
            .all(|event| event.packages.is_empty()
                && event.root.starts_with(fixture.store.templates_path()))
    );
    let ids: Vec<_> = roots
        .iter()
        .map(|outcome| fs::read_to_string(outcome.root.join("etc/machine-id")).unwrap())
        .collect();
    assert_ne!(ids[0], ids[1]);
    for outcome in &roots {
        let identity = record(
            &fixture
                .store
                .system_layout(&outcome.system_id)
                .unwrap()
                .root
                .join("identity.toml"),
        );
        assert_eq!(
            identity["machine_id"].as_str().unwrap(),
            fs::read_to_string(outcome.root.join("etc/machine-id"))
                .unwrap()
                .trim()
        );
        let other = if outcome.system_id == "desktop" {
            "new"
        } else {
            "old"
        };
        assert!(!SimulatedBackend::inventory(&outcome.root).contains_key(other));
        assert!(backend.snapshots.borrow().iter().any(|snapshot| {
            !snapshot.readonly
                && snapshot.source == template.join("root")
                && snapshot
                    .destination
                    .ends_with(format!("work/{}/v00000001/base-root", outcome.system_id))
        }));
    }
    let original_id = ids[0].clone();
    fs::remove_dir_all(fixture.cache("v00000001", "base-system")).unwrap();
    fs::remove_dir_all(fixture.cache("v00000001", "packages")).unwrap();
    backend.events.borrow_mut().clear();
    let recreated = build_with_backend(&fixture.store, Some("desktop"), &backend)
        .unwrap()
        .remove(0);
    assert_eq!(
        fs::read_to_string(recreated.root.join("etc/machine-id")).unwrap(),
        original_id
    );
    assert_eq!(backend.count("bootstrap"), 0);
    assert_eq!(backend.count("system_init"), 1);
}

/// Verify all archive resources affect new templates while existing cache maintenance is unchanged.
/// 验证所有归档资源影响新模板，而已有缓存维护保持不变。
/// # Arguments
///
/// No parameters.
/// 无参数。
#[test]
fn resource_hash_change_rebuilds_new_system_only() {
    let fixture = Fixture::new();
    fixture.system("desktop", &["editor"]);
    let backend = SimulatedBackend::default();
    let first = build_with_backend(&fixture.store, Some("desktop"), &backend)
        .unwrap()
        .remove(0);
    let original_id = fs::read_to_string(first.root.join("etc/machine-id")).unwrap();
    let old_template = completed(&fixture).remove(0);
    replace_resource(
        &fixture,
        "data/new-helper-resource",
        b"changed plugin resource",
    );
    backend.events.borrow_mut().clear();
    let maintained = build_with_backend(&fixture.store, Some("desktop"), &backend)
        .unwrap()
        .remove(0);
    assert_eq!(backend.count("bootstrap"), 0);
    assert_eq!(backend.count("system_init"), 0);
    assert_eq!(backend.count("update"), 1);
    assert_eq!(
        completed(&fixture).as_slice(),
        std::slice::from_ref(&old_template)
    );
    assert_eq!(
        fs::read_to_string(maintained.root.join("etc/machine-id")).unwrap(),
        original_id
    );
    fixture.system("server", &[]);
    backend.events.borrow_mut().clear();
    build_with_backend(&fixture.store, Some("server"), &backend).unwrap();
    assert_eq!(backend.count("bootstrap"), 1);
    assert_eq!(backend.count("base_install"), 1);
    assert_eq!(completed(&fixture).len(), 2);
    assert_eq!(
        fs::read_to_string(old_template.join("root/.base-generation")).unwrap(),
        "1"
    );
}

/// Verify a multi-architecture archive never shares a root between target architectures.
/// 验证支持多架构的归档绝不在目标架构之间共享根。
/// # Arguments
///
/// No parameters.
/// 无参数。
#[test]
fn same_archive_is_partitioned_by_architecture() {
    let fixture = Fixture::new();
    let metadata = fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("presets/plugins/arch/plugin.toml"),
    )
    .unwrap();
    replace_resource(
        &fixture,
        "plugin.toml",
        metadata
            .replace(
                "architectures = [\"x86_64\"]",
                "architectures = [\"x86_64\", \"aarch64\"]",
            )
            .as_bytes(),
    );
    fixture.system("desktop", &[]);
    fixture.system("server", &[]);
    fs::write(
        fixture
            .store
            .system_layout("server")
            .unwrap()
            .config
            .join("build/config.toml"),
        "schema_version=1\nsystem_type='arch'\narchitecture='aarch64'\npackages=[]\n",
    )
    .unwrap();
    let backend = SimulatedBackend::default();
    build_with_backend(&fixture.store, None, &backend).unwrap();
    assert_eq!(backend.count("bootstrap"), 2);
    let templates = completed(&fixture);
    assert_eq!(templates.len(), 2);
    assert_eq!(
        templates[0].parent().unwrap().parent(),
        templates[1].parent().unwrap().parent()
    );
    assert_ne!(
        record(&templates[0].join("template.toml"))["architecture"],
        record(&templates[1].join("template.toml"))["architecture"]
    );
}

/// Verify legacy roots supply this same system's ID when no identity record exists.
/// 验证没有身份记录时，旧根提供同一系统自己的 ID。
/// # Arguments
///
/// No parameters.
/// 无参数。
#[test]
fn legacy_system_identity_is_adopted_during_creation() {
    let fixture = Fixture::new();
    fixture.system("desktop", &[]);
    let backend = SimulatedBackend::default();
    let first = build_with_backend(&fixture.store, Some("desktop"), &backend)
        .unwrap()
        .remove(0);
    let old_id = fs::read_to_string(first.root.join("etc/machine-id")).unwrap();
    let identity_path = fixture
        .store
        .system_layout("desktop")
        .unwrap()
        .root
        .join("identity.toml");
    fs::remove_file(&identity_path).unwrap();
    fs::remove_dir_all(fixture.cache("v00000001", "base-system")).unwrap();
    fs::remove_dir_all(fixture.cache("v00000001", "packages")).unwrap();
    let second = build_with_backend(&fixture.store, Some("desktop"), &backend)
        .unwrap()
        .remove(0);
    assert_eq!(
        fs::read_to_string(second.root.join("etc/machine-id")).unwrap(),
        old_id
    );
    assert_eq!(
        record(&identity_path)["machine_id"].as_str(),
        Some(old_id.trim())
    );
}

/// Verify failed cleaning publishes nothing and the next attempt builds a fresh template.
/// 验证清理失败不发布模板，下一次尝试重新构建模板。
/// # Arguments
///
/// No parameters.
/// 无参数。
#[test]
fn incomplete_template_is_never_reused() {
    let fixture = Fixture::new();
    fixture.system("desktop", &[]);
    let backend = SimulatedBackend::default();
    *backend.fail_operation.borrow_mut() = Some("template_clean");
    assert!(build_with_backend(&fixture.store, Some("desktop"), &backend).is_err());
    assert!(completed(&fixture).is_empty());
    assert_eq!(backend.count("system_init"), 0);
    assert!(!fixture.cache("v00000001", "base-system.toml").exists());
    backend.events.borrow_mut().clear();
    build_with_backend(&fixture.store, Some("desktop"), &backend).unwrap();
    assert_eq!(backend.count("bootstrap"), 1);
    assert_eq!(completed(&fixture).len(), 1);
    assert!(
        fs::read_dir(completed(&fixture)[0].parent().unwrap())
            .unwrap()
            .all(|entry| !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with("template-pending-"))
    );
}

/// Verify initialization failure keeps the shared template without committing a system base.
/// 验证初始化失败保留共享模板，而不提交系统基础缓存。
/// # Arguments
///
/// No parameters.
/// 无参数。
#[test]
fn failed_system_initialization_keeps_template_and_identity_for_retry() {
    let fixture = Fixture::new();
    fixture.system("desktop", &[]);
    let backend = SimulatedBackend::default();
    *backend.fail_operation.borrow_mut() = Some("system_init");
    assert!(build_with_backend(&fixture.store, Some("desktop"), &backend).is_err());
    assert_eq!(completed(&fixture).len(), 1);
    assert!(!fixture.cache("v00000001", "base-system.toml").exists());
    let identity_path = fixture
        .store
        .system_layout("desktop")
        .unwrap()
        .root
        .join("identity.toml");
    let identity = fs::read_to_string(&identity_path).unwrap();
    fixture.cleaned("v00000001");
    backend.events.borrow_mut().clear();
    build_with_backend(&fixture.store, Some("desktop"), &backend).unwrap();
    assert_eq!(backend.count("bootstrap"), 0);
    assert_eq!(backend.count("system_init"), 1);
    assert_eq!(fs::read_to_string(identity_path).unwrap(), identity);
}

/// Verify malformed and missing markers or incorrect root properties stop reuse.
/// 验证损坏或缺失标记及错误根属性阻止复用。
/// # Arguments
///
/// No parameters.
/// 无参数。
#[test]
fn invalid_published_templates_are_rejected_before_initialization() {
    for damage in ["marker", "missing-marker", "hash", "readonly"] {
        let fixture = Fixture::new();
        fixture.system("desktop", &[]);
        let backend = SimulatedBackend::default();
        build_with_backend(&fixture.store, Some("desktop"), &backend).unwrap();
        let template = completed(&fixture).remove(0);
        let marker = template.join("template.toml");
        match damage {
            "marker" => fs::write(&marker, "broken=[").unwrap(),
            "missing-marker" => fs::remove_file(&marker).unwrap(),
            "hash" => {
                let mut value = record(&marker);
                value["plugin_sha256"] = "0".repeat(64).into();
                fs::write(&marker, toml::to_string(&value).unwrap()).unwrap();
            }
            "readonly" => fs::write(template.join("root").join(MARKER), "writable").unwrap(),
            _ => unreachable!(),
        }
        fixture.system("server", &[]);
        backend.events.borrow_mut().clear();
        assert!(build_with_backend(&fixture.store, Some("server"), &backend).is_err());
        assert!(backend.events.borrow().is_empty());
    }
}

/// Verify lock contention never creates a competing template or executes a plugin.
/// 验证锁争用不会创建竞争模板或执行插件。
/// # Arguments
///
/// No parameters.
/// 无参数。
#[test]
fn template_lock_protects_reuse_and_creation() {
    let fixture = Fixture::new();
    fixture.system("desktop", &[]);
    let backend = SimulatedBackend::default();
    build_with_backend(&fixture.store, Some("desktop"), &backend).unwrap();
    let template = completed(&fixture).remove(0);
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(template.parent().unwrap().join("build.lock"))
        .unwrap();
    lock.try_lock().unwrap();
    fixture.system("server", &[]);
    backend.events.borrow_mut().clear();
    let error = build_with_backend(&fixture.store, Some("server"), &backend).unwrap_err();
    assert!(error.to_string().contains("模板构建锁"));
    assert!(backend.events.borrow().is_empty());
    lock.unlock().unwrap();
    build_with_backend(&fixture.store, Some("server"), &backend).unwrap();
    assert_eq!(backend.count("bootstrap"), 0);
}

/// Verify templates cannot overlap the configured scratch workspace.
/// 验证模板不能与配置的临时工作目录重叠。
/// # Arguments
///
/// No parameters.
/// 无参数。
#[test]
fn template_directory_is_permanent_data() {
    let fixture = Fixture::new();
    fixture.system("desktop", &[]);
    fs::write(
        fixture.store.global_config_path(),
        "schema_version=1\nwork_dir='templates/work'\n",
    )
    .unwrap();
    let backend = SimulatedBackend::default();
    let error = build_with_backend(&fixture.store, Some("desktop"), &backend).unwrap_err();
    assert!(error.to_string().contains("work_dir 必须独立"));
    assert!(backend.events.borrow().is_empty());
}

/// Verify API 2 remains usable for maintenance, but cannot create identity-neutral templates.
/// 验证 API 2 仍可用于维护，但不能创建不依附身份的模板。
/// # Arguments
///
/// No parameters.
/// 无参数。
#[test]
fn api_two_keeps_maintenance_support() {
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
    assert_eq!(backend.count("update"), 1);
    assert_eq!(backend.count("system_init"), 0);
    fixture.system("server", &[]);
    backend.events.borrow_mut().clear();
    let error = build_with_backend(&fixture.store, Some("server"), &backend).unwrap_err();
    assert!(error.to_string().contains("API 3"));
    assert!(backend.events.borrow().is_empty());
}
