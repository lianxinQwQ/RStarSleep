//! Run ordered build steps, each with its own cached and uncached actions.
//! 按顺序执行构建步骤，每步分别处理有缓存和无缓存动作。
use super::{
    BuildBackend, BuildContext, BuildOutcome, Result,
    cache::{self, Cache, Identity, Record},
    frozen::config_hash,
    identity,
    plugins::LoadedPlugin,
    records::Attempt,
    templates,
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
    /// Independent owned temporary build directory.
    /// 独立所属构建临时目录。
    pub(super) scratch: &'a Path,
    /// Shared immutable templates, outside per-system caches and scratch space.
    /// 位于每系统缓存和临时空间之外的共享不可变模板。
    pub(super) templates: &'a Path,
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
        let base = self.base_step(base.as_ref(), packages.as_ref(), owned)?;
        let packages = self.packages_step(packages.as_ref(), &base, owned)?;
        let scripts = self.script_step(&packages, owned)?;
        self.publish(&scripts)
    }

    /// Install an uncached base, or maintain its cache unless a later step covers maintenance.
    /// 无缓存时安装基础系统；有缓存时维护，若后续步骤承担维护则跳过维护。
    ///
    /// # Arguments
    ///
    /// - `self`: Shared step inputs.
    ///   共享步骤输入。
    /// - `cached`: This step's own completed prior cache.
    ///   此步骤自身已完成的过往缓存。
    /// - `packages`: Later cache covering maintenance and optionally supplying an existing identity.
    ///   承担维护的后续缓存，也可提供此系统已有身份。
    /// - `owned`: Scratch roots reserved for cleanup.
    ///   为清理预留的临时根。
    fn base_step(
        &self,
        cached: Option<&Cache>,
        packages: Option<&Cache>,
        owned: &mut Vec<PathBuf>,
    ) -> Result<Cache> {
        let root = self.scratch.join("base-root");
        if let Some(cached) = cached {
            // Later package maintenance supplies the update; inherit this completed step unchanged.
            // 后续软件维护提供系统更新；原样继承本已完成步骤。
            if packages.is_some() {
                return self.commit(
                    "base-system",
                    &cached.root,
                    cached.record.software.clone(),
                    Some(cached),
                    true,
                );
            }
            owned.push(root.clone());
            self.backend.snapshot(&cached.root, &root, false)?;
            base_system::maintain(self.backend, self.plugin, &root, self.context)?;
        } else {
            owned.push(root.clone());
            let reused = templates::restore(
                self.backend,
                self.plugin,
                self.templates,
                &root,
                &self.context.architecture,
            )?;
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
            // A reused template may be old; the later cached package stage still covers updates.
            // 复用模板可能较旧；后续有缓存软件阶段仍负责覆盖更新。
            if reused && packages.is_none() {
                base_system::maintain(self.backend, self.plugin, &root, self.context)?;
            }
        }
        let session = PackageSession {
            backend: self.backend,
            plugin: self.plugin,
            root: &root,
            context: self.context,
        };
        self.commit(
            "base-system",
            &root,
            session.capture(Vec::new())?,
            cached,
            false,
        )
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
        let input = cached.unwrap_or(previous_step);
        self.backend.snapshot(&input.root, &root, false)?;
        let session = PackageSession {
            backend: self.backend,
            plugin: self.plugin,
            root: &root,
            context: self.context,
        };
        let software = if let Some(cached) = cached {
            session.maintain(&self.config.packages, &cached.record.software)?
        } else {
            session.install(&self.config.packages)?
        };
        self.commit("packages", &root, software, cached, false)
    }

    /// Reapply scripts on fresh software output so prior script effects are never inherited.
    /// 在全新软件输出上重新应用脚本，避免继承先前脚本效果。
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
        self.backend.snapshot(&packages.root, &root, false)?;
        let applied = super::scripts::apply(self.backend, &root, self.context)?;
        let software = if applied {
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
        self.commit("script", &root, software, None, false)
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
    /// - `inherited`: Whether the step inherited its cache without running actions.
    ///   步骤是否未经执行动作继承缓存。
    fn commit(
        &self,
        stage: &str,
        root: &Path,
        software: SoftwareState,
        source: Option<&Cache>,
        inherited: bool,
    ) -> Result<Cache> {
        let record = self.record(stage, software, source, inherited)?;
        let directory = self.attempt.layout.cache.join(&self.attempt.version);
        cache::commit(self.backend, root, &directory, record)
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
        self.backend.snapshot(&completed.root, &root, true)?;
        self.backend.check_subvolume(&root, true)?;
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
    /// - `inherited`: Whether content is inherited without mutation.
    ///   内容是否未经修改继承。
    fn record(
        &self,
        stage: &str,
        software: SoftwareState,
        source: Option<&Cache>,
        inherited: bool,
    ) -> Result<Record> {
        let origin = source.filter(|_| inherited).map(|cache| &cache.record);
        Ok(Record {
            schema_version: 1,
            stage: stage.into(),
            version: self.attempt.version.clone(),
            source_version: source.map(|cache| cache.record.version.clone()),
            origin_version: origin
                .map(|record| record.origin_version.clone())
                .unwrap_or_else(|| self.attempt.version.clone()),
            plugin_version: origin
                .map(|record| record.plugin_version.clone())
                .unwrap_or_else(|| self.plugin.metadata.version.clone()),
            plugin_sha256: origin
                .map(|record| record.plugin_sha256.clone())
                .unwrap_or_else(|| self.plugin.archive_sha256.clone()),
            identity: self.identity.clone(),
            config_sha256: config_hash(&self.attempt.config, false)?,
            software,
        })
    }
}
