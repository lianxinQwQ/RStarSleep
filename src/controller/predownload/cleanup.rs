//! Retire only obsolete versions belonging to installed plugins under a build-wide lock.
//! 在整次构建锁保护下，仅清理属于已安装插件的废弃版本。
use super::{Lock, disk};
use crate::{
    config::ConfigStore,
    controller::{BuildBackend, Error, Result, plugins, progress},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, OpenOptions},
    path::{Component, Path, PathBuf},
};

/// Keep cleanup and all environment users mutually exclusive across build invocations.
/// 使不同构建调用中的清理及全部环境使用互斥。
pub(in crate::controller) struct BuildCache {
    _lock: Lock,
    versions: BTreeMap<String, BTreeSet<String>>,
}

impl BuildCache {
    /// Validate all installed metadata before taking any decision to remove cache data.
    /// 在作出任何缓存删除决定前，校验全部已安装元数据。
    pub(in crate::controller) fn begin(store: &ConfigStore) -> Result<Self> {
        let installed = plugins::installed(&store.plugins_path())?;
        let mut versions = BTreeMap::<String, BTreeSet<String>>::new();
        for plugin in installed {
            let retained = versions.entry(plugin.metadata.id).or_default();
            if let Some(version) = plugin.metadata.predownload_version {
                retained.insert(version);
            }
        }
        let root = store.predownload_path();
        disk::directory(&root)?;
        // Preserve the file identity; unlinking it would let builds lock different files.
        // 保留文件身份；删除锁文件会使不同构建锁定不同文件，破坏互斥。
        let path = root.join(".build.lock");
        disk::checked(&path)?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&path)
            .map_err(|e| disk::io(&path, e))?;
        lock.try_lock().map_err(|e| {
            Error::Invalid(format!(
                "预下载缓存正由其他构建使用 / Predownload caches are in use by another build: {e}"
            ))
        })?;
        Ok(Self {
            _lock: Lock(lock),
            versions,
        })
    }

    /// Reject a changed installed version before cleanup can act on a stale inventory.
    /// 清理使用过期清单前，拒绝已安装版本发生变化的情况。
    pub(in crate::controller) fn check_plugin(&self, plugin: &plugins::LoadedPlugin) -> Result<()> {
        let matches = self
            .versions
            .get(&plugin.metadata.id)
            .is_some_and(|retained| {
                plugin
                    .metadata
                    .predownload_version
                    .as_ref()
                    .is_none_or(|version| retained.contains(version))
            });
        if !matches {
            return Err(Error::Invalid(format!(
                "插件 {} 的预下载版本在预检期间发生变化 / Plugin {} predownload version changed during preflight",
                plugin.metadata.id, plugin.metadata.id
            )));
        }
        Ok(())
    }

    /// Check every obsolete tree before deleting any; unknown plugin namespaces stay untouched.
    /// 删除前检查全部废弃目录树；未知插件命名空间保持不动。
    pub(in crate::controller) fn clean(
        &self,
        store: &ConfigStore,
        backend: &impl BuildBackend,
    ) -> Result<()> {
        let mut targets = Vec::new();
        for (id, retained) in &self.versions {
            let directory = store.predownload_path().join(id);
            if !disk::exists(&directory)? {
                continue;
            }
            for path in directories(&directory)? {
                let version = path.file_name().and_then(|v| v.to_str()).ok_or_else(|| {
                    Error::Invalid(
                        "预下载缓存版本不是 UTF-8 / Predownload cache version is not UTF-8".into(),
                    )
                })?;
                plugins::validate_identity(version)?;
                if retained.contains(version) {
                    continue;
                }
                if backend.is_subvolume(&path)? {
                    return Err(Error::Invalid(format!(
                        "预下载版本容器必须是普通目录: {0} / Predownload version container must be an ordinary directory: {0}",
                        path.display()
                    )));
                }
                let mut roots = backend.nested_subvolumes(&path)?;
                for relative in &roots {
                    if relative.as_os_str().is_empty()
                        || relative
                            .components()
                            .any(|v| !matches!(v, Component::Normal(_)))
                    {
                        return Err(Error::Invalid(
                            "预下载子卷路径无效 / Invalid predownload subvolume path".into(),
                        ));
                    }
                    disk::checked(&path.join(relative))?;
                }
                roots.sort_by_key(|p| (p.components().count(), p.clone()));
                let mut outer = Vec::<PathBuf>::new();
                for relative in roots {
                    if !outer.iter().any(|parent| relative.starts_with(parent)) {
                        outer.push(relative);
                    }
                }
                targets.push((path, outer));
            }
        }
        for (path, roots) in targets {
            progress::run(
                "predownload",
                format!("清理已安装插件的旧预下载版本：{}", path.display()),
                format!(
                    "Remove an installed plugin's obsolete predownload version: {}",
                    path.display()
                ),
                || {
                    for relative in roots {
                        backend.delete_subvolume_recursive(&path.join(relative))?;
                    }
                    backend.check_no_nested_subvolumes(&path)?;
                    fs::remove_dir_all(&path).map_err(|e| disk::io(&path, e))?;
                    disk::sync(path.parent().unwrap())
                },
            )?;
        }
        Ok(())
    }
}

/// Reject substituted links and non-directory entries inside a known plugin namespace.
/// 拒绝已知插件命名空间中被替换的链接及非目录条目。
fn directories(path: &Path) -> Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    for entry in fs::read_dir(path).map_err(|e| disk::io(path, e))? {
        let entry = entry.map_err(|e| disk::io(path, e))?;
        let child = entry.path();
        disk::checked(&child)?;
        if !entry.file_type().map_err(|e| disk::io(&child, e))?.is_dir() {
            return Err(Error::Invalid(format!(
                "预下载缓存版本必须是目录: {0} / Predownload cache version must be a directory: {0}",
                child.display()
            )));
        }
        paths.push(child);
    }
    paths.sort();
    Ok(paths)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Explicit unlocking releases duplicated descriptions instead of waiting for every close.
    /// 显式解锁会释放复制的文件描述所持有的锁，无需等待所有描述关闭。
    #[test]
    fn build_cache_drop_unlocks_even_with_a_duplicated_file_description() {
        let directory = tempfile::tempdir().unwrap();
        fs::create_dir(directory.path().join("plugins")).unwrap();
        let store = ConfigStore::new(directory.path()).unwrap();
        let cache = BuildCache::begin(&store).unwrap();
        let duplicate = cache._lock.0.try_clone().unwrap();
        assert!(BuildCache::begin(&store).is_err());
        drop(cache);
        let reacquired = BuildCache::begin(&store).unwrap();
        assert!(store.predownload_path().join(".build.lock").is_file());
        drop(reacquired);
        drop(duplicate);
    }

    /// Executed plugin processes cannot retain the build lock after their parent is killed.
    /// 执行后的插件进程不能在父进程被强杀后继续持有构建锁。
    #[cfg(unix)]
    #[test]
    fn build_cache_lock_descriptor_closes_on_exec() {
        use std::os::fd::AsRawFd;

        let directory = tempfile::tempdir().unwrap();
        fs::create_dir(directory.path().join("plugins")).unwrap();
        let store = ConfigStore::new(directory.path()).unwrap();
        let cache = BuildCache::begin(&store).unwrap();
        let flags = unsafe { libc::fcntl(cache._lock.0.as_raw_fd(), libc::F_GETFD) };
        assert_ne!(flags, -1);
        assert_ne!(flags & libc::FD_CLOEXEC, 0);
    }
}
