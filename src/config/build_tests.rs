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
    assert_eq!(options["Channel"], "old\nstable");
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
        "repo/pkg/extra",
        "aur/",
        "/pkg",
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
        "[Config]\nSchemaVersion=1\n[Distribution]\nDistribution=arch\n[Content]\nPackages=discarded\nPackages=\nPackages=vim, linux\n    libstdc++\n    # Comment inside the continuation\n\n    vim\n[Content]\nPackages=nano\n[Future]\nPackages=ignored\n",
    );
    let config = store.read_build_config("desktop").unwrap();
    assert_eq!(config.system_type, "arch");
    assert_eq!(
        config.packages,
        ["discarded", "vim", "linux", "libstdc++", "vim", "nano"]
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
    assert!(store.read_build_config("desktop").unwrap().packages == ["vim"]);
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

/// Normalize package syntax while fingerprinting every archived setting.
/// 规范化包列表写法，同时为所有留档设置保留指纹。
/// # Arguments
/// No parameters.
/// 无参数。
#[test]
fn normalizes_ini_packages_and_preserves_all_provenance() {
    let path = Path::new(BUILD_CONFIG_PATH);
    let compact = "[Content]\nPackages=vim,linux\n[Future]\nPackages=retained\n";
    let multiline = "# Different presentation\n[Content]\nPackages=discarded\nPackages=\nPackages=\n    vim\nPackages=linux\n[Future]\nPackages=retained\n";
    assert_ne!(
        normalized_build_config(compact, path).unwrap(),
        normalized_build_config(multiline, path).unwrap()
    );
    let changed = compact.replace("vim,linux", "nano");
    assert_ne!(
        normalized_build_config(compact, path).unwrap(),
        normalized_build_config(&changed, path).unwrap()
    );
    let unknown_change = compact.replace("retained", "changed");
    assert_ne!(
        normalized_build_config(compact, path).unwrap(),
        normalized_build_config(&unknown_change, path).unwrap()
    );
    let empty_section = format!("{compact}[FutureSection]\n");
    assert_ne!(
        normalized_build_config(compact, path).unwrap(),
        normalized_build_config(&empty_section, path).unwrap()
    );
    let legacy = "schema_version=1\nsystem_type='arch'\npackages=['vim']\ncustom=true\n";
    let value: toml::Value = toml::from_str(legacy).unwrap();
    assert_eq!(
        normalized_build_config(legacy, Path::new(LEGACY_BUILD_CONFIG_PATH)).unwrap(),
        toml::to_string(&value).unwrap().into_bytes()
    );
}

/// Keep all sections distinct, canonicalize effective values, and support bare parameters.
/// 保持各节独立，规范化生效值，并支持裸参数。
#[test]
fn reads_unified_parameters_from_all_sections_and_legacy_inputs() {
    let temporary = tempfile::tempdir().unwrap();
    let directory = temporary.path();
    fs::create_dir(directory.join("build")).unwrap();
    let path = directory.join(BUILD_CONFIG_PATH);
    fs::write(&path, "Channel=bare\n[Config]\nSchemaVersion=1\n[Distribution]\nDistribution=arch\nChannel=old\nChannel=stable\n[Content]\nPackages=discarded\nPackages=\nPackages=vim,linux\n[Custom]\nChannel=first\n    second\n[Empty]\n").unwrap();
    let parameters = read_plugin_parameters_at(directory).unwrap();
    assert_eq!(parameters.values["RSTAR_CHANNEL"], "bare");
    assert_eq!(
        parameters.values["RSTAR_DISTRIBUTION_CHANNEL"],
        "old\nstable"
    );
    assert_eq!(parameters.values["RSTAR_CUSTOM_CHANNEL"], "first\nsecond");
    assert_eq!(
        parameters.values["RSTAR_CONTENT_PACKAGES"],
        "discarded\nvim\nlinux"
    );
    assert!(parameters.sections.contains("EMPTY"));
    assert_eq!(parameters.field_sections["RSTAR_CUSTOM_CHANNEL"], "CUSTOM");
    fs::remove_file(path).unwrap();
    fs::write(directory.join(LEGACY_BUILD_CONFIG_PATH), "Channel='bare'\nschema_version=1\nsystem_type='arch'\npackages=['vim','linux']\n[distribution_options]\nChannel='stable'\n[Custom]\nChannel='first\\nsecond'\n[Empty]\n").unwrap();
    let legacy = read_plugin_parameters_at(directory).unwrap();
    assert_eq!(legacy.values["RSTAR_CHANNEL"], "bare");
    assert_eq!(legacy.values["RSTAR_DISTRIBUTION_CHANNEL"], "stable");
    assert_eq!(legacy.values["RSTAR_CONTENT_PACKAGES"], "vim\nlinux");
    assert!(legacy.sections.contains("EMPTY"));
}

#[test]
fn rejects_ambiguous_unified_parameter_names() {
    let temporary = tempfile::tempdir().unwrap();
    fs::create_dir(temporary.path().join("build")).unwrap();
    for text in [
        "[Custom]\nKey=one\nkey=two\n",
        "Custom_Key=bare\n[Custom]\nKey=section\n",
        "[Custom]\n[custom]\n",
        "[Bad-Section]\nKey=value\n",
    ] {
        fs::write(temporary.path().join(BUILD_CONFIG_PATH), text).unwrap();
        assert!(
            read_plugin_parameters_at(temporary.path()).is_err(),
            "accepted {text}"
        );
    }
}

/// Verify cross-file lists, independent sections, and repeated scalar rejection.
/// 验证跨文件列表、独立分节以及重复标量拒绝。
/// # Arguments
/// No parameters. 无参数。
#[test]
fn overlays_all_ini_files_and_retains_assignment_boundaries() {
    let (_temporary, store) = fixture();
    store.initialize_system("desktop").unwrap();
    let directory = store.system_layout("desktop").unwrap().config;
    let build = directory.join("build");
    fs::write(
        build.join("00-values.any"),
        "Channel=first\n[Custom]\nValue=one\n    two\n",
    )
    .unwrap();
    fs::write(
        build.join("zz-extra"),
        "Channel=second\n[Custom]\nValue=\nValue=three\n[Content]\nPackages=vim\n",
    )
    .unwrap();
    let parameters = read_plugin_parameters_at(&directory).unwrap();
    assert_eq!(parameters.lists["RSTAR_CHANNEL"], ["first", "second"]);
    assert_eq!(
        parameters.lists["RSTAR_CUSTOM_VALUE"],
        ["one\ntwo", "", "three"]
    );
    assert_eq!(
        store.read_build_config("desktop").unwrap().packages,
        ["vim"]
    );
    fs::write(
        build.join("zz-duplicate"),
        "[Distribution]\nDistribution=arch\n",
    )
    .unwrap();
    assert!(
        store
            .read_build_config("desktop")
            .unwrap_err()
            .to_string()
            .contains("恰好配置一次")
    );
}

#[test]
fn accepts_source_qualified_targets() {
    for target in ["paru", "archlinuxcn/paru", "aur/paru"] {
        crate::software::valid_target(target).unwrap();
    }
    assert!(crate::software::valid_name("aur/paru").is_err());
}

/// Global defaults, frozen system overrides and historical inputs share one validated TTL.
/// 全局默认值、冻结系统覆盖与旧输入使用统一校验的有效期。
/// # Arguments
/// No parameters. 无参数。
#[test]
fn resolves_cache_lifetime_and_rejects_invalid_values() {
    let (_temporary, store) = fixture();
    let directory = store.root().join("input");
    write_build(&directory, "[Config]\nSchemaVersion=1\n");
    assert_eq!(store.cache_reuse_minutes_at(&directory).unwrap(), 0);
    fs::write(
        store.global_config_path(),
        "schema_version=1\ncache_reuse_minutes=10\n",
    )
    .unwrap();
    assert_eq!(store.cache_reuse_minutes_at(&directory).unwrap(), 10);
    for (value, expected) in [("3", 3), ("0", 0)] {
        fs::write(
            directory.join(BUILD_CONFIG_PATH),
            format!("[Config]\nCacheReuseMinutes={value}\n"),
        )
        .unwrap();
        assert_eq!(store.cache_reuse_minutes_at(&directory).unwrap(), expected);
    }
    for value in [
        "",
        "-1",
        "1.5",
        "true",
        "abc",
        "18446744073709551615",
        "1\nCacheReuseMinutes=2",
    ] {
        fs::write(
            directory.join(BUILD_CONFIG_PATH),
            format!("[Config]\nCacheReuseMinutes={value}\n"),
        )
        .unwrap();
        assert!(
            store.cache_reuse_minutes_at(&directory).is_err(),
            "accepted {value}"
        );
    }
    fs::remove_file(directory.join(BUILD_CONFIG_PATH)).unwrap();
    fs::write(
        directory.join(LEGACY_BUILD_CONFIG_PATH),
        "cache_reuse_minutes=4\n",
    )
    .unwrap();
    assert_eq!(store.cache_reuse_minutes_at(&directory).unwrap(), 4);
    fs::write(
        directory.join(LEGACY_BUILD_CONFIG_PATH),
        "schema_version=1\n",
    )
    .unwrap();
    for value in ["-1", "1.5", "true", "'10'", "18446744073709551615"] {
        fs::write(
            store.global_config_path(),
            format!("schema_version=1\ncache_reuse_minutes={value}\n"),
        )
        .unwrap();
        assert!(
            store.cache_reuse_minutes_at(&directory).is_err(),
            "accepted global {value}"
        );
    }
}

/// The storage policy is opt-in, typed, and independent of plugin declarations.
/// 存储策略默认关闭，严格校验类型且不依赖插件声明配置项。
#[test]
fn package_deduplication_boolean_policy() {
    let (_temporary, store) = fixture();
    let directory = store.root().join("dedup-input");
    write_build(&directory, "[Config]\nSchemaVersion=1\n");
    assert!(!read_deduplicate_packages_at(&directory).unwrap());
    for (value, expected) in [
        ("yes", true),
        ("true", true),
        ("on", true),
        ("1", true),
        ("no", false),
        ("false", false),
        ("off", false),
        ("0", false),
    ] {
        fs::write(
            directory.join(BUILD_CONFIG_PATH),
            format!("[Config]\nDeduplicatePackages={value}\n"),
        )
        .unwrap();
        assert_eq!(read_deduplicate_packages_at(&directory).unwrap(), expected);
    }
    for value in ["", "maybe", "2", "yes\nDeduplicatePackages=no"] {
        fs::write(
            directory.join(BUILD_CONFIG_PATH),
            format!("[Config]\nDeduplicatePackages={value}\n"),
        )
        .unwrap();
        assert!(read_deduplicate_packages_at(&directory).is_err());
    }
    fs::remove_file(directory.join(BUILD_CONFIG_PATH)).unwrap();
    for (value, expected) in [
        ("true", Some(true)),
        ("false", Some(false)),
        ("'true'", None),
        ("1", None),
    ] {
        fs::write(
            directory.join(LEGACY_BUILD_CONFIG_PATH),
            format!("deduplicate_packages={value}\n"),
        )
        .unwrap();
        let actual = read_deduplicate_packages_at(&directory);
        match expected {
            Some(expected) => assert_eq!(actual.unwrap(), expected),
            None => assert!(actual.is_err()),
        }
    }
}
