//! Validate the same parameter names used by plugin environments and cache declarations.
//! 按插件环境与缓存声明使用的同一参数名称进行校验。
use super::{CacheRequirements, ConfigRequirements, Error, LoadedPlugin, Result};
use std::collections::{BTreeMap, BTreeSet};

/// Require a canonical RSTAR_ parameter name and protect controller-owned context.
/// 要求规范的 RSTAR_ 参数名，并保护控制器所属上下文。
pub(crate) fn environment_name(name: &str) -> Result<String> {
    let suffix = name
        .strip_prefix("RSTAR_")
        .ok_or_else(|| Error::Invalid(format!("插件参数须使用 RSTAR_ 名称: {name}")))?;
    if suffix.is_empty()
        || !suffix.as_bytes()[0].is_ascii_uppercase()
        || !suffix
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
    {
        return Err(Error::Invalid(format!("无效插件参数名称: {name:?}")));
    }
    if matches!(
        suffix,
        "ROOT"
            | "CONFIG_DIR"
            | "SYSTEM_ID"
            | "PLUGIN_ROOT"
            | "PLUGIN_TMP"
            | "ARCH"
            | "PARAMETERS_JSON"
    ) {
        return Err(Error::Invalid(format!(
            "插件参数占用控制器保留变量: {name}"
        )));
    }
    Ok(name.into())
}

impl ConfigRequirements {
    pub(super) fn validate(&self) -> Result<()> {
        let mut names = BTreeSet::new();
        for name in self.required.iter().chain(&self.optional) {
            if !names.insert(environment_name(name)?) {
                return Err(Error::Invalid(format!(
                    "插件配置声明重复或环境变量冲突: {name}"
                )));
            }
        }
        for (name, constraint) in &self.constraints {
            if !names.contains(name) {
                return Err(Error::Invalid(format!("约束字段未声明: {name}")));
            }
            if !matches!(
                constraint.kind.as_str(),
                "free-single" | "free-list" | "enum-single" | "enum-list" | "number"
            ) {
                return Err(Error::Invalid(format!("未知配置约束: {}", constraint.kind)));
            }
            if constraint.kind.starts_with("enum-") && constraint.values.is_empty() {
                return Err(Error::Invalid(format!("枚举约束没有候选值: {name}")));
            }
            if !constraint.kind.starts_with("enum-") && !constraint.values.is_empty() {
                return Err(Error::Invalid(format!("非枚举约束不能声明候选值: {name}")));
            }
        }
        Ok(())
    }
}

impl CacheRequirements {
    pub(super) fn validate(&self, config: &ConfigRequirements) -> Result<()> {
        for selectors in [&self.base_system, &self.software] {
            let mut names = BTreeSet::new();
            for selector in selectors {
                let name = selector
                    .strip_prefix("RSTAR_*")
                    .map(|suffix| format!("RSTAR_{suffix}"))
                    .unwrap_or_else(|| selector.clone());
                environment_name(&name)?;
                if !names.insert(name.clone()) {
                    return Err(Error::Invalid(format!("缓存检测字段重复: {selector}")));
                }
                let fields = config.required.iter().chain(&config.optional);
                let declared = if selector.starts_with("RSTAR_*") {
                    // Explicit section/parameter selector; unlike a section selector it never expands.
                    // 显式节/参数选择器；与节选择器不同，永不展开。
                    name.strip_prefix("RSTAR_")
                        .is_some_and(|suffix| suffix.contains('_'))
                        && fields.clone().any(|field| field == &name)
                } else {
                    fields
                        .clone()
                        .any(|field| field == &name || field.starts_with(&format!("{name}_")))
                };
                if !declared {
                    return Err(Error::Invalid(format!(
                        "缓存检测字段未在 config 中声明: {selector}"
                    )));
                }
            }
        }
        Ok(())
    }
}

impl LoadedPlugin {
    /// Validate all explicit effective parameters before any plugin execution.
    /// 任何插件运行之前校验全部显式生效参数。
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
                "插件 {} 的配置不满足声明：缺少或为空的必选配置 [{}]；未声明的配置 [{}]",
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

    /// Validate raw assignment cardinality and values before executing any operation.
    /// 运行任何操作前校验原始赋值的数量和值。
    /// # Arguments
    /// - `self`: Selected plugin. 所选插件。
    /// - `parameters`: Merged frozen INI assignments. 合并后的冻结 INI 赋值。
    pub fn validate_parameter_lists(
        &self,
        parameters: &crate::config::PluginParameters,
    ) -> Result<()> {
        let mut plugin_values = parameters.values.clone();
        plugin_values.retain(|name, _| !name.starts_with("RSTAR_INHERITANCE_"));
        self.validate_configuration(&plugin_values)?;
        for (name, values) in &parameters.lists {
            let Some(rule) = self.metadata.config.constraints.get(name) else {
                continue;
            };
            if matches!(rule.kind.as_str(), "free-single" | "enum-single" | "number")
                && values.len() != 1
            {
                return Err(Error::Invalid(format!(
                    "配置 {name} 要求单项，实际 {} 项",
                    values.len()
                )));
            }
            if rule.kind.starts_with("enum-")
                && values.iter().any(|value| !rule.values.contains(value))
            {
                return Err(Error::Invalid(format!(
                    "配置 {name} 不在枚举候选值中: {values:?}"
                )));
            }
            if rule.kind == "number"
                && values.iter().any(|value| {
                    value.parse::<f64>().is_err() || !value.parse::<f64>().unwrap().is_finite()
                })
            {
                return Err(Error::Invalid(format!(
                    "配置 {name} 必须是有限数字: {values:?}"
                )));
            }
        }
        Ok(())
    }

    pub(super) fn configuration_environment(
        &self,
        options: &BTreeMap<String, String>,
        architecture: &str,
    ) -> Result<BTreeMap<String, String>> {
        self.validate_configuration(options)?;
        let mut environment = options.clone();
        if self
            .metadata
            .config
            .optional
            .iter()
            .any(|name| name == "RSTAR_DISTRIBUTION_ARCHITECTURE")
        {
            environment
                .entry("RSTAR_DISTRIBUTION_ARCHITECTURE".into())
                .or_insert_with(|| architecture.into());
        }
        Ok(environment)
    }

    /// Preserve the unified multi-system download operation's original Distribution environment.
    /// 保留统一多系统下载操作原来的 Distribution 环境。
    pub(super) fn download_environment(
        &self,
        context: Option<&crate::controller::BuildContext>,
        architecture: &str,
    ) -> Result<BTreeMap<String, String>> {
        let mut environment = BTreeMap::new();
        if let Some(context) = context {
            for (name, value) in &context.distribution_options {
                let name = format!("RSTAR_{}", name.to_ascii_uppercase());
                environment_name(&name)?;
                environment.insert(name, value.clone());
            }
        }
        environment
            .entry("RSTAR_ARCHITECTURE".into())
            .or_insert_with(|| architecture.into());
        Ok(environment)
    }
}

#[cfg(test)]
mod constraint_tests {

    /// Reject duplicated singles, unsupported enums and non-finite numbers before work.
    /// 在工作前拒绝重复单项、无效枚举和非有限数字。
    /// # Arguments
    /// No parameters. 无参数。
    #[test]
    fn constraints_validate_original_lists() {
        let mut plugin = super::super::runner::tests::fixture(&[], &[]);
        plugin.metadata.config.optional.push("RSTAR_SETTING".into());
        let mut parameters = crate::config::PluginParameters::default();
        parameters
            .values
            .insert("RSTAR_SETTING".into(), "one\ntwo".into());
        for (kind, candidates, input, accepted) in [
            ("free-single", vec![], vec!["one", "two"], false),
            ("free-list", vec![], vec!["one", "two"], true),
            ("enum-single", vec!["one"], vec!["two"], false),
            ("enum-list", vec!["one", "two"], vec!["one", "two"], true),
            ("number", vec![], vec!["NaN"], false),
            ("number", vec![], vec!["1.5"], true),
        ] {
            plugin.metadata.config.constraints.insert(
                "RSTAR_SETTING".into(),
                super::super::model::ValueConstraint {
                    kind: kind.into(),
                    values: candidates.into_iter().map(String::from).collect(),
                },
            );
            parameters.lists.insert(
                "RSTAR_SETTING".into(),
                input.into_iter().map(String::from).collect(),
            );
            assert_eq!(
                plugin.validate_parameter_lists(&parameters).is_ok(),
                accepted,
                "{kind}"
            );
        }
    }
}
