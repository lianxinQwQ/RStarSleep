//! Controller-owned, explicitly selected account inheritance.
use super::{Error, PluginParameters, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AccountDriftPolicy {
    Silent,
    #[default]
    Warn,
    Error,
    Assess,
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize)]
pub struct AccountInheritanceConfig {
    pub users: Vec<String>,
    pub groups: Vec<String>,
    pub drift_policy: AccountDriftPolicy,
}

impl AccountInheritanceConfig {
    pub fn read(parameters: &PluginParameters) -> Result<Self> {
        let mut result = Self::default();
        for (key, values) in &parameters.lists {
            if !key.starts_with("RSTAR_ACCOUNTINHERITANCE_") {
                continue;
            }
            match key.as_str() {
                "RSTAR_ACCOUNTINHERITANCE_USERS" | "RSTAR_ACCOUNTINHERITANCE_GROUPS" => {
                    let target = if key.ends_with("_USERS") {
                        &mut result.users
                    } else {
                        &mut result.groups
                    };
                    for name in values
                        .iter()
                        .flat_map(|value| value.lines())
                        .filter(|name| !name.is_empty())
                    {
                        if name.trim() != name
                            || name.chars().any(|c| {
                                c.is_control()
                                    || c.is_whitespace()
                                    || matches!(c, ':' | ',' | '/' | '\\')
                            })
                        {
                            return Err(Error::Invalid(format!(
                                "无效账户名称: {name:?} / invalid account name: {name:?}"
                            )));
                        }
                        if !target.iter().any(|existing| existing == name) {
                            target.push(name.to_owned());
                        }
                    }
                }
                "RSTAR_ACCOUNTINHERITANCE_DRIFTPOLICY" => {
                    result.drift_policy = match values.as_slice() {
                        [value] => match value.as_str() {
                            "silent" => AccountDriftPolicy::Silent,
                            "warn" => AccountDriftPolicy::Warn,
                            "error" => AccountDriftPolicy::Error,
                            "assess" => AccountDriftPolicy::Assess,
                            _ => {
                                return Err(Error::Invalid(
                                    "DriftPolicy 必须为 silent、warn、error 或 assess / DriftPolicy must be silent, warn, error, or assess".into(),
                                ));
                            }
                        },
                        _ => return Err(Error::Invalid("DriftPolicy 必须只赋值一次 / DriftPolicy must be assigned exactly once".into())),
                    };
                }
                _ => {
                    return Err(Error::Invalid(format!(
                        "未知账户继承配置: {key} / unknown account inheritance setting: {key}"
                    )));
                }
            }
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn explicit_lists_and_strict_policy() {
        let mut p = PluginParameters::default();
        p.lists.insert(
            "RSTAR_ACCOUNTINHERITANCE_USERS".into(),
            vec!["alice\nbob".into(), "alice".into()],
        );
        assert_eq!(
            AccountInheritanceConfig::read(&p).unwrap().users,
            ["alice", "bob"]
        );
        p.lists.insert(
            "RSTAR_ACCOUNTINHERITANCE_DRIFTPOLICY".into(),
            vec!["assess".into()],
        );
        assert_eq!(
            AccountInheritanceConfig::read(&p).unwrap().drift_policy,
            AccountDriftPolicy::Assess
        );
        p.lists
            .get_mut("RSTAR_ACCOUNTINHERITANCE_DRIFTPOLICY")
            .unwrap()
            .push("error".into());
        assert!(AccountInheritanceConfig::read(&p).is_err());
    }
}
