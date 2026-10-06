//! Build and atomically publish system-neutral templates keyed by complete plugin archives.
//! 按完整插件归档构建并原子发布不依附系统身份的模板。
use super::{BuildBackend, BuildContext, Error, Result, fs as disk, plugins::LoadedPlugin};
use crate::{
    base_system,
    config::{BUILD_CONFIG_PATH, BuildConfig},
    software::{PackageSession, SoftwareState},
};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    path::Path,
};

/// Completion marker stored alongside the immutable template root.
/// 与不可变模板根一同保存的完成标记。
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    /// Completion record schema.
    /// 完成记录格式。
    schema_version: u32,
    /// Exact digest of the full archive, including helper resources.
    /// 包含辅助资源的完整归档准确摘要。
    plugin_sha256: String,
    /// Plugin identity retained for provenance, not used as the template key.
    /// 用于来源留档的插件身份，不作为模板索引。
    plugin_id: String,
    /// Plugin release producing the template.
    /// 生成模板的插件发布版本。
    plugin_version: String,
    /// Architecture isolated within the archive digest.
    /// 在归档摘要内独立存放的架构。
    architecture: String,
    /// Validated base package inventory without per-system targets.
    /// 无每系统目标的经校验基础包清单。
    software: SoftwareState,
}

/// Hold the template lock until its source has been snapshotted for this system.
/// 保留模板锁，直到为此系统建立来源快照。
struct Lock(File);

impl Drop for Lock {
    /// Release the lock explicitly, including file descriptions inherited by children.
    /// 显式释放锁，包括子进程继承的文件描述。
    /// # Arguments
    ///
    /// - `self`: Finished template access.
    ///   已结束的模板访问。
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

/// Restore a shared template into a new writable root; return whether it was already built.
/// 将共享模板恢复到新可写根；返回模板是否此前已构建。
/// # Arguments
///
/// - `backend`: Required CoW storage and checked plugin execution.
///   必需的 CoW 存储和经检查的插件执行。
/// - `plugin`: Complete loaded archive, including all helper resources.
///   完整加载的归档，包含所有辅助资源。
/// - `templates`: Manager-owned shared template directory.
///   管理器所属共享模板目录。
/// - `destination`: Absent writable system root to create.
///   待创建且不存在的可写系统根。
/// - `architecture`: Target architecture, isolated even within the same archive hash.
///   目标架构；同一归档哈希内也独立存放。
pub(super) fn restore(
    backend: &impl BuildBackend,
    plugin: &LoadedPlugin,
    templates: &Path,
    destination: &Path,
    architecture: &str,
) -> Result<bool> {
    if plugin.metadata.api_version != 3 {
        return Err(Error::Invalid(
            "无基础缓存的系统创建需要 API 3 插件的 template_clean 和 system_init 操作；API 2 仍可维护已有缓存".into(),
        ));
    }
    let hash = &plugin.archive_sha256;
    if hash.len() != 64
        || !hash
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(Error::Invalid("模板需要完整插件归档的 SHA-256".into()));
    }
    super::plugins::validate_identity(architecture)?;
    let directory = templates.join(hash).join(architecture);
    disk::directory(&directory)?;
    let lock_path = directory.join("build.lock");
    disk::checked(&lock_path)?;
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)
        .map_err(|e| disk::io(&lock_path, e))?;
    file.try_lock().map_err(|e| {
        Error::Invalid(format!(
            "无法取得基础模板构建锁 {}，请在占用者完成后重试: {e}",
            directory.display()
        ))
    })?;
    let _lock = Lock(file);
    let completed = directory.join("completed");
    let reused = disk::exists(&completed)?;
    if !reused {
        build(backend, plugin, &directory, &completed, architecture)?;
    }
    validate(backend, plugin, &completed, architecture)?;
    backend.snapshot(&completed.join("root"), destination, false)?;
    Ok(reused)
}

/// Reject incomplete, altered, or incorrectly keyed template metadata and storage.
/// 拒绝不完整、被修改或索引不符的模板元数据和存储。
/// # Arguments
///
/// - `backend`: Read-only subvolume validation.
///   只读子卷校验。
/// - `plugin`: Expected complete archive provenance.
///   预期完整归档来源。
/// - `completed`: Atomically published template directory.
///   原子发布的模板目录。
/// - `architecture`: Expected target architecture.
///   预期目标架构。
fn validate(
    backend: &impl BuildBackend,
    plugin: &LoadedPlugin,
    completed: &Path,
    architecture: &str,
) -> Result<()> {
    let marker = completed.join("template.toml");
    disk::checked(&marker)?;
    let info = fs::symlink_metadata(&marker).map_err(|e| disk::io(&marker, e))?;
    if !info.is_file() {
        return Err(Error::Invalid(format!(
            "模板完成标记必须是普通文件: {}",
            marker.display()
        )));
    }
    let record: Record =
        toml::from_str(&fs::read_to_string(&marker).map_err(|e| disk::io(&marker, e))?)
            .map_err(|e| Error::Invalid(format!("模板完成标记 {}: {e}", marker.display())))?;
    if record.schema_version != 1
        || record.plugin_sha256 != plugin.archive_sha256
        || record.plugin_id != plugin.metadata.id
        || record.plugin_version != plugin.metadata.version
        || record.architecture != architecture
        || !record.software.requested.is_empty()
    {
        return Err(Error::Invalid(format!(
            "模板完成标记与插件归档或架构不符: {}",
            marker.display()
        )));
    }
    record.software.validate()?;
    backend.check_subvolume(&completed.join("root"), true)
}

/// Build only with fixed template inputs and publish the root and marker in one rename.
/// 仅使用固定模板输入构建，并通过一次重命名发布根和标记。
/// # Arguments
///
/// - `backend`: Required storage and execution.
///   必需存储和执行。
/// - `plugin`: Resources whose entire archive defines the template key.
///   以整个归档定义模板索引的资源。
/// - `directory`: Locked hash/architecture directory.
///   已锁定的哈希/架构目录。
/// - `completed`: Absent final template directory.
///   不存在的最终模板目录。
/// - `architecture`: Target architecture for fixed inputs.
///   固定输入中的目标架构。
fn build(
    backend: &impl BuildBackend,
    plugin: &LoadedPlugin,
    directory: &Path,
    completed: &Path,
    architecture: &str,
) -> Result<()> {
    // Never let TempDir recursively delete a real subvolume, including on failure.
    // 即使失败，也不能让 TempDir 递归删除真实子卷。
    let staging = tempfile::Builder::new()
        .prefix("template-pending-")
        .tempdir_in(directory)
        .map_err(|e| disk::io(directory, e))?
        .keep();
    let root = staging.join("base-root");
    let pending = staging.join("completed");
    let readonly = pending.join("root");
    let result = (|| {
        disk::directory(&pending.join("config/build"))?;
        let mut types = plugin.metadata.system_types.clone();
        types.sort();
        let system_type = types.first().unwrap_or(&plugin.metadata.id);
        let config = BuildConfig {
            schema_version: 1,
            system_type: system_type.clone(),
            architecture: Some(architecture.into()),
            packages: Vec::new(),
        };
        disk::write_text(
            &pending.join("config").join(BUILD_CONFIG_PATH),
            &config.to_ini()?,
        )?;
        let context = BuildContext {
            system_id: "template".into(),
            architecture: architecture.into(),
            config: pending.join("config"),
        };
        backend.create_subvolume(&root)?;
        base_system::install(backend, plugin, &root, &context)?;
        let software = PackageSession {
            backend,
            plugin,
            root: &root,
            context: &context,
        }
        .capture(Vec::new())?;
        // Cleaning must be the last container operation; later probes may create identity state.
        // 清理必须是最后一次容器操作；后续探测可能生成身份状态。
        backend.invoke(plugin, "template_clean", &[], &root, &context)?;
        backend.snapshot(&root, &readonly, true)?;
        backend.check_subvolume(&readonly, true)?;
        disk::write(
            &pending.join("template.toml"),
            &Record {
                schema_version: 1,
                plugin_sha256: plugin.archive_sha256.clone(),
                plugin_id: plugin.metadata.id.clone(),
                plugin_version: plugin.metadata.version.clone(),
                architecture: architecture.into(),
                software,
            },
        )?;
        fs::rename(&pending, completed).map_err(|e| disk::io(completed, e))?;
        disk::sync(directory)
    })();
    let cleanup = (|| {
        for path in [&readonly, &root] {
            if disk::exists(path)? {
                backend.delete_subvolume(path)?;
            }
        }
        fs::remove_dir_all(&staging).map_err(|e| disk::io(&staging, e))
    })();
    match (result, cleanup) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(error), Err(cleanup)) => Err(Error::Invalid(format!(
            "{error}; 模板构建清理失败: {cleanup}"
        ))),
    }
}
