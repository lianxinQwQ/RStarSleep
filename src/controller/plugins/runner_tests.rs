//! Verify preflight failures prevent plugin execution without using real containers.
//! 不使用真实容器，验证预检查失败会阻止插件执行。

use std::os::unix::fs::{PermissionsExt, symlink};

use super::*;
use crate::controller::plugins::{Metadata, ToolRequirements};
use std::fs;

/// Create an isolated plugin whose entry records execution using Bash builtins only.
/// 创建隔离插件；入口仅使用 Bash 内建命令记录执行。
///
/// # Arguments
///
/// - `host`: Declared host tool requirements.
///   声明的宿主工具需求。
/// - `system`: Declared target-system tool requirements.
///   声明的目标系统工具需求。
pub(crate) fn fixture(host: &[&str], system: &[&str]) -> LoadedPlugin {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("plugin");
    fs::create_dir(&root).unwrap();
    fs::create_dir(temporary.path().join("tmp")).unwrap();
    fs::create_dir(temporary.path().join("target")).unwrap();
    fs::create_dir(temporary.path().join("host-bin")).unwrap();
    for operation in crate::controller::plugins::REQUIRED_OPERATIONS {
        fs::write(
            root.join(format!("{operation}.sh")),
            "printf '%s\\n' \"${BASH_SOURCE[0]##*/}\" > \"$RSTAR_ROOT/executed\"\n",
        )
        .unwrap();
    }
    let plugin = LoadedPlugin {
        metadata: Metadata {
            commands: Vec::new(),
            mount_inheritance: false,
            query_package_files: false,
            boot: None,
            initramfs_inheritance: None,
            schema_version: 1,
            api_version: 2,
            id: "test".into(),
            version: "1".into(),
            system_types: vec!["test".into()],
            architectures: vec!["x86_64".into()],
            cache_format: "test-v1".into(),
            protected_packages: vec![],
            predownload: false,
            package_plan: None,
            persistent_dirs: vec![],
            cache: super::super::CacheRequirements {
                base_system: vec![],
                software: vec![],
            },
            config: super::super::ConfigRequirements {
                constraints: Default::default(),
                required: vec![],
                optional: vec!["RSTAR_DISTRIBUTION_ARCHITECTURE".into()],
            },
            tools: ToolRequirements {
                host: host.iter().map(|tool| (*tool).into()).collect(),
                system: system.iter().map(|tool| (*tool).into()).collect(),
                predownload: vec![],
            },
        },
        archive: "test.tar.gz".into(),
        archive_sha256: String::new(),
        discovery_warnings: Vec::new(),
        root,
        temporary,
    };
    let path = std::env::var_os("PATH").unwrap();
    let bash = executable("bash", Path::new("/"), &path).unwrap();
    symlink(bash, plugin.temporary.path().join("host-bin/bash")).unwrap();
    plugin
}

/// Write a fake nspawn that records requests and simulates the target dependency probe.
/// 写入模拟 nspawn，记录请求并模拟目标依赖探测。
///
/// # Arguments
///
/// - `plugin`: Test session owning the launcher and trace paths.
///   管理启动器及记录路径的测试会话。
/// - `missing`: Fixed missing tool returned by the probe, or an empty string.
///   探测返回的固定缺失工具，或空字符串。
/// - `fail`: Whether the probe exits unsuccessfully instead of reporting tool names.
///   是否让探测失败退出，而不是报告工具名。
pub(super) fn launcher(plugin: &LoadedPlugin, missing: &str, fail: bool) {
    let path = plugin.temporary.path().join("host-bin/systemd-nspawn");
    let text = format!(
        r#"#!/bin/sh
printf 'CALL\n%s\n' "$*" >> launcher-trace
printf 'CALL\0' >> launcher-argv
printf '%s\0' "$@" >> launcher-argv
for argument do
    if [ "$argument" = /bin/sh ]; then
        if [ {fail} = true ]; then printf 'probe failed\n' >&2; exit 7; fi
        if [ -n '{missing}' ]; then printf '%s\n' '{missing}'; fi
        exit 0
    fi
done
printf 'executed\n' > executed-inside
"#
    );
    fs::write(&path, text).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

/// Verify all missing host tools are reported and the entry script is never invoked.
/// 验证全部缺失宿主工具被报告，且入口脚本不会被调用。
///
/// # Arguments
///
/// No parameters.
/// 无参数。
#[test]
fn blocks_host_execution_and_reports_all_missing_tools() {
    let plugin = fixture(&["curl", "missing-tool"], &[]);
    let path = plugin.temporary.path().join("host-bin");
    let root = plugin.temporary.path().join("target");
    let error = plugin
        .invoke_with_path("bootstrap", &[], &root, "x86_64", path.as_os_str())
        .unwrap_err();
    match error {
        Error::MissingTools {
            plugin,
            operation,
            environment,
            tools,
        } => {
            assert_eq!(plugin, "test");
            assert_eq!(operation, "bootstrap");
            assert_eq!(environment, "宿主");
            assert_eq!(tools, ["curl", "missing-tool"]);
        }
        error => panic!("unexpected error: {error}"),
    }
    assert!(!root.join("executed").exists());
}

/// Verify a valid host invocation is independent of tools needed only inside the target.
/// 验证有效宿主调用不受仅在目标系统使用的工具影响。
///
/// # Arguments
///
/// No parameters.
/// 无参数。
#[test]
fn invokes_host_after_checks_without_checking_target_tools() {
    let plugin = fixture(&[], &["missing-target-tool"]);
    let path = plugin.temporary.path().join("host-bin");
    let root = plugin.temporary.path().join("target");
    plugin
        .invoke_with_path("bootstrap", &[], &root, "x86_64", path.as_os_str())
        .unwrap();
    assert_eq!(
        fs::read_to_string(root.join("executed")).unwrap(),
        "bootstrap.sh\n"
    );
}

/// Verify plugin-relative binaries require executable permission before invocation.
/// 验证插件内相对路径二进制必须具备可执行权限，才允许调用。
///
/// # Arguments
///
/// No parameters.
/// 无参数。
#[test]
fn checks_bundled_tools_and_rejects_non_executable_files() {
    let plugin = fixture(&["./bin/helper"], &[]);
    fs::create_dir(plugin.root().join("bin")).unwrap();
    let helper = plugin.root().join("bin/helper");
    fs::write(&helper, "tool").unwrap();
    fs::set_permissions(&helper, fs::Permissions::from_mode(0o644)).unwrap();
    let path = plugin.temporary.path().join("host-bin");
    let root = plugin.temporary.path().join("target");
    assert!(matches!(
        plugin.invoke_with_path("bootstrap", &[], &root, "x86_64", path.as_os_str()),
        Err(Error::MissingTools { .. })
    ));
    assert!(!root.join("executed").exists());
    fs::set_permissions(helper, fs::Permissions::from_mode(0o755)).unwrap();
    plugin
        .invoke_with_path("bootstrap", &[], &root, "x86_64", path.as_os_str())
        .unwrap();
}

/// Verify a target dependency is checked inside nspawn, not against the host PATH.
/// 验证目标依赖在 nspawn 内检查，而不是检查宿主 PATH。
///
/// # Arguments
///
/// No parameters.
/// 无参数。
#[test]
fn blocks_target_operation_before_plugin_entry_when_guest_tool_is_missing() {
    let plugin = fixture(&[], &["pacman"]);
    launcher(&plugin, "pacman", false);
    let path = plugin.temporary.path().join("host-bin");
    let root = plugin.temporary.path().join("target");
    let error = plugin
        .invoke_with_path("update", &[], &root, "x86_64", path.as_os_str())
        .unwrap_err();
    assert!(
        matches!(error, Error::MissingTools { environment, tools, .. } if environment == "目标系统" && tools == ["pacman"])
    );
    assert!(!plugin.root().join("executed-inside").exists());
    assert_eq!(
        fs::read_to_string(plugin.root().join("launcher-trace"))
            .unwrap()
            .lines()
            .filter(|line| *line == "CALL")
            .count(),
        1
    );
}

/// Inheritance requirements are checked only when the optional inheritance action runs.
/// 仅执行可选继承操作时检查继承依赖。
#[test]
fn checks_inheritance_tools_only_for_the_declared_action() {
    let mut plugin = fixture(&[], &["pacman"]);
    plugin.metadata.initramfs_inheritance = Some(super::super::ActionRequirements {
        packages: vec!["btrfs-progs".into()],
        tools: vec!["mkinitcpio".into(), "btrfs".into(), "pacman".into()],
    });
    fs::write(
        plugin.root().join("prepare_inheritance_initramfs.sh"),
        "exit 0\n",
    )
    .unwrap();
    let path = plugin.temporary.path().join("host-bin");
    let root = plugin.temporary.path().join("target");
    launcher(&plugin, "", false);
    plugin
        .invoke_with_path("update", &[], &root, "x86_64", path.as_os_str())
        .unwrap();
    let trace = fs::read_to_string(plugin.root().join("launcher-trace")).unwrap();
    assert!(!trace.contains("mkinitcpio"));
    assert!(!trace.contains("btrfs"));
    fs::remove_file(plugin.root().join("executed-inside")).unwrap();
    launcher(&plugin, "mkinitcpio", false);
    assert!(matches!(
        plugin.invoke_with_path("prepare_inheritance_initramfs", &[], &root, "x86_64", path.as_os_str()),
        Err(Error::MissingTools { tools, .. }) if tools == ["mkinitcpio"]
    ));
    assert!(!plugin.root().join("executed-inside").exists());
    launcher(&plugin, "", false);
    plugin
        .invoke_with_path(
            "prepare_inheritance_initramfs",
            &[],
            &root,
            "x86_64",
            path.as_os_str(),
        )
        .unwrap();
    assert!(plugin.root().join("executed-inside").is_file());
    plugin.metadata.initramfs_inheritance = None;
    assert!(matches!(
        plugin.invoke_with_path(
            "prepare_inheritance_initramfs",
            &[],
            &root,
            "x86_64",
            path.as_os_str()
        ),
        Err(Error::Invalid(_))
    ));
}

/// Failed processes preserve diagnostics from both output streams in their reported error.
/// 报告失败进程时保留两个输出流中的诊断信息。
#[test]
fn reports_stdout_and_stderr_for_failed_operations() {
    let plugin = fixture(&[], &[]);
    fs::write(
        plugin.operation_path("bootstrap").unwrap(),
        "printf 'actual failure\\n'\nprintf 'launcher note\\n' >&2\nexit 7\n",
    )
    .unwrap();
    let path = plugin.temporary.path().join("host-bin");
    let root = plugin.temporary.path().join("target");
    let error = plugin
        .invoke_with_path("bootstrap", &[], &root, "x86_64", path.as_os_str())
        .unwrap_err();
    let diagnostic = error.to_string();
    assert!(diagnostic.contains("stdout:\nactual failure"));
    assert!(diagnostic.contains("stderr:\nlauncher note"));
    assert!(
        matches!(error, Error::Execution { stdout, stderr, .. } if stdout == "actual failure" && stderr == "launcher note")
    );
}

/// Verify successful probes invoke the task with complete plugin mounts and target context.
/// 验证成功探测后，任务带完整插件挂载和目标上下文执行。
///
/// # Arguments
///
/// No parameters.
/// 无参数。
#[test]
fn invokes_target_only_after_successful_probe() {
    let plugin = fixture(&["missing-host-bootstrap-tool"], &["pacman"]);
    launcher(&plugin, "", false);
    let path = plugin.temporary.path().join("host-bin");
    let root = plugin.temporary.path().join("target");
    plugin
        .invoke_with_path(
            "install",
            &["vim".into()],
            &root,
            "x86_64",
            path.as_os_str(),
        )
        .unwrap();
    assert!(plugin.root().join("executed-inside").is_file());
    let trace = fs::read_to_string(plugin.root().join("launcher-trace")).unwrap();
    assert!(trace.contains("--bind-ro="));
    assert!(trace.contains("--setenv=RSTAR_DISTRIBUTION_ARCHITECTURE=x86_64"));
    assert!(trace.contains("bash /run/rstarsleep/plugin/install.sh vim"));
}

/// Preserve host arguments exactly and never prepend the operation name.
/// 原样保留宿主参数，且不将操作名插入参数列表。
///
/// # Arguments
///
/// No parameters.
/// 无参数。
#[test]
fn passes_only_operation_arguments_to_host_file() {
    let plugin = fixture(&[], &[]);
    fs::write(
        plugin.operation_path("bootstrap").unwrap(),
        r#"printf '%s\0' "$@" > "$RSTAR_ROOT/arguments""#,
    )
    .unwrap();
    let root = plugin.temporary.path().join("target");
    let path = plugin.temporary.path().join("host-bin");
    let arguments = [
        "two words".into(),
        "$(touch injected)".into(),
        "".into(),
        "--flag".into(),
    ];
    plugin
        .invoke_with_path("bootstrap", &arguments, &root, "x86_64", path.as_os_str())
        .unwrap();
    assert_eq!(
        fs::read(root.join("arguments")).unwrap(),
        b"two words\0$(touch injected)\0\0--flag\0"
    );
    assert!(!plugin.root().join("injected").exists());
}

/// Preserve guest argument boundaries after selecting the operation by filename.
/// 通过文件名选择操作后，保留容器参数边界。
///
/// # Arguments
///
/// No parameters.
/// 无参数。
#[test]
fn passes_only_operation_arguments_to_guest_file() {
    let plugin = fixture(&[], &[]);
    launcher(&plugin, "", false);
    let root = plugin.temporary.path().join("target");
    let path = plugin.temporary.path().join("host-bin");
    plugin
        .invoke_with_path(
            "mark_reason",
            &["dependency".into(), "two words".into(), "".into()],
            &root,
            "x86_64",
            path.as_os_str(),
        )
        .unwrap();
    let trace = fs::read(plugin.root().join("launcher-argv")).unwrap();
    assert!(
        trace.ends_with(
            b"--\0bash\0/run/rstarsleep/plugin/mark_reason.sh\0dependency\0two words\0\0"
        )
    );
}

/// Execute a shebang entry on the host without Bash while retaining arguments and context.
/// 宿主无 Bash 时执行 shebang 入口，保留参数和上下文。
#[test]
fn invokes_unsuffixed_host_entry_without_bash() {
    let mut plugin = fixture(&["missing-host-tool"], &[]);
    fs::remove_file(plugin.root().join("bootstrap.sh")).unwrap();
    let entry = plugin.root().join("bootstrap");
    fs::write(
        &entry,
        r#"#!/bin/sh
test "$PWD" = "$RSTAR_PLUGIN_ROOT" || exit 8
test -d "$RSTAR_PLUGIN_TMP" || exit 9
test "$RSTAR_DISTRIBUTION_ARCHITECTURE" = x86_64 || exit 10
printf '%s\0' "$@" > "$RSTAR_ROOT/arguments"
printf 'done\n'
"#,
    )
    .unwrap();
    fs::set_permissions(&entry, fs::Permissions::from_mode(0o755)).unwrap();
    let root = plugin.temporary.path().join("target");
    let path = plugin.temporary.path().join("host-bin");
    fs::remove_file(path.join("bash")).unwrap();
    assert!(matches!(
        plugin.invoke_with_path("bootstrap", &[], &root, "x86_64", path.as_os_str()),
        Err(Error::MissingTools { tools, .. }) if tools == ["missing-host-tool"]
    ));
    assert!(!root.join("arguments").exists());
    plugin.metadata.tools.host.clear();
    let arguments = ["two words".into(), "$(touch injected)".into(), "".into()];
    let output = plugin
        .invoke_with_path("bootstrap", &arguments, &root, "x86_64", path.as_os_str())
        .unwrap();
    assert_eq!(output.stdout, b"done\n");
    assert_eq!(
        fs::read(root.join("arguments")).unwrap(),
        b"two words\0$(touch injected)\0\0"
    );
    assert!(!plugin.root().join("injected").exists());
    fs::write(&entry, "#!/bin/sh\nprintf 'failed' >&2\nexit 7\n").unwrap();
    assert!(matches!(
        plugin.invoke_with_path("bootstrap", &[], &root, "x86_64", path.as_os_str()),
        Err(Error::Execution { stderr, .. }) if stderr == "failed"
    ));
}

/// Invoke executable entries directly in nspawn, including nested predownload lifecycle tasks.
/// 在 nspawn 中直接调用可执行入口，包括嵌套的预下载生命周期任务。
#[test]
fn invokes_unsuffixed_guest_entries_after_declared_tool_checks() {
    let plugin = fixture(&[], &["pacman"]);
    fs::create_dir(plugin.root().join("predownload")).unwrap();
    let root = plugin.temporary.path().join("target");
    let path = plugin.temporary.path().join("host-bin");
    fs::remove_file(plugin.root().join("install.sh")).unwrap();
    for (operation, relative) in [
        ("install", "install"),
        ("predownload_install", "predownload/install"),
        ("predownload_update", "predownload/update"),
    ] {
        let entry = plugin.root().join(relative);
        fs::write(&entry, "#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(&entry, fs::Permissions::from_mode(0o755)).unwrap();
        launcher(&plugin, "pacman", false);
        assert!(matches!(
            plugin.invoke_with_path(operation, &[], &root, "x86_64", path.as_os_str()),
            Err(Error::MissingTools { tools, .. }) if tools == ["pacman"]
        ));
        assert!(!plugin.root().join("executed-inside").exists());
        fs::remove_file(plugin.root().join("launcher-argv")).unwrap();
        launcher(&plugin, "", false);
        plugin
            .invoke_with_path(
                operation,
                &["two words".into(), "".into()],
                &root,
                "x86_64",
                path.as_os_str(),
            )
            .unwrap();
        let trace = fs::read(plugin.root().join("launcher-argv")).unwrap();
        assert!(
            trace
                .windows(b"rstarsleep-tool-check\0pacman\0".len())
                .any(|part| part == b"rstarsleep-tool-check\0pacman\0")
        );
        assert!(!trace.windows(b"bash\0".len()).any(|part| part == b"bash\0"));
        let expected = format!("--\0/run/rstarsleep/plugin/{relative}\0two words\0\0");
        assert!(trace.ends_with(expected.as_bytes()));
        fs::remove_file(plugin.root().join("launcher-argv")).unwrap();
        fs::remove_file(plugin.root().join("executed-inside")).unwrap();
    }
}

/// Reject missing operation files and traversal before any dependency probe or plugin runs.
/// 依赖探测或插件运行前，拒绝缺失操作文件与路径跳转。
///
/// # Arguments
///
/// No parameters.
/// 无参数。
#[test]
fn rejects_invalid_operation_files_before_starting_processes() {
    let plugin = fixture(&[], &[]);
    launcher(&plugin, "", false);
    let root = plugin.temporary.path().join("target");
    let path = plugin.temporary.path().join("host-bin");
    let update = plugin.operation_path("update").unwrap();
    fs::rename(&update, plugin.root().join("update")).unwrap();
    for operation in ["update", "../bootstrap", "/bootstrap", "missing"] {
        assert!(matches!(
            plugin.invoke_with_path(operation, &[], &root, "x86_64", path.as_os_str()),
            Err(Error::Invalid(_))
        ));
    }
    assert!(!plugin.root().join("launcher-trace").exists());
    assert!(!root.join("executed").exists());
}

/// Verify missing launchers and failed probes never start target plugin code.
/// 验证启动器缺失或探测失败时，不运行目标插件代码。
///
/// # Arguments
///
/// No parameters.
/// 无参数。
#[test]
fn rejects_missing_nspawn_and_failed_probes() {
    let plugin = fixture(&[], &[]);
    let path = plugin.temporary.path().join("host-bin");
    let root = plugin.temporary.path().join("target");
    assert!(
        matches!(plugin.invoke_with_path("update", &[], &root, "x86_64", path.as_os_str()), Err(Error::MissingTools { tools, .. }) if tools == ["systemd-nspawn"])
    );
    launcher(&plugin, "", true);
    assert!(matches!(
        plugin.invoke_with_path("update", &[], &root, "x86_64", path.as_os_str()),
        Err(Error::Execution { .. })
    ));
    assert!(!plugin.root().join("executed-inside").exists());
}

/// Exercise the real POSIX probe with isolated PATH entries and bundled relative files.
/// 使用隔离 PATH 项和随包相对文件验证真实 POSIX 探测。
///
/// # Arguments
///
/// No parameters.
/// 无参数。
#[test]
fn probe_checks_executable_files_in_the_actual_target_search_path() {
    let plugin = fixture(&[], &[]);
    fs::create_dir(plugin.root().join("bin")).unwrap();
    let existing = plugin.root().join("bin/available");
    fs::write(&existing, "tool").unwrap();
    fs::set_permissions(&existing, fs::Permissions::from_mode(0o755)).unwrap();
    let noexec = plugin.root().join("bin/noexec");
    fs::write(&noexec, "tool").unwrap();
    fs::set_permissions(noexec, fs::Permissions::from_mode(0o644)).unwrap();
    let output = Command::new("/bin/sh")
        .args([
            "-c",
            SYSTEM_PROBE,
            "tool-check",
            "available",
            "missing",
            "./bin/available",
            "./bin/noexec",
        ])
        .current_dir(plugin.root())
        .env("PATH", "bin")
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "missing\n./bin/noexec\n"
    );
}

#[test]
fn collector_tools_are_checked_only_for_collection_and_undeclared_queries_are_rejected() {
    let mut plugin = fixture(&[], &["pacman"]);
    fs::write(plugin.root().join("collect_packages.sh"), "exit 0\n").unwrap();
    let path = plugin.temporary.path().join("host-bin");
    let root = plugin.temporary.path().join("target");
    assert!(matches!(
        plugin.invoke_with_path("collect_packages", &[], &root, "x86_64", path.as_os_str()),
        Err(Error::Invalid(_))
    ));
    plugin.metadata.package_plan = Some(super::super::PackagePlanRequirements {
        base_packages: vec!["base".into()],
        tools: vec!["python3".into(), "pacman".into()],
    });
    launcher(&plugin, "", false);
    plugin
        .invoke_with_path("update", &[], &root, "x86_64", path.as_os_str())
        .unwrap();
    assert!(
        !fs::read_to_string(plugin.root().join("launcher-trace"))
            .unwrap()
            .contains("python3")
    );
    fs::remove_file(plugin.root().join("executed-inside")).unwrap();
    launcher(&plugin, "python3", false);
    assert!(
        matches!(plugin.invoke_with_path("collect_packages", &["base".into()], &root, "x86_64", path.as_os_str()), Err(Error::MissingTools {tools, ..}) if tools == ["python3"])
    );
    assert!(!plugin.root().join("executed-inside").exists());
}
