//! Internal entrypoint called by the initramfs hook before mounting the final root.
use std::{ffi::OsString, path::PathBuf};
pub(super) fn run(arguments: &[OsString]) -> super::Result<()> {
    let mut manifest = None;
    let mut top = None;
    let mut arguments = arguments.iter();
    while let Some(argument) = arguments.next() {
        let destination = if argument == "--manifest" {
            &mut manifest
        } else if argument == "--filesystem-root" {
            &mut top
        } else {
            return Err(
                "用法：finalize-inheritance --manifest <清单> --filesystem-root <Btrfs 顶层挂载>"
                    .into(),
            );
        };
        if destination.is_some() {
            return Err("继承启动参数不能重复".into());
        }
        *destination = Some(PathBuf::from(
            arguments.next().ok_or("继承启动参数缺少路径")?,
        ));
    }
    crate::controller::finalize_inheritance(
        &manifest.ok_or("缺少 --manifest")?,
        &top.ok_or("缺少 --filesystem-root")?,
    )?;
    Ok(())
}
