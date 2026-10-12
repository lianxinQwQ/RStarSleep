//! Validate global volume mappings and prepare equivalent host workspaces.
use super::{Error, Result, identifier, io_error};
use crate::controller::PersistentMount;
use std::{
    collections::HashSet,
    fs, io,
    os::unix::fs::{PermissionsExt, symlink},
    path::{Component, Path, PathBuf},
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PersistentDirectory {
    pub name: String,
    pub target: PathBuf,
}

/// Visible plugin resources owned by a private temporary directory.
/// 由私有临时目录持有的可见插件资源。
pub struct Workspace {
    root: PathBuf,
    _temporary: tempfile::TempDir,
}

impl Workspace {
    pub fn path(&self) -> &Path {
        &self.root
    }
}

fn target(value: &Path) -> Result<PathBuf> {
    if value.as_os_str().is_empty()
        || value
            .as_os_str()
            .as_encoded_bytes()
            .iter()
            .any(|b| b"\0:\\".contains(b))
        || value
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
    {
        return Err(Error::Invalid(format!(
            "持久目录挂载点必须为相对子目录: {}",
            value.display()
        )));
    }
    Ok(value.components().collect())
}
fn validate_target(root: &Path, target: &Path) -> Result<()> {
    let mut current = root.to_path_buf();
    for part in target.components() {
        current.push(part);
        match fs::symlink_metadata(&current) {
            Ok(info) if !info.is_dir() || info.file_type().is_symlink() => {
                return Err(Error::Invalid(format!(
                    "挂载点不能覆盖资源或经过链接: {}",
                    current.display()
                )));
            }
            Ok(_) => (),
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(io_error(&current, e)),
        }
    }
    if fs::read_dir(&current)
        .map_err(|e| io_error(&current, e))?
        .next()
        .transpose()
        .map_err(|e| io_error(&current, e))?
        .is_some()
    {
        return Err(Error::Invalid(format!(
            "挂载点会遮盖非空插件目录: {}",
            current.display()
        )));
    }
    Ok(())
}
pub fn parse(mappings: &[String], root: &Path) -> Result<Vec<PersistentDirectory>> {
    let mut names = HashSet::new();
    let mut parsed: Vec<PersistentDirectory> = Vec::new();
    for mapping in mappings {
        let (name, destination) = mapping.split_once(':').unwrap_or((mapping, mapping));
        identifier(name)?;
        let destination = target(Path::new(destination))?;
        if !names.insert(name)
            || parsed
                .iter()
                .any(|p| p.target.starts_with(&destination) || destination.starts_with(&p.target))
        {
            return Err(Error::Invalid(format!(
                "持久目录重复或挂载点重叠: {mapping}"
            )));
        }
        validate_target(root, &destination)?;
        parsed.push(PersistentDirectory {
            name: name.into(),
            target: destination,
        });
    }
    Ok(parsed)
}
fn copy(source: &Path, dest: &Path) -> Result<()> {
    for entry in fs::read_dir(source).map_err(|e| io_error(source, e))? {
        let entry = entry.map_err(|e| io_error(source, e))?;
        let s = entry.path();
        let d = dest.join(entry.file_name());
        let info = fs::symlink_metadata(&s).map_err(|e| io_error(&s, e))?;
        if info.file_type().is_symlink() {
            symlink(fs::read_link(&s).map_err(|e| io_error(&s, e))?, &d)
                .map_err(|e| io_error(&d, e))?;
        } else if info.is_dir() {
            fs::create_dir(&d).map_err(|e| io_error(&d, e))?;
            copy(&s, &d)?;
        } else if info.is_file() {
            fs::copy(&s, &d).map_err(|e| io_error(&d, e))?;
            fs::set_permissions(&d, info.permissions()).map_err(|e| io_error(&d, e))?;
        } else {
            return Err(Error::Invalid("插件资源不能包含特殊文件".into()));
        }
    }
    Ok(())
}
pub fn host_workspace(root: &Path, mounts: &[PersistentMount]) -> Result<Workspace> {
    let work = tempfile::Builder::new()
        .prefix("plugin-host-")
        .permissions(fs::Permissions::from_mode(0o700))
        .tempdir()
        .map_err(|e| io_error(std::env::temp_dir(), e))?;
    copy(root, work.path())?;
    let mut seen = Vec::new();
    for mount in mounts {
        let t = target(&mount.target)?;
        if seen
            .iter()
            .any(|p: &PathBuf| p.starts_with(&t) || t.starts_with(p))
        {
            return Err(Error::Invalid("挂载点重叠".into()));
        }
        seen.push(t.clone());
        validate_target(work.path(), &t)?;
        crate::controller::fs::checked(&mount.source).map_err(|e| Error::Invalid(e.to_string()))?;
        let dest = work.path().join(t);
        fs::create_dir_all(dest.parent().unwrap()).map_err(|e| io_error(&dest, e))?;
        if dest.exists() {
            fs::remove_dir(&dest).map_err(|e| io_error(&dest, e))?;
        }
        symlink(&mount.source, &dest).map_err(|e| io_error(&dest, e))?;
    }
    Ok(Workspace {
        root: work.path().into(),
        _temporary: work,
    })
}
pub fn container_workspace(root: &Path, mounts: &[PersistentMount]) -> Result<Workspace> {
    let work = tempfile::Builder::new()
        .prefix("plugin-container-")
        .permissions(fs::Permissions::from_mode(0o700))
        .tempdir()
        .map_err(|e| io_error(std::env::temp_dir(), e))?;
    // The host parent stays private; only this child is bound into the container.
    // 宿主父目录保持私有；只将此子目录绑定到容器中。
    let visible = work.path().join("root");
    fs::create_dir(&visible).map_err(|e| io_error(&visible, e))?;
    copy(root, &visible)?;
    fs::set_permissions(&visible, fs::Permissions::from_mode(0o755))
        .map_err(|e| io_error(&visible, e))?;
    for mount in mounts {
        let t = target(&mount.target)?;
        validate_target(&visible, &t)?;
        let dest = visible.join(&t);
        fs::create_dir_all(&dest).map_err(|e| io_error(&dest, e))?;
        // Download/build users need to traverse mount ancestors even under umask 077.
        // 即使 umask 为 077，下载和构建用户也需要进入挂载路径的各级父目录。
        let mut current = visible.clone();
        for part in t.components() {
            current.push(part);
            fs::set_permissions(&current, fs::Permissions::from_mode(0o755))
                .map_err(|e| io_error(&current, e))?;
        }
    }
    Ok(Workspace {
        root: visible,
        _temporary: work,
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    fn mappings(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }
    #[test]
    fn mapping_boundaries() {
        let root = tempfile::tempdir().unwrap();
        let p = parse(&mappings(&["cache", "repo:state/repository"]), root.path()).unwrap();
        assert_eq!(p[0].target, Path::new("cache"));
        assert_eq!(p[1].target, Path::new("state/repository"));
        for v in [
            vec!["../cache"],
            vec!["cache:../out"],
            vec!["cache:/abs"],
            vec!["cache:"],
            vec!["cache:a", "repo:a/b"],
            vec!["cache:a", "cache:b"],
        ] {
            assert!(parse(&mappings(&v), root.path()).is_err());
        }
        fs::create_dir(root.path().join("occupied")).unwrap();
        fs::write(root.path().join("occupied/resource"), "x").unwrap();
        assert!(parse(&mappings(&["cache:occupied"]), root.path()).is_err());
        symlink("occupied", root.path().join("link")).unwrap();
        assert!(parse(&mappings(&["cache:link/sub"]), root.path()).is_err());
    }
    #[test]
    fn host_mount_does_not_destroy_source() {
        let root = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();
        fs::write(root.path().join("bootstrap.sh"), "exit 0").unwrap();
        fs::write(source.path().join("sentinel"), "data").unwrap();
        let work = host_workspace(
            root.path(),
            &[PersistentMount {
                source: source.path().into(),
                target: "state/cache".into(),
            }],
        )
        .unwrap();
        assert_eq!(
            fs::read_to_string(work.path().join("state/cache/sentinel")).unwrap(),
            "data"
        );
        drop(work);
        assert!(source.path().join("sentinel").exists());
    }

    #[test]
    fn container_resources_are_searchable_while_host_workspace_and_keys_stay_private() {
        use std::os::unix::fs::PermissionsExt;

        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("state")).unwrap();
        fs::set_permissions(root.path().join("state"), fs::Permissions::from_mode(0o700)).unwrap();
        let source = tempfile::tempdir().unwrap();
        let keys = source.path().join("gnupg");
        fs::create_dir(&keys).unwrap();
        fs::set_permissions(&keys, fs::Permissions::from_mode(0o700)).unwrap();
        let key = keys.join("private-key");
        fs::write(&key, "private").unwrap();
        fs::set_permissions(&key, fs::Permissions::from_mode(0o600)).unwrap();

        let work = container_workspace(
            root.path(),
            &[PersistentMount {
                source: source.path().into(),
                target: "state/sources".into(),
            }],
        )
        .unwrap();
        for path in [
            work.path().into(),
            work.path().join("state"),
            work.path().join("state/sources"),
        ] {
            assert_eq!(
                fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o755
            );
        }
        let private_parent = work.path().parent().unwrap().to_path_buf();
        assert_eq!(
            fs::metadata(&private_parent).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(&keys).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(&key).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(fs::read_to_string(&key).unwrap(), "private");
        drop(work);
        assert!(!private_parent.exists());
        assert!(key.is_file());
    }
}
