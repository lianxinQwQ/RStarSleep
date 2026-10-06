//! Sequential version orchestration with latest-attempt cache selection.
//! 使用最新尝试缓存选择的顺序版本编排。
use super::{
    BuildBackend, BuildContext, BuildOutcome, Error, Result, SystemBackend,
    cache::{self, Cache, Identity},
    frozen::config_hash,
    fs,
    plugins::{self, LoadedPlugin},
    records::{Attempt, Status},
    stages::Stages,
};
use crate::config::{ConfigStore, read_build_config_at};
use std::path::{Path, PathBuf};

/// Build one named system or every configured system using native CoW storage.
/// 使用原生 CoW 存储构建一个命名系统或所有已配置系统。
/// # Arguments
///
/// - `store`: Initialized manager root.
///   已初始化管理根。
/// - `system`: Optional selected system; omission selects all.
///   可选所选系统；省略选择全部。
/// # Errors
///
/// Returns build failures, continuing independent systems in an all-system run.
/// 返回构建错误；全部构建时继续尝试其他独立系统。
pub fn build(store: &ConfigStore, system: Option<&str>) -> Result<Vec<BuildOutcome>> {
    build_with_backend(store, system, &SystemBackend)
}

/// Build with an injectable backend while preserving locks, records, and control flow.
/// 使用可注入后端构建，保留锁、记录和控制流程。
/// # Arguments
///
/// - `store`: Initialized manager root.
///   已初始化管理根。
/// - `system`: Optional selected system.
///   可选所选系统。
/// - `backend`: Required storage and plugin execution.
///   必需存储和插件执行。
/// # Errors
///
/// Returns selection failures or aggregated individual build failures.
/// 返回选择错误或汇总各个构建错误。
pub fn build_with_backend(
    store: &ConfigStore,
    system: Option<&str>,
    backend: &impl BuildBackend,
) -> Result<Vec<BuildOutcome>> {
    let systems = if let Some(id) = system {
        store.system_layout(id)?;
        vec![id.to_owned()]
    } else {
        store.list_systems()?
    };
    let work = store.build_work_dir()?;
    check_workspace(store, &work)?;
    let mut outcomes = Vec::new();
    let mut failures = Vec::new();
    for id in systems {
        match build_system(store, &id, &work, backend) {
            Ok(outcome) => outcomes.push(outcome),
            Err(error) if system.is_some() => return Err(error),
            Err(error) => failures.push(format!("{id}: {error}")),
        }
    }
    if failures.is_empty() {
        Ok(outcomes)
    } else {
        for outcome in outcomes {
            eprintln!(
                "构建完成 {}: {} -> {}",
                outcome.system_id,
                outcome.version,
                outcome.root.display()
            );
        }
        Err(Error::Batch(failures))
    }
}

/// Require an independent workspace that cannot overlap manager-owned permanent data.
/// 要求独立工作目录，不能与管理器永久数据重叠。
/// # Arguments
///
/// - `store`: Manager root owning permanent data.
///   拥有永久数据的管理根。
/// - `work`: Resolved temporary work directory.
///   已解析临时工作目录。
fn check_workspace(store: &ConfigStore, work: &Path) -> Result<()> {
    fs::checked(work)?;
    for path in [
        store.root().join("config"),
        store.plugins_path(),
        store.root().join("systems"),
    ] {
        if work.starts_with(&path) || path.starts_with(work) {
            return Err(Error::Invalid(format!(
                "work_dir 必须独立于管理配置、插件和系统记录: {}",
                work.display()
            )));
        }
    }
    Ok(())
}

/// Persist building/failed/complete status with the loaded archive's exact provenance.
/// 持久化构建中/失败/完成状态及实际加载归档的准确来源。
/// # Arguments
///
/// - `attempt`: Locked version attempt.
///   已锁定版本尝试。
/// - `id`: System identity.
///   系统标识。
/// - `state`: State transition.
///   状态迁移。
/// - `error`: Optional failure message.
///   可选错误信息。
/// - `plugin`: Optional successfully loaded plugin.
///   可选成功加载的插件。
fn status(
    attempt: &Attempt,
    id: &str,
    state: &str,
    error: Option<String>,
    plugin: Option<&LoadedPlugin>,
) -> Result<()> {
    attempt.status(&Status {
        schema_version: 1,
        version: &attempt.version,
        system_id: id,
        state,
        error,
        plugin_id: plugin.map(|p| p.metadata.id.as_str()),
        plugin_version: plugin.map(|p| p.metadata.version.as_str()),
        plugin_sha256: plugin.map(|p| p.archive_sha256.as_str()),
    })
}

/// Build one attempt, retaining completed caches and cleaning owned writable roots on failure.
/// 构建一次尝试，失败时保留完成缓存并清理所属可写根。
/// # Arguments
///
/// - `store`: Manager configuration store.
///   管理器配置存储。
/// - `id`: Selected system.
///   所选系统。
/// - `work`: Independent scratch directory.
///   独立临时目录。
/// - `backend`: Required storage and execution backend.
///   必需存储和执行后端。
fn build_system(
    store: &ConfigStore,
    id: &str,
    work: &Path,
    backend: &impl BuildBackend,
) -> Result<BuildOutcome> {
    let attempt = Attempt::begin(store, id)?;
    let mut plugin = None;
    let scratch = work.join(id).join(&attempt.version);
    let mut owned = Vec::new();
    let result = (|| {
        status(&attempt, id, "building", None, None)?;
        attempt.freeze()?;
        let config = read_build_config_at(&attempt.config)?;
        let architecture = config
            .architecture
            .as_deref()
            .unwrap_or(std::env::consts::ARCH)
            .to_owned();
        plugin = Some(plugins::find(
            &store.plugins_path(),
            &config.system_type,
            &architecture,
        )?);
        let loaded = plugin
            .as_ref()
            .expect("plugin assigned before stage execution");
        for warning in &loaded.discovery_warnings {
            eprintln!("{warning}");
        }
        status(&attempt, id, "building", None, Some(loaded))?;
        let context = BuildContext {
            system_id: id.into(),
            architecture,
            config: attempt.config.clone(),
        };
        let mut protected = loaded.metadata.protected_packages.clone();
        protected.sort();
        protected.dedup();
        let identity = Identity {
            plugin_id: loaded.metadata.id.clone(),
            cache_format: loaded.metadata.cache_format.clone(),
            system_type: config.system_type.clone(),
            architecture: context.architecture.clone(),
            protected_packages: protected,
            options_sha256: config_hash(&attempt.config, true)?,
        };
        let (base, packages) = previous_caches(&attempt, &identity, backend)?;
        if fs::exists(&scratch)? {
            return Err(Error::Invalid(format!(
                "构建工作目录已存在: {}",
                scratch.display()
            )));
        }
        fs::directory(&scratch)?;
        let stages = Stages {
            backend,
            plugin: loaded,
            context: &context,
            attempt: &attempt,
            config: &config,
            identity: &identity,
            scratch: &scratch,
        };
        stages.run(base, packages, &mut owned)
    })();
    let cleanup = cleanup(backend, &owned);
    let result = match (result, cleanup) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Err(error)) => Err(error),
        (Err(error), Err(cleanup)) => Err(Error::Invalid(format!(
            "{error}; 工作根清理失败: {cleanup}"
        ))),
    };
    match result {
        Ok(outcome) => {
            status(&attempt, id, "complete", None, plugin.as_ref())?;
            Ok(outcome)
        }
        Err(error) => {
            if let Err(record_error) = status(
                &attempt,
                id,
                "failed",
                Some(error.to_string()),
                plugin.as_ref(),
            ) {
                return Err(Error::Invalid(format!(
                    "{error}; 失败记录写入失败: {record_error}"
                )));
            }
            Err(error)
        }
    }
}

/// Check both stages from only the highest-numbered prior attempt, never older versions.
/// 仅检查序号最大过往尝试的两个阶段，绝不搜索更早版本。
/// # Arguments
///
/// - `attempt`: Current attempt with selected predecessor.
///   带所选前驱的当前尝试。
/// - `identity`: Current compatibility requirements.
///   当前兼容性要求。
/// - `backend`: Storage validation backend.
///   存储校验后端。
fn previous_caches(
    attempt: &Attempt,
    identity: &Identity,
    backend: &impl BuildBackend,
) -> Result<(Option<Cache>, Option<Cache>)> {
    let Some(version) = &attempt.previous else {
        return Ok((None, None));
    };
    let directory = attempt.layout.cache.join(version);
    let config = attempt.layout.versions.join(version).join("config");
    Ok((
        cache::load(
            backend,
            &directory,
            &config,
            version,
            "base-system",
            identity,
        )?,
        cache::load(backend, &directory, &config, version, "packages", identity)?,
    ))
}

/// Delete only owned scratch subvolumes, collecting all cleanup failures.
/// 仅删除所属临时子卷，收集全部清理错误。
/// # Arguments
///
/// - `backend`: Checked subvolume deletion.
///   经检查的子卷删除。
/// - `owned`: Paths reserved by this attempt.
///   此次尝试预留的路径。
fn cleanup(backend: &impl BuildBackend, owned: &[PathBuf]) -> Result<()> {
    let mut failures = Vec::new();
    for root in owned.iter().rev() {
        match fs::exists(root).and_then(|exists| {
            if exists {
                backend.delete_subvolume(root)
            } else {
                Ok(())
            }
        }) {
            Ok(()) => (),
            Err(error) => failures.push(error.to_string()),
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(Error::Invalid(failures.join("; ")))
    }
}
