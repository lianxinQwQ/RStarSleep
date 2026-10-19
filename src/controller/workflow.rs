//! Sequential version orchestration with latest-attempt cache selection.
//! 使用最新尝试缓存选择的顺序版本编排。
use super::{
    BuildBackend, BuildContext, BuildOutcome, Error, Result, SystemBackend,
    cache::{self, Cache, Identity},
    fs, logging,
    plugins::{self, LoadedPlugin},
    progress,
    records::{Attempt, Preflight, Status},
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
    let work = store.build_work_dir()?;
    check_workspace(store, &work)?;
    let backend = super::scoped_backend::ScopedBackend {
        backend,
        pool: store.persistent_path(),
        state: store.root().join("persistent-state"),
    };
    let mut outcomes = Vec::new();
    let mut failures = Vec::new();
    let mut pending = Vec::new();
    let system_count = systems.len();
    for (index, id) in systems.into_iter().enumerate() {
        let checked = (|| {
            let inputs = Preflight::begin(store, &id)?;
            let _log = logging::enter([inputs.log.clone()]);
            progress::run(
                &id,
                format!("准备配置与检查缓存（{}/{system_count}）", index + 1),
                format!(
                    "Prepare configuration and check caches ({}/{system_count})",
                    index + 1
                ),
                || check_system(store, &id, &runtime, inputs),
            )
        })();
        match checked {
            Ok(value) => pending.push(value),
            Err(error) if system.is_some() => return Err(error),
            Err(error) => failures.push(format!("{id}: {error}")),
        }
    }
    let collection_failures = collect_plans(store, &backend, &runtime, &mut pending);
    let mut rejected = std::collections::BTreeSet::new();
    for (index, error) in collection_failures {
        if system.is_some() {
            return Err(error);
        }
        failures.push(format!("{}: {error}", pending[index].context.system_id));
        rejected.insert(index);
    }
    let mut prepared = Vec::new();
    let mut executable_synced = false;
    for (index, inputs) in pending.into_iter().enumerate() {
        if rejected.contains(&index) {
            continue;
        }
        let id = inputs.context.system_id.clone();
        match prepare_system(store, inputs, &backend, &runtime, &mut executable_synced) {
            Ok(value) => prepared.push(value),
            Err(error) if system.is_some() => return Err(error),
            Err(error) => failures.push(format!("{id}: {error}")),
        }
    }
    let mut groups: std::collections::BTreeMap<DownloadGroup, Vec<usize>> =
        std::collections::BTreeMap::new();
    for (index, value) in prepared.iter().enumerate() {
        if value.plugin.metadata.predownload && !value.plugin.metadata.predownload_plans {
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
        let _log = logging::enter(
            indices
                .iter()
                .map(|&index| prepared[index].attempt.log.clone()),
        );
        let download = progress::run(
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
                    prepared[indices[0]].package_plan.is_none(),
                )
            },
        )
        .and_then(|()| {
            for &index in &indices {
                prepared[index].attempt.log.check()?;
            }
            Ok(())
        });
        if let Err(error) = download {
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
        let _log = logging::enter([value.attempt.log.clone()]);
        let built = progress::run(
            &value.context.system_id,
            format!("构建版本 {}", value.attempt.version),
            format!("Build version {}", value.attempt.version),
            || build_system(value, &work, &backend, &runtime),
        );
        let built = match value.attempt.log.check() {
            Ok(()) => built,
            Err(archive) => {
                let error = match built {
                    Ok(_) => archive,
                    Err(error) => Error::Invalid(format!("{error}; 安装日志归档失败: {archive}")),
                };
                Err(error)
            }
        };
        if let Err(error) = &built {
            if let Err(record_error) = status(
                &value.attempt,
                &value.context.system_id,
                "failed",
                Some(error.to_string()),
                Some(&value.plugin),
            ) {
                return Err(Error::Invalid(format!(
                    "{error}; 失败记录写入失败: {record_error}"
                )));
            }
        }
        match built {
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
    if let Some(error) = &error {
        logging::diagnostic(format!("[{id}] 构建失败 / Build failed: {error}"));
    }
    if state != "failed" {
        attempt.log.check()?;
    }
    attempt.status(&Status {
        schema_version: 1,
        version: &attempt.version,
        system_id: id,
        state,
        error,
        plugin_id: plugin.map(|p| p.metadata.id.as_str()),
        plugin_version: plugin.map(|p| p.metadata.version.as_str()),
        plugin_sha256: plugin.map(|p| p.archive_sha256.as_str()),
    })?;
    if state == "failed" {
        // Retain the failure's last diagnostics before returning even on an interrupted build.
        // 即使构建中断，也在返回前同步失败时的最后诊断。
        let _ = attempt.log.check();
    }
    Ok(())
}

/// Retain locked, frozen, validated inputs through preparation and version publication.
/// 在准备和版本发布期间保留已锁定、冻结及校验的输入。
struct Prepared {
    attempt: Attempt,
    plugin: LoadedPlugin,
    config: crate::config::BuildConfig,
    context: BuildContext,
    upgrade_packages: Vec<String>,
    package_plan: Option<crate::software::PackagePlan>,
    reuse_minutes: u64,
}

type DownloadGroup = (
    String,
    String,
    std::collections::BTreeMap<String, String>,
    crate::config::Environment,
);

struct Pending {
    inputs: Preflight,
    plugin: LoadedPlugin,
    config: crate::config::BuildConfig,
    context: BuildContext,
    package_plan: Option<crate::software::PackagePlan>,
    reuse_minutes: u64,
}

fn collect_plan(
    value: &Pending,
    root: &Path,
    backend: &impl BuildBackend,
    runtime: &crate::config::RuntimeConfig,
) -> Result<crate::software::PackagePlan> {
    let _log = logging::enter([value.inputs.log.clone()]);
    progress::run(
        &value.context.system_id,
        "收集软件组、基础包及运行依赖，检查启动要求",
        "Collect package groups, base packages and runtime dependencies; check boot requirements",
        || {
            let requirements = value
                .plugin
                .metadata
                .package_plan
                .as_ref()
                .expect("package-plan groups contain only capable plugins");
            let mut targets: std::collections::BTreeSet<_> =
                value.config.packages.iter().cloned().collect();
            targets.extend(requirements.base_packages.iter().cloned());
            let output = backend.invoke(
                &value.plugin,
                "collect_packages",
                &targets.into_iter().collect::<Vec<_>>(),
                root,
                &value.context,
            )?;
            let plan = crate::software::PackagePlan::parse(&output)?;
            let inheritance = crate::config::InheritanceConfig::read(&read_plugin_parameters_at(
                &value.context.config,
            )?)?;
            validate_boot_packages(
                &value.context.system_id,
                runtime,
                &inheritance,
                &value.plugin,
                &plan.packages,
            )?;
            Ok(plan)
        },
    )
}

/// Check frozen inputs before reserving a version or changing build caches.
/// 预留版本或修改构建缓存前检查冻结输入。
fn check_system(
    store: &ConfigStore,
    id: &str,
    runtime: &crate::config::RuntimeConfig,
    inputs: Preflight,
) -> Result<Pending> {
    progress::message(
        id,
        "冻结配置，检查插件、参数及启动要求",
        "Freeze configuration and check plugin, parameters and boot requirements",
    );
    let frozen = inputs.config.path();
    let config = read_build_config_at(frozen)?;
    let reuse_minutes = store.cache_reuse_minutes_at(frozen)?;
    let architecture = config
        .architecture
        .as_deref()
        .unwrap_or(std::env::consts::ARCH)
        .to_owned();
    let plugin = plugins::find(&store.plugins_path(), &config.system_type, &architecture)?;
    let distribution_options = read_distribution_options_at(frozen)?;
    let input = read_plugin_parameters_at(frozen)?;
    let environment = crate::config::read_environment_at(frozen)?;
    plugin.validate_parameter_lists(&input)?;
    let inheritance = crate::config::InheritanceConfig::read(&input)?;
    validate_boot_capabilities(id, runtime, &inheritance, &plugin)?;
    for warning in &plugin.discovery_warnings {
        logging::diagnostic(warning);
    }
    progress::message(
        id,
        format!(
            "配置检查通过，使用插件 {} {}，架构 {architecture}",
            plugin.metadata.id, plugin.metadata.version
        ),
        format!(
            "Configuration checks passed; using plugin {} {}, architecture {architecture}",
            plugin.metadata.id, plugin.metadata.version
        ),
    );
    let context = BuildContext {
        system_id: id.into(),
        architecture,
        distribution_options,
        parameters: input
            .values
            .into_iter()
            .filter(|(name, _)| {
                !name.starts_with("RSTAR_INHERITANCE_")
                    && !matches!(
                        name.as_str(),
                        "RSTAR_CONFIG_CACHEREUSEMINUTES" | "RSTAR_CACHE_REUSE_MINUTES"
                    )
            })
            .collect(),
        config: frozen.to_path_buf(),
        persistent_mounts: Vec::new(),
        parameter_lists: input.lists,
        environment,
        inherited_mounts: Vec::new(),
    };
    Ok(Pending {
        inputs,
        plugin,
        config,
        context,
        package_plan: None,
        reuse_minutes,
    })
}

/// Collect each system's closure separately inside the shared resolver environment.
/// 在共享解析环境内分别收集每个系统的闭包。
fn collect_plans(
    store: &ConfigStore,
    backend: &impl BuildBackend,
    runtime: &crate::config::RuntimeConfig,
    pending: &mut [Pending],
) -> Vec<(usize, Error)> {
    let mut groups: std::collections::BTreeMap<DownloadGroup, Vec<usize>> =
        std::collections::BTreeMap::new();
    for (index, value) in pending.iter().enumerate() {
        if value.plugin.metadata.package_plan.is_some()
            && (value.plugin.metadata.predownload
                || value.plugin.metadata.package_maintenance
                || runtime.main_system.as_deref() == Some(&value.context.system_id))
        {
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
    let mut failures = Vec::new();
    for indices in groups.values() {
        let _log = logging::enter(
            indices
                .iter()
                .map(|&index| pending[index].inputs.log.clone()),
        );
        let first = &pending[indices[0]];
        let result = super::predownload::with_environment(
            backend,
            store,
            &first.plugin,
            &first.context,
            true,
            |root, _environment| {
                if first.plugin.metadata.predownload && first.plugin.metadata.predownload_plans {
                    let systems: std::collections::BTreeMap<_, _> = indices
                        .iter()
                        .map(|&index| {
                            (
                                &pending[index].context.system_id,
                                &pending[index].config.packages,
                            )
                        })
                        .collect();
                    let request = serde_json::json!({"schema_version": 1, "systems": systems});
                    let output = backend.invoke(
                        &first.plugin,
                        "predownload",
                        &["--systems-json".into(), request.to_string()],
                        root,
                        _environment,
                    )?;
                    #[derive(serde::Deserialize)]
                    #[serde(deny_unknown_fields)]
                    struct Plans {
                        schema_version: u32,
                        systems: std::collections::BTreeMap<String, serde_json::Value>,
                    }
                    let mut response: Plans = serde_json::from_str(&output)
                        .map_err(|e| Error::Invalid(format!("软件下载计划返回无效: {e}")))?;
                    if response.schema_version != 1
                        || response
                            .systems
                            .keys()
                            .map(String::as_str)
                            .ne(systems.keys().map(|id| id.as_str()))
                    {
                        return Err(Error::Invalid("软件下载计划的版本或系统集合不符".into()));
                    }
                    let mut plans = Vec::new();
                    for &index in indices {
                        let value = &pending[index];
                        let result = (|| {
                            let plan = crate::software::PackagePlan::parse(
                                &response
                                    .systems
                                    .remove(&value.context.system_id)
                                    .unwrap()
                                    .to_string(),
                            )?;
                            let inheritance = crate::config::InheritanceConfig::read(
                                &read_plugin_parameters_at(&value.context.config)?,
                            )?;
                            validate_boot_packages(
                                &value.context.system_id,
                                runtime,
                                &inheritance,
                                &value.plugin,
                                &plan.packages,
                            )?;
                            Ok(plan)
                        })();
                        plans.push((index, result));
                    }
                    return Ok(plans);
                }
                let mut plans = Vec::with_capacity(indices.len());
                for &index in indices {
                    plans.push((index, collect_plan(&pending[index], root, backend, runtime)));
                }
                Ok(plans)
            },
        );
        match result {
            Ok(plans) => {
                for (index, result) in plans {
                    match result {
                        Ok(plan) => pending[index].package_plan = Some(plan),
                        Err(error) => failures.push((index, error)),
                    }
                }
            }
            Err(error) => {
                for &index in indices {
                    failures.push((
                        index,
                        Error::Invalid(format!("软件包收集环境失败: {error}")),
                    ));
                }
            }
        }
    }
    failures
}

/// Allocate target attempts and retire caches only after package collection passes.
/// 软件包收集检查通过后才分配目标构建版本及清理缓存。
fn prepare_system(
    store: &ConfigStore,
    pending: Pending,
    backend: &impl BuildBackend,
    runtime: &crate::config::RuntimeConfig,
    executable_synced: &mut bool,
) -> Result<Prepared> {
    let Pending {
        inputs,
        plugin,
        config,
        mut context,
        package_plan,
        reuse_minutes,
    } = pending;
    let _log = logging::enter([inputs.log.clone()]);
    let id = context.system_id.clone();
    let id = id.as_str();
    if !*executable_synced {
        super::boot::sync_executable(store.root())?;
        *executable_synced = true;
    }
    let attempt = Attempt::begin(inputs)?;
    let loaded = &plugin;
    context.config = attempt.config.clone();
    let result: Result<Vec<String>> = (|| {
        status(&attempt, id, "building", None, Some(loaded))?;
        if runtime.main_system.as_deref() == Some(id) {
            fs::write(
                &attempt.directory.join("boot-spec.toml"),
                loaded.metadata.boot.as_ref().unwrap(),
            )?;
        }
        if let Some(plan) = &package_plan {
            fs::write(&attempt.directory.join("package-plan.toml"), plan)?;
        }
        // Retain script donors while timed reuse is enabled; inherit always runs again.
        // 启用限时复用时保留 script 来源；inherit 始终重新执行。
        progress::message(
            id,
            "清理不复用的历史后续阶段缓存",
            "Clear historical later stage caches that are not reused",
        );
        cache::clear_from(
            backend,
            &attempt.layout.cache,
            if reuse_minutes > 0 {
                "inherit"
            } else {
                "script"
            },
        )?;
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
                && !loaded.metadata.package_maintenance
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
        attempt.log.check()?;
        Ok(upgrade_packages)
    })();
    match result {
        Ok(upgrade_packages) => Ok(Prepared {
            attempt,
            plugin,
            config,
            context,
            upgrade_packages,
            package_plan,
            reuse_minutes,
        }),
        Err(error) => {
            status(
                &attempt,
                id,
                "failed",
                Some(error.to_string()),
                Some(&plugin),
            )?;
            Err(error)
        }
    }
}

/// Select declarations from the system's role and actual deferred inheritance request.
/// 根据系统角色及实际的延后继承请求选择声明。
fn validate_boot_capabilities(
    id: &str,
    runtime: &crate::config::RuntimeConfig,
    inheritance: &crate::config::InheritanceConfig,
    plugin: &LoadedPlugin,
) -> Result<()> {
    if runtime.main_system.as_deref() != Some(id) {
        return Ok(());
    }
    if plugin.metadata.boot.is_none() {
        return Err(Error::Invalid(format!(
            "主系统 {id} 的插件 {} 未声明通用启动要求 [boot]",
            plugin.metadata.id
        )));
    }
    if !inheritance.copy_on_write_boot.is_empty() {
        if plugin.metadata.initramfs_inheritance.is_none() {
            return Err(Error::Invalid(format!(
                "主系统 {id} 启用启动时 CoW 继承，但插件 {} 未声明 [initramfs_inheritance] 要求",
                plugin.metadata.id
            )));
        }
        plugin.operation_path("prepare_inheritance_initramfs")?;
    }
    if plugin.metadata.package_plan.is_none() {
        return Err(Error::Invalid(format!(
            "主系统 {id} 的插件 {} 未声明 [package_plan]，无法收集目标软件包并检查启动要求",
            plugin.metadata.id
        )));
    }
    Ok(())
}

fn validate_boot_packages(
    id: &str,
    runtime: &crate::config::RuntimeConfig,
    inheritance: &crate::config::InheritanceConfig,
    plugin: &LoadedPlugin,
    packages: &[String],
) -> Result<()> {
    if runtime.main_system.as_deref() != Some(id) {
        return Ok(());
    }
    require_planned_packages(
        id,
        "通用启动",
        &plugin.metadata.boot.as_ref().unwrap().packages,
        packages,
    )?;
    if !inheritance.copy_on_write_boot.is_empty() {
        require_planned_packages(
            id,
            "启动时 CoW 继承",
            &plugin
                .metadata
                .initramfs_inheritance
                .as_ref()
                .unwrap()
                .packages,
            packages,
        )?;
    }
    Ok(())
}

fn require_planned_packages(
    id: &str,
    flow: &str,
    required: &[String],
    planned: &[String],
) -> Result<()> {
    let missing: Vec<_> = required
        .iter()
        .filter(|name| !planned.contains(name))
        .cloned()
        .collect();
    if !missing.is_empty() {
        return Err(Error::Invalid(format!(
            "系统 {id} 的{flow}要求未满足：展开软件组、基础包和运行依赖后的目标安装计划缺少 {}（由插件元数据声明）；请调整 Packages 后重新构建",
            missing.join(", ")
        )));
    }
    Ok(())
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
        let scripts = if prepared.reuse_minutes > 0 {
            if let Some(version) = &attempt.previous {
                cache::load(
                    backend,
                    &attempt.layout.cache.join(version),
                    &attempt.layout.versions.join(version).join("config"),
                    version,
                    "script",
                    &software_identity,
                )?
            } else {
                None
            }
        } else {
            None
        };
        let stages = Stages {
            backend,
            plugin: loaded,
            context,
            attempt,
            config,
            identity: &identity,
            software_identity: &software_identity,
            package_plan: prepared.package_plan.as_ref(),
            scratch: &scratch,
            prepare_boot_inheritance: runtime.main_system.as_deref() == Some(id.as_str()),
            reuse_minutes: prepared.reuse_minutes,
        };
        stages.run(base, packages, scripts, &mut owned)
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
        Err(error) => Err(error),
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
                backend.delete_subvolume_recursive(root)
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
