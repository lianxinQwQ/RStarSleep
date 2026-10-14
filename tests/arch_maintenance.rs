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

def dependencies(package, upgraded=None):
    if upgraded is None:
        upgraded = state['upgraded']
    return {
        'base': ['core'],
        'vim': ['core', 'new-lib' if upgraded else 'legacy-lib'],
        'consumer': ['new-lib'],
        'cycle-a': ['cycle-b'],
        'cycle-b': ['cycle-a'],
    }.get(package, [])

def required(package, removing=()):
    return any(package in dependencies(other) for other in installed if other not in removing)

def orphans():
    optional = state.get('optional', [])
    return sorted(package for package, explicit in installed.items()
                  if not explicit and not required(package) and package not in optional)

def install(package, explicit=False):
    if package not in installed:
        installed[package] = explicit
        for dependency in dependencies(package, upgraded=True):
            install(dependency)

if operation == '-Si':
    target = targets[0]
    if target in ['desktop', 'unknown']:
        print("error: package '%s' was not found" % target, file=sys.stderr)
        sys.exit(1)
    print('Name : ' + target.split('/')[-1])
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
elif operation == '-Qdtq':
    if case == 'orphan-query-failure' or (case == 'post-orphan-query-failure' and state['upgraded']):
        print('error: orphan query failed', file=sys.stderr)
        sys.exit(2)
    print('\n'.join(orphans()))
elif operation == '-R':
    assert '--unneeded' in args
    removing = set(targets)
    while True:
        blocked = {target for target in removing if required(target, removing)}
        if not blocked:
            break
        removing -= blocked
    for target in removing:
        installed.pop(target, None)
    print('removed unneeded packages')
elif operation == '-Rns':
    if case == 'orphan-remove-failure':
        print('error: orphan removal failed', file=sys.stderr)
        sys.exit(4)
    removing = set(targets)
    while True:
        recursive = {dependency for target in removing for dependency in dependencies(target)
                     if dependency in installed and not installed[dependency]
                     and not required(dependency, removing)}
        if recursive <= removing:
            break
        removing |= recursive
    for target in removing:
        installed.pop(target, None)
    print('removed orphan packages')
elif operation == '-S':
    assert targets and '--ask=4' in args and '--asexplicit' not in args
    if case == 'install-failure':
        print('error: transaction failed', file=sys.stderr)
        sys.exit(3)
    for target in targets:
        install(target.split('/')[-1], explicit=True)
    print('installed missing targets')
elif operation == '-Syu':
    assert not targets and '--asexplicit' not in args and '--ask=4' in args
    if case == 'upgrade-failure':
        print('error: upgrade failed', file=sys.stderr)
        sys.exit(3)
    state['updated'] = sorted(installed)
    for package in list(installed):
        for dependency in dependencies(package, upgraded=True):
            install(dependency)
    if case == 'missing-dependency':
        installed.pop('new-lib', None)
    if case == 'unexpected-dependency':
        installed['surprise-runtime'] = False
    if case == 'wrong-dependency-reason':
        installed['new-lib'] = True
    state['upgraded'] = True
    print('updated all installed packages')
elif operation == '-D':
    if case == 'reason-command-failure':
        print('error: reason update failed', file=sys.stderr)
        sys.exit(5)
    if case != 'reason-failure':
        for target in targets:
            assert target in installed
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

    fn state(&self) -> serde_json::Value {
        serde_json::from_slice(&fs::read(self.temporary.path().join("state.json")).unwrap())
            .unwrap()
    }

    fn set_state(&self, state: serde_json::Value) {
        fs::write(
            self.temporary.path().join("state.json"),
            serde_json::to_vec(&state).unwrap(),
        )
        .unwrap();
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
    let trace = fixture.trace();
    let demote = trace.find("-D --asdeps -- stale\n").unwrap();
    let remove_stale = trace.find("-Rns --noconfirm -- stale\n").unwrap();
    let upgrade = trace.find("-Syu --noconfirm --ask=4\n").unwrap();
    let remove_legacy = trace.find("-Rns --noconfirm -- legacy-lib\n").unwrap();
    assert!(demote < remove_stale && remove_stale < upgrade && upgrade < remove_legacy);
    assert!(
        !trace.contains("-S "),
        "Existing targets must not be reinstalled: {trace}"
    );
    assert!(!trace.contains("-D --asexplicit"));
    assert_eq!(
        fixture.state()["updated"],
        serde_json::json!(["base", "core", "legacy-lib", "vim"])
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("updated all installed packages"));
}

#[test]
fn installs_only_missing_roots_and_reinstalls_a_precleaned_future_dependency() {
    let fixture = Fixture::new();
    let mut initial = fixture.state();
    initial["installed"]["new-lib"] = true.into();
    fixture.set_state(initial);
    let output = fixture.run(
        &[
            "--plan",
            "base",
            "core",
            "vim",
            "new-lib",
            "consumer",
            "--targets",
            "vim",
            "consumer",
        ],
        "",
    );
    assert!(output.status.success(), "{output:?}");
    let trace = fixture.trace();
    let demote = trace.find("-D --asdeps -- new-lib stale\n").unwrap();
    let remove = trace.find("-Rns --noconfirm -- new-lib stale\n").unwrap();
    let install = trace
        .find("-S --needed --noconfirm --ask=4 -- consumer\n")
        .unwrap();
    let upgrade = trace.find("-Syu --noconfirm --ask=4\n").unwrap();
    assert!(demote < remove && remove < install && install < upgrade);
    let state = fixture.state();
    assert_eq!(state["installed"]["consumer"], true);
    assert_eq!(state["installed"]["new-lib"], false);
}

#[test]
fn promotes_installed_dependency_targets_before_cleanup_without_reinstalling_them() {
    let fixture = Fixture::new();
    let mut initial = fixture.state();
    initial["installed"]["vim"] = false.into();
    initial["installed"]["core"] = true.into();
    fixture.set_state(initial);
    let output = fixture.run(PLAN, "");
    assert!(output.status.success(), "{output:?}");
    let trace = fixture.trace();
    assert!(trace.contains("-D --asexplicit -- vim\n"));
    assert!(trace.contains("-D --asdeps -- core stale\n"));
    assert!(trace.find("-D --asexplicit").unwrap() < trace.find("-Qdtq").unwrap());
    assert!(!trace.contains("-S "));
    assert_eq!(fixture.state()["installed"]["vim"], true);
    assert_eq!(fixture.state()["installed"]["core"], false);
}

#[test]
fn prunes_optional_dependencies_and_cycles_before_upgrading() {
    let fixture = Fixture::new();
    let mut initial = fixture.state();
    initial["installed"]["optional-lib"] = false.into();
    initial["installed"]["cycle-a"] = false.into();
    initial["installed"]["cycle-b"] = false.into();
    initial["optional"] = serde_json::json!(["optional-lib"]);
    fixture.set_state(initial);
    let output = fixture.run(PLAN, "");
    assert!(output.status.success(), "{output:?}");
    let trace = fixture.trace();
    let prune = trace
        .find("-R --unneeded --noconfirm -- cycle-a cycle-b legacy-lib optional-lib\n")
        .unwrap();
    assert!(prune < trace.find("-Syu").unwrap());
    for package in ["cycle-a", "cycle-b", "optional-lib"] {
        assert!(fixture.state()["installed"].get(package).is_none());
        assert!(
            !fixture.state()["updated"]
                .as_array()
                .unwrap()
                .contains(&package.into())
        );
    }
}

#[test]
fn preserves_source_qualified_reinstallation_and_aur_mapping() {
    for (target, source) in [("custom/vim", "custom/vim"), ("aur/vim", "rstarsleep/vim")] {
        let fixture = Fixture::new();
        let output = fixture.run(
            &[
                "--plan",
                "base",
                "core",
                "vim",
                "new-lib",
                "--targets",
                target,
            ],
            "",
        );
        assert!(output.status.success(), "{output:?}");
        assert!(
            fixture
                .trace()
                .contains(&format!("-S --noconfirm --ask=4 -- {source}\n"))
        );
        assert!(!fixture.trace().contains("--needed"));
    }
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
            "new-lib",
            "--targets",
            "desktop",
        ],
        "",
    );
    assert!(output.status.success(), "{output:?}");
    let state: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(state["requested"], serde_json::json!(["vim", "editor-ui"]));
    assert!(
        fixture
            .trace()
            .contains("-S --needed --noconfirm --ask=4 -- editor-ui\n")
    );
    let fixture = Fixture::new();
    let output = fixture.run(&["--plan", "base", "core", "--targets"], "");
    assert!(output.status.success(), "{output:?}");
    let state: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(state["requested"], serde_json::json!([]));
    assert_eq!(
        state["installed"],
        serde_json::json!([{"name":"base","explicit":true},{"name":"core","explicit":false}])
    );
    assert!(!fixture.trace().contains("-S "));
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
            fixture.trace().lines().all(|line| !line.starts_with("-R")
                && !line.starts_with("-S ")
                && !line.starts_with("-Syu")
                && !line.starts_with("-D")),
            "{arguments:?}"
        );
    }
}

#[test]
fn propagates_transaction_errors_and_rejects_incorrect_final_state() {
    for case in [
        "query-failure",
        "install-failure",
        "upgrade-failure",
        "orphan-query-failure",
        "post-orphan-query-failure",
        "orphan-remove-failure",
        "missing-dependency",
        "reason-failure",
        "reason-command-failure",
        "wrong-dependency-reason",
    ] {
        let fixture = Fixture::new();
        let mut initial = fixture.state();
        if case == "install-failure" {
            initial["installed"].as_object_mut().unwrap().remove("vim");
        }
        if case == "reason-failure" {
            initial["installed"]["core"] = true.into();
        }
        fixture.set_state(initial);
        let output = fixture.run(PLAN, case);
        assert!(!output.status.success(), "{case}");
        assert!(output.stdout.is_empty(), "{case}");
        assert!(!output.stderr.is_empty(), "{case}");
        assert!(!fixture.trace().contains("--nodeps"));
        if matches!(
            case,
            "orphan-query-failure" | "orphan-remove-failure" | "reason-command-failure"
        ) {
            assert!(!fixture.trace().contains("-Syu"));
        }
    }
}

#[test]
fn rejects_an_unexpected_runtime_dependency_still_needed_by_a_root() {
    let fixture = Fixture::new();
    // A dependency retained by the package manager must not be force removed.
    let executable = fixture.temporary.path().join("pacman");
    let script = fs::read_to_string(&executable).unwrap().replace(
        "'base': ['core'],",
        "'base': ['core'] + (['surprise-runtime'] if state['upgraded'] else []),",
    );
    fs::write(executable, script).unwrap();
    let output = fixture.run(PLAN, "unexpected-dependency");
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("Unexpected installed package"));
    assert_eq!(fixture.state()["installed"]["surprise-runtime"], false);
}
