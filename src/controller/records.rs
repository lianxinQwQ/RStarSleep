//! Per-system locks, monotonic attempts, frozen configuration, and build status.
//! 每系统锁、单调构建尝试、冻结配置和构建状态。
use super::{Error, Result, fs as disk};
use crate::config::{ConfigStore, SystemLayout};
use serde::Serialize;
use std::{
    fs::{self, File, OpenOptions},
    path::PathBuf,
};

/// Explicitly unlock even when a concurrent fork inherited the file description.
/// 即使并发 fork 继承了文件描述，也显式释放锁。
pub(super) struct Lock(File);

impl Drop for Lock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

/// One allocated attempt retaining its exclusive system lock.
/// 一个已分配构建尝试，保留独占系统锁。
pub(super) struct Attempt {
    /// Diagnostic archive retained independently of system roots and caches.
    /// 独立于系统根和缓存保留的诊断归档。
    pub log: super::logging::BuildLog,
    /// Manager-owned configuration, versions, and cache paths.
    /// 管理器所属配置、版本和缓存路径。
    pub layout: SystemLayout,
    /// Owning monotonic build version.
    /// 所属单调构建版本。
    pub version: String,
    /// Latest prior attempt, including failed builds.
    /// 最新过往尝试，包括失败构建。
    pub previous: Option<String>,
    /// Current version record directory outside all roots.
    /// 所有根之外的当前版本记录目录。
    pub directory: PathBuf,
    /// Frozen configuration directory used throughout this build.
    /// 此次构建始终使用的冻结配置目录。
    pub config: PathBuf,
    /// Exclusive system lock held through status and cleanup.
    /// 状态记录和清理期间持有的独占系统锁。
    _lock: Lock,
}

/// Locked, frozen inputs checked before allocating a build version.
/// 分配构建版本前用于校验的已锁定、冻结输入。
pub(super) struct Preflight {
    pub layout: SystemLayout,
    pub config: tempfile::TempDir,
    pub log: super::logging::BuildLog,
    lock: Lock,
}

impl Preflight {
    pub fn begin(store: &ConfigStore, id: &str) -> Result<Self> {
        let layout = store.system_layout(id)?;
        let lock = lock_system(&layout)?;
        let log = super::logging::BuildLog::new(&layout.root)?;
        let config = tempfile::Builder::new()
            .prefix("config-preflight-")
            .tempdir_in(&layout.root)
            .map_err(|e| disk::io(&layout.root, e))?;
        super::frozen::copy_config(&layout.config, config.path())?;
        Ok(Self {
            layout,
            config,
            log,
            lock,
        })
    }
}

/// Durable status kept outside all system-root subvolumes.
/// 保存在所有系统根子卷之外的持久化状态。
#[derive(Serialize)]
pub(super) struct Status<'a> {
    /// Supported serialized record format.
    /// 受支持序列化记录格式。
    pub schema_version: u32,
    /// Owning monotonic build version.
    /// 所属单调构建版本。
    pub version: &'a str,
    /// Stable identity of the managed system.
    /// 被管理系统的稳定标识。
    pub system_id: &'a str,
    /// Building, failed, or complete transition.
    /// 构建中、失败或完成的状态迁移。
    pub state: &'a str,
    /// Diagnostic retained when the attempt fails.
    /// 尝试失败时保留的诊断。
    pub error: Option<String>,
    /// Stable distribution plugin identity.
    /// 稳定发行版插件标识。
    pub plugin_id: Option<&'a str>,
    /// Release producing the recorded content or current status.
    /// 生成记录内容或当前状态的发布版本。
    pub plugin_version: Option<&'a str>,
    /// Exact source archive digest retained for provenance.
    /// 为来源保留的准确归档摘要。
    pub plugin_sha256: Option<&'a str>,
}

/// Acquire the shared build/cleanup system lock without allocating a version.
/// 取得构建和清理共用的系统锁，不分配版本。
/// # Arguments
/// - `layout`: Existing managed-system layout. 已有被管理系统布局。
pub(super) fn lock_system(layout: &SystemLayout) -> Result<Lock> {
    lock_directory(&layout.root).map(Lock)
}

pub(super) fn lock_directory(root: &std::path::Path) -> Result<File> {
    disk::checked(root)?;
    if !root.is_dir() {
        return Err(Error::Invalid("系统未初始化".into()));
    }
    let lock_path = root.join("build.lock");
    disk::checked(&lock_path)?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)
        .map_err(|e| disk::io(&lock_path, e))?;
    lock.try_lock()
        .map_err(|e| Error::Invalid(format!("无法取得系统构建锁: {e}")))?;
    Ok(lock)
}

impl Attempt {
    /// Allocate a version only after checking the frozen inputs under the same lock.
    /// 在同一个锁内校验冻结输入后才分配版本。
    pub fn begin(inputs: Preflight) -> Result<Self> {
        let Preflight {
            layout,
            config,
            log,
            lock,
        } = inputs;
        disk::directory(&layout.versions)?;
        let mut previous = None;
        let mut highest = 0u64;
        for entry in fs::read_dir(&layout.versions).map_err(|e| disk::io(&layout.versions, e))? {
            let entry = entry.map_err(|e| disk::io(&layout.versions, e))?;
            let name = entry.file_name();
            if let Some(text) = name.to_str()
                && let Some(number) = text.strip_prefix('v')
                && !number.is_empty()
                && number.bytes().all(|b| b.is_ascii_digit())
            {
                let number = number
                    .parse::<u64>()
                    .map_err(|_| Error::Invalid(format!("版本序号溢出: {text}")))?;
                disk::checked(&entry.path())?;
                if !entry
                    .file_type()
                    .map_err(|e| disk::io(&entry.path(), e))?
                    .is_dir()
                {
                    return Err(Error::Invalid(format!("版本记录不是目录: {text}")));
                }
                if number > highest {
                    highest = number;
                    previous = Some(text.into());
                }
            }
        }
        let counter = layout.root.join("version-counter");
        if disk::exists(&counter)? {
            let saved = fs::read_to_string(&counter)
                .map_err(|e| disk::io(&counter, e))?
                .trim()
                .parse::<u64>()
                .map_err(|e| Error::Invalid(format!("版本计数无效: {e}")))?;
            highest = highest.max(saved);
        }
        let next = highest
            .checked_add(1)
            .ok_or_else(|| Error::Invalid("版本序号耗尽".into()))?;
        disk::write_text(&counter, &next.to_string())?;
        let version = format!("v{next:08}");
        let directory = layout.versions.join(&version);
        fs::create_dir(&directory).map_err(|e| disk::io(&directory, e))?;
        disk::sync(&layout.versions)?;
        let frozen = directory.join("config");
        fs::rename(config.path(), &frozen).map_err(|e| disk::io(&frozen, e))?;
        disk::sync(&directory)?;
        log.publish(&directory)?;
        Ok(Self {
            log,
            config: directory.join("config"),
            layout,
            version,
            previous,
            directory,
            _lock: lock,
        })
    }
    /// Atomically persist a status transition.
    /// 原子持久化状态迁移。
    /// # Arguments
    ///
    /// - `self`: Locked allocated attempt.
    ///   已锁定并分配的构建尝试。
    /// - `status`: Full replacement status.
    ///   完整替换状态。
    pub fn status(&self, status: &Status<'_>) -> Result<()> {
        disk::write(&self.directory.join("state.toml"), status)
    }
}

/// Serialize source pin publication, first-boot finalization and version cleanup.
pub(super) fn lock_inheritance(manager: &std::path::Path) -> Result<Lock> {
    let path = manager.join("inheritance.lock");
    disk::checked(&path)?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&path)
        .map_err(|e| disk::io(&path, e))?;
    lock.try_lock()
        .map_err(|e| Error::Invalid(format!("无法取得继承事务锁: {e}")))?;
    Ok(Lock(lock))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dropping_lock_releases_it_even_when_another_description_survives() {
        let directory = tempfile::tempdir().unwrap();
        let guard = Lock(lock_directory(directory.path()).unwrap());
        let inherited = guard.0.try_clone().unwrap();
        assert!(lock_directory(directory.path()).is_err());
        drop(guard);
        let next = Lock(lock_directory(directory.path()).unwrap());
        drop(inherited);
        assert!(lock_directory(directory.path()).is_err());
        drop(next);
        assert!(lock_directory(directory.path()).is_ok());
    }
}
