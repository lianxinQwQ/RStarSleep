//! Execute copied script payloads inside their target system, never on the host.
//! 在目标系统内部执行已复制的脚本资源。
use crate::controller::{BuildContext, Error, Result, container, fs as disk};
use std::{path::Path, process::Command};

/// Build a container invocation whose cwd is the copied payload directory.
/// 构造以已复制资源目录为工作目录的容器调用。
/// # Arguments
/// - `root`: Writable system root.
///   可写系统根。
/// - `directory`: Absolute guest payload directory.
///   容器内资源绝对目录。
/// - `entrypoint`: Root-relative regular shell file.
///   相对资源根的普通 shell 文件。
/// - `context`: Frozen configuration and system identity.
///   冻结配置及系统标识。
fn command(
    root: &Path,
    directory: &Path,
    entrypoint: &Path,
    context: &BuildContext,
) -> Result<Command> {
    let mut command = container::command(Path::new("systemd-nspawn"), root);
    command
        .current_dir(root)
        .arg(format!("--chdir={}", directory.display()))
        .arg(container::bind(
            "--bind-ro=",
            &context.config,
            "/run/rstarsleep/config",
        )?)
        .arg("--setenv=RSTAR_CONFIG_DIR=/run/rstarsleep/config")
        .arg(format!("--setenv=RSTAR_SYSTEM_ID={}", context.system_id))
        .arg("--setenv=RSTAR_ROOT=/");
    for (name, value) in &context.parameters {
        let name = super::super::plugins::configuration::environment_name(name)?;
        command.env(&name, value).arg(format!("--setenv={name}"));
    }
    if !context
        .parameters
        .contains_key("RSTAR_DISTRIBUTION_ARCHITECTURE")
    {
        command
            .env("RSTAR_DISTRIBUTION_ARCHITECTURE", &context.architecture)
            .arg("--setenv=RSTAR_DISTRIBUTION_ARCHITECTURE");
    }
    command
        .env(
            "RSTAR_PARAMETERS_JSON",
            serde_json::to_string(&context.parameter_lists)
                .map_err(|e| Error::Invalid(e.to_string()))?,
        )
        .arg("--setenv=RSTAR_PARAMETERS_JSON");
    container::environment(&mut command, &context.environment);
    command.arg("--").arg("bash").arg("--").arg(entrypoint);
    Ok(command)
}

/// Run Bash inside nspawn and propagate failed exits before the script stage is committed.
/// 在 nspawn 内运行 Bash，在脚本阶段提交前传播失败退出。
/// # Arguments
/// - `root`: Writable system root.
///   可写系统根。
/// - `directory`: Absolute guest payload directory.
///   容器内资源绝对目录。
/// - `entrypoint`: Root-relative script filename.
///   相对资源根的脚本文件名。
/// - `context`: Frozen inputs.
///   冻结输入。
pub(in crate::controller) fn run(
    root: &Path,
    directory: &Path,
    entrypoint: &Path,
    context: &BuildContext,
) -> Result<()> {
    let output = crate::controller::output::stream(
        &mut command(root, directory, entrypoint, context)?,
        false,
    )
    .map_err(|e| disk::io(Path::new("systemd-nspawn"), e))?;
    if !output.status.success() {
        return Err(Error::Command {
            command: format!("script {}", entrypoint.display()),
            status: output.status.to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).into(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    /// Verify target execution, copied cwd, frozen binds, and argument boundaries.
    /// 验证目标执行、已复制工作目录、冻结绑定及参数边界。
    /// # Arguments
    /// No parameters.
    /// 无参数。
    #[test]
    fn command_runs_inside_the_system_with_no_payload_bind() {
        let context = BuildContext {
            system_id: "desktop".into(),
            architecture: "x86_64".into(),
            distribution_options: Default::default(),
            parameters: std::collections::BTreeMap::from([
                (
                    "RSTAR_DISTRIBUTION_MIRROR".into(),
                    "https://example.invalid/$repo/$arch".into(),
                ),
                ("RSTAR_SECRET".into(), "private $value".into()),
            ]),
            config: "/manager/config".into(),
            persistent_mounts: Vec::new(),
            parameter_lists: std::collections::BTreeMap::from([(
                "RSTAR_SECRET".into(),
                vec!["private $value".into()],
            )]),
            environment: std::collections::BTreeMap::from([
                (
                    "https_proxy".into(),
                    Some("http://localhost:10808/a=b with spaces".into()),
                ),
                ("UNSET".into(), None),
            ]),
            inherited_mounts: Vec::new(),
        };
        let root = Path::new("/work/script-root");
        let command = command(
            root,
            Path::new("/.rstarsleep-script-123"),
            Path::new("-setup.sh"),
            &context,
        )
        .unwrap();
        assert_eq!(command.get_program(), "systemd-nspawn");
        assert_eq!(command.get_current_dir(), Some(root));
        let args: Vec<OsString> = command.get_args().map(Into::into).collect();
        assert!(
            !args
                .iter()
                .any(|arg| arg.to_string_lossy().contains("private $value"))
        );
        assert!(args.contains(&"--setenv=RSTAR_SECRET".into()));
        assert!(args.contains(&"--setenv=RSTAR_PARAMETERS_JSON".into()));
        assert!(command.get_envs().any(
            |(name, value)| name == "RSTAR_SECRET" && value == Some("private $value".as_ref())
        ));
        let json = serde_json::to_string(&context.parameter_lists).unwrap();
        assert!(
            command.get_envs().any(
                |(name, value)| name == "RSTAR_PARAMETERS_JSON" && value == Some(json.as_ref())
            )
        );
        assert!(
            args.windows(2)
                .any(|pair| pair == [OsString::from("--directory"), root.into()])
        );
        assert!(args.contains(&"--chdir=/.rstarsleep-script-123".into()));
        assert!(args.contains(&"--bind-ro=/manager/config:/run/rstarsleep/config".into()));
        let delimiter = args.iter().position(|argument| argument == "--").unwrap();
        assert!(
            args[..delimiter]
                .contains(&"--setenv=https_proxy=http://localhost:10808/a=b with spaces".into())
        );
        assert!(
            !args
                .iter()
                .any(|arg| arg.to_string_lossy().starts_with("--setenv=UNSET"))
        );
        assert!(
            command
                .get_envs()
                .any(|(name, value)| name == "UNSET" && value.is_none())
        );
        assert!(args[..delimiter].contains(&"--setenv=RSTAR_DISTRIBUTION_MIRROR".into()));
        assert!(args[..delimiter].contains(&"--setenv=RSTAR_DISTRIBUTION_ARCHITECTURE".into()));
        assert!(command.get_envs().any(|(name, value)| {
            name == "RSTAR_DISTRIBUTION_MIRROR"
                && value == Some("https://example.invalid/$repo/$arch".as_ref())
        }));
        assert_eq!(
            args.iter()
                .filter(|arg| arg.to_string_lossy().starts_with("--bind"))
                .count(),
            1
        );
        assert!(args.ends_with(&["--".into(), "bash".into(), "--".into(), "-setup.sh".into()]));
    }

    /// Reject ambiguous configuration binds and preserve native shell filenames.
    /// 拒绝有歧义的配置绑定，并保留原生 shell 文件名。
    /// # Arguments
    /// No parameters.
    /// 无参数。
    #[cfg(unix)]
    #[test]
    fn command_preserves_native_names_and_rejects_ambiguous_bind_paths() {
        use std::os::unix::ffi::OsStringExt;
        let mut context = BuildContext {
            system_id: "desktop".into(),
            architecture: "x86_64".into(),
            parameters: Default::default(),
            distribution_options: Default::default(),
            config: "/manager/config".into(),
            persistent_mounts: Vec::new(),
            parameter_lists: Default::default(),
            environment: Default::default(),
            inherited_mounts: Vec::new(),
        };
        let name = OsString::from_vec(b"shell-\xff.sh".to_vec());
        let cmd = command(
            Path::new("/root"),
            Path::new("/payload"),
            Path::new(&name),
            &context,
        )
        .unwrap();
        assert_eq!(cmd.get_args().last().unwrap(), name);
        context.config = "/manager/config:other".into();
        assert!(
            command(
                Path::new("/root"),
                Path::new("/payload"),
                Path::new(&name),
                &context
            )
            .is_err()
        );
    }
}
