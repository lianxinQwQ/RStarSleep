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
        for directory in [
            "etc/pacman.d",
            "etc/ssh",
            "var/lib/dbus",
            "var/lib/systemd",
            "var/log",
            "tmp",
            "var/tmp",
        ] {
            fs::create_dir_all(temporary.path().join("root").join(directory)).unwrap();
        }
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
        let key = temporary.path().join("pacman-key");
        fs::write(
            &key,
            "#!/usr/bin/env bash\nprintf 'pacman-key %s\\n' \"$*\" >> \"$PACMAN_TRACE\"\n",
        )
        .unwrap();
        fs::set_permissions(&key, fs::Permissions::from_mode(0o755)).unwrap();
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
    /// - `operation`: Same-named operation file at the plugin root.
    ///   插件根内的同名操作文件。
    /// - `arguments`: Independently passed operation arguments.
    ///   独立传递的操作参数。
    /// - `case`: Mock query behavior, such as a forced database failure.
    ///   模拟查询行为，例如强制数据库失败。
    fn run(&self, operation: &str, arguments: &[&str], case: &str) -> Output {
        self.run_with_env(operation, arguments, case, &[])
    }

    /// Supply additional frozen build parameters to the real Bash operation.
    /// 向真实 Bash 操作提供额外的冻结构建参数。
    /// # Arguments
    /// - `self`: Isolated command environment. 隔离命令环境。
    /// - `operation`: Same-named operation file. 同名操作文件。
    /// - `arguments`: Separate operation arguments. 分别传递的操作参数。
    /// - `case`: Package-manager mock behavior. 包管理器模拟行为。
    /// - `environment`: Additional build variables. 额外构建变量。
    fn run_with_env(
        &self,
        operation: &str,
        arguments: &[&str],
        case: &str,
        environment: &[(&str, &str)],
    ) -> Output {
        let mut paths = vec![self.temporary.path().to_path_buf()];
        paths.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
        Command::new("bash")
            .arg(
                PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("presets/plugins/arch")
                    .join(format!("{operation}.sh")),
            )
            .args(arguments)
            .env("PATH", std::env::join_paths(paths).unwrap())
            .env("PACMAN_TRACE", &self.trace)
            .env("PACMAN_MOCK_CASE", case)
            .env("RSTAR_PLUGIN_TMP", self.temporary.path())
            .env("RSTAR_ROOT", self.temporary.path().join("root"))
            .envs(environment.iter().copied())
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

/// Verify inherited mounts are safely encoded and appended alongside user-supplied rows.
/// 验证继承挂载安全编码后与用户配置行一同追加。
/// # Arguments
/// No parameters. 无参数。
#[test]
fn inheritance_mounts_append_escaped_rows_and_extra_configuration() {
    let fixture = Fixture::new();
    let fstab = fixture.temporary.path().join("root/etc/fstab");
    fs::write(&fstab, "# installed defaults without final newline").unwrap();
    let output = fixture.run_with_env(
        "mount_inheritance",
        &[
            "UUID=1234",
            "/home/alice",
            "btrfs",
            "rw,subvol=@RStarSleep/systems/main/persistent/-home-alice",
            "/rstarsleep/file with\ttab\\slash#tag\nrow",
            "/etc/a b",
            "none",
            "bind,x-systemd.requires-mounts-for=/rstarsleep",
        ],
        "",
        &[(
            "RSTAR_CONTENT_FSTABLINES",
            "tmpfs /tmp tmpfs defaults 0 0\nUUID=abcd /data ext4 defaults 0 2",
        )],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read_to_string(&fstab).unwrap(),
        concat!(
            "# installed defaults without final newline\n",
            "# BEGIN RStarSleep inherited mounts\n",
            "UUID=1234 /home/alice btrfs rw,subvol=@RStarSleep/systems/main/persistent/-home-alice 0 0\n",
            "/rstarsleep/file\\040with\\011tab\\134slash\\043tag\\012row /etc/a\\040b none bind,x-systemd.requires-mounts-for=/rstarsleep 0 0\n",
            "tmpfs /tmp tmpfs defaults 0 0\n",
            "UUID=abcd /data ext4 defaults 0 2\n",
            "# END RStarSleep inherited mounts\n",
        )
    );
    assert!(fixture.trace().is_empty());
}

/// Extra rows work without inherited mounts; clearing them removes only the owned block.
/// 无继承挂载时仍处理额外行，清空时仅删除所属配置块。
/// # Arguments
/// No parameters. 无参数。
#[test]
fn inheritance_mounts_support_extra_rows_without_mount_arguments() {
    let fixture = Fixture::new();
    let fstab = fixture.temporary.path().join("root/etc/fstab");
    assert!(fixture.run("mount_inheritance", &[], "").status.success());
    assert!(!fstab.exists());
    let output = fixture.run_with_env(
        "mount_inheritance",
        &[],
        "",
        &[("RSTAR_CONTENT_FSTABLINES", "tmpfs /tmp tmpfs defaults 0 0")],
    );
    assert!(output.status.success());
    assert_eq!(
        fs::read_to_string(&fstab).unwrap(),
        "# BEGIN RStarSleep inherited mounts\ntmpfs /tmp tmpfs defaults 0 0\n# END RStarSleep inherited mounts\n"
    );
    assert_eq!(
        fs::metadata(&fstab).unwrap().permissions().mode() & 0o777,
        0o644
    );
    assert!(fixture.run("mount_inheritance", &[], "").status.success());
    assert_eq!(fs::read_to_string(&fstab).unwrap(), "");
}

/// Reusing persistent fstab replaces generated rows, without duplicating mounts or losing user rows.
/// 复用持久 fstab 时替换生成行，不重复挂载或丢失用户行。
/// # Arguments
/// No parameters. 无参数。
#[test]
fn inheritance_mounts_replace_managed_rows_on_repeated_builds() {
    let fixture = Fixture::new();
    let fstab = fixture.temporary.path().join("root/etc/fstab");
    let original = concat!(
        "# user before\nUUID=user /data ext4 defaults 0 2\n",
        "# BEGIN RStarSleep inherited mounts\nUUID=old /old btrfs rw 0 0\n",
        "# END RStarSleep inherited mounts\n# user after\n",
    );
    fs::write(&fstab, original).unwrap();
    let arguments = ["UUID=1234", "/home", "btrfs", "rw"];
    let environment = [("RSTAR_CONTENT_FSTABLINES", "tmpfs /tmp tmpfs defaults 0 0")];
    let first = fixture.run_with_env("mount_inheritance", &arguments, "", &environment);
    assert!(first.status.success());
    let expected = concat!(
        "# user before\nUUID=user /data ext4 defaults 0 2\n# user after\n",
        "# BEGIN RStarSleep inherited mounts\nUUID=1234 /home btrfs rw 0 0\n",
        "tmpfs /tmp tmpfs defaults 0 0\n# END RStarSleep inherited mounts\n",
    );
    assert_eq!(fs::read_to_string(&fstab).unwrap(), expected);
    for _ in 0..2 {
        let output = fixture.run_with_env("mount_inheritance", &arguments, "", &environment);
        assert!(output.status.success());
        assert_eq!(fs::read_to_string(&fstab).unwrap(), expected);
    }
}

/// Ambiguous ownership markers fail before replacing user configuration.
/// 所有权标记含歧义时，在替换用户配置前失败。
/// # Arguments
/// No parameters. 无参数。
#[test]
fn inheritance_mounts_reject_malformed_ownership_blocks() {
    let fixture = Fixture::new();
    let fstab = fixture.temporary.path().join("root/etc/fstab");
    let arguments = ["UUID=1234", "/home", "btrfs", "rw"];
    for content in [
        "# user\n# BEGIN RStarSleep inherited mounts\nUUID=old /old btrfs rw 0 0\n",
        "# END RStarSleep inherited mounts\n# user\n",
        "# BEGIN RStarSleep inherited mounts\n# BEGIN RStarSleep inherited mounts\n# END RStarSleep inherited mounts\n",
    ] {
        fs::write(&fstab, content).unwrap();
        assert_eq!(
            fixture
                .run("mount_inheritance", &arguments, "")
                .status
                .code(),
            Some(64)
        );
        assert_eq!(fs::read_to_string(&fstab).unwrap(), content);
    }
    let original = "# retained user\n";
    fs::write(&fstab, original).unwrap();
    for marker in [
        "# BEGIN RStarSleep inherited mounts",
        "# END RStarSleep inherited mounts",
    ] {
        assert_eq!(
            fixture
                .run_with_env(
                    "mount_inheritance",
                    &arguments,
                    "",
                    &[("RSTAR_CONTENT_FSTABLINES", marker)],
                )
                .status
                .code(),
            Some(64)
        );
        assert_eq!(fs::read_to_string(&fstab).unwrap(), original);
    }
}

/// Reject malformed groups and symbolic links before touching fstab; break preexisting hard links.
/// 修改 fstab 前拒绝错误参数组和符号链接，并断开原有硬链接。
/// # Arguments
/// No parameters. 无参数。
#[test]
fn inheritance_mounts_validate_before_writing_and_do_not_follow_file_links() {
    let fixture = Fixture::new();
    let root = fixture.temporary.path().join("root");
    let fstab = root.join("etc/fstab");
    let original = "# original fstab\n";
    fs::write(&fstab, original).unwrap();
    for args in [
        vec!["UUID=1234"],
        vec!["UUID=1234", "relative", "btrfs", "rw"],
        vec!["", "/home", "btrfs", "rw"],
        vec!["UUID=1234", "/home", "btrfs injected", "rw"],
        vec!["UUID=1234", "/home", "btrfs", ""],
        vec![
            "UUID=1234",
            "/home",
            "btrfs",
            "rw",
            "invalid",
            "relative",
            "none",
            "bind",
        ],
    ] {
        assert_eq!(
            fixture.run("mount_inheritance", &args, "").status.code(),
            Some(64)
        );
        assert_eq!(fs::read_to_string(&fstab).unwrap(), original);
    }
    let outside = fixture.temporary.path().join("outside-fstab");
    fs::write(&outside, original).unwrap();
    fs::remove_file(&fstab).unwrap();
    std::os::unix::fs::symlink(&outside, &fstab).unwrap();
    let arguments = ["UUID=1234", "/home", "btrfs", "rw"];
    assert_eq!(
        fixture
            .run("mount_inheritance", &arguments, "")
            .status
            .code(),
        Some(64)
    );
    assert_eq!(fs::read_to_string(&outside).unwrap(), original);
    fs::remove_file(&fstab).unwrap();
    fs::hard_link(&outside, &fstab).unwrap();
    assert!(
        fixture
            .run("mount_inheritance", &arguments, "")
            .status
            .success()
    );
    assert_eq!(fs::read_to_string(&outside).unwrap(), original);
    assert!(
        fs::read_to_string(&fstab)
            .unwrap()
            .contains("UUID=1234 /home btrfs rw 0 0")
    );
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
    let output = fixture.run("query_packages", &[], "");
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
    let empty = fixture.run("list_orphans", &[], "");
    assert!(empty.status.success());
    assert!(empty.stdout.is_empty());
    let failure = fixture.run("list_orphans", &[], "failure");
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
    assert!(fixture.run("install", &[], "").status.success());
    assert!(fixture.run("remove", &[], "").status.success());
    for arguments in [
        vec!["remove", "base"],
        vec!["remove_orphans", "base"],
        vec!["mark_reason", "dependency", "base"],
        vec!["install", "--malicious-option"],
    ] {
        assert_eq!(
            fixture.run(arguments[0], &arguments[1..], "").status.code(),
            Some(64)
        );
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
            .run("install", &["vim", "emacs"], "")
            .status
            .success()
    );
    assert!(
        fixture
            .trace()
            .contains("-S --needed --asexplicit --noconfirm --ask=4 -- vim emacs")
    );
    let resolved = fixture.run("resolve_packages", &["vim", "emacs"], "");
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
            .run("install", &["pipewire-pulse"], "conflict")
            .status
            .success()
    );
    let trace = fixture.trace();
    assert!(trace.contains("--ask=4 -- pipewire-pulse"));
    assert!(!trace.contains("--nodeps"));
    assert!(!trace.contains("-Rdd"));
}

/// Verify split maintenance files retain their arguments and shared helper behavior.
/// 验证拆分后的维护文件保留参数语义和公共函数行为。
///
/// # Arguments
///
/// No parameters.
/// 无参数。
#[test]
fn runs_split_maintenance_operations_with_mock_system_tools() {
    let fixture = Fixture::new();
    for (operation, arguments) in [
        ("base_install", vec![]),
        ("update", vec![]),
        ("mark_reason", vec!["explicit", "vim"]),
        ("mark_reason", vec!["dependency", "vim"]),
        ("remove", vec!["vim"]),
        ("remove_orphans", vec!["libdependency"]),
        ("clean_orphans", vec![]),
    ] {
        let output = fixture.run(operation, &arguments, "");
        assert!(
            output.status.success(),
            "{operation} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let trace = fixture.trace();
    for command in [
        "pacman-key --init",
        "pacman-key --populate archlinux",
        "-Syu --needed --asexplicit --noconfirm -- base",
        "-D --asdeps -- vim libdependency",
        "-D --asexplicit -- base",
        "-Syu --noconfirm",
        "-D --asexplicit -- vim",
        "-D --asdeps -- vim",
        "-R --noconfirm -- vim",
        "-Rns --noconfirm -- libdependency",
    ] {
        assert!(
            trace.lines().any(|line| line == command),
            "missing {command}: {trace}"
        );
    }
}

/// Verify a system clone receives the requested identity and a fresh local signing keyring.
/// 验证系统副本获得指定身份及新的本地签名密钥环。
/// # Arguments
///
/// No parameters.
/// 无参数。
#[test]
fn initializes_independent_system_identity() {
    let fixture = Fixture::new();
    let root = fixture.temporary.path().join("root");
    let id = "1234567890abcdef1234567890abcdef";
    let output = fixture.run("system_init", &[id], "");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    for name in ["etc/machine-id", "var/lib/dbus/machine-id"] {
        assert_eq!(
            fs::read_to_string(root.join(name)).unwrap(),
            format!("{id}\n")
        );
    }
    let trace = fixture.trace();
    let gpgdir = root.join("etc/pacman.d/gnupg");
    assert!(trace.contains(&format!("pacman-key --gpgdir {} --init", gpgdir.display())));
    assert!(trace.contains(&format!(
        "pacman-key --gpgdir {} --populate archlinux",
        gpgdir.display()
    )));
}

/// Reject invalid identities and redirected state directories before any mutation.
/// 在任何修改前拒绝无效身份及被重定向的状态目录。
/// # Arguments
///
/// No parameters.
/// 无参数。
#[test]
fn identity_operations_reject_invalid_inputs_and_directory_links() {
    let fixture = Fixture::new();
    for args in [
        vec![],
        vec!["bad"],
        vec!["00000000000000000000000000000000"],
        vec!["1234567890abcdef1234567890abcdef", "extra"],
    ] {
        assert_eq!(
            fixture.run("system_init", &args, "").status.code(),
            Some(64)
        );
    }
    let root = fixture.temporary.path().join("root");
    let outside = fixture.temporary.path().join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("keep"), "retained").unwrap();
    fs::remove_dir(root.join("var/log")).unwrap();
    std::os::unix::fs::symlink(&outside, root.join("var/log")).unwrap();
    assert_eq!(
        fixture
            .run("system_init", &["1234567890abcdef1234567890abcdef"], "")
            .status
            .code(),
        Some(64)
    );
    assert!(outside.join("keep").is_file());
    assert!(fixture.trace().is_empty());
}

#[test]
fn arch_preparation_protocol_handles_dependencies_providers_and_corruption() {
    let output = Command::new("python3")
        .arg(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/arch_predownload.py"))
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn shared_cache_options_precede_the_package_argument_delimiter() {
    let fixture = Fixture::new();
    let resources = fixture.temporary.path().join("plugin");
    fs::create_dir_all(resources.join("lib")).unwrap();
    fs::create_dir_all(resources.join("state/packages")).unwrap();
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("presets/plugins/arch");
    fs::copy(
        source.join("lib/common.sh"),
        resources.join("lib/common.sh"),
    )
    .unwrap();
    let mut paths = vec![fixture.temporary.path().to_path_buf()];
    paths.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
    let output = Command::new("bash")
        .arg(source.join("install.sh"))
        .args(["vim", "git"])
        .env("PATH", std::env::join_paths(paths).unwrap())
        .env("PACMAN_TRACE", &fixture.trace)
        .env("RSTAR_PLUGIN_ROOT", &resources)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let trace = fixture.trace();
    assert!(trace.starts_with("--cachedir "));
    assert!(trace.ends_with("-- vim git\n"));
}
