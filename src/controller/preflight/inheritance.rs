//! Diagnose the current view once, before allocating versions or running build operations.
//! 分配版本或执行构建操作前，统一检查选定来源视图。
use super::super::{
    BuildBackend, Error, Result, deferred::InheritanceRoot, fs as disk, inheritance,
};
use crate::config::{
    InheritanceConfig, NocowHandling, inheritance_name, validate_inheritance_type,
};
use std::{
    fs,
    path::{Component, Path},
};

pub(in crate::controller) fn inspect(
    backend: &impl BuildBackend,
    id: &str,
    config: &InheritanceConfig,
    system: &Path,
    work: &Path,
    source_root: &Path,
    warnings: &mut Vec<String>,
) -> Result<Option<InheritanceRoot>> {
    for source in &config.mounts {
        let persistent = system.join("persistent").join(inheritance_name(source)?);
        let directory = if disk::exists(&persistent)? {
            fs::metadata(&persistent)
                .map_err(|e| disk::io(&persistent, e))?
                .is_dir()
        } else {
            let unmarked = Path::new(source.to_str().unwrap().trim_end_matches('/'));
            let resolved = source_root.join(unmarked.strip_prefix("/").unwrap());
            inheritance::check_mount_source(&resolved, system)?
        };
        validate_inheritance_type(source, directory)?;
    }
    if config.copy_on_write_build.is_empty() && config.copy_on_write_boot.is_empty() {
        return Ok(None);
    }
    if config.nocow_handling == NocowHandling::MoveSnapshot {
        warnings.push("NocowHandling=move-snapshot 会将普通来源目录转换为子卷，并可能进行完整复制。该操作修改实际来源，要求来源离线且已停止写入。 / Converts actual offline source directories into subvolumes; migration may fully copy data.".into());
    }
    // Work directories may not have been created yet; their nearest existing ancestor
    // identifies the same filesystem without allocating any build storage.
    // 工作目录可能尚未创建；最近的已有祖先可在不分配构建存储时识别同一文件系统。
    let destination = work.ancestors().find(|path| path.exists()).unwrap();
    for source in &config.copy_on_write_build {
        let unmarked = Path::new(source.to_str().unwrap().trim_end_matches('/'));
        let resolved = disk::inheritance_source_path(source_root, unmarked)?;
        let directory = inheritance::check_source(&resolved, system)?;
        validate_inheritance_type(source, directory)?;
        super::super::preservation::validate_source(&resolved, config.preserve_for(source))?;
        backend.check_cow_source(&resolved, destination)?;
    }
    if config.copy_on_write_boot.is_empty() {
        return Ok(None);
    }
    let target_uuid = backend.filesystem_location(system).map(|(uuid, _)| uuid);
    for source in &config.copy_on_write_boot {
        // This is only a diagnostic of the current mount view, never a source mapping.
        // 此处只诊断所选来源的当前挂载视图，不生成启动来源映射。
        let check = (|| {
            let unmarked = Path::new(source.to_str().unwrap().trim_end_matches('/'));
            let resolved = disk::inheritance_source_path(source_root, unmarked)?;
            let info = fs::symlink_metadata(&resolved).map_err(|e| disk::io(source, e))?;
            validate_inheritance_type(source, info.is_dir())?;
            super::super::preservation::validate_source(&resolved, config.preserve_for(source))?;
            let container = if info.file_type().is_symlink() {
                resolved.parent().unwrap()
            } else {
                &resolved
            };
            let (uuid, _) = backend.filesystem_location(container)?;
            match &target_uuid {
                Ok(target) if *target == uuid => Ok(()),
                Ok(_) => Err(Error::Invalid(
                    "当前视图下来源与目标 Btrfs UUID 不一致 / Source and target Btrfs UUIDs do not match in the current view".into(),
                )),
                Err(error) => Err(Error::Invalid(error.to_string())),
            }
        })();
        if let Err(error) = check {
            warnings.push(format!(
                "警告：系统 {id} 启动继承路径 {} 当前视图检查未通过：{error}；保留配置原路径，启动时检查实际来源。 / Warning: boot inheritance current-view check failed; the configured path is retained for validation at boot.", source.display()));
        }
    }
    let root: Result<InheritanceRoot> = (|| {
        let (filesystem_uuid, path) = backend.filesystem_location(source_root)?;
        if path.to_str().is_none()
            || !path.is_absolute()
            || path
                .components()
                .any(|part| !matches!(part, Component::RootDir | Component::Normal(_)))
        {
            return Err(Error::Invalid(
                "启动继承来源根的文件系统路径必须为规范 UTF-8 绝对路径 / Boot inheritance source root filesystem path must be a normalized UTF-8 absolute path".into()
            ));
        }
        let subvolume_id = backend.source_subvolume_id(source_root)?;
        if filesystem_uuid.is_empty() || subvolume_id.is_empty() {
            return Err(Error::Invalid(
                "启动继承来源根身份无效 / Invalid boot inheritance source root identity".into(),
            ));
        }
        Ok(InheritanceRoot {
            filesystem_uuid,
            path,
            subvolume_id,
        })
    })();
    match root {
        Ok(root) => {
            if let Ok(uuid) = &target_uuid
                && *uuid != root.filesystem_uuid
            {
                if source_root != Path::new("/") {
                    return Err(Error::Invalid(format!(
                        "系统 {id} 的指定启动继承来源根与目标 Btrfs UUID 不一致 / Selected boot inheritance source root and target Btrfs UUIDs do not match for system {id}"
                    )));
                }
                warnings.push(format!("警告：系统 {id} 启动继承来源根与目标 Btrfs UUID 不一致；启动时无法在目标文件系统内完成继承。 / Warning: the boot source root is on a different filesystem."));
            }
            if source_root != Path::new("/") {
                target_uuid?;
            }
            Ok(Some(root))
        }
        Err(error) => {
            if source_root != Path::new("/") {
                return Err(Error::Invalid(format!(
                    "无法固定指定启动继承来源根 {0}: {error} / Cannot pin the selected boot inheritance source root {0}: {error}",
                    source_root.display()
                )));
            }
            warnings.push(format!("警告：系统 {id} 无法固定启动继承来源根：{error}；需在有效的 Btrfs 来源根环境重新构建，否则启动继承将退出。 / Warning: cannot pin the boot source root; rebuild from a valid Btrfs root before booting."));
            Ok(None)
        }
    }
}
