//! Global persistent subvolume transactions with a durable multi-volume commit journal.
use super::{
    BuildBackend, Error, PersistentMount, Result, fs as disk, plugins::PersistentDirectory,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    fs::{self, File, OpenOptions},
    io::Write,
    os::fd::AsRawFd,
    path::{Path, PathBuf},
};
unsafe extern "C" {
    fn syncfs(fd: std::ffi::c_int) -> std::ffi::c_int;
}
#[derive(Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Phase {
    Preparing,
    Swapping,
    Committed,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    name: String,
    had_original: bool,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Journal {
    schema_version: u32,
    phase: Phase,
    /// A live working copy can outlast several separately published saves.
    #[serde(default)]
    working: bool,
    entries: Vec<Entry>,
}

pub(super) struct PersistentTransaction {
    pool: PathBuf,
    state: PathBuf,
    mounts: Vec<PersistentMount>,
    lock: Option<File>,
}
impl Drop for PersistentTransaction {
    fn drop(&mut self) {
        if let Some(file) = &self.lock {
            let _ = file.unlock();
        }
    }
}

/// Hold the publication lock while exposing current data without snapshotting it.
pub(super) struct PersistentRead {
    _lock: File,
    _empty: tempfile::TempDir,
    pub mounts: Vec<PersistentMount>,
}
impl Drop for PersistentRead {
    fn drop(&mut self) {
        // Explicit unlock also releases locks inherited briefly by concurrent forks.
        let _ = self._lock.unlock();
    }
}
impl PersistentRead {
    pub fn begin(
        backend: &impl BuildBackend,
        pool: &Path,
        state: &Path,
        mappings: &[PersistentDirectory],
    ) -> Result<Self> {
        let lock = lock_pool(pool, state)?;
        recover(backend, pool, &state.join("save"))?;
        recover(backend, pool, &state.join("transaction"))?;
        let empty = tempfile::tempdir_in(state).map_err(|e| disk::io(state, e))?;
        let mut mounts = Vec::new();
        for mapping in mappings {
            let source = pool.join(&mapping.name);
            disk::checked(&source)?;
            let source = if disk::exists(&source)? {
                backend.check_subvolume(&source, false)?;
                backend.check_no_nested_subvolumes(&source)?;
                source
            } else {
                let path = empty.path().join(&mapping.name);
                disk::directory(&path)?;
                path
            };
            mounts.push(PersistentMount {
                source,
                target: mapping.target.clone(),
            });
        }
        Ok(Self {
            _lock: lock,
            _empty: empty,
            mounts,
        })
    }
}

fn lock_pool(pool: &Path, state: &Path) -> Result<File> {
    disk::checked(pool)?;
    disk::checked(state)?;
    if pool.starts_with(state) || state.starts_with(pool) {
        return Err(Error::Invalid("持久数据与事务记录必须独立".into()));
    }
    disk::directory(pool)?;
    disk::directory(state)?;
    let lock_path = state.join("lock");
    disk::checked(&lock_path)?;
    if disk::exists(&lock_path)?
        && !fs::symlink_metadata(&lock_path)
            .map_err(|e| disk::io(&lock_path, e))?
            .is_file()
    {
        return Err(Error::Invalid("持久锁必须是普通文件".into()));
    }
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)
        .map_err(|e| disk::io(&lock_path, e))?;
    lock.try_lock()
        .map_err(|e| Error::Invalid(format!("全局持久目录正在使用: {e}")))?;
    Ok(lock)
}
impl PersistentTransaction {
    pub fn begin(
        backend: &impl BuildBackend,
        pool: &Path,
        state: &Path,
        mappings: &[PersistentDirectory],
    ) -> Result<Self> {
        let lock = lock_pool(pool, state)?;
        let root = state.join("transaction");
        recover(backend, pool, &state.join("save"))?;
        recover(backend, pool, &root)?;
        let mut entries = Vec::new();
        let mut names = BTreeSet::new();
        for mapping in mappings {
            super::plugins::validate_identity(&mapping.name)?;
            if !names.insert(&mapping.name) {
                return Err(Error::Invalid("重复持久目录".into()));
            }
            let source = pool.join(&mapping.name);
            let had_original = disk::exists(&source)?;
            if had_original {
                backend.check_subvolume(&source, false)?;
                backend.check_no_nested_subvolumes(&source)?;
            }
            entries.push(Entry {
                name: mapping.name.clone(),
                had_original,
            });
        }
        let mut tx = Self {
            pool: pool.into(),
            state: state.into(),
            mounts: mappings
                .iter()
                .map(|m| PersistentMount {
                    source: root.join("new").join(&m.name),
                    target: m.target.clone(),
                })
                .collect(),
            lock: Some(lock),
        };
        let result = (|| {
            disk::directory(&root)?;
            disk::sync(state)?;
            let record = Journal {
                schema_version: 1,
                phase: Phase::Preparing,
                working: true,
                entries,
            };
            write_journal(&root, &record)?;
            disk::directory(&root.join("new"))?;
            disk::directory(&root.join("old"))?;
            disk::sync(&root)?;
            for entry in &record.entries {
                let new = root.join("new").join(&entry.name);
                if entry.had_original {
                    backend.snapshot(&pool.join(&entry.name), &new, false)?;
                } else {
                    backend.create_subvolume(&new)?;
                }
                backend.check_subvolume(&new, false)?;
                backend.check_no_nested_subvolumes(&new)?;
            }
            flush(&root)
        })();
        if let Err(error) = result {
            return match tx.rollback(backend) {
                Ok(()) => Err(error),
                Err(c) => Err(Error::Invalid(format!(
                    "{error}; 持久事务准备回滚失败: {c}"
                ))),
            };
        }
        Ok(tx)
    }
    pub fn mounts(&self) -> Vec<PersistentMount> {
        self.mounts.clone()
    }
    /// Durably publish snapshots without moving or unlocking the mounted working copies.
    /// 持久发布快照，不移动或解锁正在挂载的工作副本。
    pub fn save(&mut self, backend: &impl BuildBackend) -> Result<()> {
        if self.lock.is_none() {
            return Err(Error::Invalid("持久事务已经结束".into()));
        }
        let work = self.state.join("transaction");
        let working = read(&work)?;
        if working.phase != Phase::Preparing || !working.working {
            return Err(Error::Invalid("持久事务保存状态不符".into()));
        }
        let root = self.state.join("save");
        recover(backend, &self.pool, &root)?;
        let result = (|| {
            validate_layout(&work, &working)?;
            let mut entries = Vec::new();
            for entry in &working.entries {
                let source = work.join("new").join(&entry.name);
                backend.check_subvolume(&source, false)?;
                backend.check_no_nested_subvolumes(&source)?;
                entries.push(Entry {
                    name: entry.name.clone(),
                    had_original: disk::exists(&self.pool.join(&entry.name))?,
                });
            }
            disk::directory(&root)?;
            disk::sync(&self.state)?;
            let record = Journal {
                schema_version: 1,
                phase: Phase::Preparing,
                working: false,
                entries,
            };
            write_journal(&root, &record)?;
            disk::directory(&root.join("new"))?;
            disk::directory(&root.join("old"))?;
            disk::sync(&root)?;
            // The plugin has quiesced its writers until the save acknowledgement.
            flush(&work)?;
            for entry in &record.entries {
                backend.snapshot(
                    &work.join("new").join(&entry.name),
                    &root.join("new").join(&entry.name),
                    false,
                )?;
            }
            publish(backend, &self.pool, &root, record)
        })();
        match result {
            Ok(()) => Ok(()),
            Err(error) => match recover(backend, &self.pool, &root) {
                Ok(()) => Err(error),
                Err(cleanup) => Err(Error::Invalid(format!(
                    "{error}; 持久保存恢复失败: {cleanup}"
                ))),
            },
        }
    }
    pub fn commit(&mut self, backend: &impl BuildBackend) -> Result<()> {
        if self.lock.is_none() {
            return Err(Error::Invalid("持久事务已经结束".into()));
        }
        let root = self.state.join("transaction");
        let mut record = read(&root)?;
        if record.phase != Phase::Preparing {
            return Err(Error::Invalid("持久事务提交状态不符".into()));
        }
        let result = (|| {
            recover(backend, &self.pool, &self.state.join("save"))?;
            if record.working {
                for entry in &mut record.entries {
                    entry.had_original = disk::exists(&self.pool.join(&entry.name))?;
                }
                record.working = false;
                write_journal(&root, &record)?;
            }
            publish(backend, &self.pool, &root, record)
        })();
        match result {
            Ok(()) => {
                self.finish();
                Ok(())
            }
            Err(error) => match self.rollback(backend) {
                Ok(()) => Err(error),
                Err(c) => Err(Error::Invalid(format!("{error}; 持久提交恢复失败: {c}"))),
            },
        }
    }
    pub fn rollback(&mut self, backend: &impl BuildBackend) -> Result<()> {
        if self.lock.is_none() {
            return Ok(());
        }
        recover(backend, &self.pool, &self.state.join("save"))?;
        recover(backend, &self.pool, &self.state.join("transaction"))?;
        self.finish();
        Ok(())
    }
    fn finish(&mut self) {
        if let Some(file) = self.lock.take() {
            let _ = file.unlock();
        }
    }
}
/// Publish one journaled set, shared by an explicit save and the final commit.
fn publish(
    backend: &impl BuildBackend,
    pool: &Path,
    root: &Path,
    mut record: Journal,
) -> Result<()> {
    validate_layout(root, &record)?;
    for entry in &record.entries {
        let new = root.join("new").join(&entry.name);
        backend.check_subvolume(&new, false)?;
        backend.check_no_nested_subvolumes(&new)?;
        let original = pool.join(&entry.name);
        if entry.had_original {
            backend.check_subvolume(&original, false)?;
            backend.check_no_nested_subvolumes(&original)?;
        } else if disk::exists(&original)? {
            return Err(Error::Invalid("事务期间出现新的正式持久子卷".into()));
        }
        if disk::exists(&root.join("old").join(&entry.name))? {
            return Err(Error::Invalid("事务已有旧卷".into()));
        }
    }
    flush(root)?;
    record.phase = Phase::Swapping;
    write_journal(root, &record)?;
    for entry in &record.entries {
        let published = pool.join(&entry.name);
        if entry.had_original {
            rename(&published, &root.join("old").join(&entry.name))?;
        }
        rename(&root.join("new").join(&entry.name), &published)?;
    }
    record.phase = Phase::Committed;
    write_journal(root, &record)?;
    recover(backend, pool, root)
}
fn read(root: &Path) -> Result<Journal> {
    let path = root.join("journal.toml");
    disk::checked(&path)?;
    if !fs::symlink_metadata(&path)
        .map_err(|e| disk::io(&path, e))?
        .is_file()
    {
        return Err(Error::Invalid("持久事务日志必须是普通文件".into()));
    }
    let record: Journal =
        toml::from_str(&fs::read_to_string(&path).map_err(|e| disk::io(&path, e))?)
            .map_err(|e| Error::Invalid(format!("持久事务日志无效: {e}")))?;
    if record.schema_version != 1 {
        return Err(Error::Invalid("不支持的持久事务日志版本".into()));
    }
    if record.working && record.phase != Phase::Preparing {
        return Err(Error::Invalid("工作卷日志不能处于发布阶段".into()));
    }
    let mut names = BTreeSet::new();
    for entry in &record.entries {
        super::plugins::validate_identity(&entry.name)?;
        if !names.insert(&entry.name) {
            return Err(Error::Invalid("持久日志名称重复".into()));
        }
    }
    Ok(record)
}
fn validate_layout(root: &Path, record: &Journal) -> Result<()> {
    disk::checked(root)?;
    for entry in fs::read_dir(root).map_err(|e| disk::io(root, e))? {
        let entry = entry.map_err(|e| disk::io(root, e))?;
        match entry.file_name().to_str() {
            Some("journal.toml" | ".journal.pending") => {
                if !fs::symlink_metadata(entry.path())
                    .map_err(|e| disk::io(&entry.path(), e))?
                    .is_file()
                {
                    return Err(Error::Invalid("日志暂存必须是普通文件".into()));
                }
            }
            Some("new" | "old") => {
                disk::checked(&entry.path())?;
                for child in fs::read_dir(entry.path()).map_err(|e| disk::io(&entry.path(), e))? {
                    let child = child.map_err(|e| disk::io(&entry.path(), e))?;
                    if !record
                        .entries
                        .iter()
                        .any(|e| child.file_name() == std::ffi::OsStr::new(&e.name))
                    {
                        return Err(Error::Invalid("持久事务有未记录子卷".into()));
                    }
                    disk::checked(&child.path())?;
                }
            }
            _ => {
                return Err(Error::Invalid(format!(
                    "持久事务有未知条目: {}",
                    entry.path().display()
                )));
            }
        }
    }
    Ok(())
}
fn recover(backend: &impl BuildBackend, pool: &Path, root: &Path) -> Result<()> {
    if !disk::exists(root)? {
        return Ok(());
    }
    if !disk::exists(&root.join("journal.toml"))? {
        for item in fs::read_dir(root).map_err(|e| disk::io(root, e))? {
            let item = item.map_err(|e| disk::io(root, e))?;
            if item.file_name() != std::ffi::OsStr::new(".journal.pending")
                || !item
                    .file_type()
                    .map_err(|e| disk::io(&item.path(), e))?
                    .is_file()
            {
                return Err(Error::Invalid("持久事务目录存在数据但缺少日志".into()));
            }
            fs::remove_file(item.path()).map_err(|e| disk::io(&item.path(), e))?;
        }
        fs::remove_dir(root).map_err(|e| disk::io(root, e))?;
        return disk::sync(root.parent().unwrap());
    }
    let mut record = read(root)?;
    validate_layout(root, &record)?;
    if record.phase == Phase::Swapping {
        for entry in &record.entries {
            let published = pool.join(&entry.name);
            let new = root.join("new").join(&entry.name);
            let old = root.join("old").join(&entry.name);
            let (p, n, o) = (
                disk::exists(&published)?,
                disk::exists(&new)?,
                disk::exists(&old)?,
            );
            let valid = if entry.had_original {
                matches!(
                    (p, n, o),
                    (true, true, false) | (false, true, true) | (true, false, true)
                )
            } else {
                !o && (p != n)
            };
            if !valid {
                return Err(Error::Invalid(format!(
                    "持久事务恢复状态不符: {}",
                    entry.name
                )));
            }
            for path in [&published, &new, &old] {
                if disk::exists(path)? {
                    backend.check_subvolume(path, false)?;
                }
            }
        }
        for entry in record.entries.iter().rev() {
            let published = pool.join(&entry.name);
            let new = root.join("new").join(&entry.name);
            let old = root.join("old").join(&entry.name);
            if disk::exists(&old)? {
                if disk::exists(&published)? {
                    rename(&published, &new)?;
                }
                rename(&old, &published)?;
            } else if !entry.had_original && disk::exists(&published)? {
                rename(&published, &new)?;
            }
        }
        record.phase = Phase::Preparing;
        write_journal(root, &record)?;
    }
    // Validate the entire published set before deleting any transaction data.
    for entry in &record.entries {
        let published = pool.join(&entry.name);
        let old = root.join("old").join(&entry.name);
        let new = root.join("new").join(&entry.name);
        if record.phase == Phase::Committed {
            backend.check_subvolume(&published, false)?;
            if disk::exists(&new)? || (!entry.had_original && disk::exists(&old)?) {
                return Err(Error::Invalid("已提交持久事务布局不符".into()));
            }
        } else {
            if disk::exists(&old)? {
                return Err(Error::Invalid("准备阶段不能存在旧卷".into()));
            }
            if record.working {
                // Explicit saves may have replaced or first published this volume.
                if disk::exists(&published)? {
                    backend.check_subvolume(&published, false)?;
                }
            } else if entry.had_original {
                backend.check_subvolume(&published, false)?;
            } else if disk::exists(&published)? {
                return Err(Error::Invalid("未提交的新持久卷已发布".into()));
            }
        }
    }
    for entry in &record.entries {
        let owned = root
            .join(if record.phase == Phase::Committed {
                "old"
            } else {
                "new"
            })
            .join(&entry.name);
        if disk::exists(&owned)? {
            backend.delete_subvolume_recursive(&owned)?;
            flush(root)?;
        }
    }
    for name in ["new", "old"] {
        let p = root.join(name);
        if disk::exists(&p)? {
            fs::remove_dir(&p).map_err(|e| disk::io(&p, e))?;
            disk::sync(root)?;
        }
    }
    let temporary = root.join(".journal.pending");
    if disk::exists(&temporary)? {
        fs::remove_file(&temporary).map_err(|e| disk::io(&temporary, e))?;
    }
    let journal = root.join("journal.toml");
    fs::remove_file(&journal).map_err(|e| disk::io(&journal, e))?;
    disk::sync(root)?;
    fs::remove_dir(root).map_err(|e| disk::io(root, e))?;
    disk::sync(root.parent().unwrap())
}
fn write_journal(root: &Path, record: &Journal) -> Result<()> {
    let path = root.join(".journal.pending");
    disk::checked(&path)?;
    if disk::exists(&path)?
        && !fs::symlink_metadata(&path)
            .map_err(|e| disk::io(&path, e))?
            .is_file()
    {
        return Err(Error::Invalid("日志暂存必须是普通文件".into()));
    }
    let text = toml::to_string(record).map_err(|e| Error::Invalid(e.to_string()))?;
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&path)
        .map_err(|e| disk::io(&path, e))?;
    file.write_all(text.as_bytes())
        .map_err(|e| disk::io(&path, e))?;
    file.sync_all().map_err(|e| disk::io(&path, e))?;
    fs::rename(&path, root.join("journal.toml")).map_err(|e| disk::io(root, e))?;
    disk::sync(root)
}
fn rename(source: &Path, destination: &Path) -> Result<()> {
    if disk::exists(destination)? {
        return Err(Error::Invalid(format!(
            "不覆盖持久事务目标: {}",
            destination.display()
        )));
    }
    fs::rename(source, destination).map_err(|e| disk::io(destination, e))?;
    disk::sync(source.parent().unwrap())?;
    disk::sync(destination.parent().unwrap())
}
fn flush(path: &Path) -> Result<()> {
    let file = File::open(path).map_err(|e| disk::io(path, e))?;
    if unsafe { syncfs(file.as_raw_fd()) } != 0 {
        return Err(disk::io(path, std::io::Error::last_os_error()));
    }
    Ok(())
}

#[cfg(test)]
#[path = "persistent_tests.rs"]
mod tests;
