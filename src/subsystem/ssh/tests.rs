//! Real SSH handshakes and SFTP transactions over loopback sockets.
//! 回环套接字上的真实 SSH 握手及 SFTP 事务。
use super::*;
use crate::subsystem::management::tests::fixture;
use russh::{ChannelMsg, client, keys::PrivateKeyWithHashAlg};
use russh_sftp::{client::SftpSession, protocol::OpenFlags};
use tokio::io::AsyncWriteExt;

struct Client {
    host: PublicKey,
}
/// Collect real wire output until a rendered marker arrives, with a bounded deadline.
/// 收集真实传输输出直到绘制标记出现，并设置有限超时。
async fn rendered(channel: &mut russh::Channel<client::Msg>, marker: &str) -> String {
    tokio::time::timeout(Duration::from_secs(3), async {
        let mut output = Vec::new();
        loop {
            match channel.wait().await {
                Some(ChannelMsg::Data { data }) => output.extend_from_slice(&data),
                Some(ChannelMsg::Close) | None => panic!("SSH 提前关闭 / SSH closed early"),
                _ => {}
            }
            if String::from_utf8_lossy(&output).contains(marker) {
                return String::from_utf8(output).unwrap();
            }
        }
    })
    .await
    .expect("SSH 绘制超时 / SSH rendering timed out")
}
impl client::Handler for Client {
    type Error = russh::Error;
    /// Pin the generated server key during the real SSH handshake.
    /// 在真实 SSH 握手中固定生成的服务端密钥。
    /// # Arguments
    /// - `self`, `key`: Client and received host key. 客户端及收到的主机密钥。
    async fn check_server_key(
        &mut self,
        key: &PublicKey,
    ) -> std::result::Result<bool, russh::Error> {
        Ok(key.key_data() == self.host.key_data())
    }
}

/// Substitute fixture ownership checks only; actual public-key proof remains in russh.
/// 仅替换测试目录的所有权检查；真实公钥证明仍由 russh 完成。
/// # Arguments
/// - `store`, `id`, `key`: Fixture manager, subsystem and proved key. 测试管理器、子系统及已证明公钥。
fn fixture_authorized(store: &ConfigStore, id: &str, key: &PublicKey) -> Result<bool> {
    super::super::SubsystemConfig::read(store, id)?;
    let text = fs::read_to_string(
        store
            .system_layout(id)?
            .config
            .join("subsystem/authorized_keys"),
    )?;
    Ok(parse_keys(&text)?
        .iter()
        .any(|candidate| candidate.key_data() == key.key_data()))
}

/// Public-key login, management exec, binary SFTP and key revocation form one closed loop.
/// 公钥登录、管理命令、二进制 SFTP 及公钥撤销形成闭环。
/// # Arguments
/// No parameters. 无参数。
#[tokio::test]
async fn ssh_exec_sftp_and_revocation() {
    let (_directory, workspace) = fixture();
    let identity = Arc::new(PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).unwrap());
    let wrong = Arc::new(PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).unwrap());
    let host = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).unwrap();
    let host_public = host.public_key().clone();
    let authorization = workspace
        .store
        .system_layout("apps")
        .unwrap()
        .config
        .join("subsystem/authorized_keys");
    fs::write(&authorization, identity.public_key().to_openssh().unwrap()).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let config = Arc::new(server::Config {
        keys: vec![host],
        auth_rejection_time: Duration::ZERO,
        auth_rejection_time_initial: Some(Duration::ZERO),
        ..Default::default()
    });
    let mut server = SshServer {
        store: workspace.store.clone(),
        builds: workspace.builds.clone(),
        authorize: fixture_authorized,
    };
    let running = server.run_on_socket(config, &listener);
    let shutdown = running.handle();
    let test = async {
        let connect = || {
            client::connect(
                Arc::new(client::Config::default()),
                address,
                Client {
                    host: host_public.clone(),
                },
            )
        };
        let mut connection = connect().await.unwrap();
        assert!(
            !connection
                .authenticate_publickey("apps", PrivateKeyWithHashAlg::new(wrong.clone(), None))
                .await
                .unwrap()
                .success()
        );
        assert!(
            connection
                .authenticate_publickey("apps", PrivateKeyWithHashAlg::new(identity.clone(), None))
                .await
                .unwrap()
                .success()
        );
        let mut channel = connection.channel_open_session().await.unwrap();
        channel.exec(true, "ls /").await.unwrap();
        let (mut output, mut exit) = (Vec::new(), None);
        while let Some(message) = channel.wait().await {
            match message {
                ChannelMsg::Data { data } => output.extend_from_slice(&data),
                ChannelMsg::ExitStatus { exit_status } => exit = Some(exit_status),
                _ => {}
            }
        }
        assert_eq!(exit, Some(0));
        let output = String::from_utf8(output).unwrap();
        assert!(output.contains("build"));
        assert!(output.contains("script"));
        assert!(!output.contains("subsystem"));
        let mut channel = connection.channel_open_session().await.unwrap();
        channel
            .exec(true, "status; touch /host-file")
            .await
            .unwrap();
        let mut code = None;
        while let Some(message) = channel.wait().await {
            if let ChannelMsg::ExitStatus { exit_status } = message {
                code = Some(exit_status);
            }
        }
        assert_eq!(code, Some(1));
        let mut terminal = connection.channel_open_session().await.unwrap();
        terminal
            .request_pty(true, "xterm-256color", 90, 28, 0, 0, &[])
            .await
            .unwrap();
        terminal.request_shell(true).await.unwrap();
        rendered(&mut terminal, "apps:/>").await;
        terminal
            .data(b"cat /script/missing\r".as_slice())
            .await
            .unwrap();
        let error = rendered(&mut terminal, "operation failed").await;
        assert!(error.contains("apps:/>"));
        assert!(!error.contains("\x1b[H"));
        assert!(!error.contains("\x1b[r"));
        terminal
            .data(b"unknown-command\r".as_slice())
            .await
            .unwrap();
        let error = rendered(&mut terminal, "unknown command").await;
        assert!(error.contains("apps:/>"));
        assert!(!error.contains("\x1b[H"));
        assert!(!error.contains("\x1b[r"));
        terminal.data(b"clear\rtui\r".as_slice()).await.unwrap();
        rendered(&mut terminal, "Management workspace").await;
        terminal.data(b"j".as_slice()).await.unwrap();
        let result = rendered(&mut terminal, "[]").await;
        assert!(result.contains("Result"));
        assert!(!result.contains("apps:/>"));
        terminal.data(b"h".as_slice()).await.unwrap();
        let error = rendered(&mut terminal, "operation failed").await;
        assert!(error.contains("Result"));
        assert!(error.contains("\x1b[?1049h"));
        terminal.window_change(70, 20, 0, 0).await.unwrap();
        rendered(&mut terminal, "\x1b[20;1H").await;
        terminal.data(b"qexit\r".as_slice()).await.unwrap();
        let mut code = None;
        while let Some(message) = terminal.wait().await {
            if let ChannelMsg::ExitStatus { exit_status } = message {
                code = Some(exit_status);
            }
        }
        assert_eq!(code, Some(0));
        let jobs = super::super::admin::directory(&workspace.store, "apps").unwrap();
        fs::create_dir_all(&jobs).unwrap();
        let job = super::super::admin::Job {
            task: "build-wire".into(),
            operation: "build".into(),
            state: "complete".into(),
            started: 1,
            finished: Some(2),
            exit_code: Some(7),
            error: None,
        };
        super::super::write_record(&jobs.join("build-wire.toml"), &job).unwrap();
        fs::write(jobs.join("build-wire.log"), b"FINAL-WIRE\n").unwrap();
        let mut followed = connection.channel_open_session().await.unwrap();
        followed.exec(true, "logs build --follow").await.unwrap();
        followed.eof().await.unwrap();
        let (mut output, mut exit) = (Vec::new(), None);
        while let Some(message) = followed.wait().await {
            match message {
                ChannelMsg::Data { data } => output.extend_from_slice(&data),
                ChannelMsg::ExitStatus { exit_status } => exit = Some(exit_status),
                _ => {}
            }
        }
        assert_eq!(exit, Some(7));
        assert_eq!(output, b"FINAL-WIRE\n");

        // Live pages stay in the dashboard and observe the same durable build.
        // 实时页保留在面板中，并观察同一个持久构建任务。
        let build_lock = fs::File::create(jobs.join("build-wire.lock")).unwrap();
        let supervisor_lock = fs::File::create(jobs.join("build-wire.supervisor")).unwrap();
        use std::os::fd::AsRawFd;
        for lock in [&build_lock, &supervisor_lock] {
            assert_eq!(
                unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
                0
            );
        }
        let registration = workspace.builds.begin("apps", &job.task).unwrap();
        let mut job = job;
        job.state = "running".into();
        job.finished = None;
        job.exit_code = None;
        super::super::write_record(&jobs.join("build-wire.toml"), &job).unwrap();
        registration.recorded().unwrap();
        fs::write(jobs.join("build-wire.log"), b"BUILD-FIRST\n").unwrap();
        let mut cli = connection.channel_open_session().await.unwrap();
        cli.request_pty(true, "xterm-256color", 90, 28, 0, 0, &[])
            .await
            .unwrap();
        cli.request_shell(true).await.unwrap();
        rendered(&mut cli, "apps:/>").await;
        cli.data(b"build status\r".as_slice()).await.unwrap();
        let output = rendered(&mut cli, "BUILD-FIRST\r\n").await;
        assert!(output.contains("BUILD-FIRST\r\n"));
        fs::write(
            jobs.join("build-wire.log"),
            b"BUILD-FIRST\nBUILD-CRLF\r\nBUILD-NEXT\n",
        )
        .unwrap();
        let output = rendered(&mut cli, "BUILD-NEXT\r\n").await;
        assert!(output.contains("BUILD-CRLF\r\nBUILD-NEXT\r\n"));
        assert!(!output.contains("\r\r\n"));
        cli.close().await.unwrap();
        fs::write(jobs.join("build-wire.log"), b"BUILD-FIRST\n").unwrap();
        // A new SSH connection reads the same daemon-owned build registration.
        // 新的 SSH 连接读取同一份 daemon 所属构建登记。
        let mut observer = connect().await.unwrap();
        assert!(
            observer
                .authenticate_publickey("apps", PrivateKeyWithHashAlg::new(identity.clone(), None))
                .await
                .unwrap()
                .success()
        );
        let mut observing = observer.channel_open_session().await.unwrap();
        observing.exec(true, "build status").await.unwrap();
        rendered(&mut observing, "BUILD-FIRST").await;
        observing.close().await.unwrap();
        drop(observer);
        assert!(workspace.builds.active("apps").unwrap().is_some());
        let mut watching = connection.channel_open_session().await.unwrap();
        watching.exec(true, "build status").await.unwrap();
        watching.eof().await.unwrap();
        rendered(&mut watching, "BUILD-FIRST").await;
        fs::write(jobs.join("build-wire.log"), b"BUILD-FIRST\nBUILD-LATER\n").unwrap();
        rendered(&mut watching, "BUILD-LATER").await;

        let mut panel = connection.channel_open_session().await.unwrap();
        panel
            .request_pty(true, "xterm-256color", 100, 30, 0, 0, &[])
            .await
            .unwrap();
        panel.request_shell(true).await.unwrap();
        rendered(&mut panel, "apps:/>").await;
        panel.data(b"tui\rb".as_slice()).await.unwrap();
        let page = rendered(&mut panel, "BUILD-LATER").await;
        assert!(page.contains("Management workspace"));
        assert!(!page.contains("\x1b[?1049l"));
        panel.data(b"\x03".as_slice()).await.unwrap();
        rendered(&mut panel, "Stopping build").await;
        assert!(jobs.join("build-wire.cancel").exists());
        let mut stopper = connection.channel_open_session().await.unwrap();
        stopper.exec(true, "build stop").await.unwrap();
        stopper.eof().await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(20), async {
                loop {
                    match stopper.wait().await {
                        Some(
                            ChannelMsg::Data { .. }
                            | ChannelMsg::ExtendedData { .. }
                            | ChannelMsg::ExitStatus { .. }
                            | ChannelMsg::Close,
                        )
                        | None => break,
                        _ => {}
                    }
                }
            })
            .await
            .is_err()
        );
        fs::write(
            jobs.join("build-wire.log"),
            b"BUILD-FIRST\nBUILD-LATER\nBUILD-STOPPED\n",
        )
        .unwrap();
        job.state = "cancelled".into();
        job.finished = Some(3);
        job.exit_code = Some(130);
        super::super::write_record(&jobs.join("build-wire.toml"), &job).unwrap();
        drop(registration);
        let page = rendered(&mut panel, "This system is not building").await;
        assert!(page.contains("BUILD-STOPPED"));
        let mut output = Vec::new();
        let mut code = None;
        while let Some(message) = watching.wait().await {
            match message {
                ChannelMsg::Data { data } => output.extend_from_slice(&data),
                ChannelMsg::ExitStatus { exit_status } => code = Some(exit_status),
                _ => {}
            }
        }
        assert_eq!(code, Some(130));
        assert!(String::from_utf8(output).unwrap().contains("BUILD-STOPPED"));
        let mut output = Vec::new();
        while let Some(message) = stopper.wait().await {
            if let ChannelMsg::Data { data } = message {
                output.extend_from_slice(&data);
            }
        }
        assert!(String::from_utf8(output).unwrap().contains("build stopped"));
        let mut idle = connection.channel_open_session().await.unwrap();
        idle.exec(true, "build status").await.unwrap();
        let output = rendered(&mut idle, "this system is not building").await;
        assert!(!output.contains("BUILD-FIRST"));
        while idle.wait().await.is_some() {}

        let console = super::super::console::directory(&workspace.store, "apps").unwrap();
        fs::create_dir_all(&console).unwrap();
        let capture_lock = fs::File::create(console.join("capture.lock")).unwrap();
        assert_eq!(
            unsafe { libc::flock(capture_lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
            0
        );
        super::super::write_record(
            &console.join("current.toml"),
            &super::super::console::Run {
                task: "boot-test".into(),
                machine: "test".into(),
                offset: 0,
                shutdown_offset: None,
                finished: false,
                exit_code: None,
                error: None,
                invocation: None,
                origin: None,
            },
        )
        .unwrap();
        fs::write(
            console.join("console.log"),
            b"\x1b[?1049l\x1b[HCONSOLE-FIRST\r\n",
        )
        .unwrap();
        panel.data(b"c".as_slice()).await.unwrap();
        let page = rendered(&mut panel, "CONSOLE-FIRST").await;
        assert!(page.contains("Management workspace"));
        assert!(page.contains("read-only"));
        assert!(!page.contains("\x1b[?1049l"));
        fs::write(
            console.join("console.log"),
            b"\x1b[?1049l\x1b[HCONSOLE-FIRST\r\nCONSOLE-LATER\r\n",
        )
        .unwrap();
        let page = rendered(&mut panel, "CONSOLE-LATER").await;
        assert!(page.contains("Management workspace"));
        panel.window_change(65, 20, 0, 0).await.unwrap();
        rendered(&mut panel, "\x1b[20;1H").await;
        panel.data(b"b".as_slice()).await.unwrap();
        rendered(&mut panel, "BUILD-STOPPED").await;
        panel.data(b"qexit\r".as_slice()).await.unwrap();
        while panel.wait().await.is_some() {}
        drop((capture_lock, build_lock, supervisor_lock));

        let channel = connection.channel_open_session().await.unwrap();
        channel.request_subsystem(true, "sftp").await.unwrap();
        let sftp = SftpSession::new(channel.into_stream()).await.unwrap();
        let names = sftp
            .read_dir("/")
            .await
            .unwrap()
            .map(|entry| entry.file_name())
            .collect::<Vec<_>>();
        assert_eq!(names, ["build", "script"]);
        assert!(sftp.read("/subsystem/authorized_keys").await.is_err());
        let data = b"binary\0\xff\x01contents";
        let mut upload = sftp
            .open_with_flags(
                "/script/data.tmp",
                OpenFlags::CREATE | OpenFlags::WRITE | OpenFlags::TRUNCATE,
            )
            .await
            .unwrap();
        upload.write_all(data).await.unwrap();
        upload.flush().await.unwrap();
        assert!(workspace.read("/script/data.tmp").is_err());
        assert!(
            crate::config::lock_configuration(&workspace.store.system_layout("apps").unwrap().root)
                .is_err()
        );
        upload.shutdown().await.unwrap();
        sftp.rename("/script/data.tmp", "/script/data.bin")
            .await
            .unwrap();
        assert_eq!(sftp.read("/script/data.bin").await.unwrap(), data);
        let mut abandoned = sftp.create("/script/abandoned").await.unwrap();
        abandoned.write_all(b"partial").await.unwrap();
        drop(abandoned);
        // Dropping the file sends CLOSE; abandoning the entire transport is tested separately.
        // 丢弃文件会发送 CLOSE；放弃整个传输的行为另行验证。
        fs::write(&authorization, "").unwrap();
        let mut next = connect().await.unwrap();
        assert!(
            !next
                .authenticate_publickey("apps", PrivateKeyWithHashAlg::new(identity, None))
                .await
                .unwrap()
                .success()
        );
        assert_eq!(sftp.read("/script/data.bin").await.unwrap(), data);
        connection
            .disconnect(russh::Disconnect::ByApplication, "done", "")
            .await
            .unwrap();
        next.disconnect(russh::Disconnect::ByApplication, "done", "")
            .await
            .unwrap();
        shutdown.shutdown("测试完成 / test complete".into());
    };
    let (result, ()) = tokio::join!(running, async {
        tokio::time::timeout(Duration::from_secs(20), test)
            .await
            .unwrap()
    });
    result.unwrap();
}

/// Malformed or option-bearing key lines fail rather than partially authorizing a file.
/// 公钥行格式错误或含选项时失败，不部分授权整个文件。
/// # Arguments
/// No parameters. 无参数。
#[test]
fn key_file_validation_is_strict() {
    let key = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519)
        .unwrap()
        .public_key()
        .to_openssh()
        .unwrap();
    assert_eq!(
        parse_keys(&format!("# admin\n{key} person\n"))
            .unwrap()
            .len(),
        1
    );
    assert!(parse_keys(&format!("{key}\ninvalid")).is_err());
    assert!(parse_keys(&format!("command=\"id\" {key}")).is_err());
    assert!(parse_keys("\n# empty\n").unwrap().is_empty());
}

/// RSA authorization must verify signatures with both modern SHA-2 algorithms.
/// RSA 授权必须能验证两种现代 SHA-2 算法的签名。
#[tokio::test]
async fn rsa_sha2_login_and_revocation() {
    use russh::keys::ssh_key::{HashAlg, private::RsaKeypair};

    let (_directory, workspace) = fixture();
    let key = RsaKeypair::random(&mut rand::rng(), 2048).unwrap();
    let identity = Arc::new(PrivateKey::new(key.into(), "test RSA / 测试 RSA").unwrap());
    let host = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).unwrap();
    let host_public = host.public_key().clone();
    let authorization = workspace
        .store
        .system_layout("apps")
        .unwrap()
        .config
        .join("subsystem/authorized_keys");
    fs::write(&authorization, identity.public_key().to_openssh().unwrap()).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let config = Arc::new(server::Config {
        methods: (&[russh::MethodKind::PublicKey][..]).into(),
        keys: vec![host],
        auth_rejection_time: Duration::ZERO,
        auth_rejection_time_initial: Some(Duration::ZERO),
        ..Default::default()
    });
    let mut server = SshServer {
        store: workspace.store.clone(),
        builds: workspace.builds.clone(),
        authorize: fixture_authorized,
    };
    let running = server.run_on_socket(config, &listener);
    let shutdown = running.handle();
    let test = async {
        for hash in [HashAlg::Sha256, HashAlg::Sha512] {
            let mut connection = client::connect(
                Arc::new(client::Config::default()),
                address,
                Client {
                    host: host_public.clone(),
                },
            )
            .await
            .unwrap();
            assert!(
                connection
                    .authenticate_publickey(
                        "apps",
                        PrivateKeyWithHashAlg::new(identity.clone(), Some(hash)),
                    )
                    .await
                    .unwrap()
                    .success(),
                "RSA SHA-2 登录失败 / RSA SHA-2 login failed: {hash:?}"
            );
            let mut channel = connection.channel_open_session().await.unwrap();
            channel.exec(true, "pwd").await.unwrap();
            let (mut output, mut exit) = (Vec::new(), None);
            while let Some(message) = channel.wait().await {
                match message {
                    ChannelMsg::Data { data } => output.extend_from_slice(&data),
                    ChannelMsg::ExitStatus { exit_status } => exit = Some(exit_status),
                    _ => {}
                }
            }
            assert_eq!(exit, Some(0));
            assert_eq!(output, b"/\n");
            connection
                .disconnect(
                    russh::Disconnect::ByApplication,
                    "测试完成 / test complete",
                    "",
                )
                .await
                .unwrap();
        }
        fs::write(&authorization, "").unwrap();
        let mut connection = client::connect(
            Arc::new(client::Config::default()),
            address,
            Client { host: host_public },
        )
        .await
        .unwrap();
        assert!(
            !connection
                .authenticate_publickey(
                    "apps",
                    PrivateKeyWithHashAlg::new(identity, Some(HashAlg::Sha512)),
                )
                .await
                .unwrap()
                .success(),
            "撤销的 RSA 公钥仍可登录 / Revoked RSA key can still log in"
        );
        connection
            .disconnect(
                russh::Disconnect::ByApplication,
                "测试完成 / test complete",
                "",
            )
            .await
            .unwrap();
        shutdown.shutdown("测试完成 / test complete".into());
    };
    let (result, ()) = tokio::join!(running, async {
        tokio::time::timeout(Duration::from_secs(20), test)
            .await
            .expect("RSA SSH 测试超时 / RSA SSH test timed out")
    });
    result.unwrap();
}
