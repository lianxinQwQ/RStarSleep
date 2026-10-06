//! Exercise discovery, wrapper normalization, and archive rejection without running plugins.
//! 不运行插件，验证查找、包装目录归一化和非法归档拒绝。

use super::validation::validate_metadata;
use super::*;
use std::{fs, io::Write, path::Path};

/// Create a minimal plugin tar stream with an optional wrapper and test entries.
/// 创建包含可选包装目录和测试条目的最小插件 tar 数据流。
///
/// # Arguments
///
/// - `wrapper`: Optional directory prefix placed above all plugin resources.
///   放在全部插件资源之上的可选目录前缀。
/// - `extra`: Additional raw entry path to exercise validation, or none.
///   用于验证路径校验的额外原始条目路径，或无。
/// - `api_version`: Task interface version written into metadata.
///   写入元数据的任务接口版本。
fn tar_bytes(wrapper: Option<&str>, extra: Option<&str>, api_version: u32) -> Vec<u8> {
    let metadata = format!(
        "schema_version=1\napi_version={api_version}\nid='test-plugin'\nversion='1'\nsystem_types=['arch']\nentry='plugin.sh'\narchitectures=['x86_64']\ncache_format='test-v1'\n[tools]\nhost=[]\nsystem=[]\n"
    );
    let mut archive = tar::Builder::new(Vec::new());
    for (name, contents) in [
        ("plugin.toml", metadata.as_bytes()),
        ("plugin.sh", b"#!/bin/bash\nexit 0\n".as_slice()),
        (".hidden", b"retained".as_slice()),
        ("bin/helper", b"tool".as_slice()),
    ] {
        let path = wrapper.map_or_else(|| name.to_owned(), |wrapper| format!("{wrapper}/{name}"));
        let mut header = tar::Header::new_gnu();
        header.set_size(contents.len() as u64);
        header.set_mode(0o755);
        header.set_cksum();
        archive.append_data(&mut header, path, contents).unwrap();
    }
    if let Some(path) = extra {
        let mut header = tar::Header::new_gnu();
        header.as_mut_bytes()[..100].fill(0);
        header.as_mut_bytes()[..path.len()].copy_from_slice(path.as_bytes());
        header.set_entry_type(tar::EntryType::Regular);
        header.set_size(1);
        header.set_mode(0o644);
        header.set_cksum();
        archive.append(&header, b"x".as_slice()).unwrap();
    }
    archive.into_inner().unwrap()
}

/// Write a gzip fixture with a chosen archive layout and interface version.
/// 写入指定归档布局和接口版本的 gzip 测试包。
///
/// # Arguments
///
/// - `path`: Destination archive path.
///   目标压缩包路径。
/// - `wrapper`: Optional top-level wrapper directory.
///   可选顶层包装目录。
/// - `extra`: Optional additional archive entry.
///   可选额外归档条目。
/// - `api_version`: API version embedded in the fixture metadata.
///   测试元数据内的 API 版本。
fn fixture(path: &Path, wrapper: Option<&str>, extra: Option<&str>, api_version: u32) {
    let file = fs::File::create(path).unwrap();
    let mut gzip = flate2::write::GzEncoder::new(file, flate2::Compression::default());
    gzip.write_all(&tar_bytes(wrapper, extra, api_version))
        .unwrap();
    gzip.finish().unwrap();
}

/// Verify direct and single-wrapper packages retain resources and clean up on drop.
/// 验证直接根及单层包装包保留资源，并在释放时清理临时文件。
///
/// # Arguments
///
/// No parameters.
/// 无参数。
#[test]
fn normalizes_roots_and_retains_hidden_files_and_tools() {
    let temporary = tempfile::tempdir().unwrap();
    for wrapper in [None, Some("arch-release")] {
        let path = temporary.path().join("arbitrary-name.tar.gz");
        fixture(&path, wrapper, None, 1);
        let plugin = load(&path).unwrap();
        assert_eq!(plugin.root().file_name().unwrap(), "plugin");
        assert_eq!(
            fs::read(plugin.root().join(".hidden")).unwrap(),
            b"retained"
        );
        assert_eq!(fs::read(plugin.root().join("bin/helper")).unwrap(), b"tool");
        assert!(plugin.entry_path().is_file());
        assert!(plugin.temporary_path().is_dir());
        assert_eq!(plugin.archive_sha256.len(), 64);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_ne!(
                fs::metadata(plugin.root().join("bin/helper"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o111,
                0
            );
        }
        let root = plugin.root().to_path_buf();
        drop(plugin);
        assert!(!root.exists());
    }
}

/// Verify metadata selection ignores filenames and refuses ambiguous matches.
/// 验证元数据选择不依赖文件名，并拒绝不明确的匹配。
///
/// # Arguments
///
/// No parameters.
/// 无参数。
#[test]
fn selects_by_system_type_and_rejects_duplicates_and_wrong_architecture() {
    let temporary = tempfile::tempdir().unwrap();
    fixture(
        &temporary.path().join("unrelated-name.tar.gz"),
        None,
        None,
        1,
    );
    assert_eq!(
        find(temporary.path(), "arch", "x86_64")
            .unwrap()
            .metadata
            .id,
        "test-plugin"
    );
    assert!(find(temporary.path(), "debian", "x86_64").is_err());
    assert!(find(temporary.path(), "arch", "aarch64").is_err());
    fixture(&temporary.path().join("second.tar.gz"), None, None, 1);
    assert!(
        find(temporary.path(), "arch", "x86_64")
            .unwrap_err()
            .to_string()
            .contains("多个插件")
    );
}

/// Verify an unrelated broken archive does not block a unique valid plugin.
/// 验证无关的损坏压缩包不会阻止唯一有效插件。
///
/// # Arguments
///
/// No parameters.
/// 无参数。
#[test]
fn retains_bad_archive_diagnostics_without_blocking_valid_selection() {
    let temporary = tempfile::tempdir().unwrap();
    fixture(&temporary.path().join("valid.tar.gz"), None, None, 1);
    fs::write(temporary.path().join("broken.tar.gz"), b"invalid").unwrap();
    let plugin = find(temporary.path(), "arch", "x86_64").unwrap();
    assert_eq!(plugin.discovery_warnings.len(), 1);
    assert!(plugin.discovery_warnings[0].contains("broken.tar.gz"));
    let missing = find(temporary.path(), "debian", "x86_64")
        .unwrap_err()
        .to_string();
    assert!(missing.contains("broken.tar.gz"));
}

/// Reject multiple roots, recursive wrappers, unsafe paths, and incompatible metadata.
/// 拒绝多根、多层包装、不安全路径和不兼容元数据。
///
/// # Arguments
///
/// No parameters.
/// 无参数。
#[test]
fn rejects_invalid_layouts_paths_and_api_versions() {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("plugin.tar.gz");
    for (wrapper, extra, api) in [
        (Some("one"), Some(".extra"), 1),
        (Some("one/two"), None, 1),
        (None, Some("../escape"), 1),
        (None, Some("/absolute"), 1),
        (None, None, 2),
    ] {
        fixture(&path, wrapper, extra, api);
        assert!(
            load(&path).is_err(),
            "accepted {wrapper:?}, {extra:?}, {api}"
        );
    }
}

/// Verify all documented compression formats load the same metadata.
/// 验证文档中的各压缩格式加载同一元数据。
///
/// # Arguments
///
/// No parameters.
/// 无参数。
#[test]
fn accepts_tar_xz_and_zstd_archives() {
    let temporary = tempfile::tempdir().unwrap();
    let tar = tar_bytes(None, None, 1);
    let plain = temporary.path().join("plugin.tar");
    fs::write(&plain, &tar).unwrap();
    let xz = temporary.path().join("plugin.tar.xz");
    let mut encoder = xz2::write::XzEncoder::new(fs::File::create(&xz).unwrap(), 1);
    encoder.write_all(&tar).unwrap();
    encoder.finish().unwrap();
    let zstd = temporary.path().join("plugin.tar.zst");
    fs::write(&zstd, zstd::stream::encode_all(tar.as_slice(), 1).unwrap()).unwrap();
    for path in [&plain, &xz, &zstd] {
        assert_eq!(load(path).unwrap().metadata.id, "test-plugin");
    }
}

/// Reject missing declarations, misspelled fields, and unsafe tool paths before invocation.
/// 调用前拒绝缺失声明、拼错字段和不安全工具路径。
///
/// # Arguments
///
/// No parameters.
/// 无参数。
#[test]
fn requires_and_validates_declared_tools() {
    let temporary = tempfile::tempdir().unwrap();
    let archive = temporary.path().join("plugin.tar.gz");
    fixture(&archive, None, None, 1);
    let plugin = load(&archive).unwrap();
    let text = fs::read_to_string(plugin.root().join("plugin.toml")).unwrap();
    assert!(
        toml::from_str::<Metadata>(&text.replace("[tools]\nhost=[]\nsystem=[]\n", "")).is_err()
    );
    assert!(toml::from_str::<Metadata>(&text.replace("host=[]", "hots=[]")).is_err());
    for invalid in ["", "../escape", "/usr/bin/tool", "bad tool", "./"] {
        let mut metadata = plugin.metadata.clone();
        metadata.tools.host = vec![invalid.into()];
        assert!(
            validate_metadata(&metadata, plugin.root()).is_err(),
            "accepted {invalid:?}"
        );
    }
}

/// Reject protected targets that cannot be passed as concrete package arguments.
/// 拒绝无法作为具体包参数传递的受保护目标。
///
/// # Arguments
///
/// No parameters.
/// 无参数。
#[test]
fn validates_protected_package_targets() {
    let temporary = tempfile::tempdir().unwrap();
    let archive = temporary.path().join("plugin.tar.gz");
    fixture(&archive, None, None, 1);
    let plugin = load(&archive).unwrap();
    let mut metadata = plugin.metadata.clone();
    metadata.protected_packages = vec!["base".into(), "libstdc++".into()];
    validate_metadata(&metadata, plugin.root()).unwrap();
    for invalid in [
        "",
        "-flag",
        ".hidden",
        "repo/pkg",
        "pkg:version",
        "bad name",
    ] {
        metadata.protected_packages = vec![invalid.into()];
        assert!(
            validate_metadata(&metadata, plugin.root()).is_err(),
            "accepted {invalid:?}"
        );
    }
}
