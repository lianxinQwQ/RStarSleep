//! Select each system's inheritance source from existing versions before any build work.
//! 在任何构建操作前，从已有版本中选择各系统的继承来源。
use super::super::{BuildBackend, Error, Result, fs as disk};
use std::{
    fs,
    path::{Path, PathBuf},
};

pub(in crate::controller) fn check_directory(root: &Path) -> Result<()> {
    disk::checked(root)?;
    if !fs::metadata(root).map_err(|e| disk::io(root, e))?.is_dir() {
        return Err(Error::Invalid(format!(
            "继承来源根必须为已有目录: {0} / Inheritance source root must be an existing directory: {0}",
            root.display()
        )));
    }
    Ok(())
}

fn read_state(directory: &Path, id: &str, version: &str) -> Result<toml::Value> {
    let state = directory.join("state.toml");
    disk::checked(&state)?;
    if !fs::symlink_metadata(&state)
        .map_err(|e| disk::io(&state, e))?
        .is_file()
    {
        return Err(Error::Invalid(format!(
            "继承来源版本状态必须为普通文件: {0} / Inheritance source version state must be a regular file: {0}",
            state.display()
        )));
    }
    let value: toml::Value = toml::from_str(
        &fs::read_to_string(&state).map_err(|e| disk::io(&state, e))?,
    )
    .map_err(|e| {
        Error::Invalid(format!(
            "继承来源版本状态无效: {0}: {e} / Invalid inheritance source version state: {0}: {e}",
            state.display()
        ))
    })?;
    if value.get("schema_version").and_then(|v| v.as_integer()) != Some(1)
        || value.get("system_id").and_then(|v| v.as_str()) != Some(id)
        || value.get("version").and_then(|v| v.as_str()) != Some(version)
    {
        return Err(Error::Invalid(format!(
            "继承来源版本状态身份不匹配: {id}/{version} / Inheritance source version state identity mismatch: {id}/{version}"
        )));
    }
    if !matches!(
        value.get("state").and_then(|v| v.as_str()),
        Some("building" | "failed" | "complete" | "pending-inheritance" | "cancelled")
    ) {
        return Err(Error::Invalid(format!(
            "继承来源版本状态无效: {id}/{version} / Invalid inheritance source version state: {id}/{version}"
        )));
    }
    Ok(value)
}

pub(in crate::controller) fn select(
    backend: &impl BuildBackend,
    id: &str,
    main: bool,
    versions: &Path,
    explicit: Option<&Path>,
) -> Result<Option<PathBuf>> {
    if let Some(root) = explicit {
        return Ok(Some(root.to_path_buf()));
    }
    let mut candidates = Vec::new();
    disk::checked(versions)?;
    for entry in fs::read_dir(versions).map_err(|e| disk::io(versions, e))? {
        let entry = entry.map_err(|e| disk::io(versions, e))?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let Some(number) = name
            .strip_prefix('v')
            .filter(|suffix| !suffix.is_empty() && suffix.bytes().all(|b| b.is_ascii_digit()))
        else {
            continue;
        };
        let number = number.parse::<u64>().map_err(|_| {
            Error::Invalid(format!(
                "版本序号溢出: {name} / Version sequence number overflow: {name}"
            ))
        })?;
        let directory = entry.path();
        check_directory(&directory)?;
        candidates.push((number, name.to_owned(), directory));
    }
    candidates.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.cmp(&a.1)));
    if main {
        for (_, version, directory) in &candidates {
            let root = directory.join("root");
            if disk::exists(&root)? {
                check_directory(&root)?;
                if backend.is_running_root(&root)? {
                    read_state(directory, id, version)?;
                    return Ok(Some(PathBuf::from("/")));
                }
            }
        }
    }
    for (_, version, directory) in candidates {
        let state = directory.join("state.toml");
        if !disk::exists(&state)? {
            continue;
        }
        let value = read_state(&directory, id, &version)?;
        if value.get("state").and_then(|v| v.as_str()) == Some("complete") {
            let root = directory.join("root");
            check_directory(&root)?;
            return Ok(Some(root));
        }
    }
    Ok(None)
}
