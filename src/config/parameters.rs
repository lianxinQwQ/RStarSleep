//! Unified names shared by configuration validation, plugin invocation, and cache selectors.
//! 配置校验、插件调用和缓存选择器共享的统一名称。
use super::{Error, Result};
use std::collections::{BTreeMap, BTreeSet};

/// Effective parameter values and explicitly present sections.
/// 生效参数值和显式存在的节。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PluginParameters {
    /// Values keyed by the canonical environment name.
    /// 以规范环境名称索引的值。
    pub values: BTreeMap<String, String>,
    /// Normalized sections, including explicitly empty sections.
    /// 规范化的节，包括显式空节。
    pub sections: BTreeSet<String>,
    /// Original section of each canonical field; empty means a bare parameter.
    /// 各规范字段的原始节；空值表示裸参数。
    pub field_sections: BTreeMap<String, String>,
}

/// Validate and normalize one section or parameter component.
/// 校验并规范化单个节名或参数名。
pub(super) fn component(name: &str) -> Result<String> {
    let mut bytes = name.bytes();
    if !bytes.next().is_some_and(|byte| byte.is_ascii_alphabetic())
        || !bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    {
        return Err(Error::Invalid(format!("无效插件参数名称: {name:?}")));
    }
    Ok(name.to_ascii_uppercase())
}

impl PluginParameters {
    pub(super) fn insert(&mut self, section: &str, key: &str, value: String) -> Result<()> {
        let key = component(key)?;
        let normalized_section = if section.is_empty() {
            String::new()
        } else {
            component(section)?
        };
        let suffix = if normalized_section.is_empty() {
            key
        } else {
            self.sections.insert(normalized_section.clone());
            format!("{normalized_section}_{key}")
        };
        let name = format!("RSTAR_{suffix}");
        if self.values.insert(name.clone(), value).is_some() {
            return Err(Error::Invalid(format!("插件参数环境变量冲突: {name}")));
        }
        self.field_sections.insert(name, normalized_section);
        Ok(())
    }
}
