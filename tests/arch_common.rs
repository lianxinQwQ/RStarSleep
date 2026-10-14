//! Shared Arch configuration must survive conditional calls and remain readable to pacman.
//! Arch 共用配置必须在条件调用中传播错误，并保证 pacman 可读取。
#![cfg(unix)]

use std::{fs, os::unix::fs::PermissionsExt, path::PathBuf, process::Command};

struct Fixture {
    temporary: tempfile::TempDir,
}

impl Fixture {
    fn new(frozen_marker: Option<&str>, local_repository: bool) -> Self {
        Self::with_cache(frozen_marker, local_repository, true)
    }

    fn with_cache(frozen_marker: Option<&str>, local_repository: bool, cache: bool) -> Self {
        let temporary = tempfile::Builder::new()
            .prefix("arch common ")
            .tempdir()
            .unwrap();
        let root = temporary.path();
        let data = root.join("plugin with spaces/data");
        fs::create_dir_all(&data).unwrap();
        fs::write(
            data.join("pacman.conf.in"),
            concat!(
                "[options]\nArchitecture = x86_64\nSigLevel = Required DatabaseOptional\n",
                "[core]\nInclude = @MIRRORLIST@\n",
                "[extra]\nInclude = @MIRRORLIST@\n",
                "Include = @PLUGIN_ROOT@/data/thirdparty.conf\n",
            ),
        )
        .unwrap();
        fs::write(
            data.join("mirrorlist"),
            "Server = https://default.example/$repo/os/$arch\n",
        )
        .unwrap();
        fs::write(
            data.join("thirdparty.conf"),
            "[archlinuxcn]\nServer = https://repo.example/$arch\n",
        )
        .unwrap();
        if cache {
            fs::create_dir_all(root.join("plugin with spaces/state/packages")).unwrap();
        }
        let repository = root.join("plugin with spaces/state/repository");
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
            r#"#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >> "$COMMON_TEST_ROOT/pacman-trace"
[[ $1 == --config && -r $2 && -r ${2%/*}/mirrorlist ]] || exit 91
printf '%s\n' "$2" > "$COMMON_TEST_ROOT/configuration-path"
cat -- "$2" > "$COMMON_TEST_ROOT/configuration-used"
cat -- "${2%/*}/mirrorlist" > "$COMMON_TEST_ROOT/mirrorlist-used"
stat -c '%a' -- "${2%/*}" "$2" "${2%/*}/mirrorlist" > "$COMMON_TEST_ROOT/configuration-modes"
exit "${COMMON_TEST_PACMAN_STATUS:-0}"
"#,
        )
        .unwrap();
        fs::set_permissions(executable, fs::Permissions::from_mode(0o755)).unwrap();
        Self { temporary }
    }

    fn command(&self) -> Command {
        let mut paths = vec![self.temporary.path().to_path_buf()];
        paths.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
        let mut command = Command::new("bash");
        command
            .env("PATH", std::env::join_paths(paths).unwrap())
            .env(
                "RSTAR_PLUGIN_ROOT",
                self.temporary.path().join("plugin with spaces"),
            )
            .env("RSTAR_PLUGIN_TMP", self.temporary.path())
            .env("RSTAR_SYSTEM_ID", "desktop")
            .env("COMMON_TEST_ROOT", self.temporary.path())
            .env_remove("RSTAR_DISTRIBUTION_MIRROR")
            .env_remove("RSTAR_MIRROR");
        command
    }

    fn run(&self, failure: &str, pacman_status: u8) -> std::process::Output {
        self.command()
            .arg("-c")
            .arg(
                r#"
source "$1"
# Intercept system paths so no fixture ever touches the host package database.
mkdir() {
    if [[ $* == '-p /var/lib/pacman/sync' ]]; then
        [[ $COMMON_TEST_FAILURE != mkdir ]] || { printf 'mkdir failed\n' >&2; return 71; }
    else
        command mkdir "$@"
    fi
}
cp() {
    if [[ ${@: -1} == /var/lib/pacman/sync/* ]]; then
        [[ $COMMON_TEST_FAILURE != cp ]] || { printf 'copy failed\n' >&2; return 72; }
    else
        command cp "$@"
    fi
}
mktemp() {
    [[ $COMMON_TEST_FAILURE != mktemp ]] || { printf 'mktemp failed\n' >&2; return 73; }
    local directory
    directory=$(command mktemp "$@") || return
    printf '%s\n' "$directory" >> "$COMMON_TEST_ROOT/temporary-directories"
    printf '%s\n' "$directory"
}
if [[ $COMMON_TEST_FAILURE == render ]]; then
    write_pacman_configuration() {
        touch -- "$1/partial-configuration"
        printf 'configuration failed\n' >&2
        return 74
    }
fi
rm() {
    [[ $COMMON_TEST_FAILURE != cleanup ]] || { printf 'cleanup failed\n' >&2; return 75; }
    command rm "$@"
}
if pacman -Syu --noconfirm; then status=0; else status=$?; fi
exit "$status"
"#,
            )
            .arg("arch-common-test")
            .arg(common_script())
            .env("COMMON_TEST_FAILURE", failure)
            .env("COMMON_TEST_PACMAN_STATUS", pacman_status.to_string())
            .output()
            .unwrap()
    }

    fn generate(&self, environment: &[(&str, &str)]) -> std::process::Output {
        self.command()
            .args([
                "-c",
                "source \"$1\"; write_pacman_configuration \"$COMMON_TEST_ROOT/generated configuration\"",
                "arch-configuration-test",
            ])
            .arg(common_script())
            .envs(environment.iter().copied())
            .output()
            .unwrap()
    }

    fn trace(&self) -> String {
        fs::read_to_string(self.temporary.path().join("pacman-trace")).unwrap_or_default()
    }

    fn assert_temporary_configuration_removed(&self) {
        let directories = fs::read_to_string(self.temporary.path().join("temporary-directories"))
            .unwrap_or_default();
        for directory in directories.lines() {
            assert!(
                !PathBuf::from(directory).exists(),
                "configuration leaked: {directory}"
            );
        }
    }
}

fn common_script() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("presets/plugins/arch/lib/common.sh")
}

#[test]
fn mounted_data_is_initialized_from_defaults_and_existing_content_is_preserved() {
    let fixture = Fixture::with_cache(None, false, false);
    let plugin = fixture.temporary.path().join("plugin with spaces");
    let data = plugin.join("data");
    let defaults = plugin.join("default-data");
    fs::rename(&data, &defaults).unwrap();
    fs::create_dir(&data).unwrap();
    fs::create_dir(defaults.join("nested")).unwrap();
    fs::write(defaults.join("nested/extra.conf"), "new default\n").unwrap();
    fs::write(defaults.join(".extra.conf"), "hidden default\n").unwrap();
    let output = fixture.generate(&[]);
    assert!(output.status.success(), "{output:?}");
    for name in [
        "pacman.conf.in",
        "mirrorlist",
        "thirdparty.conf",
        "nested/extra.conf",
        ".extra.conf",
    ] {
        assert_eq!(
            fs::read(data.join(name)).unwrap(),
            fs::read(defaults.join(name)).unwrap()
        );
    }
    fs::write(
        data.join("mirrorlist"),
        "Server = https://custom.example/$repo/os/$arch\n",
    )
    .unwrap();
    fs::write(data.join("nested/extra.conf"), "custom nested\n").unwrap();
    fs::write(data.join("user-only.conf"), "user file\n").unwrap();
    fs::write(
        defaults.join("pacman.conf.in"),
        "must not replace the existing template\n",
    )
    .unwrap();
    fs::write(defaults.join("added.conf"), "new file after upgrade\n").unwrap();
    let output = fixture.generate(&[]);
    assert!(output.status.success(), "{output:?}");
    assert!(
        fs::read_to_string(data.join("pacman.conf.in"))
            .unwrap()
            .contains("[core]")
    );
    assert_eq!(
        fs::read_to_string(data.join("mirrorlist")).unwrap(),
        "Server = https://custom.example/$repo/os/$arch\n"
    );
    assert_eq!(
        fs::read_to_string(data.join("nested/extra.conf")).unwrap(),
        "custom nested\n"
    );
    assert_eq!(
        fs::read_to_string(data.join("user-only.conf")).unwrap(),
        "user file\n"
    );
    assert_eq!(
        fs::read_to_string(data.join("added.conf")).unwrap(),
        "new file after upgrade\n"
    );
}

#[test]
fn default_data_copy_failures_stop_configuration_even_in_a_conditional() {
    let fixture = Fixture::with_cache(None, false, false);
    let defaults = fixture
        .temporary
        .path()
        .join("plugin with spaces/default-data");
    fs::create_dir(&defaults).unwrap();
    fs::write(defaults.join("missing.conf"), "default\n").unwrap();
    let output = fixture.command()
        .args(["-c", "source \"$1\"; cp() { return 73; }; if write_pacman_configuration \"$COMMON_TEST_ROOT/generated\"; then exit 91; else exit $?; fi", "arch-data-copy-test"])
        .arg(common_script())
        .output().unwrap();
    assert_eq!(output.status.code(), Some(73), "{output:?}");
    assert!(
        !fixture
            .temporary
            .path()
            .join("generated/pacman.conf")
            .exists()
    );
    assert!(fixture.trace().is_empty());
}

#[test]
fn frozen_database_failures_stop_before_pacman_even_in_a_conditional() {
    for marker in [".planned.json", ".prepared.json"] {
        for (failure, status) in [("mkdir", 71), ("cp", 72)] {
            let fixture = Fixture::new(Some(marker), false);
            let output = fixture.run(failure, 0);
            assert_eq!(
                output.status.code(),
                Some(status),
                "{marker}: {failure}: {output:?}"
            );
            assert!(fixture.trace().is_empty());
            assert!(!output.stderr.is_empty());
            fixture.assert_temporary_configuration_removed();
        }
        let fixture = Fixture::new(Some(marker), false);
        fs::remove_file(
            fixture
                .temporary
                .path()
                .join("plugin with spaces/state/repository/official-sync/core.db"),
        )
        .unwrap();
        let output = fixture.run("", 0);
        assert_eq!(output.status.code(), Some(1));
        assert!(fixture.trace().is_empty());
        assert!(String::from_utf8_lossy(&output.stderr).contains("databases are missing"));
        fixture.assert_temporary_configuration_removed();
    }
}

#[test]
fn local_repository_preparation_failures_stop_and_clean_up_temporary_configuration() {
    for (failure, status) in [("mkdir", 71), ("cp", 72), ("mktemp", 73), ("render", 74)] {
        let fixture = Fixture::new(None, true);
        let output = fixture.run(failure, 0);
        assert_eq!(output.status.code(), Some(status), "{failure}: {output:?}");
        assert!(fixture.trace().is_empty());
        fixture.assert_temporary_configuration_removed();
    }
}

#[test]
fn successful_preparation_preserves_frozen_database_options_and_pacman_errors() {
    let fixture = Fixture::new(Some(".planned.json"), true);
    let output = fixture.run("", 0);
    assert!(output.status.success(), "{output:?}");
    let trace = fixture.trace();
    assert!(trace.starts_with("--config "));
    assert!(trace.contains(" --cachedir "));
    assert!(trace.ends_with(" -Su --noconfirm\n"));
    let configuration =
        fs::read_to_string(fixture.temporary.path().join("configuration-used")).unwrap();
    assert!(configuration.contains("/data/thirdparty.conf\n"));
    assert!(configuration.contains("[rstarsleep]"));
    assert!(configuration.contains("SigLevel = Optional TrustAll"));
    fixture.assert_temporary_configuration_removed();
    let fixture = Fixture::new(None, true);
    let output = fixture.run("", 76);
    assert_eq!(output.status.code(), Some(76));
    fixture.assert_temporary_configuration_removed();
}

#[test]
fn preparation_environment_keeps_refreshing_its_own_metadata() {
    let fixture = Fixture::new(Some(".prepared.json"), false);
    let output = fixture
        .command()
        .args([
            "-c",
            "source \"$1\"; pacman -Syu --noconfirm",
            "arch-refresh-test",
        ])
        .arg(common_script())
        .env("RSTAR_SYSTEM_ID", "predownload")
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert!(fixture.trace().ends_with(" -Syu --noconfirm\n"));
    let configuration =
        fs::read_to_string(fixture.temporary.path().join("configuration-path")).unwrap();
    assert!(!PathBuf::from(configuration.trim()).exists());
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

#[test]
fn operations_without_shared_cache_still_use_readable_plugin_configuration() {
    let fixture = Fixture::with_cache(None, false, false);
    let output = fixture.run("", 0);
    assert!(output.status.success(), "{output:?}");
    assert!(fixture.trace().starts_with("--config "));
    assert!(!fixture.trace().contains("--cachedir"));
    assert!(fixture.trace().ends_with(" -Syu --noconfirm\n"));
    assert_eq!(
        fs::read_to_string(fixture.temporary.path().join("configuration-modes")).unwrap(),
        "755\n644\n644\n"
    );
    assert_eq!(
        fs::read_to_string(fixture.temporary.path().join("mirrorlist-used")).unwrap(),
        "Server = https://default.example/$repo/os/$arch\n"
    );
    fixture.assert_temporary_configuration_removed();
}

#[test]
fn missing_plugin_template_stops_and_cleans_up_even_without_shared_cache() {
    let fixture = Fixture::with_cache(None, false, false);
    fs::remove_file(
        fixture
            .temporary
            .path()
            .join("plugin with spaces/data/pacman.conf.in"),
    )
    .unwrap();
    let output = fixture.run("", 0);
    assert!(!output.status.success());
    assert!(fixture.trace().is_empty());
    assert!(!output.stderr.is_empty());
    fixture.assert_temporary_configuration_removed();
}

#[test]
fn generated_includes_and_mirror_overrides_work_with_spaces_and_literal_pacman_variables() {
    for environment in [
        vec![],
        vec![(
            "RSTAR_DISTRIBUTION_MIRROR",
            "https://configured.example/$repo/os/$arch",
        )],
        vec![("RSTAR_MIRROR", "https://legacy.example/$repo/os/$arch")],
        vec![
            (
                "RSTAR_DISTRIBUTION_MIRROR",
                "https://configured.example/$repo/os/$arch",
            ),
            ("RSTAR_MIRROR", "https://ignored.example/$repo/os/$arch"),
        ],
    ] {
        let fixture = Fixture::with_cache(None, false, false);
        let output = fixture.generate(&environment);
        assert!(output.status.success(), "{environment:?}: {output:?}");
        let directory = fixture.temporary.path().join("generated configuration");
        let configuration = fs::read_to_string(directory.join("pacman.conf")).unwrap();
        assert!(configuration.contains(&format!(
            "Include = {}\n",
            directory.join("mirrorlist").display()
        )));
        assert!(configuration.contains(&format!(
            "Include = {}\n",
            fixture.temporary.path().join("plugin with spaces/data/thirdparty.conf").display()
        )));
        assert!(!configuration.contains("@MIRRORLIST@"));
        assert!(!configuration.contains("@PLUGIN_ROOT@"));
        let expected_mirror = environment
            .first()
            .map_or("https://default.example/$repo/os/$arch", |(_, mirror)| {
                *mirror
            });
        assert_eq!(
            fs::read_to_string(directory.join("mirrorlist")).unwrap(),
            format!("Server = {expected_mirror}\n")
        );
        // This parser is optional on non-Arch developer hosts and needs no network.
        let parsed = Command::new("pacman-conf")
            .arg("--config")
            .arg(directory.join("pacman.conf"))
            .args(["--repo", "core", "Server"])
            .output();
        if let Ok(parsed) = parsed {
            assert!(parsed.status.success(), "{parsed:?}");
            assert_eq!(
                String::from_utf8(parsed.stdout).unwrap().trim(),
                expected_mirror
                    .replace("$repo", "core")
                    .replace("$arch", "x86_64")
            );
            let parsed = Command::new("pacman-conf")
                .arg("--config")
                .arg(directory.join("pacman.conf"))
                .args(["--repo", "archlinuxcn", "Server"])
                .output()
                .unwrap();
            assert!(parsed.status.success(), "{parsed:?}");
            assert_eq!(
                String::from_utf8(parsed.stdout).unwrap().trim(),
                "https://repo.example/x86_64"
            );
        }
    }
}
