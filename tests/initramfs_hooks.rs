//! Exercise initramfs hook ordering, dependency packaging and failure propagation.
use std::{fs, os::unix::fs::PermissionsExt, path::Path, process::Command};

fn write_executable(path: &Path, body: &str) {
    fs::write(path, body).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

#[test]
fn hook_config_preserves_device_order_and_inserts_exactly_once() {
    let directory = tempfile::tempdir().unwrap();
    let config = directory.path().join("config");
    fs::write(&config, include_str!("../presets/initramfs/config")).unwrap();
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

#[test]
fn systemd_root_mount_requires_successful_inheritance_and_busybox_cannot_return_on_failure() {
    let dropin = include_str!("../presets/initramfs/sysroot.conf");
    assert!(dropin.contains("Requires=rstarsleep-inheritance.service"));
    assert!(dropin.contains("After=rstarsleep-inheritance.service"));
    let unit = include_str!("../presets/initramfs/rstarsleep-inheritance.service");
    assert!(unit.contains("DefaultDependencies=no") && unit.contains("Before=sysroot.mount"));
    assert!(!unit.contains("cryptsetup.target"));
    let hook = include_str!("../presets/initramfs/hook");
    assert!(hook.contains("exec /usr/lib/rstarsleep/tools/sh"));
    assert!(hook.contains("while :; do"));
}
