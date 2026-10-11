//! Maintain an independent cached environment, then discard its download work root.
//! 维护独立缓存环境，随后丢弃其下载工作根。
use super::{
    BuildBackend, BuildContext, Error, Result, fs as disk, identity, plugins::LoadedPlugin,
};
use crate::base_system;
use crate::{
    config::ConfigStore,
    software::{PackageSession, SoftwareState},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    path::Path,
};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    schema_version: u32,
    plugin_sha256: String,
    architecture: String,
    distribution_sha256: String,
    software: SoftwareState,
}
struct Lock(File);
impl Drop for Lock {
    /// Release the environment lock, including descriptions inherited by child processes.
    /// 释放环境锁，包括子进程继承的文件描述。
    /// # Arguments
    /// - `self`: Finished environment access.
    ///   已完成的环境访问。
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

/// Prepare one group of systems with the same plugin, architecture, and declared settings.
/// 为插件、架构及已声明设置相同的一组系统准备下载。
/// # Arguments
/// - `backend`: Checked CoW storage and plugin execution.
///   经检查的 CoW 存储及插件执行。
/// - `store`: Manager holding persistent download-environment caches.
///   保存持久下载环境缓存的管理器。
/// - `plugin`: Validated plugin providing independent environment operations.
///   提供独立环境操作的已校验插件。
/// - `target`: Frozen configuration shared by this group.
///   此组共用的冻结配置。
/// - `packages`: Sorted package demand, including planned full-upgrade inputs.
///   已排序的软件需求，包括计划完整更新的输入。
pub(super) fn run(
    backend: &impl BuildBackend,
    store: &ConfigStore,
    plugin: &LoadedPlugin,
    target: &BuildContext,
    packages: &[String],
) -> Result<()> {
    plugin.validate_configuration(&target.parameters)?;
    let architecture = &target.architecture;
    let options = toml::to_string(&target.distribution_options)
        .map_err(|e| Error::Invalid(format!("预下载配置无法编码: {e}")))?;
    let options_hash = format!("{:x}", Sha256::digest(options.as_bytes()));
    let directory = store
        .predownload_path()
        .join(&plugin.archive_sha256)
        .join(architecture)
        .join(&options_hash);
    disk::directory(&directory)?;
    let lock_path = directory.join("build.lock");
    disk::checked(&lock_path)?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)
        .map_err(|e| disk::io(&lock_path, e))?;
    lock.try_lock()
        .map_err(|e| Error::Invalid(format!("预下载环境正在使用: {e}")))?;
    let _lock = Lock(lock);
    let completed = directory.join("completed");
    let old = directory.join("previous");
    let pending = directory.join("pending");
    if disk::exists(&old)? {
        if !disk::exists(&completed)? {
            fs::rename(&old, &completed).map_err(|e| disk::io(&completed, e))?;
            disk::sync(&directory)?;
        } else {
            remove_environment(backend, &old)?;
        }
    }
    if disk::exists(&pending)? {
        remove_environment(backend, &pending)?;
    }
    let work = directory.join("work-root");
    if disk::exists(&work)? {
        backend.delete_subvolume_recursive(&work)?;
    }
    disk::directory(&pending)?;
    let root = pending.join("work-root");
    let readonly = pending.join("root");
    let context = BuildContext {
        system_id: "predownload".into(),
        architecture: architecture.clone(),
        distribution_options: target.distribution_options.clone(),
        parameters: target.parameters.clone(),
        config: plugin.root().join("predownload"),
        persistent_mounts: Vec::new(),
        parameter_lists: target.parameter_lists.clone(),
        environment: target.environment.clone(),
        inherited_mounts: Vec::new(),
    };
    let result = (|| {
        let reused = disk::exists(&completed)?;
        if reused {
            let path = completed.join("environment.toml");
            disk::checked(&path)?;
            if !fs::symlink_metadata(&path)
                .map_err(|e| disk::io(&path, e))?
                .is_file()
            {
                return Err(Error::Invalid("预下载环境完成记录必须是普通文件".into()));
            }
            let record: Record =
                toml::from_str(&fs::read_to_string(&path).map_err(|e| disk::io(&path, e))?)
                    .map_err(|e| Error::Invalid(format!("预下载环境记录无效: {e}")))?;
            if record.schema_version != 1
                || record.plugin_sha256 != plugin.archive_sha256
                || &record.architecture != architecture
                || record.distribution_sha256 != options_hash
            {
                return Err(Error::Invalid("预下载环境来源不符".into()));
            }
            record.software.validate()?;
            backend.check_subvolume(&completed.join("root"), true)?;
            backend.check_no_nested_subvolumes(&completed.join("root"))?;
            backend.snapshot(&completed.join("root"), &root, false)?;
        } else {
            backend.create_subvolume(&root)?;
            base_system::install(backend, plugin, &root, &context)?;
            let machine_id = identity::machine_id(&directory, None)?;
            backend.invoke(plugin, "system_init", &[machine_id], &root, &context)?;
        }
        base_system::maintain(backend, plugin, &root, &context)?;
        backend.invoke(
            plugin,
            if reused {
                "predownload_update"
            } else {
                "predownload_install"
            },
            &[],
            &root,
            &context,
        )?;
        let software = PackageSession {
            backend,
            plugin,
            root: &root,
            context: &context,
        }
        .capture(Vec::new())?;
        backend.check_no_nested_subvolumes(&root)?;
        backend.snapshot(&root, &readonly, true)?;
        disk::write(
            &pending.join("environment.toml"),
            &Record {
                schema_version: 1,
                plugin_sha256: plugin.archive_sha256.clone(),
                architecture: architecture.clone(),
                distribution_sha256: options_hash.clone(),
                software,
            },
        )?;
        backend.delete_subvolume_recursive(&root)?;
        if reused {
            fs::rename(&completed, &old).map_err(|e| disk::io(&old, e))?;
            disk::sync(&directory)?;
        }
        fs::rename(&pending, &completed).map_err(|e| disk::io(&completed, e))?;
        disk::sync(&directory)?;
        if disk::exists(&old)? {
            remove_environment(backend, &old)?;
        }
        backend.snapshot(&completed.join("root"), &work, false)?;
        backend.invoke(plugin, "predownload", packages, &work, &context)?;
        Ok(())
    })();
    let cleanup = (|| {
        if disk::exists(&work)? {
            backend.delete_subvolume_recursive(&work)?;
        }
        if disk::exists(&pending)? {
            remove_environment(backend, &pending)?;
        }
        Ok(())
    })();
    match (result, cleanup) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(e), Ok(())) | (Ok(()), Err(e)) => Err(e),
        (Err(e), Err(c)) => Err(Error::Invalid(format!("{e}; 预下载环境清理失败: {c}"))),
    }
}
/// Remove known environment subvolumes before deleting their ordinary parent directory.
/// 删除已知环境子卷后，再删除其普通父目录。
/// # Arguments
/// - `backend`: Checked recursive subvolume deletion.
///   经检查的递归子卷删除。
/// - `path`: Owned environment directory to retire.
///   待撤销的所属环境目录。
fn remove_environment(backend: &impl BuildBackend, path: &Path) -> Result<()> {
    disk::checked(path)?;
    for name in ["work-root", "root"] {
        let root = path.join(name);
        if disk::exists(&root)? {
            backend.delete_subvolume_recursive(&root)?;
        }
    }
    fs::remove_dir_all(path).map_err(|e| disk::io(path, e))
}
