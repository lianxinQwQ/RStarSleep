//! Run ordered build steps, each with its own cached and uncached actions.
//! 按顺序执行构建步骤，每步分别处理有缓存和无缓存动作。
use super::{
    BuildBackend, BuildContext, BuildOutcome, Result,
    cache::{self, Cache, Identity, Record},
    frozen::config_hash,
    identity,
    plugins::LoadedPlugin,
    progress,
    records::Attempt,
};
use crate::{
    base_system,
    config::BuildConfig,
    software::{PackageSession, SoftwareState},
};
use std::path::{Path, PathBuf};
/// Shared immutable inputs for the independently committed stage workflows.
/// 独立提交阶段流程共享的不可变输入。
pub(super) struct Stages<'a, B: BuildBackend> {
    /// Required CoW storage and checked execution.
    /// 必需 CoW 存储和经检查执行。
    pub(super) backend: &'a B,
    /// One loaded plugin session for every stage.
    /// 各阶段共用的已加载插件会话。
    pub(super) plugin: &'a LoadedPlugin,
    /// Frozen system identity, architecture, and configuration.
    /// 冻结系统标识、架构和配置。
    pub(super) context: &'a BuildContext,
    /// Locked allocated build attempt.
    /// 已锁定并分配的构建尝试。
    pub(super) attempt: &'a Attempt,
    /// Frozen configuration directory used throughout this build.
    /// 此次构建始终使用的冻结配置目录。
    pub(super) config: &'a BuildConfig,
    /// Compatibility requirements of this completed stage.
    /// 此完成阶段的兼容性要求。
    pub(super) identity: &'a Identity,
    pub(super) software_identity: &'a Identity,
    pub(super) package_plan: Option<&'a crate::software::PackagePlan>,
    /// Independent owned temporary build directory.
    /// 独立所属构建临时目录。
    pub(super) scratch: &'a Path,
    /// Whether to prepare main-system initramfs integration.
    /// 是否准备主系统 initramfs 集成。
    pub(super) prepare_boot_inheritance: bool,
}

impl<B: BuildBackend> Stages<'_, B> {
    /// Run base-system, package, and script steps before publishing their completed output.
    /// 依次执行基础系统、软件包和脚本步骤，再发布已完成输出。
    ///
    /// # Arguments
    ///
    /// - `self`: Shared inputs for every step.
    ///   所有步骤共享的输入。
    /// - `base`: This base step's prior cache, if available.
    ///   本基础步骤的过往缓存，若可用。
    /// - `packages`: This package step's prior cache, if available.
    ///   本软件包步骤的过往缓存，若可用。
    /// - `owned`: Scratch roots to clean after the attempt.
    ///   此次尝试后需要清理的临时根。
    pub(super) fn run(
        &self,
        base: Option<Cache>,
        packages: Option<Cache>,
        owned: &mut Vec<PathBuf>,
    ) -> Result<BuildOutcome> {
        let id = &self.context.system_id;
        let base = progress::run(id, "基础系统（1/4）", "Base system (1/4)", || {
            self.base_step(base.as_ref(), packages.as_ref(), owned)
        })?;
        let packages = progress::run(id, "软件包（2/4）", "Software packages (2/4)", || {
            self.packages_step(packages.as_ref(), &base, owned)
        })?;
        let scripts = progress::run(
            id,
            "软件维护后处理和用户脚本（3/4）",
            "Post-maintenance and user scripts (3/4)",
            || self.script_step(&packages, owned),
        )?;
        progress::run(
            id,
            "继承和版本发布（4/4）",
            "Inheritance and version publication (4/4)",
            || self.inheritance_step(&scripts, owned),
        )
    }

    /// Create or restore this system's own base, then always maintain it before committing.
    /// 创建或恢复此系统自己的基础根，然后始终先维护再提交。
    ///
    /// # Arguments
    ///
    /// - `self`: Shared step inputs.
    ///   共享步骤输入。
    /// - `cached`: This step's own completed prior cache.
    ///   此步骤自身已完成的过往缓存。
    /// - `packages`: Later cache optionally supplying this system's existing identity.
    ///   可提供此系统已有身份的后续缓存。
    /// - `owned`: Scratch roots reserved for cleanup.
    ///   为清理预留的临时根。
    fn base_step(
        &self,
        cached: Option<&Cache>,
        packages: Option<&Cache>,
        owned: &mut Vec<PathBuf>,
    ) -> Result<Cache> {
        let root = self.scratch.join("base-root");
        owned.push(root.clone());
        if let Some(cached) = cached {
            progress::message(
                &self.context.system_id,
                format!(
                    "复用基础系统缓存 {}，创建可写工作副本",
                    cached.record.version
                ),
                format!(
                    "Reuse base system cache {}; create a writable working copy",
                    cached.record.version
                ),
            );
            self.backend
                .snapshot_recursive(&cached.root, &root, false)?;
        } else {
            progress::message(
                &self.context.system_id,
                "无可用基础缓存，创建新基础系统",
                "No reusable base cache; create a fresh base system",
            );
            if self.plugin.metadata.api_version != 3 {
                return Err(super::Error::Invalid(
                    "无基础缓存的系统创建需要 API 3 的 system_init 操作".into(),
                ));
            }
            self.backend.create_subvolume(&root)?;
            base_system::install(self.backend, self.plugin, &root, self.context)?;
            let previous_root = packages.map(|cache| cache.root.clone()).or_else(|| {
                self.attempt
                    .previous
                    .as_ref()
                    .map(|version| self.attempt.layout.versions.join(version).join("root"))
            });
            let machine_id =
                identity::machine_id(&self.attempt.layout.root, previous_root.as_deref())?;
            self.backend.invoke(
                self.plugin,
                "system_init",
                &[machine_id],
                &root,
                self.context,
            )?;
        }
        // Unified maintenance runs where configured targets are applied. A fresh
        // base is already updated by base_install; legacy plugins still update here.
        // 统一维护在应用配置目标的层运行；新基础已由 base_install 更新，旧插件仍在此维护。
        let session = PackageSession {
            backend: self.backend,
            plugin: self.plugin,
            root: &root,
            context: self.context,
        };
        let software = if self.plugin.metadata.package_maintenance {
            if cached.is_some() && self.config.packages.is_empty() {
                session.apply_plan(
                    &[],
                    self.package_plan
                        .ok_or_else(|| super::Error::Invalid("软件维护缺少目标计划".into()))?,
                )?
            } else {
                session.capture(Vec::new())?
            }
        } else {
            base_system::maintain(self.backend, self.plugin, &root, self.context)?;
            session.capture(Vec::new())?
        };
        self.commit("base-system", &root, software, cached)
    }

    /// Install from the preceding step's output, or maintain this step's own restored cache.
    /// 从前一步输出首次安装，或维护此步骤自身恢复的缓存。
    ///
    /// # Arguments
    ///
    /// - `self`: Shared step inputs.
    ///   共享步骤输入。
    /// - `cached`: This package step's completed prior cache.
    ///   本软件包步骤已完成的过往缓存。
    /// - `previous_step`: Base output committed earlier in this attempt.
    ///   本次尝试中先前已提交的基础输出。
    /// - `owned`: Scratch roots reserved for cleanup.
    ///   为清理预留的临时根。
    fn packages_step(
        &self,
        cached: Option<&Cache>,
        previous_step: &Cache,
        owned: &mut Vec<PathBuf>,
    ) -> Result<Cache> {
        let root = self.scratch.join("root");
        owned.push(root.clone());
        let cached = if self.plugin.metadata.package_maintenance && self.config.packages.is_empty()
        {
            None
        } else {
            cached
        };
        let input = cached.unwrap_or(previous_step);
        if self.plugin.metadata.package_maintenance && self.config.packages.is_empty() {
            progress::message(
                &self.context.system_id,
                "未配置软件目标，复用本次基础阶段输出",
                "No package targets configured; use this build's base stage output",
            );
        } else if let Some(cached) = cached {
            progress::message(
                &self.context.system_id,
                format!(
                    "复用软件包缓存 {}，更新并协调当前软件需求",
                    cached.record.version
                ),
                format!(
                    "Reuse package cache {}; update and reconcile current requirements",
                    cached.record.version
                ),
            );
        } else {
            progress::message(
                &self.context.system_id,
                "从基础系统快照安装配置的软件",
                "Install configured software from the base system snapshot",
            );
        }
        self.backend.snapshot_recursive(&input.root, &root, false)?;
        let session = PackageSession {
            backend: self.backend,
            plugin: self.plugin,
            root: &root,
            context: self.context,
        };
        let software = if self.plugin.metadata.package_maintenance {
            if self.config.packages.is_empty() {
                previous_step.record.software.clone()
            } else {
                session.apply_plan(
                    &self.config.packages,
                    self.package_plan
                        .ok_or_else(|| super::Error::Invalid("软件维护缺少目标计划".into()))?,
                )?
            }
        } else if let Some(cached) = cached {
            session.maintain(&self.config.packages, &cached.record.software)?
        } else {
            session.install(&self.config.packages)?
        };
        self.commit("packages", &root, software, cached)
    }

    /// Reapply plugin behavior and scripts on fresh software output without prior effects.
    /// 在全新软件输出上重新应用插件后处理及脚本，避免继承先前执行效果。
    /// # Arguments
    /// - `self`: Shared stage inputs.
    ///   共享阶段输入。
    /// - `packages`: Completed software stage.
    ///   已完成软件阶段。
    /// - `owned`: Scratch roots to clean on success or failure.
    ///   成功或失败时需要清理的临时根。
    fn script_step(&self, packages: &Cache, owned: &mut Vec<PathBuf>) -> Result<Cache> {
        let root = self.scratch.join("script-root");
        owned.push(root.clone());
        self.backend
            .snapshot_recursive(&packages.root, &root, false)?;
        if self.plugin.metadata.post_maintenance {
            self.backend
                .invoke(self.plugin, "post_maintenance", &[], &root, self.context)?;
        }
        let applied = super::scripts::apply(self.backend, &root, self.context)?;
        let software = if applied || self.plugin.metadata.post_maintenance {
            let session = PackageSession {
                backend: self.backend,
                plugin: self.plugin,
                root: &root,
                context: self.context,
            };
            session.capture(packages.record.software.requested.clone())?
        } else {
            packages.record.software.clone()
        };
        if self.plugin.metadata.query_package_files {
            progress::message(
                &self.context.system_id,
                "生成软件包文件归属清单",
                "Generate the package file ownership inventory",
            );
            let bytes = self
                .backend
                .query_package_files(self.plugin, &root, self.context)?;
            let records = crate::subsystem::package_files::parse(&bytes)
                .map_err(|e| super::Error::Invalid(e.to_string()))?;
            let canonical = records
                .into_iter()
                .map(|(owner, path)| format!("{owner}\t{path}\n"))
                .collect::<String>();
            crate::subsystem::atomic_write(
                &self.attempt.directory.join("application-files.tsv"),
                canonical.as_bytes(),
                0o600,
            )
            .map_err(|e| super::Error::Invalid(e.to_string()))?;
        }
        self.commit("script", &root, software, None)
    }

    /// Reapply inheritance from the running system and explicitly publish directory subvolumes.
    /// 重新从运行系统应用继承，并显式发布目录子卷。
    /// # Arguments
    /// - `self`: Frozen stage inputs. 冻结阶段输入。
    /// - `scripts`: Completed script output. 完成的脚本输出。
    /// - `owned`: Work roots reserved for cleanup. 为清理预留的工作根。
    fn inheritance_step(&self, scripts: &Cache, owned: &mut Vec<PathBuf>) -> Result<BuildOutcome> {
        let input = crate::config::read_plugin_parameters_at(&self.context.config)?;
        let config = crate::config::InheritanceConfig::read(&input)?;
        if config.copy_on_write.is_empty()
            && config.mounts.is_empty()
            && !self
                .context
                .parameters
                .contains_key("RSTAR_CONTENT_FSTABLINES")
        {
            progress::message(
                &self.context.system_id,
                "未配置继承，直接发布脚本阶段产物",
                "No inheritance configured; publish the script stage output",
            );
            return self.publish(scripts);
        }
        progress::message(
            &self.context.system_id,
            "创建继承阶段的可写工作副本",
            "Create a writable working copy for inheritance",
        );
        let root = self.scratch.join("inherit-root");
        owned.push(root.clone());
        self.backend
            .snapshot_recursive(&scripts.root, &root, false)?;
        let inherited = super::inheritance::apply(
            self.backend,
            self.plugin,
            self.context,
            &root,
            self.scratch,
            &self.attempt.layout.root,
            owned,
        )?;
        if config.timing == crate::config::InheritanceTiming::Initramfs
            && !config.copy_on_write.is_empty()
        {
            if self.prepare_boot_inheritance {
                progress::message(
                    &self.context.system_id,
                    "准备启动阶段继承的 initramfs 集成",
                    "Prepare initramfs integration for boot-time inheritance",
                );
                self.backend.prepare_inheritance_initramfs(&root)?;
                self.backend.invoke(
                    self.plugin,
                    "prepare_inheritance_initramfs",
                    &[],
                    &root,
                    self.context,
                )?;
            }
            let prepared = self.attempt.directory.join("prepared-root");
            progress::message(
                &self.context.system_id,
                "保存待继承的只读快照和启动阶段任务",
                "Save the read-only prepared snapshot and boot-time inheritance task",
            );
            self.backend.snapshot_recursive(&root, &prepared, true)?;
            super::deferred::prepare(self.backend, self.context, &config, &prepared)?;
            return Ok(BuildOutcome {
                system_id: self.context.system_id.clone(),
                version: self.attempt.version.clone(),
                root: prepared,
            });
        }
        let mut inherited_context = self.context.clone();
        inherited_context.inherited_mounts = inherited.mounts;
        let session = PackageSession {
            backend: self.backend,
            plugin: self.plugin,
            root: &root,
            context: &inherited_context,
        };
        let software = session.capture(scripts.record.software.requested.clone())?;
        let record = self.record("inherit", software, None)?;
        let directory = self.attempt.layout.cache.join(&self.attempt.version);
        let cache = directory.join("inherit");
        if super::fs::exists(&cache)? {
            return Err(super::Error::Invalid("不覆盖已有继承缓存".into()));
        }
        progress::run(
            &self.context.system_id,
            "保存继承阶段只读缓存",
            "Save the read-only inheritance cache",
            || {
                super::inheritance::snapshot_tree(self.backend, &root, &cache, true)?;
                super::fs::write(&directory.join("inherit.toml"), &record)
            },
        )?;
        let published = self.attempt.directory.join("root");
        progress::run(
            &self.context.system_id,
            "发布最终只读系统根",
            "Publish the final read-only system root",
            || super::inheritance::snapshot_tree(self.backend, &cache, &published, true),
        )?;
        Ok(BuildOutcome {
            system_id: self.context.system_id.clone(),
            version: self.attempt.version.clone(),
            root: published,
        })
    }

    /// Commit one successful step before allowing the next step to run.
    /// 允许下一步骤运行前提交一个成功步骤。
    ///
    /// # Arguments
    ///
    /// - `self`: Shared record and storage inputs.
    ///   共享记录和存储输入。
    /// - `stage`: Step's stable cache identifier.
    ///   步骤的稳定缓存标识。
    /// - `root`: Fully completed root to snapshot.
    ///   待建立快照的完整完成根。
    /// - `software`: Validated installed inventory and targets.
    ///   经校验的已安装清单和目标。
    /// - `source`: Actual prior cache donor, absent for first installation.
    ///   实际过往缓存来源，首次安装时省略。
    fn commit(
        &self,
        stage: &str,
        root: &Path,
        software: SoftwareState,
        source: Option<&Cache>,
    ) -> Result<Cache> {
        let record = self.record(stage, software, source)?;
        let directory = self.attempt.layout.cache.join(&self.attempt.version);
        progress::run(
            &self.context.system_id,
            format!("保存 {stage} 阶段的只读缓存"),
            format!("Save the read-only {stage} stage cache"),
            || cache::commit(self.backend, root, &directory, record),
        )
    }

    /// Publish the last committed step as the version's read-only root.
    /// 将最后已提交步骤发布为版本只读根。
    ///
    /// # Arguments
    ///
    /// - `self`: Current version and CoW backend.
    ///   当前版本和 CoW 后端。
    /// - `completed`: Last successfully committed step.
    ///   最后成功提交的步骤。
    fn publish(&self, completed: &Cache) -> Result<BuildOutcome> {
        let root = self.attempt.directory.join("root");
        progress::run(
            &self.context.system_id,
            "发布最终只读系统根",
            "Publish the final read-only system root",
            || {
                self.backend
                    .snapshot_recursive(&completed.root, &root, true)?;
                self.backend.check_subvolume(&root, true)
            },
        )?;
        Ok(BuildOutcome {
            system_id: self.context.system_id.clone(),
            version: self.attempt.version.clone(),
            root,
        })
    }

    /// Create completion metadata using frozen hashes and exact plugin provenance.
    /// 使用冻结摘要和准确插件来源创建完成元数据。
    /// # Arguments
    ///
    /// - `self`: Immutable stage inputs.
    ///   不可变阶段输入。
    /// - `stage`: Completed stage name.
    ///   已完成阶段名称。
    /// - `software`: Actual inventory and targets.
    ///   实际清单和目标。
    /// - `source`: Actual donor cache, absent for a fresh root.
    ///   实际来源缓存，全新根则省略。
    fn record(
        &self,
        stage: &str,
        software: SoftwareState,
        source: Option<&Cache>,
    ) -> Result<Record> {
        Ok(Record {
            schema_version: 1,
            stage: stage.into(),
            version: self.attempt.version.clone(),
            source_version: source.map(|cache| cache.record.version.clone()),
            origin_version: self.attempt.version.clone(),
            plugin_version: self.plugin.metadata.version.clone(),
            plugin_sha256: self.plugin.archive_sha256.clone(),
            identity: if stage == "base-system" {
                self.identity.clone()
            } else {
                self.software_identity.clone()
            },
            config_sha256: config_hash(&self.attempt.config)?,
            software,
        })
    }
}
