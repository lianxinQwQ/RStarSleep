//! First installation and maintenance with dependency-preserving orphan cleanup.
//! 首次安装和保留必要依赖的维护及孤儿清理。
use super::{PackageSession, SoftwareState, model::names};
use crate::controller::{BuildBackend, Error, Result, progress};
use std::collections::BTreeSet;

impl<B: BuildBackend> PackageSession<'_, B> {
    /// Let the distribution reconcile its complete plan in one operation.
    /// 由发行版插件在单个操作内协调完整计划。
    pub fn apply_plan(
        &self,
        targets: &[String],
        plan: &super::PackagePlan,
    ) -> Result<SoftwareState> {
        let plan = super::PackagePlan::parse(
            &serde_json::to_string(plan).map_err(|e| Error::Invalid(e.to_string()))?,
        )?;
        for target in targets {
            super::model::valid_target(target)?;
        }
        let mut arguments = vec!["--plan".into()];
        arguments.extend(plan.packages.iter().cloned());
        arguments.push("--targets".into());
        arguments.extend(targets.iter().cloned());
        let output = self.call("maintain_packages", &arguments)?;
        let state: SoftwareState = serde_json::from_str(&output).map_err(|e| {
            Error::Invalid(format!(
                "软件维护返回的清单无效 / Software maintenance returned an invalid manifest: {e}"
            ))
        })?;
        state.validate()?;
        let actual: BTreeSet<_> = state.installed.iter().map(|p| p.name.as_str()).collect();
        let expected: BTreeSet<_> = plan.packages.iter().map(String::as_str).collect();
        if actual != expected {
            return Err(Error::Invalid("软件维护后的包清单与目标计划不符 / Package manifest after software maintenance does not match the target plan".into()));
        }
        let protected = self.resolve(&self.plugin.metadata.protected_packages)?;
        let mut requested = if targets.is_empty() {
            BTreeSet::new()
        } else {
            let resolved = super::model::names(&self.call("resolve_packages", targets)?)?;
            if resolved.is_empty() {
                return Err(Error::Invalid("软件维护后配置目标解析为空 / Configured targets resolve to an empty set after software maintenance".into()));
            }
            resolved
        };
        requested.retain(|name| !protected.contains(name));
        if requested != state.requested.iter().cloned().collect() {
            return Err(Error::Invalid("软件维护返回的目标与当前配置不符 / Targets returned by software maintenance do not match the current configuration".into()));
        }
        for name in protected {
            if !state.installed.iter().any(|p| p.name == name && p.explicit) {
                return Err(Error::Invalid("软件维护后缺少显式基础目标 / Explicit base target missing after software maintenance".into()));
            }
        }
        Ok(state)
    }

    /// Install configured software without repeating the base-system update.
    /// 安装配置软件，不重复基础系统更新。
    /// # Arguments
    ///
    /// - `self`: Stage execution environment.
    ///   阶段执行环境。
    /// - `targets`: Current frozen package table.
    ///   当前冻结包表。
    /// # Errors
    ///
    /// Returns query, installation, reason, or cleanup failures.
    /// 返回查询、安装、安装原因或清理错误。
    pub fn install(&self, targets: &[String]) -> Result<SoftwareState> {
        self.reconcile(targets, &SoftwareState::default(), false)
    }
    /// Update once and reconcile current targets against the source cache's targets.
    /// 更新一次，再协调当前目标与来源缓存目标。
    /// # Arguments
    ///
    /// - `self`: Stage execution environment.
    ///   阶段执行环境。
    /// - `targets`: Current frozen package table.
    ///   当前冻结包表。
    /// - `previous`: Actual source cache state.
    ///   实际来源缓存状态。
    /// # Errors
    ///
    /// Returns update, query, installation, reason, or cleanup failures.
    /// 返回更新、查询、安装、安装原因或清理错误。
    pub fn maintain(&self, targets: &[String], previous: &SoftwareState) -> Result<SoftwareState> {
        previous.validate()?;
        self.call("update", &[])?;
        self.reconcile(targets, previous, true)
    }
    /// Retain desired explicit roots, demote removed roots, then clean only real orphans.
    /// 保留所需显式根、降低已移除根的安装原因，再清理真正孤儿。
    /// # Arguments
    ///
    /// - `self`: Stage execution environment.
    ///   阶段执行环境。
    /// - `targets`: Current configured roots.
    ///   当前配置根。
    /// - `previous`: Prior managed roots.
    ///   先前管理根。
    /// - `maintenance`: Whether a system update may have introduced explicit replacement names.
    ///   系统更新是否可能引入显式安装的替代包名。
    fn reconcile(
        &self,
        targets: &[String],
        previous: &SoftwareState,
        maintenance: bool,
    ) -> Result<SoftwareState> {
        progress::message(
            &self.context.system_id,
            "解析软件包、软件组及受保护的基础目标",
            "Resolve packages, package groups and protected base targets",
        );
        let requested = self.resolve(targets)?;
        let protected = self.resolve(&self.plugin.metadata.protected_packages)?;
        let retained: BTreeSet<_> = requested.union(&protected).cloned().collect();
        progress::message(
            &self.context.system_id,
            "核对已安装软件和当前软件需求",
            "Compare installed packages with current requirements",
        );
        let actual = self.inventory()?;
        // Identify replacement roots from the post-update database before installation adds packages.
        // 安装增添软件前，从更新后的数据库识别替代根。
        let mut demotion_roots = BTreeSet::new();
        if maintenance {
            let previous_explicit: BTreeSet<_> = previous
                .installed
                .iter()
                .filter(|package| package.explicit)
                .map(|package| &package.name)
                .collect();
            demotion_roots.extend(
                actual
                    .iter()
                    .filter(|(name, explicit)| **explicit && !previous_explicit.contains(name))
                    .map(|(name, _)| name.clone()),
            );
        }
        demotion_roots.extend(previous.requested.iter().cloned());
        let mut missing: Vec<_> = retained
            .iter()
            .filter(|name| !actual.contains_key(*name))
            .cloned()
            .collect();
        for target in targets.iter().filter(|target| target.contains('/')) {
            let resolved = self.resolve(std::slice::from_ref(target))?;
            missing.retain(|name| !resolved.contains(name));
            missing.push(target.clone());
        }
        if !missing.is_empty() {
            progress::message(
                &self.context.system_id,
                format!("需要安装 {} 个软件包", missing.len()),
                format!("{} packages need installation", missing.len()),
            );
            self.call("install", &missing)?;
        } else {
            progress::message(
                &self.context.system_id,
                "所需软件均已安装，跳过安装",
                "All required packages are installed; skip installation",
            );
        }
        let mut explicit = vec!["explicit".into()];
        explicit.extend(retained.iter().cloned());
        if explicit.len() > 1 {
            self.call("mark_reason", &explicit)?;
        }
        let actual = self.inventory()?;
        let mut demote = vec!["dependency".into()];
        demote.extend(
            demotion_roots
                .into_iter()
                .filter(|name| !retained.contains(name) && actual.get(name) == Some(&true)),
        );
        if demote.len() > 1 {
            self.call("mark_reason", &demote)?;
        }
        progress::message(
            &self.context.system_id,
            "检查并清理不再需要的孤儿软件包",
            "Check and remove orphan packages that are no longer needed",
        );
        let orphans = names(&self.call("list_orphans", &[])?)?;
        let actual = self.inventory()?;
        if orphans
            .iter()
            .any(|name| retained.contains(name) || actual.get(name) != Some(&false))
        {
            return Err(Error::Invalid(
                "孤儿查询包含保留目标、显式包或未安装包 / Orphan query contains retained targets, explicitly installed packages, or packages that are not installed".into(),
            ));
        }
        if !orphans.is_empty() {
            self.call("remove_orphans", &orphans.into_iter().collect::<Vec<_>>())?;
        }
        progress::message(
            &self.context.system_id,
            "校验最终软件清单及安装原因",
            "Validate the final package inventory and installation reasons",
        );
        let result = self.capture(requested.into_iter().collect())?;
        if retained.iter().any(|name| {
            !result
                .installed
                .iter()
                .any(|p| &p.name == name && p.explicit)
        }) {
            return Err(Error::Invalid(
                "最终根缺少显式保留目标 / Final root lacks an explicit retained target".into(),
            ));
        }
        Ok(result)
    }
}
