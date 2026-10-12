//! Sequential version orchestration with latest-attempt cache selection.
//! 使用最新尝试缓存选择的顺序版本编排。
use super::{
    BuildBackend, BuildContext, BuildOutcome, Error, Result, SystemBackend,
    cache::{self, Cache, Identity},
    fs,
    plugins::{self, LoadedPlugin},
    progress,
    records::{Attempt, Status},
    stages::Stages,
};
use crate::config::{
    ConfigStore, read_build_config_at, read_distribution_options_at, read_plugin_parameters_at,
};
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
    progress::message(
        "build",
        format!("准备构建 {} 个系统", systems.len()),
        format!("Preparing to build {} systems", systems.len()),
    );
    let runtime = store.runtime_config()?;
    super::boot::sync_executable(store.root())?;
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
    let system_count = systems.len();
    for (index, id) in systems.into_iter().enumerate() {
        match progress::run(
            &id,
            format!("准备配置与检查缓存（{}/{system_count}）", index + 1),
            format!(
                "Prepare configuration and check caches ({}/{system_count})",
                index + 1
            ),
            || prepare_system(store, &id, &backend),
        ) {
            Ok(value) => prepared.push(value),
            Err(error) if system.is_some() => return Err(error),
            Err(error) => failures.push(format!("{id}: {error}")),
        }
    }
    type DownloadGroup = (
        String,
        String,
        std::collections::BTreeMap<String, String>,
        crate::config::Environment,
    );
    let mut groups: std::collections::BTreeMap<DownloadGroup, Vec<usize>> =
        std::collections::BTreeMap::new();
    for (index, value) in prepared.iter().enumerate() {
        if value.plugin.metadata.predownload {
            groups
                .entry((
                    value.plugin.archive_sha256.clone(),
                    value.context.architecture.clone(),
                    value.context.distribution_options.clone(),
                    value.context.environment.clone(),
                ))
                .or_default()
                .push(index);
        }
    }
    let mut failed = std::collections::BTreeSet::new();
    let group_count = groups.len();
    for (group_index, ((_, _, _, _), indices)) in groups.into_iter().enumerate() {
        let mut requirements = std::collections::BTreeSet::new();
        for &index in &indices {
            requirements.extend(prepared[index].config.packages.iter().cloned());
            requirements.extend(prepared[index].upgrade_packages.iter().cloned());
        }
        let packages: Vec<_> = requirements.into_iter().collect();
        let systems = indices
            .iter()
            .map(|&index| prepared[index].context.system_id.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        if let Err(error) = progress::run(
            "predownload",
            format!(
                "预下载批次 {}/{}：系统 {systems}，{} 项软件需求",
                group_index + 1,
                group_count,
                packages.len()
            ),
            format!(
                "Predownload batch {}/{}: systems {systems}, {} package requirements",
                group_index + 1,
                group_count,
                packages.len()
            ),
            || {
                super::predownload::run(
                    &backend,
                    store,
                    &prepared[indices[0]].plugin,
                    &prepared[indices[0]].context,
                    &packages,
                )
            },
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
        match progress::run(
            &value.context.system_id,
            format!("构建版本 {}", value.attempt.version),
            format!("Build version {}", value.attempt.version),
            || build_system(value, &work, &backend, &runtime),
        ) {
            Ok(outcome) => outcomes.push(outcome),
            Err(error) if system.is_some() => return Err(error),
            Err(error) => failures.push(format!("{}: {error}", value.context.system_id)),
        }
    }
    if failures.is_empty() {
        progress::message(
            "build",
            format!("构建结束：{} 个系统成功", outcomes.len()),
            format!("Build finished: {} systems succeeded", outcomes.len()),
        );
        Ok(outcomes)
    } else {
        progress::message(
            "build",
            format!(
                "构建结束：{} 个系统成功，{} 项失败",
                outcomes.len(),
                failures.len()
            ),
            format!(
                "Build finished: {} systems succeeded, {} failures",
                outcomes.len(),
                failures.len()
            ),
        );
        for outcome in outcomes {
            eprintln!(
                "构建完成 / Build completed {}: {} -> {}",
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
        progress::message(
            id,
            "清理历史脚本及后续阶段缓存",
            "Clear previous script and later stage caches",
        );
        cache::clear_from(backend, &attempt.layout.cache, "script")?;
        progress::message(
            id,
            "冻结构建配置，检查插件及参数",
            "Freeze build configuration and validate plugin parameters",
        );
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
        progress::message(
            id,
            format!(
                "使用插件 {} {}，架构 {architecture}",
                loaded.metadata.id, loaded.metadata.version
            ),
            format!(
                "Using plugin {} {}, architecture {architecture}",
                loaded.metadata.id, loaded.metadata.version
            ),
        );
        let distribution_options = read_distribution_options_at(&attempt.config)?;
        let input = read_plugin_parameters_at(&attempt.config)?;
        let environment = crate::config::read_environment_at(&attempt.config)?;
        loaded.validate_parameter_lists(&input)?;
        crate::config::InheritanceConfig::read(&input)?;
        let parameters = input
            .values
            .into_iter()
            .filter(|(name, _)| !name.starts_with("RSTAR_INHERITANCE_"))
            .collect();
        for warning in &loaded.discovery_warnings {
            eprintln!("{warning}");
        }
        status(&attempt, id, "building", None, Some(loaded))?;
        let context = BuildContext {
            system_id: id.into(),
            architecture,
            distribution_options,
            parameters,
            config: attempt.config.clone(),
            persistent_mounts: Vec::new(),
            parameter_lists: input.lists,
            environment,
            inherited_mounts: Vec::new(),
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
                options_sha256: loaded.cache_configuration_hash(&attempt.config, false)?,
            };
            let (base, packages) = previous_caches(&attempt, &identity, loaded, backend)?;
            if let Some(version) = &attempt.previous {
                let previous = attempt.layout.versions.join(version).join("config");
                // Completed donors above have verified provenance. A failed attempt without a
                // readable config has no donor; retire older caches and install independently.
                // 上方完成来源已校验出处。失败尝试若无可读配置则没有来源；清理旧缓存并独立安装。
                if fs::exists(&previous)?
                    && loaded
                        .cache_configuration_hash(&previous, false)
                        .ok()
                        .as_ref()
                        != Some(&identity.options_sha256)
                {
                    progress::message(
                        id,
                        "基础配置已变化，清理基础及后续缓存",
                        "Base configuration changed; clearing base and later caches",
                    );
                    cache::clear_from(backend, &attempt.layout.cache, "base-system")?;
                } else if fs::exists(&previous)?
                    && loaded.cache_configuration_hash(&previous, true).ok()
                        != Some(loaded.cache_configuration_hash(&attempt.config, true)?)
                {
                    progress::message(
                        id,
                        "软件配置已变化，清理软件及后续缓存",
                        "Software configuration changed; clearing software and later caches",
                    );
                    cache::clear_from(backend, &attempt.layout.cache, "packages")?;
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
    runtime: &crate::config::RuntimeConfig,
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
            options_sha256: loaded.cache_configuration_hash(&attempt.config, false)?,
        };
        let (base, packages) = previous_caches(attempt, &identity, loaded, backend)?;
        if fs::exists(&scratch)? {
            return Err(Error::Invalid(format!(
                "构建工作目录已存在: {}",
                scratch.display()
            )));
        }
        fs::directory(&scratch)?;
        let mut software_identity = identity.clone();
        software_identity.options_sha256 =
            loaded.cache_configuration_hash(&attempt.config, true)?;
        let stages = Stages {
            backend,
            plugin: loaded,
            context,
            attempt,
            config,
            identity: &identity,
            software_identity: &software_identity,
            scratch: &scratch,
            prepare_boot_inheritance: runtime.main_system.as_deref() == Some(id.as_str()),
        };
        stages.run(base, packages, &mut owned)
    })();
    let cleanup = progress::run(
        id,
        "清理临时工作根",
        "Clean temporary work roots",
        || cleanup(backend, &owned),
    );
    let result = match (result, cleanup) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Err(error)) => Err(error),
        (Err(error), Err(cleanup)) => Err(Error::Invalid(format!(
            "{error}; 工作根清理失败: {cleanup}"
        ))),
    };
    let result = result.and_then(|outcome| {
        if runtime.main_system.as_deref() == Some(id.as_str()) {
            progress::run(
                id,
                "发布内核、启动镜像和启动项",
                "Publish kernel, initramfs and boot entry",
                || backend.publish_boot(&outcome, runtime),
            )?;
        }
        Ok(outcome)
    });
    match result {
        Ok(outcome) => {
            let state = if outcome
                .root
                .file_name()
                .is_some_and(|s| s == "prepared-root")
            {
                "pending-inheritance"
            } else {
                "complete"
            };
            status(attempt, id, state, None, Some(loaded))?;
            progress::message(
                id,
                "发布当前构建版本引用",
                "Publish current build version reference",
            );
            super::current::publish_current_build(&attempt.layout.root, id, &attempt.version)?;
            if state == "pending-inheritance" {
                progress::message(
                    id,
                    "构建产物已准备，等待启动阶段完成继承",
                    "Build artifacts prepared; inheritance will finish during boot",
                );
            }
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
    plugin: &LoadedPlugin,
    backend: &impl BuildBackend,
) -> Result<(Option<Cache>, Option<Cache>)> {
    let Some(version) = &attempt.previous else {
        return Ok((None, None));
    };
    let directory = attempt.layout.cache.join(version);
    let config = attempt.layout.versions.join(version).join("config");
    let mut software_identity = identity.clone();
    software_identity.options_sha256 = plugin.cache_configuration_hash(&attempt.config, true)?;
    let base = cache::load(
        backend,
        &directory,
        &config,
        version,
        "base-system",
        identity,
    )?;
    let packages = cache::load(
        backend,
        &directory,
        &config,
        version,
        "packages",
        &software_identity,
    )?;
    // The base identity must match even when only the software donor is available.
    // 即使仅有软件来源，基础身份也必须相符。
    Ok((base, packages))
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
