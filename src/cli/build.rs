//! Select systems and dispatch versioned builds without a mandatory directory argument.
//! 选择系统并分派版本构建，不要求必需的目录参数。

use crate::config::{ConfigStore, DEFAULT_MOUNT_POINT};
use std::{ffi::OsString, path::PathBuf};

/// Help describing system selection, manager location, inheritance source, and scratch defaults.
/// 描述系统选择、管理器位置、继承来源和临时目录默认值的帮助。
const USAGE: &str = concat!(
    "用法：RStarSleep build [<系统标识> | --all] [--root <管理根绝对路径>] [--inheritance-root <继承来源根绝对路径>]\n\n",
    "省略系统或使用 --all 时构建全部系统；默认管理根为 /rstarsleep。\n",
    "默认自动选择继承来源：构建主系统且当前运行根属于该系统的某个版本时，使用当前运行根；否则使用该系统最近完成开机复制的版本。\n",
    "配置继承但没有来源时，构建前警告并等待 3 秒，本次不执行该系统的继承。\n",
    "--inheritance-root 仅可在显式指定一个系统时使用；继承路径相对于来源系统根解析；--root 仅选择管理存储。\n",
    "构建临时目录从全局 work_dir 读取，默认使用管理根下的 work/。\n\n",
    "Usage: RStarSleep build [<system-id> | --all] [--root <absolute-management-root>] [--inheritance-root <absolute-inheritance-root>]\n\n",
    "Omit the system or use --all to build every system; the management root defaults to /rstarsleep.\n",
    "Inheritance sources are selected automatically: use the running root when building the primary system and that root belongs to one of its versions; otherwise use that system's latest version that completed boot copying.\n",
    "If inheritance is configured but no source exists, warn and wait 3 seconds before building, then skip inheritance for that system.\n",
    "--inheritance-root requires an explicit single system; inheritance paths resolve within the source system root; --root only selects management storage.\n",
    "Temporary build directories use the global work_dir setting, which defaults to work/ under the management root."
);

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
    let (root, system, inheritance_root) = parse(arguments)?;
    let store = ConfigStore::new(root)?;
    let options = crate::controller::BuildOptions { inheritance_root };
    for outcome in crate::controller::build_with_options_and_backend_and_warning(
        &store,
        system.as_deref(),
        &options,
        &crate::controller::SystemBackend,
        warn_preflight,
    )? {
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
/// 在控制器分配版本或使缓存失效之前发出警告。
fn warn_preflight(warnings: &[String]) -> crate::controller::Result<()> {
    use std::io::Write;
    let report = || -> std::io::Result<()> {
        if !warnings.is_empty() {
            let mut stderr = std::io::stderr().lock();
            for warning in warnings {
                writeln!(stderr, "{warning}")?;
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
    };
    report().map_err(|source| crate::controller::Error::Io {
        path: "stderr".into(),
        source,
    })
}

/// Validate selection while preserving native bytes in optional manager and source paths.
/// 校验系统选择，保留可选管理路径和来源路径的原生字节。
///
/// # Arguments
///
/// - `arguments`: Native build arguments.
///   原生 build 参数。
fn parse(arguments: &[OsString]) -> super::Result<(PathBuf, Option<String>, Option<PathBuf>)> {
    let mut root = None;
    let mut inheritance_root = None;
    let mut system = None;
    let mut all = false;
    let mut arguments = arguments.iter();
    while let Some(argument) = arguments.next() {
        if argument == "--root" || argument == "--inheritance-root" {
            let (slot, flag, label, english_label) = if argument == "--root" {
                (&mut root, "--root", "管理根", "management root")
            } else {
                (
                    &mut inheritance_root,
                    "--inheritance-root",
                    "继承来源根",
                    "inheritance source root",
                )
            };
            if slot.is_some() {
                return Err(format!(
                    "{flag} 只能指定一次 / {flag} may only be specified once\n{USAGE}"
                )
                .into());
            }
            let path =
                PathBuf::from(arguments.next().ok_or_else(|| {
                    format!("{flag} 缺少路径 / {flag} is missing a path\n{USAGE}")
                })?);
            if !path.is_absolute() {
                return Err(format!(
                    "{label}必须为绝对路径 / {english_label} must be an absolute path"
                )
                .into());
            }
            *slot = Some(path);
        } else if argument == "--all" {
            if all || system.is_some() {
                return Err(format!("--all 不能重复或与系统标识同时使用 / --all must not be repeated or combined with a system identifier\n{USAGE}").into());
            }
            all = true;
        } else {
            let text = argument
                .to_str()
                .ok_or("系统标识必须是 UTF-8 / system identifier must be UTF-8")?;
            if text.starts_with('-') || all || system.is_some() {
                return Err(format!(
                    "无效构建参数: {text} / invalid build argument: {text}\n{USAGE}"
                )
                .into());
            }
            system = Some(text.to_owned());
        }
    }
    if inheritance_root.is_some() && system.is_none() {
        return Err(format!(
            "--inheritance-root 必须显式指定一个具体构建系统，不能省略系统或使用 --all / --inheritance-root requires an explicit single build system; omitting the system or using --all is not allowed\n{USAGE}"
        )
        .into());
    }
    Ok((
        root.unwrap_or_else(|| DEFAULT_MOUNT_POINT.into()),
        system,
        inheritance_root,
    ))
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
            (PathBuf::from(DEFAULT_MOUNT_POINT), None, None)
        );
        assert_eq!(
            parse(&arguments(&["--all"])).unwrap(),
            (PathBuf::from(DEFAULT_MOUNT_POINT), None, None)
        );
        for values in [
            vec!["desktop", "--root", "/mnt/manager"],
            vec!["--root", "/mnt/manager", "desktop"],
        ] {
            assert_eq!(
                parse(&arguments(&values)).unwrap(),
                (PathBuf::from("/mnt/manager"), Some("desktop".into()), None)
            );
        }
        assert_eq!(
            parse(&arguments(&["--all", "--root", "/mnt/manager"])).unwrap(),
            (PathBuf::from("/mnt/manager"), None, None)
        );
    }

    /// Keep the management and source roots independent regardless of argument order.
    /// 无论参数顺序如何，管理根和来源根都保持独立。
    #[test]
    fn selects_inheritance_root_independently_of_management_root() {
        for values in [
            vec![
                "desktop",
                "--root",
                "/mnt/manager",
                "--inheritance-root",
                "/mnt/source",
            ],
            vec![
                "--inheritance-root",
                "/mnt/source",
                "desktop",
                "--root",
                "/mnt/manager",
            ],
        ] {
            assert_eq!(
                parse(&arguments(&values)).unwrap(),
                (
                    PathBuf::from("/mnt/manager"),
                    Some("desktop".into()),
                    Some(PathBuf::from("/mnt/source"))
                )
            );
        }
    }

    /// Require an explicit single system before accepting an inheritance source override.
    /// 接受继承来源覆盖前，要求显式指定一个系统。
    #[test]
    fn inheritance_root_requires_an_explicit_single_system() {
        for values in [
            vec!["--inheritance-root", "/mnt/source"],
            vec!["--all", "--inheritance-root", "/mnt/source"],
            vec!["--inheritance-root", "/mnt/source", "--all"],
        ] {
            let error = parse(&arguments(&values)).unwrap_err().to_string();
            assert!(error.contains("必须显式指定一个具体构建系统"));
            assert!(error.contains("requires an explicit single build system"));
        }
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
            vec!["--inheritance-root"],
            vec!["--inheritance-root", "relative"],
            vec!["--inheritance-root", "/one", "--inheritance-root", "/two"],
            vec!["--inheritance-root", "--all"],
        ] {
            assert!(parse(&arguments(&values)).is_err(), "accepted {values:?}");
        }
        assert!(run(&arguments(&["--help"])).is_ok());
        assert!(run(&arguments(&["-h"])).is_ok());
    }

    /// Preserve non-UTF-8 manager and source paths while rejecting non-UTF-8 system identities.
    /// 保留非 UTF-8 管理器和来源路径，拒绝非 UTF-8 系统标识。
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
        let inheritance_root = OsString::from_vec(b"/mnt/source-\xfe".to_vec());
        let actual = parse(&[
            OsString::from("--root"),
            root.clone(),
            OsString::from("--inheritance-root"),
            inheritance_root.clone(),
            OsString::from("desktop"),
        ])
        .unwrap();
        assert_eq!(actual.0.as_os_str(), root);
        assert_eq!(actual.2.unwrap().as_os_str(), inheritance_root);
        assert!(parse(&[OsString::from_vec(b"system-\xff".to_vec())]).is_err());
    }
}
