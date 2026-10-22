//! Durable, explicitly authorized conversion of offline source directories to subvolumes.
use super::{BuildBackend, Error, Result, fs as disk};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    ffi::CString,
    fs,
    os::unix::{ffi::OsStrExt, fs::MetadataExt},
    path::{Path, PathBuf},
};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Migration {
    schema_version: u32,
    filesystem_uuid: String,
    source: PathBuf,
    old_subvolume_id: String,
    old_inode: u64,
    parent_subvolume_id: String,
    authorization: String,
    phase: String,
    replacement_id: Option<String>,
    transaction_inode: Option<u64>,
    work_id: Option<String>,
}

fn location(
    backend: &impl BuildBackend,
    source: &Path,
    manager: &Path,
) -> Result<(String, PathBuf, PathBuf, PathBuf)> {
    let (uuid, source_location) = backend.filesystem_location(source)?;
    let key = format!(
        "{:x}",
        Sha256::digest(format!("{uuid}\0{}", source_location.display()))
    );
    let record = manager
        .join("inheritance-migrations")
        .join(format!("{key}.toml"));
    let transaction = source
        .parent()
        .ok_or_else(|| {
            Error::Invalid("迁移来源缺少父目录 / Migration source has no parent directory".into())
        })?
        .join(format!(".rstarsleep-migration-{key}"));
    Ok((uuid, source_location, record, transaction))
}

fn read(path: &Path) -> Result<Migration> {
    disk::checked(path)?;
    let text = fs::read_to_string(path).map_err(|e| disk::io(path, e))?;
    let value: Migration = toml::from_str(&text)
        .map_err(|e| Error::Invalid(format!("迁移记录无效 / Invalid migration record: {e}")))?;
    if value.schema_version != 1
        || !matches!(value.phase.as_str(), "preparing" | "ready" | "committed")
    {
        return Err(Error::Invalid(
            "迁移记录版本或状态无效 / Invalid migration record version or state".into(),
        ));
    }
    Ok(value)
}

/// Accept only an exact, previously authorized conversion; never relax arbitrary ID changes.
pub(super) fn validates_identity(
    backend: &impl BuildBackend,
    source: &Path,
    manager: &Path,
    expected: &str,
    authorization: &str,
) -> Result<bool> {
    let actual = backend.source_subvolume_id(source)?;
    if actual == expected {
        return Ok(true);
    }
    let (uuid, source_location, record, transaction) = location(backend, source, manager)?;
    if !disk::exists(&record)? {
        return Ok(false);
    }
    let migration = read(&record)?;
    if migration.filesystem_uuid != uuid
        || migration.source != source_location
        || migration.old_subvolume_id != expected
        || migration.authorization != authorization
        || migration.replacement_id.as_deref() != Some(actual.as_str())
        || backend.source_subvolume_id(source.parent().unwrap())? != migration.parent_subvolume_id
    {
        return Ok(false);
    }
    if migration.phase == "ready" {
        check_transaction(&transaction, &migration)?;
        let backup = transaction.join("replacement");
        if !original_matches(backend, &backup, &migration)? {
            return Ok(false);
        }
    } else if migration.phase != "committed" {
        return Ok(false);
    }
    backend.check_subvolume(source, false)?;
    Ok(true)
}

// Reject aliases in any observable mount namespace and processes using the containing subvolume.
// An inaccessible process namespace is not evidence that a source is offline.
pub(super) fn check_offline(source: &Path) -> Result<()> {
    super::mounts::check_unmounted_tree(source)?;
    let device = fs::metadata(source).map_err(|e| disk::io(source, e))?.dev();
    let device_text = format!("{}:{}", libc::major(device), libc::minor(device));
    for process in fs::read_dir("/proc").map_err(|e| disk::io(Path::new("/proc"), e))? {
        let process = process.map_err(|e| disk::io(Path::new("/proc"), e))?;
        if !process
            .file_name()
            .as_bytes()
            .iter()
            .all(u8::is_ascii_digit)
        {
            continue;
        }
        let process = process.path();
        let mountinfo = process.join("mountinfo");
        let text = match fs::read_to_string(&mountinfo) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(disk::io(&mountinfo, e)),
        };
        if text
            .lines()
            .any(|line| line.split_whitespace().nth(2) == Some(&device_text))
        {
            return Err(Error::Invalid(format!(
                "move-snapshot 来源所属子卷仍被挂载: {0} / Subvolume containing the move-snapshot source is still mounted: {0}",
                source.display()
            )));
        }
        let maps_path = process.join("maps");
        let maps = match fs::read_to_string(&maps_path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(disk::io(&maps_path, e)),
        };
        for line in maps.lines() {
            let Some(mapped_device) = line.split_whitespace().nth(3) else {
                continue;
            };
            let Some((major, minor)) = mapped_device.split_once(':') else {
                continue;
            };
            if u32::from_str_radix(major, 16).ok() == Some(libc::major(device))
                && u32::from_str_radix(minor, 16).ok() == Some(libc::minor(device))
            {
                return Err(Error::Invalid(format!(
                    "move-snapshot 来源所属子卷仍被进程映射: {0} / Subvolume containing the move-snapshot source is still mapped by a process: {0}",
                    source.display()
                )));
            }
        }
        let mut handles = vec![process.join("root"), process.join("cwd")];
        let descriptors = process.join("fd");
        match fs::read_dir(&descriptors) {
            Ok(entries) => {
                for entry in entries {
                    handles.push(entry.map_err(|e| disk::io(&descriptors, e))?.path());
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(disk::io(&descriptors, e)),
        }
        for handle in handles {
            match fs::metadata(&handle) {
                Ok(info) if info.dev() == device => {
                    return Err(Error::Invalid(format!(
                        "move-snapshot 来源所属子卷仍被进程使用: {0} ({1}) / Subvolume containing the move-snapshot source is still used by a process: {0} ({1})",
                        source.display(),
                        handle.display()
                    )));
                }
                Ok(_) => (),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
                Err(e) => return Err(disk::io(&handle, e)),
            }
        }
    }
    Ok(())
}

fn check_tree(backend: &impl BuildBackend, source: &Path) -> Result<()> {
    backend.check_migration_source(source)?;
    let mut links: BTreeMap<(u64, u64), (u64, u64)> = BTreeMap::new();
    let mut paths = vec![source.to_path_buf()];
    while let Some(path) = paths.pop() {
        let info = fs::symlink_metadata(&path).map_err(|e| disk::io(&path, e))?;
        if info.is_dir() {
            if path != source && backend.is_subvolume(&path)? {
                backend.check_migration_source(&path)?;
            }
            for entry in fs::read_dir(&path).map_err(|e| disk::io(&path, e))? {
                paths.push(entry.map_err(|e| disk::io(&path, e))?.path());
            }
        } else {
            if !info.is_file() && !info.file_type().is_symlink() {
                return Err(Error::Invalid(format!(
                    "move-snapshot 无法安全迁移特殊文件: {0} / move-snapshot cannot safely migrate a special file: {0}",
                    path.display()
                )));
            }
            let counts = links
                .entry((info.dev(), info.ino()))
                .or_insert((0, info.nlink()));
            counts.0 += 1;
        }
        if (info.is_dir() || info.is_file()) && super::nocow::flags(&path)? & (0x10 | 0x20) != 0 {
            return Err(Error::Invalid(format!(
                "move-snapshot 来源含不可变或追加属性: {0} / move-snapshot source has immutable or append-only attributes: {0}",
                path.display()
            )));
        }
    }
    if links.values().any(|(inside, total)| inside != total) {
        return Err(Error::Invalid(
            "move-snapshot 来源含连接目录外对象的硬链接 / move-snapshot source contains hard links to objects outside the directory".into(),
        ));
    }
    Ok(())
}

fn exchange(left: &Path, right: &Path) -> Result<()> {
    let left_native = CString::new(left.as_os_str().as_bytes())
        .map_err(|_| Error::Invalid("迁移路径含 NUL / Migration path contains NUL".into()))?;
    let right_native = CString::new(right.as_os_str().as_bytes())
        .map_err(|_| Error::Invalid("迁移路径含 NUL / Migration path contains NUL".into()))?;
    // SAFETY: both paths are stable NUL-terminated strings; no descriptors are borrowed.
    if unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            left_native.as_ptr(),
            libc::AT_FDCWD,
            right_native.as_ptr(),
            libc::RENAME_EXCHANGE,
        )
    } < 0
    {
        return Err(disk::io(left, std::io::Error::last_os_error()));
    }
    Ok(())
}

fn original_matches(
    backend: &impl BuildBackend,
    path: &Path,
    migration: &Migration,
) -> Result<bool> {
    disk::checked(path)?;
    let info = fs::symlink_metadata(path).map_err(|e| disk::io(path, e))?;
    Ok(info.is_dir()
        && !backend.is_subvolume(path)?
        && info.ino() == migration.old_inode
        && backend.source_subvolume_id(path)? == migration.old_subvolume_id)
}

fn check_transaction(transaction: &Path, migration: &Migration) -> Result<()> {
    disk::checked(transaction)?;
    if disk::exists(transaction)? {
        let info = fs::symlink_metadata(transaction).map_err(|e| disk::io(transaction, e))?;
        if !info.is_dir() || Some(info.ino()) != migration.transaction_inode {
            return Err(Error::Invalid(
                "迁移临时目录身份不匹配，保留现场供手动恢复 / Migration temporary-directory identity mismatch; preserving it for manual recovery".into(),
            ));
        }
    }
    Ok(())
}

fn remove_owned(
    backend: &impl BuildBackend,
    path: &Path,
    expected_id: Option<&str>,
    original: Option<&Migration>,
) -> Result<()> {
    disk::checked(path)?;
    if !disk::exists(path)? {
        return Ok(());
    }
    let matches = match original {
        Some(migration) => original_matches(backend, path, migration)?,
        None => {
            backend.is_subvolume(path)?
                && Some(backend.source_subvolume_id(path)?.as_str()) == expected_id
        }
    };
    if !matches {
        return Err(Error::Invalid(format!(
            "迁移临时对象身份不匹配，拒绝删除: {0} / Migration temporary-object identity mismatch; refusing deletion: {0}",
            path.display()
        )));
    }
    super::inheritance::remove_tree(backend, path)
}

// Attaching nested snapshots changes parent mtimes. Restore metadata only for this
// explicitly offline conversion, rather than rescanning ordinary live snapshots.
fn restore_times(backend: &impl BuildBackend, source: &Path, target: &Path) -> Result<()> {
    for child in backend.nested_subvolumes(source)? {
        super::nocow::restore_directory_times(&source.join(&child), &target.join(child))?;
    }
    super::nocow::restore_directory_times(source, target)
}

/// Recover or convert a source while the caller holds the manager inheritance lock.
/// The original complete tree remains intact until the replacement has been synced.
pub(super) fn prepare(
    backend: &impl BuildBackend,
    source: &Path,
    manager: &Path,
    authorization: &str,
) -> Result<()> {
    let (uuid, source_location, record, transaction) = location(backend, source, manager)?;
    let existing = disk::exists(&record)?;
    if !existing && (backend.is_subvolume(source)? || !super::nocow::contains_nocow(source)?) {
        return Ok(());
    }
    let mut migration = if existing {
        let migration = read(&record)?;
        if migration.filesystem_uuid != uuid || migration.source != source_location {
            return Err(Error::Invalid(
                "来源迁移记录身份不匹配 / Source migration record identity mismatch".into(),
            ));
        }
        migration
    } else {
        check_tree(backend, source)?;
        if disk::exists(&transaction)? {
            return Err(Error::Invalid("存在未登记的来源迁移目录，拒绝覆盖 / An unregistered source migration directory exists; refusing to overwrite it".into()));
        }
        disk::directory(record.parent().unwrap())?;
        let migration = Migration {
            schema_version: 1,
            filesystem_uuid: uuid,
            source: source_location,
            old_subvolume_id: backend.source_subvolume_id(source)?,
            old_inode: fs::symlink_metadata(source)
                .map_err(|e| disk::io(source, e))?
                .ino(),
            parent_subvolume_id: backend.source_subvolume_id(source.parent().unwrap())?,
            authorization: authorization.into(),
            phase: "preparing".into(),
            replacement_id: None,
            transaction_inode: None,
            work_id: None,
        };
        disk::write(&record, &migration)?;
        migration
    };
    if backend.source_subvolume_id(source.parent().unwrap())? != migration.parent_subvolume_id {
        return Err(Error::Invalid(
            "来源迁移父目录身份已改变 / Source migration parent-directory identity has changed"
                .into(),
        ));
    }
    let replacement = transaction.join("replacement");
    let work = transaction.join("work");
    check_transaction(&transaction, &migration)?;
    let current_id = backend.source_subvolume_id(source)?;
    if migration.replacement_id.as_deref() == Some(current_id.as_str()) {
        backend.check_subvolume(source, false)?;
        if migration.phase != "committed" {
            if migration.authorization != authorization
                || !original_matches(backend, &replacement, &migration)?
            {
                return Err(Error::Invalid("迁移中断后的原目录备份身份不匹配 / Original-directory backup identity mismatch after interrupted migration".into()));
            }
            migration.phase = "committed".into();
            backend.sync_filesystem(source)?;
            disk::write(&record, &migration)?;
        }
    } else {
        if migration.phase == "committed"
            || migration.authorization != authorization
            || current_id != migration.old_subvolume_id
            || fs::symlink_metadata(source)
                .map_err(|e| disk::io(source, e))?
                .ino()
                != migration.old_inode
        {
            return Err(Error::Invalid("来源迁移对象或授权已改变，拒绝继续 / Source migration object or authorization has changed; refusing to continue".into()));
        }
        check_tree(backend, source)?;
        if !disk::exists(&transaction)? {
            fs::create_dir(&transaction).map_err(|e| disk::io(&transaction, e))?;
            migration.transaction_inode = Some(
                fs::symlink_metadata(&transaction)
                    .map_err(|e| disk::io(&transaction, e))?
                    .ino(),
            );
            disk::sync(source.parent().unwrap())?;
            disk::write(&record, &migration)?;
        }
        remove_owned(
            backend,
            &replacement,
            migration.replacement_id.as_deref(),
            None,
        )?;
        remove_owned(backend, &work, migration.work_id.as_deref(), None)?;
        backend.create_subvolume(&work)?;
        migration.work_id = Some(backend.source_subvolume_id(&work)?);
        migration.replacement_id = None;
        migration.phase = "preparing".into();
        disk::write(&record, &migration)?;
        super::inheritance::copy_directory(
            backend,
            source,
            &work,
            crate::config::NocowHandling::Copy,
        )?;
        restore_times(backend, source, &work)?;
        if super::inheritance_inputs::migration_hash(backend, source)?
            != super::inheritance_inputs::migration_hash(backend, &work)?
        {
            return Err(Error::Invalid("来源迁移校验失败，保留原目录 / Source migration validation failed; preserving the original directory".into()));
        }
        backend.snapshot_recursive(&work, &replacement, false)?;
        restore_times(backend, &work, &replacement)?;
        // Preserve the source's existing readonly child-subvolume policy on restoration.
        for child in backend.nested_subvolumes(source)? {
            if backend.check_subvolume(&source.join(&child), true).is_ok() {
                backend.set_readonly(&replacement.join(child), true)?;
            }
        }
        migration.replacement_id = Some(backend.source_subvolume_id(&replacement)?);
        migration.phase = "ready".into();
        backend.sync_filesystem(&replacement)?;
        disk::write(&record, &migration)?;
        // Revalidate after the potentially expensive copy and before mutating the source.
        check_tree(backend, source)?;
        if super::inheritance_inputs::migration_hash(backend, source)?
            != super::inheritance_inputs::migration_hash(backend, &replacement)?
        {
            return Err(Error::Invalid("来源在迁移期间改变，保留原目录 / Source changed during migration; preserving the original directory".into()));
        }
        if !original_matches(backend, source, &migration)?
            || backend.source_subvolume_id(source.parent().unwrap())?
                != migration.parent_subvolume_id
            || backend.source_subvolume_id(&replacement)?
                != migration.replacement_id.as_deref().unwrap()
        {
            return Err(Error::Invalid("交换前来源或临时子卷身份已改变 / Source or temporary subvolume identity changed before exchange".into()));
        }
        check_transaction(&transaction, &migration)?;
        exchange(source, &replacement)?;
        disk::sync(source.parent().unwrap())?;
        disk::sync(&transaction)?;
        backend.sync_filesystem(source)?;
        migration.phase = "committed".into();
        disk::write(&record, &migration)?;
    }
    // A committed receipt remains outside the source tree for old pending-ID verification.
    check_transaction(&transaction, &migration)?;
    remove_owned(backend, &replacement, None, Some(&migration))?;
    remove_owned(backend, &work, migration.work_id.as_deref(), None)?;
    if disk::exists(&transaction)? {
        fs::remove_dir(&transaction).map_err(|e| disk::io(&transaction, e))?;
        disk::sync(source.parent().unwrap())?;
    }
    Ok(())
}

/// An interrupted conversion protects its source from ordinary version/cache cleanup.
pub(super) fn pinned_sources(manager: &Path) -> Result<Vec<PathBuf>> {
    let records = manager.join("inheritance-migrations");
    if !disk::exists(&records)? {
        return Ok(Vec::new());
    }
    let mut sources = Vec::new();
    for entry in fs::read_dir(&records).map_err(|e| disk::io(&records, e))? {
        let record = read(&entry.map_err(|e| disk::io(&records, e))?.path())?;
        if record.phase != "committed" {
            sources.push(record.source);
        }
    }
    Ok(sources)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        config::NocowHandling,
        controller::{BuildContext, SystemBackend, plugins::LoadedPlugin},
    };
    use std::{cell::Cell, process::Command};

    #[derive(Default)]
    struct TestBackend {
        fail_snapshot: Cell<bool>,
        fail_after_exchange: Cell<bool>,
        fail_cleanup: Cell<bool>,
        busy: Cell<bool>,
    }
    fn native(arguments: &[&std::ffi::OsStr]) -> Result<String> {
        let output = Command::new("btrfs")
            .args(arguments)
            .output()
            .map_err(|e| disk::io(Path::new("btrfs"), e))?;
        if !output.status.success() {
            return Err(Error::Invalid(
                String::from_utf8_lossy(&output.stderr).into(),
            ));
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().into())
    }
    impl BuildBackend for TestBackend {
        fn create_subvolume(&self, p: &Path) -> Result<()> {
            native(&["subvolume".as_ref(), "create".as_ref(), p.as_os_str()])?;
            Ok(())
        }
        fn check_subvolume(&self, p: &Path, ro: bool) -> Result<()> {
            if !self.is_subvolume(p)? {
                return Err(Error::Invalid("not a native subvolume".into()));
            }
            let value = native(&[
                "property".as_ref(),
                "get".as_ref(),
                "-ts".as_ref(),
                p.as_os_str(),
                "ro".as_ref(),
            ])?;
            if value != format!("ro={ro}") {
                return Err(Error::Invalid("unexpected readonly property".into()));
            }
            Ok(())
        }
        fn snapshot(&self, s: &Path, t: &Path, ro: bool) -> Result<()> {
            if t.ends_with("replacement") && self.fail_snapshot.replace(false) {
                return Err(Error::Invalid(
                    "injected replacement snapshot failure".into(),
                ));
            }
            native(&[
                "subvolume".as_ref(),
                "snapshot".as_ref(),
                s.as_os_str(),
                t.as_os_str(),
            ])?;
            self.set_readonly(t, ro)
        }
        fn delete_subvolume(&self, p: &Path) -> Result<()> {
            self.set_readonly(p, false)?;
            // rmdir permits an owning unprivileged user to remove test subvolumes.
            fs::remove_dir_all(p).map_err(|e| disk::io(p, e))
        }
        fn delete_subvolume_recursive(&self, p: &Path) -> Result<()> {
            if p.ends_with("work") && self.fail_cleanup.replace(false) {
                return Err(Error::Invalid("injected cleanup failure".into()));
            }
            self.set_readonly(p, false)?;
            for child in self.nested_subvolumes(p)?.iter().rev() {
                self.set_readonly(&p.join(child), false)?;
            }
            self.delete_subvolume(p)
        }
        fn nested_subvolumes(&self, p: &Path) -> Result<Vec<PathBuf>> {
            let mut result = Vec::new();
            let mut pending = vec![p.to_path_buf()];
            while let Some(path) = pending.pop() {
                for child in fs::read_dir(&path).map_err(|e| disk::io(&path, e))? {
                    let child = child.map_err(|e| disk::io(&path, e))?.path();
                    if fs::symlink_metadata(&child)
                        .map_err(|e| disk::io(&child, e))?
                        .is_dir()
                    {
                        if self.is_subvolume(&child)? {
                            result.push(child.strip_prefix(p).unwrap().to_path_buf());
                        }
                        pending.push(child);
                    }
                }
            }
            result.sort_by_key(|p| std::cmp::Reverse(p.components().count()));
            Ok(result)
        }
        fn is_subvolume(&self, p: &Path) -> Result<bool> {
            let info = fs::symlink_metadata(p).map_err(|e| disk::io(p, e))?;
            Ok(info.is_dir() && info.ino() == 256)
        }
        fn source_subvolume_id(&self, p: &Path) -> Result<String> {
            SystemBackend.source_subvolume_id(p)
        }
        fn filesystem_location(&self, p: &Path) -> Result<(String, PathBuf)> {
            Ok(("test-uuid".into(), p.to_path_buf()))
        }
        fn set_readonly(&self, p: &Path, ro: bool) -> Result<()> {
            native(&[
                "property".as_ref(),
                "set".as_ref(),
                "-ts".as_ref(),
                p.as_os_str(),
                "ro".as_ref(),
                ro.to_string().as_ref(),
            ])?;
            Ok(())
        }
        fn sync_filesystem(&self, p: &Path) -> Result<()> {
            if p.ends_with("source")
                && self.is_subvolume(p)?
                && self.fail_after_exchange.replace(false)
            {
                return Err(Error::Invalid("injected failure after exchange".into()));
            }
            disk::sync(p)
        }
        fn copy_inherited_with_policy(
            &self,
            s: &Path,
            t: &Path,
            policy: NocowHandling,
        ) -> Result<()> {
            SystemBackend.copy_inherited_with_policy(s, t, policy)
        }
        fn check_migration_source(&self, _: &Path) -> Result<()> {
            if self.busy.get() {
                return Err(Error::Invalid("injected active source".into()));
            }
            Ok(())
        }
        fn invoke(
            &self,
            _: &LoadedPlugin,
            _: &str,
            _: &[String],
            _: &Path,
            _: &BuildContext,
        ) -> Result<String> {
            unreachable!()
        }
    }

    struct Fixture {
        directory: tempfile::TempDir,
        source: PathBuf,
        root: PathBuf,
        manager: PathBuf,
    }
    impl Fixture {
        fn new() -> Option<Self> {
            let directory = super::super::nocow::tests::btrfs_fixture()?;
            let root = directory.path().join("root");
            fs::create_dir(&root).unwrap();
            let source = super::super::nocow::tests::source_tree(&root);
            let manager = directory.path().join("manager");
            fs::create_dir_all(manager.join("systems")).unwrap();
            Some(Self {
                directory,
                source,
                root,
                manager,
            })
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let backend = TestBackend::default();
            for child in backend.nested_subvolumes(&self.root).unwrap().iter().rev() {
                backend.set_readonly(&self.root.join(child), false).unwrap();
            }
            fs::remove_dir_all(&self.root).unwrap();
            assert!(self.directory.path().exists());
        }
    }

    #[test]
    fn ordinary_nocow_directory_becomes_source_subvolume_and_preserves_tree() {
        let Some(fixture) = Fixture::new() else {
            return;
        };
        let backend = TestBackend::default();
        let before =
            super::super::inheritance_inputs::migration_hash(&backend, &fixture.source).unwrap();
        let old = backend.source_subvolume_id(&fixture.source).unwrap();
        prepare(
            &backend,
            &fixture.source,
            &fixture.manager,
            "manifest-sha256",
        )
        .unwrap();
        backend.check_subvolume(&fixture.source, false).unwrap();
        assert_ne!(old, backend.source_subvolume_id(&fixture.source).unwrap());
        assert_eq!(
            before,
            super::super::inheritance_inputs::migration_hash(&backend, &fixture.source).unwrap()
        );
        assert!(
            validates_identity(
                &backend,
                &fixture.source,
                &fixture.manager,
                &old,
                "manifest-sha256"
            )
            .unwrap()
        );
        assert!(
            !validates_identity(
                &backend,
                &fixture.source,
                &fixture.manager,
                &old,
                "other-manifest"
            )
            .unwrap()
        );
        assert!(pinned_sources(&fixture.manager).unwrap().is_empty());
        let target = fixture.root.join("target");
        super::super::inheritance::inherit_path(
            &backend,
            &fixture.source,
            &target,
            true,
            NocowHandling::MoveSnapshot,
        )
        .unwrap();
        assert_eq!(
            before,
            super::super::inheritance_inputs::migration_hash(&backend, &target).unwrap()
        );
    }

    #[test]
    fn failures_before_and_after_exchange_are_recoverable_without_source_data_loss() {
        for phase in ["snapshot", "exchange", "cleanup"] {
            let Some(fixture) = Fixture::new() else {
                return;
            };
            let backend = TestBackend::default();
            let old = backend.source_subvolume_id(&fixture.source).unwrap();
            let before =
                super::super::inheritance_inputs::migration_hash(&backend, &fixture.source)
                    .unwrap();
            match phase {
                "snapshot" => backend.fail_snapshot.set(true),
                "exchange" => backend.fail_after_exchange.set(true),
                _ => backend.fail_cleanup.set(true),
            }
            assert!(prepare(&backend, &fixture.source, &fixture.manager, "manifest").is_err());
            assert_eq!(
                before,
                super::super::inheritance_inputs::migration_hash(&backend, &fixture.source)
                    .unwrap()
            );
            assert!(
                validates_identity(
                    &backend,
                    &fixture.source,
                    &fixture.manager,
                    &old,
                    "manifest"
                )
                .unwrap()
            );
            if phase != "cleanup" {
                assert_eq!(pinned_sources(&fixture.manager).unwrap().len(), 1);
            }
            prepare(&backend, &fixture.source, &fixture.manager, "manifest").unwrap();
            backend.check_subvolume(&fixture.source, false).unwrap();
            assert!(pinned_sources(&fixture.manager).unwrap().is_empty());
        }
    }

    #[test]
    fn busy_and_external_hardlink_sources_are_rejected_before_mutation() {
        let Some(fixture) = Fixture::new() else {
            return;
        };
        let backend = TestBackend::default();
        backend.busy.set(true);
        assert!(prepare(&backend, &fixture.source, &fixture.manager, "manifest").is_err());
        backend.busy.set(false);
        fs::hard_link(fixture.source.join(".hidden"), fixture.root.join("outside")).unwrap();
        assert!(prepare(&backend, &fixture.source, &fixture.manager, "manifest").is_err());
        assert!(!backend.is_subvolume(&fixture.source).unwrap());
        assert!(pinned_sources(&fixture.manager).unwrap().is_empty());
    }
    fn pending_fixture(fixture: &Fixture, backend: &TestBackend, version: &str) -> PathBuf {
        use super::super::deferred::{InheritanceSource, PendingInheritance};
        let directory = fixture
            .manager
            .join("systems/desktop/versions")
            .join(version);
        fs::create_dir_all(&directory).unwrap();
        let prepared = directory.join("prepared-root");
        backend.create_subvolume(&prepared).unwrap();
        backend.set_readonly(&prepared, true).unwrap();
        let manifest = PendingInheritance {
            schema_version: 1,
            source_root: None,
            system_id: "desktop".into(),
            version: version.into(),
            filesystem_uuid: "test-uuid".into(),
            prepared_root: prepared.clone(),
            prepared_subvolume_id: backend.source_subvolume_id(&prepared).unwrap(),
            final_root: directory.join("root"),
            directory_subvolume: true,
            nocow_handling: NocowHandling::MoveSnapshot,
            sources: vec![InheritanceSource {
                path: fixture.source.clone(),
                target: "/var/".into(),
                subvolume_id: backend.source_subvolume_id(&fixture.source).unwrap(),
                directory: true,
            }],
        };
        let path = directory.join("inheritance-pending.toml");
        disk::write(&path, &manifest).unwrap();
        fs::write(
            directory.join("state.toml"),
            "state='pending-inheritance'\n",
        )
        .unwrap();
        path
    }

    #[test]
    fn boot_migration_retries_only_with_original_manifest_and_preserves_readonly_children() {
        for schema in [1, 2] {
            let Some(fixture) = Fixture::new() else {
                return;
            };
            let backend = TestBackend::default();
            let child = fixture.source.join("nested");
            backend.create_subvolume(&child).unwrap();
            fs::write(child.join("data"), "nested subvolume data").unwrap();
            backend.set_readonly(&child, true).unwrap();
            let manifest = pending_fixture(&fixture, &backend, "v00000001");
            if schema == 2 {
                use super::super::deferred::{InheritanceRoot, PendingInheritance};
                let mut task: PendingInheritance =
                    toml::from_str(&fs::read_to_string(&manifest).unwrap()).unwrap();
                task.schema_version = 2;
                task.source_root = Some(InheritanceRoot {
                    filesystem_uuid: "test-uuid".into(),
                    path: fixture.root.clone(),
                    subvolume_id: backend.source_subvolume_id(&fixture.root).unwrap(),
                });
                task.sources[0].path = "/source/".into();
                task.sources[0].target = "/source/".into();
                task.sources[0].subvolume_id.clear();
                disk::write(&manifest, &task).unwrap();
            }
            let original = fs::read_to_string(&manifest).unwrap();
            backend.fail_after_exchange.set(true);
            let first_error = super::super::deferred::finalize_inheritance_with_backend(
                &manifest,
                Path::new("/"),
                &backend,
            )
            .unwrap_err();
            assert!(
                first_error
                    .to_string()
                    .contains("injected failure after exchange"),
                "{first_error}"
            );
            assert!(!manifest.parent().unwrap().join("root").exists());
            backend.check_subvolume(&fixture.source, false).unwrap();
            backend.check_subvolume(&child, true).unwrap();
            fs::write(&manifest, format!("{original}\n# edited task\n")).unwrap();
            assert!(
                super::super::deferred::finalize_inheritance_with_backend(
                    &manifest,
                    Path::new("/"),
                    &backend
                )
                .is_err()
            );
            fs::write(&manifest, &original).unwrap();
            super::super::deferred::finalize_inheritance_with_backend(
                &manifest,
                Path::new("/"),
                &backend,
            )
            .unwrap();
            assert_eq!(fs::read_to_string(&manifest).unwrap(), original);
            let inherited = manifest.parent().unwrap().join(if schema == 1 {
                "root/var"
            } else {
                "root/source"
            });
            backend.check_subvolume(&inherited, false).unwrap();
            backend
                .check_subvolume(&inherited.join("nested"), false)
                .unwrap();
            assert_eq!(
                fs::read(inherited.join("nested/data")).unwrap(),
                b"nested subvolume data"
            );
            // Fixture cleanup includes prepared/final native readonly subvolumes.
            for child in backend
                .nested_subvolumes(&fixture.manager)
                .unwrap()
                .iter()
                .rev()
            {
                backend
                    .set_readonly(&fixture.manager.join(child), false)
                    .unwrap();
            }
        }
    }

    #[test]
    fn overlapping_pending_and_failed_sources_do_not_block_conversion() {
        for state in ["pending-inheritance", "failed"] {
            for overlap in ["same", "ancestor", "descendant"] {
                let Some(fixture) = Fixture::new() else {
                    return;
                };
                let backend = TestBackend::default();
                let other = pending_fixture(&fixture, &backend, "v00000001");
                let mut manifest = super::super::deferred::read_manifest(&other).unwrap();
                manifest.sources[0].path = match overlap {
                    "ancestor" => fixture.root.clone(),
                    "descendant" => fixture.source.join("journal"),
                    _ => fixture.source.clone(),
                };
                disk::write(&other, &manifest).unwrap();
                let state_path = other.parent().unwrap().join("state.toml");
                fs::write(&state_path, format!("state='{state}'\n")).unwrap();
                let saved_manifest = fs::read(&other).unwrap();
                let saved_state = fs::read(&state_path).unwrap();
                let before =
                    super::super::inheritance_inputs::migration_hash(&backend, &fixture.source)
                        .unwrap();

                prepare(&backend, &fixture.source, &fixture.manager, "build").unwrap();

                backend.check_subvolume(&fixture.source, false).unwrap();
                assert_eq!(
                    super::super::inheritance_inputs::migration_hash(&backend, &fixture.source)
                        .unwrap(),
                    before
                );
                assert_eq!(fs::read(&other).unwrap(), saved_manifest);
                assert_eq!(fs::read(&state_path).unwrap(), saved_state);
                assert!(pinned_sources(&fixture.manager).unwrap().is_empty());
                backend
                    .set_readonly(&other.parent().unwrap().join("prepared-root"), false)
                    .unwrap();
            }
        }
    }

    #[test]
    fn replaced_temporary_subvolume_is_never_deleted_on_retry() {
        let Some(fixture) = Fixture::new() else {
            return;
        };
        let backend = TestBackend::default();
        backend.fail_snapshot.set(true);
        assert!(prepare(&backend, &fixture.source, &fixture.manager, "build").is_err());
        let (_, _, _, transaction) = location(&backend, &fixture.source, &fixture.manager).unwrap();
        let work = transaction.join("work");
        fs::rename(&work, transaction.join("saved-work")).unwrap();
        backend.create_subvolume(&work).unwrap();
        fs::write(work.join("unrelated"), "must survive").unwrap();
        assert!(prepare(&backend, &fixture.source, &fixture.manager, "build").is_err());
        assert_eq!(fs::read(work.join("unrelated")).unwrap(), b"must survive");
        assert!(!backend.is_subvolume(&fixture.source).unwrap());
    }
    #[test]
    fn build_freeze_converts_actual_source_before_capturing_and_retains_frozen_policy() {
        let Some(fixture) = Fixture::new() else {
            return;
        };
        let backend = TestBackend::default();
        let system = fixture.manager.join("systems/desktop");
        let config = system.join("config");
        fs::create_dir_all(config.join("build")).unwrap();
        let inheritance_config = config.join("build/inherit.conf");
        fs::write(&inheritance_config, format!("[Inheritance]\nDirectorySubvolume=yes\nNocowHandling=move-snapshot\nCopyOnWriteBuild={}/\n", fixture.source.display())).unwrap();
        let context = BuildContext {
            system_id: "desktop".into(),
            architecture: "x86_64".into(),
            parameters: Default::default(),
            parameter_lists: Default::default(),
            environment: Default::default(),
            distribution_options: Default::default(),
            config,
            persistent_mounts: Vec::new(),
            inherited_mounts: Vec::new(),
        };
        let scratch = fixture.root.join("scratch");
        fs::create_dir(&scratch).unwrap();
        let mut owned = Vec::new();
        let frozen = super::super::inheritance_inputs::freeze(
            &backend,
            &context,
            &scratch,
            &system,
            Path::new("/"),
            &mut owned,
        )
        .unwrap();
        backend.check_subvolume(&fixture.source, false).unwrap();
        assert_eq!(owned, [scratch.join("cow-inputs")]);
        let content = fixture.source.join("journal/system.journal");
        fs::write(&content, "changed after freezing").unwrap();
        fs::write(
            &inheritance_config,
            "[Inheritance]\nDirectorySubvolume=no\nNocowHandling=error\n",
        )
        .unwrap();
        let target = fixture.root.join("applied");
        fs::create_dir(&target).unwrap();
        frozen.apply(&backend, &target).unwrap();
        let inherited = target.join(fixture.source.strip_prefix("/").unwrap());
        backend.check_subvolume(&inherited, false).unwrap();
        assert_eq!(
            fs::read(inherited.join("journal/system.journal")).unwrap(),
            b"NOCOW journal data"
        );
        assert_eq!(fs::read(&content).unwrap(), b"changed after freezing");
    }
}
