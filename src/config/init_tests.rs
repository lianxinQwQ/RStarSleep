//! Verify preservation and failure handling through a simulated Btrfs backend.
//! 通过模拟 Btrfs 实现验证文件保留与错误处理行为。

use std::cell::Cell;

use super::*;

/// Simulate subvolume operations using temporary directories and injected failures.
/// 使用临时目录和注入的错误模拟子卷操作。
#[derive(Default)]
struct FakeBtrfs {
    creates: Cell<usize>,
    reject_existing: bool,
    reject_create: bool,
}

impl Subvolumes for FakeBtrfs {
    /// Accept the test parent path without invoking Btrfs.
    /// 直接接受测试父目录路径，不调用 Btrfs。
    ///
    /// # Arguments
    ///
    /// - `self`: Simulated backend; no state is read or changed by this check.
    ///   模拟实现；此检查不读取或修改其状态。
    /// - `_`: Ignored parent path because tests bypass real filesystem tree validation.
    ///   被忽略的父目录路径；测试跳过真实文件系统树校验。
    fn check_top_level(&self, _: &Path) -> Result<()> {
        Ok(())
    }
    /// Simulate acceptance or rejection of an existing workspace subvolume.
    /// 模拟接受或拒绝既有工作目录子卷。
    ///
    /// # Arguments
    ///
    /// - `self`: Simulated backend whose reject_existing flag controls the result.
    ///   模拟实现，通过 reject_existing 标志控制检查结果。
    /// - `_`: Ignored path; the result depends only on the injected failure flag.
    ///   被忽略的路径；结果只取决于注入的失败标志。
    fn check_subvolume(&self, _: &Path) -> Result<()> {
        if self.reject_existing {
            Err(Error::Btrfs("普通目录".into()))
        } else {
            Ok(())
        }
    }
    /// Simulate creation with an ordinary test directory or return an injected failure.
    /// 通过普通测试目录模拟创建，或返回注入的失败错误。
    ///
    /// # Arguments
    ///
    /// - `self`: Simulated backend supplying the failure flag and creation counter.
    ///   提供失败标志和创建次数计数器的模拟实现。
    /// - `path`: Temporary directory used to represent the created subvolume.
    ///   用于代表已创建子卷的临时目录路径。
    fn create(&self, path: &Path) -> Result<()> {
        if self.reject_create {
            return Err(Error::Btrfs("权限不足".into()));
        }
        self.creates.set(self.creates.get() + 1);
        fs::create_dir(path).map_err(|e| io_error(path, e))
    }
}

/// Verify that repeated initialization creates one subvolume and preserves edited global settings.
/// 验证重复初始化仅创建一次子卷，并保留已修改的全局配置。
///
/// # Arguments
///
/// No parameters.
/// 无参数。
#[test]
fn initializes_once_and_preserves_existing_configuration() {
    let temp = tempfile::tempdir().unwrap();
    let btrfs = FakeBtrfs::default();
    let store = initialize_with(temp.path(), &btrfs).unwrap();
    let edited = "schema_version = 1\nmain_system = \"desktop\"\nsubsystems = []\n";
    fs::write(store.global_config_path(), edited).unwrap();
    initialize_with(temp.path(), &btrfs).unwrap();
    assert_eq!(btrfs.creates.get(), 1);
    assert_eq!(
        fs::read_to_string(store.global_config_path()).unwrap(),
        edited
    );
    assert!(store.root().join("systems").is_dir());
}

/// Verify that an existing ordinary directory is rejected before configuration files are written.
/// 验证既有普通目录会在写入配置文件之前被拒绝。
///
/// # Arguments
///
/// No parameters.
/// 无参数。
#[test]
fn refuses_existing_ordinary_directory() {
    let temp = tempfile::tempdir().unwrap();
    fs::create_dir(temp.path().join(SUBVOLUME_NAME)).unwrap();
    let btrfs = FakeBtrfs {
        reject_existing: true,
        ..Default::default()
    };
    assert!(initialize_with(temp.path(), &btrfs).is_err());
    assert!(!temp.path().join(SUBVOLUME_NAME).join("config").exists());
}

/// Verify that subvolume creation failure does not create a fallback directory.
/// 验证子卷创建失败后不会创建替代的普通目录。
///
/// # Arguments
///
/// No parameters.
/// 无参数。
#[test]
fn does_not_fall_back_to_directory_when_subvolume_creation_fails() {
    let temp = tempfile::tempdir().unwrap();
    let btrfs = FakeBtrfs {
        reject_create: true,
        ..Default::default()
    };
    assert!(initialize_with(temp.path(), &btrfs).is_err());
    assert!(!temp.path().join(SUBVOLUME_NAME).exists());
}

/// Verify that initialization preserves global settings without interpreting future options.
/// 验证初始化会保留全局配置，且不解释尚未实现的选项。
///
/// # Arguments
///
/// No parameters.
/// 无参数。
#[test]
fn preserves_global_config_without_reading_unimplemented_options() {
    let temp = tempfile::tempdir().unwrap();
    let btrfs = FakeBtrfs::default();
    let store = initialize_with(temp.path(), &btrfs).unwrap();
    fs::write(
        store.global_config_path(),
        "# Custom global options.\n# 自定义全局选项。\nfuture_option = true\n",
    )
    .unwrap();
    initialize_with(temp.path(), &btrfs).unwrap();
    assert_eq!(
        fs::read_to_string(store.global_config_path()).unwrap(),
        "# Custom global options.\n# 自定义全局选项。\nfuture_option = true\n"
    );
}
