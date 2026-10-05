//! Initialize the dedicated Btrfs workspace and default configuration templates.
//! 初始化专用 Btrfs 工作目录及默认配置模板。

use std::{fs, path::Path, process::Command};

use super::{ConfigStore, Error, Result, SUBVOLUME_NAME, io_error};

/// Global role template; initialization writes it without interpreting its fields.
/// 全局角色模板；初始化只写入文件，不解释其中的字段。
pub(super) const GLOBAL_DEFAULT: &str = "\
# Global RStarSleep settings; system IDs match directory names under systems/.\n\
# RStarSleep 全局配置。系统标识对应 systems/ 下的目录名。\n\
schema_version = 1\n\
# Optional main system ID; omit this setting until a main system is selected.\n\
# 可选的主系统标识；选定主系统前省略此项。\n\
# main_system = \"desktop\"\n\
# Only explicitly listed systems receive the subsystem role.\n\
# 只有显式列出的系统才具有子系统角色。\n\
subsystems = []\n";

/// Editable build template with an explicitly empty package list.
/// 可编辑的构建配置模板，显式提供空的软件包列表。
pub(super) const BUILD_DEFAULT: &str = "\
# Current editable build configuration.\n\
# 当前可编辑的系统构建配置。\n\
schema_version = 1\n\
\n\
# Distribution plugin type; selected from archives in the workspace plugins/ directory.\n\
# 发行版插件类型；从工作目录 plugins/ 内的压缩包自动选择。\n\
system_type = \"arch\"\n\
\n\
# Packages to install; other build options will be added with their features.\n\
# 要安装的软件包。其余构建选项随对应功能添加。\n\
packages = []\n";

/// Reserved runtime template for systems explicitly enabled as subsystems.
/// 为显式启用为子系统的系统预留的运行配置模板。
pub(super) const SUBSYSTEM_DEFAULT: &str = "\
# The runtime module applies this file only when global settings enable this subsystem.\n\
# 只有全局配置启用此子系统时，运行模块才应用本文件。\n\
schema_version = 1\n\
# Runtime options, including application exports, will be added with their features.\n\
# 子系统运行选项（包括应用入口导出）随对应功能添加。\n";

/// Create or reuse `@rstarsleep` within the mounted Btrfs top-level tree (ID 5).
/// 在已挂载的 Btrfs 顶层树（标识为 5）中创建或复用 `@rstarsleep`。
///
/// The caller supplies an absolute directory and permissions to run `btrfs` and write files.
/// 调用者提供绝对目录，并具备执行 `btrfs` 和写入文件所需的权限。
///
/// Existing configuration files are preserved; no mount operation is performed.
/// 保留已有配置文件；不执行挂载操作。
///
/// # Arguments
///
/// - `btrfs_top_level`: Existing absolute directory in the mounted Btrfs top-level tree (ID 5).
///   已挂载 Btrfs 顶层树（标识为 5）中的现有绝对目录。
///
/// # Errors
///
/// Returns an error for an invalid parent, an existing non-subvolume, or a command/I/O failure.
/// 父目录无效、既有路径不是子卷，或命令及 I/O 操作失败时返回错误。
///
/// Initialization is not transactional; files created before a failure remain for a retry.
/// 初始化不是事务操作；失败前已创建的文件会保留，供后续重试使用。
pub fn initialize(btrfs_top_level: impl AsRef<Path>) -> Result<ConfigStore> {
    initialize_with(btrfs_top_level.as_ref(), &BtrfsCommand)
}

/// Isolate Btrfs operations so tests can verify initialization without creating real subvolumes.
/// 隔离 Btrfs 操作，使测试能够在不创建真实子卷的情况下验证初始化行为。
trait Subvolumes {
    /// Require the containing tree ID to be 5; this does not verify the mount boundary.
    /// 要求所属树标识为 5；此检查不验证路径是否恰好位于挂载边界。
    ///
    /// # Arguments
    ///
    /// - `self`: Subvolume backend responsible for validating the containing tree.
    ///   负责校验所属树的子卷操作实现。
    /// - `path`: Directory whose containing Btrfs tree must have ID 5.
    ///   待检查的目录，其所属 Btrfs 树标识必须为 5。
    fn check_top_level(&self, path: &Path) -> Result<()>;
    /// Require the path itself to be a subvolume rather than an ordinary directory.
    /// 要求路径本身是子卷，而非普通目录。
    ///
    /// # Arguments
    ///
    /// - `self`: Subvolume backend responsible for validating existing storage.
    ///   负责校验既有存储路径的子卷操作实现。
    /// - `path`: Existing path that must itself be a Btrfs subvolume.
    ///   待检查的既有路径，其本身必须是 Btrfs 子卷。
    fn check_subvolume(&self, path: &Path) -> Result<()>;
    /// Create the subvolume without falling back to an ordinary directory on failure.
    /// 创建子卷，失败时不使用普通目录替代。
    ///
    /// # Arguments
    ///
    /// - `self`: Subvolume backend responsible for creating storage.
    ///   负责创建存储路径的子卷操作实现。
    /// - `path`: Destination path of the new workspace subvolume.
    ///   新工作目录子卷的目标路径。
    fn create(&self, path: &Path) -> Result<()>;
}

/// Production backend using the installed `btrfs` command-line utility.
/// 使用已安装的 `btrfs` 命令行工具的生产实现。
struct BtrfsCommand;

impl BtrfsCommand {
    /// Pass arguments directly to the process and return trimmed stdout on success.
    /// 将参数直接传递给进程，并在成功时返回去除首尾空白的标准输出。
    ///
    /// No shell is invoked; native path bytes are preserved in the final argument.
    /// 不经过 shell；最后一个参数保留路径的原生字节表示。
    ///
    /// # Arguments
    ///
    /// - `arguments`: Btrfs subcommand and options, excluding the executable and final path argument.
    ///   Btrfs 子命令及选项，不包含可执行程序名称和最后的路径参数。
    /// - `path`: Native filesystem path appended as the final process argument.
    ///   以原生文件系统路径形式追加到进程参数末尾的路径。
    fn run(arguments: &[&str], path: &Path) -> Result<String> {
        let output = Command::new("btrfs")
            .args(arguments)
            .arg(path)
            .output()
            .map_err(|e| Error::Btrfs(format!("无法运行 btrfs: {e}")))?;
        if !output.status.success() {
            return Err(Error::Btrfs(format!(
                "btrfs {} {}: {}",
                arguments.join(" "),
                path.display(),
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
    }
}

impl Subvolumes for BtrfsCommand {
    /// Query the containing Btrfs tree and reject any tree ID other than 5.
    /// 查询所属 Btrfs 树，并拒绝标识不为 5 的树。
    ///
    /// # Arguments
    ///
    /// - `self`: Stateless backend that invokes the installed btrfs utility.
    ///   调用已安装 btrfs 工具的无状态实现。
    /// - `path`: Directory passed to btrfs inspect-internal rootid to check for tree ID 5.
    ///   传递给 btrfs inspect-internal rootid 的目录，用于检查树标识是否为 5。
    fn check_top_level(&self, path: &Path) -> Result<()> {
        let id = Self::run(&["inspect-internal", "rootid", "--"], path)?;
        if id != "5" {
            return Err(Error::Btrfs(format!(
                "{} 属于子卷 {id}，请提供 subvolid=5 的 Btrfs 顶层路径",
                path.display()
            )));
        }
        Ok(())
    }

    /// Use btrfs subvolume show to verify that an existing path is a subvolume.
    /// 使用 btrfs subvolume show 验证既有路径是否为子卷。
    ///
    /// # Arguments
    ///
    /// - `self`: Stateless backend that invokes the installed btrfs utility.
    ///   调用已安装 btrfs 工具的无状态实现。
    /// - `path`: Existing path passed to btrfs subvolume show.
    ///   传递给 btrfs subvolume show 的既有路径。
    fn check_subvolume(&self, path: &Path) -> Result<()> {
        Self::run(&["subvolume", "show", "--"], path).map(|_| ())
    }

    /// Create a real workspace subvolume with the btrfs utility.
    /// 使用 btrfs 工具创建真实工作目录子卷。
    ///
    /// # Arguments
    ///
    /// - `self`: Stateless backend that invokes the installed btrfs utility.
    ///   调用已安装 btrfs 工具的无状态实现。
    /// - `path`: Destination passed to btrfs subvolume create.
    ///   传递给 btrfs subvolume create 的目标路径。
    fn create(&self, path: &Path) -> Result<()> {
        Self::run(&["subvolume", "create", "--"], path).map(|_| ())
    }
}

/// Validate the storage location before creating workspace directories and templates.
/// 在创建工作目录和模板之前，先校验存储位置。
///
/// # Arguments
///
/// - `parent`: Existing absolute directory where the workspace subvolume is located.
///   用于存放工作目录子卷的现有绝对目录。
/// - `btrfs`: Backend used to validate and create subvolumes; tests provide a simulated backend.
///   用于校验和创建子卷的实现；测试时传入模拟实现。
fn initialize_with(parent: &Path, btrfs: &impl Subvolumes) -> Result<ConfigStore> {
    if !parent.is_absolute() || !parent.is_dir() {
        return Err(Error::Invalid("请提供已挂载的 Btrfs 顶层绝对目录".into()));
    }
    btrfs.check_top_level(parent)?;
    let root = parent.join(SUBVOLUME_NAME);
    // Inspect the entry itself so a symlink cannot stand in for the dedicated subvolume.
    // 检查目录项本身，避免符号链接替代专用子卷。
    match fs::symlink_metadata(&root) {
        Ok(metadata) => {
            if !metadata.is_dir() || metadata.file_type().is_symlink() {
                return Err(Error::Invalid(format!(
                    "{} 必须是独立子卷目录",
                    root.display()
                )));
            }
            btrfs.check_subvolume(&root)?;
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => btrfs.create(&root)?,
        Err(e) => return Err(io_error(&root, e)),
    }
    let store = ConfigStore::new(root)?;
    store.initialize_directories()?;
    Ok(store)
}

#[cfg(test)]
mod tests {
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
}
