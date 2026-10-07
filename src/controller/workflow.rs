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
use crate::config::{ConfigStore, read_build_config_at, read_distribution_options_at};
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
    let backend = super::scoped_backend::ScopedBackend {
        backend,
        pool: store.persistent_path(),
        state: store.root().join("persistent-state"),
    };
    let mut outcomes = Vec::new();
    let mut failures = Vec::new();
    let mut prepared = Vec::new();
    for id in systems {
        match prepare_system(store, &id, &backend) {
            Ok(value) => prepared.push(value),
            Err(error) if system.is_some() => return Err(error),
            Err(error) => failures.push(format!("{id}: {error}")),
        }
    }
    let mut groups: std::collections::BTreeMap<
        (String, String, std::collections::BTreeMap<String, String>),
        Vec<usize>,
    > = std::collections::BTreeMap::new();
    for (index, value) in prepared.iter().enumerate() {
        if value.plugin.metadata.predownload {
            groups
                .entry((
                    value.plugin.archive_sha256.clone(),
                    value.context.architecture.clone(),
                    value.context.distribution_options.clone(),
                ))
                .or_default()
                .push(index);
        }
    }
    let mut failed = std::collections::BTreeSet::new();
    for ((_, _, _), indices) in groups {
        let mut requirements = std::collections::BTreeSet::new();
        for &index in &indices {
            requirements.extend(prepared[index].config.packages.iter().cloned());
            requirements.extend(prepared[index].upgrade_packages.iter().cloned());
        }
        let packages: Vec<_> = requirements.into_iter().collect();
        if let Err(error) = super::predownload::run(
            &backend,
            store,
            &prepared[indices[0]].plugin,
            &prepared[indices[0]].context,
            &packages,
        ) {
            for &index in &indices {
                let value = &prepared[index];
                status(
                    &value.attempt,
                    &value.context.system_id,
                    "failed",
                    Some(error.to_string()),
                    Some(&value.plugin),
                )?;
                failed.insert(index);
            }
            if system.is_some() {
                return Err(error);
            }
            failures.push(format!(
                "预下载 {}: {error}",
                prepared[indices[0]].plugin.metadata.id
            ));
        }
    }
    for (index, value) in prepared.iter().enumerate() {
        if failed.contains(&index) {
            continue;
        }
        match build_system(value, &work, &backend) {
            Ok(outcome) => outcomes.push(outcome),
            Err(error) if system.is_some() => return Err(error),
            Err(error) => failures.push(format!("{}: {error}", value.context.system_id)),
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
        store.predownload_path(),
        store.persistent_path(),
        store.root().join("persistent-state"),
        store.root().join("systems"),
    ] {
        if work.starts_with(&path) || path.starts_with(work) {
            return Err(Error::Invalid(format!(
                "work_dir 必须独立于管理配置、插件、持久数据和系统记录: {}",
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

/// Retain locked, frozen, validated inputs through preparation and version publication.
/// 在准备和版本发布期间保留已锁定、冻结及校验的输入。
struct Prepared {
    attempt: Attempt,
    plugin: LoadedPlugin,
    config: crate::config::BuildConfig,
    context: BuildContext,
    upgrade_packages: Vec<String>,
}

/// Freeze one system, validate its selected plugin, and remove invalidated caches before preparation.
/// 冻结单个系统、校验其选定插件，并在准备前删除失效缓存。
/// # Arguments
/// - `store`: Initialized manager containing the system and plugins.
///   包含系统和插件的已初始化管理器。
/// - `id`: Managed-system identifier to lock and prepare.
///   待锁定和准备的被管理系统标识。
/// - `backend`: Checked cache storage backend.
///   经检查的缓存存储后端。
fn prepare_system(store: &ConfigStore, id: &str, backend: &impl BuildBackend) -> Result<Prepared> {
    let attempt = Attempt::begin(store, id)?;
    let mut plugin = None;
    let result: Result<(crate::config::BuildConfig, BuildContext, Vec<String>)> = (|| {
        status(&attempt, id, "building", None, None)?;
        // Script output is never a donor. Append future stages after script to cache::STAGES
        // so every build also removes their caches, even when configuration validation fails.
        // script 输出绝不作为来源。未来 script 后的新阶段须追加到 cache::STAGES，
        // 使每次构建均清理其缓存，包括配置校验失败的情况。
        cache::clear_from(backend, &attempt.layout.cache, "script")?;
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
        let loaded = plugin.as_ref().unwrap();
        let distribution_options = read_distribution_options_at(&attempt.config)?;
        loaded.validate_configuration(&distribution_options)?;
        for warning in &loaded.discovery_warnings {
            eprintln!("{warning}");
        }
        status(&attempt, id, "building", None, Some(loaded))?;
        let context = BuildContext {
            system_id: id.into(),
            architecture,
            distribution_options,
            config: attempt.config.clone(),
            persistent_mounts: Vec::new(),
        };
        let mut upgrade_packages = Vec::new();
        {
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
            if let Some(version) = &attempt.previous {
                let previous = attempt.layout.versions.join(version).join("config");
                // Completed donors above have verified provenance. A failed attempt without a
                // readable config has no donor; retire older caches and install independently.
                // 上方完成来源已校验出处。失败尝试若无可读配置则没有来源；清理旧缓存并独立安装。
                if fs::exists(&previous)?
                    && config_hash(&previous, true).ok().as_ref() != Some(&identity.options_sha256)
                {
                    cache::clear_from(backend, &attempt.layout.cache, "base-system")?;
                }
            }
            if loaded.metadata.predownload
                && let Some(cache) = packages.or(base)
            {
                upgrade_packages = cache
                    .record
                    .software
                    .installed
                    .into_iter()
                    .map(|p| p.name)
                    .collect();
            }
        }
        Ok((config, context, upgrade_packages))
    })();
    match result {
        Ok((config, context, upgrade_packages)) => Ok(Prepared {
            attempt,
            plugin: plugin.unwrap(),
            config,
            context,
            upgrade_packages,
        }),
        Err(error) => {
            status(
                &attempt,
                id,
                "failed",
                Some(error.to_string()),
                plugin.as_ref(),
            )?;
            Err(error)
        }
    }
}

/// Run prepared stages and record completion, preserving earlier completed caches on failure.
/// 执行已准备阶段并记录完成状态，失败时保留先前完成的缓存。
/// # Arguments
/// - `prepared`: Locked and validated frozen build inputs.
///   已锁定并校验的冻结构建输入。
/// - `work`: Independent manager scratch directory.
///   管理器的独立临时目录。
/// - `backend`: Checked storage and plugin execution.
///   经检查的存储及插件执行。
fn build_system(
    prepared: &Prepared,
    work: &Path,
    backend: &impl BuildBackend,
) -> Result<BuildOutcome> {
    let Prepared {
        attempt,
        plugin: loaded,
        config,
        context,
        ..
    } = prepared;
    let id = &context.system_id;
    let scratch = work.join(id).join(&attempt.version);
    let mut owned = Vec::new();
    let result = (|| {
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
        let (base, packages) = previous_caches(attempt, &identity, backend)?;
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
            context,
            attempt,
            config,
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
            status(attempt, id, "complete", None, Some(loaded))?;
            Ok(outcome)
        }
        Err(error) => {
            if let Err(record_error) =
                status(attempt, id, "failed", Some(error.to_string()), Some(loaded))
            {
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
