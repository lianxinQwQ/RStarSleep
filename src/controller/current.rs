//! Publish and retain a single current artifact without runtime version selection.
//! 发布并保留唯一当前产物，不提供运行版本选择。
use super::{Error, Result, fs as disk};
use crate::config::ConfigStore;
use serde::{Deserialize, Serialize};
use std::{fs, path::Path};

/// Reference atomically replaced after successful build preparation.
/// 构建准备成功后原子替换的引用。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CurrentBuild {
    pub schema_version: u32,
    pub system_id: String,
    pub version: String,
}

/// Validate the fixed numeric build identifier.
/// 校验固定数字构建标识。
/// # Arguments
/// - `version`: Internal version name. 内部版本名。
fn version_id(version: &str) -> Result<()> {
    if !version.strip_prefix('v').is_some_and(|v| {
        !v.is_empty() && v.bytes().all(|c| c.is_ascii_digit()) && v.parse::<u64>().is_ok()
    }) {
        return Err(Error::Invalid(format!("产物版本无效: {version}")));
    }
    Ok(())
}

/// Validate the exact referenced state and required independently retained root.
/// 校验精确引用状态及独立保留的必需根。
/// # Arguments
/// - `system`: Existing system directory. 已有系统目录。
/// - `current`: Validated record identity. 经校验记录身份。
fn validate_state(system: &Path, current: &CurrentBuild) -> Result<()> {
    let directory = system.join("versions").join(&current.version);
    disk::checked(&directory)?;
    let path = directory.join("state.toml");
    disk::checked(&path)?;
    let state: toml::Value =
        toml::from_str(&fs::read_to_string(&path).map_err(|e| disk::io(&path, e))?)
            .map_err(|e| Error::Invalid(e.to_string()))?;
    if state.get("system_id").and_then(|v| v.as_str()) != Some(&current.system_id)
        || state.get("version").and_then(|v| v.as_str()) != Some(&current.version)
        || state.get("schema_version").and_then(|v| v.as_integer()) != Some(1)
    {
        return Err(Error::Invalid("当前产物状态身份不匹配".into()));
    }
    let root = match state.get("state").and_then(|v| v.as_str()) {
        Some("complete") => directory.join("root"),
        Some("pending-inheritance") => {
            let manifest = directory.join("inheritance-pending.toml");
            let pending = super::deferred::read_manifest(&manifest)?;
            if pending.system_id != current.system_id || pending.version != current.version {
                return Err(Error::Invalid("当前产物待办身份不匹配".into()));
            }
            directory.join("prepared-root")
        }
        _ => return Err(Error::Invalid("当前产物未成功交付或已经取消".into())),
    };
    disk::checked(&root)?;
    if !root.is_dir() {
        return Err(Error::Invalid("当前产物根不存在".into()));
    }
    Ok(())
}

/// Read the current build reference without searching historical artifacts.
/// 读取当前构建引用，不查找历史产物。
/// # Arguments
/// - `store`: Owning manager. 所属管理器。
/// - `id`: Expected managed system. 预期被管理系统。
/// # Errors
/// Returns missing, malformed or incomplete artifacts. 返回缺失、损坏或未完成产物错误。
pub fn read_current_build(store: &ConfigStore, id: &str) -> Result<CurrentBuild> {
    let system = store.system_layout(id)?.root;
    let path = system.join("build-current.toml");
    disk::checked(&path)?;
    let current: CurrentBuild =
        toml::from_str(&fs::read_to_string(&path).map_err(|e| disk::io(&path, e))?)
            .map_err(|e| Error::Invalid(e.to_string()))?;
    version_id(&current.version)?;
    if current.schema_version != 1 || current.system_id != id {
        return Err(Error::Invalid("当前产物引用身份不匹配".into()));
    }
    validate_state(&system, &current)?;
    Ok(current)
}

/// Publish a successful artifact while holding the existing build lock.
/// 持有现有构建锁时发布成功产物。
/// # Arguments
/// - `system`: Owned system directory. 所属系统目录。
/// - `id`: Expected system identity. 预期系统身份。
/// - `version`: Successfully prepared artifact. 成功准备的产物。
pub(super) fn publish_current_build(system: &Path, id: &str, version: &str) -> Result<()> {
    version_id(version)?;
    let current = CurrentBuild {
        schema_version: 1,
        system_id: id.into(),
        version: version.into(),
    };
    validate_state(system, &current)?;
    disk::write(&system.join("build-current.toml"), &current)
}

/// Refuse deletion of references before any cleanup mutations begin.
/// 清理修改开始前拒绝删除引用对象。
/// # Arguments
/// - `store`: Owning manager. 所属管理器。
/// - `id`: System being cleaned. 被清理系统。
/// - `selected`: Complete deletion cutoff set. 完整截止删除集合。
pub(super) fn check_retained(store: &ConfigStore, id: &str, selected: &[String]) -> Result<()> {
    let system = store.system_layout(id)?.root;
    for name in [
        "build-current.toml",
        "runtime/current.toml",
        "runtime/transaction.toml",
    ] {
        let path = system.join(name);
        disk::checked(&path)?;
        if !path.exists() {
            continue;
        }
        let value: toml::Value =
            toml::from_str(&fs::read_to_string(&path).map_err(|e| disk::io(&path, e))?)
                .map_err(|e| Error::Invalid(e.to_string()))?;
        if value.get("schema_version").and_then(|v| v.as_integer()) != Some(1)
            || value.get("system_id").and_then(|v| v.as_str()) != Some(id)
            || value.get("version").and_then(|v| v.as_str()).is_none()
        {
            return Err(Error::Invalid(format!("引用记录无效: {}", path.display())));
        }
        for key in ["version", "previous_version"] {
            if let Some(raw) = value.get(key) {
                let version = raw
                    .as_str()
                    .ok_or_else(|| Error::Invalid("引用版本必须为字符串".into()))?;
                version_id(version)?;
                if selected.iter().any(|v| v == version) {
                    return Err(Error::Invalid(format!(
                        "不能清理被 {} 引用的版本 {version}",
                        path.display()
                    )));
                }
            }
        }
    }
    Ok(())
}
