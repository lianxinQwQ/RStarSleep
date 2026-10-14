//! Normalized concrete package names and installed inventories.
//! 规范化具体包名和已安装清单。
use crate::controller::{Error, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// Concrete target installation closure returned by package collection.
/// 软件包收集返回的具体目标安装闭包。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackagePlan {
    pub schema_version: u32,
    pub packages: Vec<String>,
}

impl PackagePlan {
    pub(crate) fn parse(text: &str) -> Result<Self> {
        let plan: Self = serde_json::from_str(text)
            .map_err(|e| Error::Invalid(format!("软件包计划查询结果无效: {e}")))?;
        if plan.schema_version != 1 {
            return Err(Error::Invalid("软件包计划仅支持格式版本 1".into()));
        }
        names(&plan.packages.join("\n"))?;
        for package in &plan.packages {
            valid_name(package)?;
        }
        Ok(plan)
    }
}

/// One concrete installed package and its normalized installation reason.
/// 一个具体已安装包及其规范化安装原因。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstalledPackage {
    /// Concrete package identity.
    /// 具体包标识。
    pub name: String,
    /// Whether the package is explicit.
    /// 包是否显式安装。
    pub explicit: bool,
}

/// Resolved managed targets and the actual inventory at a completed stage.
/// 完成阶段中的已解析管理目标和实际清单。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SoftwareState {
    /// Resolved configured targets, excluding separately protected base targets.
    /// 已解析配置目标，不包括单独受保护的基础目标。
    pub requested: Vec<String>,
    /// Actual installed database after reconciliation.
    /// 协调之后的实际安装数据库。
    pub installed: Vec<InstalledPackage>,
}

/// Validate a concrete name suitable for a package-manager argument.
/// 校验适合作为包管理器参数的具体名称。
/// # Arguments
///
/// - `name`: Concrete package name.
///   具体包名。
pub(crate) fn valid_name(name: &str) -> Result<()> {
    let mut chars = name.chars();
    if !chars
        .next()
        .is_some_and(|c| c.is_ascii_alphanumeric() || "@_+".contains(c))
        || !chars.all(|c| c.is_ascii_alphanumeric() || "@_.+-".contains(c))
    {
        return Err(Error::Invalid(format!("无效具体包名: {name:?}")));
    }
    Ok(())
}

/// Validate a configured package target, optionally qualified by its source.
pub(crate) fn valid_target(target: &str) -> Result<()> {
    if let Some((source, name)) = target.split_once('/') {
        valid_name(source)?;
        valid_name(name)
    } else {
        valid_name(target)
    }
}

/// Parse one concrete name per line and reject malformed or duplicate results.
/// 解析每行一个具体名称，拒绝格式错误或重复结果。
/// # Arguments
///
/// - `text`: Plugin query output.
///   插件查询输出。
pub(super) fn names(text: &str) -> Result<BTreeSet<String>> {
    let mut result = BTreeSet::new();
    for line in text.lines() {
        valid_name(line)?;
        if !result.insert(line.to_owned()) {
            return Err(Error::Invalid(format!("重复包查询结果: {line}")));
        }
    }
    Ok(result)
}

/// Parse the tab-delimited inventory with explicit/dependency reasons.
/// 解析制表符分隔的清单及显式/依赖原因。
/// # Arguments
///
/// - `text`: Plugin inventory output.
///   插件清单输出。
pub(super) fn inventory(text: &str) -> Result<BTreeMap<String, bool>> {
    let mut result = BTreeMap::new();
    for line in text.lines() {
        let (name, reason) = line
            .split_once('\t')
            .ok_or_else(|| Error::Invalid(format!("无效包清单行: {line:?}")))?;
        valid_name(name)?;
        let explicit = match reason {
            "explicit" => true,
            "dependency" => false,
            _ => return Err(Error::Invalid(format!("无效安装原因: {reason:?}"))),
        };
        if result.insert(name.into(), explicit).is_some() {
            return Err(Error::Invalid(format!("重复已安装包: {name}")));
        }
    }
    Ok(result)
}

impl SoftwareState {
    /// Validate completed-cache data before allowing it to influence maintenance.
    /// 完成缓存数据影响维护前先进行校验。
    /// # Arguments
    ///
    /// - `self`: Stored normalized targets and inventory.
    ///   已存储的规范化目标和清单。
    /// # Errors
    ///
    /// Returns errors for duplicate, malformed, missing, or demoted targets.
    /// 目标重复、格式错误、缺失或被降为依赖时返回错误。
    pub fn validate(&self) -> Result<()> {
        let requested = names(&self.requested.join("\n"))?;
        let mut installed = BTreeMap::new();
        for package in &self.installed {
            valid_name(&package.name)?;
            if installed
                .insert(package.name.clone(), package.explicit)
                .is_some()
            {
                return Err(Error::Invalid("缓存包清单重复".into()));
            }
        }
        if requested
            .iter()
            .any(|name| installed.get(name) != Some(&true))
        {
            return Err(Error::Invalid("缓存管理目标缺失或不是显式安装".into()));
        }
        Ok(())
    }
}
