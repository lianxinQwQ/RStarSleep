//! Verify frozen input delivery and mount-path encoding without starting real containers.
//! 不启动真实容器，验证冻结输入传递和挂载路径编码。

use super::tests::{fixture, launcher};
use super::*;
use std::fs;

/// Verify bootstrap receives the archived directory and stable system identity unchanged.
/// 验证 bootstrap 原样接收留档目录和稳定系统标识。
///
/// # Arguments
///
/// No parameters.
/// 无参数。
#[test]
fn passes_frozen_configuration_to_host_operations() {
    let plugin = fixture(&[], &[]);
    let config = plugin.temporary.path().join("frozen config");
    fs::create_dir(&config).unwrap();
    let root = plugin.temporary.path().join("target");
    fs::write(plugin.entry_path(),
        "printf '%s\\0%s\\0%s' \"$RSTAR_CONFIG_DIR\" \"$RSTAR_SYSTEM_ID\" \"$RSTAR_ARCH\" > \"$RSTAR_ROOT/context\"\n").unwrap();
    let context = crate::controller::BuildContext {
        system_id: "desktop".into(),
        architecture: "x86_64".into(),
        config: config.clone(),
    };
    let path = plugin.temporary.path().join("host-bin");
    plugin
        .invoke_environment(
            "bootstrap",
            &[],
            &root,
            "x86_64",
            (path.as_os_str(), Some(&context)),
        )
        .unwrap();
    let actual = fs::read(root.join("context")).unwrap();
    let fields = actual.split(|byte| *byte == 0).collect::<Vec<_>>();
    assert_eq!(
        fields,
        [config.as_os_str().as_encoded_bytes(), b"desktop", b"x86_64"]
    );
}

/// Verify both target probe and entry receive configuration mounts before their command delimiter.
/// 验证目标探测和入口都在命令分隔符之前接收配置挂载。
///
/// # Arguments
///
/// No parameters.
/// 无参数。
#[test]
fn passes_build_context_to_guest_probe_and_task_before_delimiter() {
    let plugin = fixture(&[], &[]);
    launcher(&plugin, "", false);
    let config = plugin.temporary.path().join("frozen config");
    fs::create_dir(&config).unwrap();
    let root = plugin.temporary.path().join("target");
    let path = plugin.temporary.path().join("host-bin");
    let context = crate::controller::BuildContext {
        system_id: "desktop".into(),
        architecture: "x86_64".into(),
        config: config.clone(),
    };
    plugin
        .invoke_environment(
            "update",
            &[],
            &root,
            "x86_64",
            (path.as_os_str(), Some(&context)),
        )
        .unwrap();
    let trace = fs::read(plugin.root().join("launcher-argv")).unwrap();
    let fields = trace
        .split(|byte| *byte == 0)
        .filter(|field| !field.is_empty())
        .collect::<Vec<_>>();
    let calls = fields
        .split(|field| *field == b"CALL")
        .filter(|call| !call.is_empty())
        .collect::<Vec<_>>();
    assert_eq!(calls.len(), 2);
    let mut expected_bind = OsString::from("--bind-ro=");
    expected_bind.push(&config);
    expected_bind.push(":/run/rstarsleep/config");
    for call in calls {
        let delimiter = call.iter().position(|argument| *argument == b"--").unwrap();
        let options = &call[..delimiter];
        assert!(options.contains(&expected_bind.as_encoded_bytes()));
        assert!(options.contains(&b"--setenv=RSTAR_CONFIG_DIR=/run/rstarsleep/config".as_slice()));
        assert!(options.contains(&b"--setenv=RSTAR_SYSTEM_ID=desktop".as_slice()));
    }
}

/// Preserve native mount bytes and reject characters that nspawn treats as mount syntax.
/// 保留原生挂载字节，并拒绝 nspawn 视为挂载语法的字符。
///
/// # Arguments
///
/// No parameters.
/// 无参数。
#[test]
fn mount_arguments_preserve_native_bytes_and_reject_ambiguous_paths() {
    use std::os::unix::ffi::OsStringExt;
    let plugin = fixture(&[], &[]);
    let root = plugin.temporary.path().join("target");
    let context = crate::controller::BuildContext {
        system_id: "desktop".into(),
        architecture: "x86_64".into(),
        config: plugin
            .temporary
            .path()
            .join(OsString::from_vec(b"frozen-\xff".to_vec())),
    };
    let command = nspawn(
        &plugin,
        Path::new("/usr/bin/systemd-nspawn"),
        &root,
        OsStr::new("/usr/bin"),
        "x86_64",
        Some(&context),
    )
    .unwrap();
    let mut expected = OsString::from("--bind-ro=");
    expected.push(&context.config);
    expected.push(":/run/rstarsleep/config");
    assert!(command.get_args().any(|argument| argument == expected));
    for suffix in ["bad:config", "bad\\config"] {
        let context = crate::controller::BuildContext {
            config: plugin.temporary.path().join(suffix),
            ..context.clone()
        };
        assert!(matches!(
            nspawn(
                &plugin,
                Path::new("/usr/bin/systemd-nspawn"),
                &root,
                OsStr::new("/usr/bin"),
                "x86_64",
                Some(&context)
            ),
            Err(Error::Invalid(_))
        ));
    }
}
