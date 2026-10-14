//! Shared infrastructure for building and maintaining managed systems.
//! 构建和维护被管理系统的共享基础设施。

/// Discover and freeze Bash distribution plugin archives or directories.
/// 查找并冻结 Bash 发行版插件压缩包或目录。
pub mod plugins;

mod backend;
mod boot;
mod cache;
mod clean;
mod container;
mod current;
mod deferred;
mod frozen;
mod fs;
mod identity;
mod inheritance;
mod initramfs;
mod logging;
mod model;
mod mounts;
mod output;
mod persistent;
mod predownload;
pub(crate) mod progress;
mod records;
mod resources;
mod scoped_backend;
mod scripts;
mod stages;
mod workflow;

pub use backend::SystemBackend;
pub use clean::{clean, clean_with_backend};
pub use current::{CurrentBuild, read_current_build};
pub use deferred::finalize_inheritance_locked;
pub use deferred::{finalize_inheritance, finalize_inheritance_with_backend};
pub use model::{BuildBackend, BuildContext, BuildOutcome, Error, PersistentMount, Result};
pub use workflow::{build, build_with_backend};

/// Acquire the shared build, startup and cleanup system lock.
/// 获取构建、启动和清理共用的系统锁。
/// # Arguments
/// - `path`: Existing owned system directory. 已有所属系统目录。
/// # Errors
/// Returns invalid paths or lock contention. 返回路径无效或锁争用。
pub fn lock_system_directory(path: &std::path::Path) -> Result<std::fs::File> {
    records::lock_directory(path)
}

/// Delete an obsolete runtime root while protecting pending sources and mount namespaces.
/// 保护待办来源及挂载命名空间期间删除过时运行根。
/// # Arguments
/// - `store`, `root`, `backend`: Owning manager, obsolete root and storage backend. 所属管理器、过时根及存储后端。
pub(crate) fn retire_runtime_root(
    store: &crate::config::ConfigStore,
    root: &std::path::Path,
    backend: &impl BuildBackend,
) -> Result<bool> {
    let _pins = records::lock_inheritance(store.root())?;
    fs::checked(root)?;
    let (_, location) = backend.filesystem_location(root)?;
    for id in store.list_systems()? {
        let layout = store.system_layout(&id)?;
        for entry in std::fs::read_dir(&layout.versions).map_err(|e| fs::io(&layout.versions, e))? {
            let directory = entry.map_err(|e| fs::io(&layout.versions, e))?.path();
            if fs::exists(&directory.join("inheritance-pending.toml"))?
                && deferred::pinned_sources_one(&directory)?
                    .iter()
                    .any(|source| source.starts_with(&location))
            {
                return Ok(false);
            }
        }
    }
    mounts::check_unmounted_tree(root)?;
    if backend.is_running_root(root)? {
        return Ok(false);
    }
    backend.delete_subvolume_recursive(root)?;
    Ok(true)
}
