//! Validate completed stages and atomically publish read-only CoW caches.
//! 校验已完成阶段并原子发布只读 CoW 缓存。
use super::{BuildBackend, Error, Result, frozen::config_hash, fs as disk};
use crate::software::SoftwareState;
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
};

/// Ordered cache stages. Append future stages after script here so startup cleanup covers them.
/// 有序缓存阶段。未来 script 之后的新阶段须追加于此，以纳入构建前固定清理。
pub(super) const STAGES: &[&str] = &["base-system", "packages", "script", "inherit"];

/// Delete a stage and its successors across this system's historical cache directories.
/// 删除此系统历史缓存目录中指定阶段及其后续阶段。
/// # Arguments
/// - `backend`: Checked recursive subvolume deletion; ordinary directories are never a fallback.
///   经检查的递归子卷删除；不回退为普通目录删除。
/// - `cache_root`: One system's cache root, protected by its build lock.
///   由系统构建锁保护的单个系统缓存根。
/// - `stage`: First invalidated stage in STAGES.
///   STAGES 中首个失效阶段。
pub(super) fn clear_from(
    backend: &impl BuildBackend,
    cache_root: &Path,
    stage: &str,
) -> Result<()> {
    let start = STAGES
        .iter()
        .position(|candidate| *candidate == stage)
        .ok_or_else(|| Error::Invalid(format!("未知缓存阶段: {stage}")))?;
    if !disk::exists(cache_root)? {
        return Ok(());
    }
    disk::checked(cache_root)?;
    for entry in fs::read_dir(cache_root).map_err(|e| disk::io(cache_root, e))? {
        let entry = entry.map_err(|e| disk::io(cache_root, e))?;
        let directory = entry.path();
        disk::checked(&directory)?;
        if !entry
            .file_type()
            .map_err(|e| disk::io(&directory, e))?
            .is_dir()
        {
            return Err(Error::Invalid(format!(
                "系统缓存条目必须是目录: {}",
                directory.display()
            )));
        }
        // Retire completion markers first; an interrupted deletion must never reuse stale roots.
        // 先撤销完成标记，避免删除中断后复用失效根。
        for stage in &STAGES[start..] {
            let sidecar = directory.join(format!("{stage}.toml"));
            if disk::exists(&sidecar)? {
                disk::checked(&sidecar)?;
                fs::remove_file(&sidecar).map_err(|e| disk::io(&sidecar, e))?;
            }
        }
        disk::sync(&directory)?;
        for stage in STAGES[start..].iter().rev() {
            let root = directory.join(stage);
            if disk::exists(&root)? {
                disk::checked(&root)?;
                backend.delete_subvolume_recursive(&root)?;
            }
        }
        disk::sync(&directory)?;
    }
    Ok(())
}

/// Compatibility of filesystem and non-package inputs across plugin releases.
/// 跨插件发布版本的文件系统及非包输入兼容性。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Identity {
    /// Stable distribution plugin identity.
    /// 稳定发行版插件标识。
    pub plugin_id: String,
    /// Filesystem cache compatibility format.
    /// 文件系统缓存兼容格式。
    pub cache_format: String,
    /// Frozen system type selected by configuration.
    /// 配置选定的冻结系统类型。
    pub system_type: String,
    /// Frozen target architecture.
    /// 冻结目标架构。
    pub architecture: String,
    /// Normalized protected explicit base targets.
    /// 规范化受保护显式基础目标。
    pub protected_packages: Vec<String>,
    /// Digest of the parameter dependencies selected by the plugin for this stage.
    /// 插件为此阶段选定的参数依赖摘要。
    pub options_sha256: String,
}

/// Completion sidecar outside the cached root, installed after its snapshot.
/// 缓存根之外的完成旁记录，在快照之后安装。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Record {
    /// Supported serialized record format.
    /// 受支持序列化记录格式。
    pub schema_version: u32,
    /// Completed base-system, packages, or script stage.
    /// 已完成的基础、软件或脚本阶段。
    pub stage: String,
    /// Owning monotonic build version.
    /// 所属单调构建版本。
    pub version: String,
    /// Actual donor stage version, absent when installed fresh.
    /// 实际来源阶段版本，全新安装则省略。
    pub source_version: Option<String>,
    /// Version that last changed this stage content.
    /// 最后改变此阶段内容的版本。
    pub origin_version: String,
    /// Release producing the recorded content or current status.
    /// 生成记录内容或当前状态的发布版本。
    pub plugin_version: String,
    /// Exact source archive digest retained for provenance.
    /// 为来源保留的准确归档摘要。
    pub plugin_sha256: String,
    /// Compatibility requirements of this completed stage.
    /// 此完成阶段的兼容性要求。
    pub identity: Identity,
    /// Full archived configuration digest including the package table.
    /// 包括包表的完整留档配置摘要。
    pub config_sha256: String,
    /// Resolved managed targets and actual installed inventory.
    /// 已解析管理目标和实际安装清单。
    pub software: SoftwareState,
}

/// One validated completed cache and its durable inventory/provenance.
/// 一个经过校验的完成缓存及其持久化清单/来源。
pub(super) struct Cache {
    /// Validated read-only cached root subvolume.
    /// 经校验的只读缓存根子卷。
    pub root: PathBuf,
    /// Durable stage completion sidecar.
    /// 持久化阶段完成旁记录。
    pub record: Record,
}

/// Validate sidecar invariants even when compatibility prevents reuse.
/// 即使兼容性阻止复用，也校验旁记录的不变量。
/// # Arguments
///
/// - `record`: Decoded completion record.
///   已解码完成记录。
/// - `version`: Directory's expected version.
///   目录的预期版本。
/// - `stage`: Expected stage.
///   预期阶段。
fn validate(record: &Record, version: &str, stage: &str) -> Result<()> {
    if record.schema_version != 1
        || record.version != version
        || record.stage != stage
        || record.plugin_version.is_empty()
    {
        return Err(Error::Invalid(format!(
            "缓存完成记录不符: {version}/{stage}"
        )));
    }
    for hash in [
        &record.plugin_sha256,
        &record.config_sha256,
        &record.identity.options_sha256,
    ] {
        if hash.len() != 64 || !hash.bytes().all(|c| c.is_ascii_hexdigit()) {
            return Err(Error::Invalid("缓存摘要无效".into()));
        }
    }
    for id in [
        &record.origin_version,
        &record.identity.plugin_id,
        &record.identity.cache_format,
        &record.identity.system_type,
        &record.identity.architecture,
    ] {
        super::plugins::validate_identity(id)?;
    }
    if let Some(source) = &record.source_version {
        super::plugins::validate_identity(source)?;
    }
    for name in &record.identity.protected_packages {
        crate::software::valid_name(name)?;
    }
    record.software.validate()
}

/// Load only the selected prior attempt's completed and compatible stage.
/// 仅加载所选过往尝试中已完成且兼容的阶段。
/// # Arguments
///
/// - `backend`: Real or simulated CoW validation.
///   真实或模拟 CoW 校验。
/// - `cache_directory`: Prior attempt's cache directory.
///   过往尝试的缓存目录。
/// - `config`: That attempt's archived configuration.
///   该尝试的留档配置。
/// - `version`: Selected prior version.
///   所选过往版本。
/// - `stage`: Fixed stage name.
///   固定阶段名称。
/// - `identity`: Current compatibility requirements.
///   当前兼容性要求。
pub(super) fn load(
    backend: &impl BuildBackend,
    cache_directory: &Path,
    config: &Path,
    version: &str,
    stage: &str,
    identity: &Identity,
) -> Result<Option<Cache>> {
    let root = cache_directory.join(stage);
    let sidecar = cache_directory.join(format!("{stage}.toml"));
    let root_exists = disk::exists(&root)?;
    if root_exists {
        backend.check_subvolume(&root, true)?;
        for child in backend.nested_subvolumes(&root)? {
            backend.check_subvolume(&root.join(child), true)?;
        }
    }
    if !disk::exists(&sidecar)? {
        return Ok(None);
    }
    if !fs::symlink_metadata(&sidecar)
        .map_err(|e| disk::io(&sidecar, e))?
        .is_file()
    {
        return Err(Error::Invalid(format!(
            "缓存完成记录必须是普通文件: {}",
            sidecar.display()
        )));
    }
    let text = fs::read_to_string(&sidecar).map_err(|e| disk::io(&sidecar, e))?;
    let record: Record = toml::from_str(&text)
        .map_err(|e| Error::Invalid(format!("缓存记录 {}: {e}", sidecar.display())))?;
    validate(&record, version, stage)?;
    if !root_exists {
        return Ok(None);
    }
    if config_hash(config)? != record.config_sha256 {
        return Err(Error::Invalid(format!("缓存来源配置已改变: {version}")));
    }
    if &record.identity != identity {
        return Ok(None);
    }
    // Source packages must be read from this exact attempt, including failed attempts.
    // 来源包必须从此次准确尝试读取，包括失败尝试。
    crate::config::read_build_config_at(config)?;
    Ok(Some(Cache { root, record }))
}

/// Snapshot a successful stage and publish its completion marker last.
/// 对成功阶段建立快照，最后发布完成标记。
/// # Arguments
///
/// - `backend`: Required CoW backend.
///   必需 CoW 后端。
/// - `source`: Fully completed stage root.
///   完整完成的阶段根。
/// - `cache_directory`: Current attempt's cache directory.
///   当前尝试缓存目录。
/// - `record`: Completed stage metadata.
///   已完成阶段元数据。
pub(super) fn commit(
    backend: &impl BuildBackend,
    source: &Path,
    cache_directory: &Path,
    record: Record,
) -> Result<Cache> {
    validate(&record, &record.version, &record.stage)?;
    disk::directory(cache_directory)?;
    let root = cache_directory.join(&record.stage);
    let sidecar = cache_directory.join(format!("{}.toml", record.stage));
    if disk::exists(&root)? || disk::exists(&sidecar)? {
        return Err(Error::Invalid("不覆盖现有阶段缓存".into()));
    }
    // Keep the snapshot beside its destination; moving a read-only Btrfs subvolume across parents fails.
    // 快照与目标同级；跨父目录移动只读 Btrfs 子卷会失败。
    // Remove the ordinary placeholder before snapshotting; no TempDir may own a subvolume.
    // 快照前移除普通占位目录；不能让 TempDir 持有子卷。
    let pending = tempfile::Builder::new()
        .prefix("stage-pending-")
        .tempdir_in(cache_directory)
        .map_err(|e| disk::io(cache_directory, e))?
        .keep();
    fs::remove_dir(&pending).map_err(|e| disk::io(&pending, e))?;
    let result = (|| {
        backend.snapshot_recursive(source, &pending, true)?;
        backend.check_subvolume(&pending, true)?;
        fs::rename(&pending, &root).map_err(|e| disk::io(&root, e))?;
        disk::sync(cache_directory)?;
        disk::write(&sidecar, &record)?;
        Ok(Cache { root, record })
    })();
    let cleanup = (|| -> Result<()> {
        if disk::exists(&pending)? {
            backend.delete_subvolume_recursive(&pending)?;
        }
        Ok(())
    })();
    match (result, cleanup) {
        (Ok(cache), Ok(())) => Ok(cache),
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Err(error)) => Err(error),
        (Err(error), Err(cleanup)) => Err(Error::Invalid(format!(
            "{error}; 暂存缓存清理失败: {cleanup}"
        ))),
    }
}
