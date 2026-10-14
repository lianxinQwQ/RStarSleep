//! Exercise the combined Arch maintenance operation with a stateful package manager.
//! 使用带状态的包管理器验证合并后的 Arch 软件维护操作。
#![cfg(unix)]

use std::{fs, os::unix::fs::PermissionsExt, path::PathBuf, process::Command};

struct Fixture {
    temporary: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        let temporary = tempfile::tempdir().unwrap();
        let preset = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("presets/plugins/arch");
        let plugin = temporary.path().join("plugin");
        fs::create_dir_all(plugin.join("data")).unwrap();
        std::os::unix::fs::symlink(preset.join("lib"), plugin.join("lib")).unwrap();
        fs::copy(
            preset.join("resolve_packages.sh"),
            plugin.join("resolve_packages.sh"),
        )
        .unwrap();
        std::os::unix::fs::symlink(preset.join("default-data"), plugin.join("default-data"))
            .unwrap();
        fs::write(
            temporary.path().join("state.json"),
            r#"{"installed":{"base":true,"core":false,"vim":true,"legacy-lib":false,"stale":true},"upgraded":false}"#,
        )
        .unwrap();
        let pacman = temporary.path().join("pacman");
        fs::write(
            &pacman,
            r#"#!/usr/bin/env python3
import json, os, pathlib, sys
root = pathlib.Path(os.environ['MAINTENANCE_TEST_ROOT'])
args = sys.argv[1:]
assert args[0] == '--config' and pathlib.Path(args[1]).is_file()
assert pathlib.Path(args[1]).with_name('mirrorlist').is_file()
args = args[2:]
with (root / 'trace').open('a') as trace:
    trace.write(' '.join(args) + '\n')
state_path = root / 'state.json'
state = json.loads(state_path.read_text())
installed = state['installed']
case = os.environ.get('MAINTENANCE_TEST_CASE', '')
operation = args[0]
targets = args[args.index('--') + 1:] if '--' in args else []
if operation == '-Si':
    target = targets[0]
    if target in ['desktop', 'unknown']:
        print("error: package '%s' was not found" % target, file=sys.stderr)
        sys.exit(1)
    print('Name : ' + target)
elif operation == '-Sgq':
    if targets == ['desktop']:
        print('vim\neditor-ui')
    else:
        sys.exit(1)
elif operation == '-Qq':
    if case == 'query-failure':
        print('error: unreadable package database', file=sys.stderr)
        sys.exit(2)
    print('\n'.join(sorted(installed)))
elif operation == '-Qqe':
    print('\n'.join(sorted(name for name, explicit in installed.items() if explicit)))
elif operation == '-R':
    assert '--unneeded' in args
    for target in targets:
        if target == 'legacy-lib' and not state['upgraded']:
            continue
        if target == 'surprise-runtime':
            continue
        installed.pop(target, None)
    print('removed unneeded packages')
elif operation == '-S':
    assert '--needed' in args and '--ask=4' in args
    if case == 'install-failure':
        print('error: transaction failed', file=sys.stderr)
        sys.exit(3)
    for target in targets:
        installed.setdefault(target, True)
    if case == 'missing-dependency':
        installed['surprise-runtime'] = False
    state['upgraded'] = True
    print('installed and updated the selected plan')
elif operation == '-D':
    if case != 'reason-failure':
        for target in targets:
            installed[target] = '--asexplicit' in args
    print('updated installation reasons')
else:
    raise AssertionError(args)
state_path.write_text(json.dumps(state))
"#,
        )
        .unwrap();
        fs::set_permissions(&pacman, fs::Permissions::from_mode(0o755)).unwrap();
        Self { temporary }
    }

    fn run(&self, arguments: &[&str], case: &str) -> std::process::Output {
        let mut paths = vec![self.temporary.path().to_path_buf()];
        paths.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
        Command::new("bash")
            .arg(
                PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("presets/plugins/arch/maintain_packages.sh"),
            )
            .args(arguments)
            .env("PATH", std::env::join_paths(paths).unwrap())
            .env("RSTAR_PLUGIN_TMP", self.temporary.path())
            .env("RSTAR_PLUGIN_ROOT", self.temporary.path().join("plugin"))
            .env("MAINTENANCE_TEST_ROOT", self.temporary.path())
            .env("MAINTENANCE_TEST_CASE", case)
            .output()
            .unwrap()
    }

    fn trace(&self) -> String {
        fs::read_to_string(self.temporary.path().join("trace")).unwrap_or_default()
    }
}

const PLAN: &[&str] = &[
    "--plan",
    "base",
    "core",
    "vim",
    "new-lib",
    "--targets",
    "vim",
];

#[test]
fn removes_old_targets_before_updating_and_old_dependencies_afterwards() {
    let fixture = Fixture::new();
    let output = fixture.run(PLAN, "");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let state: rstarsleep::software::SoftwareState =
        serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(state.requested, ["vim"]);
    assert_eq!(
        state
            .installed
            .iter()
            .map(|package| (package.name.as_str(), package.explicit))
            .collect::<Vec<_>>(),
        [
            ("base", true),
            ("core", false),
            ("new-lib", false),
            ("vim", true)
        ]
    );
    assert_eq!(
        fixture.trace(),
        concat!(
            "-Si -- base\n",
            "-Si -- vim\n",
            "-Qq\n",
            "-R --unneeded --noconfirm -- legacy-lib stale\n",
            "-S --needed --noconfirm --ask=4 -- base core vim new-lib\n",
            "-Qq\n",
            "-R --unneeded --noconfirm -- legacy-lib\n",
            "-D --asexplicit -- base vim\n",
            "-D --asdeps -- core new-lib\n",
            "-Qq\n",
            "-Qqe\n",
        )
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("installed and updated"));
}

#[test]
fn expands_groups_and_protects_base_with_no_configured_targets() {
    let fixture = Fixture::new();
    let output = fixture.run(
        &[
            "--plan",
            "base",
            "core",
            "vim",
            "editor-ui",
            "--targets",
            "desktop",
        ],
        "",
    );
    assert!(output.status.success());
    let state: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(state["requested"], serde_json::json!(["vim", "editor-ui"]));
    let fixture = Fixture::new();
    let output = fixture.run(&["--plan", "base", "core", "--targets"], "");
    assert!(output.status.success());
    let state: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(state["requested"], serde_json::json!([]));
    assert_eq!(
        state["installed"],
        serde_json::json!([{"name":"base","explicit":true},{"name":"core","explicit":false}])
    );
}

#[test]
fn rejects_invalid_or_incomplete_plans_before_mutation() {
    for arguments in [
        vec!["base"],
        vec!["--plan", "base"],
        vec!["--plan", "vim", "--targets", "vim"],
        vec!["--plan", "base", "base", "--targets"],
        vec!["--plan", "base", "bad name", "--targets"],
        vec!["--plan", "base", "--targets", "vim"],
        vec!["--plan", "base", "--targets", "unknown"],
    ] {
        let fixture = Fixture::new();
        let output = fixture.run(&arguments, "");
        assert!(!output.status.success(), "{arguments:?}");
        assert!(output.stdout.is_empty(), "{arguments:?}");
        assert!(
            fixture
                .trace()
                .lines()
                .all(|line| !line.starts_with("-R ") && !line.starts_with("-S ")),
            "{arguments:?}"
        );
    }
}

#[test]
fn propagates_transaction_errors_and_rejects_incorrect_final_state() {
    for case in [
        "query-failure",
        "install-failure",
        "missing-dependency",
        "reason-failure",
    ] {
        let fixture = Fixture::new();
        let output = fixture.run(PLAN, case);
        assert!(!output.status.success(), "{case}");
        assert!(output.stdout.is_empty(), "{case}");
        assert!(!output.stderr.is_empty(), "{case}");
        assert!(!fixture.trace().contains("--nodeps"));
    }
}
