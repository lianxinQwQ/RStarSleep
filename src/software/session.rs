//! Checked package queries bound to one writable root and frozen build inputs.
//! 绑定一个可写根和冻结构建输入的经检查包查询。
use super::model::{InstalledPackage, SoftwareState, inventory, names, valid_name};
use crate::controller::{BuildBackend, BuildContext, Error, Result, plugins::LoadedPlugin};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

/// Package operations sharing one execution environment throughout a stage.
/// 一个阶段中共享执行环境的包操作。
pub struct PackageSession<'a, B: BuildBackend> {
    /// Execution backend.
    /// 执行后端。
    pub backend: &'a B,
    /// Selected distribution plugin.
    /// 选定发行版插件。
    pub plugin: &'a LoadedPlugin,
    /// Writable stage root.
    /// 可写阶段根。
    pub root: &'a Path,
    /// Archived inputs.
    /// 留档输入。
    pub context: &'a BuildContext,
}

impl<B: BuildBackend> PackageSession<'_, B> {
    /// Invoke one operation with independently passed arguments.
    /// 使用独立传递的参数调用一个操作。
    /// # Arguments
    ///
    /// - `self`: Shared execution environment.
    ///   共享执行环境。
    /// - `operation`: Fixed API operation.
    ///   固定 API 操作。
    /// - `args`: Concrete task arguments.
    ///   具体任务参数。
    pub(super) fn call(&self, operation: &str, args: &[String]) -> Result<String> {
        self.backend
            .invoke(self.plugin, operation, args, self.root, self.context)
    }
    /// Resolve every unique target to a nonempty package set and merge overlapping targets.
    /// 将每个唯一目标解析为非空包集合，合并重叠目标。
    /// # Arguments
    ///
    /// - `self`: Shared execution environment.
    ///   共享执行环境。
    /// - `targets`: Configured package or group targets understood by the plugin.
    ///   插件支持的已配置包或组目标。
    pub(super) fn resolve(&self, targets: &[String]) -> Result<BTreeSet<String>> {
        let mut resolved = BTreeSet::new();
        for target in targets.iter().collect::<BTreeSet<_>>() {
            valid_name(target)?;
            let output = names(&self.call("resolve_packages", std::slice::from_ref(target))?)?;
            if output.is_empty() {
                return Err(Error::Invalid(format!(
                    "目标 {target} 必须解析为至少一个具体包"
                )));
            }
            resolved.extend(output);
        }
        Ok(resolved)
    }
    /// Read and validate the actual installed package database.
    /// 读取并校验实际安装数据库。
    /// # Arguments
    ///
    /// - `self`: Shared execution environment.
    ///   共享执行环境。
    pub(super) fn inventory(&self) -> Result<BTreeMap<String, bool>> {
        inventory(&self.call("query_packages", &[])?)
    }
    /// Capture the actual inventory and resolved targets for stage records.
    /// 捕获实际清单和已解析目标，供阶段记录使用。
    /// # Arguments
    ///
    /// - `self`: Shared execution environment.
    ///   共享执行环境。
    /// - `requested`: Resolved managed targets.
    ///   已解析管理目标。
    /// # Errors
    ///
    /// Returns query or completed-state validation failures.
    /// 返回查询或完成状态校验错误。
    pub fn capture(&self, requested: Vec<String>) -> Result<SoftwareState> {
        let state = SoftwareState {
            requested,
            installed: self
                .inventory()?
                .into_iter()
                .map(|(name, explicit)| InstalledPackage { name, explicit })
                .collect(),
        };
        state.validate()?;
        let protected = self.resolve(&self.plugin.metadata.protected_packages)?;
        if protected.iter().any(|name| {
            !state
                .installed
                .iter()
                .any(|package| &package.name == name && package.explicit)
        }) {
            return Err(Error::Invalid("阶段根缺少显式基础目标".into()));
        }
        Ok(state)
    }
}
