//! Verify optional root password configuration without touching host account data.
//! 验证可选 root 密码配置，不接触宿主账户数据。
#![cfg(unix)]

use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Output},
};

struct Fixture {
    temporary: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        let temporary = tempfile::tempdir().unwrap();
        let mock = temporary.path().join("chpasswd");
        fs::create_dir_all(temporary.path().join("root/etc")).unwrap();
        fs::write(temporary.path().join("root/etc/shadow"), "unchanged\n").unwrap();
        fs::write(
            &mock,
            r#"#!/usr/bin/env bash
set -euo pipefail
[[ $# == 2 && $1 == --root && $2 == "$POST_MAINTENANCE_TEST_ROOT/root" ]] || exit 91
printf '%s\0' "$@" > "$POST_MAINTENANCE_TEST_ROOT/arguments"
[[ ! ${RSTAR_CONTENT_ROOTPASSWORD+x} ]] || exit 92
cat > "$POST_MAINTENANCE_TEST_ROOT/input"
# Deliberately echo the secret to verify that failed command output stays private.
cat "$POST_MAINTENANCE_TEST_ROOT/input"
cat "$POST_MAINTENANCE_TEST_ROOT/input" >&2
exit "${POST_MAINTENANCE_TEST_STATUS:-0}"
"#,
        )
        .unwrap();
        fs::set_permissions(mock, fs::Permissions::from_mode(0o755)).unwrap();
        Self { temporary }
    }

    fn path(&self) -> &Path {
        self.temporary.path()
    }

    fn run(&self, password: Option<&str>, status: u8) -> Output {
        let mut paths = vec![self.path().to_path_buf()];
        paths.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
        let mut command = Command::new("bash");
        command
            .arg(preset().join("post_maintenance.sh"))
            .env("PATH", std::env::join_paths(paths).unwrap())
            .env("RSTAR_PLUGIN_ROOT", preset())
            .env("RSTAR_ROOT", self.path().join("root"))
            .env("POST_MAINTENANCE_TEST_ROOT", self.path())
            .env("POST_MAINTENANCE_TEST_STATUS", status.to_string())
            .env_remove("RSTAR_CONTENT_ROOTPASSWORD");
        if let Some(password) = password {
            command.env("RSTAR_CONTENT_ROOTPASSWORD", password);
        }
        let output = command.output().unwrap();
        assert_eq!(
            fs::read_to_string(self.path().join("root/etc/shadow")).unwrap(),
            "unchanged\n"
        );
        output
    }
}

fn preset() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("presets/plugins/arch")
}

#[test]
fn omitted_and_empty_password_leave_accounts_unchanged() {
    for password in [None, Some("")] {
        let fixture = Fixture::new();
        let output = fixture.run(password, 0);
        assert!(output.status.success(), "{output:?}");
        assert!(!fixture.path().join("input").exists());
        assert!(!fixture.path().join("arguments").exists());
        assert!(output.stdout.is_empty());
        assert!(output.stderr.is_empty());
    }
}

#[test]
fn password_is_preserved_on_stdin_and_absent_from_arguments_and_logs() {
    let fixture = Fixture::new();
    let password = " space\t$%\\'\":#=;&<>|()` 中文 ";
    let output = fixture.run(Some(password), 0);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        fs::read_to_string(fixture.path().join("input")).unwrap(),
        format!("root:{password}\n")
    );
    assert_eq!(
        fs::read(fixture.path().join("arguments")).unwrap(),
        format!("--root\0{}\0", fixture.path().join("root").display()).as_bytes()
    );
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
}

#[test]
fn line_breaks_are_rejected_before_updating_any_account() {
    for password in ["secret\nother:injected", "secret\rother:injected"] {
        let fixture = Fixture::new();
        let output = fixture.run(Some(password), 0);
        assert_eq!(output.status.code(), Some(64));
        assert!(!fixture.path().join("input").exists());
        assert!(!fixture.path().join("arguments").exists());
        assert!(output.stdout.is_empty());
        assert_eq!(
            String::from_utf8(output.stderr).unwrap(),
            "RootPassword 不能包含换行符 / RootPassword must not contain line breaks\n"
        );
    }
}

#[test]
fn password_update_failure_preserves_exit_status_without_echoing_the_secret() {
    let fixture = Fixture::new();
    let output = fixture.run(Some("private value"), 42);
    assert_eq!(output.status.code(), Some(42));
    assert!(fixture.path().join("input").exists());
    assert!(output.stdout.is_empty());
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        "设置 root 密码失败 / Failed to set the root password\n"
    );
}

#[test]
fn frozen_ini_keeps_internal_spaces_and_accepts_one_password_assignment() {
    let fixture = Fixture::new();
    let configuration = fixture.path().join("config");
    fs::create_dir_all(configuration.join("build")).unwrap();
    let password = "two words $%\\'\"=:;&<>|()` 中文";
    fs::write(
        configuration.join("build/config.conf"),
        format!(
            "[Config]\nSchemaVersion=1\n[Distribution]\nDistribution=arch\n[Content]\nRootPassword={password}\n"
        ),
    )
    .unwrap();
    let parameters = rstarsleep::config::read_plugin_parameters_at(&configuration).unwrap();
    let plugin = rstarsleep::controller::plugins::load(&preset()).unwrap();
    plugin.validate_parameter_lists(&parameters).unwrap();
    assert_eq!(
        parameters.lists["RSTAR_CONTENT_ROOTPASSWORD"],
        vec![password.to_owned()]
    );
    let output = fixture.run(Some(&parameters.values["RSTAR_CONTENT_ROOTPASSWORD"]), 0);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        fs::read_to_string(fixture.path().join("input")).unwrap(),
        format!("root:{password}\n")
    );

    fs::write(
        configuration.join("build/config.conf"),
        "[Content]\nRootPassword=first\nRootPassword=second\n",
    )
    .unwrap();
    let repeated = rstarsleep::config::read_plugin_parameters_at(&configuration).unwrap();
    let error = plugin.validate_parameter_lists(&repeated).unwrap_err();
    assert!(error.to_string().contains("要求单项"));
    assert!(!error.to_string().contains("first"));
    assert!(!error.to_string().contains("second"));
}
