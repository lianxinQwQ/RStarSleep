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
    let filenames: Vec<_> = REQUIRED_OPERATIONS
        .iter()
        .chain(
            if api_version == 3 {
                TEMPLATE_OPERATIONS
            } else {
                &[]
            }
            .iter(),
        )
        .map(|operation| format!("{operation}.sh"))
        .collect();
    let operations: Vec<_> = filenames.iter().map(String::as_str).collect();
    tar_bytes_with_operations(wrapper, extra, api_version, &operations)
}

/// Create an archive with explicitly chosen operation filenames to test layout rejection.
/// 创建包含指定操作文件名的归档，验证布局拒绝行为。
///
/// # Arguments
///
/// - `wrapper`: Optional top-level directory.
///   可选顶层目录。
/// - `extra`: Optional raw archive entry.
///   可选原始归档条目。
/// - `api_version`: Task protocol version.
///   任务协议版本。
/// - `operations`: Operation filenames included in the package.
///   包中包含的操作文件名。
fn tar_bytes_with_operations(
    wrapper: Option<&str>,
    extra: Option<&str>,
    api_version: u32,
    operations: &[&str],
) -> Vec<u8> {
    let metadata = format!(
        "schema_version=1\napi_version={api_version}\nid='test-plugin'\nversion='1'\nsystem_types=['arch']\narchitectures=['x86_64']\ncache_format='test-v1'\n[tools]\nhost=[]\nsystem=[]\n"
    );
    let mut archive = tar::Builder::new(Vec::new());
    let mut files = vec![
        ("plugin.toml", metadata.as_bytes()),
        (".hidden", b"retained".as_slice()),
        ("bin/helper", b"tool".as_slice()),
    ];
    files.extend(
        operations
            .iter()
            .map(|name| (*name, b"#!/bin/bash\nexit 0\n".as_slice())),
    );
    for (name, contents) in files {
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
        fixture(&path, wrapper, None, 2);
        let plugin = load(&path).unwrap();
        assert_eq!(plugin.root().file_name().unwrap(), "plugin");
        assert_eq!(
            fs::read(plugin.root().join(".hidden")).unwrap(),
            b"retained"
        );
        assert_eq!(fs::read(plugin.root().join("bin/helper")).unwrap(), b"tool");
        for operation in REQUIRED_OPERATIONS {
            assert!(plugin.operation_path(operation).unwrap().is_file());
        }
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
        2,
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
    fixture(&temporary.path().join("second.tar.gz"), None, None, 2);
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
    fixture(&temporary.path().join("valid.tar.gz"), None, None, 2);
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
        (Some("one"), Some(".extra"), 2),
        (Some("one/two"), None, 2),
        (None, Some("../escape"), 2),
        (None, Some("/absolute"), 2),
        (None, None, 1),
        (None, None, 4),
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
    let tar = tar_bytes(None, None, 2);
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

/// Reject incomplete operation sets and unsuffixed or incorrectly suffixed alternatives during archive loading.
/// 加载归档时拒绝缺少操作的插件及无后缀或后缀错误的替代文件。
///
/// # Arguments
///
/// No parameters.
/// 无参数。
#[test]
fn requires_every_same_named_operation_file() {
    let temporary = tempfile::tempdir().unwrap();
    let archive = temporary.path().join("plugin.tar");
    for missing in REQUIRED_OPERATIONS {
        let filenames: Vec<_> = REQUIRED_OPERATIONS
            .iter()
            .filter(|operation| *operation != missing)
            .map(|operation| format!("{operation}.sh"))
            .collect();
        for replacement in [
            None,
            Some((*missing).to_owned()),
            Some(format!("{missing}.bash")),
        ] {
            let mut operations: Vec<_> = filenames.iter().map(String::as_str).collect();
            if let Some(ref replacement) = replacement {
                operations.push(replacement);
            }
            fs::write(
                &archive,
                tar_bytes_with_operations(None, None, 2, &operations),
            )
            .unwrap();
            let error = load(&archive).unwrap_err().to_string();
            assert!(
                error.contains(&format!("{missing}.sh")),
                "unexpected error: {error}"
            );
            assert!(
                error.contains("缺少同名普通文件"),
                "unexpected error: {error}"
            );
        }
    }
}

/// Require both template lifecycle operations for API 3 while continuing to load API 2.
/// API 3 要求两个模板生命周期操作，同时继续加载 API 2。
/// # Arguments
///
/// No parameters.
/// 无参数。
#[test]
fn api_three_requires_template_operations() {
    let temporary = tempfile::tempdir().unwrap();
    let archive = temporary.path().join("plugin.tar");
    fs::write(&archive, tar_bytes(None, None, 3)).unwrap();
    assert_eq!(load(&archive).unwrap().metadata.api_version, 3);
    for missing in TEMPLATE_OPERATIONS {
        let filenames: Vec<_> = REQUIRED_OPERATIONS
            .iter()
            .chain(TEMPLATE_OPERATIONS)
            .filter(|name| *name != missing)
            .map(|name| format!("{name}.sh"))
            .collect();
        let operations: Vec<_> = filenames.iter().map(String::as_str).collect();
        fs::write(
            &archive,
            tar_bytes_with_operations(None, None, 3, &operations),
        )
        .unwrap();
        assert!(
            load(&archive)
                .unwrap_err()
                .to_string()
                .contains(&format!("{missing}.sh"))
        );
    }
    fs::write(&archive, tar_bytes(None, None, 2)).unwrap();
    assert_eq!(load(&archive).unwrap().metadata.api_version, 2);
}

/// Reject legacy entry declarations and non-regular operation files.
/// 拒绝旧入口声明和非普通操作文件。
///
/// # Arguments
///
/// No parameters.
/// 无参数。
#[test]
fn rejects_legacy_entry_and_non_regular_operations() {
    let temporary = tempfile::tempdir().unwrap();
    let archive = temporary.path().join("plugin.tar.gz");
    fixture(&archive, None, None, 2);
    let plugin = load(&archive).unwrap();
    let text = fs::read_to_string(plugin.root().join("plugin.toml")).unwrap();
    assert!(toml::from_str::<Metadata>(&format!("entry='plugin.sh'\n{text}")).is_err());
    let operation = plugin.operation_path("update").unwrap();
    fs::remove_file(&operation).unwrap();
    fs::create_dir(&operation).unwrap();
    assert!(validate_metadata(&plugin.metadata, plugin.root()).is_err());
    #[cfg(unix)]
    {
        fs::remove_dir(&operation).unwrap();
        std::os::unix::fs::symlink("bootstrap.sh", &operation).unwrap();
        assert!(validate_metadata(&plugin.metadata, plugin.root()).is_err());
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
    fixture(&archive, None, None, 2);
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
    fixture(&archive, None, None, 2);
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
