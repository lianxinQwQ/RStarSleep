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
