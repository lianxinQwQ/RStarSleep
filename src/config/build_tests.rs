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

/// Write one build input preserving its source TOML for independent readers.
/// 写入一份构建输入，保留源 TOML 供独立读取接口使用。
///
/// # Arguments
///
/// - `directory`: Current or archived configuration root.
///   当前或留档配置根。
/// - `text`: Exact build TOML to write.
///   待写入的准确构建 TOML。
fn write_build(directory: &Path, text: &str) {
    fs::create_dir_all(directory.join("build")).unwrap();
    fs::write(directory.join("build/config.toml"), text).unwrap();
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
        "schema_version=1\ndistribution='arch'\narchitecture='x86_64'\npackages=['vim', 'libstdc++', 'vim']\ncustom='retained'\n",
    );
    let config = store.read_build_config("desktop").unwrap();
    assert_eq!(config.system_type, "arch");
    assert_eq!(config.architecture.as_deref(), Some("x86_64"));
    assert_eq!(config.packages, ["vim", "libstdc++", "vim"]);
    write_build(
        &layout.versions.join("v00000001/config"),
        "schema_version=1\nsystem_type='arch'\npackages=['nano']\n",
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
        "schema_version=1\nsystem_type='arch'\n",
        "schema_version=2\nsystem_type='arch'\npackages=[]\n",
        "schema_version=1\nsystem_type='../arch'\npackages=[]\n",
        "schema_version=1\nsystem_type='arch'\narchitecture=''\npackages=[]\n",
    ] {
        write_build(&directory, text);
        assert!(
            read_build_config_at(&directory).is_err(),
            "accepted {text:?}"
        );
    }
    for package in [
        "",
        "-flag",
        ".hidden",
        "pkg:version",
        "repo/pkg",
        "a b",
        "name>=1",
    ] {
        write_build(
            &directory,
            &format!("schema_version=1\nsystem_type='arch'\npackages=['{package}']\n"),
        );
        assert!(
            read_build_config_at(&directory).is_err(),
            "accepted {package:?}"
        );
    }
    write_build(
        &directory,
        "schema_version=1\nsystem_type='arch'\npackages=[]\n",
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
