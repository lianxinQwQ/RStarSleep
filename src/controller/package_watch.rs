//! Observe package databases across inheritance, plugin behavior and arbitrary user scripts.
use std::{
    ffi::CString,
    fs, io,
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd},
        unix::{ffi::OsStrExt, fs::MetadataExt},
    },
    path::{Path, PathBuf},
};

/// A missing, replaced, overflowing or unavailable watch always requires a fresh query.
/// 缺失、替换、溢出或不可用的监听始终回退到完整查询。
pub(super) struct PackageWatch {
    fd: OwnedFd,
    identities: Vec<(PathBuf, u64, u64)>,
}

/// Each line is a target-relative database tree. An empty response disables reuse.
pub(super) fn parse_paths(output: &str) -> Option<Vec<PathBuf>> {
    let mut paths = Vec::new();
    for line in output.lines() {
        let path = PathBuf::from(line);
        if line.is_empty()
            || line.chars().any(char::is_control)
            || path
                .components()
                .any(|part| !matches!(part, std::path::Component::Normal(_)))
        {
            return None;
        }
        paths.push(path);
    }
    paths.sort();
    paths.dedup();
    Some(paths)
}

impl PackageWatch {
    pub fn start(root: &Path, paths: &[PathBuf]) -> Option<Self> {
        if paths.is_empty() {
            return None;
        }
        let raw = unsafe { libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC) };
        if raw < 0 {
            return None;
        }
        let mut watch = Self {
            fd: unsafe { OwnedFd::from_raw_fd(raw) },
            identities: Vec::new(),
        };
        for relative in paths {
            if relative.as_os_str().is_empty()
                || relative
                    .components()
                    .any(|c| !matches!(c, std::path::Component::Normal(_)))
            {
                return None;
            }
            // Pin path identities too: replacing an ancestor need not notify its descendants.
            let mut path = root.to_path_buf();
            watch.remember(&path).ok()?;
            for component in relative.components() {
                path.push(component);
                watch.remember(&path).ok()?;
            }
            watch.tree(&path).ok()?;
        }
        // Registration is not atomic. Any writes during it make this observation unusable.
        if !watch.unchanged() {
            return None;
        }
        Some(watch)
    }

    fn remember(&mut self, path: &Path) -> io::Result<()> {
        let info = fs::symlink_metadata(path)?;
        if !info.is_dir() || info.file_type().is_symlink() {
            return Err(io::Error::other(
                "数据库路径不是真实目录 / database path is not a real directory",
            ));
        }
        self.identities
            .push((path.to_path_buf(), info.dev(), info.ino()));
        Ok(())
    }

    fn observe(&self, path: &Path, directory: bool) -> io::Result<()> {
        let name = CString::new(path.as_os_str().as_bytes())?;
        let mut mask = libc::IN_MODIFY
            | libc::IN_ATTRIB
            | libc::IN_CLOSE_WRITE
            | libc::IN_CREATE
            | libc::IN_DELETE
            | libc::IN_MOVED_FROM
            | libc::IN_MOVED_TO
            | libc::IN_DELETE_SELF
            | libc::IN_MOVE_SELF
            | libc::IN_UNMOUNT
            | libc::IN_DONT_FOLLOW;
        if directory {
            mask |= libc::IN_ONLYDIR;
        }
        if unsafe { libc::inotify_add_watch(self.fd.as_raw_fd(), name.as_ptr(), mask) } < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    fn tree(&mut self, path: &Path) -> io::Result<()> {
        self.observe(path, true)?;
        self.remember(path)?;
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            let kind = entry.file_type()?;
            if kind.is_symlink() {
                return Err(io::Error::other(
                    "数据库包含符号链接 / database contains a symbolic link",
                ));
            }
            if kind.is_dir() {
                self.tree(&entry.path())?;
            } else if !kind.is_file() || entry.metadata()?.nlink() > 1 {
                // A write through a hard link outside this tree bypasses directory watches.
                return Err(io::Error::other(
                    "无法可靠地检查数据库条目 / database entry cannot be observed reliably",
                ));
            } else {
                // An alias created later outside the tree does not notify its parent watch.
                // Observe the inode too, so link changes and writes through aliases invalidate.
                self.observe(&entry.path(), false)?;
            }
        }
        Ok(())
    }

    pub fn unchanged(&self) -> bool {
        if self.identities.iter().any(|(path, dev, ino)| {
            !fs::symlink_metadata(path).is_ok_and(|info| {
                info.is_dir()
                    && !info.file_type().is_symlink()
                    && info.dev() == *dev
                    && info.ino() == *ino
            })
        }) {
            return false;
        }
        // Any event, including overflow/ignored/unmount, conservatively invalidates the result.
        let mut buffer = [0u8; 4096];
        loop {
            let read = unsafe {
                libc::read(
                    self.fd.as_raw_fd(),
                    buffer.as_mut_ptr().cast(),
                    buffer.len(),
                )
            };
            if read >= 0 {
                return false;
            }
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return error.kind() == io::ErrorKind::WouldBlock;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> tempfile::TempDir {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir_all(temp.path().join("db/pkg")).unwrap();
        fs::write(temp.path().join("db/pkg/desc"), "explicit").unwrap();
        temp
    }
    fn watch(root: &Path) -> PackageWatch {
        PackageWatch::start(root, &["db".into()]).unwrap()
    }
    #[test]
    fn parses_only_target_relative_query_paths() {
        assert_eq!(
            parse_paths("custom database/local\n"),
            Some(vec!["custom database/local".into()])
        );
        assert_eq!(parse_paths(""), Some(Vec::new()));
        for output in [
            "/db\n",
            "../db\n",
            "var/../db\n",
            "\n",
            "db\tbad\n",
            "db\0bad\n",
        ] {
            assert_eq!(parse_paths(output), None);
        }
    }
    #[test]
    fn unrelated_password_updates_and_reads_preserve_queries() {
        let temp = fixture();
        let w = watch(temp.path());
        fs::write(temp.path().join("shadow"), "new password").unwrap();
        fs::read(temp.path().join("db/pkg/desc")).unwrap();
        assert!(w.unchanged());
    }
    #[test]
    fn detects_in_place_reason_changes_and_new_packages() {
        for add in [false, true] {
            let temp = fixture();
            let w = watch(temp.path());
            if add {
                fs::create_dir(temp.path().join("db/new")).unwrap();
            } else {
                fs::write(temp.path().join("db/pkg/desc"), "dependency").unwrap();
            }
            assert!(!w.unchanged());
        }
    }
    #[test]
    fn detects_ancestor_replacement() {
        let temp = fixture();
        let w = watch(temp.path());
        fs::rename(temp.path().join("db"), temp.path().join("old-db")).unwrap();
        fs::create_dir_all(temp.path().join("db/pkg")).unwrap();
        assert!(!w.unchanged());
    }
    #[test]
    fn detects_new_external_hardlinks_and_writes_through_them() {
        for write in [false, true] {
            let temp = fixture();
            let w = watch(temp.path());
            let alias = temp.path().join("outside-desc");
            fs::hard_link(temp.path().join("db/pkg/desc"), &alias).unwrap();
            if write {
                fs::write(alias, "dependency").unwrap();
            }
            assert!(!w.unchanged());
        }
    }
    #[test]
    fn unavailable_or_symlinked_databases_require_queries() {
        let temp = fixture();
        assert!(PackageWatch::start(temp.path(), &["missing".into()]).is_none());
        assert!(PackageWatch::start(temp.path(), &[]).is_none());
        std::os::unix::fs::symlink("desc", temp.path().join("db/pkg/link")).unwrap();
        assert!(PackageWatch::start(temp.path(), &["db".into()]).is_none());
    }
}
