//! Deterministic topology and private Xray/nftables configuration.
//! 确定网络拓扑及私有 Xray/nftables 配置。
use super::NetworkConfig;
use crate::{
    config::ConfigStore,
    subsystem::{Result, protocol},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    net::{Ipv4Addr, Ipv6Addr},
    path::PathBuf,
};

/// Stable identities are checked against kernel ownership before reuse.
/// 稳定身份在复用前与内核资源所有权核对。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Plan {
    pub owner: String,
    pub namespace: String,
    pub interface: String,
    pub peer: String,
    pub table: String,
    pub route_table: u32,
    pub gateway4: Ipv4Addr,
    pub guest4: Ipv4Addr,
    pub gateway6: Ipv6Addr,
    pub guest6: Ipv6Addr,
    pub directory: PathBuf,
}

impl Plan {
    /// Use canonical manager identity; never truncate an existing machine name.
    /// 使用规范管理器身份；不截断现有机器名。
    pub fn new(store: &ConfigStore, id: &str) -> Result<Self> {
        protocol::validate_id(id)?;
        let root = fs::canonicalize(store.root())?;
        let hash = Sha256::digest(
            [
                root.as_os_str().as_encoded_bytes(),
                b"\0network\0",
                id.as_bytes(),
            ]
            .concat(),
        );
        let owner = format!("{hash:x}");
        let addr = 0x0a80_0000u32
            | ((u32::from_be_bytes(hash[..4].try_into().unwrap()) & 0x001f_ffff) << 2);
        let mut v6 = [0u8; 16];
        v6[0] = 0xfd;
        v6[1..8].copy_from_slice(&hash[4..11]);
        v6[15] = 1;
        let gateway6 = Ipv6Addr::from(v6);
        v6[15] = 2;
        Ok(Self {
            namespace: format!("rsn-{}", &owner[..24]),
            interface: format!("rs{}", &owner[..12]),
            peer: format!("rp{}", &owner[..12]),
            table: format!("rs_{}", &owner[..24]),
            route_table: 100_000
                + (u32::from_be_bytes(hash[12..16].try_into().unwrap()) & 0x00ff_ffff),
            gateway4: Ipv4Addr::from(addr + 1),
            guest4: Ipv4Addr::from(addr + 2),
            gateway6,
            guest6: Ipv6Addr::from(v6),
            directory: store.system_layout(id)?.root.join("runtime/network"),
            owner,
        })
    }

    pub fn namespace_path(&self) -> PathBuf {
        PathBuf::from("/run/netns").join(&self.namespace)
    }
    pub fn configured(store: &ConfigStore, id: &str, config: &NetworkConfig) -> Result<Self> {
        let mut plan = Self::new(store, id)?;
        let (v4, v6) = config.subnets()?;
        if let Some(ip) = v4 {
            plan.gateway4 = (u32::from(ip) + 1).into();
            plan.guest4 = (u32::from(ip) + 2).into();
        }
        if let Some(ip) = v6 {
            plan.gateway6 = (u128::from(ip) + 1).into();
            plan.guest6 = (u128::from(ip) + 2).into();
        }
        Ok(plan)
    }

    pub fn same_owner(&self, other: &Self) -> bool {
        let mut plan = self.clone();
        plan.gateway4 = other.gateway4;
        plan.guest4 = other.guest4;
        plan.gateway6 = other.gateway6;
        plan.guest6 = other.guest6;
        plan == *other
    }
    pub fn unit(&self) -> String {
        format!("rstarsleep-network-{}.service", self.owner)
    }
    pub fn identity(&self, epoch: &str) -> String {
        format!("RStarSleep network {} {epoch}", self.owner)
    }
    pub fn alias(&self, epoch: &str) -> String {
        format!("rstarsleep:{}:{epoch}", self.owner)
    }
    pub fn fingerprint(config: &NetworkConfig) -> String {
        format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(config).expect("serializable network policy"))
        )
    }

    /// All default and explicit routes end at the same fixed proxy; DNS is TCP-only upstream.
    /// 默认及显式路由均进入同一固定代理；DNS 上游只使用 TCP。
    pub fn xray(&self, config: &NetworkConfig) -> Value {
        self.xray_at(
            config,
            &self.gateway4.to_string(),
            &self.gateway6.to_string(),
            1081,
            1053,
        )
    }

    pub(super) fn xray_at(
        &self,
        config: &NetworkConfig,
        listen4: &str,
        listen6: &str,
        relay: u16,
        dns: u16,
    ) -> Value {
        let p = &config.proxy;
        let mut server = json!({"address": p.address, "port": p.port});
        if let (Some(user), Some(pass)) = (&p.username, &p.password) {
            server["users"] = json!([{"user": user, "pass": pass}]);
        }
        let mut inbounds = Vec::new();
        for (label, listen) in [("4", listen4), ("6", listen6)] {
            for (tag, port) in [("capture", relay), ("dns", dns)] {
                inbounds.push(json!({"tag": format!("{tag}{label}"), "listen": listen, "port": port,
                    "protocol": "dokodemo-door", "settings": {"network": "tcp,udp", "followRedirect": true},
                    "streamSettings": {"sockopt": {"tproxy": "tproxy"}}}));
            }
        }
        let dns_address = if config.dns.address.is_ipv6() {
            format!("[{}]", config.dns.address)
        } else {
            config.dns.address.to_string()
        };
        json!({
            "log": {"loglevel": "none", "access": "none", "dnsLog": false},
            "dns": {"tag": "dns-upstream", "disableCache": true, "disableFallback": true,
                "servers": [format!("tcp://{dns_address}:{}", config.dns.port)]},
            "inbounds": inbounds,
            "outbounds": [
                {"tag": "fixed", "protocol": if p.protocol == "socks5" {"socks"} else {"http"},
                    "settings": {"servers": [server]},
                    "streamSettings": {"sockopt": {"mark": config.outbound_mark}}},
                {"tag": "dns-forward", "protocol": "dns", "settings": {
                    "network": "tcp", "address": config.dns.address.to_string(), "port": config.dns.port,
                    "nonIPQuery": "skip"}, "proxySettings": {"tag": "fixed"}},
                {"tag": "blocked", "protocol": "blackhole"}
            ],
            "routing": {"domainStrategy": "AsIs", "rules": [
                {"type": "field", "inboundTag": ["dns4", "dns6"], "outboundTag": "dns-forward"},
                {"type": "field", "inboundTag": ["dns-upstream"], "outboundTag": "fixed"},
                {"type": "field", "network": if p.protocol == "http" {"tcp"} else {"tcp,udp"}, "outboundTag": "fixed"},
                {"type": "field", "network": "tcp,udp", "outboundTag": "blocked"}
            ]}
        })
    }

    /// TPROXY failure falls through to DROP; no listener never means direct forwarding.
    /// TPROXY 失败后落入 DROP；无监听者绝不意味着直接转发。
    pub fn nft(&self, config: &NetworkConfig, epoch: &str) -> String {
        let Self {
            interface: i,
            table: t,
            gateway4: g4,
            gateway6: g6,
            guest4: c4,
            guest6: c6,
            ..
        } = self;
        let udp = if config.proxy.protocol == "socks5" {
            "tcp, udp"
        } else {
            "tcp"
        };
        format!("table inet {t} {{
 comment \"{}\"
 chain capture {{
  type filter hook prerouting priority -151; policy accept;
  iifname != \"{i}\" return
  meta nfproto ipv6 icmpv6 type {{ nd-neighbor-solicit, nd-neighbor-advert }} ip6 hoplimit 255 accept
  meta nfproto ipv4 ip saddr != {c4} drop
  meta nfproto ipv6 ip6 saddr != {c6} drop
  ct state related meta l4proto {{ icmp, ipv6-icmp }} accept
  meta nfproto ipv4 meta l4proto {{ tcp, udp }} th dport 53 tproxy ip to {g4}:1053 accept
  meta nfproto ipv6 meta l4proto {{ tcp, udp }} th dport 53 tproxy ip6 to [{g6}]:1053 accept
  meta nfproto ipv4 meta l4proto {{ {udp} }} tproxy ip to {g4}:1081 accept
  meta nfproto ipv6 meta l4proto {{ {udp} }} tproxy ip6 to [{g6}]:1081 accept
  drop
 }}
 chain input {{
  type filter hook input priority -5; policy accept;
  iifname \"{i}\" meta l4proto {{ tcp, udp }} accept
  iifname \"{i}\" icmpv6 type {{ nd-neighbor-solicit, nd-neighbor-advert }} ip6 hoplimit 255 accept
  iifname \"{i}\" ct state related accept
  iifname \"{i}\" drop
 }}
 chain forward {{
  type filter hook forward priority -300; policy accept;
  iifname \"{i}\" drop
  oifname \"{i}\" drop
 }}
}}
", self.alias(epoch))
    }
}
