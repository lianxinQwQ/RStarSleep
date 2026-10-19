//! Fingerprint only plugin-declared dependencies, retaining missing values and section membership.
//! 仅计算插件声明依赖的指纹，保留缺失值和节成员信息。
use super::{Error, LoadedPlugin, Result};
use sha2::{Digest, Sha256};
use std::{collections::BTreeSet, path::Path};

impl LoadedPlugin {
    /// Software depends on both its own selected fields and the base it inherits.
    /// 软件依赖自身选定字段以及所继承基础的选定字段。
    pub(crate) fn cache_configuration_hash(&self, config: &Path, software: bool) -> Result<String> {
        let mut parameters = crate::config::read_plugin_parameters_at(config)?;
        // Controller policy is not an input to the plugin's stage output, even for RSTAR_CONFIG.
        // 即使选择整个 RSTAR_CONFIG，控制器策略也不属于插件阶段产物的输入。
        for name in [
            "RSTAR_CONFIG_CACHEREUSEMINUTES",
            "RSTAR_CACHE_REUSE_MINUTES",
            "RSTAR_CONFIG_DEDUPLICATEPACKAGES",
            "RSTAR_DEDUPLICATE_PACKAGES",
        ] {
            parameters.values.remove(name);
            parameters.lists.remove(name);
            parameters.field_sections.remove(name);
        }
        let mut selectors: BTreeSet<_> = self.metadata.cache.base_system.iter().collect();
        if software {
            selectors.extend(&self.metadata.cache.software);
        }
        let mut digest = Sha256::new();
        digest.update(b"rstarsleep-plugin-cache-inputs-v1");
        for selector in selectors {
            field(&mut digest, selector);
            let explicit = selector.starts_with("RSTAR_*");
            let name = selector
                .strip_prefix("RSTAR_*")
                .map(|suffix| format!("RSTAR_{suffix}"))
                .unwrap_or_else(|| selector.clone());
            let section = name.strip_prefix("RSTAR_").unwrap();
            let exact_declared = self
                .metadata
                .config
                .required
                .iter()
                .chain(&self.metadata.config.optional)
                .any(|field| field == &name);
            let is_section = !explicit
                && (parameters.sections.contains(section)
                    || (!exact_declared
                        && self
                            .metadata
                            .config
                            .required
                            .iter()
                            .chain(&self.metadata.config.optional)
                            .any(|field| field.starts_with(&format!("{name}_")))));
            if is_section {
                if parameters.lists.contains_key(&name) {
                    return Err(Error::Invalid(format!(
                        "缓存选择器同时匹配裸参数和节，请使用不冲突的名称: {selector}"
                    )));
                }
                digest.update(b"section");
                digest.update([u8::from(parameters.sections.contains(section))]);
                let fields: Vec<_> = parameters
                    .lists
                    .iter()
                    .filter(|(key, _)| {
                        parameters
                            .field_sections
                            .get(*key)
                            .is_some_and(|owner| owner == section)
                    })
                    .collect();
                digest.update((fields.len() as u64).to_le_bytes());
                for (key, value) in fields {
                    field(&mut digest, key);
                    field(
                        &mut digest,
                        &serde_json::to_string(value).map_err(|e| Error::Invalid(e.to_string()))?,
                    );
                }
            } else {
                if explicit
                    && parameters
                        .field_sections
                        .get(&name)
                        .is_some_and(String::is_empty)
                {
                    return Err(Error::Invalid(format!(
                        "精确节字段选择器不能匹配裸参数: {selector}"
                    )));
                }
                digest.update(b"parameter");
                if let Some(value) = parameters.lists.get(&name) {
                    digest.update([1]);
                    field(
                        &mut digest,
                        &serde_json::to_string(value).map_err(|e| Error::Invalid(e.to_string()))?,
                    );
                } else {
                    digest.update([0]);
                }
            }
        }
        Ok(format!("{:x}", digest.finalize()))
    }
}

fn field(digest: &mut Sha256, value: &str) {
    digest.update((value.len() as u64).to_le_bytes());
    digest.update(value.as_bytes());
}

#[cfg(test)]
mod tests {
    use std::fs;

    #[test]
    fn selectors_track_only_effective_selected_values_and_section_presence() {
        let mut plugin = super::super::runner::tests::fixture(&[], &[]);
        plugin.metadata.config.optional.extend(
            [
                "RSTAR_CHANNEL",
                "RSTAR_CUSTOM_ENABLED",
                "RSTAR_CUSTOM_MODE",
                "RSTAR_CUSTOM_SUB_VALUE",
            ]
            .into_iter()
            .map(String::from),
        );
        let directory = tempfile::tempdir().unwrap();
        fs::create_dir(directory.path().join("build")).unwrap();
        let path = directory.path().join("build/config.conf");
        let text = "Channel=bare\n[Custom]\nEnabled=old\nEnabled=true\nMode=stable\n[Custom_Sub]\nValue=one\n";
        for (selector, changes, ignored) in [
            (
                "RSTAR_CHANNEL",
                vec![("Channel=bare", "Channel=other")],
                vec![("Mode=stable", "Mode=new")],
            ),
            (
                "RSTAR_CUSTOM",
                vec![
                    ("Enabled=true", "Enabled=false"),
                    ("Mode=stable", "Mode=new"),
                ],
                vec![("Value=one", "Value=two")],
            ),
            (
                "RSTAR_*CUSTOM_ENABLED",
                vec![
                    ("Enabled=true", "Enabled=false"),
                    ("Enabled=old\nEnabled=true\n", ""),
                ],
                vec![("Mode=stable", "Mode=new")],
            ),
        ] {
            plugin.metadata.cache.base_system = vec![selector.into()];
            plugin
                .metadata
                .cache
                .validate(&plugin.metadata.config)
                .unwrap();
            fs::write(&path, text).unwrap();
            let original = plugin
                .cache_configuration_hash(directory.path(), false)
                .unwrap();
            for (before, after) in changes {
                fs::write(&path, text.replace(before, after)).unwrap();
                assert_ne!(
                    plugin
                        .cache_configuration_hash(directory.path(), false)
                        .unwrap(),
                    original,
                    "ignored {selector}: {before}"
                );
            }
            for (before, after) in ignored {
                fs::write(&path, text.replace(before, after)).unwrap();
                assert_eq!(
                    plugin
                        .cache_configuration_hash(directory.path(), false)
                        .unwrap(),
                    original,
                    "changed {selector}: {before}"
                );
            }
        }
        plugin.metadata.cache.base_system = vec!["RSTAR_CUSTOM".into()];
        fs::write(&path, "[Custom]\n").unwrap();
        let empty_section = plugin
            .cache_configuration_hash(directory.path(), false)
            .unwrap();
        fs::write(&path, "").unwrap();
        assert_ne!(
            plugin
                .cache_configuration_hash(directory.path(), false)
                .unwrap(),
            empty_section
        );
        plugin.metadata.cache.base_system.clear();
        let unselected = plugin
            .cache_configuration_hash(directory.path(), false)
            .unwrap();
        fs::write(&path, text).unwrap();
        assert_eq!(
            plugin
                .cache_configuration_hash(directory.path(), false)
                .unwrap(),
            unselected
        );
    }
}
