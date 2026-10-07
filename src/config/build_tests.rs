//! Verify complete build inputs and optional scratch-directory placement.
//! 验证完整构建输入和可选临时目录位置。

use super::*;

/// Create a manager directory with only the global input needed by these readers.
/// 创建管理目录，仅提供这些读取接口所需的全局输入。
///
/// # Arguments
///
/// No parameters.
/// 无参数。
fn fixture() -> (tempfile::TempDir, ConfigStore) {
    let temporary = tempfile::tempdir().unwrap();
    let store = ConfigStore::new(temporary.path()).unwrap();
    fs::create_dir(store.root().join("config")).unwrap();
    fs::create_dir(store.root().join("systems")).unwrap();
    fs::write(
        store.global_config_path(),
        "schema_version = 1\nsubsystems = []\n",
    )
    .unwrap();
    (temporary, store)
}

/// Write one build input preserving its source INI for independent readers.
/// 写入一份构建输入，保留源 INI 供独立读取接口使用。
///
/// # Arguments
///
/// - `directory`: Current or archived configuration root.
///   当前或留档配置根。
/// - `text`: Exact build INI to write.
///   待写入的准确构建 INI。
fn write_build(directory: &Path, text: &str) {
    fs::create_dir_all(directory.join("build")).unwrap();
    fs::write(directory.join(BUILD_CONFIG_PATH), text).unwrap();
}

/// Read every original distribution key, applying last-assignment semantics and retaining multiline values.
/// 读取所有原始发行版键，采用最后赋值语义并保留多行值。
/// # Arguments
/// No parameters.
/// 无参数。
#[test]
fn reads_plugin_distribution_options_without_renaming_or_splitting_values() {
    let (_temporary, store) = fixture();
    let directory = store.system_layout("desktop").unwrap().config;
    write_build(
        &directory,
        "[Distribution]\nDistribution=arch\nArchitecture=x86_64\nChannel=old\nChannel=stable\nMirror=https://example/$repo/$arch\nList=first\n    second\nEmpty=\n[Other]\nChannel=ignored\n",
    );
    let options = read_distribution_options_at(&directory).unwrap();
    assert_eq!(options.len(), 5);
    assert_eq!(options["Channel"], "stable");
    assert_eq!(options["Architecture"], "x86_64");
    assert_eq!(options["Mirror"], "https://example/$repo/$arch");
    assert_eq!(options["List"], "first\nsecond");
    assert_eq!(options["Empty"], "");
    assert!(!options.contains_key("Distribution"));
    fs::remove_file(directory.join(BUILD_CONFIG_PATH)).unwrap();
    fs::write(
        directory.join(LEGACY_BUILD_CONFIG_PATH),
        "architecture='x86_64'\n[distribution_options]\nChannel='stable'\n",
    )
    .unwrap();
    let options = read_distribution_options_at(&directory).unwrap();
    assert_eq!(options["Architecture"], "x86_64");
    assert_eq!(options["Channel"], "stable");
}

/// Preserve configured package order and duplicates and read archives independently of current inputs.
/// 保留配置包顺序和重复项，并独立于当前输入读取留档。
///
/// # Arguments
///
/// No parameters.
/// 无参数。
#[test]
fn reads_complete_inputs_and_uses_recorded_configuration() {
    let (_temporary, store) = fixture();
    let layout = store.system_layout("desktop").unwrap();
    write_build(
        &layout.config,
        "[Config]\nSchemaVersion=1\n[Distribution]\nDistribution=arch\nArchitecture=x86_64\n[Content]\nPackages=vim libstdc++ vim\nCustom=retained\n",
    );
    let config = store.read_build_config("desktop").unwrap();
    assert_eq!(config.system_type, "arch");
    assert_eq!(config.architecture.as_deref(), Some("x86_64"));
    assert_eq!(config.packages, ["vim", "libstdc++", "vim"]);
    write_build(
        &layout.versions.join("v00000001/config"),
        "[Config]\nSchemaVersion=1\n[Distribution]\nDistribution=arch\n[Content]\nPackages=nano\n",
    );
    write_build(&layout.config, "broken = [");
    assert_eq!(
        store
            .read_recorded_build_config("desktop", "v00000001")
            .unwrap()
            .packages,
        ["nano"]
    );
    assert!(store.read_build_config("desktop").is_err());
}

/// Reject unsupported schemas and package expressions before they reach plugin operations.
/// 在输入传入插件任务之前拒绝不支持的格式版本和包表达式。
///
/// # Arguments
///
/// No parameters.
/// 无参数。
#[test]
fn rejects_missing_and_invalid_build_inputs() {
    let (_temporary, store) = fixture();
    let directory = store.system_layout("desktop").unwrap().config;
    for text in [
        "[Config]\nSchemaVersion=1\n[Distribution]\nDistribution=arch\n",
        "[Config]\nSchemaVersion=2\n[Distribution]\nDistribution=arch\n[Content]\nPackages=\n",
        "[Config]\nSchemaVersion=1\n[Distribution]\nDistribution=../arch\n[Content]\nPackages=\n",
        "[Config]\nSchemaVersion=1\n[Distribution]\nDistribution=arch\nArchitecture=\n[Content]\nPackages=\n",
        "[Config]\nSchemaVersion=invalid\n[Distribution]\nDistribution=arch\n[Content]\nPackages=\n",
    ] {
        write_build(&directory, text);
        assert!(
            read_build_config_at(&directory).is_err(),
            "accepted {text:?}"
        );
    }
    for package in [
        "-flag",
        ".hidden",
        "pkg:version",
        "repo/pkg",
        "'a b'",
        "['vim']",
        "name>=1",
    ] {
        write_build(
            &directory,
            &format!(
                "[Config]\nSchemaVersion=1\n[Distribution]\nDistribution=arch\n[Content]\nPackages={package}\n"
            ),
        );
        assert!(
            read_build_config_at(&directory).is_err(),
            "accepted {package:?}"
        );
    }
    write_build(
        &directory,
        "[Config]\nSchemaVersion=1\n[Distribution]\nDistribution=arch\n[Content]\nPackages=\n",
    );
    assert!(
        read_build_config_at(&directory)
            .unwrap()
            .packages
            .is_empty()
    );
}

/// Resolve default, relative, and absolute work directories without relocating manager inputs.
/// 解析默认、相对及绝对工作目录，不改变管理器输入位置。
///
/// # Arguments
///
/// No parameters.
/// 无参数。
#[test]
fn resolves_optional_work_directory() {
    let (_temporary, store) = fixture();
    assert_eq!(store.build_work_dir().unwrap(), store.root().join("work"));
    for (selected, expected) in [
        ("scratch/build", store.root().join("scratch/build")),
        ("/mnt/btrfs/build", PathBuf::from("/mnt/btrfs/build")),
    ] {
        fs::write(
            store.global_config_path(),
            format!("schema_version=1\nwork_dir='{selected}'\n"),
        )
        .unwrap();
        assert_eq!(store.build_work_dir().unwrap(), expected);
        assert_eq!(store.plugins_path(), store.root().join("plugins"));
        assert_eq!(
            store.system_layout("desktop").unwrap().config,
            store.root().join("systems/desktop/config")
        );
    }
    for text in [
        "schema_version=1\nwork_dir=''\n",
        "schema_version=1\nwork_dir='../other'\n",
        "schema_version=2\n",
        "schema_version=1\nwork_dir=42\n",
    ] {
        fs::write(store.global_config_path(), text).unwrap();
        assert!(store.build_work_dir().is_err(), "accepted {text:?}");
    }
}

/// Enumerate system identities without failing early on an individual malformed build file.
/// 列举系统标识，不因单个构建文件格式错误而提前失败。
///
/// # Arguments
///
/// No parameters.
/// 无参数。
#[test]
fn lists_systems_in_order_without_parsing_individual_inputs() {
    let (_temporary, store) = fixture();
    write_build(&store.system_layout("zulu").unwrap().config, "invalid [");
    write_build(&store.system_layout("alpha").unwrap().config, "invalid [");
    assert_eq!(store.list_systems().unwrap(), ["alpha", "zulu"]);
    fs::write(store.root().join("systems/ordinary-file"), "unexpected").unwrap();
    assert!(store.list_systems().is_err());
}

/// Parse thousands of indented package names with comments, blank lines, and CRLF endings.
/// 解析数千个缩进包名，支持注释、空行及 CRLF 换行。
/// # Arguments
/// No parameters.
/// 无参数。
#[test]
fn reads_large_multiline_package_lists() {
    let (_temporary, store) = fixture();
    let directory = store.system_layout("desktop").unwrap().config;
    let packages: Vec<_> = (0..5000).map(|i| format!("package-{i}")).collect();
    let mut text = String::from(
        "[Config]\r\nSchemaVersion=1\r\n[Distribution]\r\nDistribution=arch\r\n[Content]\r\nPackages=\r\n",
    );
    for package in &packages {
        text.push_str(&format!("    {package} # 软件包\r\n\r\n"));
    }
    write_build(&directory, &text);
    assert_eq!(store.read_packages("desktop").unwrap().packages, packages);
    assert_eq!(
        store.read_build_config("desktop").unwrap().packages,
        packages
    );
}

/// Apply mkosi list append/reset rules across repeated sections and resolve the last scalar.
/// 跨重复分节应用 mkosi 列表追加、清空规则，并使用最后一个标量赋值。
/// # Arguments
/// No parameters.
/// 无参数。
#[test]
fn handles_repeated_assignments_continuations_and_empty_resets() {
    let (_temporary, store) = fixture();
    let directory = store.system_layout("desktop").unwrap().config;
    write_build(
        &directory,
        "[Config]\nSchemaVersion=1\n[Distribution]\nDistribution=obsolete\nDistribution=arch\n[Content]\nPackages=discarded\nPackages=\nPackages=vim, linux\n    libstdc++\n    # Comment inside the continuation\n\n    vim\n[Content]\nPackages=nano\n[Future]\nPackages=ignored\n",
    );
    let config = store.read_build_config("desktop").unwrap();
    assert_eq!(config.system_type, "arch");
    assert_eq!(
        config.packages,
        ["vim", "linux", "libstdc++", "vim", "nano"]
    );
    write_build(
        &directory,
        "    [Config]\n    SchemaVersion=1\n    [Distribution]\n    Distribution=arch\n    [Content]\n    Packages=vim\n    Packages=\n        nano\n",
    );
    assert_eq!(
        store.read_build_config("desktop").unwrap().packages,
        ["vim", "nano"]
    );
    write_build(
        &directory,
        "[Config]\nSchemaVersion=1\n[Distribution]\nDistribution=arch\n[Content]\nPackages=vim\nPackages= # Explicitly clear all packages\n",
    );
    assert!(
        store
            .read_build_config("desktop")
            .unwrap()
            .packages
            .is_empty()
    );
}

/// Preserve historical TOML inputs on initialization and prefer INI without hiding its errors.
/// 初始化时保留历史 TOML 输入，优先读取 INI 且不掩盖其错误。
/// # Arguments
/// No parameters.
/// 无参数。
#[test]
fn preserves_legacy_inputs_and_prioritizes_ini() {
    let (_temporary, store) = fixture();
    let directory = store.system_layout("desktop").unwrap().config;
    fs::create_dir_all(directory.join("build")).unwrap();
    let legacy = "schema_version=1\ndistribution='arch'\npackages=['vim']\n";
    fs::write(directory.join(LEGACY_BUILD_CONFIG_PATH), legacy).unwrap();
    store.initialize_system("desktop").unwrap();
    assert!(!directory.join(BUILD_CONFIG_PATH).exists());
    assert_eq!(
        fs::read_to_string(directory.join(LEGACY_BUILD_CONFIG_PATH)).unwrap(),
        legacy
    );
    assert_eq!(store.read_packages("desktop").unwrap().packages, ["vim"]);
    assert_eq!(
        store.read_system_type("desktop").unwrap().system_type,
        "arch"
    );
    assert_eq!(
        store.read_build_config("desktop").unwrap().packages,
        ["vim"]
    );
    write_build(
        &directory,
        "[Config]\nSchemaVersion=1\n[Distribution]\nDistribution=arch\n[Content]\nPackages=nano\n",
    );
    assert_eq!(
        store.read_build_config("desktop").unwrap().packages,
        ["nano"]
    );
    write_build(&directory, "[Content]\nPackages=vim\ninvalid line\n");
    assert!(matches!(
        store.read_build_config("desktop"),
        Err(Error::Ini { line: 3, .. })
    ));
}

/// Keep cache compatibility independent of package syntax while fingerprinting unknown settings.
/// 缓存兼容性不依赖包列表写法，同时为未知设置保留指纹。
/// # Arguments
/// No parameters.
/// 无参数。
#[test]
fn normalizes_ini_packages_and_excludes_only_the_content_package_setting() {
    let path = Path::new(BUILD_CONFIG_PATH);
    let compact = "[Content]\nPackages=vim,linux\n[Future]\nPackages=retained\n";
    let multiline = "# Different presentation\n[Content]\nPackages=discarded\nPackages=\nPackages=\n    vim\nPackages=linux\n[Future]\nPackages=retained\n";
    assert_eq!(
        normalized_build_config(compact, path, false).unwrap(),
        normalized_build_config(multiline, path, false).unwrap()
    );
    let changed = compact.replace("vim,linux", "nano");
    assert_ne!(
        normalized_build_config(compact, path, false).unwrap(),
        normalized_build_config(&changed, path, false).unwrap()
    );
    assert_eq!(
        normalized_build_config(compact, path, true).unwrap(),
        normalized_build_config(&changed, path, true).unwrap()
    );
    let unknown_change = compact.replace("retained", "changed");
    assert_ne!(
        normalized_build_config(compact, path, true).unwrap(),
        normalized_build_config(&unknown_change, path, true).unwrap()
    );
    let empty_section = format!("{compact}[FutureSection]\n");
    assert_ne!(
        normalized_build_config(compact, path, true).unwrap(),
        normalized_build_config(&empty_section, path, true).unwrap()
    );
    let legacy = "schema_version=1\nsystem_type='arch'\npackages=['vim']\ncustom=true\n";
    let mut value: toml::Value = toml::from_str(legacy).unwrap();
    assert_eq!(
        normalized_build_config(legacy, Path::new(LEGACY_BUILD_CONFIG_PATH), false).unwrap(),
        toml::to_string(&value).unwrap().into_bytes()
    );
    value.as_table_mut().unwrap().remove("packages");
    assert_eq!(
        normalized_build_config(legacy, Path::new(LEGACY_BUILD_CONFIG_PATH), true).unwrap(),
        toml::to_string(&value).unwrap().into_bytes()
    );
}
