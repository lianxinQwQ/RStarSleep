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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_root: Option<InheritanceRoot>,
    pub sources: Vec<InheritanceSource>,
}

/// Fixed root view; configured paths are resolved below it only at first boot.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InheritanceRoot {
    pub filesystem_uuid: String,
    pub path: PathBuf,
    pub subvolume_id: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InheritanceSource {
    pub path: PathBuf,
    pub target: PathBuf,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub subvolume_id: String,
    pub directory: bool,
}

pub(super) fn prepare(
    backend: &impl BuildBackend,
    context: &BuildContext,
    config: &InheritanceConfig,
    prepared: &Path,
    source_root: Option<&InheritanceRoot>,
) -> Result<()> {
    let (uuid, prepared_root) = backend.filesystem_location(prepared)?;
    let directory = prepared.parent().ok_or_else(|| {
        Error::Invalid("准备根没有父目录 / Preparation root has no parent directory".into())
    })?;
    let (_, final_root) = backend.filesystem_location(directory)?;
    let manager = directory
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .and_then(Path::parent)
        .ok_or_else(|| {
            Error::Invalid("准备版本缺少管理根 / Prepared version has no management root".into())
        })?;
    let _pins_lock = super::records::lock_inheritance(manager)?;
    // Never inspect configured sources here: missing paths and today's mount overlays
    // do not describe the clean source tree that will be available at first boot.
    let sources = config
        .copy_on_write_boot
        .iter()
        .map(|source| InheritanceSource {
            path: source.clone(),
            target: source.clone(),
            subvolume_id: String::new(),
            directory: source.as_os_str().as_encoded_bytes().ends_with(b"/"),
        })
        .collect();
    let manifest = PendingInheritance {
        schema_version: 2,
        system_id: context.system_id.clone(),
        version: directory
            .file_name()
            .and_then(|s| s.to_str())
            .ok_or_else(|| Error::Invalid("版本号无效 / Invalid version number".into()))?
            .into(),
        filesystem_uuid: uuid,
        prepared_subvolume_id: backend.source_subvolume_id(prepared)?,
        prepared_root,
        final_root: final_root.join("root"),
        directory_subvolume: config.directory_subvolume,
        nocow_handling: config.nocow_handling,
        source_root: source_root.cloned(),
        sources,
    };
    disk::write(&directory.join("inheritance-pending.toml"), &manifest)
}

fn validate_top_location(location: &Path) -> Result<()> {
    if !location.is_absolute()
        || location
            .components()
            .any(|p| !matches!(p, Component::RootDir | Component::Normal(_)))
    {
        return Err(Error::Invalid(
            "继承清单中的文件系统路径无效 / Invalid filesystem path in inheritance manifest".into(),
        ));
    }
    Ok(())
}

fn top_path(top: &Path, location: &Path) -> Result<PathBuf> {
    validate_top_location(location)?;
    let path = top.join(location.strip_prefix("/").unwrap());
    disk::checked(&path)?;
    Ok(path)
}

pub(super) fn read_manifest(path: &Path) -> Result<PendingInheritance> {
    disk::checked(path)?;
    if !fs::symlink_metadata(path)
        .map_err(|e| disk::io(path, e))?
        .is_file()
    {
        return Err(Error::Invalid(
            "待继承清单必须为普通文件 / Pending inheritance manifest must be a regular file".into(),
        ));
    }
    let text = fs::read_to_string(path).map_err(|e| disk::io(path, e))?;
    let manifest: PendingInheritance =
        toml::from_str(&text).map_err(|e| Error::Invalid(e.to_string()))?;
    if !matches!(manifest.schema_version, 1 | 2) || manifest.sources.is_empty() {
        return Err(Error::Invalid(
            "待继承清单格式或来源无效 / Invalid pending inheritance manifest format or source"
                .into(),
        ));
    }
    if manifest.schema_version == 1 && manifest.source_root.is_some() {
        return Err(Error::Invalid(
            "旧继承清单不能指定来源根 / Legacy inheritance manifest cannot specify a source root"
                .into(),
        ));
    }
    if manifest.schema_version == 2 {
        if let Some(root) = &manifest.source_root {
            validate_top_location(&root.path)?;
            if root.filesystem_uuid.is_empty() || root.subvolume_id.is_empty() {
                return Err(Error::Invalid(
                    "来源根身份无效 / Invalid source root identity".into(),
                ));
            }
        }
        for source in &manifest.sources {
            crate::config::inheritance_name(&source.path)?;
            validate_inheritance_type(&source.target, source.directory)?;
            if source.path.as_os_str() != source.target.as_os_str()
                || !source.subvolume_id.is_empty()
            {
                return Err(Error::Invalid("启动清单必须保留配置原路径 / Boot manifest must retain the original configured paths".into()));
            }
        }
    }
    if manifest.nocow_handling == NocowHandling::MoveSnapshot && !manifest.directory_subvolume {
        return Err(Error::Invalid(
            "move-snapshot 清单要求 DirectorySubvolume=yes / move-snapshot manifest requires DirectorySubvolume=yes".into(),
        ));
    }
    crate::config::validate_identifier(&manifest.system_id)?;
    if !manifest.version.starts_with('v')
        || manifest.version.len() < 2
        || !manifest.version[1..].bytes().all(|b| b.is_ascii_digit())
    {
        return Err(Error::Invalid(
            "待继承版本号无效 / Invalid pending inheritance version number".into(),
        ));
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
    disk::checked(manifest_path.parent().ok_or_else(|| {
        Error::Invalid("清单缺少父目录 / Manifest has no parent directory".into())
    })?)?;
    let directory = manifest_path.parent().ok_or_else(|| {
        Error::Invalid("清单缺少父目录 / Manifest has no parent directory".into())
    })?;
    let log = super::logging::BuildLog::append(&directory.join("inheritance.log"))?;
    let _scope = super::logging::enter([log.clone()]);
    super::logging::diagnostic(format!(
        "\n==> [{}] 开始启动继承检查 / Starting boot inheritance check\n清单 / Manifest: {}\n文件系统顶层 / Filesystem root: {}",
        timestamp(),
        manifest_path.display(),
        filesystem_root.display()
    ));
    let mut result = log
        .check()
        .and_then(|()| finalize_inheritance_inner(manifest_path, filesystem_root, backend));
    match &result {
        Ok(()) => super::logging::diagnostic(format!(
            "==> [{}] 启动继承检查完成 / Boot inheritance check completed",
            timestamp()
        )),
        Err(error) => super::logging::diagnostic(format!(
            "==> [{}] 启动继承失败 / Boot inheritance failed: {error}",
            timestamp()
        )),
    }
    if let Err(error) = log.check() {
        retain_diagnostic_failure(&mut result, error);
    }
    if let Err(error) = &result
        && let Err(error) =
            disk::write_text(&directory.join("inheritance-error.txt"), &error.to_string())
    {
        retain_diagnostic_failure(&mut result, error);
        // Best effort: the same storage failure can also prevent this last sync.
        // 尽力同步；同一个存储故障也可能使最后的同步失败。
        let _ = log.check();
    }
    result
}

/// Preserve the original failure when the diagnostic storage also fails.
/// 诊断存储也失败时，保留原始继承失败原因。
fn retain_diagnostic_failure(result: &mut Result<()>, error: Error) {
    super::logging::diagnostic(format!(
        "无法保存继承诊断 / Cannot save inheritance diagnostics: {error}"
    ));
    let message = match result {
        Ok(()) => error.to_string(),
        Err(original) => {
            format!("{original}; 无法保存继承诊断 / Cannot save inheritance diagnostics: {error}")
        }
    };
    *result = Err(Error::Invalid(message));
}

fn timestamp() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as libc::time_t;
    let mut time: libc::tm = unsafe { std::mem::zeroed() };
    if unsafe { libc::gmtime_r(&now, &mut time) }.is_null() {
        return format!("Unix {now}");
    }
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        time.tm_year + 1900,
        time.tm_mon + 1,
        time.tm_mday,
        time.tm_hour,
        time.tm_min,
        time.tm_sec
    )
}

fn finalize_inheritance_inner(
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
            "待继承清单与挂载文件系统 UUID 不匹配 / Pending inheritance manifest does not match the mounted filesystem UUID".into(),
        ));
    }
    let prepared = top_path(filesystem_root, &manifest.prepared_root)?;
    let final_root = top_path(filesystem_root, &manifest.final_root)?;
    let directory = manifest_path.parent().ok_or_else(|| {
        Error::Invalid("清单缺少父目录 / Manifest has no parent directory".into())
    })?;
    if prepared != directory.join("prepared-root")
        || final_root != directory.join("root")
        || directory.file_name().and_then(|p| p.to_str()) != Some(&manifest.version)
    {
        return Err(Error::Invalid(
            "清单必须与其准备根及最终根位于同一版本目录 / Manifest must reside in the same version directory as its preparation root and final root".into(),
        ));
    }
    let system = directory.parent().and_then(Path::parent).ok_or_else(|| {
        Error::Invalid(
            "待继承版本缺少系统目录 / Pending inheritance version has no system directory".into(),
        )
    })?;
    if system.file_name().and_then(|s| s.to_str()) != Some(&manifest.system_id) {
        return Err(Error::Invalid(
            "清单系统身份不匹配 / Manifest system identity mismatch".into(),
        ));
    }
    let manager = system.parent().and_then(Path::parent).ok_or_else(|| {
        Error::Invalid("继承系统缺少管理根 / Inheritance system has no management root".into())
    })?;
    let _pins_lock = super::records::lock_inheritance(manager)?;
    let state_path = directory.join("state.toml");
    let state_text = fs::read_to_string(&state_path).map_err(|e| disk::io(&state_path, e))?;
    let state: toml::Value =
        toml::from_str(&state_text).map_err(|e| Error::Invalid(e.to_string()))?;
    if state.get("state").and_then(|v| v.as_str()) == Some("cancelled") {
        return Err(Error::Invalid(
            "此版本的继承任务已取消 / Inheritance task for this version has been canceled".into(),
        ));
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
        return Err(Error::Invalid(
            "准备根子卷身份已改变 / Preparation root subvolume identity has changed".into(),
        ));
    }
    let staging = directory.join("inheritance-staging");
    if disk::exists(&staging)? {
        backend.delete_subvolume_recursive(&staging)?;
    }
    let source_root = if manifest.schema_version == 2 {
        let root = manifest.source_root.as_ref().ok_or_else(|| {
            Error::Invalid("构建时无法固定启动继承来源根，请在 Btrfs 来源根环境重新构建 / Cannot pin the boot inheritance source root during build; rebuild in the Btrfs source-root environment".into())
        })?;
        if root.filesystem_uuid != manifest.filesystem_uuid {
            return Err(Error::Invalid(
                "启动继承来源根与目标 Btrfs UUID 不一致 / Boot inheritance source root and target Btrfs UUIDs do not match".into(),
            ));
        }
        let path = top_path(filesystem_root, &root.path)?;
        if !path.is_dir() || backend.source_subvolume_id(&path)? != root.subvolume_id {
            return Err(Error::Invalid(
                "启动继承来源根身份已改变 / Boot inheritance source root identity has changed"
                    .into(),
            ));
        }
        Some(path)
    } else {
        None
    };
    // Validate every source before creating or modifying a destination.
    let mut sources = Vec::new();
    for source in &manifest.sources {
        super::progress::message(
            &manifest.system_id,
            format!("校验继承来源：{}", source.target.display()),
            format!("Validate inheritance source: {}", source.target.display()),
        );
        crate::config::inheritance_name(&source.target)?;
        validate_inheritance_type(&source.target, source.directory)?;
        let path = if let Some(root) = &source_root {
            disk::inheritance_source_path(root, &source.path)?
        } else {
            top_path(filesystem_root, &source.path)?
        };
        if path.starts_with(directory) || directory.starts_with(&path) {
            return Err(Error::Invalid("继承来源与候选版本存储重叠 / Inheritance source overlaps candidate version storage".into()));
        }
        let info = fs::symlink_metadata(&path).map_err(|e| disk::io(&path, e))?;
        validate_inheritance_type(&source.target, info.is_dir())?;
        // Schema 1 pins each resolved source; schema 2 pins only its root view.
        // Sources (including newly created subvolumes) are discovered at first boot.
        let identity_valid = manifest.schema_version == 2
            || if manifest.nocow_handling == NocowHandling::MoveSnapshot && source.directory {
                super::inheritance_migration::validates_identity(
                    backend,
                    &path,
                    manager,
                    &source.subvolume_id,
                    &digest,
                )?
            } else {
                backend.source_subvolume_id(&path)? == source.subvolume_id
            };
        if info.is_dir() != source.directory
            || (!info.is_dir() && !info.is_file() && !info.file_type().is_symlink())
            || !identity_valid
        {
            return Err(Error::Invalid(format!(
                "来源类型或子卷身份已改变: {0} / Source type or subvolume identity has changed: {0}",
                source.target.display()
            )));
        }
        backend.check_cow_source(&path, directory)?;
        sources.push((source, path));
    }
    if manifest.nocow_handling == NocowHandling::MoveSnapshot {
        for (source, path) in &sources {
            if source.directory {
                super::inheritance_migration::prepare(backend, path, manager, &digest)?;
            }
        }
    }
    super::progress::message(
        &manifest.system_id,
        "从准备根创建继承暂存根",
        "Create inheritance staging root from the prepared root",
    );
    backend.snapshot_recursive(&prepared, &staging, false)?;
    super::progress::run(
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
            super::progress::message(
                &manifest.system_id,
                "同步继承提交记录并发布最终根",
                "Sync inheritance commit record and publish the final root",
            );
            disk::write(&directory.join("inheritance-commit.toml"), &receipt)?;
            backend.sync_filesystem(&staging)?;
            fs::rename(&staging, &final_root).map_err(|e| disk::io(&final_root, e))?;
            disk::sync(directory)?;
            backend.sync_filesystem(directory)?;
            mark_complete(directory)
        },
    )
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
            "最终根与继承提交记录不匹配，拒绝继续启动 / Final root does not match the inheritance commit record; refusing to continue booting".into(),
        ));
    }
    for (child, _) in &receipt.children {
        if child.as_os_str().is_empty()
            || child
                .components()
                .any(|p| !matches!(p, Component::Normal(_)))
        {
            return Err(Error::Invalid(
                "提交记录子卷路径无效 / Invalid subvolume path in commit record".into(),
            ));
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
            "最终根与继承提交记录不匹配，拒绝继续启动 / Final root does not match the inheritance commit record; refusing to continue booting".into(),
        ));
    }
    for (child, id) in receipt.children {
        backend.check_subvolume(&root.join(&child), false)?;
        if backend.source_subvolume_id(&root.join(child))? != id {
            return Err(Error::Invalid(
                "最终根内部子卷身份已改变 / Subvolume identity inside the final root has changed"
                    .into(),
            ));
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
    let manifest = read_manifest(&directory.join("inheritance-pending.toml"))?;
    if manifest.schema_version == 1 {
        return Ok(manifest.sources.into_iter().map(|s| s.path).collect());
    }
    let Some(root) = manifest.source_root else {
        return Ok(Vec::new());
    };
    if root.filesystem_uuid != manifest.filesystem_uuid {
        return Ok(Vec::new()); // No source on this filesystem; boot will reject the mismatch.
    }
    // Lexical resolution only: these paths belong to the top-level view, not today's mounts.
    Ok(manifest
        .sources
        .into_iter()
        .map(|s| root.path.join(s.path.strip_prefix("/").unwrap()))
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
