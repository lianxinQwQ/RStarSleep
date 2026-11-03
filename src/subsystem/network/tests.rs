//! Exercise the production kernel path in disposable user/mount/network namespaces.
//! 在可丢弃用户、挂载及网络命名空间中验证生产内核路径。
use super::*;
use crate::subsystem::network::{DnsConfig, ProxyConfig};
use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream, UdpSocket},
    process::{Child, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

struct Process(Child);
impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Allow compatibility runs against a separately downloaded Xray without replacing host tools.
/// 允许使用单独下载的 Xray 进行兼容性验证，无需替换宿主工具。
fn xray_executable() -> PathBuf {
    std::env::var_os("RSTAR_TEST_XRAY")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/usr/bin/xray"))
}

fn command(program: &str, args: &[&str]) -> Output {
    let output = Command::new(program).args(args).output().unwrap();
    assert!(
        output.status.success(),
        "命令失败 / command failed: {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn answer(mut query: Vec<u8>) -> Vec<u8> {
    let kind = u16::from_be_bytes(query[query.len() - 4..query.len() - 2].try_into().unwrap());
    query[2] = 0x81;
    query[3] = 0x80;
    let record = match kind {
        1 => vec![192, 0, 2, 5],
        28 => "2001:db8::5"
            .parse::<std::net::Ipv6Addr>()
            .unwrap()
            .octets()
            .to_vec(),
        16 => b"\x05fixed".to_vec(),
        _ => Vec::new(),
    };
    if !record.is_empty() {
        query[7] = 1;
        query.extend([0xc0, 0x0c]);
        query.extend(kind.to_be_bytes());
        query.extend([0, 1, 0, 0, 0, 30]);
        query.extend((record.len() as u16).to_be_bytes());
        query.extend(record);
    }
    query
}

fn proxy_connection(mut stream: TcpStream, requests: Arc<Mutex<Vec<(String, u16)>>>, http: bool) {
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    let (host, port) = if http {
        let mut bytes = Vec::new();
        while !bytes.ends_with(b"\r\n\r\n") && bytes.len() < 8192 {
            let mut b = [0];
            if stream.read_exact(&mut b).is_err() {
                return;
            }
            bytes.push(b[0]);
        }
        let text = String::from_utf8(bytes).unwrap();
        let endpoint = text.split_whitespace().nth(1).unwrap();
        let (host, port) = endpoint.rsplit_once(':').unwrap();
        stream
            .write_all(b"HTTP/1.1 200 Connection established\r\n\r\n")
            .unwrap();
        (
            host.trim_matches(['[', ']']).to_string(),
            port.parse().unwrap(),
        )
    } else {
        let mut greeting = [0; 2];
        if stream.read_exact(&mut greeting).is_err() {
            return;
        }
        let mut methods = vec![0; greeting[1] as usize];
        stream.read_exact(&mut methods).unwrap();
        stream.write_all(&[5, 0]).unwrap();
        let mut header = [0; 4];
        if stream.read_exact(&mut header).is_err() {
            return;
        }
        let host = match header[3] {
            1 => {
                let mut addr = [0; 4];
                stream.read_exact(&mut addr).unwrap();
                std::net::Ipv4Addr::from(addr).to_string()
            }
            4 => {
                let mut addr = [0; 16];
                stream.read_exact(&mut addr).unwrap();
                std::net::Ipv6Addr::from(addr).to_string()
            }
            3 => {
                let mut len = [0];
                stream.read_exact(&mut len).unwrap();
                let mut addr = vec![0; len[0] as usize];
                stream.read_exact(&mut addr).unwrap();
                String::from_utf8(addr).unwrap()
            }
            _ => panic!("无效 SOCKS 地址 / invalid SOCKS address"),
        };
        let mut port = [0; 2];
        stream.read_exact(&mut port).unwrap();
        if header[1] == 3 {
            let udp = UdpSocket::bind("127.0.0.1:0").unwrap();
            udp.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
            let mut reply = vec![5, 0, 0, 1, 127, 0, 0, 1];
            reply.extend(udp.local_addr().unwrap().port().to_be_bytes());
            stream.write_all(&reply).unwrap();
            let mut packet = [0; 2048];
            while let Ok((n, peer)) = udp.recv_from(&mut packet) {
                assert_eq!(&packet[..3], &[0, 0, 0]);
                let size = match packet[3] {
                    1 => 10,
                    4 => 22,
                    3 => 7 + packet[4] as usize,
                    _ => panic!(),
                };
                requests.lock().unwrap().push(("udp-relay".into(), 443));
                let mut response = packet[..size].to_vec();
                response.extend(b"via-fixed-udp");
                udp.send_to(&response, peer).unwrap();
                assert!(n > size);
            }
            return;
        }
        assert_eq!(header[1], 1);
        stream.write_all(&[5, 0, 0, 1, 127, 0, 0, 1, 0, 0]).unwrap();
        (host, u16::from_be_bytes(port))
    };
    requests.lock().unwrap().push((host.clone(), port));
    if host == "198.18.0.53" && port == 53 {
        loop {
            let mut size = [0; 2];
            if stream.read_exact(&mut size).is_err() {
                break;
            }
            let mut query = vec![0; u16::from_be_bytes(size) as usize];
            if stream.read_exact(&mut query).is_err() {
                break;
            }
            let response = answer(query);
            if stream
                .write_all(&(response.len() as u16).to_be_bytes())
                .and_then(|_| stream.write_all(&response))
                .is_err()
            {
                break;
            }
        }
    } else {
        let mut probe = [0; 64];
        if stream.read(&mut probe).is_ok() {
            let _ = stream.write_all(b"via-fixed-tcp");
        }
    }
}

fn guest(plan: &Plan, script: &str, args: &[&str]) -> Output {
    let mut cmd = Command::new("ip");
    cmd.args(["netns", "exec", &plan.namespace, "python3", "-c", script])
        .args(args);
    cmd.output().unwrap()
}

const TCP: &str = "import socket,sys; s=socket.create_connection((sys.argv[1],int(sys.argv[2])),2); s.sendall(b'probe'); sys.stdout.buffer.write(s.recv(128))";
const UDP: &str = "import socket,sys; s=socket.socket(socket.AF_INET6 if ':' in sys.argv[1] else socket.AF_INET,socket.SOCK_DGRAM); s.settimeout(2); s.sendto(b'probe',(sys.argv[1],443)); sys.stdout.buffer.write(s.recv(128))";
const DNS: &str = r#"import socket,sys,struct
q=bytes.fromhex('123401000001000000000000056669786564076578616d706c650000')+bytes([int(sys.argv[2])])+b'\0\1'
tcp=sys.argv[3]=='tcp'
s=socket.socket(socket.AF_INET,socket.SOCK_STREAM if tcp else socket.SOCK_DGRAM)
s.settimeout(3)
s.connect((sys.argv[1],53))
s.sendall(struct.pack('!H',len(q))+q if tcp else q)
def read(size):
    data=b''
    while len(data)<size:
        chunk=s.recv(size-len(data))
        assert chunk, 'DNS response truncated / DNS 响应截断'
        data+=chunk
    return data
r=read(struct.unpack('!H',read(2))[0]) if tcp else s.recv(4096)
assert len(r)>=12 and r[:2]==q[:2] and r[2]&128 and r[3]&15==0, 'DNS response invalid / DNS 响应无效'
assert struct.unpack('!H',r[6:8])[0]>=1 if int(sys.argv[2]) in (1,28,16) else True, 'DNS answer missing / DNS 应答缺失'
sys.stdout.buffer.write(r)
"#;

/// Exercise the numeric-string representation used by current iproute2 releases.
/// 验证当前 iproute2 使用的数字字符串表示形式。
#[test]
fn routing_table_identifiers() {
    assert_eq!(table_id(&serde_json::json!(123456)), Some(123456));
    assert_eq!(table_id(&serde_json::json!("123456")), Some(123456));
    assert_eq!(table_id(&serde_json::json!("main")), None);
    assert_eq!(table_id(&serde_json::json!(u64::MAX)), None);
}

/// Validate the reported SOCKS policy with the real Xray parser, without network setup.
/// 使用真实 Xray 解析器校验所报告的 SOCKS 策略，不创建网络。
#[test]
fn reported_socks_policy_passes_xray_validation() {
    let xray = xray_executable();
    if !xray.exists() {
        assert!(
            std::env::var_os("RSTAR_TEST_XRAY").is_none(),
            "指定的 Xray 不存在 / selected Xray does not exist"
        );
        eprintln!("跳过 Xray 配置测试，工具缺失 / skipping Xray configuration test, missing tool");
        return;
    }
    let config: crate::subsystem::SubsystemConfig = toml::from_str(
        r#"
[network]
mode = "proxy"
[network.proxy]
protocol = "socks5"
address = "127.0.0.1"
port = 10808
[network.dns]
address = "8.8.8.8"
port = 53
"#,
    )
    .unwrap();
    config.validate().unwrap();
    let directory = tempfile::tempdir().unwrap();
    let store = ConfigStore::new(directory.path()).unwrap();
    let config = config.network.unwrap();
    let plan = Plan::configured(&store, "abc", &config).unwrap();
    let path = directory.path().join("xray.json");
    let rendered = plan.xray(&config);
    let dns = &rendered["outbounds"][1];
    assert!(dns.get("proxySettings").is_none());
    assert_eq!(dns["streamSettings"]["sockopt"]["dialerProxy"], "fixed");
    fs::write(&path, serde_json::to_vec(&rendered).unwrap()).unwrap();
    check_xray(&xray, &path, &config).unwrap();
    eprintln!("所报告配置通过 Xray 预检 / reported configuration passed Xray preflight");
}

/// Preserve actionable parser errors and exit status while suppressing credential-bearing output.
/// 保留可操作的解析错误及退出状态，同时抑制含凭据输出。
#[test]
fn xray_diagnostics_preserve_failure_and_protect_credentials() {
    use std::os::unix::process::ExitStatusExt;
    let mut config: NetworkConfig = toml::from_str(
        r#"
mode = "proxy"
[proxy]
protocol = "socks5"
address = "127.0.0.1"
port = 10808
[dns]
address = "8.8.8.8"
port = 53
"#,
    )
    .unwrap();
    let output = Output {
        status: std::process::ExitStatus::from_raw(23 << 8),
        stdout: b"Failed to build config".to_vec(),
        stderr: b"unsupported field".to_vec(),
    };
    let error = xray_check_error(Path::new("/usr/bin/xray"), &output, &config).to_string();
    assert!(error.contains("Xray configuration preflight failed"));
    assert!(error.contains("23"));
    assert!(error.contains("Failed to build config"));
    assert!(error.contains("unsupported field"));
    config.proxy.username = Some("private-user".into());
    config.proxy.password = Some("private-password".into());
    let output = Output {
        stderr: b"private-user private-password".to_vec(),
        ..output
    };
    let error = xray_check_error(Path::new("/usr/bin/xray"), &output, &config).to_string();
    assert!(error.contains("23"));
    assert!(!error.contains("private-user"));
    assert!(!error.contains("private-password"));
}

/// The parent never touches host networking; every mutation occurs after unshare.
/// 父进程不修改宿主网络；所有变更均在 unshare 后执行。
#[test]
fn isolated_fixed_proxy_path() {
    if std::env::var_os("RSTAR_PROXY_TEST_CHILD").is_none() {
        for name in ["xray", "ip", "nft", "python3", "unshare", "mount", "sysctl"] {
            if Command::new("sh")
                .args(["-c", "command -v \"$1\"", "sh", name])
                .output()
                .map_or(true, |o| !o.status.success())
            {
                eprintln!(
                    "跳过隔离网络测试，工具缺失 / skipping isolated network test, missing tool: {name}"
                );
                return;
            }
        }
        let probe = Command::new("unshare")
            .args(["--user", "--map-root-user", "--mount", "--net", "true"])
            .output()
            .unwrap();
        if !probe.status.success() {
            eprintln!(
                "跳过隔离网络测试，用户命名空间不可用 / skipping isolated network test, user namespaces unavailable"
            );
            return;
        }
        let mut cmd = Command::new("unshare");
        cmd.args([
            "--user",
            "--map-root-user",
            "--mount",
            "--net",
            "--pid",
            "--fork",
            "--mount-proc",
        ])
        .arg(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "subsystem::network::runtime::tests::isolated_fixed_proxy_path",
            "--nocapture",
        ])
        .env("RSTAR_PROXY_TEST_CHILD", "1");
        let output = commands::output(cmd, Duration::from_secs(55)).unwrap();
        assert!(
            output.status.success(),
            "隔离数据路径失败 / isolated data path failed:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    assert_eq!(unsafe { libc::geteuid() }, 0);
    let mapping = fs::read_to_string("/proc/self/uid_map").unwrap();
    assert_eq!(mapping.split_whitespace().nth(2), Some("1"));
    command("mount", &["--make-rprivate", "/"]);
    command("mount", &["-t", "tmpfs", "tmpfs", "/run"]);
    fs::create_dir_all("/run/netns").unwrap();
    command("ip", &["link", "set", "lo", "up"]);
    command("ip", &["address", "add", "198.18.0.100/32", "dev", "lo"]);
    command(
        "ip",
        &[
            "-6",
            "address",
            "add",
            "2001:db8::100/128",
            "dev",
            "lo",
            "nodad",
        ],
    );
    let sentinel = TcpListener::bind("198.18.0.100:8080").unwrap();
    sentinel.set_nonblocking(true).unwrap();
    for protocol in ["socks5", "http"] {
        let directory = tempfile::tempdir().unwrap();
        let store = ConfigStore::new(directory.path()).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let stopped = Arc::new(AtomicBool::new(false));
        let log = requests.clone();
        let stop = stopped.clone();
        let port = listener.local_addr().unwrap().port();
        let http = protocol == "http";
        let proxy = thread::spawn(move || {
            while !stop.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let log = log.clone();
                        thread::spawn(move || proxy_connection(stream, log, http));
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5))
                    }
                    Err(e) => panic!("{e}"),
                }
            }
        });
        let config = NetworkConfig {
            mode: "proxy".into(),
            proxy: ProxyConfig {
                protocol: protocol.into(),
                address: "127.0.0.1".into(),
                port,
                username: None,
                password: None,
            },
            dns: DnsConfig {
                address: "198.18.0.53".parse().unwrap(),
                port: 53,
            },
            outbound_mark: 0,
            ipv4_subnet: None,
            ipv6_subnet: None,
        };
        let plan = Plan::new(&store, "apps").unwrap();
        fs::create_dir_all(&plan.directory).unwrap();
        let mut state = State {
            schema: 1,
            plan,
            policy: Plan::fingerprint(&config),
            epoch: "0123456789abcdef0123456789abcdef".into(),
            boot: boot().unwrap(),
            tools: Tools {
                ip: "/usr/bin/ip".into(),
                nft: "/usr/bin/nft".into(),
                xray: xray_executable(),
                sysctl: "/usr/bin/sysctl".into(),
            },
            namespace_inode: None,
            firewall: None,
        };
        create_kernel(&mut state, &config).unwrap();
        assert_eq!(
            routing(&state, true).unwrap(),
            vec![("-4", true, true), ("-6", true, true)]
        );
        let mut relay = Process(
            Command::new(&state.tools.xray)
                .args(["run", "-c"])
                .arg(state.plan.directory.join("xray.json"))
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap(),
        );
        thread::sleep(Duration::from_millis(350));
        if let Some(code) = relay.0.try_wait().unwrap() {
            let mut errors = String::new();
            relay
                .0
                .stderr
                .as_mut()
                .unwrap()
                .read_to_string(&mut errors)
                .unwrap();
            panic!("接管程序退出 / relay exited: {code}: {errors}");
        }
        assert_eq!(relay_process_at(&state, relay.0.id()).unwrap(), Some(true));
        let response = guest(&state.plan, TCP, &["198.18.0.100", "8080"]);
        assert!(
            response.status.success(),
            "{}",
            String::from_utf8_lossy(&response.stderr)
        );
        assert_eq!(response.stdout, b"via-fixed-tcp");
        let response = guest(&state.plan, TCP, &["2001:db8::100", "8080"]);
        assert!(
            response.status.success(),
            "{}",
            String::from_utf8_lossy(&response.stderr)
        );
        assert_eq!(response.stdout, b"via-fixed-tcp");
        for target in ["198.18.0.100", "2001:db8::100"] {
            if protocol == "socks5" {
                let response = guest(&state.plan, UDP, &[target]);
                assert!(
                    response.status.success(),
                    "{}",
                    String::from_utf8_lossy(&response.stderr)
                );
                assert_eq!(response.stdout, b"via-fixed-udp");
            } else {
                assert!(!guest(&state.plan, UDP, &[target]).status.success());
            }
        }
        for transport in ["udp", "tcp"] {
            for kind in ["1", "28", "16", "65"] {
                let response = guest(
                    &state.plan,
                    DNS,
                    &[&state.plan.gateway4.to_string(), kind, transport],
                );
                assert!(
                    response.status.success(),
                    "DNS {transport}/{kind}: {}",
                    String::from_utf8_lossy(&response.stderr)
                );
                assert!(!response.stdout.is_empty());
            }
        }
        assert!(
            requests
                .lock()
                .unwrap()
                .iter()
                .any(|(host, port)| host == "198.18.0.53" && *port == 53)
        );
        stopped.store(true, Ordering::SeqCst);
        proxy.join().unwrap();
        let unavailable = guest(&state.plan, TCP, &["198.18.0.100", "8080"]);
        assert!(unavailable.stdout.is_empty());
        relay.0.kill().unwrap();
        relay.0.wait().unwrap();
        assert!(
            !guest(&state.plan, TCP, &["198.18.0.100", "8080"])
                .status
                .success()
        );
        assert!(
            !guest(&state.plan, TCP, &["2001:db8::100", "8080"])
                .status
                .success()
        );
        assert!(sentinel.accept().is_err());
        // Foreign routes are retained, and failed cleanup closes the owned interface first.
        // 保留外来路由，清理失败时先关闭所属网卡。
        let table = state.plan.route_table.to_string();
        command(
            "ip",
            &[
                "route",
                "add",
                "blackhole",
                "203.0.113.0/24",
                "table",
                &table,
            ],
        );
        assert!(routing(&state, true).is_err());
        assert!(resources::destroy_with(&state, || Ok(())).is_err());
        assert!(firewall(&state).is_ok());
        let links = json(
            &state.tools.ip,
            &["-j", "link", "show", &state.plan.interface],
        )
        .unwrap();
        assert!(
            !links[0]["flags"]
                .as_array()
                .unwrap()
                .iter()
                .any(|v| v == "UP")
        );
        command(
            "ip",
            &[
                "route",
                "del",
                "blackhole",
                "203.0.113.0/24",
                "table",
                &table,
            ],
        );
        // A retained guest task also prevents removal of its namespace and protection.
        // 残留客体任务也会阻止删除其命名空间和防护。
        let mut remaining = Process(
            Command::new("ip")
                .args(["netns", "exec", &state.plan.namespace, "sleep", "10"])
                .spawn()
                .unwrap(),
        );
        thread::sleep(Duration::from_millis(100));
        assert!(resources::destroy_with(&state, || Ok(())).is_err());
        assert!(owned_namespace(&state).unwrap());
        assert!(firewall(&state).is_ok());
        remaining.0.kill().unwrap();
        remaining.0.wait().unwrap();
        resources::destroy_with(&state, || Ok(())).unwrap();
        assert_eq!(
            routing(&state, false).unwrap(),
            vec![("-4", false, false), ("-6", false, false)]
        );
        assert!(!owned_namespace(&state).unwrap());
        assert!(!owned_link(&state).unwrap());
    }
}
