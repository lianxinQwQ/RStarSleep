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
    /// All prior attempts in descending numeric order, captured under the system lock.
    /// 在系统锁内取得的全部过往尝试，按数字序号倒序排列。
    pub history: Vec<String>,
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
    pub config: PathBuf,
    pub log: super::logging::BuildLog,
    lock: Lock,
}

/// Retain temporary preflight sessions until every build handle has been released.
/// 在所有构建句柄释放之前保留预检临时会话。
#[derive(Default)]
pub(super) struct PreflightSessions {
    directories: Vec<tempfile::TempDir>,
}

impl PreflightSessions {
    /// Report cleanup failures on every ordinary return, preserving the original build error.
    /// 在每个正常返回路径报告清理失败，同时保留原始构建错误。
    pub fn run<T>(operation: impl FnOnce(&mut Self) -> Result<T>) -> Result<T> {
        let mut sessions = Self::default();
        let result = operation(&mut sessions);
        let mut failures = Vec::new();
        for directory in sessions.directories {
            let path = directory.path().to_owned();
            if let Err(error) = directory.close() {
                failures.push(disk::io(&path, error).to_string());
            }
        }
        if failures.is_empty() {
            return result;
        }
        let cleanup = format!(
            "预检临时目录清理失败 / Preflight temporary-directory cleanup failed:\n{}",
            failures.join("\n")
        );
        Err(Error::Invalid(match result {
            Ok(_) => cleanup,
            Err(error) => format!("{error}; {cleanup}"),
        }))
    }

    fn create(&mut self) -> Result<PathBuf> {
        let parent = std::env::temp_dir();
        // Resolve environment-selected aliases before applying owned-path checks.
        // 在校验所属路径前解析环境所选临时路径的别名。
        let parent = fs::canonicalize(&parent).map_err(|error| disk::io(&parent, error))?;
        disk::checked(&parent)?;
        let mut builder = tempfile::Builder::new();
        builder.prefix("rstarsleep-preflight-");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            builder.permissions(fs::Permissions::from_mode(0o700));
        }
        let directory = builder
            .tempdir_in(&parent)
            .map_err(|error| disk::io(&parent, error))?;
        let path = directory.path().to_owned();
        self.directories.push(directory);
        Ok(path)
    }
}

impl Preflight {
    pub fn begin(store: &ConfigStore, id: &str, sessions: &mut PreflightSessions) -> Result<Self> {
        let layout = store.system_layout(id)?;
        let lock = lock_system(&layout)?;
        let session = sessions.create()?;
        let log = super::logging::BuildLog::new(&session)?;
        let config = session.join("config");
        fs::create_dir(&config).map_err(|error| disk::io(&config, error))?;
        super::frozen::copy_config(&layout.config, &config)?;
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
    lock_directory_guard(&layout.root)
}

/// Release the directory lock explicitly even if a concurrent fork inherited it.
/// 即使并发 fork 继承了锁，也显式释放目录锁。
pub(super) fn lock_directory_guard(root: &std::path::Path) -> Result<Lock> {
    lock_directory(root).map(Lock)
}

pub(super) fn lock_directory(root: &std::path::Path) -> Result<File> {
    disk::checked(root)?;
    if !root.is_dir() {
        return Err(Error::Invalid(
            "系统未初始化 / System is not initialized".into(),
        ));
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
    lock.try_lock().map_err(|e| {
        Error::Invalid(format!(
            "无法取得系统构建锁 / Cannot acquire the system build lock: {e}"
        ))
    })?;
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
        let mut history = Vec::new();
        let mut highest = 0u64;
        for entry in fs::read_dir(&layout.versions).map_err(|e| disk::io(&layout.versions, e))? {
            let entry = entry.map_err(|e| disk::io(&layout.versions, e))?;
            let name = entry.file_name();
            if let Some(text) = name.to_str()
                && let Some(number) = text.strip_prefix('v')
                && !number.is_empty()
                && number.bytes().all(|b| b.is_ascii_digit())
            {
                let number = number.parse::<u64>().map_err(|_| {
                    Error::Invalid(format!(
                        "版本序号溢出: {text} / Version sequence number overflow: {text}"
                    ))
                })?;
                disk::checked(&entry.path())?;
                if !entry
                    .file_type()
                    .map_err(|e| disk::io(&entry.path(), e))?
                    .is_dir()
                {
                    return Err(Error::Invalid(format!(
                        "版本记录不是目录: {text} / Version record is not a directory: {text}"
                    )));
                }
                highest = highest.max(number);
                history.push((number, text.to_owned()));
            }
        }
        history.sort_by(|a, b| b.cmp(a));
        let history: Vec<_> = history.into_iter().map(|(_, version)| version).collect();
        let previous = history.first().cloned();
        let counter = layout.root.join("version-counter");
        if disk::exists(&counter)? {
            let saved = fs::read_to_string(&counter)
                .map_err(|e| disk::io(&counter, e))?
                .trim()
                .parse::<u64>()
                .map_err(|e| {
                    Error::Invalid(format!("版本计数无效 / Invalid version count: {e}"))
                })?;
            highest = highest.max(saved);
        }
        let next = highest.checked_add(1).ok_or_else(|| {
            Error::Invalid("版本序号耗尽 / Version sequence numbers exhausted".into())
        })?;
        disk::write_text(&counter, &next.to_string())?;
        let version = format!("v{next:08}");
        let directory = layout.versions.join(&version);
        fs::create_dir(&directory).map_err(|e| disk::io(&directory, e))?;
        disk::sync(&layout.versions)?;
        super::frozen::publish_config(&config, &directory)?;
        log.publish(&directory)?;
        Ok(Self {
            log,
            config: directory.join("config"),
            layout,
            version,
            previous,
            history,
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
    lock.try_lock().map_err(|e| {
        Error::Invalid(format!(
            "无法取得继承事务锁 / Cannot acquire the inheritance transaction lock: {e}"
        ))
    })?;
    Ok(Lock(lock))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preflight_uses_system_temporary_storage_and_archives_the_checked_snapshot() {
        // Keep the manager on the workspace filesystem so a separate system temp filesystem is exercised.
        // 将管理器置于工作区文件系统，以覆盖系统临时目录位于另一文件系统的情况。
        let directory = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
        let store = ConfigStore::new(directory.path()).unwrap();
        fs::create_dir(directory.path().join("config")).unwrap();
        fs::write(store.global_config_path(), "schema_version = 1\n").unwrap();
        let layout = store.initialize_system("desktop").unwrap();
        fs::write(
            layout.config.join("build/environment.conf"),
            "[Environment]\nSOURCE=frozen\n",
        )
        .unwrap();
        fs::create_dir_all(layout.config.join("script/example/resources")).unwrap();
        fs::write(
            layout.config.join("script/example/main.sh"),
            "printf frozen\n",
        )
        .unwrap();
        fs::write(
            layout.config.join("script/example/resources/data"),
            b"\0\xfffrozen",
        )
        .unwrap();
        let mut session_path = PathBuf::new();
        let archived = PreflightSessions::run(|sessions| {
            let inputs = Preflight::begin(&store, "desktop", sessions)?;
            session_path = inputs.config.parent().unwrap().to_owned();
            assert_eq!(
                session_path.parent().unwrap(),
                fs::canonicalize(std::env::temp_dir()).unwrap()
            );
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                assert_eq!(
                    fs::metadata(&session_path).unwrap().permissions().mode() & 0o777,
                    0o700
                );
            }
            assert!(!session_path.starts_with(&layout.root));
            assert_eq!(fs::read_dir(&layout.versions).unwrap().count(), 0);
            assert!(!layout.root.join("version-counter").exists());
            let expected = super::super::frozen::config_hash(&inputs.config)?;
            fs::write(
                layout.config.join("build/environment.conf"),
                "[Environment]\nSOURCE=changed\n",
            )
            .unwrap();
            fs::write(
                layout.config.join("script/example/resources/data"),
                b"changed",
            )
            .unwrap();
            let _log = super::super::logging::enter([inputs.log.clone()]);
            super::super::logging::diagnostic("before publication");
            let attempt = Attempt::begin(inputs)?;
            assert_eq!(
                super::super::frozen::config_hash(&attempt.config)?,
                expected
            );
            super::super::logging::diagnostic("after publication");
            attempt.log.check()?;
            let bytes = fs::read_to_string(attempt.directory.join("install.log")).unwrap();
            assert!(bytes.contains("before publication\nafter publication\n"));
            assert_eq!(fs::read_dir(&attempt.directory).unwrap().count(), 2);
            Ok(attempt.config.clone())
        })
        .unwrap();
        assert!(!session_path.exists());
        assert_eq!(
            fs::read_to_string(archived.join("build/environment.conf")).unwrap(),
            "[Environment]\nSOURCE=frozen\n"
        );
        assert_eq!(
            fs::read(archived.join("script/example/resources/data")).unwrap(),
            b"\0\xfffrozen"
        );
        assert!(fs::read_dir(&layout.root).unwrap().all(|entry| {
            let name = entry.unwrap().file_name();
            !name.to_string_lossy().contains("preflight-")
        }));
    }

    #[test]
    fn failed_preflight_removes_its_session_without_allocating_a_version() {
        let directory = tempfile::tempdir().unwrap();
        let store = ConfigStore::new(directory.path()).unwrap();
        let layout = store.system_layout("desktop").unwrap();
        fs::create_dir_all(layout.config.join("build")).unwrap();
        fs::write(layout.config.join("build/config.conf"), "frozen").unwrap();
        let mut session_path = PathBuf::new();
        let result: Result<()> = PreflightSessions::run(|sessions| {
            let inputs = Preflight::begin(&store, "desktop", sessions)?;
            session_path = inputs.config.parent().unwrap().to_owned();
            Err(Error::Invalid("预检拒绝 / Preflight rejected".into()))
        });
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Preflight rejected")
        );
        assert!(!session_path.exists());
        assert!(!layout.versions.exists());
        assert!(!layout.root.join("version-counter").exists());
        assert!(lock_system(&layout).is_ok());
    }

    #[test]
    fn cleanup_reports_failures_and_preserves_the_original_error() {
        let directory = tempfile::tempdir().unwrap();
        let result: Result<()> = PreflightSessions::run(|sessions| {
            let blocked = tempfile::tempdir_in(directory.path()).unwrap();
            let path = blocked.path().to_owned();
            fs::remove_dir(&path).unwrap();
            fs::write(&path, b"replacement").unwrap();
            sessions.directories.push(blocked);
            sessions
                .directories
                .push(tempfile::tempdir_in(directory.path()).unwrap());
            Err(Error::Invalid("原始错误 / Original error".into()))
        });
        let error = result.unwrap_err().to_string();
        assert!(error.contains("Original error"), "{error}");
        assert!(
            error.contains("Preflight temporary-directory cleanup failed"),
            "{error}"
        );
        assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
    }

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
