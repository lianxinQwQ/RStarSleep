//! Owned kernel resources and services survive daemon restarts.
//! 所属内核资源和服务在 daemon 重启后保留。
use super::{NetworkConfig, Plan};
#[path = "resources.rs"]
mod resources;
use crate::{
    config::ConfigStore,
    subsystem::{
        self, Error, Result, commands,
        supervision::{Phase, Service, Supervisor, systemd::Systemd},
    },
};
use resources::{create_kernel, destroy, firewall, owned_link, owned_namespace, routing};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    ffi::OsString,
    fs::{self, OpenOptions},
    os::{
        fd::AsRawFd,
        unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    },
    path::{Path, PathBuf},
    process::{Command, Output},
    thread,
    time::{Duration, Instant},
};

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Tools {
    ip: PathBuf,
    nft: PathBuf,
    xray: PathBuf,
    sysctl: PathBuf,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct State {
    schema: u32,
    plan: Plan,
    policy: String,
    epoch: String,
    boot: String,
    tools: Tools,
    namespace_inode: Option<(u64, u64)>,
    firewall: Option<String>,
}

struct Lock(fs::File);
impl Drop for Lock {
    fn drop(&mut self) {
        unsafe {
            libc::flock(self.0.as_raw_fd(), libc::LOCK_UN);
        }
    }
}

fn lock() -> Result<Lock> {
    let directory = Path::new("/run/rstarsleep");
    subsystem::checked(directory)?;
    fs::create_dir_all(directory)?;
    subsystem::trust::path(directory)?;
    let path = directory.join("network.lock");
    subsystem::checked(&path)?;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?;
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(Lock(file))
}

fn tool(name: &str) -> Result<PathBuf> {
    for prefix in [
        "/usr/local/sbin",
        "/usr/local/bin",
        "/usr/sbin",
        "/usr/bin",
        "/sbin",
        "/bin",
    ] {
        let candidate = Path::new(prefix).join(name);
        match fs::canonicalize(candidate) {
            Ok(path) => {
                subsystem::trust::path(&path)?;
                let meta = fs::metadata(&path)?;
                if meta.is_file() && meta.mode() & 0o111 != 0 {
                    return Ok(path);
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    Err(Error::Invalid(format!(
        "缺少网络工具 {name} / missing network tool {name}"
    )))
}

fn run(program: &Path, args: &[&str]) -> Result<Output> {
    let mut command = Command::new(program);
    command.args(args);
    commands::output(command, Duration::from_secs(15))
}

fn checked(program: &Path, args: &[&str]) -> Result<()> {
    if !run(program, args)?.status.success() {
        // External diagnostics can contain credentials or user-controlled input.
        // 外部诊断可能包含凭据或用户控制的输入。
        return Err(Error::Invalid(format!(
            "网络操作失败 ({}) / network operation failed ({})",
            program.display(),
            program.display()
        )));
    }
    Ok(())
}

/// Identify parser failures; never expose arbitrary parser output when credentials are present.
/// 标明解析器失败；含凭据时绝不披露任意解析器输出。
fn check_xray(program: &Path, path: &Path, config: &NetworkConfig) -> Result<()> {
    let output = run(program, &["run", "-test", "-c", path.to_str().unwrap()])?;
    if output.status.success() {
        return Ok(());
    }
    Err(xray_check_error(program, &output, config))
}

fn xray_check_error(program: &Path, output: &Output, config: &NetworkConfig) -> Error {
    let detail = if config.proxy.username.is_some() || config.proxy.password.is_some() {
        "外部诊断已隐藏以保护代理凭据 / external diagnostics hidden to protect proxy credentials"
            .into()
    } else {
        [output.stdout.as_slice(), output.stderr.as_slice()]
            .into_iter()
            .map(|bytes| String::from_utf8_lossy(bytes))
            .collect::<Vec<_>>()
            .join("\n")
            .chars()
            .filter(|c| !c.is_control() || matches!(c, '\n' | '\t'))
            .take(4096)
            .collect::<String>()
            .trim()
            .to_string()
    };
    Error::Invalid(format!(
        "Xray 配置预检失败 / Xray configuration preflight failed ({}, {}): {detail}",
        program.display(),
        output.status
    ))
}

fn json(program: &Path, args: &[&str]) -> Result<Value> {
    let output = run(program, args)?;
    if !output.status.success() {
        return Err(Error::Invalid(
            "无法查询网络资源 / cannot query network resources".into(),
        ));
    }
    serde_json::from_slice(&output.stdout)
        .map_err(|_| Error::Invalid("网络资源响应无效 / invalid network resource response".into()))
}

/// iproute2 encodes numeric table identifiers as either strings or numbers.
/// iproute2 将数值路由表标识编码为字符串或数字。
fn table_id(value: &Value) -> Option<u32> {
    value
        .as_u64()
        .and_then(|v| v.try_into().ok())
        .or_else(|| value.as_str()?.parse().ok())
}

fn boot() -> Result<String> {
    Ok(fs::read_to_string("/proc/sys/kernel/random/boot_id")?
        .trim()
        .into())
}
fn state_path(plan: &Plan) -> PathBuf {
    plan.directory.join("state.toml")
}
fn save(state: &State) -> Result<()> {
    subsystem::write_record(&state_path(&state.plan), state)
}

fn load(plan: &Plan) -> Result<Option<State>> {
    subsystem::checked(&state_path(plan))?;
    match fs::read_to_string(state_path(plan)) {
        Ok(text) => {
            subsystem::trust::path(&state_path(plan))?;
            let state: State = toml::from_str(&text)
                .map_err(|_| Error::Invalid("网络记录损坏 / corrupt network record".into()))?;
            if state.schema != 1
                || !state.plan.same_owner(plan)
                || state.epoch.len() != 32
                || !state.epoch.bytes().all(|b| b.is_ascii_hexdigit())
            {
                return Err(Error::Invalid(
                    "网络记录身份不匹配 / network record identity mismatch".into(),
                ));
            }
            Ok(Some(state))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Check tools and syntax without contacting the remote endpoint or changing networking.
/// 检查工具及语法，不连接远端端点或修改网络。
pub(crate) fn preflight(
    store: &ConfigStore,
    id: &str,
    config: Option<&NetworkConfig>,
) -> Result<()> {
    let Some(config) = config else {
        return Ok(());
    };
    config.validate()?;
    let status = fs::read_to_string("/proc/self/status")?;
    let capabilities = status
        .lines()
        .find_map(|line| line.strip_prefix("CapEff:\t"))
        .and_then(|v| u64::from_str_radix(v.trim(), 16).ok())
        .unwrap_or(0);
    let required = (1u64 << 21) | (1u64 << 12);
    if capabilities & required != required {
        return Err(Error::Invalid("代理网络需要 SYS_ADMIN 与 NET_ADMIN 能力 / proxy networking requires SYS_ADMIN and NET_ADMIN capabilities".into()));
    }
    for setting in ["all", "default"] {
        if fs::read_to_string(format!("/proc/sys/net/ipv6/conf/{setting}/disable_ipv6"))?.trim()
            != "0"
        {
            return Err(Error::Invalid(
                "固定代理网络需要启用 IPv6 / fixed proxy networking requires IPv6 enabled".into(),
            ));
        }
    }
    subsystem::checked(Path::new("/run/netns"))?;
    if Path::new("/run/netns").try_exists()? {
        subsystem::trust::path(Path::new("/run/netns"))?;
    }
    let plan = Plan::configured(store, id, config)?;
    let tools = Tools {
        ip: tool("ip")?,
        nft: tool("nft")?,
        xray: tool("xray")?,
        sysctl: tool("sysctl")?,
    };
    let directory = tempfile::Builder::new()
        .prefix("rstarsleep-network-check-")
        .tempdir()?;
    let xray = directory.path().join("xray.json");
    subsystem::atomic_write(
        &xray,
        &serde_json::to_vec(&plan.xray(config)).unwrap(),
        0o600,
    )?;
    check_xray(&tools.xray, &xray, config)?;
    let rules = directory.path().join("policy.nft");
    subsystem::atomic_write(
        &rules,
        plan.nft(config, "00000000000000000000000000000000")
            .as_bytes(),
        0o600,
    )?;
    checked(&tools.nft, &["--check", "--file", rules.to_str().unwrap()])?;
    let _lock = lock()?;
    match load(&plan)? {
        Some(state) if state.boot == boot()? => {}
        _ => vacant(&plan, &tools)?,
    }
    Ok(())
}

fn vacant(plan: &Plan, tools: &Tools) -> Result<()> {
    if plan.namespace_path().try_exists()?
        || json(&tools.ip, &["-j", "link", "show"])?
            .as_array()
            .is_none_or(|links| {
                links.iter().any(|l| {
                    l["ifname"]
                        .as_str()
                        .is_some_and(|n| n == plan.interface || n == plan.peer)
                })
            })
        || json(&tools.nft, &["-j", "list", "tables"])?["nftables"]
            .as_array()
            .is_none_or(|items| {
                items
                    .iter()
                    .any(|v| v["table"]["name"].as_str() == Some(&plan.table))
            })
    {
        return Err(Error::Invalid(
            "网络资源名称已占用 / network resource name already occupied".into(),
        ));
    }
    let status = Systemd.query(&plan.unit())?;
    if status.phase != Phase::Missing {
        return Err(Error::Invalid(
            "网络服务名称已占用 / network service name already occupied".into(),
        ));
    }
    for family in ["-4", "-6"] {
        let routes = json(
            &tools.ip,
            &["-j", "-N", family, "route", "show", "table", "all"],
        )?;
        if routes.as_array().is_none_or(|rows| {
            rows.iter().any(|r| {
                table_id(&r["table"]) == Some(plan.route_table)
                    || r["dst"].as_str().is_some_and(|v| overlaps(v, plan))
            })
        }) {
            return Err(Error::Invalid(
                "网络地址或路由表冲突 / network address or route table conflict".into(),
            ));
        }
        if json(&tools.ip, &["-j", "-N", family, "rule", "show"])?
            .as_array()
            .is_none_or(|rules| {
                rules
                    .iter()
                    .any(|r| table_id(&r["table"]) == Some(plan.route_table))
            })
        {
            return Err(Error::Invalid(
                "网络路由规则冲突 / network routing rule conflict".into(),
            ));
        }
    }
    Ok(())
}

fn overlaps(route: &str, plan: &Plan) -> bool {
    let (address, prefix) = route
        .split_once('/')
        .map_or((route, None), |(a, p)| (a, p.parse::<u32>().ok()));
    match address.parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V4(ip)) => {
            let prefix = prefix.unwrap_or(32);
            if prefix == 0 || prefix > 32 {
                return false;
            }
            let mask = u32::MAX << (32 - prefix.min(30));
            u32::from(ip) & mask == u32::from(plan.gateway4) & mask
        }
        Ok(std::net::IpAddr::V6(ip)) => {
            let prefix = prefix.unwrap_or(128);
            if prefix == 0 || prefix > 128 {
                return false;
            }
            let mask = u128::MAX << (128 - prefix.min(64));
            u128::from(ip) & mask == u128::from(plan.gateway6) & mask
        }
        _ => false,
    }
}

/// Freeze paths in nspawn arguments without requiring a guest network manager.
/// 在 nspawn 参数中固定路径，无需客体网络管理器。
pub(crate) fn boot_arguments(
    store: &ConfigStore,
    id: &str,
    config: Option<&NetworkConfig>,
    root: &Path,
) -> Result<Vec<OsString>> {
    let Some(config) = config else {
        return Ok(Vec::new());
    };
    let plan = Plan::configured(store, id, config)?;
    // Reuse the confined guest path resolver for symlinked resolv.conf files.
    // 对符号链接 resolv.conf 复用受限客体路径解析器。
    let target = subsystem::guest_path(root, Path::new("/etc/resolv.conf"))?;
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)?;
    }
    if !target.exists() {
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o644)
            .open(target)?;
    }
    Ok(vec![
        format!(
            "--network-namespace-path={}",
            plan.namespace_path().display()
        )
        .into(),
        "--drop-capability=CAP_NET_ADMIN,CAP_NET_RAW".into(),
        "--resolv-conf=off".into(),
        super::super::preparation::bind_argument(&subsystem::RuntimeMount {
            source: plan.directory.join("resolv.conf"),
            target: "/etc/resolv.conf".into(),
            read_only: true,
        })?,
    ])
}

/// Construct the isolated path before nspawn starts; upstream liveness is irrelevant.
/// 在 nspawn 启动前构造独立路径；上游在线状态不参与。
pub(crate) fn start(store: &ConfigStore, id: &str, config: Option<&NetworkConfig>) -> Result<()> {
    let Some(config) = config else {
        return Ok(());
    };
    let _lock = lock()?;
    let plan = Plan::configured(store, id, config)?;
    if let Some(state) = load(&plan)? {
        if state.boot == boot()? {
            return Err(Error::Invalid(
                "旧网络资源尚未清理 / previous network resources have not been cleaned".into(),
            ));
        }
    }
    let tools = Tools {
        ip: tool("ip")?,
        nft: tool("nft")?,
        xray: tool("xray")?,
        sysctl: tool("sysctl")?,
    };
    vacant(&plan, &tools)?;
    subsystem::checked(&plan.directory)?;
    fs::create_dir_all(&plan.directory)?;
    fs::set_permissions(&plan.directory, fs::Permissions::from_mode(0o700))?;
    subsystem::trust::path(&plan.directory)?;
    let mut state = State {
        schema: 1,
        plan,
        policy: Plan::fingerprint(config),
        epoch: format!("{:032x}", rand::random::<u128>()),
        boot: boot()?,
        tools,
        namespace_inode: None,
        firewall: None,
    };
    save(&state)?;
    let result = create(&mut state, config);
    if let Err(error) = result {
        // An incomplete owned path remains blocked until cleanup is verified safe.
        // 未完成的所属路径保持阻断，直到确认可安全清理。
        if let Err(cleanup) = destroy(&state) {
            return Err(Error::Invalid(format!(
                "{error}; 网络清理保留资源 / network cleanup retained resources: {cleanup}"
            )));
        }
        fs::remove_file(state_path(&state.plan))?;
        return Err(error);
    }
    Ok(())
}

fn create(state: &mut State, config: &NetworkConfig) -> Result<()> {
    create_kernel(state, config)?;
    start_relay(state)
}

fn start_relay(state: &State) -> Result<()> {
    let p = &state.plan;
    Systemd.start(&Service {
        name: p.unit(),
        identity: p.identity(&state.epoch),
        executable: state.tools.xray.clone(),
        restart_on_failure: true,
        arguments: vec![
            "run".into(),
            "-c".into(),
            p.directory.join("xray.json").into_os_string(),
        ],
    })?;
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let service = Systemd.query(&p.unit())?;
        service.verify(&p.identity(&state.epoch))?;
        if !service.live() {
            return Err(Error::Invalid(
                "本地代理服务已退出 / local proxy service exited".into(),
            ));
        }
        if listening(state)? {
            break;
        }
        if Instant::now() >= deadline {
            return Err(Error::Invalid(
                "本地代理监听未就绪 / local proxy listeners not ready".into(),
            ));
        }
        thread::sleep(Duration::from_millis(50));
    }
    Ok(())
}

fn listening(state: &State) -> Result<bool> {
    let mut command = Command::new(commands::SYSTEMCTL);
    command.args(["show", "--property=MainPID", "--value", &state.plan.unit()]);
    let output = commands::output(command, Duration::from_secs(5))?;
    let pid = String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse::<u32>()
        .unwrap_or(0);
    if !output.status.success() || pid == 0 {
        return Ok(false);
    }
    let mut sockets = BTreeSet::new();
    for fd in fs::read_dir(format!("/proc/{pid}/fd"))? {
        let fd = fd?;
        if let Ok(target) = fs::read_link(fd.path()) {
            sockets.insert(target.to_string_lossy().into_owned());
        }
    }
    let mut count = 0;
    for name in ["tcp", "tcp6", "udp", "udp6"] {
        for line in fs::read_to_string(format!("/proc/{pid}/net/{name}"))?
            .lines()
            .skip(1)
        {
            let fields: Vec<_> = line.split_whitespace().collect();
            if fields.len() < 10 || !sockets.contains(&format!("socket:[{}]", fields[9])) {
                continue;
            }
            let port = fields[1].rsplit(':').next().unwrap_or("");
            if matches!(port, "0439" | "041D") && (name.starts_with("udp") || fields[3] == "0A") {
                count += 1;
            }
        }
    }
    Ok(count == 8)
}

/// Verify the frozen owned path; recovery never applies newly edited policy.
/// 核验已冻结的所属路径；恢复不应用新编辑的策略。
pub(crate) fn verify(
    store: &ConfigStore,
    id: &str,
    config: Option<&NetworkConfig>,
    leader: Option<u32>,
) -> Result<()> {
    let Some(config) = config else {
        return Ok(());
    };
    let _lock = lock()?;
    let plan = Plan::configured(store, id, config)?;
    let state = load(&plan)?.ok_or_else(|| {
        Error::Invalid("运行中代理网络记录缺失 / running proxy network record missing".into())
    })?;
    let result = verify_state(&state, &plan, config, leader);
    if let Err(error) = result {
        // Only an authenticated link may be changed; retain every other resource on drift.
        // 仅可修改已核实归属的网卡；漂移时保留其余全部资源。
        match owned_link(&state) {
            Ok(true) => {
                checked(
                    &state.tools.ip,
                    &["link", "set", &state.plan.interface, "down"],
                )?;
            }
            Ok(false) => {}
            Err(_) => {
                return Err(Error::Invalid(format!(
                    "{error}; 无法确认网卡归属并保证阻断 / cannot authenticate interface ownership or guarantee blocking"
                )));
            }
        }
        return Err(error);
    }
    Ok(())
}

fn verify_state(
    state: &State,
    plan: &Plan,
    config: &NetworkConfig,
    leader: Option<u32>,
) -> Result<()> {
    if state.plan != *plan
        || state.boot != boot()?
        || state.policy != Plan::fingerprint(config)
        || !owned_namespace(&state)?
        || !owned_link(&state)?
        || state.firewall.as_deref() != Some(&firewall(&state)?)
    {
        return Err(Error::Invalid(
            "运行中代理网络不一致 / running proxy network inconsistent".into(),
        ));
    }
    routing(state, true)?;
    let path = plan.directory.join("xray.json");
    subsystem::trust::path(&path)?;
    if fs::read(path)? != serde_json::to_vec(&plan.xray(config)).unwrap() {
        return Err(Error::Invalid(
            "代理配置文件不一致 / proxy configuration file inconsistent".into(),
        ));
    }
    if let Some(pid) = leader {
        let meta = fs::metadata(format!("/proc/{pid}/ns/net"))?;
        if state.namespace_inode != Some((meta.dev(), meta.ino())) {
            return Err(Error::Invalid(
                "客体网络命名空间不匹配 / guest network namespace mismatch".into(),
            ));
        }
    }
    // Relay failure keeps the path blocked; do not turn a remote failure into a lifecycle decision.
    // 接管程序故障仍保持路径阻断；不把远端失败变为生命周期决策。
    let service = Systemd.query(&plan.unit())?;
    service.verify(&plan.identity(&state.epoch))?;
    if service.live() && relay_process(state)? == Some(false) {
        return Err(Error::Invalid(
            "本地代理服务命令不匹配 / local proxy service command mismatch".into(),
        ));
    }
    Ok(())
}

/// Verify the running executable and literal argv, without logging configuration contents.
/// 核验运行程序及字面参数，不记录配置内容。
fn relay_process(state: &State) -> Result<Option<bool>> {
    let output = run(
        Path::new(commands::SYSTEMCTL),
        &["show", "--property=MainPID", "--value", &state.plan.unit()],
    )?;
    let pid = String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse::<u32>()
        .unwrap_or(0);
    if !output.status.success() || pid == 0 {
        return Ok(None);
    }
    relay_process_at(state, pid)
}

fn relay_process_at(state: &State, pid: u32) -> Result<Option<bool>> {
    let arguments = [
        state.tools.xray.as_os_str(),
        std::ffi::OsStr::new("run"),
        std::ffi::OsStr::new("-c"),
        state.plan.directory.join("xray.json").as_os_str(),
    ]
    .iter()
    .flat_map(|arg| {
        arg.as_encoded_bytes()
            .iter()
            .copied()
            .chain(std::iter::once(0))
    })
    .collect::<Vec<_>>();
    match (
        fs::read_link(format!("/proc/{pid}/exe")),
        fs::read(format!("/proc/{pid}/cmdline")),
    ) {
        (Ok(executable), Ok(actual)) => {
            Ok(Some(executable == state.tools.xray && actual == arguments))
        }
        (Err(e), _) | (_, Err(e)) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        (Err(e), _) | (_, Err(e)) => Err(e.into()),
    }
}

/// Remove only owned, unused resources, after the recorder and guest have drained.
/// 在记录器和客体排空后，仅移除所属且未使用的资源。
pub(crate) fn cleanup(store: &ConfigStore, id: &str) -> Result<()> {
    let plan = Plan::new(store, id)?;
    subsystem::checked(&state_path(&plan))?;
    if !state_path(&plan).try_exists()? {
        return Ok(());
    }
    let _lock = lock()?;
    let Some(state) = load(&plan)? else {
        return Ok(());
    };
    if state.boot == boot()? {
        destroy(&state)?;
    }
    fs::remove_file(state_path(&plan))?;
    for name in ["xray.json", "policy.nft", "resolv.conf"] {
        let path = plan.directory.join(name);
        subsystem::checked(&path)?;
        if path.try_exists()? {
            fs::remove_file(path)?;
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
