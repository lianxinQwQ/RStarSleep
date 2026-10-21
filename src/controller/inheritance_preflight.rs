//! Diagnose the current view once, before allocating versions or running build operations.
use super::{BuildBackend, Result, deferred::InheritanceRoot, fs as disk, inheritance};
use crate::config::{InheritanceConfig, NocowHandling, validate_inheritance_type};
use std::{fs, path::Path};

pub(super) fn inspect(
    backend: &impl BuildBackend,
    id: &str,
    config: &InheritanceConfig,
    system: &Path,
    work: &Path,
    warnings: &mut Vec<String>,
) -> Result<Option<InheritanceRoot>> {
    if config.copy_on_write_build.is_empty() && config.copy_on_write_boot.is_empty() {
        return Ok(None);
    }
    warnings.push(format!(
        "警告：系统 {id} 将在构建时继承 {:?}，首次启动时继承 {:?}；DirectorySubvolume={}，NocowHandling={}。\n继承路径不设黑名单，内容及兼容性由用户负责；启动时继承使用的内核与启动镜像固定为构建时产物。\nWarning: system {id} will inherit the listed paths at build time and first boot. Inherited content and compatibility are user-managed; the kernel and boot image remain build-time artifacts.",
        config.copy_on_write_build, config.copy_on_write_boot,
        if config.directory_subvolume { "yes" } else { "no" }, config.nocow_handling));
    if config.nocow_handling == NocowHandling::MoveSnapshot {
        warnings.push("NocowHandling=move-snapshot 会将普通来源目录转换为子卷，并可能进行完整复制。该操作修改实际来源，要求来源离线且已停止写入。 / Converts actual offline source directories into subvolumes; migration may fully copy data.".into());
    }
    // Work directories may not have been created yet; their nearest existing ancestor
    // identifies the same filesystem without allocating any build storage.
    let destination = work.ancestors().find(|path| path.exists()).unwrap();
    for source in &config.copy_on_write_build {
        let unmarked = Path::new(source.to_str().unwrap().trim_end_matches('/'));
        let resolved = fs::canonicalize(unmarked).map_err(|e| disk::io(source, e))?;
        let directory = inheritance::check_source(&resolved, system)?;
        validate_inheritance_type(source, directory)?;
        backend.check_cow_source(&resolved, destination)?;
    }
    if config.copy_on_write_boot.is_empty() {
        return Ok(None);
    }
    let target_uuid = backend.filesystem_location(system).map(|(uuid, _)| uuid);
    for source in &config.copy_on_write_boot {
        // This is only a diagnostic of the current mount view, never a source mapping.
        let check = (|| {
            let unmarked = Path::new(source.to_str().unwrap().trim_end_matches('/'));
            let resolved = fs::canonicalize(unmarked).map_err(|e| disk::io(source, e))?;
            let info = fs::metadata(&resolved).map_err(|e| disk::io(source, e))?;
            validate_inheritance_type(source, info.is_dir())?;
            let (uuid, _) = backend.filesystem_location(&resolved)?;
            match &target_uuid {
                Ok(target) if *target == uuid => Ok(()),
                Ok(_) => Err(super::Error::Invalid(
                    "当前视图下来源与目标 Btrfs UUID 不一致".into(),
                )),
                Err(error) => Err(super::Error::Invalid(error.to_string())),
            }
        })();
        if let Err(error) = check {
            warnings.push(format!(
                "警告：系统 {id} 启动继承路径 {} 当前视图检查未通过：{error}；保留配置原路径，启动时检查实际来源。 / Warning: boot inheritance current-view check failed; the configured path is retained for validation at boot.", source.display()));
        }
    }
    let root: Result<InheritanceRoot> = (|| {
        let (filesystem_uuid, path) = backend.filesystem_location(Path::new("/"))?;
        Ok(InheritanceRoot {
            filesystem_uuid,
            path,
            subvolume_id: backend.source_subvolume_id(Path::new("/"))?,
        })
    })();
    match root {
        Ok(root) => {
            if let Ok(uuid) = &target_uuid
                && *uuid != root.filesystem_uuid
            {
                warnings.push(format!("警告：系统 {id} 启动继承来源根与目标 Btrfs UUID 不一致；启动时无法在目标文件系统内完成继承。 / Warning: the boot source root is on a different filesystem."));
            }
            Ok(Some(root))
        }
        Err(error) => {
            warnings.push(format!("警告：系统 {id} 无法固定启动继承来源根：{error}；需在有效的 Btrfs 来源根环境重新构建，否则启动继承将退出。 / Warning: cannot pin the boot source root; rebuild from a valid Btrfs root before booting."));
            Ok(None)
        }
    }
}
