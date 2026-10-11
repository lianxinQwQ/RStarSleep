//! Validate runtime policy separately from archived build configuration.
//! 独立于构建留档校验运行策略。
use super::{Error, Result, checked};
use crate::config::ConfigStore;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    fs,
    path::{Component, Path, PathBuf},
};

/// A host object bound at a guest path for the machine lifetime.
/// 在机器生命周期中绑定到容器路径的宿主对象。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeMount {
    pub source: PathBuf,
    pub target: PathBuf,
    #[serde(default)]
    pub read_only: bool,
}

/// Explicit and package-derived application entry requests.
/// 显式及按包推导的应用入口请求。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ExportConfig {
    pub packages: Vec<String>,
    pub commands: Vec<String>,
    pub desktop_files: Vec<String>,
}

/// Current runtime policy; a schema-only historical template remains valid.
/// 当前运行策略；仅含版本号的旧模板继续有效。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SubsystemConfig {
    pub schema_version: u32,
    pub autostart: bool,
    pub idle_timeout_seconds: u64,
    pub session_mode: String,
    pub allowed_uids: Vec<u32>,
    pub session_user: String,
    pub mounts: Vec<RuntimeMount>,
    pub exports: ExportConfig,
    pub tmp_size_bytes: u64,
    pub gpu: bool,
}

impl Default for SubsystemConfig {
    /// Supply conservative runtime defaults for existing templates.
    /// 为旧模板提供保守的运行默认值。
    /// # Arguments
    /// No parameters. 无参数。
    fn default() -> Self {
        Self {
            schema_version: 1,
            autostart: false,
            idle_timeout_seconds: 30,
            session_mode: "isolated".into(),
            allowed_uids: Vec::new(),
            session_user: "caller".into(),
            mounts: Vec::new(),
            exports: ExportConfig::default(),
            tmp_size_bytes: 0,
            gpu: false,
        }
    }
}

/// Require a normalized absolute guest or configured host path.
/// 要求规范绝对容器路径或配置的宿主路径。
/// # Arguments
/// - `path`: Path to inspect. 待检查路径。
/// - `root_allowed`: Whether the root itself is allowed. 是否允许根自身。
pub(crate) fn absolute(path: &Path, root_allowed: bool) -> Result<()> {
    let value = path
        .to_str()
        .ok_or_else(|| Error::Invalid("路径必须为 UTF-8".into()))?;
    if !path.is_absolute()
        || (!root_allowed && path == Path::new("/"))
        || path
            .components()
            .any(|p| !matches!(p, Component::RootDir | Component::Normal(_)))
        || value.contains("//")
        || value.contains("/./")
        || value.ends_with("/.")
        || value.chars().any(char::is_control)
        || value.contains([':', '\\'])
    {
        return Err(Error::Invalid(format!(
            "路径必须为规范绝对路径: {}",
            path.display()
        )));
    }
    Ok(())
}

impl SubsystemConfig {
    /// Read current runtime options for one explicitly registered subsystem.
    /// 为一个显式登记的子系统读取当前运行选项。
    /// # Arguments
    /// - `store`: Initialized manager root. 已初始化管理根。
    /// - `id`: Registered system identifier. 已登记系统标识。
    /// # Errors
    /// Returns role, syntax or policy validation failures. 返回角色、语法或策略校验错误。
    pub fn read(store: &ConfigStore, id: &str) -> Result<Self> {
        let roles = store.runtime_config()?;
        if !roles.subsystems.iter().any(|value| value == id) {
            return Err(Error::Invalid(format!("系统 {id} 未登记为子系统")));
        }
        let path = store
            .system_layout(id)?
            .config
            .join("subsystem/config.toml");
        checked(&path)?;
        let text = fs::read_to_string(&path).map_err(|source| Error::Io {
            path: path.clone(),
            source,
        })?;
        let config: Self = toml::from_str(&text)
            .map_err(|e| Error::Invalid(format!("{}: {e}", path.display())))?;
        config.validate()?;
        Ok(config)
    }

    /// Reject unsupported identities, modes and overlapping mount targets.
    /// 拒绝不支持的身份、模式与重叠挂载目标。
    /// # Arguments
    /// - `self`: Current runtime policy. 当前运行策略。
    /// # Errors
    /// Returns invalid or conflicting settings. 返回无效或冲突设置。
    pub fn validate(&self) -> Result<()> {
        if self.schema_version != 1
            || self.session_user != "caller"
            || !matches!(self.session_mode.as_str(), "off" | "isolated" | "native")
            || self.idle_timeout_seconds > 86400
        {
            return Err(Error::Invalid(
                "运行 schema、用户、会话模式或闲置时长无效".into(),
            ));
        }
        if self
            .allowed_uids
            .iter()
            .copied()
            .collect::<BTreeSet<_>>()
            .len()
            != self.allowed_uids.len()
        {
            return Err(Error::Invalid("allowed_uids 不能重复".into()));
        }
        for (i, mount) in self.mounts.iter().enumerate() {
            absolute(&mount.source, false)?;
            absolute(&mount.target, false)?;
            for reserved in ["/run", "/dev", "/proc", "/sys", "/rstarsleep"] {
                if mount.target.starts_with(reserved)
                    || Path::new(reserved).starts_with(&mount.target)
                {
                    return Err(Error::Invalid(format!(
                        "挂载目标为保留路径: {}",
                        mount.target.display()
                    )));
                }
            }
            for other in &self.mounts[..i] {
                if mount.target.starts_with(&other.target)
                    || other.target.starts_with(&mount.target)
                {
                    return Err(Error::Invalid("挂载目标不能重复或包含彼此".into()));
                }
            }
        }
        for path in self
            .exports
            .commands
            .iter()
            .chain(&self.exports.desktop_files)
        {
            absolute(Path::new(path), false)?;
        }
        if self
            .exports
            .packages
            .iter()
            .any(|p| p.is_empty() || p.starts_with('-') || p.chars().any(char::is_whitespace))
        {
            return Err(Error::Invalid("导出包名无效".into()));
        }
        Ok(())
    }

    /// Authorize a kernel-authenticated application caller.
    /// 授权内核认证的应用调用者。
    /// # Arguments
    /// - `self`: Current policy. 当前策略。
    /// - `uid`: Peer user identity. 对端用户身份。
    pub fn allows(&self, uid: u32) -> bool {
        uid == 0 || self.allowed_uids.contains(&uid)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    /// Preserve old templates and reject confused deputy mount/identity policies.
    /// 保留旧模板并拒绝混淆代理的挂载和身份策略。
    /// # Arguments
    /// No parameters. 无参数。
    #[test]
    fn validates_compatibility_and_boundaries() {
        let old: SubsystemConfig = toml::from_str("schema_version=1").unwrap();
        assert!(old.validate().is_ok());
        assert!(old.allows(0));
        assert!(!old.allows(1000));
        let mut c = old.clone();
        c.allowed_uids = vec![1000, 1000];
        assert!(c.validate().is_err());
        c = old.clone();
        c.session_user = "root".into();
        assert!(c.validate().is_err());
        for target in ["/", "/run/user/1000", "/dev/dri", "/home/../etc"] {
            c = old.clone();
            c.mounts.push(RuntimeMount {
                source: "/home/alice".into(),
                target: target.into(),
                read_only: false,
            });
            assert!(c.validate().is_err(), "{target}");
        }
        c = old;
        c.allowed_uids = vec![1000];
        assert!(c.allows(1000));
    }
}
