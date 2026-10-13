//! Repository preparation failures must stop conditional package-manager calls.
//! 仓库准备失败必须中止处于条件上下文的软件包管理调用。
#![cfg(unix)]

use std::{fs, os::unix::fs::PermissionsExt, path::PathBuf, process::Command};

struct Fixture {
    temporary: tempfile::TempDir,
}

impl Fixture {
    fn new(frozen_marker: Option<&str>, local_repository: bool) -> Self {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path();
        fs::create_dir_all(root.join("plugin/state/packages")).unwrap();
        let repository = root.join("plugin/state/repository");
        fs::create_dir_all(repository.join("official-sync")).unwrap();
        if let Some(marker) = frozen_marker {
            fs::write(repository.join(marker), "{}").unwrap();
            fs::write(repository.join("official-sync/core.db"), "fixture").unwrap();
        }
        if local_repository {
            fs::write(repository.join("rstarsleep.db.tar.gz"), "fixture").unwrap();
        }
        let executable = root.join("pacman");
        fs::write(
            &executable,
            "#!/usr/bin/env bash\nprintf '%s\\n' \"$*\" >> \"$COMMON_TEST_ROOT/pacman-trace\"\nexit \"${COMMON_TEST_PACMAN_STATUS:-0}\"\n",
        )
        .unwrap();
        fs::set_permissions(executable, fs::Permissions::from_mode(0o755)).unwrap();
        Self { temporary }
    }

    fn run(&self, failure: &str, pacman_status: u8) -> std::process::Output {
        let mut paths = vec![self.temporary.path().to_path_buf()];
        paths.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
        Command::new("bash")
            .arg("-c")
            .arg(
                r#"
source "$1"
# Intercept absolute system paths so this test never touches the host database.
mkdir() {
    [[ $* == '-p /var/lib/pacman/sync' ]] || return 90
    [[ $COMMON_TEST_FAILURE != mkdir ]] || { printf 'mkdir failed\n' >&2; return 71; }
}
cp() {
    [[ $COMMON_TEST_FAILURE != cp ]] || { printf 'copy failed\n' >&2; return 72; }
}
mktemp() {
    [[ $COMMON_TEST_FAILURE != mktemp ]] || { printf 'mktemp failed\n' >&2; return 73; }
    printf '%s/prepared.conf\n' "$COMMON_TEST_ROOT"
}
awk() {
    [[ $COMMON_TEST_FAILURE != awk ]] || { printf 'awk failed\n' >&2; return 74; }
    printf '[options]\nArchitecture = x86_64\n'
}
rm() {
    [[ $COMMON_TEST_FAILURE != cleanup ]] || { printf 'cleanup failed\n' >&2; return 75; }
    command rm "$@"
}
if pacman -Syu --noconfirm; then status=0; else status=$?; fi
exit "$status"
"#,
            )
            .arg("arch-common-test")
            .arg(
                PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("presets/plugins/arch/lib/common.sh"),
            )
            .env("PATH", std::env::join_paths(paths).unwrap())
            .env("RSTAR_PLUGIN_ROOT", self.temporary.path().join("plugin"))
            .env("RSTAR_PLUGIN_TMP", self.temporary.path())
            .env("RSTAR_SYSTEM_ID", "desktop")
            .env("COMMON_TEST_ROOT", self.temporary.path())
            .env("COMMON_TEST_FAILURE", failure)
            .env("COMMON_TEST_PACMAN_STATUS", pacman_status.to_string())
            .output()
            .unwrap()
    }

    fn trace(&self) -> String {
        fs::read_to_string(self.temporary.path().join("pacman-trace")).unwrap_or_default()
    }
}

#[test]
fn frozen_database_failures_stop_before_pacman_even_in_a_conditional() {
    for marker in [".planned.json", ".prepared.json"] {
        for (failure, status) in [("mkdir", 71), ("cp", 72)] {
            let fixture = Fixture::new(Some(marker), false);
            let output = fixture.run(failure, 0);
            assert_eq!(output.status.code(), Some(status), "{marker}: {failure}");
            assert!(fixture.trace().is_empty());
            assert!(!output.stderr.is_empty());
        }
        let fixture = Fixture::new(Some(marker), false);
        fs::remove_file(
            fixture
                .temporary
                .path()
                .join("plugin/state/repository/official-sync/core.db"),
        )
        .unwrap();
        let output = fixture.run("", 0);
        assert_eq!(output.status.code(), Some(1));
        assert!(fixture.trace().is_empty());
        assert!(String::from_utf8_lossy(&output.stderr).contains("databases are missing"));
    }
}

#[test]
fn local_repository_preparation_failures_stop_and_clean_up_temporary_configuration() {
    for (failure, status) in [("mkdir", 71), ("cp", 72), ("mktemp", 73), ("awk", 74)] {
        let fixture = Fixture::new(None, true);
        let output = fixture.run(failure, 0);
        assert_eq!(output.status.code(), Some(status), "{failure}");
        assert!(fixture.trace().is_empty());
        assert!(!fixture.temporary.path().join("prepared.conf").exists());
    }
}

#[test]
fn successful_preparation_preserves_frozen_database_options_and_pacman_errors() {
    let fixture = Fixture::new(Some(".planned.json"), true);
    let output = fixture.run("", 0);
    assert!(output.status.success());
    let trace = fixture.trace();
    assert!(trace.starts_with("--config "));
    assert!(trace.contains(" --cachedir "));
    assert!(trace.ends_with(" -Su --noconfirm\n"));
    assert!(!fixture.temporary.path().join("prepared.conf").exists());
    let fixture = Fixture::new(None, true);
    let output = fixture.run("", 76);
    assert_eq!(output.status.code(), Some(76));
    assert!(!fixture.temporary.path().join("prepared.conf").exists());
}

#[test]
fn cleanup_errors_are_reported_without_hiding_the_package_transaction_error() {
    for (pacman_status, expected) in [(0, 75), (76, 76)] {
        let fixture = Fixture::new(None, true);
        let output = fixture.run("cleanup", pacman_status);
        assert_eq!(output.status.code(), Some(expected));
        assert!(!fixture.trace().is_empty());
    }
}
