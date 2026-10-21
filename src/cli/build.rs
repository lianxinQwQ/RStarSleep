//! Select systems and dispatch versioned builds without a mandatory directory argument.
//! 选择系统并分派版本构建，不要求必需的目录参数。

use crate::config::{ConfigStore, DEFAULT_MOUNT_POINT};
use std::{ffi::OsString, path::PathBuf};

/// Help describing system selection, manager location, and scratch defaults.
/// 描述系统选择、管理器位置和临时目录默认值的帮助。
const USAGE: &str = "用法：RStarSleep build [<系统标识> | --all] [--root <管理根绝对路径>]\n\n省略系统或使用 --all 时构建全部系统；默认管理根为 /rstarsleep。\n构建临时目录从全局 work_dir 读取，默认使用管理根下的 work/。";

/// Parse arguments and run one system or every managed system.
/// 解析参数并构建一个系统或全部被管理系统。
///
/// # Arguments
///
/// - `arguments`: Native arguments after the build subcommand.
///   build 子命令之后的原生参数。
///
/// # Errors
///
/// Returns argument, configuration, or build failures.
/// 返回参数、配置或构建错误。
pub(super) fn run(arguments: &[OsString]) -> super::Result<()> {
    if arguments == [OsString::from("--help")] || arguments == [OsString::from("-h")] {
        println!("{USAGE}");
        return Ok(());
    }
    let (root, system) = parse(arguments)?;
    let store = ConfigStore::new(root)?;
    warn_inheritance(&store, system.as_deref())?;
    for outcome in crate::controller::build(&store, system.as_deref())? {
        println!(
            "{}: {} -> {}",
            outcome.system_id,
            outcome.version,
            outcome.root.display()
        );
    }
    Ok(())
}

/// Warn before the controller allocates versions or invalidates caches.
fn warn_inheritance(store: &ConfigStore, system: Option<&str>) -> super::Result<()> {
    use std::io::Write;
    let systems = match system {
        Some(id) => vec![id.to_owned()],
        None => store.list_systems()?,
    };
    let mut warnings = Vec::new();
    for id in systems {
        let layout = store.system_layout(&id)?;
        let parameters = crate::config::read_plugin_parameters_at(&layout.config)?;
        let config = crate::config::InheritanceConfig::read(&parameters)?;
        if !config.copy_on_write_build.is_empty() || !config.copy_on_write_boot.is_empty() {
            warnings.push(format!(
                "警告：系统 {id} 将在构建时继承 {:?}，首次启动时继承 {:?}；DirectorySubvolume={}，NocowHandling={}。\n继承路径不设黑名单，内容及兼容性由用户负责；启动时继承使用的内核与启动镜像固定为构建时产物。\nWarning: system {id} will inherit the paths above at build time and first boot, respectively, with the listed directory subvolume setting. Inherited content and compatibility are user-managed; boot-time inheritance uses the kernel and boot image produced by this build.\n",
                config.copy_on_write_build, config.copy_on_write_boot,
                if config.directory_subvolume { "yes" } else { "no" }, config.nocow_handling));
            if config.nocow_handling == crate::config::NocowHandling::MoveSnapshot {
                warnings.push("NocowHandling=move-snapshot 会将普通来源目录转换为子卷，并可能进行完整复制。该操作修改实际来源，要求来源离线且已停止写入。 / Converts actual offline source directories into subvolumes; migration may fully copy data.\n".into());
            }
        }
    }
    if !warnings.is_empty() {
        let mut stderr = std::io::stderr().lock();
        for warning in warnings {
            write!(stderr, "{warning}")?;
        }
        writeln!(
            stderr,
            "将在 3 秒后开始构建；按 Ctrl+C 取消。 / Build starts in 3 seconds; press Ctrl+C to cancel."
        )?;
        stderr.flush()?;
        drop(stderr);
        std::thread::sleep(std::time::Duration::from_secs(3));
    }
    Ok(())
}

/// Validate selection while preserving native bytes in an optional manager path.
/// 校验系统选择，保留可选管理路径的原生字节。
///
/// # Arguments
///
/// - `arguments`: Native build arguments.
///   原生 build 参数。
fn parse(arguments: &[OsString]) -> super::Result<(PathBuf, Option<String>)> {
    let mut root = None;
    let mut system = None;
    let mut all = false;
    let mut arguments = arguments.iter();
    while let Some(argument) = arguments.next() {
        if argument == "--root" {
            if root.is_some() {
                return Err(format!("--root 只能指定一次\n{USAGE}").into());
            }
            let path = PathBuf::from(
                arguments
                    .next()
                    .ok_or_else(|| format!("--root 缺少路径\n{USAGE}"))?,
            );
            if !path.is_absolute() {
                return Err("管理根必须为绝对路径".into());
            }
            root = Some(path);
        } else if argument == "--all" {
            if all || system.is_some() {
                return Err(format!("--all 不能重复或与系统标识同时使用\n{USAGE}").into());
            }
            all = true;
        } else {
            let text = argument.to_str().ok_or("系统标识必须是 UTF-8")?;
            if text.starts_with('-') || all || system.is_some() {
                return Err(format!("无效构建参数: {text}\n{USAGE}").into());
            }
            system = Some(text.to_owned());
        }
    }
    Ok((root.unwrap_or_else(|| DEFAULT_MOUNT_POINT.into()), system))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Convert UTF-8 fixtures into native command-line arguments.
    /// 将 UTF-8 测试值转换为原生命令行参数。
    ///
    /// # Arguments
    ///
    /// - `values`: Arguments after the build subcommand.
    ///   build 子命令之后的参数。
    fn arguments(values: &[&str]) -> Vec<OsString> {
        values.iter().map(OsString::from).collect()
    }

    /// Verify default and explicit all-system selection and optional manager-root placement.
    /// 验证默认和显式全系统选择，以及可选的管理根位置。
    ///
    /// # Arguments
    ///
    /// No parameters.
    /// 无参数。
    #[test]
    fn selects_all_by_default_and_accepts_one_system_in_either_order() {
        assert_eq!(
            parse(&[]).unwrap(),
            (PathBuf::from(DEFAULT_MOUNT_POINT), None)
        );
        assert_eq!(
            parse(&arguments(&["--all"])).unwrap(),
            (PathBuf::from(DEFAULT_MOUNT_POINT), None)
        );
        for values in [
            vec!["desktop", "--root", "/mnt/manager"],
            vec!["--root", "/mnt/manager", "desktop"],
        ] {
            assert_eq!(
                parse(&arguments(&values)).unwrap(),
                (PathBuf::from("/mnt/manager"), Some("desktop".into()))
            );
        }
        assert_eq!(
            parse(&arguments(&["--all", "--root", "/mnt/manager"])).unwrap(),
            (PathBuf::from("/mnt/manager"), None)
        );
    }

    /// Reject conflicting selectors, repeated roots, unknown flags, and missing root values.
    /// 拒绝冲突选择器、重复根、未知参数和缺失的根值。
    ///
    /// # Arguments
    ///
    /// No parameters.
    /// 无参数。
    #[test]
    fn rejects_ambiguous_or_incomplete_arguments() {
        for values in [
            vec!["desktop", "--all"],
            vec!["--all", "desktop"],
            vec!["--all", "--all"],
            vec!["desktop", "server"],
            vec!["--root"],
            vec!["--root", "relative"],
            vec!["--unknown"],
            vec!["--root", "/one", "--root", "/two"],
        ] {
            assert!(parse(&arguments(&values)).is_err(), "accepted {values:?}");
        }
        assert!(run(&arguments(&["--help"])).is_ok());
        assert!(run(&arguments(&["-h"])).is_ok());
    }

    /// Preserve non-UTF-8 manager paths while rejecting non-UTF-8 system identities.
    /// 保留非 UTF-8 管理器路径，拒绝非 UTF-8 系统标识。
    ///
    /// # Arguments
    ///
    /// No parameters.
    /// 无参数。
    #[cfg(unix)]
    #[test]
    fn preserves_native_path_bytes() {
        use std::os::unix::ffi::OsStringExt;
        let root = OsString::from_vec(b"/mnt/manager-\xff".to_vec());
        let actual = parse(&[
            OsString::from("--root"),
            root.clone(),
            OsString::from("desktop"),
        ])
        .unwrap();
        assert_eq!(actual.0.as_os_str(), root);
        assert!(parse(&[OsString::from_vec(b"system-\xff".to_vec())]).is_err());
    }
}
