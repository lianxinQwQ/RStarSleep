//! Verify package parsing and path ownership using isolated temporary workspaces.
//! 使用隔离的临时工作目录验证软件包解析与路径归属。

use super::*;

/// Create an initialized test workspace and retain its temporary-directory guard.
/// 创建已初始化的测试工作目录，并保留控制其存活周期的临时目录对象。
///
/// # Arguments
///
/// No parameters.
/// 无参数。
fn fixture() -> (tempfile::TempDir, ConfigStore) {
    let temp = tempfile::tempdir().unwrap();
    let store = ConfigStore::new(temp.path()).unwrap();
    store.initialize_directories().unwrap();
    (temp, store)
}

/// Verify that repeated system initialization preserves edited files and does not assign roles.
/// 验证重复初始化系统会保留已修改文件，且不分配运行角色。
///
/// # Arguments
///
/// No parameters.
/// 无参数。
#[test]
fn system_initialization_preserves_files_and_leaves_roles_unassigned() {
    let (_temp, store) = fixture();
    let layout = store.initialize_system("desktop").unwrap();
    let global_before = fs::read_to_string(store.global_config_path()).unwrap();
    let custom = "[Content]\nPackages=vim\n";
    let environment = layout.config.join("build/environment.conf");
    assert!(
        fs::read_to_string(&environment)
            .unwrap()
            .contains("[Environment]")
    );
    assert!(
        crate::config::read_environment_at(&layout.config)
            .unwrap()
            .is_empty()
    );
    fs::write(&environment, "[Environment]\nhttp_proxy=$http_proxy\n").unwrap();
    fs::write(layout.config.join("build/config.conf"), custom).unwrap();
    store.initialize_system("desktop").unwrap();
    assert_eq!(
        fs::read_to_string(environment).unwrap(),
        "[Environment]\nhttp_proxy=$http_proxy\n"
    );
    assert_eq!(
        fs::read_to_string(store.global_config_path()).unwrap(),
        global_before
    );
    assert_eq!(
        fs::read_to_string(layout.config.join("build/config.conf")).unwrap(),
        custom
    );
    assert!(layout.persistent.is_dir());
    assert!(layout.versions.is_dir());
    assert!(layout.cache.is_dir());
    assert!(layout.config.join("script").is_dir());
}

/// Verify that package reads ignore other build options and unrelated configuration files.
/// 验证软件包读取会忽略其他构建选项及无关配置文件。
///
/// # Arguments
///
/// No parameters.
/// 无参数。
#[test]
fn reads_packages_without_interpreting_other_options_or_runtime_config() {
    let (_temp, store) = fixture();
    let layout = store.initialize_system("desktop").unwrap();
    fs::write(
        layout.config.join("build/config.conf"),
        "[Content]\nPackages=linux vim linux\n[Future]\nEnabled=true\n",
    )
    .unwrap();
    fs::write(
        layout.config.join("subsystem/config.toml"),
        "not valid TOML",
    )
    .unwrap();
    fs::write(store.global_config_path(), "not valid TOML").unwrap();
    assert_eq!(
        store.read_packages("desktop").unwrap().packages,
        ["linux", "vim", "linux"]
    );
}

/// Verify that the generated build template provides a valid empty package list.
/// 验证生成的构建模板提供有效的空软件包列表。
///
/// # Arguments
///
/// No parameters.
/// 无参数。
#[test]
fn reads_empty_default_packages() {
    let (_temp, store) = fixture();
    store.initialize_system("desktop").unwrap();
    assert!(store.read_packages("desktop").unwrap().packages.is_empty());
}

/// Verify initialization installs the embedded archive and current type selects it automatically.
/// 验证初始化安装内嵌压缩包，并由当前系统类型自动选择。
///
/// # Arguments
///
/// No parameters.
/// 无参数。
#[test]
fn installs_and_selects_embedded_arch_plugin_without_registry_settings() {
    let (_temp, store) = fixture();
    store.initialize_system("desktop").unwrap();
    assert!(store.plugins_path().join("arch.tar.gz").is_file());
    assert_eq!(
        store.read_system_type("desktop").unwrap().system_type,
        "arch"
    );
    let plugin = crate::controller::plugins::for_system(&store, "desktop").unwrap();
    assert_eq!(plugin.metadata.id, "arch");
    assert!(plugin.root().join("data/mirrorlist").is_file());
    assert!(
        fs::read_to_string(plugin.operation_path("base_install").unwrap())
            .unwrap()
            .contains("base_install")
    );
}

/// Verify repeated initialization preserves a user-provided archive byte for byte.
/// 验证重复初始化逐字节保留用户提供的压缩包。
///
/// # Arguments
///
/// No parameters.
/// 无参数。
#[test]
fn preserves_user_archives_on_reinitialization() {
    let (_temp, store) = fixture();
    let archive = store.plugins_path().join("arch.tar.gz");
    fs::write(&archive, b"user archive").unwrap();
    store.initialize_directories().unwrap();
    assert_eq!(fs::read(archive).unwrap(), b"user archive");
}

/// Verify explicit updates replace the preset archive without changing other workspace resources.
/// 验证显式更新替换预设压缩包，且保留工作目录内的其他资源。
#[test]
fn updates_presets_and_preserves_configuration_and_other_plugins() {
    let (_temp, store) = fixture();
    let archive = store.plugins_path().join("arch.tar.gz");
    let embedded = fs::read(&archive).unwrap();
    let layout = store.initialize_system("desktop").unwrap();
    let configuration = [
        store.global_config_path(),
        layout.config.join("build/config.conf"),
        layout.config.join("subsystem/config.toml"),
    ];
    for path in &configuration {
        fs::write(path, b"user configuration").unwrap();
    }
    let custom_plugin = store.plugins_path().join("custom.tar.gz");
    fs::write(&custom_plugin, b"custom plugin").unwrap();
    let custom_directory = store.plugins_path().join("custom");
    fs::create_dir(&custom_directory).unwrap();
    fs::write(custom_directory.join("plugin.toml"), b"custom metadata").unwrap();
    fs::write(&archive, b"old preset").unwrap();

    store.update_presets().unwrap();
    assert_eq!(fs::read(&archive).unwrap(), embedded);
    for path in configuration {
        assert_eq!(fs::read(path).unwrap(), b"user configuration");
    }
    assert_eq!(fs::read(custom_plugin).unwrap(), b"custom plugin");
    assert_eq!(
        fs::read(custom_directory.join("plugin.toml")).unwrap(),
        b"custom metadata"
    );

    fs::remove_file(&archive).unwrap();
    store.update_presets().unwrap();
    assert_eq!(fs::read(&archive).unwrap(), embedded);
    store.update_presets().unwrap();
    assert_eq!(fs::read(&archive).unwrap(), embedded);
}

/// Verify an update publishes a new file instead of truncating the previously installed archive.
/// 验证更新发布新文件，而非截断原有压缩包。
#[cfg(unix)]
#[test]
fn preset_update_atomically_replaces_the_existing_file() {
    use std::io::Read;

    let (_temp, store) = fixture();
    let archive = store.plugins_path().join("arch.tar.gz");
    let embedded = fs::read(&archive).unwrap();
    fs::write(&archive, b"old preset").unwrap();
    let mut previous = fs::File::open(&archive).unwrap();
    store.update_presets().unwrap();
    let mut old_contents = Vec::new();
    previous.read_to_end(&mut old_contents).unwrap();
    assert_eq!(old_contents, b"old preset");
    assert_eq!(fs::read(&archive).unwrap(), embedded);
    assert_eq!(fs::read_dir(store.plugins_path()).unwrap().count(), 1);
}

/// Verify preset installation and updates reject directory and symlink destinations.
/// 验证预设安装及更新拒绝目录和符号链接目标。
#[cfg(unix)]
#[test]
fn preset_operations_reject_non_file_destinations() {
    use std::os::unix::fs::symlink;

    let (temp, store) = fixture();
    let archive = store.plugins_path().join("arch.tar.gz");
    fs::remove_file(&archive).unwrap();
    fs::create_dir(&archive).unwrap();
    fs::write(archive.join("keep"), b"directory contents").unwrap();
    assert!(store.initialize_directories().is_err());
    assert!(store.update_presets().is_err());
    assert_eq!(
        fs::read(archive.join("keep")).unwrap(),
        b"directory contents"
    );
    fs::remove_dir_all(&archive).unwrap();

    let outside = temp.path().join("outside");
    fs::write(&outside, b"outside contents").unwrap();
    symlink(&outside, &archive).unwrap();
    assert!(store.initialize_directories().is_err());
    assert!(store.update_presets().is_err());
    assert!(
        fs::symlink_metadata(&archive)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(fs::read(&outside).unwrap(), b"outside contents");

    fs::remove_file(&outside).unwrap();
    assert!(store.initialize_directories().is_err());
    assert!(store.update_presets().is_err());
    assert!(!outside.exists());
}

/// Reject missing or unsafe distribution settings independently of other build fields.
/// 独立于其他构建字段拒绝缺失或不安全的发行版设置。
///
/// # Arguments
///
/// No parameters.
/// 无参数。
#[test]
fn validates_system_types() {
    let (_temp, store) = fixture();
    let layout = store.initialize_system("desktop").unwrap();
    let path = layout.config.join("build/config.conf");
    for text in [
        "[Content]\nPackages=",
        "[Distribution]\nDistribution=../arch",
        "[Distribution]\nDistribution=",
        "[Distribution]\nDistribution=arch\nArchitecture=../x86_64",
    ] {
        fs::write(&path, text).unwrap();
        assert!(store.read_system_type("desktop").is_err());
    }
    fs::write(
        &path,
        "[Distribution]\nDistribution=arch\nArchitecture=x86_64",
    )
    .unwrap();
    let config = store.read_system_type("desktop").unwrap();
    assert_eq!(config.system_type, "arch");
    assert_eq!(config.architecture.as_deref(), Some("x86_64"));
}

/// Verify that recorded package options are read independently and missing records report errors.
/// 验证版本留档的软件包选项独立于当前配置读取，且缺失留档会报告错误。
///
/// # Arguments
///
/// No parameters.
/// 无参数。
#[test]
fn reads_version_packages_independently_of_current_config() {
    let (_temp, store) = fixture();
    let layout = store.initialize_system("desktop").unwrap();
    let record = layout.versions.join("v1/config/build");
    fs::create_dir_all(&record).unwrap();
    fs::write(record.join("config.toml"), "packages = ['linux', 'vim']").unwrap();
    assert_eq!(
        store
            .read_version_packages("desktop", "v1")
            .unwrap()
            .packages,
        ["linux", "vim"]
    );
    assert!(store.read_packages("desktop").unwrap().packages.is_empty());
    assert!(matches!(
        store.read_version_packages("desktop", "missing"),
        Err(Error::Io { .. })
    ));
}

/// Verify that invalid identifiers and relative workspace paths are rejected.
/// 验证无效标识和相对工作目录路径会被拒绝。
///
/// # Arguments
///
/// No parameters.
/// 无参数。
#[test]
fn rejects_paths_that_escape_system_version_and_stage_directories() {
    let (_temp, store) = fixture();
    for id in ["", ".", "..", "../other", "/tmp/other", "a/b", "a\\b"] {
        assert!(store.initialize_system(id).is_err());
        assert!(store.read_packages(id).is_err());
        assert!(store.read_version_packages("desktop", id).is_err());
        assert!(store.build_cache_path("desktop", "v1", id).is_err());
    }
    assert!(ConfigStore::new("relative").is_err());
    assert!(
        store
            .build_cache_path("desktop", "v1", "packages")
            .unwrap()
            .ends_with("systems/desktop/cache/v1/packages")
    );
}

/// Verify that missing fields, malformed INI, invalid package tokens, and missing files fail.
/// 验证字段缺失、INI 格式错误、包名无效及文件缺失都会失败。
///
/// # Arguments
///
/// No parameters.
/// 无参数。
#[test]
fn rejects_missing_malformed_and_invalid_package_options() {
    let (_temp, store) = fixture();
    let layout = store.initialize_system("desktop").unwrap();
    for text in [
        "",
        "Packages=vim",
        "[Content\nPackages=vim",
        "[Content]\n=vim",
        "[Content]\nPackages=['vim']",
        "[Other]\nPackages=vim",
        "[Content]\nPackages=-flag",
        "[Content]\nvim",
    ] {
        fs::write(layout.config.join("build/config.conf"), text).unwrap();
        assert!(store.read_packages("desktop").is_err(), "accepted: {text}");
    }
    fs::remove_file(layout.config.join("build/config.conf")).unwrap();
    assert!(store.read_packages("desktop").is_err());
}

/// Verify that system initialization requires an initialized manager workspace.
/// 验证初始化系统必须先有已初始化的管理器工作目录。
///
/// # Arguments
///
/// No parameters.
/// 无参数。
#[test]
fn refuses_system_initialization_before_manager_initialization() {
    let temp = tempfile::tempdir().unwrap();
    let store = ConfigStore::new(temp.path()).unwrap();
    assert!(store.initialize_system("desktop").is_err());
    assert!(!temp.path().join("systems").exists());
}
