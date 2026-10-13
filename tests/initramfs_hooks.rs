//! Exercise initramfs hook ordering, dependency packaging and failure propagation.
use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    path::{Path, PathBuf},
    process::{Command, Output},
};

fn write_executable(path: &Path, body: &str) {
    fs::write(path, body).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

struct ArchAction {
    directory: tempfile::TempDir,
    root: PathBuf,
    trace: PathBuf,
}

impl ArchAction {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("root");
        fs::create_dir_all(root.join("boot")).unwrap();
        fs::create_dir_all(root.join("etc/mkinitcpio.conf.d")).unwrap();
        fs::create_dir(directory.path().join("bin")).unwrap();
        fs::write(root.join("boot/vmlinuz-linux"), "kernel").unwrap();
        fs::write(
            root.join("etc/mkinitcpio.conf"),
            "HOOKS=(base block filesystems)\n",
        )
        .unwrap();
        fs::write(
            root.join("etc/mkinitcpio.conf.d/extra.conf"),
            "MODULES=(btrfs)\n",
        )
        .unwrap();
        write_executable(
            &directory.path().join("bin/mkinitcpio"),
            r#"#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$@" > "$TRACE"
printf '%s\n' '==> mkinitcpio compiler output'
printf '%s\n' 'Note: compiler stderr remains visible' >&2
if [[ ${COMPILER_EXIT:-0} != 0 ]]; then
    printf '%s\n' '==> ERROR: failed to generate initramfs'
    exit "$COMPILER_EXIT"
fi
if [[ ${SKIP_IMAGE:-0} == 0 ]]; then
    while (( $# )); do
        if [[ $1 == -g ]]; then printf image > "$2"; break; fi
        shift
    done
fi
"#,
        );
        let trace = directory.path().join("trace");
        Self {
            directory,
            root,
            trace,
        }
    }

    fn run(&self, compiler_exit: &str, skip_image: bool) -> Output {
        Command::new("bash")
            .arg(
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("presets/plugins/arch/prepare_inheritance_initramfs.sh"),
            )
            .env("RSTAR_ROOT", &self.root)
            .env(
                "RSTAR_PLUGIN_ROOT",
                Path::new(env!("CARGO_MANIFEST_DIR")).join("presets/plugins/arch"),
            )
            .env(
                "PATH",
                format!(
                    "{}:/usr/bin:/bin",
                    self.directory.path().join("bin").display()
                ),
            )
            .env("TRACE", &self.trace)
            .env("COMPILER_EXIT", compiler_exit)
            .env("SKIP_IMAGE", if skip_image { "1" } else { "0" })
            .output()
            .unwrap()
    }
}

#[test]
fn arch_action_preserves_user_configuration_and_propagates_compiler_diagnostics() {
    let fixture = ArchAction::new();
    let configuration = fs::read(fixture.root.join("etc/mkinitcpio.conf")).unwrap();
    let dropin = fs::read(fixture.root.join("etc/mkinitcpio.conf.d/extra.conf")).unwrap();
    for exit in ["0", "23"] {
        let output = fixture.run(exit, false);
        assert_eq!(output.status.code(), exit.parse().ok());
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("mkinitcpio compiler output"));
        if exit != "0" {
            assert!(stdout.contains("ERROR: failed to generate initramfs"));
        }
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("compiler stderr remains visible")
        );
        assert_eq!(
            fs::read(fixture.root.join("etc/mkinitcpio.conf")).unwrap(),
            configuration
        );
        assert_eq!(
            fs::read(fixture.root.join("etc/mkinitcpio.conf.d/extra.conf")).unwrap(),
            dropin
        );
    }
    let args = fs::read_to_string(&fixture.trace).unwrap();
    assert_eq!(
        args.lines().collect::<Vec<_>>(),
        [
            "-c".into(),
            fixture
                .root
                .join("usr/lib/rstarsleep/mkinitcpio.conf")
                .display()
                .to_string(),
            "-k".into(),
            fixture
                .root
                .join("boot/vmlinuz-linux")
                .display()
                .to_string(),
            "-g".into(),
            fixture
                .root
                .join("boot/initramfs-linux.img")
                .display()
                .to_string(),
        ]
    );
    let wrapper = fs::read_to_string(fixture.root.join("usr/lib/rstarsleep/mkinitcpio.conf"))
        .unwrap()
        .replace(
            "/etc/mkinitcpio",
            &format!("{}/etc/mkinitcpio", fixture.root.display()),
        )
        .replace(
            "/usr/lib/rstarsleep",
            &format!("{}/usr/lib/rstarsleep", fixture.root.display()),
        );
    let config = fixture.directory.path().join("configuration");
    fs::write(&config, wrapper).unwrap();
    let output = Command::new("bash")
        .args([
            "-ec",
            "source \"$1\"; printf '%s\\n' \"${HOOKS[*]}\" \"${MODULES[*]}\"",
            "test",
        ])
        .arg(config)
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "base block rstarsleep filesystems\nbtrfs\n"
    );
}

#[test]
fn arch_action_rejects_linked_outputs_and_a_compiler_that_produces_no_image() {
    let fixture = ArchAction::new();
    let outside = fixture.directory.path().join("outside");
    fs::write(&outside, "keep").unwrap();
    symlink(&outside, fixture.root.join("boot/initramfs-linux.img")).unwrap();
    let output = fixture.run("0", false);
    assert!(!output.status.success());
    assert!(!fixture.trace.exists());
    assert_eq!(fs::read_to_string(&outside).unwrap(), "keep");
    fs::remove_file(fixture.root.join("boot/initramfs-linux.img")).unwrap();
    let output = fixture.run("0", true);
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("did not produce a regular, nonempty")
    );
}

#[test]
fn hook_config_preserves_device_order_and_inserts_exactly_once() {
    let directory = tempfile::tempdir().unwrap();
    let config = directory.path().join("config");
    fs::write(
        &config,
        include_str!("../presets/plugins/arch/initramfs/config"),
    )
    .unwrap();
    for hooks in [
        "base udev block encrypt rstarsleep filesystems fsck",
        "base systemd autodetect block sd-encrypt filesystems",
    ] {
        let output = Command::new("bash")
            .args([
                "-ec",
                "read -ra HOOKS <<< \"$1\"; source \"$2\"; printf '%s\\n' \"${HOOKS[@]}\"",
                "test",
                hooks,
            ])
            .arg(&config)
            .output()
            .unwrap();
        assert!(output.status.success());
        let text = String::from_utf8(output.stdout).unwrap();
        let parsed: Vec<_> = text.lines().collect();
        assert_eq!(parsed.iter().filter(|p| **p == "rstarsleep").count(), 1);
        let index = parsed.iter().position(|p| *p == "rstarsleep").unwrap();
        assert_eq!(parsed[index + 1], "filesystems");
        assert!(parsed[..index].contains(&"block"));
    }
    let output = Command::new("bash")
        .args(["-ec", "HOOKS=(base block); source \"$1\"", "test"])
        .arg(config)
        .output()
        .unwrap();
    assert!(!output.status.success());
}

#[test]
fn finalize_hook_mounts_top_level_and_propagates_executor_failure() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let runtime = root.join("runtime");
    let tools = runtime.join("tools");
    let top = root.join("top");
    fs::create_dir_all(runtime.join("early-runtime")).unwrap();
    fs::create_dir_all(&tools).unwrap();
    fs::create_dir_all(top.join("versions")).unwrap();
    fs::write(top.join("versions/pending.toml"), "manifest").unwrap();
    let device = root.join("devices");
    fs::create_dir(&device).unwrap();
    fs::write(device.join("abcd-1234"), "mock block device").unwrap();
    let trace = root.join("trace");
    write_executable(
        &tools.join("mount"),
        "#!/bin/sh\nprintf 'mount %s\\n' \"$*\" >> \"$TRACE\"\n",
    );
    write_executable(&tools.join("findmnt"), "#!/bin/sh\nexit 1\n");
    write_executable(
        &runtime.join("early-runtime/run-rstarsleep"),
        "#!/bin/sh\nprintf 'engine %s\\n' \"$*\" >> \"$TRACE\"\nexit \"${ENGINE_EXIT:-0}\"\n",
    );
    let cmdline = root.join("cmdline");
    let script = root.join("finalize");
    let text = include_str!("../presets/initramfs/finalize")
        .replace("/usr/lib/rstarsleep", runtime.to_str().unwrap())
        .replace("/proc/cmdline", cmdline.to_str().unwrap())
        .replace("/dev/disk/by-uuid", device.to_str().unwrap())
        .replace("/run/rstarsleep-top", top.to_str().unwrap())
        .replace("[ ! -b \"$device\" ]", "[ ! -f \"$device\" ]");
    fs::write(&script, text).unwrap();
    fs::write(
        &cmdline,
        "quiet rstarsleep.pending=/versions/pending.toml rstarsleep.storage=UUID=abcd-1234\n",
    )
    .unwrap();
    for exit in ["0", "1"] {
        let output = Command::new("bash")
            .arg(&script)
            .env("TRACE", &trace)
            .env("ENGINE_EXIT", exit)
            .output()
            .unwrap();
        assert_eq!(output.status.success(), exit == "0");
        assert_eq!(
            String::from_utf8_lossy(&output.stdout).contains("committed"),
            exit == "0"
        );
    }
    let calls = fs::read_to_string(&trace).unwrap();
    assert!(calls.contains("mount -t btrfs -o rw,subvolid=5"));
    assert!(calls.contains("engine finalize-inheritance --manifest"));
    fs::write(&cmdline, "quiet\n").unwrap();
    fs::remove_file(&trace).unwrap();
    assert!(
        Command::new("bash")
            .arg(&script)
            .env("TRACE", &trace)
            .status()
            .unwrap()
            .success()
    );
    assert!(!trace.exists());
    fs::write(
        &cmdline,
        "rstarsleep.pending=/versions/pending.toml rstarsleep.pending=/other\n",
    )
    .unwrap();
    assert!(
        !Command::new("bash")
            .arg(&script)
            .env("TRACE", &trace)
            .output()
            .unwrap()
            .status
            .success()
    );
}

struct RuntimeWrapper {
    _directory: tempfile::TempDir,
    wrapper: PathBuf,
    loader_name: PathBuf,
}

impl RuntimeWrapper {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let runtime = directory.path().join("early runtime");
        fs::create_dir_all(runtime.join("bin")).unwrap();
        fs::create_dir_all(runtime.join("lib")).unwrap();
        write_executable(
            &runtime.join("bin/rstarsleep"),
            "#!/bin/sh\nprintf '%s\\n' \"$@\"\nexit \"${ENGINE_EXIT:-0}\"\n",
        );
        write_executable(
            &runtime.join("lib/ld-test.so"),
            r#"#!/bin/sh
[ "$1" = --library-path ] || exit 64
[ "$2" = "$EXPECTED_LIBRARY_PATH" ] || exit 64
shift 2
printf '%s\n' private-loader
exec "$@"
"#,
        );
        let wrapper = directory.path().join("run-rstarsleep");
        fs::write(
            &wrapper,
            include_str!("../presets/initramfs/run-rstarsleep").replace(
                "runtime=/usr/lib/rstarsleep/early-runtime",
                &format!("runtime='{}'", runtime.display()),
            ),
        )
        .unwrap();
        Self {
            wrapper,
            loader_name: runtime.join("loader-name"),
            _directory: directory,
        }
    }

    fn run(&self, exit: &str) -> Output {
        Command::new("bash")
            .arg(&self.wrapper)
            .args([
                "finalize-inheritance",
                "--manifest",
                "manifest with spaces.toml",
                "--filesystem-root",
                "root with spaces",
            ])
            .env("ENGINE_EXIT", exit)
            .env(
                "EXPECTED_LIBRARY_PATH",
                self.loader_name.parent().unwrap().join("lib"),
            )
            .output()
            .unwrap()
    }
}

#[test]
fn runtime_wrapper_accepts_loader_names_with_or_without_a_final_newline() {
    let fixture = RuntimeWrapper::new();
    for record in ["ld-test.so", "ld-test.so\n"] {
        fs::write(&fixture.loader_name, record).unwrap();
        for exit in ["0", "23"] {
            let output = fixture.run(exit);
            assert_eq!(
                output.status.code(),
                exit.parse().ok(),
                "record {record:?}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(
                String::from_utf8(output.stdout).unwrap(),
                "private-loader\nfinalize-inheritance\n--manifest\nmanifest with spaces.toml\n--filesystem-root\nroot with spaces\n"
            );
        }
    }
}

#[test]
fn runtime_wrapper_executes_static_engine_and_rejects_invalid_loader_names() {
    let fixture = RuntimeWrapper::new();
    let output = fixture.run("23");
    assert_eq!(output.status.code(), Some(23));
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "finalize-inheritance\n--manifest\nmanifest with spaces.toml\n--filesystem-root\nroot with spaces\n"
    );

    for record in [
        "",
        "\n",
        "../ld-test.so\n",
        "ld test.so\n",
        "ld-test.so/child\n",
    ] {
        fs::write(&fixture.loader_name, record).unwrap();
        let output = fixture.run("0");
        assert!(!output.status.success(), "accepted loader name {record:?}");
        assert!(output.stdout.is_empty(), "executed loader name {record:?}");
    }
}

#[test]
fn systemd_root_mount_requires_successful_inheritance_and_busybox_cannot_return_on_failure() {
    let dropin = include_str!("../presets/plugins/arch/initramfs/sysroot.conf");
    assert!(dropin.contains("Requires=rstarsleep-inheritance.service"));
    assert!(dropin.contains("After=rstarsleep-inheritance.service"));
    let unit = include_str!("../presets/plugins/arch/initramfs/rstarsleep-inheritance.service");
    assert!(unit.contains("DefaultDependencies=no") && unit.contains("Before=sysroot.mount"));
    assert!(!unit.contains("cryptsetup.target"));
    let hook = include_str!("../presets/plugins/arch/initramfs/hook");
    assert!(hook.contains("exec /usr/lib/rstarsleep/tools/sh"));
    assert!(hook.contains("while :; do"));
}
