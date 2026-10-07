//! Distribution-independent base-system installation and maintenance.
//! 独立于发行版的基础系统安装和维护。
use crate::controller::{BuildBackend, BuildContext, Result, plugins::LoadedPlugin};
use std::path::Path;

/// Bootstrap a fresh root and install its protected base targets.
/// 引导全新根并安装受保护基础目标。
/// # Arguments
///
/// - `backend`: Checked execution backend.
///   经检查的执行后端。
/// - `plugin`: Distribution operations.
///   发行版操作。
/// - `root`: Empty writable subvolume.
///   空可写子卷。
/// - `context`: Frozen inputs.
///   冻结输入。
/// # Errors
///
/// Returns bootstrap or installation failures.
/// 返回引导或安装错误。
pub fn install(
    backend: &impl BuildBackend,
    plugin: &LoadedPlugin,
    root: &Path,
    context: &BuildContext,
) -> Result<()> {
    backend.invoke(plugin, "bootstrap", &[], root, context)?;
    backend.invoke(plugin, "base_install", &[], root, context)?;
    Ok(())
}

/// Update a newly installed or restored base root once before its cache is committed.
/// 新安装或恢复的基础根均先更新一次再提交缓存。
/// # Arguments
///
/// - `backend`: Checked execution backend.
///   经检查的执行后端。
/// - `plugin`: Distribution operations.
///   发行版操作。
/// - `root`: Writable restored root.
///   恢复的可写根。
/// - `context`: Frozen inputs.
///   冻结输入。
/// # Errors
///
/// Returns full-update failures.
/// 返回完整更新错误。
pub fn maintain(
    backend: &impl BuildBackend,
    plugin: &LoadedPlugin,
    root: &Path,
    context: &BuildContext,
) -> Result<()> {
    backend.invoke(plugin, "update", &[], root, context)?;
    Ok(())
}
