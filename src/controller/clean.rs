//! Delete bounded version history while preserving runtime data and monotonic numbering.
//! 删除限定版本历史，保留运行数据和单调版本编号。
use super::{
    BuildBackend, Error, Result, SystemBackend, boot::BootRecord, cache, fs as disk, records,
};
use crate::config::ConfigStore;
use std::{collections::BTreeSet, fs, path::Path};

/// Parse a numeric version without lexicographic ordering assumptions.
/// 解析数字版本，不依赖字典序。
/// # Arguments
/// - `version`: Stored version name. 已存版本名。
fn number(version: &str) -> Result<u64> {
    let suffix = version
        .strip_prefix('v')
        .filter(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
        .ok_or_else(|| Error::Invalid(format!("无效版本: {version}")))?;
    suffix
        .parse()
        .map_err(|e| Error::Invalid(format!("无效版本 {version}: {e}")))
}

/// Clean one system through an inclusive version cutoff using native Btrfs operations.
/// 使用原生 Btrfs 操作清理一个系统直到包含指定截止版本。
/// # Arguments
/// - `store`: Manager root. 管理根。
/// - `system`: Managed system ID. 被管理系统标识。
/// - `version`: Inclusive cutoff. 包含在内的截止版本。
/// # Errors
/// Returns lock, ownership, running-version or deletion failures.
/// 返回锁、所有权、运行版本或删除错误。
pub fn clean(store: &ConfigStore, system: &str, version: &str) -> Result<Vec<String>> {
    clean_with_backend(store, system, version, &SystemBackend)
}

/// Preflight every target before removing entries, caches and version roots.
/// 删除启动项、缓存和版本根前预检查全部目标。
/// # Arguments
/// - `store`: Initialized manager. 已初始化管理器。
/// - `system`: System ID. 系统标识。
/// - `version`: Inclusive cutoff. 包含在内的截止版本。
/// - `backend`: Checked storage/runtime backend. 经检查的存储和运行后端。
/// # Errors
/// Refuses mounted/running roots and malformed ownership records.
/// 拒绝运行根及损坏的所有权记录。
pub fn clean_with_backend(
    store: &ConfigStore,
    system: &str,
    version: &str,
    backend: &impl BuildBackend,
) -> Result<Vec<String>> {
    let cutoff = number(version)?;
    let layout = store.system_layout(system)?;
    let _lock = records::lock_system(&layout)?;
    let _pins_lock = records::lock_inheritance(store.root())?;
    let selected = layout.versions.join(version);
    disk::checked(&selected)?;
    if !selected.is_dir() {
        return Err(Error::Invalid(format!("截止版本不存在: {version}")));
    }
    let mut targets = Vec::new();
    let mut highest = 0;
    for entry in fs::read_dir(&layout.versions).map_err(|e| disk::io(&layout.versions, e))? {
        let entry = entry.map_err(|e| disk::io(&layout.versions, e))?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| Error::Invalid("版本名不是 UTF-8".into()))?;
        let value = number(&name)?;
        highest = highest.max(value);
        if value <= cutoff {
            disk::checked(&entry.path())?;
            if !entry
                .file_type()
                .map_err(|e| disk::io(&entry.path(), e))?
                .is_dir()
            {
                return Err(Error::Invalid("版本记录必须为普通目录".into()));
            }
            targets.push((value, name, entry.path()));
        }
    }
    targets.sort_by_key(|(value, _, _)| *value);
    super::current::check_retained(
        store,
        system,
        &targets
            .iter()
            .map(|(_, name, _)| name.clone())
            .collect::<Vec<_>>(),
    )?;
    // Any retained pending version pins the source versions it will consume at boot.
    let mut pins = Vec::new();
    for id in store.list_systems()? {
        let other = store.system_layout(&id)?;
        for entry in fs::read_dir(&other.versions).map_err(|e| disk::io(&other.versions, e))? {
            let directory = entry.map_err(|e| disk::io(&other.versions, e))?.path();
            if targets.iter().any(|(_, _, target)| target == &directory) {
                continue;
            }
            let manifest = directory.join("inheritance-pending.toml");
            if disk::exists(&manifest)? {
                pins.extend(super::deferred::pinned_sources_one(&directory)?);
            }
        }
    }
    for (_, _, directory) in &targets {
        let (_, location) = backend.filesystem_location(directory)?;
        if pins.iter().any(|source| source.starts_with(&location)) {
            return Err(Error::Invalid(
                "版本被尚未完成的 initramfs 继承引用，不能清理".into(),
            ));
        }
        for name in ["prepared-root", "inheritance-staging"] {
            let root = directory.join(name);
            if disk::exists(&root)? {
                super::mounts::check_unmounted_tree(&root)?;
                if backend.is_running_root(&root)? {
                    return Err(Error::Invalid("不能清理运行中的准备根".into()));
                }
            }
        }
    }
    let mut boots = Vec::new();
    for (_, name, directory) in &targets {
        super::mounts::check_unmounted_tree(directory)?;
        let root = directory.join("root");
        if disk::exists(&root)? {
            backend
                .check_subvolume(&root, true)
                .or_else(|_| backend.check_subvolume(&root, false))?;
            if backend.is_running_root(&root)? {
                return Err(Error::Invalid(format!(
                    "不能清理正在运行的版本: {system}/{name}"
                )));
            }
        }
        let cache = layout.cache.join(name);
        disk::checked(&cache)?;
        super::mounts::check_unmounted_tree(&cache)?;
        for stage in cache::STAGES {
            let path = cache.join(stage);
            if disk::exists(&path)? {
                backend
                    .check_subvolume(&path, true)
                    .or_else(|_| backend.check_subvolume(&path, false))?;
            }
        }
        let record = directory.join("boot.toml");
        if disk::exists(&record)? {
            let text = fs::read_to_string(&record).map_err(|e| disk::io(&record, e))?;
            let boot: BootRecord =
                toml::from_str(&text).map_err(|e| Error::Invalid(e.to_string()))?;
            validate_boot(&boot, system, name)?;
            boots.push(boot);
        }
    }
    // Preserve numbering before deleting the highest attempt, including all-history cleanup.
    // 删除最高尝试前保留编号，包括清空全部历史的情况。
    let counter = layout.root.join("version-counter");
    if disk::exists(&counter)? {
        let saved = fs::read_to_string(&counter)
            .map_err(|e| disk::io(&counter, e))?
            .trim()
            .parse::<u64>()
            .map_err(|e| Error::Invalid(e.to_string()))?;
        highest = highest.max(saved);
    }
    disk::write_text(&counter, &highest.to_string())?;
    for (_, _, directory) in &targets {
        super::deferred::cancel(directory)?;
    }
    for boot in boots {
        if disk::exists(&boot.entry)? {
            fs::remove_file(&boot.entry).map_err(|e| disk::io(&boot.entry, e))?;
            disk::sync(boot.entry.parent().unwrap())?;
        }
        if disk::exists(&boot.directory)? {
            fs::remove_dir_all(&boot.directory).map_err(|e| disk::io(&boot.directory, e))?;
            disk::sync(boot.directory.parent().unwrap())?;
        }
    }
    let mut removed = Vec::new();
    for (_, name, directory) in targets {
        let root = directory.join("root");
        if disk::exists(&root)? {
            backend.delete_subvolume_recursive(&root)?;
        }
        for extra in ["prepared-root", "inheritance-staging"] {
            let path = directory.join(extra);
            if disk::exists(&path)? {
                backend.delete_subvolume_recursive(&path)?;
            }
        }
        let cache = layout.cache.join(&name);
        if disk::exists(&cache)? {
            for stage in cache::STAGES.iter().rev() {
                let path = cache.join(stage);
                if disk::exists(&path)? {
                    backend.delete_subvolume_recursive(&path)?;
                }
            }
            fs::remove_dir_all(&cache).map_err(|e| disk::io(&cache, e))?;
        }
        fs::remove_dir_all(&directory).map_err(|e| disk::io(&directory, e))?;
        removed.push(name);
    }
    disk::sync(&layout.versions)?;
    Ok(removed)
}

/// Require exact generated filenames beneath one boot root and regular owned artifacts.
/// 要求同一启动根下准确的生成文件名及普通所属产物。
/// # Arguments
/// - `record`: Version ownership record. 版本所有权记录。
/// - `system`: Expected system ID. 预期系统标识。
/// - `version`: Expected version. 预期版本。
fn validate_boot(record: &BootRecord, system: &str, version: &str) -> Result<()> {
    disk::checked(&record.directory)?;
    disk::checked(&record.entry)?;
    super::mounts::check_unmounted_tree(&record.directory)?;
    super::mounts::check_unmounted_tree(&record.entry)?;
    let boot_root = record
        .directory
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| Error::Invalid("无效启动所有权目录".into()))?;
    let expected_directory = boot_root.join("RStarSleep").join(version);
    let expected_entry = boot_root
        .join("loader/entries")
        .join(format!("RStarSleep-{system}-{version}.conf"));
    if record.system_id != system
        || record.version != version
        || record.directory != expected_directory
        || record.entry != expected_entry
    {
        return Err(Error::Invalid("启动所有权记录不匹配版本".into()));
    }
    let owned: BTreeSet<_> = match &record.files {
        Some(files) => {
            let mut names = BTreeSet::new();
            for name in files {
                if !super::boot::artifact_name(name) || !names.insert(name.to_ascii_lowercase()) {
                    return Err(Error::Invalid("启动所有权文件名无效或重复".into()));
                }
            }
            if names.is_empty() {
                return Err(Error::Invalid("启动所有权文件列表不能为空".into()));
            }
            // Preserve exact case when comparing actual directory entries.
            files.iter().map(String::as_str).collect()
        }
        // Compatibility for versions published before declarations were retained.
        None => [
            "vmlinuz",
            "initramfs-linux.img",
            "intel-ucode.img",
            "amd-ucode.img",
        ]
        .into_iter()
        .collect(),
    };
    if disk::exists(&record.directory)? {
        for entry in fs::read_dir(&record.directory).map_err(|e| disk::io(&record.directory, e))? {
            let entry = entry.map_err(|e| disk::io(&record.directory, e))?;
            if !owned.contains(entry.file_name().to_str().unwrap_or(""))
                || !entry
                    .file_type()
                    .map_err(|e| disk::io(&entry.path(), e))?
                    .is_file()
            {
                return Err(Error::Invalid("启动目录含未知文件或非普通文件".into()));
            }
        }
    }
    if disk::exists(&record.entry)?
        && !fs::symlink_metadata(&record.entry)
            .map_err(|e| disk::io(&record.entry, e))?
            .is_file()
    {
        return Err(Error::Invalid("启动项必须为普通文件".into()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(root: &Path, files: Option<Vec<String>>) -> BootRecord {
        BootRecord {
            system_id: "desktop".into(),
            version: "v00000001".into(),
            directory: root.join("RStarSleep/v00000001"),
            entry: root.join("loader/entries/RStarSleep-desktop-v00000001.conf"),
            files,
        }
    }

    #[test]
    fn cleanup_uses_retained_artifact_ownership_for_custom_distributions() {
        let temporary = tempfile::tempdir().unwrap();
        let boot = record(
            temporary.path(),
            Some(vec!["custom-kernel".into(), "custom-initrd".into()]),
        );
        fs::create_dir_all(&boot.directory).unwrap();
        fs::write(boot.directory.join("custom-kernel"), "kernel").unwrap();
        fs::write(boot.directory.join("custom-initrd"), "initrd").unwrap();
        validate_boot(&boot, "desktop", "v00000001").unwrap();
        fs::write(boot.directory.join("unowned"), "keep").unwrap();
        assert!(validate_boot(&boot, "desktop", "v00000001").is_err());
        fs::remove_file(boot.directory.join("unowned")).unwrap();
        fs::remove_file(boot.directory.join("custom-initrd")).unwrap();
        std::os::unix::fs::symlink("custom-kernel", boot.directory.join("custom-initrd")).unwrap();
        assert!(validate_boot(&boot, "desktop", "v00000001").is_err());
    }

    #[test]
    fn cleanup_rejects_unsafe_owned_names_and_preserves_legacy_records() {
        let temporary = tempfile::tempdir().unwrap();
        for names in [
            vec!["../outside"],
            vec!["kernel", "KERNEL"],
            vec!["kernel."],
            vec![],
        ] {
            let boot = record(
                temporary.path(),
                Some(names.into_iter().map(str::to_string).collect()),
            );
            assert!(validate_boot(&boot, "desktop", "v00000001").is_err());
        }
        let boot = record(temporary.path(), None);
        fs::create_dir_all(&boot.directory).unwrap();
        fs::write(boot.directory.join("vmlinuz"), "kernel").unwrap();
        validate_boot(&boot, "desktop", "v00000001").unwrap();
        fs::write(boot.directory.join("custom-kernel"), "kernel").unwrap();
        assert!(validate_boot(&boot, "desktop", "v00000001").is_err());
    }
}
