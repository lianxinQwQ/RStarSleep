//! Kernel path creation and conservative ownership-checked cleanup.
//! 内核路径创建及保守的所有权校验清理。
use super::*;

pub(super) fn create_kernel(state: &mut State, config: &NetworkConfig) -> Result<()> {
    let p = &state.plan;
    let xray = p.directory.join("xray.json");
    let policy = p.directory.join("policy.nft");
    subsystem::atomic_write(&xray, &serde_json::to_vec(&p.xray(config)).unwrap(), 0o600)?;
    subsystem::atomic_write(&policy, p.nft(config, &state.epoch).as_bytes(), 0o600)?;
    subsystem::atomic_write(
        &p.directory.join("resolv.conf"),
        format!(
            "# 固定代理 DNS / Fixed proxy DNS\nnameserver {}\noptions timeout:2 attempts:2\n",
            p.gateway4
        )
        .as_bytes(),
        0o644,
    )?;
    checked(&state.tools.ip, &["netns", "add", &p.namespace])?;
    let meta = fs::metadata(p.namespace_path())?;
    state.namespace_inode = Some((meta.dev(), meta.ino()));
    save(state)?;
    checked(
        &state.tools.ip,
        &[
            "link",
            "add",
            &p.interface,
            "type",
            "veth",
            "peer",
            "name",
            &p.peer,
        ],
    )?;
    checked(
        &state.tools.ip,
        &[
            "link",
            "set",
            "dev",
            &p.interface,
            "alias",
            &p.alias(&state.epoch),
        ],
    )?;
    checked(
        &state.tools.ip,
        &["link", "set", "dev", &p.peer, "netns", &p.namespace],
    )?;
    checked(
        &state.tools.ip,
        &["-n", &p.namespace, "link", "set", &p.peer, "name", "host0"],
    )?;
    checked(
        &state.tools.ip,
        &[
            "-n",
            &p.namespace,
            "link",
            "set",
            "host0",
            "alias",
            &p.alias(&state.epoch),
        ],
    )?;
    for (address, family, interface, ns) in [
        (
            format!("{}/30", p.gateway4),
            "-4",
            p.interface.as_str(),
            false,
        ),
        (
            format!("{}/64", p.gateway6),
            "-6",
            p.interface.as_str(),
            false,
        ),
        (format!("{}/30", p.guest4), "-4", "host0", true),
        (format!("{}/64", p.guest6), "-6", "host0", true),
    ] {
        let mut args = vec![];
        if ns {
            args.extend(["-n", &p.namespace]);
        }
        args.extend([family, "address", "add", &address, "dev", interface]);
        if family == "-6" {
            args.push("nodad");
        }
        checked(&state.tools.ip, &args)?;
    }
    for (family, setting, value) in [
        ("ipv4", "rp_filter", 2),
        ("ipv4", "accept_redirects", 0),
        ("ipv4", "send_redirects", 0),
        ("ipv4", "accept_source_route", 0),
        ("ipv6", "accept_ra", 0),
        ("ipv6", "autoconf", 0),
        ("ipv6", "accept_redirects", 0),
        ("ipv6", "accept_source_route", 0),
    ] {
        checked(
            &state.tools.sysctl,
            &[
                "-q",
                "-w",
                &format!("net.{family}.conf.{}.{setting}={value}", p.interface),
            ],
        )?;
    }
    checked(&state.tools.nft, &["--file", policy.to_str().unwrap()])?;
    state.firewall = Some(firewall(state)?);
    save(state)?;
    let table = p.route_table.to_string();
    for family in ["-4", "-6"] {
        checked(
            &state.tools.ip,
            &[
                family, "route", "add", "local", "default", "dev", "lo", "table", &table,
            ],
        )?;
        checked(
            &state.tools.ip,
            &[
                family,
                "rule",
                "add",
                "priority",
                "100",
                "iif",
                &p.interface,
                "lookup",
                &table,
            ],
        )?;
    }
    checked(&state.tools.ip, &["link", "set", &p.interface, "up"])?;
    checked(
        &state.tools.ip,
        &["-n", &p.namespace, "link", "set", "lo", "up"],
    )?;
    checked(
        &state.tools.ip,
        &["-n", &p.namespace, "link", "set", "host0", "up"],
    )?;
    for (family, gateway) in [
        ("-4", p.gateway4.to_string()),
        ("-6", p.gateway6.to_string()),
    ] {
        checked(
            &state.tools.ip,
            &[
                "-n",
                &p.namespace,
                family,
                "route",
                "add",
                "default",
                "via",
                &gateway,
                "dev",
                "host0",
            ],
        )?;
    }
    Ok(())
}

pub(super) fn firewall(state: &State) -> Result<String> {
    let mut value = json(
        &state.tools.nft,
        &["-j", "list", "table", "inet", &state.plan.table],
    )?;
    if !value["nftables"].as_array().is_some_and(|items| {
        items
            .iter()
            .any(|v| v["table"]["comment"].as_str() == Some(&state.plan.alias(&state.epoch)))
    }) {
        return Err(Error::Invalid(
            "防火墙所有权不匹配 / firewall ownership mismatch".into(),
        ));
    }
    fn normalize(value: &mut Value) {
        match value {
            Value::Object(map) => {
                for key in ["handle", "metainfo", "packets", "bytes"] {
                    map.remove(key);
                }
                for value in map.values_mut() {
                    normalize(value);
                }
            }
            Value::Array(items) => {
                for value in items {
                    normalize(value);
                }
            }
            _ => {}
        }
    }
    normalize(&mut value);
    Ok(format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&value).unwrap())
    ))
}

pub(super) fn owned_namespace(state: &State) -> Result<bool> {
    match fs::metadata(state.plan.namespace_path()) {
        Ok(meta) if state.namespace_inode == Some((meta.dev(), meta.ino())) => Ok(true),
        Ok(_) => Err(Error::Invalid(
            "网络命名空间身份不匹配 / network namespace identity mismatch".into(),
        )),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e.into()),
    }
}

pub(super) fn owned_link(state: &State) -> Result<bool> {
    let links = json(&state.tools.ip, &["-j", "link", "show"])?;
    let link = links.as_array().and_then(|links| {
        links
            .iter()
            .find(|l| l["ifname"].as_str() == Some(&state.plan.interface))
    });
    match link {
        None => Ok(false),
        Some(link) if link["ifalias"].as_str() == Some(&state.plan.alias(&state.epoch)) => Ok(true),
        _ => Err(Error::Invalid(
            "虚拟网卡所有权不匹配 / virtual interface ownership mismatch".into(),
        )),
    }
}

/// Inspect both families before deleting anything; partial creation permits missing entries.
/// 在删除任何资源前检查两个地址族；未完成创建允许条目缺失。
pub(super) fn routing(state: &State, complete: bool) -> Result<Vec<(&'static str, bool, bool)>> {
    let p = &state.plan;
    let mut found = Vec::new();
    for family in ["-4", "-6"] {
        let rules = json(&state.tools.ip, &["-j", "-N", family, "rule", "show"])?;
        let routes = json(
            &state.tools.ip,
            &["-j", "-N", family, "route", "show", "table", "all"],
        )?;
        let valid = rules
            .as_array()
            .zip(routes.as_array())
            .and_then(|(rules, routes)| {
                let rules: Vec<_> = rules
                    .iter()
                    .filter(|r| {
                        table_id(&r["table"]) == Some(p.route_table)
                            || r["iif"].as_str() == Some(&p.interface)
                            || r["oif"].as_str() == Some(&p.interface)
                    })
                    .collect();
                let routes: Vec<_> = routes
                    .iter()
                    .filter(|r| table_id(&r["table"]) == Some(p.route_table))
                    .collect();
                let rule = rules.len() <= 1
                    && rules.iter().all(|r| {
                        table_id(&r["table"]) == Some(p.route_table)
                            && r["priority"].as_u64() == Some(100)
                            && r["src"].as_str() == Some("all")
                            && r["iif"].as_str() == Some(&p.interface)
                            && r.as_object().is_some_and(|m| {
                                m.keys().all(|k| {
                                    matches!(
                                        k.as_str(),
                                        "table" | "priority" | "src" | "iif" | "protocol"
                                    )
                                })
                            })
                    });
                let route = routes.len() <= 1
                    && routes.iter().all(|r| {
                        matches!(r["type"].as_str(), Some("local" | "2"))
                            && r["dst"].as_str() == Some("default")
                            && r["dev"].as_str() == Some("lo")
                            && r.as_object().is_some_and(|m| {
                                m.keys().all(|k| {
                                    matches!(
                                        k.as_str(),
                                        "table"
                                            | "type"
                                            | "dst"
                                            | "dev"
                                            | "scope"
                                            | "protocol"
                                            | "flags"
                                            | "pref"
                                            | "metric"
                                    )
                                })
                            })
                            && r["metric"]
                                .as_u64()
                                .is_none_or(|v| v == if family == "-6" { 1024 } else { 0 })
                            && r["flags"].as_array().is_none_or(Vec::is_empty)
                    });
                (rule && route && (!complete || (rules.len() == 1 && routes.len() == 1)))
                    .then_some((family, !rules.is_empty(), !routes.is_empty()))
            });
        found.push(valid.ok_or_else(|| {
            Error::Invalid(
                "代理路由规则或专用表不一致 / proxy routing rules or dedicated table inconsistent"
                    .into(),
            )
        })?);
    }
    Ok(found)
}

/// Process leaders are insufficient: any thread can retain a different network namespace.
/// 仅检查进程首线程不足：任意线程都可能保留不同的网络命名空间。
fn namespace_unused(state: &State) -> Result<()> {
    for process in fs::read_dir("/proc")? {
        let process = process?;
        if process
            .file_name()
            .to_str()
            .is_none_or(|name| !name.bytes().all(|c| c.is_ascii_digit()))
        {
            continue;
        }
        let tasks = match fs::read_dir(process.path().join("task")) {
            Ok(tasks) => tasks,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e.into()),
        };
        for task in tasks {
            let task = task?;
            match fs::metadata(task.path().join("ns/net")) {
                Ok(meta) if state.namespace_inode == Some((meta.dev(), meta.ino())) => {
                    return Err(Error::Invalid("网络命名空间仍有线程；资源保留 / network namespace still has threads; resources retained".into()));
                }
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
        }
    }
    Ok(())
}

pub(super) fn destroy(state: &State) -> Result<()> {
    destroy_with(state, || {
        let service = Systemd.query(&state.plan.unit())?;
        service.verify(&state.plan.identity(&state.epoch))?;
        if service.live() {
            checked(
                Path::new(commands::SYSTEMCTL),
                &["stop", &state.plan.unit()],
            )?;
        }
        if service.phase == Phase::Failed {
            checked(
                Path::new(commands::SYSTEMCTL),
                &["reset-failed", &state.plan.unit()],
            )?;
        }
        // A previous epoch must disappear before another transient unit can use its name.
        // 旧 epoch 必须消失后，另一临时服务才能使用其名称。
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let current = Systemd.query(&state.plan.unit())?;
            current.verify(&state.plan.identity(&state.epoch))?;
            if current.phase == Phase::Missing {
                break;
            }
            if Instant::now() >= deadline {
                return Err(Error::Invalid("网络服务未被回收；防护保留 / network service not collected; protection retained".into()));
            }
            thread::sleep(Duration::from_millis(50));
        }
        Ok(())
    })
}

pub(super) fn destroy_with(state: &State, stop_relay: impl FnOnce() -> Result<()>) -> Result<()> {
    let p = &state.plan;
    let link = owned_link(state)?;
    if link {
        checked(&state.tools.ip, &["link", "set", &p.interface, "down"])?;
    }
    let namespace = owned_namespace(state)?;
    if namespace {
        namespace_unused(state)?;
    }
    let routing = routing(state, false)?;
    stop_relay()?;
    let tables = json(&state.tools.nft, &["-j", "list", "tables"])?;
    let exists = tables["nftables"].as_array().is_some_and(|v| {
        v.iter()
            .any(|v| v["table"]["name"].as_str() == Some(&p.table))
    });
    if exists {
        let _fingerprint = firewall(state)?;
        // Keep the block until the guest link is down and the service has stopped.
        // 保持阻断，直到客体链路已关闭且服务已停止。
        checked(&state.tools.nft, &["delete", "table", "inet", &p.table])?;
    }
    for (family, rule, route) in routing {
        if rule {
            checked(
                &state.tools.ip,
                &[
                    family,
                    "rule",
                    "del",
                    "priority",
                    "100",
                    "iif",
                    &p.interface,
                    "lookup",
                    &p.route_table.to_string(),
                ],
            )?;
        }
        if route {
            checked(
                &state.tools.ip,
                &[
                    family,
                    "route",
                    "del",
                    "local",
                    "default",
                    "dev",
                    "lo",
                    "table",
                    &p.route_table.to_string(),
                ],
            )?;
        }
    }
    if link {
        checked(&state.tools.ip, &["link", "delete", &p.interface])?;
    }
    if namespace {
        checked(&state.tools.ip, &["netns", "delete", &p.namespace])?;
    }
    Ok(())
}
