//! Per-system locks, monotonic attempts, frozen configuration, and build status.
//! 每系统锁、单调构建尝试、冻结配置和构建状态。
use super::{Error, Result, fs as disk};
use crate::config::{ConfigStore, SystemLayout};
use serde::Serialize;
use std::{
    fs::{self, File, OpenOptions},
    path::PathBuf,
};

/// One allocated attempt retaining its exclusive system lock.
/// 一个已分配构建尝试，保留独占系统锁。
pub(super) struct Attempt {
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
    _lock: File,
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

impl Attempt {
    /// Lock the system and reserve the next version without overwriting any prior attempt.
    /// 锁定系统并预留下一版本，不覆盖任何过往尝试。
    /// # Arguments
    ///
    /// - `store`: Manager configuration store.
    ///   管理器配置存储。
    /// - `id`: Managed system identity.
    ///   被管理系统标识。
    pub fn begin(store: &ConfigStore, id: &str) -> Result<Self> {
        let layout = store.system_layout(id)?;
        disk::checked(&layout.root)?;
        if !layout.root.is_dir() {
            return Err(Error::Invalid(format!("系统未初始化: {id}")));
        }
        let lock_path = layout.root.join("build.lock");
        disk::checked(&lock_path)?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&lock_path)
            .map_err(|e| disk::io(&lock_path, e))?;
        lock.try_lock()
            .map_err(|e| Error::Invalid(format!("无法取得系统 {id} 构建锁: {e}")))?;
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
        let next = highest
            .checked_add(1)
            .ok_or_else(|| Error::Invalid("版本序号耗尽".into()))?;
        let version = format!("v{next:08}");
        let directory = layout.versions.join(&version);
        fs::create_dir(&directory).map_err(|e| disk::io(&directory, e))?;
        disk::sync(&layout.versions)?;
        Ok(Self {
            config: directory.join("config"),
            layout,
            version,
            previous,
            directory,
            _lock: lock,
        })
    }
    /// Archive every ordinary configuration file before interpreting build inputs.
    /// 解释构建输入前归档所有普通配置文件。
    /// # Arguments
    ///
    /// - `self`: Locked allocated attempt.
    ///   已锁定并分配的构建尝试。
    pub fn freeze(&self) -> Result<()> {
        let staging = tempfile::Builder::new()
            .prefix("config-pending-")
            .tempdir_in(&self.directory)
            .map_err(|e| disk::io(&self.directory, e))?;
        super::frozen::copy_config(&self.layout.config, staging.path())?;
        fs::rename(staging.path(), &self.config).map_err(|e| disk::io(&self.config, e))?;
        disk::sync(&self.directory)
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

impl Drop for Attempt {
    /// Explicitly release the lock before closing, including descriptions inherited during fork.
    /// 关闭前显式释放锁，包括 fork 期间继承的文件描述。
    /// # Arguments
    ///
    /// - `self`: Finished or failed attempt's lock guard.
    ///   已完成或失败尝试的锁守卫。
    fn drop(&mut self) {
        let _ = self._lock.unlock();
    }
}
