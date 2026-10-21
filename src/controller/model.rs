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
    /// All explicit effective settings keyed by canonical RSTAR_ names, frozen before execution.
    /// 执行前冻结的全部显式生效设置，以规范 RSTAR_ 名称索引。
    pub parameters: std::collections::BTreeMap<String, String>,
    /// Original frozen assignment lists delivered as RSTAR_PARAMETERS_JSON.
    /// 通过 RSTAR_PARAMETERS_JSON 传递的原始冻结赋值列表。
    pub parameter_lists: std::collections::BTreeMap<String, Vec<String>>,
    /// Controller-owned [Environment] resolved once before any plugin execution.
    /// 任何插件执行前一次性解析的控制器所属 [Environment]。
    pub environment: crate::config::Environment,
    /// Distribution-only settings retained for the unified download operation.
    /// 为统一下载操作保留的仅 Distribution 设置。
    pub distribution_options: std::collections::BTreeMap<String, String>,
    /// Version's frozen configuration directory.
    /// 版本的冻结配置目录。
    pub config: PathBuf,
    /// Writable CoW copies of globally named persistent subvolumes.
    /// 全局命名持久子卷的可写 CoW 副本。
    pub persistent_mounts: Vec<PersistentMount>,
    /// Current-system persistent objects bound at their final guest paths after scripts.
    /// 脚本后绑定到最终容器路径的当前系统持久对象。
    pub inherited_mounts: Vec<PersistentMount>,
}

/// Bind one transactional subvolume below the plugin working directory.
/// 在插件工作目录下绑定一个事务子卷。
#[derive(Debug, Clone)]
pub struct PersistentMount {
    pub source: PathBuf,
    pub target: PathBuf,
}

/// A completed version or a prepared version awaiting first-boot inheritance.
/// 已完成版本或等待首次启动继承的准备版本。
#[derive(Debug, Clone)]
pub struct BuildOutcome {
    /// System owning the version.
    /// 版本所属系统。
    pub system_id: String,
    /// Monotonically allocated version identifier.
    /// 单调递增分配的版本标识。
    pub version: String,
    /// Writable root or independent read-only prepared-root for a pending version.
    /// 可写系统根，或待继承版本独立保存的只读 prepared-root。
    pub root: PathBuf,
}

/// Required CoW storage and checked plugin execution operations.
/// 必需的 CoW 存储和已检查插件执行操作。
pub trait BuildBackend {
    /// Share equal file data while preserving independent inodes and metadata.
    /// 共享一致文件数据，保留独立 inode 和元数据。
    /// # Arguments
    /// - `source`, `target`: Pinned regular source and writable destination. 固定普通来源及可写目标。
    fn deduplicate_file(
        &self,
        _source: &std::fs::File,
        _target: &std::fs::File,
    ) -> Result<super::DedupOutcome> {
        Ok(super::DedupOutcome::Unsupported)
    }

    /// Query optional owned-file candidates inside the frozen build environment.
    /// 在冻结构建环境内查询可选所属文件候选项。
    /// # Arguments
    /// - `plugin`, `root`, `context`: Plugin and frozen writable target. 插件及冻结可写目标。
    /// # Errors
    /// Returns unsupported capability or plugin execution failures.
    /// 返回不支持能力或插件执行失败。
    fn query_package_files(
        &self,
        plugin: &LoadedPlugin,
        root: &Path,
        context: &BuildContext,
    ) -> Result<Vec<u8>> {
        Ok(self
            .invoke(plugin, "query_package_files", &[], root, context)?
            .into_bytes())
    }
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

    /// Enumerate every descendant subvolume, relative to this tree's root.
    fn nested_subvolumes(&self, path: &Path) -> Result<Vec<PathBuf>> {
        self.check_no_nested_subvolumes(path)?;
        Ok(Vec::new())
    }

    /// Identify a subvolume without treating ordinary directories as snapshots.
    fn is_subvolume(&self, path: &Path) -> Result<bool> {
        Ok(self.check_subvolume(path, false).is_ok() || self.check_subvolume(path, true).is_ok())
    }

    /// Snapshot the complete hierarchy; Btrfs parent snapshots omit child contents.
    fn snapshot_recursive(&self, source: &Path, destination: &Path, readonly: bool) -> Result<()> {
        let mut children = self.nested_subvolumes(source)?;
        for child in &children {
            if child.as_os_str().is_empty()
                || child
                    .components()
                    .any(|c| !matches!(c, std::path::Component::Normal(_)))
            {
                return Err(Error::Invalid("子卷相对路径无效".into()));
            }
        }
        children.sort_by_key(|p| (p.components().count(), p.clone()));
        children.dedup();
        if children.is_empty() {
            return self.snapshot(source, destination, readonly);
        }
        self.snapshot(source, destination, false)?;
        for relative in &children {
            let target = destination.join(relative);
            if super::fs::exists(&target)? {
                std::fs::remove_dir(&target).map_err(|e| super::fs::io(&target, e))?;
            }
            self.snapshot(&source.join(relative), &target, false)?;
        }
        if readonly {
            for relative in children.iter().rev() {
                self.set_readonly(&destination.join(relative), true)?;
            }
            self.set_readonly(destination, true)?;
        }
        Ok(())
    }

    /// Flush all subvolume data before publishing a completion record.
    fn sync_filesystem(&self, path: &Path) -> Result<()> {
        super::fs::sync(path)
    }

    /// Pin the containing source subvolume across an offline inheritance operation.
    fn source_subvolume_id(&self, path: &Path) -> Result<String> {
        let _ = path;
        Err(Error::Invalid("此后端不能识别来源子卷".into()))
    }

    /// Bundle the generic early executor before the distribution plugin builds its initramfs.
    /// 发行版插件生成 initramfs 前，打包通用早期执行器。
    fn prepare_inheritance_initramfs(&self, root: &Path) -> Result<()> {
        let _ = root;
        Err(Error::Invalid("此后端不支持 initramfs 继承".into()))
    }

    /// Delete an owned work root including subvolumes created during plugin execution.
    /// 删除所属工作根及插件运行时创建的子卷。
    fn delete_subvolume_recursive(&self, path: &Path) -> Result<()> {
        self.check_no_nested_subvolumes(path)?;
        self.delete_subvolume(path)
    }

    /// Resolve the Btrfs UUID and top-level path used for persistent mounts.
    /// 解析持久挂载所用的 Btrfs UUID 和顶层路径。
    /// # Arguments
    /// - `self`: Filesystem inspection backend. 文件系统检查后端。
    /// - `path`: Existing Btrfs path. 已有 Btrfs 路径。
    fn filesystem_location(&self, path: &Path) -> Result<(String, PathBuf)> {
        let mount = super::mounts::covering(path)?;
        let uuid = mount
            .uuid
            .clone()
            .filter(|v| !v.is_empty())
            .ok_or_else(|| Error::Invalid("Btrfs 挂载没有 UUID".into()))?;
        Ok((uuid, mount.subvolume_path(path)?))
    }

    /// Set an owned subvolume's read-only property after attaching inherited children.
    /// 附加继承子卷后设置所属子卷只读属性。
    /// # Arguments
    /// - `self`: Storage backend. 存储后端。
    /// - `path`: Owned subvolume. 所属子卷。
    /// - `readonly`: New property. 新属性。
    fn set_readonly(&self, path: &Path, readonly: bool) -> Result<()> {
        let _ = (path, readonly);
        Err(Error::Invalid("此后端不能修改子卷只读属性".into()))
    }

    /// Copy a file or directory while optionally requiring reflink for all regular files.
    /// 复制文件或目录，可要求所有普通文件强制 reflink。
    /// # Arguments
    /// - `self`: Native data-copy backend. 原生数据复制后端。
    /// - `source`: Existing running-system source. 当前运行系统中的已有来源。
    /// - `target`: Owned file or empty directory. 所属文件或空目录。
    /// - `force_cow`: Reject any full-copy fallback. 拒绝全量复制回退。
    fn copy_inherited(&self, source: &Path, target: &Path, force_cow: bool) -> Result<()> {
        let _ = (source, target, force_cow);
        Err(Error::Invalid("此后端不支持继承复制".into()))
    }

    /// Execute exactly the selected policy; backends must opt into NOCOW adaptation.
    fn copy_inherited_with_policy(
        &self,
        source: &Path,
        target: &Path,
        policy: crate::config::NocowHandling,
    ) -> Result<()> {
        if policy == crate::config::NocowHandling::Error || !super::nocow::contains_nocow(source)? {
            self.copy_inherited(source, target, true)
        } else {
            Err(Error::Invalid("此后端不支持所选 NOCOW 处理策略".into()))
        }
    }

    /// Establish that mutating the actual source is safe, before conversion begins.
    fn check_migration_source(&self, _source: &Path) -> Result<()> {
        Err(Error::Invalid("此后端不能验证来源目录处于离线状态".into()))
    }

    /// Reject cross-filesystem CoW sources and unresolved mounted descendants.
    /// 拒绝跨文件系统 CoW 来源及未解析的内部挂载，允许嵌套子卷。
    /// # Arguments
    /// - `self`: Storage backend. 存储后端。
    /// - `source`: Current-system path. 当前系统路径。
    /// - `destination_parent`: Existing destination filesystem directory. 目标文件系统上的已有目录。
    fn check_cow_source(&self, source: &Path, destination_parent: &Path) -> Result<()> {
        let source_info =
            std::fs::symlink_metadata(source).map_err(|e| super::fs::io(source, e))?;
        let (source_uuid, _) = self.filesystem_location(source)?;
        let (target_uuid, _) = self.filesystem_location(destination_parent)?;
        if source_uuid != target_uuid {
            return Err(Error::Invalid(format!(
                "CoW 继承必须位于同一 Btrfs 分区: {}",
                source.display()
            )));
        }
        super::mounts::check_descendant_mounts(source)?;
        let _ = source_info;
        Ok(())
    }

    /// Determine whether a version root is the currently running system.
    /// 判断版本根是否为当前运行系统。
    /// # Arguments
    /// - `self`: Runtime inspection backend. 运行检查后端。
    /// - `path`: Candidate version root. 候选版本根。
    fn is_running_root(&self, path: &Path) -> Result<bool> {
        let _ = path;
        Err(Error::Invalid("此后端不能检查当前运行版本".into()))
    }

    /// Publish a completed main system to its configured boot filesystem.
    /// 将完成的主系统发布到配置的启动文件系统。
    /// # Arguments
    /// - `self`: Runtime publication backend. 运行发布后端。
    /// - `outcome`: Completed root. 完成根。
    /// - `config`: Global boot settings. 全局启动设置。
    fn publish_boot(
        &self,
        outcome: &BuildOutcome,
        config: &crate::config::RuntimeConfig,
    ) -> Result<()> {
        let _ = (outcome, config);
        Err(Error::Invalid("此后端不支持启动发布".into()))
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
    /// Execute with synchronous saves handled on the calling thread.
    /// 在调用线程处理同步保存；旧后端仍可执行不使用保存的插件。
    fn invoke_with_save(
        &self,
        plugin: &LoadedPlugin,
        operation: &str,
        args: &[String],
        root: &Path,
        context: &BuildContext,
        _save: &mut dyn FnMut() -> Result<()>,
    ) -> Result<String> {
        self.invoke(plugin, operation, args, root, context)
    }
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
