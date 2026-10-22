//! Inspect inherited data only. Btrfs subvolumes are not filesystem boundaries.
use super::Drift;
use crate::{
    config::{InheritanceConfig, inheritance_name},
    controller::{BuildBackend, BuildContext, Result, fs as disk, logging},
};
use std::{
    collections::{BTreeSet, HashMap, HashSet},
    ffi::CString,
    fs,
    os::unix::{ffi::OsStrExt, fs::MetadataExt},
    path::{Path, PathBuf},
};

struct Scan<'a, B> {
    backend: &'a B,
    ids: &'a BTreeSet<(bool, u32)>,
    seen: HashSet<(u64, u64)>,
    hits: usize,
    examples: Vec<PathBuf>,
    errors: Vec<String>,
    skipped: usize,
}

impl<B: BuildBackend> Scan<'_, B> {
    fn visit(
        &mut self,
        path: &Path,
        device: u64,
        uuid: &Option<String>,
        devices: &mut HashMap<u64, bool>,
    ) {
        let info = match fs::symlink_metadata(path) {
            Ok(info) => info,
            Err(e) => {
                self.errors.push(format!("{}: {e}", path.display()));
                return;
            }
        };
        let allowed = *devices.entry(info.dev()).or_insert_with(|| {
            info.dev() == device
                || uuid.as_ref().is_some_and(|uuid| {
                    self.backend
                        .filesystem_location(path)
                        .is_ok_and(|(other, _)| &other == uuid)
                })
        });
        if !allowed {
            self.skipped += 1;
            return;
        }
        if !self.seen.insert((info.dev(), info.ino())) {
            return;
        }
        let mut hit =
            self.ids.contains(&(true, info.uid())) || self.ids.contains(&(false, info.gid()));
        for attribute in [c"system.posix_acl_access", c"system.posix_acl_default"] {
            match acl_ids(path, attribute) {
                Ok(ids) => hit |= ids.iter().any(|id| self.ids.contains(id)),
                Err(e) => self.errors.push(format!("{} ACL: {e}", path.display())),
            }
        }
        if hit {
            self.hits += 1;
            if self.examples.len() < 100 {
                self.examples.push(path.into());
            }
        }
        if info.is_dir() {
            match fs::read_dir(path) {
                Ok(entries) => {
                    for entry in entries {
                        match entry {
                            Ok(entry) => self.visit(&entry.path(), device, uuid, devices),
                            Err(e) => self.errors.push(format!("{}: {e}", path.display())),
                        }
                    }
                }
                Err(e) => self.errors.push(format!("{}: {e}", path.display())),
            }
        }
    }
}

fn acl_ids(path: &Path, name: &std::ffi::CStr) -> std::io::Result<Vec<(bool, u32)>> {
    let native = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| std::io::Error::from(std::io::ErrorKind::InvalidInput))?;
    let size = unsafe { libc::lgetxattr(native.as_ptr(), name.as_ptr(), std::ptr::null_mut(), 0) };
    if size < 0 {
        let e = std::io::Error::last_os_error();
        return if matches!(e.raw_os_error(), Some(libc::ENODATA | libc::EOPNOTSUPP)) {
            Ok(Vec::new())
        } else {
            Err(e)
        };
    }
    let mut data = vec![0u8; size as usize];
    let count = unsafe {
        libc::lgetxattr(
            native.as_ptr(),
            name.as_ptr(),
            data.as_mut_ptr().cast(),
            data.len(),
        )
    };
    if count < 0 {
        return Err(std::io::Error::last_os_error());
    }
    data.truncate(count as usize);
    decode_acl(&data)
}

fn decode_acl(data: &[u8]) -> std::io::Result<Vec<(bool, u32)>> {
    if data.len() < 4 || data[..4] != 2u32.to_le_bytes() || !(data.len() - 4).is_multiple_of(8) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "无效的 POSIX ACL / invalid POSIX ACL",
        ));
    }
    Ok(data[4..]
        .as_chunks::<8>()
        .0
        .iter()
        .filter_map(|entry| {
            let tag = u16::from_le_bytes(entry[..2].try_into().unwrap());
            let id = u32::from_le_bytes(entry[4..].try_into().unwrap());
            match tag {
                2 => Some((true, id)),
                8 => Some((false, id)),
                _ => None,
            }
        })
        .collect())
}

pub(super) fn assess(
    backend: &impl BuildBackend,
    context: &BuildContext,
    system: &Path,
    drifts: &[Drift],
    source_root: &Path,
) -> Result<()> {
    let config =
        InheritanceConfig::read(&crate::config::read_plugin_parameters_at(&context.config)?)?;
    let ids = drifts.iter().map(|d| (d.kind == "uid", d.old_id)).collect();
    let mut scanner = Scan {
        backend,
        ids: &ids,
        seen: HashSet::new(),
        hits: 0,
        examples: Vec::new(),
        errors: Vec::new(),
        skipped: 0,
    };
    let mut sources = config
        .copy_on_write_build
        .iter()
        .chain(&config.copy_on_write_boot)
        .map(|target| disk::source_path(source_root, target))
        .collect::<Vec<_>>();
    for target in &config.mounts {
        let persistent = system.join("persistent").join(inheritance_name(target)?);
        sources.push(if disk::exists(&persistent)? {
            Ok(persistent)
        } else {
            disk::source_path(source_root, target)
        });
    }
    for source in sources {
        let source = match source {
            Ok(source) => source,
            Err(error) => {
                scanner.errors.push(error.to_string());
                continue;
            }
        };
        let path = source.as_path();
        // Resolve a declared root once; never follow links encountered below that root.
        match fs::canonicalize(path) {
            Ok(path) => match fs::symlink_metadata(&path) {
                Ok(info) => {
                    let uuid = backend
                        .filesystem_location(&path)
                        .ok()
                        .map(|(uuid, _)| uuid);
                    scanner.visit(&path, info.dev(), &uuid, &mut HashMap::new());
                }
                Err(e) => scanner.errors.push(format!("{}: {e}", path.display())),
            },
            Err(e) => scanner.errors.push(format!("{}: {e}", path.display())),
        }
    }
    logging::diagnostic(format!(
        "继承项影响评估：{} 个对象引用漂移的 UID/GID，跳过 {} 个跨文件系统入口。",
        scanner.hits, scanner.skipped
    ));
    for path in scanner.examples {
        logging::diagnostic(format!("  {}", path.display()));
    }
    if scanner.hits > 100 {
        logging::diagnostic("仅展示前 100 个路径。");
    }
    for error in scanner.errors.iter().take(100) {
        logging::diagnostic(format!(
            "评估未完成的项目 / Items whose evaluation is incomplete: {error}"
        ));
    }
    logging::diagnostic(
        "评估仅检查当前继承来源的属主、属组和 POSIX ACL；不扫描新系统，也不解析文件内容中的数字 ID。首次启动来源之后的变化不包含在本次评估中。",
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::controller::{BuildContext, plugins::LoadedPlugin};

    struct BoundaryBackend;
    impl BuildBackend for BoundaryBackend {
        fn create_subvolume(&self, _: &Path) -> Result<()> {
            unreachable!()
        }
        fn check_subvolume(&self, _: &Path, _: bool) -> Result<()> {
            unreachable!()
        }
        fn snapshot(&self, _: &Path, _: &Path, _: bool) -> Result<()> {
            unreachable!()
        }
        fn delete_subvolume(&self, _: &Path) -> Result<()> {
            unreachable!()
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
        fn filesystem_location(&self, path: &Path) -> Result<(String, PathBuf)> {
            Ok((
                if path.starts_with("/proc") {
                    "other"
                } else {
                    "same"
                }
                .into(),
                path.into(),
            ))
        }
    }

    #[test]
    fn traversal_accepts_subvolume_device_changes_but_stops_at_other_filesystems() {
        let tree = tempfile::tempdir().unwrap();
        fs::create_dir(tree.path().join("child")).unwrap();
        fs::write(tree.path().join("child/data"), b"data").unwrap();
        std::os::unix::fs::symlink("/proc", tree.path().join("link")).unwrap();
        let info = fs::metadata(tree.path()).unwrap();
        let ids = BTreeSet::from([(true, info.uid())]);
        let mut scan = Scan {
            backend: &BoundaryBackend,
            ids: &ids,
            seen: HashSet::new(),
            hits: 0,
            examples: Vec::new(),
            errors: Vec::new(),
            skipped: 0,
        };
        // Btrfs subvolumes can expose a different st_dev within the same filesystem UUID.
        scan.visit(
            tree.path(),
            info.dev().wrapping_add(1),
            &Some("same".into()),
            &mut HashMap::new(),
        );
        assert_eq!(scan.hits, 4);
        assert!(scan.errors.is_empty());
        assert!(scan.examples.iter().all(|p| p.starts_with(tree.path())));
        assert!(fs::metadata("/proc").unwrap().dev() != info.dev());
        scan.visit(
            Path::new("/proc"),
            info.dev(),
            &Some("same".into()),
            &mut HashMap::new(),
        );
        assert_eq!(scan.skipped, 1);
        assert_eq!(scan.hits, 4);
    }

    #[test]
    fn assessment_reads_named_acl_references_on_real_files() {
        let tree = tempfile::tempdir().unwrap();
        let path = tree.path().join("acl");
        fs::write(&path, b"content").unwrap();
        let uid = fs::metadata(&path).unwrap().uid().wrapping_add(1234);
        let mut data = 2u32.to_le_bytes().to_vec();
        for (tag, id) in [
            (1u16, u32::MAX),
            (2, uid),
            (4, u32::MAX),
            (16, u32::MAX),
            (32, u32::MAX),
        ] {
            data.extend(tag.to_le_bytes());
            data.extend(4u16.to_le_bytes());
            data.extend(id.to_le_bytes());
        }
        let native = CString::new(path.as_os_str().as_bytes()).unwrap();
        let result = unsafe {
            libc::lsetxattr(
                native.as_ptr(),
                c"system.posix_acl_access".as_ptr(),
                data.as_ptr().cast(),
                data.len(),
                0,
            )
        };
        if result < 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::EOPNOTSUPP) {
            return;
        }
        assert_eq!(result, 0, "{}", std::io::Error::last_os_error());
        assert!(
            acl_ids(&path, c"system.posix_acl_access")
                .unwrap()
                .contains(&(true, uid))
        );
        let ids = BTreeSet::from([(true, uid)]);
        let mut scan = Scan {
            backend: &BoundaryBackend,
            ids: &ids,
            seen: HashSet::new(),
            hits: 0,
            examples: Vec::new(),
            errors: Vec::new(),
            skipped: 0,
        };
        scan.visit(
            tree.path(),
            fs::metadata(tree.path()).unwrap().dev(),
            &None,
            &mut HashMap::new(),
        );
        assert_eq!(scan.hits, 1);
        assert_eq!(scan.examples, [path]);
    }
    #[test]
    fn reads_named_acl_users_and_groups_only() {
        let mut data = 2u32.to_le_bytes().to_vec();
        for (tag, id) in [(1u16, u32::MAX), (2, 1234), (8, 5678)] {
            data.extend(tag.to_le_bytes());
            data.extend(7u16.to_le_bytes());
            data.extend(id.to_le_bytes());
        }
        assert_eq!(decode_acl(&data).unwrap(), [(true, 1234), (false, 5678)]);
        assert!(decode_acl(&data[..data.len() - 1]).is_err());
    }
}
