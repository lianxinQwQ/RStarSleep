//! Validate explicit plugin configuration contracts and map original names to environment variables.
//! 校验显式插件配置契约，并将原始名称映射为环境变量。
use super::{ConfigRequirements, Error, LoadedPlugin, Result};
use std::collections::{BTreeMap, BTreeSet};

/// Convert one original setting name to its uppercase RSTAR_ environment name.
/// 将一个原始设置名转换为大写 RSTAR_ 环境变量名。
/// # Arguments
/// - `name`: Case-sensitive [Distribution] key, excluding the selector and reserved context names.
///   大小写敏感的 [Distribution] 键，不包括选择字段及保留上下文名称。
pub(crate) fn environment_name(name: &str) -> Result<String> {
    let mut bytes = name.bytes();
    if !bytes.next().is_some_and(|byte| byte.is_ascii_alphabetic())
        || !bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    {
        return Err(Error::Invalid(format!("无效插件配置名称: {name:?}")));
    }
    let suffix = name.to_ascii_uppercase();
    if matches!(
        suffix.as_str(),
        "DISTRIBUTION"
            | "ROOT"
            | "CONFIG_DIR"
            | "SYSTEM_ID"
            | "PLUGIN_ROOT"
            | "PLUGIN_TMP"
            | "ARCH"
    ) {
        return Err(Error::Invalid(format!(
            "插件配置名称占用控制器保留变量: {name}"
        )));
    }
    Ok(format!("RSTAR_{suffix}"))
}

impl ConfigRequirements {
    /// Reject duplicate, overlapping, reserved, and case-colliding configuration declarations.
    /// 拒绝重复、交叉、保留名称和大小写冲突的配置声明。
    /// # Arguments
    /// - `self`: Explicit required and optional original setting names.
    ///   显式声明的必选和可选原始设置名。
    pub(super) fn validate(&self) -> Result<()> {
        let mut names = BTreeSet::new();
        for name in self.required.iter().chain(&self.optional) {
            if !names.insert(environment_name(name)?) {
                return Err(Error::Invalid(format!(
                    "插件配置声明重复或环境变量冲突: {name}"
                )));
            }
        }
        Ok(())
    }
}

impl LoadedPlugin {
    /// Validate every explicit setting and report all missing and undeclared keys before execution.
    /// 执行前校验所有显式设置，并报告全部缺失和未声明的键。
    /// # Arguments
    /// - `self`: Selected plugin with its configuration contract.
    ///   已选定并包含配置契约的插件。
    /// - `options`: Explicit [Distribution] settings excluding Distribution; defaults are not inputs.
    ///   [Distribution] 中排除 Distribution 的显式设置；默认值不算输入。
    /// # Errors
    /// Returns an error for a missing or empty required key, an undeclared key, or invalid values.
    /// 必选键缺失或为空、键未声明或值无效时返回错误。
    pub fn validate_configuration(&self, options: &BTreeMap<String, String>) -> Result<()> {
        self.metadata.config.validate()?;
        let missing: Vec<_> = self
            .metadata
            .config
            .required
            .iter()
            .filter(|name| {
                options
                    .get(*name)
                    .is_none_or(|value| value.trim().is_empty())
            })
            .cloned()
            .collect();
        let unknown: Vec<_> = options
            .keys()
            .filter(|name| {
                !self.metadata.config.required.contains(name)
                    && !self.metadata.config.optional.contains(name)
            })
            .cloned()
            .collect();
        if !missing.is_empty() || !unknown.is_empty() {
            return Err(Error::Invalid(format!(
                "插件 {} 的 [Distribution] 配置不满足声明：缺少或为空的必选配置 [{}]；未声明的配置 [{}]",
                self.metadata.id,
                missing.join(", "),
                unknown.join(", ")
            )));
        }
        for (name, value) in options {
            environment_name(name)?;
            if value.contains('\0') {
                return Err(Error::Invalid(format!("插件配置 {name} 包含 NUL")));
            }
        }
        Ok(())
    }

    /// Resolve accepted settings for Bash, using the effective architecture only if declared optional.
    /// 解析 Bash 接受的设置，仅在 Architecture 声明为可选时使用生效架构默认值。
    /// # Arguments
    /// - `self`: Selected plugin configuration contract.
    ///   已选定的插件配置契约。
    /// - `options`: Explicit frozen settings, checked before adding defaults.
    ///   添加默认值前先校验的显式冻结设置。
    /// - `architecture`: Effective target architecture used for plugin matching.
    ///   插件匹配使用的生效目标架构。
    pub(super) fn configuration_environment(
        &self,
        options: &BTreeMap<String, String>,
        architecture: &str,
    ) -> Result<BTreeMap<String, String>> {
        self.validate_configuration(options)?;
        let mut environment = BTreeMap::new();
        for (name, value) in options {
            environment.insert(environment_name(name)?, value.clone());
        }
        if self
            .metadata
            .config
            .optional
            .iter()
            .any(|name| name == "Architecture")
        {
            environment
                .entry("RSTAR_ARCHITECTURE".into())
                .or_insert_with(|| architecture.into());
        }
        Ok(environment)
    }
}
