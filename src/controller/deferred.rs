//! Durable first-boot inheritance, independent of mutable build caches and plugins.
use super::{BuildBackend, BuildContext, Error, Result, fs as disk};
use crate::config::{InheritanceConfig, NocowHandling, validate_inheritance_type};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Component, Path, PathBuf},
};

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PendingInheritance {
    pub schema_version: u32,
    pub system_id: String,
    pub version: String,
    pub filesystem_uuid: String,
    pub prepared_root: PathBuf,
    pub prepared_subvolume_id: String,
    pub final_root: PathBuf,
    pub directory_subvolume: bool,
    #[serde(default)]
    pub nocow_handling: NocowHandling,
    pub sources: Vec<InheritanceSource>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InheritanceSource {
    pub path: PathBuf,
    pub target: PathBuf,
    pub subvolume_id: String,
    pub directory: bool,
}

pub(super) fn prepare(
    backend: &impl BuildBackend,
    context: &BuildContext,
    config: &InheritanceConfig,
    prepared: &Path,
) -> Result<()> {
    let (uuid, prepared_root) = backend.filesystem_location(prepared)?;
    let directory = prepared
        .parent()
        .ok_or_else(|| Error::Invalid("准备根没有父目录".into()))?;
    let (_, final_root) = backend.filesystem_location(directory)?;
    let manager = directory
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .and_then(Path::parent)
        .ok_or_else(|| Error::Invalid("准备版本缺少管理根".into()))?;
    let _pins_lock = super::records::lock_inheritance(manager)?;
    let mut sources = Vec::new();
    for source in &config.copy_on_write_boot {
        let unmarked = Path::new(source.to_str().unwrap().trim_end_matches('/'));
        let physical = fs::canonicalize(unmarked).map_err(|e| disk::io(source, e))?;
        let info = fs::symlink_metadata(&physical).map_err(|e| disk::io(&physical, e))?;
        validate_inheritance_type(source, info.is_dir())?;
        let mut paths = vec![physical.clone()];
        paths.extend(super::mounts::descendant_mounts(&physical)?);
        paths.sort_by_key(|p| (p.components().count(), p.clone()));
        paths.dedup();
        for path in paths {
            disk::checked(&path)?;
            let info = fs::symlink_metadata(&path).map_err(|e| disk::io(&path, e))?;
            if !info.is_file() && !info.is_dir() {
                return Err(Error::Invalid(format!(
                    "继承来源必须为普通文件或目录: {}",
                    path.display()
                )));
            }
            let (source_uuid, location) = backend.filesystem_location(&path)?;
            if uuid != source_uuid {
                return Err(Error::Invalid(format!(
                    "CoW 继承必须位于同一 Btrfs 文件系统: {}",
                    path.display()
                )));
            }
            if location.starts_with(&final_root) || final_root.starts_with(&location) {
                return Err(Error::Invalid("继承来源与候选版本存储重叠".into()));
            }
            let mut target = {
                let relative = path
                    .strip_prefix(&physical)
                    .map_err(|e| Error::Invalid(e.to_string()))?;
                if relative.as_os_str().is_empty() {
                    source.clone()
                } else {
                    source.join(relative)
                }
            };
            // Generated mappings for descendant mounts carry the same type syntax.
            if info.is_dir() {
                target.push("");
            }
            sources.push(InheritanceSource {
                path: location,
                target,
                subvolume_id: backend.source_subvolume_id(&path)?,
                directory: info.is_dir(),
            });
        }
    }
    let manifest = PendingInheritance {
        schema_version: 1,
        system_id: context.system_id.clone(),
        version: directory
            .file_name()
            .and_then(|s| s.to_str())
            .ok_or_else(|| Error::Invalid("版本号无效".into()))?
            .into(),
        filesystem_uuid: uuid,
        prepared_subvolume_id: backend.source_subvolume_id(prepared)?,
        prepared_root,
        final_root: final_root.join("root"),
        directory_subvolume: config.directory_subvolume,
        nocow_handling: config.nocow_handling,
        sources,
    };
    disk::write(&directory.join("inheritance-pending.toml"), &manifest)
}

fn top_path(top: &Path, location: &Path) -> Result<PathBuf> {
    if !location.is_absolute()
        || location
            .components()
            .any(|p| !matches!(p, Component::RootDir | Component::Normal(_)))
    {
        return Err(Error::Invalid("继承清单中的文件系统路径无效".into()));
    }
    let path = top.join(location.strip_prefix("/").unwrap());
    disk::checked(&path)?;
    Ok(path)
}

fn read_manifest(path: &Path) -> Result<PendingInheritance> {
    disk::checked(path)?;
    if !fs::symlink_metadata(path)
        .map_err(|e| disk::io(path, e))?
        .is_file()
    {
        return Err(Error::Invalid("待继承清单必须为普通文件".into()));
    }
    let text = fs::read_to_string(path).map_err(|e| disk::io(path, e))?;
    let manifest: PendingInheritance =
        toml::from_str(&text).map_err(|e| Error::Invalid(e.to_string()))?;
    if manifest.schema_version != 1 || manifest.sources.is_empty() {
        return Err(Error::Invalid("待继承清单格式或来源无效".into()));
    }
    if manifest.nocow_handling == NocowHandling::MoveSnapshot && !manifest.directory_subvolume {
        return Err(Error::Invalid(
            "move-snapshot 清单要求 DirectorySubvolume=yes".into(),
        ));
    }
    crate::config::validate_identifier(&manifest.system_id)?;
    if !manifest.version.starts_with('v')
        || manifest.version.len() < 2
        || !manifest.version[1..].bytes().all(|b| b.is_ascii_digit())
    {
        return Err(Error::Invalid("待继承版本号无效".into()));
    }
    Ok(manifest)
}

/// Complete exactly this selected version before its final root is mounted.
pub fn finalize_inheritance(manifest: &Path, filesystem_root: &Path) -> Result<()> {
    finalize_inheritance_with_backend(manifest, filesystem_root, &super::SystemBackend)
}

pub fn finalize_inheritance_with_backend(
    manifest_path: &Path,
    filesystem_root: &Path,
    backend: &impl BuildBackend,
) -> Result<()> {
    disk::checked(manifest_path)?;
    let system = manifest_path
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .ok_or_else(|| Error::Invalid("待继承清单缺少系统目录".into()))?;
    let _lock = super::records::lock_directory(system)?;
    finalize_inheritance_locked(manifest_path, filesystem_root, backend)
}

/// Complete a version while the caller already owns its shared system lock.
/// 调用者已持有共用系统锁时完成版本。
/// # Arguments
/// - `manifest_path`: Exact pending manifest. 精确待办清单。
/// - `filesystem_root`: Btrfs subvolid=5 view. Btrfs 顶层视图。
/// - `backend`: Validated storage backend. 经校验存储后端。
/// # Errors
/// Returns identity, source or publication failures. 返回身份、来源或发布错误。
pub fn finalize_inheritance_locked(
    manifest_path: &Path,
    filesystem_root: &Path,
    backend: &impl BuildBackend,
) -> Result<()> {
    let manifest = read_manifest(manifest_path)?;
    super::progress::message(
        &manifest.system_id,
        "校验启动阶段继承任务和来源",
        "Validate the boot-time inheritance task and sources",
    );
    disk::checked(filesystem_root)?;
    let (uuid, _) = backend.filesystem_location(filesystem_root)?;
    if uuid != manifest.filesystem_uuid {
        return Err(Error::Invalid(
            "待继承清单与挂载文件系统 UUID 不匹配".into(),
        ));
    }
    let prepared = top_path(filesystem_root, &manifest.prepared_root)?;
    let final_root = top_path(filesystem_root, &manifest.final_root)?;
    let directory = manifest_path
        .parent()
        .ok_or_else(|| Error::Invalid("清单缺少父目录".into()))?;
    if prepared != directory.join("prepared-root")
        || final_root != directory.join("root")
        || directory.file_name().and_then(|p| p.to_str()) != Some(&manifest.version)
    {
        return Err(Error::Invalid(
            "清单必须与其准备根及最终根位于同一版本目录".into(),
        ));
    }
    let system = directory
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| Error::Invalid("待继承版本缺少系统目录".into()))?;
    if system.file_name().and_then(|s| s.to_str()) != Some(&manifest.system_id) {
        return Err(Error::Invalid("清单系统身份不匹配".into()));
    }
    let manager = system
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| Error::Invalid("继承系统缺少管理根".into()))?;
    let _pins_lock = super::records::lock_inheritance(manager)?;
    let state_path = directory.join("state.toml");
    let state_text = fs::read_to_string(&state_path).map_err(|e| disk::io(&state_path, e))?;
    let state: toml::Value =
        toml::from_str(&state_text).map_err(|e| Error::Invalid(e.to_string()))?;
    if state.get("state").and_then(|v| v.as_str()) == Some("cancelled") {
        return Err(Error::Invalid("此版本的继承任务已取消".into()));
    }
    let digest = format!(
        "{:x}",
        Sha256::digest(fs::read(manifest_path).map_err(|e| disk::io(manifest_path, e))?)
    );
    if disk::exists(&final_root)? {
        verify_commit(
            backend,
            directory,
            &final_root,
            &digest,
            state.get("state").and_then(|v| v.as_str()) != Some("complete"),
        )?;
        mark_complete(directory)?;
        super::progress::message(
            &manifest.system_id,
            "启动阶段继承已完成，复用最终系统根",
            "Boot-time inheritance already complete; reuse the final system root",
        );
        return Ok(());
    }
    backend.check_subvolume(&prepared, true)?;
    if backend.source_subvolume_id(&prepared)? != manifest.prepared_subvolume_id {
        return Err(Error::Invalid("准备根子卷身份已改变".into()));
    }
    let staging = directory.join("inheritance-staging");
    if disk::exists(&staging)? {
        backend.delete_subvolume_recursive(&staging)?;
    }
    // Validate every source before creating or modifying a destination.
    let mut sources = Vec::new();
    for source in &manifest.sources {
        crate::config::inheritance_name(&source.target)?;
        validate_inheritance_type(&source.target, source.directory)?;
        let path = top_path(filesystem_root, &source.path)?;
        if path.starts_with(directory) || directory.starts_with(&path) {
            return Err(Error::Invalid("继承来源与候选版本存储重叠".into()));
        }
        let info = fs::symlink_metadata(&path).map_err(|e| disk::io(&path, e))?;
        if info.is_dir() != source.directory
            || (!info.is_dir() && !info.is_file())
            || !(if manifest.nocow_handling == NocowHandling::MoveSnapshot && source.directory {
                super::inheritance_migration::validates_identity(
                    backend,
                    &path,
                    manager,
                    &source.subvolume_id,
                    &digest,
                )?
            } else {
                backend.source_subvolume_id(&path)? == source.subvolume_id
            })
        {
            return Err(Error::Invalid(format!(
                "来源类型或子卷身份已改变: {}",
                source.target.display()
            )));
        }
        backend.check_cow_source(&path, directory)?;
        sources.push((source, path));
    }
    if manifest.nocow_handling == NocowHandling::MoveSnapshot {
        for (source, path) in &sources {
            if source.directory {
                super::inheritance_migration::prepare(
                    backend,
                    path,
                    manager,
                    &digest,
                    Some(manifest_path),
                )?;
            }
        }
    }
    backend.snapshot_recursive(&prepared, &staging, false)?;
    let result = super::progress::run(
        &manifest.system_id,
        "应用启动阶段继承并发布可写系统根",
        "Apply boot-time inheritance and publish the writable system root",
        || {
            for (source, path) in sources {
                super::progress::message(
                    &manifest.system_id,
                    format!("继承路径：{}", source.target.display()),
                    format!("Inherit path: {}", source.target.display()),
                );
                let target = disk::guest_path(&staging, &source.target)?;
                super::inheritance::inherit_path(
                    backend,
                    &path,
                    &target,
                    manifest.directory_subvolume,
                    manifest.nocow_handling,
                )?;
            }
            let mut identities = Vec::new();
            for child in backend.nested_subvolumes(&staging)? {
                identities.push((
                    child.clone(),
                    backend.source_subvolume_id(&staging.join(child))?,
                ));
            }
            let receipt = CommitReceipt {
                manifest_sha256: digest.clone(),
                root_subvolume_id: backend.source_subvolume_id(&staging)?,
                children: identities,
            };
            disk::write(&directory.join("inheritance-commit.toml"), &receipt)?;
            backend.sync_filesystem(&staging)?;
            fs::rename(&staging, &final_root).map_err(|e| disk::io(&final_root, e))?;
            disk::sync(directory)?;
            backend.sync_filesystem(directory)?;
            mark_complete(directory)
        },
    );
    if let Err(error) = &result {
        disk::write_text(&directory.join("inheritance-error.txt"), &error.to_string())?;
    }
    result
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CommitReceipt {
    manifest_sha256: String,
    root_subvolume_id: String,
    children: Vec<(PathBuf, String)>,
}

fn verify_commit(
    backend: &impl BuildBackend,
    directory: &Path,
    root: &Path,
    digest: &str,
    verify_children: bool,
) -> Result<()> {
    backend.check_subvolume(root, false)?;
    let path = directory.join("inheritance-commit.toml");
    let text = fs::read_to_string(&path).map_err(|e| disk::io(&path, e))?;
    let receipt: CommitReceipt =
        toml::from_str(&text).map_err(|e| Error::Invalid(e.to_string()))?;
    if receipt.manifest_sha256 != digest
        || receipt.root_subvolume_id != backend.source_subvolume_id(root)?
    {
        return Err(Error::Invalid(
            "最终根与继承提交记录不匹配，拒绝继续启动".into(),
        ));
    }
    for (child, _) in &receipt.children {
        if child.as_os_str().is_empty()
            || child
                .components()
                .any(|p| !matches!(p, Component::Normal(_)))
        {
            return Err(Error::Invalid("提交记录子卷路径无效".into()));
        }
    }
    // Completed roots can change during normal use; recovery still verifies the full published tree.
    // 已完成的根允许运行时修改；中断恢复仍核验完整发布子卷树。
    if !verify_children {
        return Ok(());
    }
    let mut actual = backend.nested_subvolumes(root)?;
    actual.sort();
    let mut expected: Vec<_> = receipt.children.iter().map(|(p, _)| p.clone()).collect();
    expected.sort();
    if actual != expected {
        return Err(Error::Invalid(
            "最终根与继承提交记录不匹配，拒绝继续启动".into(),
        ));
    }
    for (child, id) in receipt.children {
        backend.check_subvolume(&root.join(&child), false)?;
        if backend.source_subvolume_id(&root.join(child))? != id {
            return Err(Error::Invalid("最终根内部子卷身份已改变".into()));
        }
    }
    Ok(())
}

fn mark_complete(directory: &Path) -> Result<()> {
    let state = directory.join("state.toml");
    let text = fs::read_to_string(&state).map_err(|e| disk::io(&state, e))?;
    let mut value: toml::Value =
        toml::from_str(&text).map_err(|e| Error::Invalid(e.to_string()))?;
    value["state"] = toml::Value::String("complete".into());
    if let Some(table) = value.as_table_mut() {
        table.remove("error");
    }
    disk::write(&state, &value)?;
    let error = directory.join("inheritance-error.txt");
    if disk::exists(&error)? {
        fs::remove_file(&error).map_err(|e| disk::io(&error, e))?;
        disk::sync(directory)?;
    }
    Ok(())
}

/// Pending tasks retain their source locations until completion or explicit cancellation.
pub(super) fn pinned_sources_one(directory: &Path) -> Result<Vec<PathBuf>> {
    let state = directory.join("state.toml");
    let text = fs::read_to_string(&state).map_err(|e| disk::io(&state, e))?;
    let value: toml::Value = toml::from_str(&text).map_err(|e| Error::Invalid(e.to_string()))?;
    if matches!(
        value.get("state").and_then(|v| v.as_str()),
        Some("complete" | "cancelled")
    ) {
        return Ok(Vec::new());
    }
    Ok(read_manifest(&directory.join("inheritance-pending.toml"))?
        .sources
        .into_iter()
        .map(|s| s.path)
        .collect())
}

pub(super) fn cancel(directory: &Path) -> Result<()> {
    if !disk::exists(&directory.join("inheritance-pending.toml"))? {
        return Ok(());
    }
    let path = directory.join("state.toml");
    let text = fs::read_to_string(&path).map_err(|e| disk::io(&path, e))?;
    let mut state: toml::Value =
        toml::from_str(&text).map_err(|e| Error::Invalid(e.to_string()))?;
    if state.get("state").and_then(|v| v.as_str()) != Some("complete") {
        state["state"] = toml::Value::String("cancelled".into());
        disk::write(&path, &state)?;
    }
    Ok(())
}
