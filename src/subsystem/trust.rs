//! Reject user-controlled privileged policy and service paths.
//! 拒绝用户控制的特权策略及服务路径。
use super::{Error, Result, checked};
use crate::config::ConfigStore;
use std::{fs, os::unix::fs::MetadataExt, path::Path};

/// Verify root ownership and non-writable ancestors for a control path.
/// 为控制路径校验 root 所有权及不可写祖先。
/// # Arguments
/// - `path`: Existing trusted control path. 已有可信控制路径。
pub(crate) fn path(path: &Path) -> Result<()> {
    checked(path)?;
    for part in path.ancestors() {
        let m = fs::symlink_metadata(part)?;
        if m.uid() != 0 || m.mode() & 0o022 != 0 || m.file_type().is_symlink() {
            return Err(Error::Invalid(format!(
                "不可信的特权路径: {0} / untrusted privileged path: {0}",
                part.display()
            )));
        }
    }
    Ok(())
}

/// Verify policy trees without imposing ownership on guest data.
/// 校验策略树，不要求容器数据归管理员所有。
/// # Arguments
/// - `directory`: Administrator-owned configuration tree. 管理员所有配置树。
fn tree(directory: &Path) -> Result<()> {
    path(directory)?;
    if directory.is_dir() {
        for entry in fs::read_dir(directory)? {
            let entry = entry?;
            tree(&entry.path())?;
        }
    }
    Ok(())
}

/// Validate all inputs that can make a root daemon execute or bind host paths.
/// 校验所有可令 root 守护进程执行或绑定宿主路径的输入。
/// # Arguments
/// - `store`: Fixed management root. 固定管理根。
/// - `id`: Optional system policy to inspect. 可选待检查系统策略。
pub(crate) fn store(store: &ConfigStore, id: Option<&str>) -> Result<()> {
    path(store.root())?;
    let global = store.root().join("config");
    if global.exists() {
        tree(&global)?;
    }
    for entry in fs::read_dir(store.root())? {
        let entry = entry?;
        if entry.file_type()?.is_file() {
            path(&entry.path())?;
        }
    }
    if let Some(id) = id {
        let layout = store.system_layout(id)?;
        tree(&layout.config)?;
        for record in [
            "build-current.toml",
            "runtime/current.toml",
            "runtime/transaction.toml",
        ] {
            let p = layout.root.join(record);
            if p.exists() {
                path(&p)?;
            }
        }
        if let Ok(current) = crate::controller::read_current_build(store, id) {
            let version = layout.versions.join(current.version);
            tree(&version.join("config"))?;
            for name in [
                "state.toml",
                "inheritance-pending.toml",
                "inheritance-commit.toml",
            ] {
                let p = version.join(name);
                if p.exists() {
                    path(&p)?;
                }
            }
        }
    }
    Ok(())
}
