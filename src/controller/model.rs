//! Shared build interfaces and errors independent of distribution commands.
//! 独立于发行版命令的构建接口与错误。

use std::{
    io,
    path::{Path, PathBuf},
};

use super::plugins::LoadedPlugin;

/// Result shared by orchestration, base-system, and software workflows.
/// 编排、基础系统和软件流程共用的结果。
pub type Result<T> = std::result::Result<T, Error>;

/// A build failure with the system, command, or filesystem context retained.
/// 保留系统、命令或文件系统上下文的构建错误。
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Configuration could not be read or validated.
    /// 配置读取或校验失败。
    #[error(transparent)]
    Config(#[from] crate::config::Error),
    /// A plugin could not be loaded or invoked.
    /// 插件加载或调用失败。
    #[error(transparent)]
    Plugin(#[from] super::plugins::Error),
    /// A filesystem operation failed.
    /// 文件系统操作失败。
    #[error("无法访问 {path}: {source}")]
    Io {
        /// Filesystem path involved in the failure.
        /// 失败涉及的文件系统路径。
        path: PathBuf,
        /// Original filesystem error.
        /// 原始文件系统错误。
        #[source]
        source: io::Error,
    },
    /// A native command exited unsuccessfully.
    /// 原生命令执行失败。
    #[error("{command} 失败（{status}）: {stderr}")]
    Command {
        /// Command and arguments for diagnostics.
        /// 用于诊断的命令和参数。
        command: String,
        /// Process status.
        /// 进程状态。
        status: String,
        /// Captured command diagnostics.
        /// 捕获的命令诊断信息。
        stderr: String,
    },
    /// Storage, state, or package output is invalid.
    /// 存储、状态或包输出无效。
    #[error("构建无效: {0}")]
    Invalid(String),
    /// Independent systems were attempted, but at least one failed.
    /// 已尝试独立系统，但至少一个失败。
    #[error("部分系统构建失败:\n{}", .0.join("\n"))]
    Batch(Vec<String>),
}

/// Frozen inputs visible to plugin operations during one version build.
/// 一个版本构建过程中插件操作可见的冻结输入。
#[derive(Debug, Clone)]
pub struct BuildContext {
    /// Stable managed-system identifier.
    /// 稳定的被管理系统标识。
    pub system_id: String,
    /// Target architecture selected before cache reuse.
    /// 缓存复用前确定的目标架构。
    pub architecture: String,
    /// Explicit [Distribution] settings other than Distribution, frozen before execution.
    /// 执行前冻结的 [Distribution] 显式设置，不包括 Distribution。
    pub distribution_options: std::collections::BTreeMap<String, String>,
    /// Version's frozen configuration directory.
    /// 版本的冻结配置目录。
    pub config: PathBuf,
    /// Writable CoW copies of globally named persistent subvolumes.
    /// 全局命名持久子卷的可写 CoW 副本。
    pub persistent_mounts: Vec<PersistentMount>,
}

/// Bind one transactional subvolume below the plugin working directory.
/// 在插件工作目录下绑定一个事务子卷。
#[derive(Debug, Clone)]
pub struct PersistentMount {
    pub source: PathBuf,
    pub target: PathBuf,
}

/// A completely built version, ready for later boot or runtime integration.
/// 完整构建的版本，供后续启动或运行集成使用。
#[derive(Debug, Clone)]
pub struct BuildOutcome {
    /// System owning the version.
    /// 版本所属系统。
    pub system_id: String,
    /// Monotonically allocated version identifier.
    /// 单调递增分配的版本标识。
    pub version: String,
    /// Read-only completed system-root subvolume.
    /// 已完成的只读系统根子卷。
    pub root: PathBuf,
}

/// Required CoW storage and checked plugin execution operations.
/// 必需的 CoW 存储和已检查插件执行操作。
pub trait BuildBackend {
    /// Create a writable subvolume, failing rather than using an ordinary directory.
    /// 创建可写子卷；失败时不使用普通目录替代。
    ///
    /// # Arguments
    ///
    /// - `self`: Backend executing native storage operations.
    ///   执行原生存储操作的后端。
    /// - `path`: New absolute subvolume path.
    ///   新子卷的绝对路径。
    ///
    /// # Errors
    ///
    /// Returns an error if creation or validation fails.
    /// 创建或校验失败时返回错误。
    fn create_subvolume(&self, path: &Path) -> Result<()>;

    /// Require a real subvolume with the specified read-only state.
    /// 要求真实子卷具有指定只读状态。
    ///
    /// # Arguments
    ///
    /// - `self`: Storage validation backend.
    ///   存储校验后端。
    /// - `path`: Existing subvolume to check.
    ///   待检查的已有子卷。
    /// - `readonly`: Expected read-only property.
    ///   预期只读属性。
    ///
    /// # Errors
    ///
    /// Returns an error for invalid storage or an unexpected property.
    /// 存储无效或属性不符时返回错误。
    fn check_subvolume(&self, path: &Path, readonly: bool) -> Result<()>;

    /// Create a forced CoW snapshot without a full-copy fallback.
    /// 创建强制 CoW 快照，不提供全量复制回退。
    ///
    /// # Arguments
    ///
    /// - `self`: Snapshot backend.
    ///   快照后端。
    /// - `source`: Existing source subvolume.
    ///   已有源子卷。
    /// - `destination`: New snapshot path.
    ///   新快照路径。
    /// - `readonly`: Whether the new snapshot is read-only.
    ///   新快照是否只读。
    ///
    /// # Errors
    ///
    /// Returns an error if snapshot creation or validation fails.
    /// 快照创建或校验失败时返回错误。
    fn snapshot(&self, source: &Path, destination: &Path, readonly: bool) -> Result<()>;

    /// Delete an owned temporary subvolume after its processes have exited.
    /// 进程退出后删除本次构建所属的临时子卷。
    ///
    /// # Arguments
    ///
    /// - `self`: Cleanup backend.
    ///   清理后端。
    /// - `path`: Temporary subvolume owned by the current build.
    ///   当前构建所属的临时子卷。
    ///
    /// # Errors
    ///
    /// Returns an error if validation or deletion fails.
    /// 校验或删除失败时返回错误。
    fn delete_subvolume(&self, path: &Path) -> Result<()>;

    /// Reject descendants that would be omitted by a non-recursive snapshot.
    /// 拒绝不能由普通快照覆盖的后代子卷。
    fn check_no_nested_subvolumes(&self, path: &Path) -> Result<()> {
        Err(Error::Invalid(format!(
            "此后端不能检查嵌套子卷: {}",
            path.display()
        )))
    }

    /// Delete an owned work root including subvolumes created during plugin execution.
    /// 删除所属工作根及插件运行时创建的子卷。
    fn delete_subvolume_recursive(&self, path: &Path) -> Result<()> {
        self.check_no_nested_subvolumes(path)?;
        self.delete_subvolume(path)
    }

    /// Run a checked plugin operation and return query output.
    /// 运行已检查的插件操作并返回查询输出。
    ///
    /// # Arguments
    ///
    /// - `self`: Plugin execution backend.
    ///   插件执行后端。
    /// - `plugin`: Loaded distribution resources.
    ///   已加载的发行版资源。
    /// - `operation`: Fixed operation identifier.
    ///   固定操作标识。
    /// - `args`: Individually passed task arguments.
    ///   独立传递的任务参数。
    /// - `root`: Writable current system root.
    ///   当前可写系统根。
    /// - `context`: Frozen version configuration and system identity.
    ///   冻结的版本配置和系统标识。
    ///
    /// # Errors
    ///
    /// Returns an error for missing tools, execution failures, or invalid query output.
    /// 工具缺失、执行失败或查询输出无效时返回错误。
    fn invoke(
        &self,
        plugin: &LoadedPlugin,
        operation: &str,
        args: &[String],
        root: &Path,
        context: &BuildContext,
    ) -> Result<String>;
    /// Execute a copied build script inside the target system.
    /// 在目标系统内部执行已复制的构建脚本。
    /// # Arguments
    /// - `self`: Target execution backend.
    ///   目标执行后端。
    /// - `root`: Writable script-stage root.
    ///   可写脚本阶段根。
    /// - `directory`: Absolute guest working directory.
    ///   容器内绝对工作目录。
    /// - `entrypoint`: Relative shell filename.
    ///   相对 shell 文件名。
    /// - `context`: Frozen build inputs.
    ///   冻结构建输入。
    /// # Errors
    /// Returns execution failures; unsupported backends fail before running scripts.
    /// 返回执行失败；不支持的后端在执行脚本前报错。
    fn run_script(
        &self,
        root: &Path,
        directory: &Path,
        entrypoint: &Path,
        context: &BuildContext,
    ) -> Result<()> {
        let _ = (root, directory, entrypoint, context);
        Err(Error::Invalid("此构建后端不支持 script 执行".into()))
    }
}
