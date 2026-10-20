//! Exercise discovery, wrapper normalization, and archive rejection without running plugins.
//! 不运行插件，验证查找、包装目录归一化和非法归档拒绝。

use super::validation::validate_metadata;
use super::*;
use std::{fs, io::Write, path::Path};

/// Post-maintenance is optional; advertising it requires a safe executable entry.
/// 软件维护后行为是可选能力；声明后必须提供安全的操作入口。
#[test]
fn optional_post_maintenance_capability() {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("plugin.tar");
    for api_version in [2, 3] {
        fs::write(&path, tar_bytes(None, None, api_version)).unwrap();
        let plugin = load(&path).unwrap();
        assert!(!plugin.metadata.post_maintenance);
        let mut metadata = plugin.metadata.clone();
        metadata.post_maintenance = true;
        assert!(validate_metadata(&metadata, plugin.root()).is_err());
        let entry = plugin.root().join("post_maintenance.sh");
        fs::write(&entry, "#!/bin/bash\nexit 0\n").unwrap();
        assert!(validate_metadata(&metadata, plugin.root()).is_ok());
        fs::remove_file(&entry).unwrap();
        std::os::unix::fs::symlink("/bin/sh", &entry).unwrap();
        assert!(validate_metadata(&metadata, plugin.root()).is_err());
    }
}

/// Older plugins keep working while advertised file-query capabilities require a safe entry.
/// 旧插件继续可用，声明文件查询能力时必须提供安全入口。
/// # Arguments
/// No parameters. 无参数。
#[test]
fn optional_package_file_capability() {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("plugin.tar");
    fs::write(&path, tar_bytes(None, None, 3)).unwrap();
    let plugin = load(&path).unwrap();
    assert!(!plugin.metadata.query_package_files);
    let mut metadata = plugin.metadata.clone();
    metadata.query_package_files = true;
    assert!(validate_metadata(&metadata, plugin.root()).is_err());
    let entry = plugin.root().join("query_package_files.sh");
    fs::write(&entry, "#!/bin/bash\nexit 0\n").unwrap();
    assert!(validate_metadata(&metadata, plugin.root()).is_ok());
    fs::remove_file(&entry).unwrap();
    std::os::unix::fs::symlink("/bin/sh", &entry).unwrap();
    assert!(validate_metadata(&metadata, plugin.root()).is_err());
}

/// Reject incomplete configuration contracts and keys colliding with each other or controller state.
/// 拒绝不完整配置契约及相互冲突或占用控制器状态的键。
/// # Arguments
/// No parameters.
/// 无参数。
#[test]
fn requires_valid_explicit_configuration_declarations() {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("plugin.tar");
    fs::write(&path, tar_bytes(None, None, 3)).unwrap();
    let plugin = load(&path).unwrap();
    for declaration in ["[config]\nrequired=[]\n", "[config]\noptional=[]\n", ""] {
        let text = fs::read_to_string(plugin.root().join("plugin.toml"))
            .unwrap()
            .replace(
                "[config]\nrequired=[]\noptional=['RSTAR_DISTRIBUTION_ARCHITECTURE','RSTAR_CONFIG_SCHEMAVERSION','RSTAR_DISTRIBUTION_DISTRIBUTION','RSTAR_CONTENT_PACKAGES']\n",
                declaration,
            );
        assert!(toml::from_str::<Metadata>(&text).is_err());
    }
    for names in [
        vec!["RSTAR_ROOT"],
        vec!["RSTAR_PLUGIN_TMP"],
        vec!["Distribution"],
        vec!["RSTAR_BAD-NAME"],
        vec!["RSTAR_ARCHITECTURE", "RSTAR_architecture"],
        vec!["RSTAR_CHANNEL", "RSTAR_CHANNEL"],
    ] {
        let mut metadata = plugin.metadata.clone();
        metadata.config.required = names.into_iter().map(String::from).collect();
        metadata.config.optional.clear();
        assert!(validate_metadata(&metadata, plugin.root()).is_err());
    }
    let mut metadata = plugin.metadata.clone();
    metadata.config.required = vec!["RSTAR_DISTRIBUTION_ARCHITECTURE".into()];
    assert!(validate_metadata(&metadata, plugin.root()).is_err());
}

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
                SYSTEM_OPERATIONS
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
        "schema_version=1\napi_version={api_version}\nid='test-plugin'\nversion='1'\nsystem_types=['arch']\narchitectures=['x86_64']\ncache_format='test-v1'\n[config]\nrequired=[]\noptional=['RSTAR_DISTRIBUTION_ARCHITECTURE','RSTAR_CONFIG_SCHEMAVERSION','RSTAR_DISTRIBUTION_DISTRIBUTION','RSTAR_CONTENT_PACKAGES']\n[cache]\nbase_system=[]\nsoftware=[]\n[tools]\nhost=[]\nsystem=[]\n"
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

/// Reject incomplete operation sets and incorrectly suffixed alternatives during archive loading.
/// 加载归档时拒绝缺少操作的插件及后缀错误的替代文件。
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
        for replacement in [None, Some(format!("{missing}.bash"))] {
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

/// Require system initialization for API 3 while continuing to load API 2.
/// API 3 要求系统初始化操作，同时继续加载 API 2。
/// # Arguments
///
/// No parameters.
/// 无参数。
#[test]
fn api_three_requires_system_initialization() {
    let temporary = tempfile::tempdir().unwrap();
    let archive = temporary.path().join("plugin.tar");
    fs::write(&archive, tar_bytes(None, None, 3)).unwrap();
    assert_eq!(load(&archive).unwrap().metadata.api_version, 3);
    for missing in SYSTEM_OPERATIONS {
        let filenames: Vec<_> = REQUIRED_OPERATIONS
            .iter()
            .chain(SYSTEM_OPERATIONS)
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
        toml::from_str::<Metadata>(&text.replace(
            "[config]\nrequired=[]\noptional=['RSTAR_DISTRIBUTION_ARCHITECTURE','RSTAR_CONFIG_SCHEMAVERSION','RSTAR_DISTRIBUTION_DISTRIBUTION','RSTAR_CONTENT_PACKAGES']\n[cache]\nbase_system=[]\nsoftware=[]\n[tools]\nhost=[]\nsystem=[]\n",
            ""
        ))
        .is_err()
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

/// A conventional kernel and ordered initrd declaration for metadata validation tests.
/// 用于元数据校验测试的常规内核及有序 initrd 声明。
fn boot_declaration() -> BootRequirements {
    BootRequirements {
        packages: vec!["linux".into()],
        artifacts: vec![
            BootArtifact {
                source: "boot/vmlinuz-linux".into(),
                destination: "vmlinuz-linux".into(),
                kind: BootArtifactKind::Kernel,
                optional: false,
            },
            BootArtifact {
                source: "boot/intel-ucode.img".into(),
                destination: "intel-ucode.img".into(),
                kind: BootArtifactKind::Initrd,
                optional: true,
            },
            BootArtifact {
                source: "boot/initramfs-linux.img".into(),
                destination: "initramfs-linux.img".into(),
                kind: BootArtifactKind::Initrd,
                optional: false,
            },
        ],
    }
}

/// Optional capability declarations are backward compatible and do not require unused actions.
/// 可选能力声明兼容旧插件，不要求尚未使用的操作入口存在。
#[test]
fn validates_optional_boot_and_inheritance_declarations_without_action_files() {
    let temporary = tempfile::tempdir().unwrap();
    let archive = temporary.path().join("plugin.tar");
    fs::write(&archive, tar_bytes(None, None, 3)).unwrap();
    let plugin = load(&archive).unwrap();
    assert!(plugin.metadata.boot.is_none());
    assert!(plugin.metadata.initramfs_inheritance.is_none());
    let mut metadata = plugin.metadata.clone();
    metadata.boot = Some(boot_declaration());
    metadata.initramfs_inheritance = Some(ActionRequirements {
        packages: vec!["mkinitcpio".into(), "btrfs-progs".into()],
        tools: vec!["mkinitcpio".into(), "btrfs".into()],
    });
    validate_metadata(&metadata, plugin.root()).unwrap();
    assert!(
        plugin
            .operation_path("prepare_inheritance_initramfs")
            .is_err()
    );
    let boot = metadata.boot.as_ref().unwrap();
    let serialized = toml::to_string(boot).unwrap();
    assert!(serialized.contains("kind = \"kernel\""));
    assert_eq!(
        toml::from_str::<BootRequirements>(&serialized).unwrap(),
        *boot
    );
    let text = fs::read_to_string(plugin.root().join("plugin.toml")).unwrap();
    let metadata_boot = serialized.replace("[[artifacts]]", "[[boot.artifacts]]");
    let parsed = toml::from_str::<Metadata>(&format!(
        "{text}\n[boot]\n{metadata_boot}\n[initramfs_inheritance]\npackages=['mkinitcpio']\ntools=['mkinitcpio']\n"
    ))
    .unwrap();
    validate_metadata(&parsed, plugin.root()).unwrap();
    assert_eq!(parsed.boot.as_ref(), metadata.boot.as_ref());
}

/// Unsafe declarations and ambiguous boot-loader roles are rejected before build side effects.
/// 构建产生副作用前，拒绝不安全声明及含糊的启动加载角色。
#[test]
fn rejects_unsafe_or_incomplete_boot_declarations() {
    let temporary = tempfile::tempdir().unwrap();
    let archive = temporary.path().join("plugin.tar");
    fs::write(&archive, tar_bytes(None, None, 3)).unwrap();
    let plugin = load(&archive).unwrap();
    let mut metadata = plugin.metadata.clone();
    for invalid in [
        "",
        "/boot/kernel",
        "../kernel",
        "boot/../kernel",
        "./boot/kernel",
        "boot//kernel",
        "boot/kernel/",
        "boot\\kernel",
        "boot/a b",
        "boot/a\n",
    ] {
        let mut boot = boot_declaration();
        boot.artifacts[0].source = invalid.into();
        metadata.boot = Some(boot);
        assert!(
            validate_metadata(&metadata, plugin.root()).is_err(),
            "accepted source {invalid:?}"
        );
    }
    for invalid in [
        "",
        ".",
        "..",
        "/kernel",
        "dir/kernel",
        "a\\b",
        "kernel:",
        "kernel.",
        "two words",
        "a\n",
    ] {
        let mut boot = boot_declaration();
        boot.artifacts[0].destination = invalid.into();
        metadata.boot = Some(boot);
        assert!(
            validate_metadata(&metadata, plugin.root()).is_err(),
            "accepted destination {invalid:?}"
        );
    }
    let mut invalid_structures = vec![BootRequirements {
        packages: vec![],
        artifacts: vec![],
    }];
    let mut optional_kernel = boot_declaration();
    optional_kernel.artifacts[0].optional = true;
    invalid_structures.push(optional_kernel);
    let mut two_kernels = boot_declaration();
    two_kernels.artifacts[1].kind = BootArtifactKind::Kernel;
    invalid_structures.push(two_kernels);
    let mut optional_initrds = boot_declaration();
    optional_initrds.artifacts[2].optional = true;
    invalid_structures.push(optional_initrds);
    let mut duplicate = boot_declaration();
    duplicate.artifacts[1].destination = "VMLINUZ-LINUX".into();
    invalid_structures.push(duplicate);
    for boot in invalid_structures {
        metadata.boot = Some(boot);
        assert!(validate_metadata(&metadata, plugin.root()).is_err());
    }
}

/// Capability package and executable names obey the same safety rules as existing declarations.
/// 能力所需包名及工具名遵循现有声明的安全约束。
#[test]
fn rejects_invalid_action_package_and_tool_declarations() {
    let temporary = tempfile::tempdir().unwrap();
    let archive = temporary.path().join("plugin.tar");
    fs::write(&archive, tar_bytes(None, None, 3)).unwrap();
    let plugin = load(&archive).unwrap();
    let mut metadata = plugin.metadata.clone();
    for invalid in ["", "-flag", "repo/pkg", "pkg:version", "bad name"] {
        let mut boot = boot_declaration();
        boot.packages = vec![invalid.into()];
        metadata.boot = Some(boot);
        assert!(validate_metadata(&metadata, plugin.root()).is_err());
        metadata.boot = None;
        metadata.initramfs_inheritance = Some(ActionRequirements {
            packages: vec![invalid.into()],
            tools: vec![],
        });
        assert!(validate_metadata(&metadata, plugin.root()).is_err());
        metadata.initramfs_inheritance = None;
    }
    for invalid in ["", "../escape", "/usr/bin/tool", "bad tool", "./"] {
        metadata.initramfs_inheritance = Some(ActionRequirements {
            packages: vec![],
            tools: vec![invalid.into()],
        });
        assert!(validate_metadata(&metadata, plugin.root()).is_err());
    }
}

/// Verify directory discovery, immutable copies, and complete deterministic fingerprints.
/// 验证目录发现、不可变副本及完整确定性指纹。
/// # Arguments
/// No parameters.
/// 无参数。
#[test]
fn directory_plugins_are_frozen_and_hashed_independently_of_location() {
    let temporary = tempfile::tempdir().unwrap();
    let source = temporary.path().join("directory-plugin");
    fs::create_dir(&source).unwrap();
    tar::Archive::new(tar_bytes(None, None, 3).as_slice())
        .unpack(&source)
        .unwrap();
    let plugin = find(temporary.path(), "arch", "x86_64").unwrap();
    let hash = plugin.archive_sha256.clone();
    assert_eq!(load(&source).unwrap().archive_sha256, hash);
    fs::write(source.join(".hidden"), "edited").unwrap();
    assert_eq!(
        fs::read_to_string(plugin.root().join(".hidden")).unwrap(),
        "retained"
    );
    assert_ne!(load(&source).unwrap().archive_sha256, hash);
    let edited = load(&source).unwrap().archive_sha256;
    fs::create_dir(source.join("empty-directory")).unwrap();
    assert_ne!(load(&source).unwrap().archive_sha256, edited);
    let renamed = temporary.path().join("renamed-plugin");
    fs::rename(&source, &renamed).unwrap();
    let hash = load(&renamed).unwrap().archive_sha256;
    let elsewhere = temporary.path().join("elsewhere");
    fs::rename(&renamed, &elsewhere).unwrap();
    assert_eq!(load(&elsewhere).unwrap().archive_sha256, hash);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(
            elsewhere.join("bin/helper"),
            fs::Permissions::from_mode(0o644),
        )
        .unwrap();
        assert_ne!(load(&elsewhere).unwrap().archive_sha256, hash);
    }
    let frozen = plugin.root().to_path_buf();
    drop(plugin);
    assert!(!frozen.exists());
}

/// Apply the same matching and link boundaries to directories and archives.
/// 对目录及压缩包应用相同匹配及链接边界规则。
/// # Arguments
/// No parameters.
/// 无参数。
#[cfg(unix)]
#[test]
fn directory_plugins_reject_escaping_links_and_duplicate_matches() {
    let temporary = tempfile::tempdir().unwrap();
    let archive = temporary.path().join("plugin.tar.gz");
    fixture(&archive, None, None, 3);
    let directory = temporary.path().join("directory");
    fs::create_dir(&directory).unwrap();
    tar::Archive::new(tar_bytes(None, None, 3).as_slice())
        .unpack(&directory)
        .unwrap();
    assert!(
        find(temporary.path(), "arch", "x86_64")
            .unwrap_err()
            .to_string()
            .contains("多个插件")
    );
    fs::remove_file(archive).unwrap();
    let link = directory.join("resource-link");
    std::os::unix::fs::symlink(".hidden", &link).unwrap();
    let plugin = load(&directory).unwrap();
    assert_eq!(
        fs::read_link(plugin.root().join("resource-link")).unwrap(),
        Path::new(".hidden")
    );
    for target in ["../escape", "/etc/passwd"] {
        fs::remove_file(&link).unwrap();
        std::os::unix::fs::symlink(target, &link).unwrap();
        assert!(load(&directory).is_err());
    }
    fs::remove_file(&link).unwrap();
    let operation = directory.join("install.sh");
    fs::remove_file(&operation).unwrap();
    std::os::unix::fs::symlink("update.sh", operation).unwrap();
    assert!(load(&directory).is_err());
}

#[test]
fn predownload_capability_requires_its_entrypoint_and_independent_lifecycle() {
    let root = tempfile::tempdir().unwrap();
    let mut metadata:Metadata=toml::from_str("schema_version=1\napi_version=3\nid='test'\nversion='1'\narchitectures=['x86_64']\ncache_format='test-v1'\npredownload=true\n[config]\nrequired=[]\noptional=['RSTAR_DISTRIBUTION_ARCHITECTURE','RSTAR_CONFIG_SCHEMAVERSION','RSTAR_DISTRIBUTION_DISTRIBUTION','RSTAR_CONTENT_PACKAGES']\n[cache]\nbase_system=[]\nsoftware=[]\n[tools]\nhost=[]\nsystem=[]\n").unwrap();
    for op in REQUIRED_OPERATIONS.iter().chain(SYSTEM_OPERATIONS) {
        fs::write(root.path().join(format!("{op}.sh")), "exit 0").unwrap();
    }
    assert!(validate_metadata(&metadata, root.path()).is_err());
    fs::create_dir(root.path().join("predownload")).unwrap();
    for file in [
        "predownload.sh",
        "predownload/install.sh",
        "predownload/update.sh",
    ] {
        fs::write(root.path().join(file), "exit 0").unwrap();
    }
    validate_metadata(&metadata, root.path()).unwrap();
    #[cfg(unix)]
    for file in ["predownload/install", "predownload/update"] {
        use std::os::unix::fs::PermissionsExt;
        let entry = root.path().join(file);
        fs::rename(root.path().join(format!("{file}.sh")), &entry).unwrap();
        fs::set_permissions(&entry, fs::Permissions::from_mode(0o755)).unwrap();
        validate_metadata(&metadata, root.path()).unwrap();
    }
    metadata.predownload = false;
    fs::remove_file(super::validation::operation_path(root.path(), "predownload").unwrap())
        .unwrap();
    validate_metadata(&metadata, root.path()).unwrap();
}

/// Load mixed shell and executable entries from both archives and frozen directories.
/// 从归档和冻结目录加载混合的脚本与可执行入口。
#[test]
#[cfg(unix)]
fn loads_unsuffixed_and_mixed_operation_entries() {
    let temporary = tempfile::tempdir().unwrap();
    let archive = temporary.path().join("plugin.tar");
    for mixed in [false, true] {
        let filenames: Vec<_> = REQUIRED_OPERATIONS
            .iter()
            .chain(SYSTEM_OPERATIONS)
            .enumerate()
            .map(|(index, name)| {
                if mixed && index % 2 == 0 {
                    format!("{name}.sh")
                } else {
                    (*name).to_owned()
                }
            })
            .collect();
        let operations: Vec<_> = filenames.iter().map(String::as_str).collect();
        fs::write(
            &archive,
            tar_bytes_with_operations(Some("wrapper"), None, 3, &operations),
        )
        .unwrap();
        let packaged = load(&archive).unwrap();
        let directory = load(packaged.root()).unwrap();
        for plugin in [&packaged, &directory] {
            for (operation, filename) in REQUIRED_OPERATIONS
                .iter()
                .chain(SYSTEM_OPERATIONS)
                .zip(&filenames)
            {
                assert_eq!(
                    plugin.operation_path(operation).unwrap(),
                    plugin.root().join(filename)
                );
            }
        }
    }
}

/// Keep shell precedence and reject unsafe or non-executable unsuffixed entries.
/// 保持脚本优先，拒绝不安全或不可执行的无后缀入口。
#[test]
#[cfg(unix)]
fn validates_unsuffixed_entry_permissions_and_shell_precedence() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let temporary = tempfile::tempdir().unwrap();
    let archive = temporary.path().join("plugin.tar");
    fs::write(&archive, tar_bytes(None, None, 3)).unwrap();
    let plugin = load(&archive).unwrap();
    let script = plugin.root().join("update.sh");
    let entry = plugin.root().join("update");
    fs::write(&entry, "#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(&entry, fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(plugin.operation_path("update").unwrap(), script);
    fs::remove_file(&script).unwrap();
    assert!(
        plugin
            .operation_path("update")
            .unwrap_err()
            .to_string()
            .contains("可执行权限")
    );
    fs::set_permissions(&entry, fs::Permissions::from_mode(0o755)).unwrap();
    validate_metadata(&plugin.metadata, plugin.root()).unwrap();
    fs::create_dir(&script).unwrap();
    assert!(plugin.operation_path("update").is_err());
    fs::remove_dir(&script).unwrap();
    fs::remove_file(&entry).unwrap();
    symlink("bootstrap.sh", &entry).unwrap();
    assert!(plugin.operation_path("update").is_err());
    fs::remove_file(&entry).unwrap();
    fs::create_dir(&entry).unwrap();
    assert!(plugin.operation_path("update").is_err());
}

#[test]
fn requires_explicit_cache_dependencies_and_validates_selectors() {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("plugin.tar");
    fs::write(&path, tar_bytes(None, None, 3)).unwrap();
    let plugin = load(&path).unwrap();
    let text = fs::read_to_string(plugin.root().join("plugin.toml")).unwrap();
    for declaration in ["", "[cache]\nbase_system=[]\n", "[cache]\nsoftware=[]\n"] {
        assert!(
            toml::from_str::<Metadata>(
                &text.replace("[cache]\nbase_system=[]\nsoftware=[]\n", declaration)
            )
            .is_err()
        );
    }
    for selectors in [
        vec!["Architecture"],
        vec!["RSTAR_ROOT"],
        vec!["RSTAR_MISSING"],
        vec!["RSTAR_DISTRIBUTION", "RSTAR_DISTRIBUTION"],
        vec!["RSTAR_*DISTRIBUTION_MISSING"],
        vec![
            "RSTAR_DISTRIBUTION_ARCHITECTURE",
            "RSTAR_*DISTRIBUTION_ARCHITECTURE",
        ],
    ] {
        let mut metadata = plugin.metadata.clone();
        metadata.cache.base_system = selectors.into_iter().map(String::from).collect();
        assert!(validate_metadata(&metadata, plugin.root()).is_err());
    }
    let mut metadata = plugin.metadata.clone();
    metadata.cache.base_system = vec!["RSTAR_DISTRIBUTION".into()];
    metadata.cache.software = vec!["RSTAR_*DISTRIBUTION_ARCHITECTURE".into()];
    validate_metadata(&metadata, plugin.root()).unwrap();
}

#[test]
fn package_collection_requires_explicit_capability_and_safe_resolver_lifecycle() {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("plugin.tar");
    fs::write(&path, tar_bytes(None, None, 3)).unwrap();
    let plugin = load(&path).unwrap();
    assert!(plugin.metadata.package_plan.is_none());
    let mut metadata = plugin.metadata.clone();
    metadata.package_plan = Some(PackagePlanRequirements {
        base_packages: vec!["base".into()],
        tools: vec!["python3".into()],
    });
    for operation in [
        "collect_packages.sh",
        "predownload/install.sh",
        "predownload/update.sh",
    ] {
        assert!(validate_metadata(&metadata, plugin.root()).is_err());
        let entry = plugin.root().join(operation);
        fs::create_dir_all(entry.parent().unwrap()).unwrap();
        fs::write(entry, "exit 0\n").unwrap();
    }
    assert!(!metadata.predownload);
    assert!(validate_metadata(&metadata, plugin.root()).is_ok());
    let mut invalid = metadata.clone();
    invalid.api_version = 2;
    assert!(validate_metadata(&invalid, plugin.root()).is_err());
    for package in ["bad name", "--unsafe"] {
        let mut invalid = metadata.clone();
        invalid.package_plan.as_mut().unwrap().base_packages = vec![package.into()];
        assert!(validate_metadata(&invalid, plugin.root()).is_err());
    }
    metadata.package_plan.as_mut().unwrap().tools = vec!["../outside".into()];
    assert!(validate_metadata(&metadata, plugin.root()).is_err());
    assert!(is_query("collect_packages"));
}

#[test]
fn package_database_paths_are_optional_and_confined_to_target() {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("plugin.tar");
    fs::write(&path, tar_bytes(None, None, 3)).unwrap();
    let plugin = load(&path).unwrap();
    assert!(plugin.metadata.package_database_paths.is_empty());
    for value in ["", "/var/lib/pacman", "../db", "var/../db"] {
        let mut metadata = plugin.metadata.clone();
        metadata.package_database_paths = vec![value.into()];
        assert!(validate_metadata(&metadata, plugin.root()).is_err());
    }
    let mut metadata = plugin.metadata.clone();
    metadata.package_database_paths = vec!["var/lib/pacman/local".into()];
    assert!(validate_metadata(&metadata, plugin.root()).is_ok());
}
