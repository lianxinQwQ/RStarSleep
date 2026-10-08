//! Parse bounded history cleanup requests.
//! 解析限定历史清理请求。
use crate::config::{ConfigStore, DEFAULT_MOUNT_POINT};
use std::{ffi::OsString, path::PathBuf};

/// Clean all versions through the requested cutoff and display removed versions.
/// 清理直到所请求截止版本的全部版本并显示已删除版本。
/// # Arguments
/// - `arguments`: Native arguments following clean. clean 后的原生参数。
pub(super) fn run(arguments: &[OsString]) -> super::Result<()> {
    if arguments == ["--help"] || arguments == ["-h"] {
        println!("用法：RStarSleep clean <系统> <版本> [--root <管理根绝对路径>]");
        return Ok(());
    }
    let (system, version, root) = match arguments {
        [system, version] => (system, version, PathBuf::from(DEFAULT_MOUNT_POINT)),
        [system, version, flag, root] if flag == "--root" => (system, version, PathBuf::from(root)),
        _ => return Err("用法：RStarSleep clean <系统> <版本> [--root <管理根绝对路径>]".into()),
    };
    for version in crate::controller::clean(
        &ConfigStore::new(root)?,
        system.to_str().ok_or("系统标识必须是 UTF-8")?,
        version.to_str().ok_or("版本必须是 UTF-8")?,
    )? {
        println!("已清理 {version}");
    }
    Ok(())
}
