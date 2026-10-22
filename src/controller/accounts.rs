//! Optional account inheritance: frozen inputs, plugin semantics and Rust policy.
mod impact;

use super::{
    BuildBackend, BuildContext, Error, Result, fs as disk, plugins::LoadedPlugin, preflight,
};
use crate::config::{AccountDriftPolicy, AccountInheritanceConfig, InheritanceConfig};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs,
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Drift {
    pub kind: String,
    pub name: String,
    pub old_id: u32,
    pub new_id: Option<u32>,
    pub new_name: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Check {
    schema_version: u32,
    consistent: bool,
    #[serde(default)]
    added_users: Vec<String>,
    #[serde(default)]
    added_groups: Vec<String>,
    #[serde(default)]
    dropped_users: Vec<String>,
    #[serde(default)]
    dropped_groups: Vec<String>,
    #[serde(default)]
    drifts: Vec<Drift>,
    #[serde(default)]
    differences: Vec<String>,
}

fn parse_check(text: &str) -> Result<Check> {
    // Never include plugin records in errors: they may describe credential changes.
    let report: Check = serde_json::from_str(text).map_err(|_| {
        Error::Invalid(
            "账户检查返回了无效的 JSON 协议 / Account check returned an invalid JSON protocol"
                .into(),
        )
    })?;
    if report.schema_version != 1
        || report
            .drifts
            .iter()
            .any(|d| !matches!(d.kind.as_str(), "uid" | "gid"))
    {
        return Err(Error::Invalid("账户检查协议版本或身份种类不受支持 / Unsupported account-check protocol version or identity kind".into()));
    }
    Ok(report)
}

fn workspace(plugin: &LoadedPlugin) -> PathBuf {
    plugin.temporary_path().join(
        &plugin
            .metadata
            .account_inheritance
            .as_ref()
            .unwrap()
            .workdir,
    )
}

fn call(
    plugin: &LoadedPlugin,
    context: &BuildContext,
    root: &Path,
    operation: &str,
    preflight: bool,
) -> Result<String> {
    let mut args = vec!["--workspace".into(), workspace(plugin).into_os_string()];
    if preflight {
        args.push("--preflight".into());
    }
    let output = plugin.invoke_for_build(operation, &args, root, context)?;
    String::from_utf8(output.stdout).map_err(|_| {
        Error::Invalid("账户检查输出不是 UTF-8 / Account-check output is not UTF-8".into())
    })
}

/// Capture without following links out of a declared tree. Only files and directories are inputs.
fn copy_tree(source: &Path, target: &Path) -> Result<()> {
    disk::checked(source)?;
    let info = match fs::symlink_metadata(source) {
        Ok(info) => info,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(disk::io(source, e)),
    };
    if info.is_dir() {
        disk::directory(target)?;
        fs::set_permissions(target, fs::Permissions::from_mode(0o700))
            .map_err(|e| disk::io(target, e))?;
        for entry in fs::read_dir(source).map_err(|e| disk::io(source, e))? {
            let entry = entry.map_err(|e| disk::io(source, e))?;
            copy_tree(&entry.path(), &target.join(entry.file_name()))?;
        }
    } else if info.is_file() {
        disk::directory(target.parent().unwrap())?;
        fs::copy(source, target).map_err(|e| disk::io(source, e))?;
    } else {
        return Err(Error::Invalid(format!(
            "账户输入必须为普通文件或目录: {0} / Account input must be a regular file or directory: {0}",
            source.display()
        )));
    }
    Ok(())
}

fn capture(plugin: &LoadedPlugin, source_root: &Path, name: &str) -> Result<()> {
    let target = workspace(plugin).join(name);
    disk::directory(&target)?;
    for path in &plugin.metadata.account_inheritance.as_ref().unwrap().paths {
        let source = disk::guest_path(source_root, path)?;
        copy_tree(&source, &target.join(path.strip_prefix("/").unwrap()))?;
    }
    Ok(())
}

/// Called under the system lock, before versions, target downloads or cache retirement.
pub(super) fn preflight(
    plugin: &LoadedPlugin,
    context: &BuildContext,
    report: &mut preflight::Report,
) -> Result<()> {
    let parameters = crate::config::read_plugin_parameters_at(&context.config)?;
    let config = AccountInheritanceConfig::read(&parameters)?;
    let Some(requirements) = &plugin.metadata.account_inheritance else {
        // Absence is an explicit distribution opt-out, including for nontraditional account systems.
        return Ok(());
    };
    let work = workspace(plugin);
    disk::directory(&work)?;
    fs::set_permissions(&work, fs::Permissions::from_mode(0o700))
        .map_err(|e| disk::io(&work, e))?;
    for name in ["old", "new", "output"] {
        disk::directory(&work.join(name))?;
    }
    #[derive(Serialize)]
    struct Request<'a> {
        schema_version: u32,
        users: &'a [String],
        groups: &'a [String],
    }
    let request = serde_json::to_vec(&Request {
        schema_version: 1,
        users: &config.users,
        groups: &config.groups,
    })
    .unwrap();
    crate::subsystem::atomic_write(&work.join("request.json"), &request, 0o600)
        .map_err(|e| Error::Invalid(e.to_string()))?;
    capture(plugin, Path::new("/"), "old")?;
    let checked = parse_check(&call(
        plugin,
        context,
        Path::new("/"),
        "account_check",
        true,
    )?)?;
    if !checked.differences.is_empty() {
        return Err(Error::Invalid(format!(
            "账户来源预检失败: {0} / Account-source preflight failed: {0}",
            checked.differences.join("; ")
        )));
    }
    if !checked.dropped_users.is_empty() || !checked.dropped_groups.is_empty() {
        report.warn(
            &context.system_id,
            "account-selection",
            format!(
                "未声明继承的来源账户可能不被保留；用户 {0:?}，组 {1:?}。新构建仍可能提供同名账户。 / Source accounts without declared inheritance may not be preserved; users {0:?}, groups {1:?}. The new build may still provide accounts with the same names.",
                checked.dropped_users, checked.dropped_groups
            ),
        );
    }
    let inheritance = InheritanceConfig::read(&parameters)?;
    for (kind, targets) in [
        ("CopyOnWriteBoot", &inheritance.copy_on_write_boot),
        ("Mount", &inheritance.mounts),
    ] {
        let overlaps: Vec<_> = targets
            .iter()
            .filter(|target| {
                requirements
                    .paths
                    .iter()
                    .any(|account| account.starts_with(target) || target.starts_with(account))
            })
            .collect();
        if !overlaps.is_empty() {
            report.warn(&context.system_id, "account-override", format!(
                "{kind} {0:?} 会覆盖或遮蔽账户相关路径，可能覆盖账户继承和 RootPassword 的构建结果。 / {kind} {0:?} overwrites or shadows account-related paths and may overwrite the build results of account inheritance and RootPassword.", overlaps));
        }
    }
    Ok(())
}

/// The same frozen old input is hashed and later used by the plugin.
pub(super) fn fingerprint(plugin: &LoadedPlugin) -> Result<String> {
    let Some(_) = &plugin.metadata.account_inheritance else {
        return Ok(String::new());
    };
    fn hash(path: &Path, digest: &mut Sha256) -> Result<()> {
        let info = fs::symlink_metadata(path).map_err(|e| disk::io(path, e))?;
        digest.update(info.mode().to_le_bytes());
        if info.is_file() {
            let bytes = fs::read(path).map_err(|e| disk::io(path, e))?;
            digest.update((bytes.len() as u64).to_le_bytes());
            digest.update(bytes);
        } else if info.is_dir() {
            let mut entries = fs::read_dir(path)
                .map_err(|e| disk::io(path, e))?
                .collect::<std::io::Result<Vec<_>>>()
                .map_err(|e| disk::io(path, e))?;
            entries.sort_by_key(|e| e.file_name());
            digest.update((entries.len() as u64).to_le_bytes());
            for entry in entries {
                let name = entry.file_name();
                let bytes = name.as_encoded_bytes();
                digest.update((bytes.len() as u64).to_le_bytes());
                digest.update(bytes);
                hash(&entry.path(), digest)?;
            }
        } else {
            return Err(Error::Invalid(
                "账户输入类型已改变 / Account input type has changed".into(),
            ));
        }
        Ok(())
    }
    let mut digest = Sha256::new();
    digest.update(b"rstarsleep-accounts-v1");
    hash(&workspace(plugin).join("old"), &mut digest)?;
    Ok(format!("{:x}", digest.finalize()))
}

pub(super) fn capture_new(plugin: &LoadedPlugin, root: &Path) -> Result<()> {
    if plugin.metadata.account_inheritance.is_some() {
        capture(plugin, root, "new")?;
    }
    Ok(())
}

pub(super) fn apply(
    backend: &impl BuildBackend,
    plugin: &LoadedPlugin,
    context: &BuildContext,
    root: &Path,
    system: &Path,
) -> Result<bool> {
    if plugin.metadata.account_inheritance.is_none() {
        return Ok(false);
    }
    call(plugin, context, root, "account_prepare", false)?;
    let checked = parse_check(&call(plugin, context, root, "account_check", false)?)?;
    if !checked.differences.is_empty() {
        return Err(Error::Invalid(format!(
            "账户语义校验失败: {0} / Account semantic validation failed: {0}",
            checked.differences.join("; ")
        )));
    }
    if !checked.consistent
        && checked.drifts.is_empty()
        && checked.dropped_users.is_empty()
        && checked.dropped_groups.is_empty()
    {
        return Err(Error::Invalid("账户语义不一致但插件未说明原因 / Account semantics are inconsistent, but the plugin provided no reason".into()));
    }
    if !checked.drifts.is_empty() {
        let config = AccountInheritanceConfig::read(&crate::config::read_plugin_parameters_at(
            &context.config,
        )?)?;
        let summary = checked
            .drifts
            .iter()
            .map(|d| {
                format!(
                    "{} {}: {} → {:?} ({:?})",
                    d.kind, d.name, d.old_id, d.new_id, d.new_name
                )
            })
            .collect::<Vec<_>>()
            .join("; ");
        match config.drift_policy {
            AccountDriftPolicy::Silent => (),
            AccountDriftPolicy::Warn => super::logging::diagnostic(format!(
                "警告：UID/GID 漂移（不自动修复）：{summary} / Warning: UID/GID drift (not automatically repaired): {summary}"
            )),
            AccountDriftPolicy::Error => {
                return Err(Error::Invalid(format!(
                    "UID/GID 漂移：{summary} / UID/GID drift: {summary}"
                )));
            }
            AccountDriftPolicy::Assess => {
                super::logging::diagnostic(format!("UID/GID 漂移影响评估：{summary}"));
                impact::assess(backend, context, system, &checked.drifts)?;
            }
        }
    }
    super::logging::diagnostic(format!(
        "账户检查：构建新增用户 {:?}，组 {:?}；未保留用户 {:?}，组 {:?}",
        checked.added_users, checked.added_groups, checked.dropped_users, checked.dropped_groups
    ));
    call(plugin, context, root, "account_apply", false)?;
    Ok(true)
}
