//! NOCOW-aware copies. Only newly created destinations are ever modified.
use super::{Error, Result, fs as disk};
use crate::config::NocowHandling;
use std::{
    collections::BTreeMap,
    fs::{self, OpenOptions},
    os::{
        fd::AsRawFd,
        unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    },
    path::{Path, PathBuf},
    process::Command,
};

const NOCOW: libc::c_long = 0x0080_0000;

pub(super) fn flags(path: &Path) -> Result<libc::c_long> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|e| disk::io(path, e))?;
    let mut flags: libc::c_long = 0;
    // SAFETY: the descriptor is live and flags points to a correctly sized ioctl buffer.
    if unsafe { libc::ioctl(file.as_raw_fd(), libc::FS_IOC_GETFLAGS, &mut flags) } < 0 {
        let error = std::io::Error::last_os_error();
        if matches!(error.raw_os_error(), Some(libc::ENOTTY | libc::EOPNOTSUPP)) {
            return Ok(0);
        }
        return Err(disk::io(path, error));
    }
    Ok(flags)
}

pub(super) fn is_nocow(path: &Path) -> Result<bool> {
    Ok(flags(path)? & NOCOW != 0)
}

pub(super) fn set_nocow(path: &Path, nocow: bool) -> Result<()> {
    let current = flags(path)?;
    let mut wanted = if nocow {
        current | NOCOW
    } else {
        current & !NOCOW
    };
    if current == wanted {
        return Ok(());
    }
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(|e| disk::io(path, e))?;
    // SAFETY: the descriptor and long-sized flags buffer live through this ioctl.
    if unsafe { libc::ioctl(file.as_raw_fd(), libc::FS_IOC_SETFLAGS, &mut wanted) } < 0 {
        return Err(disk::io(path, std::io::Error::last_os_error()));
    }
    if is_nocow(path)? != nocow {
        return Err(Error::Invalid(format!(
            "无法匹配目标 NOCOW 属性: {}",
            path.display()
        )));
    }
    Ok(())
}

struct Entry {
    relative: PathBuf,
    info: fs::Metadata,
    nocow: bool,
}

// Match cp --one-file-system: nested subvolumes are restored separately by the caller.
fn entry_path(root: &Path, relative: &Path) -> PathBuf {
    if relative.as_os_str().is_empty() {
        root.to_path_buf()
    } else {
        root.join(relative)
    }
}

fn entries(root: &Path) -> Result<Vec<Entry>> {
    let root_info = fs::symlink_metadata(root).map_err(|e| disk::io(root, e))?;
    let device = root_info.dev();
    let mut result = Vec::new();
    let mut pending = vec![PathBuf::new()];
    while let Some(relative) = pending.pop() {
        let path = entry_path(root, &relative);
        let info = fs::symlink_metadata(&path).map_err(|e| disk::io(&path, e))?;
        let boundary = !relative.as_os_str().is_empty() && info.is_dir() && info.dev() != device;
        if boundary {
            continue;
        }
        let nocow = (info.is_file() || info.is_dir()) && is_nocow(&path)?;
        if info.is_dir() {
            let mut children = fs::read_dir(&path)
                .map_err(|e| disk::io(&path, e))?
                .collect::<std::io::Result<Vec<_>>>()
                .map_err(|e| disk::io(&path, e))?;
            children.sort_by_key(|e| e.file_name());
            for child in children.into_iter().rev() {
                pending.push(relative.join(child.file_name()));
            }
        }
        result.push(Entry {
            relative,
            info,
            nocow,
        });
    }
    Ok(result)
}

pub(super) fn contains_nocow(source: &Path) -> Result<bool> {
    Ok(entries(source)?.iter().any(|e| e.info.is_file() && e.nocow))
}

/// Restoring omitted child snapshots changes ordinary parent directory timestamps.
pub(super) fn restore_directory_times(source: &Path, target: &Path) -> Result<()> {
    let device = fs::metadata(source).map_err(|e| disk::io(source, e))?.dev();
    let mut pending = vec![PathBuf::new()];
    while let Some(relative) = pending.pop() {
        let original = entry_path(source, &relative);
        let info = fs::metadata(&original).map_err(|e| disk::io(&original, e))?;
        if !relative.as_os_str().is_empty() && info.dev() != device {
            continue;
        }
        for entry in fs::read_dir(&original).map_err(|e| disk::io(&original, e))? {
            let entry = entry.map_err(|e| disk::io(&original, e))?;
            if entry
                .file_type()
                .map_err(|e| disk::io(&entry.path(), e))?
                .is_dir()
            {
                pending.push(relative.join(entry.file_name()));
            }
        }
        let path = entry_path(target, &relative);
        let accessed = info.accessed().map_err(|e| disk::io(&original, e))?;
        let modified = info.modified().map_err(|e| disk::io(&original, e))?;
        fs::File::open(&path)
            .and_then(|file| {
                file.set_times(
                    fs::FileTimes::new()
                        .set_accessed(accessed)
                        .set_modified(modified),
                )
            })
            .map_err(|e| disk::io(&path, e))?;
    }
    Ok(())
}

fn cp(
    source: &Path,
    target: &Path,
    attributes_only: bool,
    reflink: &str,
    directory: bool,
) -> Result<()> {
    let mut command = Command::new("cp");
    command.args(["--archive", "--preserve=xattr"]);
    if attributes_only {
        command.arg("--attributes-only");
    } else {
        command.arg(format!("--reflink={reflink}"));
    }
    if directory {
        command.arg("--one-file-system");
    }
    let input = if directory {
        source.join(".")
    } else {
        source.to_path_buf()
    };
    let output = command
        .arg("--")
        .arg(input)
        .arg(target)
        .output()
        .map_err(|e| disk::io(target, e))?;
    if !output.status.success() {
        return Err(Error::Command {
            command: format!("cp NOCOW inheritance {}", source.display()),
            status: output.status.to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).into(),
        });
    }
    Ok(())
}

/// Build a metadata skeleton, copy each inode exactly once, then restore metadata.
pub(super) fn copy(source: &Path, target: &Path, policy: NocowHandling) -> Result<()> {
    if !matches!(policy, NocowHandling::Reflink | NocowHandling::Copy) {
        return Err(Error::Invalid("NOCOW 复制策略不允许自动降级".into()));
    }
    disk::checked(source)?;
    disk::checked(target)?;
    let directory = fs::symlink_metadata(source)
        .map_err(|e| disk::io(source, e))?
        .is_dir();
    if disk::exists(target)?
        && (!directory
            || fs::read_dir(target)
                .map_err(|e| disk::io(target, e))?
                .next()
                .is_some())
    {
        return Err(Error::Invalid("NOCOW 复制目标必须为空".into()));
    }
    let entries = entries(source)?;
    cp(source, target, true, "always", directory)?;
    // cp -a has already restored potentially unwritable source modes.
    for entry in &entries {
        if entry.info.is_dir() || entry.info.is_file() {
            let path = entry_path(target, &entry.relative);
            fs::set_permissions(&path, fs::Permissions::from_mode(entry.info.mode() | 0o700))
                .map_err(|e| disk::io(&path, e))?;
        }
    }
    // Btrfs refuses NOCOW cloning into a multiply-linked destination inode.
    // Temporarily unlink secondary skeleton names; rebuild links after cloning.
    let mut representatives: BTreeMap<(u64, u64), PathBuf> = BTreeMap::new();
    let mut secondary = Vec::new();
    for entry in &entries {
        if !entry.info.is_file() {
            continue;
        }
        let key = (entry.info.dev(), entry.info.ino());
        let path = entry_path(target, &entry.relative);
        if let Some(first) = representatives.get(&key) {
            fs::remove_file(&path).map_err(|e| disk::io(&path, e))?;
            secondary.push((first.clone(), path));
        } else {
            representatives.insert(key, path);
        }
    }
    let mut groups = BTreeMap::new();
    for entry in &entries {
        if !entry.info.is_file() {
            continue;
        }
        let path = entry_path(target, &entry.relative);
        let key = (entry.info.dev(), entry.info.ino());
        if groups.contains_key(&key) {
            continue;
        }
        set_nocow(&path, entry.nocow)?;
        let mode = if policy == NocowHandling::Copy && entry.nocow {
            "never"
        } else {
            "always"
        };
        cp(
            &entry_path(source, &entry.relative),
            &path,
            false,
            mode,
            false,
        )?;
        let actual = fs::symlink_metadata(&path).map_err(|e| disk::io(&path, e))?;
        groups.insert(key, (actual.dev(), actual.ino()));
    }
    for (first, path) in secondary {
        fs::hard_link(first, &path).map_err(|e| disk::io(&path, e))?;
    }
    for entry in &entries {
        if entry.info.is_dir() {
            set_nocow(&entry_path(target, &entry.relative), entry.nocow)?;
        } else if !entry.info.is_file() {
            // cp --attributes-only recreates symlinks/special entries and otherwise
            // reports EEXIST. Their parents are owned real directories in this skeleton.
            let path = entry_path(target, &entry.relative);
            fs::remove_file(&path).map_err(|e| disk::io(&path, e))?;
        }
    }
    for entry in &entries {
        if entry.info.is_file() || entry.info.is_dir() {
            let path = entry_path(target, &entry.relative);
            fs::set_permissions(&path, fs::Permissions::from_mode(entry.info.mode() | 0o700))
                .map_err(|e| disk::io(&path, e))?;
        }
    }
    cp(source, target, true, "always", directory)?;
    for entry in &entries {
        if !entry.info.is_file() {
            continue;
        }
        let path = entry_path(target, &entry.relative);
        let actual = fs::symlink_metadata(&path).map_err(|e| disk::io(&path, e))?;
        if groups[&(entry.info.dev(), entry.info.ino())] != (actual.dev(), actual.ino())
            || is_nocow(&path)? != entry.nocow
        {
            return Err(Error::Invalid(format!(
                "继承硬链接或 NOCOW 属性不匹配: {}",
                path.display()
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use crate::controller::{BuildBackend, SystemBackend};
    use std::os::unix::fs::symlink;

    // Use the workspace filesystem so Btrfs fixtures work without privileged mounts.
    pub(in crate::controller) fn btrfs_fixture() -> Option<tempfile::TempDir> {
        let directory = tempfile::Builder::new()
            .prefix(".nocow-test-")
            .tempdir_in(env!("CARGO_MANIFEST_DIR"))
            .unwrap();
        let probe = directory.path().join("probe");
        fs::write(&probe, []).unwrap();
        if set_nocow(&probe, true).is_err() || !is_nocow(&probe).unwrap_or(false) {
            eprintln!("Btrfs-specific check skipped: workspace does not support NOCOW");
            return None;
        }
        Some(directory)
    }

    pub(in crate::controller) fn source_tree(root: &Path) -> PathBuf {
        let source = root.join("source");
        fs::create_dir(&source).unwrap();
        let journal = source.join("journal");
        fs::create_dir(&journal).unwrap();
        set_nocow(&journal, true).unwrap();
        fs::write(journal.join("system.journal"), b"NOCOW journal data").unwrap();
        fs::hard_link(journal.join("system.journal"), source.join("journal-link")).unwrap();
        let cow = journal.join("ordinary");
        fs::write(&cow, []).unwrap();
        set_nocow(&cow, false).unwrap();
        fs::write(&cow, b"ordinary CoW data").unwrap();
        fs::write(source.join(".hidden"), b"hidden").unwrap();
        symlink("journal/system.journal", source.join("link")).unwrap();
        source
    }

    #[test]
    fn readonly_modes_xattrs_and_timestamps_survive_explicit_copy() {
        let Some(fixture) = btrfs_fixture() else {
            return;
        };
        let source = source_tree(fixture.path());
        let file = source.join("journal/system.journal");
        let native = std::ffi::CString::new(file.as_os_str().as_encoded_bytes()).unwrap();
        // SAFETY: both strings and the value buffer remain valid for the syscall.
        assert_eq!(
            unsafe {
                libc::setxattr(
                    native.as_ptr(),
                    c"user.inheritance-test".as_ptr(),
                    b"value".as_ptr().cast(),
                    5,
                    0,
                )
            },
            0
        );
        fs::set_permissions(&file, fs::Permissions::from_mode(0o444)).unwrap();
        fs::set_permissions(source.join("journal"), fs::Permissions::from_mode(0o555)).unwrap();
        let before =
            super::super::inheritance_inputs::migration_hash(&SystemBackend, &source).unwrap();
        for policy in [NocowHandling::Reflink, NocowHandling::Copy] {
            let target = fixture.path().join(format!("metadata-{policy}"));
            fs::create_dir(&target).unwrap();
            copy(&source, &target, policy).unwrap();
            assert_eq!(
                super::super::inheritance_inputs::migration_hash(&SystemBackend, &target).unwrap(),
                before
            );
            assert_eq!(
                fs::metadata(target.join("journal/system.journal"))
                    .unwrap()
                    .mtime_nsec(),
                fs::metadata(&file).unwrap().mtime_nsec()
            );
            fs::set_permissions(target.join("journal"), fs::Permissions::from_mode(0o755)).unwrap();
        }
        fs::set_permissions(source.join("journal"), fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[test]
    fn explicit_policies_preserve_mixed_flags_hardlinks_and_symlinks() {
        let Some(fixture) = btrfs_fixture() else {
            return;
        };
        let source = source_tree(fixture.path());
        for policy in [NocowHandling::Reflink, NocowHandling::Copy] {
            let target = fixture.path().join(format!("{policy:?}"));
            fs::create_dir(&target).unwrap();
            copy(&source, &target, policy).unwrap();
            assert_eq!(
                fs::read(target.join("journal/system.journal")).unwrap(),
                b"NOCOW journal data"
            );
            assert_eq!(
                fs::read(target.join("journal/ordinary")).unwrap(),
                b"ordinary CoW data"
            );
            assert!(is_nocow(&target.join("journal/system.journal")).unwrap());
            assert!(!is_nocow(&target.join("journal/ordinary")).unwrap());
            assert!(is_nocow(&target.join("journal")).unwrap());
            assert_eq!(
                fs::metadata(target.join("journal/system.journal"))
                    .unwrap()
                    .ino(),
                fs::metadata(target.join("journal-link")).unwrap().ino()
            );
            assert_eq!(
                fs::read_link(target.join("link")).unwrap(),
                Path::new("journal/system.journal")
            );
            assert_eq!(fs::read(target.join(".hidden")).unwrap(), b"hidden");
        }
        let target = fixture.path().join("strict");
        fs::create_dir(&target).unwrap();
        assert!(
            SystemBackend
                .copy_inherited_with_policy(&source, &target, NocowHandling::Error)
                .is_err()
        );
    }

    #[test]
    fn file_policy_matches_empty_target_flags_in_both_directions() {
        let Some(fixture) = btrfs_fixture() else {
            return;
        };
        let source = fixture.path().join("file");
        fs::write(&source, b"CoW").unwrap();
        let target_directory = fixture.path().join("nocow-parent");
        fs::create_dir(&target_directory).unwrap();
        set_nocow(&target_directory, true).unwrap();
        let target = target_directory.join("copy");
        copy(&source, &target, NocowHandling::Reflink).unwrap();
        assert!(!is_nocow(&target).unwrap());
        fs::remove_file(&source).unwrap();
        fs::write(&source, []).unwrap();
        set_nocow(&source, true).unwrap();
        fs::write(&source, b"NOCOW").unwrap();
        let target = fixture.path().join("nocow-copy");
        copy(&source, &target, NocowHandling::Reflink).unwrap();
        assert!(is_nocow(&target).unwrap());
        assert_eq!(fs::read(&target).unwrap(), b"NOCOW");
        let single = fixture.path().join("single-migration");
        assert!(
            super::super::inheritance::inherit_path(
                &SystemBackend,
                &source,
                &single,
                true,
                NocowHandling::MoveSnapshot,
            )
            .is_err()
        );
        assert!(!single.exists());
    }
}
