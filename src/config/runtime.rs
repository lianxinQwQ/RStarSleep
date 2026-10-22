//! Global main-system settings used by boot publication and cleanup.
//! 启动发布和清理使用的全局主系统设置。
use super::{ConfigStore, Error, Result, io_error, validate_identifier};
use serde::Deserialize;
use std::{fs, path::PathBuf};

/// Global boot and explicitly enabled subsystem settings.
/// 全局启动及显式启用子系统设置。
#[derive(Debug, Deserialize)]
pub struct RuntimeConfig {
    pub schema_version: u32,
    pub main_system: Option<String>,
    #[serde(default)]
    pub subsystems: Vec<String>,
    #[serde(default = "boot_directory")]
    pub boot_dir: PathBuf,
    #[serde(default)]
    pub kernel_options: Vec<String>,
}

/// Return the default systemd-boot filesystem mount.
/// 返回默认 systemd-boot 文件系统挂载目录。
/// # Arguments
/// No parameters. 无参数。
fn boot_directory() -> PathBuf {
    "/boot".into()
}

impl ConfigStore {
    /// Read and validate global settings before building or deleting versions.
    /// 构建或删除版本前读取并校验全局设置。
    /// # Arguments
    /// - `self`: Initialized manager root. 已初始化管理根。
    /// # Errors
    /// Returns malformed configuration or unsafe option errors.
    /// 返回配置格式错误或不安全选项错误。
    pub fn runtime_config(&self) -> Result<RuntimeConfig> {
        let path = self.global_config_path();
        let text = fs::read_to_string(&path).map_err(|e| io_error(&path, e))?;
        let config: RuntimeConfig =
            toml::from_str(&text).map_err(|source| Error::Parse { path, source })?;
        if config.schema_version != 1 {
            return Err(Error::Invalid(
                "仅支持全局 schema_version=1 / only global schema_version=1 is supported".into(),
            ));
        }
        if let Some(id) = &config.main_system {
            validate_identifier(id)?;
        }
        let mut seen = std::collections::BTreeSet::new();
        for id in &config.subsystems {
            validate_identifier(id)?;
            if !seen.insert(id) || config.main_system.as_deref() == Some(id.as_str()) {
                return Err(Error::Invalid(format!(
                    "子系统名单重复或与主系统重叠: {id} / subsystem list contains a duplicate or overlaps with the main system: {id}"
                )));
            }
        }
        if !config.boot_dir.is_absolute()
            || config
                .boot_dir
                .components()
                .any(|part| matches!(part, std::path::Component::ParentDir))
        {
            return Err(Error::Invalid(
                "boot_dir 必须为不含父级跳转的绝对路径 / boot_dir must be an absolute path without parent traversal".into(),
            ));
        }
        for option in &config.kernel_options {
            if option.is_empty()
                || option.chars().any(char::is_whitespace)
                || option.chars().any(char::is_control)
                || option.starts_with("root=")
                || option.starts_with("rootflags=")
                || option.starts_with("rstarsleep.pending=")
                || option.starts_with("rstarsleep.storage=")
                || matches!(option.as_str(), "rw" | "ro")
            {
                return Err(Error::Invalid(format!(
                    "无效额外内核参数: {option:?} / invalid extra kernel argument: {option:?}"
                )));
            }
        }
        Ok(config)
    }
}
