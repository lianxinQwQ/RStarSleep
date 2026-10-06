//! Verify the preset Bash interface through mock package-manager commands.
//! 通过模拟包管理命令验证预设 Bash 接口。
#![cfg(unix)]

use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    process::{Command, Output},
};

/// Isolated command environment preventing tests from changing the host package database.
/// 隔离命令环境，避免测试修改宿主软件包数据库。
struct Fixture {
    temporary: tempfile::TempDir,
    trace: PathBuf,
}

impl Fixture {
    /// Create a mock pacman and an empty trace file inside a temporary directory.
    /// 在临时目录中创建模拟 pacman 和空调用记录文件。
    ///
    /// # Arguments
    ///
    /// No parameters.
    /// 无参数。
    fn new() -> Self {
        let temporary = tempfile::tempdir().unwrap();
        let executable = temporary.path().join("pacman");
        fs::write(
            &executable,
            r#"#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >> "$PACMAN_TRACE"
case "$1" in
    -Qq) printf 'base\nvim\nlibdependency\n' ;;
    -Qqe) printf 'base\nvim\n' ;;
    -Qdtq)
        if [[ ${PACMAN_MOCK_CASE:-} == failure ]]; then
            printf 'database failure\n' >&2
            exit 2
        fi
        exit 1
        ;;
    -Si) printf 'Name : %s\n' "${@: -1}" ;;
    -S)
        if [[ ${PACMAN_MOCK_CASE:-} == conflict && " $* " != *" --ask=4 "* ]]; then
            printf 'conflicting package removal refused\n' >&2
            exit 1
        fi
        exit 0
        ;;
    *) exit 0 ;;
esac
"#,
        )
        .unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).unwrap();
        let trace = temporary.path().join("trace");
        fs::write(&trace, "").unwrap();
        Self { temporary, trace }
    }

    /// Invoke the real Bash plugin while replacing all pacman commands with the mock.
    /// 调用真实 Bash 插件，将所有 pacman 命令替换为模拟实现。
    ///
    /// # Arguments
    ///
    /// - `self`: Fixture providing the mock executable and temporary query directory.
    ///   提供模拟可执行文件和临时查询目录的测试环境。
    /// - `arguments`: Plugin operation and its independently passed arguments.
    ///   插件操作及独立传递的参数。
    /// - `case`: Mock query behavior, such as a forced database failure.
    ///   模拟查询行为，例如强制数据库失败。
    fn run(&self, arguments: &[&str], case: &str) -> Output {
        let mut paths = vec![self.temporary.path().to_path_buf()];
        paths.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
        Command::new("bash")
            .arg(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("presets/plugins/arch/plugin.sh"))
            .args(arguments)
            .env("PATH", std::env::join_paths(paths).unwrap())
            .env("PACMAN_TRACE", &self.trace)
            .env("PACMAN_MOCK_CASE", case)
            .env("RSTAR_PLUGIN_TMP", self.temporary.path())
            .output()
            .unwrap()
    }

    /// Read the mock's command trace for argument and no-operation assertions.
    /// 读取模拟命令记录，用于参数和空操作断言。
    ///
    /// # Arguments
    ///
    /// - `self`: Fixture owning the trace file.
    ///   管理记录文件的测试环境。
    fn trace(&self) -> String {
        fs::read_to_string(&self.trace).unwrap()
    }
}

/// Verify that package queries report normalized explicit/dependency installation reasons.
/// 验证包查询输出规范化的显式及依赖安装原因。
///
/// # Arguments
///
/// No parameters.
/// 无参数。
#[test]
fn queries_installation_reasons() {
    let fixture = Fixture::new();
    let output = fixture.run(&["query_packages"], "");
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "base\texplicit\nvim\texplicit\nlibdependency\tdependency\n"
    );
}

/// Verify an empty orphan query succeeds while a real database failure remains a failure.
/// 验证空孤儿查询成功，而真正的数据库失败仍然失败。
///
/// # Arguments
///
/// No parameters.
/// 无参数。
#[test]
fn distinguishes_empty_queries_from_database_errors() {
    let fixture = Fixture::new();
    let empty = fixture.run(&["list_orphans"], "");
    assert!(empty.status.success());
    assert!(empty.stdout.is_empty());
    let failure = fixture.run(&["list_orphans"], "failure");
    assert_eq!(failure.status.code(), Some(2));
    assert!(
        String::from_utf8(failure.stderr)
            .unwrap()
            .contains("database failure")
    );
}

/// Verify no targets and protected/invalid targets never invoke a mutating package operation.
/// 验证空目标及受保护或无效目标不会调用修改包状态的操作。
///
/// # Arguments
///
/// No parameters.
/// 无参数。
#[test]
fn skips_empty_operations_and_protects_base() {
    let fixture = Fixture::new();
    assert!(fixture.run(&["install"], "").status.success());
    assert!(fixture.run(&["remove"], "").status.success());
    for arguments in [
        vec!["remove", "base"],
        vec!["remove_orphans", "base"],
        vec!["mark_reason", "dependency", "base"],
        vec!["install", "--malicious-option"],
    ] {
        assert_eq!(fixture.run(&arguments, "").status.code(), Some(64));
    }
    assert!(fixture.trace().is_empty());
}

/// Verify requested packages are installed explicitly with arguments separated from options.
/// 验证所需包以显式安装方式传递，包参数与选项分隔。
///
/// # Arguments
///
/// No parameters.
/// 无参数。
#[test]
fn installs_requested_targets_and_resolves_concrete_names() {
    let fixture = Fixture::new();
    assert!(
        fixture
            .run(&["install", "vim", "emacs"], "")
            .status
            .success()
    );
    assert!(
        fixture
            .trace()
            .contains("-S --needed --asexplicit --noconfirm --ask=4 -- vim emacs")
    );
    let resolved = fixture.run(&["resolve_packages", "vim", "emacs"], "");
    assert!(resolved.status.success());
    assert_eq!(String::from_utf8(resolved.stdout).unwrap(), "vim\nemacs\n");
}

/// Verify a conflicting package switch is delegated to libalpm with dependency checks intact.
/// 验证冲突包切换交由 libalpm 执行且保留依赖检查。
/// # Arguments
///
/// No parameters.
/// 无参数。
#[test]
fn permits_conflict_replacement_in_the_install_transaction() {
    let fixture = Fixture::new();
    assert!(
        fixture
            .run(&["install", "pipewire-pulse"], "conflict")
            .status
            .success()
    );
    let trace = fixture.trace();
    assert!(trace.contains("--ask=4 -- pipewire-pulse"));
    assert!(!trace.contains("--nodeps"));
    assert!(!trace.contains("-Rdd"));
}
