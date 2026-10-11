//! Exercise snapshot-aware CLI defaults with isolated host-tool replacements.
//! 使用隔离宿主工具替代实现，验证快照环境中的命令行默认值。
#![cfg(unix)]

use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::Path,
    process::{Command, Output},
};

use serde_json::json;

struct Fixture {
    temporary: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        let fixture = Self {
            temporary: tempfile::tempdir().unwrap(),
        };
        fs::create_dir(fixture.path().join("bin")).unwrap();
        fs::create_dir(fixture.top()).unwrap();
        fixture.executable(
            "findmnt",
            r#"#!/bin/bash
set -euo pipefail
printf 'findmnt\n' >> "$INIT_TRACE"
if [[ ${*: -2} == '--target /' ]]; then
    cat "$INIT_FIXTURE/root.json"
else
    cat "$INIT_FIXTURE/mounts.json"
fi
"#,
        );
        fixture.executable(
            "btrfs",
            r#"#!/bin/bash
set -euo pipefail
printf '%s\n' "$*" >> "$INIT_TRACE"
case "$1 $2" in
    'inspect-internal rootid')
        [[ ${!#} == "$INIT_FIXTURE/disk root" ]]
        printf '%s\n' "${INIT_ROOTID:-5}"
        ;;
    'subvolume create') mkdir -- "${!#}" ;;
    'subvolume show') [[ -d ${!#} ]] ;;
    *) exit 1 ;;
esac
"#,
        );
        fixture.executable("mount", "#!/bin/bash\nexit 99\n");
        fixture.executable("umount", "#!/bin/bash\nexit 99\n");
        fixture.root("btrfs");
        fs::write(fixture.path().join("mounts.json"), serde_json::to_vec(&json!({
            "filesystems": [
                {"target":"/mnt/other", "fstype":"btrfs", "uuid":"cd-34", "fsroot":"/", "options":"rw,subvolid=5"},
                {"target":"/", "fstype":"btrfs", "uuid":"ab-12", "fsroot":"/snapshots/current", "options":"rw,subvolid=339"},
                {"target":fixture.top(), "fstype":"btrfs", "uuid":"ab-12", "fsroot":"/", "options":"rw,subvolid=5"}
            ]
        })).unwrap()).unwrap();
        fixture
    }

    fn path(&self) -> &Path {
        self.temporary.path()
    }
    fn top(&self) -> std::path::PathBuf {
        self.path().join("disk root")
    }

    fn executable(&self, name: &str, script: &str) {
        let path = self.path().join("bin").join(name);
        fs::write(&path, script).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn root(&self, fstype: &str) {
        fs::write(self.path().join("root.json"), serde_json::to_vec(&json!({
            "filesystems": [{"target":"/", "fstype":fstype, "uuid":"ab-12", "fsroot":"/snapshots/current", "options":"rw,subvolid=339"}]
        })).unwrap()).unwrap();
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_RStarSleep"));
        command
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    self.path().join("bin").display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .env("INIT_FIXTURE", self.path())
            .env("INIT_TRACE", self.path().join("trace"));
        command
    }

    fn run(&self, args: &[&str]) -> Output {
        self.command().args(args).output().unwrap()
    }

    fn success(&self, args: &[&str]) {
        let output = self.run(args);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn both_commands_default_to_workspace_outside_booted_snapshot() {
    let fixture = Fixture::new();
    fixture.success(&["init"]);
    fixture.success(&["init-system", "desktop"]);
    let root = fixture.top().join("@RStarSleep");
    assert!(root.join("config/rstarsleep.toml").is_file());
    assert!(
        root.join("systems/desktop/config/build/config.conf")
            .is_file()
    );
    fs::write(root.join("config/rstarsleep.toml"), "# preserve\n").unwrap();
    fixture.success(&["init"]);
    assert_eq!(
        fs::read_to_string(root.join("config/rstarsleep.toml")).unwrap(),
        "# preserve\n"
    );
}

#[test]
fn default_system_initialization_reuses_legacy_workspace() {
    let fixture = Fixture::new();
    let root = fixture.top().join("@rstarsleep");
    fs::create_dir_all(root.join("config")).unwrap();
    fs::write(root.join("config/rstarsleep.toml"), "# legacy\n").unwrap();
    fixture.success(&["init"]);
    fixture.success(&["init-system", "desktop"]);
    assert!(
        root.join("systems/desktop/config/build/config.conf")
            .is_file()
    );
    assert!(!fixture.top().join("@RStarSleep").exists());
}

#[test]
fn system_default_requires_initialized_manager() {
    let fixture = Fixture::new();
    let output = fixture.run(&["init-system", "desktop"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("请先运行 RStarSleep init"));
    assert!(!fixture.top().join("@RStarSleep").exists());
}

#[test]
fn explicit_paths_bypass_current_system_detection() {
    let fixture = Fixture::new();
    fixture.executable("findmnt", "#!/bin/bash\nexit 99\n");
    fixture.success(&["init", fixture.top().to_str().unwrap()]);
    fixture.success(&[
        "init-system",
        fixture.top().join("@RStarSleep").to_str().unwrap(),
        "desktop",
    ]);
}

#[test]
fn non_btrfs_root_and_failed_top_level_validation_do_not_create_workspace() {
    let fixture = Fixture::new();
    fixture.root("ext4");
    for args in [&["init"][..], &["init-system", "desktop"][..]] {
        let output = fixture.run(args);
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("必须位于 Btrfs"));
    }
    fixture.root("btrfs");
    let output = fixture
        .command()
        .arg("init")
        .env("INIT_ROOTID", "339")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("subvolid=5"));
    assert!(!fixture.top().join("@RStarSleep").exists());
}

#[test]
fn help_and_invalid_arguments_do_not_query_mounts() {
    let fixture = Fixture::new();
    for args in [&["init", "--help"][..], &["init-system", "--help"][..]] {
        fixture.success(args);
    }
    for args in [
        &["init", "/one", "/two"][..],
        &["init-system"][..],
        &["init-system", "../invalid"][..],
    ] {
        assert!(!fixture.run(args).status.success());
    }
    assert!(!fixture.path().join("trace").exists());
}
