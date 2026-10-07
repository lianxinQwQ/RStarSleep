use super::*;
use crate::controller::{BuildContext, plugins::LoadedPlugin};
use std::cell::Cell;
#[derive(Default)]
struct Backend {
    snapshots: Cell<usize>,
    fail_snapshot: Cell<usize>,
}
fn copy_tree(source: &Path, destination: &Path) {
    fs::create_dir(destination).unwrap();
    for entry in fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &destination.join(entry.file_name()));
        } else {
            fs::copy(entry.path(), destination.join(entry.file_name())).unwrap();
        }
    }
}
impl BuildBackend for Backend {
    fn create_subvolume(&self, p: &Path) -> Result<()> {
        fs::create_dir_all(p).unwrap();
        fs::write(p.join(".volume"), "writable").unwrap();
        Ok(())
    }
    fn check_subvolume(&self, p: &Path, ro: bool) -> Result<()> {
        if fs::read_to_string(p.join(".volume")).ok().as_deref()
            != Some(if ro { "readonly" } else { "writable" })
        {
            return Err(Error::Invalid("not a volume".into()));
        }
        Ok(())
    }
    fn snapshot(&self, s: &Path, d: &Path, ro: bool) -> Result<()> {
        self.snapshots.set(self.snapshots.get() + 1);
        if self.fail_snapshot.get() == self.snapshots.get() {
            return Err(Error::Invalid("snapshot failure".into()));
        }
        copy_tree(s, d);
        fs::write(d.join(".volume"), if ro { "readonly" } else { "writable" }).unwrap();
        Ok(())
    }
    fn delete_subvolume(&self, p: &Path) -> Result<()> {
        self.check_subvolume(p, false)?;
        fs::remove_dir_all(p).unwrap();
        Ok(())
    }
    fn check_no_nested_subvolumes(&self, p: &Path) -> Result<()> {
        if p.join("nested/.volume").exists() {
            Err(Error::Invalid("nested volume".into()))
        } else {
            Ok(())
        }
    }
    fn delete_subvolume_recursive(&self, p: &Path) -> Result<()> {
        self.check_subvolume(p, false)
            .or_else(|_| self.check_subvolume(p, true))?;
        fs::remove_dir_all(p).unwrap();
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
fn mappings() -> Vec<PersistentDirectory> {
    vec![
        PersistentDirectory {
            name: "packages".into(),
            target: "state/packages".into(),
        },
        PersistentDirectory {
            name: "repo".into(),
            target: "state/repo".into(),
        },
    ]
}
struct Fixture {
    temp: tempfile::TempDir,
    backend: Backend,
}
impl Fixture {
    fn new() -> Self {
        Self {
            temp: tempfile::tempdir().unwrap(),
            backend: Backend::default(),
        }
    }
    fn pool(&self) -> PathBuf {
        self.temp.path().join("persistent")
    }
    fn state(&self) -> PathBuf {
        self.temp.path().join("state")
    }
    fn begin(&self) -> PersistentTransaction {
        PersistentTransaction::begin(&self.backend, &self.pool(), &self.state(), &mappings())
            .unwrap()
    }
    fn seed(&self) {
        fs::create_dir_all(self.pool()).unwrap();
        for m in mappings() {
            let p = self.pool().join(m.name);
            self.backend.create_subvolume(&p).unwrap();
            fs::write(p.join("data"), "old").unwrap();
        }
    }
}
#[test]
fn first_use_rollback_does_not_publish_empty_volumes() {
    let f = Fixture::new();
    let mut tx = f.begin();
    fs::write(tx.mounts()[0].source.join("data"), "new").unwrap();
    tx.rollback(&f.backend).unwrap();
    assert!(!f.pool().join("packages").exists());
    assert!(!f.state().join("transaction").exists());
}
#[test]
fn commit_replaces_all_volumes_and_preserves_isolation() {
    let f = Fixture::new();
    f.seed();
    let mut tx = f.begin();
    for m in tx.mounts() {
        fs::write(m.source.join("data"), "new").unwrap();
    }
    assert_eq!(
        fs::read_to_string(f.pool().join("repo/data")).unwrap(),
        "old"
    );
    tx.commit(&f.backend).unwrap();
    for m in mappings() {
        assert_eq!(
            fs::read_to_string(f.pool().join(m.name).join("data")).unwrap(),
            "new"
        );
    }
    assert!(!f.state().join("transaction").exists());
}
#[test]
fn second_snapshot_failure_removes_first_snapshot() {
    let f = Fixture::new();
    f.seed();
    f.backend.fail_snapshot.set(2);
    assert!(PersistentTransaction::begin(&f.backend, &f.pool(), &f.state(), &mappings()).is_err());
    assert!(!f.state().join("transaction").exists());
    assert_eq!(
        fs::read_to_string(f.pool().join("packages/data")).unwrap(),
        "old"
    );
}
#[test]
fn rejects_nested_original_and_new_subvolumes() {
    let f = Fixture::new();
    f.seed();
    f.backend
        .create_subvolume(&f.pool().join("packages/nested"))
        .unwrap();
    assert!(PersistentTransaction::begin(&f.backend, &f.pool(), &f.state(), &mappings()).is_err());
    fs::remove_dir_all(f.pool().join("packages/nested")).unwrap();
    let mut tx = f.begin();
    f.backend
        .create_subvolume(&tx.mounts()[0].source.join("nested"))
        .unwrap();
    assert!(tx.commit(&f.backend).is_err());
    assert_eq!(
        fs::read_to_string(f.pool().join("packages/data")).unwrap(),
        "old"
    );
    assert!(!f.state().join("transaction").exists());
}
#[test]
fn interrupted_multi_volume_swaps_restore_all_originals() {
    for step in 0..=4 {
        let f = Fixture::new();
        f.seed();
        let tx = f.begin();
        let root = f.state().join("transaction");
        let mut record = read(&root).unwrap();
        for m in tx.mounts() {
            fs::write(m.source.join("data"), "new").unwrap();
        }
        record.phase = Phase::Swapping;
        write_journal(&root, &record).unwrap();
        let mut count = 0;
        for e in &record.entries {
            if count < step {
                rename(&f.pool().join(&e.name), &root.join("old").join(&e.name)).unwrap();
            }
            count += 1;
            if count < step {
                rename(&root.join("new").join(&e.name), &f.pool().join(&e.name)).unwrap();
            }
            count += 1;
        }
        drop(tx);
        let mut next = f.begin();
        for m in mappings() {
            assert_eq!(
                fs::read_to_string(f.pool().join(m.name).join("data")).unwrap(),
                "old",
                "step {step}"
            );
        }
        next.rollback(&f.backend).unwrap();
    }
}
#[test]
fn committed_cleanup_can_resume_after_empty_directories_are_removed() {
    let f = Fixture::new();
    f.seed();
    let tx = f.begin();
    let root = f.state().join("transaction");
    let mut record = read(&root).unwrap();
    record.phase = Phase::Swapping;
    write_journal(&root, &record).unwrap();
    for e in &record.entries {
        rename(&f.pool().join(&e.name), &root.join("old").join(&e.name)).unwrap();
        rename(&root.join("new").join(&e.name), &f.pool().join(&e.name)).unwrap();
    }
    record.phase = Phase::Committed;
    write_journal(&root, &record).unwrap();
    for e in &record.entries {
        f.backend
            .delete_subvolume(&root.join("old").join(&e.name))
            .unwrap();
    }
    fs::remove_dir(root.join("new")).unwrap();
    fs::remove_dir(root.join("old")).unwrap();
    drop(tx);
    let mut next = f.begin();
    next.rollback(&f.backend).unwrap();
}
#[test]
fn malformed_journal_and_plain_directories_fail_closed() {
    let f = Fixture::new();
    fs::create_dir_all(f.pool().join("packages")).unwrap();
    assert!(PersistentTransaction::begin(&f.backend, &f.pool(), &f.state(), &mappings()).is_err());
    fs::remove_dir(f.pool().join("packages")).unwrap();
    let tx = f.begin();
    fs::write(f.state().join("transaction/journal.toml"), "malformed").unwrap();
    drop(tx);
    assert!(PersistentTransaction::begin(&f.backend, &f.pool(), &f.state(), &mappings()).is_err());
    assert!(f.state().join("transaction/new/packages").exists());
}
#[test]
fn global_lock_covers_snapshot_through_publication() {
    let f = Fixture::new();
    let mut tx = f.begin();
    assert!(PersistentTransaction::begin(&f.backend, &f.pool(), &f.state(), &mappings()).is_err());
    tx.rollback(&f.backend).unwrap();
}

#[test]
fn failed_commit_cleans_work_volume_even_if_plugin_changed_readonly_property() {
    let f = Fixture::new();
    f.seed();
    let mut tx = f.begin();
    fs::write(tx.mounts()[0].source.join(".volume"), "readonly").unwrap();
    assert!(tx.commit(&f.backend).is_err());
    assert_eq!(
        fs::read_to_string(f.pool().join("packages/data")).unwrap(),
        "old"
    );
    assert!(!f.state().join("transaction").exists());
}
