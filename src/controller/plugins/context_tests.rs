//! Verify frozen input delivery and mount-path encoding without starting real containers.
//! 不启动真实容器，验证冻结输入传递和挂载路径编码。

use super::tests::{fixture, launcher};
use super::*;
use crate::controller::plugins::container::nspawn;
use std::fs;

/// Both probe and task receive literal secrets through the environment, including list JSON.
/// 探测和任务通过环境接收秘密的原值及列表 JSON，命令行只包含变量名。
#[test]
fn configuration_values_and_json_stay_out_of_container_arguments() {
    use std::{collections::BTreeMap, os::unix::fs::PermissionsExt};
    let mut plugin = fixture(&[], &[]);
    plugin.metadata.config.optional.push("RSTAR_SECRET".into());
    let secret = "private $value with spaces";
    let parameters = BTreeMap::from([("RSTAR_SECRET".into(), secret.into())]);
    let parameter_lists = BTreeMap::from([("RSTAR_SECRET".into(), vec![secret.into()])]);
    let root = plugin.temporary.path().join("target");
    let path = plugin.temporary.path().join("host-bin");
    let context = crate::controller::BuildContext {
        system_id: "desktop".into(),
        architecture: "x86_64".into(),
        parameters,
        parameter_lists,
        config: root.clone(),
        distribution_options: Default::default(),
        environment: Default::default(),
        persistent_mounts: Vec::new(),
        inherited_mounts: Vec::new(),
    };
    let launcher_path = path.join("systemd-nspawn");
    fs::write(
        &launcher_path,
        r#"#!/bin/sh
printf '%s\0' "$@" >> launcher-argv
printf '%s\0%s\0' "$RSTAR_SECRET" "$RSTAR_PARAMETERS_JSON" >> launcher-values
"#,
    )
    .unwrap();
    fs::set_permissions(&launcher_path, fs::Permissions::from_mode(0o755)).unwrap();
    plugin
        .invoke_environment(
            "update",
            &[],
            &root,
            "x86_64",
            (path.as_os_str(), Some(&context)),
        )
        .unwrap();
    let arguments = fs::read_to_string(plugin.root().join("launcher-argv")).unwrap();
    assert!(!arguments.contains(secret));
    assert_eq!(arguments.matches("--setenv=RSTAR_SECRET\0").count(), 2);
    assert_eq!(
        arguments
            .matches("--setenv=RSTAR_PARAMETERS_JSON\0")
            .count(),
        2
    );
    let json = serde_json::to_string(&context.parameter_lists).unwrap();
    assert_eq!(
        fs::read_to_string(plugin.root().join("launcher-values")).unwrap(),
        format!("{secret}\0{json}\0{secret}\0{json}\0")
    );
}

/// Pass native values literally to bootstrap, the guest probe and the guest task.
/// 将原生值以字面量传入 bootstrap、容器检查及容器任务。
/// # Arguments
/// No parameters. 无参数。
#[test]
fn forwards_environment_to_host_and_both_container_invocations() {
    use std::collections::BTreeMap;
    use std::os::unix::ffi::OsStringExt;
    let plugin = fixture(&[], &[]);
    let config = plugin.temporary.path().join("config");
    fs::create_dir(&config).unwrap();
    let root = plugin.temporary.path().join("target");
    let path = plugin.temporary.path().join("host-bin");
    let value =
        OsString::from_vec(b"socks5h://localhost:1080/a=b $(touch unexpected) \xff".to_vec());
    let context = crate::controller::BuildContext {
        system_id: "desktop".into(),
        architecture: "x86_64".into(),
        parameters: Default::default(),
        parameter_lists: Default::default(),
        environment: BTreeMap::from([
            ("http_proxy".into(), Some(value.clone())),
            ("HOME".into(), None),
        ]),
        distribution_options: Default::default(),
        config,
        persistent_mounts: Vec::new(),
        inherited_mounts: Vec::new(),
    };
    fs::write(plugin.operation_path("bootstrap").unwrap(),
        "test -z \"${HOME+x}\" || exit 1\nprintf '%s' \"$http_proxy\" > \"$RSTAR_ROOT/environment\"\n").unwrap();
    plugin
        .invoke_environment(
            "bootstrap",
            &[],
            &root,
            "x86_64",
            (path.as_os_str(), Some(&context)),
        )
        .unwrap();
    assert_eq!(
        fs::read(root.join("environment")).unwrap(),
        value.as_encoded_bytes()
    );
    assert!(!plugin.root().join("unexpected").exists());

    let command = nspawn(
        &plugin,
        Path::new("/usr/bin/systemd-nspawn"),
        &root,
        path.as_os_str(),
        "x86_64",
        Some(&context),
    )
    .unwrap();
    assert!(
        command
            .get_envs()
            .any(|(name, value)| name == "HOME" && value.is_none())
    );
    assert!(
        !command
            .get_args()
            .any(|arg| arg.as_encoded_bytes().starts_with(b"--setenv=HOME="))
    );

    launcher(&plugin, "", false);
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
    let mut expected = b"--setenv=http_proxy=".to_vec();
    expected.extend(value.as_encoded_bytes());
    let calls: Vec<_> = trace.split(|byte| *byte == 0).collect();
    assert_eq!(
        calls
            .iter()
            .filter(|field| **field == expected.as_slice())
            .count(),
        2
    );
    for call in calls.split(|field| *field == b"CALL") {
        if let Some(delimiter) = call.iter().position(|field| *field == b"--") {
            assert!(call[..delimiter].contains(&expected.as_slice()));
        }
    }
}

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
    fs::write(plugin.operation_path("bootstrap").unwrap(),
        "printf '%s\\0%s\\0%s' \"$RSTAR_CONFIG_DIR\" \"$RSTAR_SYSTEM_ID\" \"$RSTAR_DISTRIBUTION_ARCHITECTURE\" > \"$RSTAR_ROOT/context\"\n").unwrap();
    let context = crate::controller::BuildContext {
        system_id: "desktop".into(),
        architecture: "x86_64".into(),
        parameters: Default::default(),
        distribution_options: Default::default(),
        config: config.clone(),
        persistent_mounts: Vec::new(),
        parameter_lists: Default::default(),
        environment: Default::default(),
        inherited_mounts: Vec::new(),
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

/// Pass original configuration names to Bash without executing shell syntax in their values.
/// 将原始配置名传递给 Bash，值中的 shell 语法不执行。
/// # Arguments
/// No parameters.
/// 无参数。
#[test]
fn passes_declared_configuration_values_literally_to_host_and_guest() {
    let mut plugin = fixture(&[], &[]);
    plugin.metadata.config.required = vec!["RSTAR_DISTRIBUTION_CHANNEL".into()];
    plugin
        .metadata
        .config
        .optional
        .push("RSTAR_DISTRIBUTION_MIRROR".into());
    let config = plugin.temporary.path().join("config");
    fs::create_dir(&config).unwrap();
    let root = plugin.temporary.path().join("target");
    let path = plugin.temporary.path().join("host-bin");
    let value = "https://example.invalid/$repo/$(touch unexpected) with spaces\nsecond line";
    let context = crate::controller::BuildContext {
        system_id: "desktop".into(),
        architecture: "x86_64".into(),
        distribution_options: Default::default(),
        parameters: std::collections::BTreeMap::from([
            ("RSTAR_DISTRIBUTION_CHANNEL".into(), "stable".into()),
            ("RSTAR_DISTRIBUTION_MIRROR".into(), value.into()),
        ]),
        config,
        persistent_mounts: Vec::new(),
        parameter_lists: Default::default(),
        environment: Default::default(),
        inherited_mounts: Vec::new(),
    };
    fs::write(plugin.operation_path("bootstrap").unwrap(),
        "test -z \"${RSTAR_ARCH+x}\" && test -z \"${RSTAR_DISTRIBUTION+x}\"\nprintf '%s\\0%s\\0%s' \"$RSTAR_DISTRIBUTION_ARCHITECTURE\" \"$RSTAR_DISTRIBUTION_CHANNEL\" \"$RSTAR_DISTRIBUTION_MIRROR\" > \"$RSTAR_ROOT/options\"\n").unwrap();
    plugin
        .invoke_environment(
            "bootstrap",
            &[],
            &root,
            "x86_64",
            (path.as_os_str(), Some(&context)),
        )
        .unwrap();
    assert_eq!(
        fs::read(root.join("options")).unwrap(),
        format!("x86_64\0stable\0{value}").as_bytes()
    );
    assert!(!plugin.root().join("unexpected").exists());
    launcher(&plugin, "", false);
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
    let fields: Vec<_> = trace.split(|byte| *byte == 0).collect();
    assert_eq!(
        fields
            .iter()
            .filter(|field| **field == b"--setenv=RSTAR_DISTRIBUTION_MIRROR")
            .count(),
        2
    );
    assert_eq!(
        fields
            .iter()
            .filter(|field| **field == b"--setenv=RSTAR_DISTRIBUTION_CHANNEL")
            .count(),
        2
    );
}

/// Block all execution before tool probes when required keys are missing or keys are undeclared.
/// 必选键缺失或键未声明时在工具探测前阻止所有执行。
/// # Arguments
/// No parameters.
/// 无参数。
#[test]
fn validates_configuration_before_host_and_container_execution() {
    let mut plugin = fixture(&[], &[]);
    plugin.metadata.config.required = vec!["RSTAR_DISTRIBUTION_CHANNEL".into()];
    let config = plugin.temporary.path().join("config");
    fs::create_dir(&config).unwrap();
    let root = plugin.temporary.path().join("target");
    let path = plugin.temporary.path().join("host-bin");
    launcher(&plugin, "", false);
    for options in [
        std::collections::BTreeMap::new(),
        std::collections::BTreeMap::from([("RSTAR_DISTRIBUTION_CHANNEL".into(), " ".into())]),
        std::collections::BTreeMap::from([
            ("RSTAR_DISTRIBUTION_CHANNEL".into(), "stable".into()),
            ("RSTAR_DISTRIBUTION_CHANEL".into(), "typo".into()),
        ]),
    ] {
        let context = crate::controller::BuildContext {
            system_id: "desktop".into(),
            architecture: "x86_64".into(),
            parameters: options,
            distribution_options: Default::default(),
            config: config.clone(),
            persistent_mounts: Vec::new(),
            parameter_lists: Default::default(),
            environment: Default::default(),
            inherited_mounts: Vec::new(),
        };
        for operation in ["bootstrap", "update"] {
            let error = plugin
                .invoke_environment(
                    operation,
                    &[],
                    &root,
                    "x86_64",
                    (path.as_os_str(), Some(&context)),
                )
                .unwrap_err();
            assert!(error.to_string().contains("配置不满足声明"));
        }
    }
    assert!(!root.join("executed").exists());
    assert!(!plugin.root().join("launcher-argv").exists());
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
        parameters: Default::default(),
        distribution_options: Default::default(),
        config: config.clone(),
        persistent_mounts: Vec::new(),
        parameter_lists: Default::default(),
        environment: Default::default(),
        inherited_mounts: Vec::new(),
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
        parameters: Default::default(),
        distribution_options: Default::default(),
        config: plugin
            .temporary
            .path()
            .join(OsString::from_vec(b"frozen-\xff".to_vec())),
        persistent_mounts: Vec::new(),
        parameter_lists: Default::default(),
        environment: Default::default(),
        inherited_mounts: Vec::new(),
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

#[test]
fn host_operation_gets_transactional_directory_without_mutating_frozen_resources() {
    let mut plugin = fixture(&[], &[]);
    plugin.metadata.persistent_dirs = vec!["packages:state/cache".into()];
    fs::write(
        plugin.operation_path("bootstrap").unwrap(),
        "printf prepared > \"$RSTAR_PLUGIN_ROOT/state/cache/data\"\n",
    )
    .unwrap();
    let source = plugin.temporary.path().join("persistent-copy");
    fs::create_dir(&source).unwrap();
    let context = crate::controller::BuildContext {
        system_id: "desktop".into(),
        architecture: "x86_64".into(),
        parameters: Default::default(),
        distribution_options: Default::default(),
        config: plugin.temporary.path().join("target"),
        parameter_lists: Default::default(),
        environment: Default::default(),
        inherited_mounts: Vec::new(),
        persistent_mounts: vec![crate::controller::PersistentMount {
            source: source.clone(),
            target: "state/cache".into(),
        }],
    };
    let path = plugin.temporary.path().join("host-bin");
    plugin
        .invoke_environment(
            "bootstrap",
            &[],
            &context.config,
            "x86_64",
            (path.as_os_str(), Some(&context)),
        )
        .unwrap();
    assert_eq!(fs::read_to_string(source.join("data")).unwrap(), "prepared");
    assert!(!plugin.root().join("state").exists());
    assert!(
        plugin
            .invoke_with_path(
                "bootstrap",
                &[],
                &context.config,
                "x86_64",
                path.as_os_str()
            )
            .is_err()
    );
}

#[test]
fn container_mounts_use_explicit_relative_target_before_command_delimiter() {
    let mut plugin = fixture(&[], &[]);
    plugin.metadata.persistent_dirs = vec!["packages:state/cache".into()];
    let source = plugin.temporary.path().join("persistent-copy");
    fs::create_dir(&source).unwrap();
    let context = crate::controller::BuildContext {
        system_id: "desktop".into(),
        architecture: "x86_64".into(),
        parameters: Default::default(),
        distribution_options: Default::default(),
        config: plugin.temporary.path().join("target"),
        parameter_lists: Default::default(),
        environment: Default::default(),
        inherited_mounts: Vec::new(),
        persistent_mounts: vec![crate::controller::PersistentMount {
            source: source.clone(),
            target: "state/cache".into(),
        }],
    };
    let resources =
        super::super::persistent::container_workspace(plugin.root(), &context.persistent_mounts)
            .unwrap();
    assert!(resources.path().join("state/cache").is_dir());
    let command = super::super::container::nspawn_from(
        &plugin,
        Path::new("/bin/nspawn"),
        &context.config,
        OsStr::new("/usr/bin"),
        Some(&context),
        resources.path(),
        &plugin
            .configuration_environment(&context.parameters, &context.architecture)
            .unwrap(),
    )
    .unwrap();
    let expected = format!(
        "--bind={}:{}",
        source.display(),
        "/run/rstarsleep/plugin/state/cache"
    );
    assert!(command.get_args().any(|a| a == OsStr::new(&expected)));
}

/// Preserve the download exception while every target operation uses canonical parameter names.
/// 保留下载例外，其余目标操作使用规范参数名。
#[test]
fn unified_download_keeps_legacy_environment_and_package_arguments() {
    let plugin = fixture(&[], &[]);
    fs::write(plugin.root().join("predownload.sh"), "exit 0\n").unwrap();
    launcher(&plugin, "", false);
    let root = plugin.temporary.path().join("target");
    let path = plugin.temporary.path().join("host-bin");
    let context = crate::controller::BuildContext {
        system_id: "predownload".into(),
        architecture: "x86_64".into(),
        parameters: std::collections::BTreeMap::from([(
            "RSTAR_DISTRIBUTION_ARCHITECTURE".into(),
            "x86_64".into(),
        )]),
        distribution_options: std::collections::BTreeMap::from([
            ("Architecture".into(), "x86_64".into()),
            ("AurKeyserver".into(), "hkps://example.invalid".into()),
            ("AurDevel".into(), "false".into()),
            ("AurRebuild".into(), "sample\nother".into()),
        ]),
        config: root.clone(),
        persistent_mounts: Vec::new(),
        parameter_lists: Default::default(),
        environment: Default::default(),
        inherited_mounts: Vec::new(),
    };
    plugin
        .invoke_environment(
            "predownload",
            &["vim".into(), "linux".into()],
            &root,
            "x86_64",
            (path.as_os_str(), Some(&context)),
        )
        .unwrap();
    let trace = fs::read(plugin.root().join("launcher-argv")).unwrap();
    let fields: Vec<_> = trace.split(|byte| *byte == 0).collect();
    assert!(fields.contains(&b"--setenv=RSTAR_ARCHITECTURE".as_slice()));
    assert!(fields.contains(&b"--setenv=RSTAR_AURKEYSERVER".as_slice()));
    assert!(fields.contains(&b"--setenv=RSTAR_AURDEVEL".as_slice()));
    assert!(fields.contains(&b"--setenv=RSTAR_AURREBUILD".as_slice()));
    assert!(
        !fields
            .iter()
            .any(|value| value.starts_with(b"--setenv=RSTAR_DISTRIBUTION_"))
    );
    assert!(trace.ends_with(b"--\0bash\0/run/rstarsleep/plugin/predownload.sh\0vim\0linux\0"));
}
