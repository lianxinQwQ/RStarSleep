//! Fixed proxy policy without remote availability checks.
//! 不检查远端可用性的固定代理策略。
use crate::subsystem::{Error, Result};
use serde::{Deserialize, Serialize};
use std::{
    fmt,
    net::{IpAddr, Ipv4Addr, Ipv6Addr},
};

/// An explicit proxy mode; absence of this section preserves host networking.
/// 显式代理模式；省略此节保留宿主网络。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkConfig {
    pub mode: String,
    pub proxy: ProxyConfig,
    pub dns: DnsConfig,
    /// Host SO_MARK reserved by the administrator for avoiding double capture.
    /// 管理员为避免二次接管而保留的宿主 SO_MARK。
    #[serde(default)]
    pub outbound_mark: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ipv4_subnet: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ipv6_subnet: Option<String>,
}

/// One endpoint with redacted diagnostics.
/// 诊断中隐藏凭据的唯一端点。
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProxyConfig {
    pub protocol: String,
    pub address: String,
    pub port: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
}

impl fmt::Debug for ProxyConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProxyConfig")
            .field("protocol", &self.protocol)
            .field("address", &self.address)
            .field("port", &self.port)
            .field("credentials", &self.username.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

/// Numeric TCP DNS upstream, always reached through the fixed proxy.
/// 始终经固定代理访问的数值 TCP DNS 上游。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DnsConfig {
    pub address: IpAddr,
    pub port: u16,
}

impl NetworkConfig {
    /// Validate local policy only; never resolve or connect to the endpoint.
    /// 仅校验本地策略；不解析或连接端点。
    pub fn validate(&self) -> Result<()> {
        self.subnets()?;
        let p = &self.proxy;
        if self.mode != "proxy" || !matches!(p.protocol.as_str(), "socks5" | "http") {
            return Err(Error::Invalid(
                "网络模式或代理协议无效 / invalid network mode or proxy protocol".into(),
            ));
        }
        let host = p.address.as_str();
        let hostname = host.len() <= 253
            && host.trim_end_matches('.').split('.').all(|label| {
                !label.is_empty()
                    && label.len() <= 63
                    && !label.starts_with('-')
                    && !label.ends_with('-')
                    && label
                        .bytes()
                        .all(|v| v.is_ascii_alphanumeric() || v == b'-')
            });
        if p.port == 0
            || self.dns.port == 0
            || host.is_empty()
            || (host.parse::<IpAddr>().is_err() && !hostname)
            || self.dns.address.is_unspecified()
            || self.dns.address.is_multicast()
            || p.address
                .parse::<IpAddr>()
                .is_ok_and(|ip| ip.is_unspecified() || ip.is_multicast())
        {
            return Err(Error::Invalid(
                "代理地址、DNS 地址或端口无效 / invalid proxy address, DNS address or port".into(),
            ));
        }
        match (&p.username, &p.password) {
            (None, None) => {}
            (Some(user), Some(pass))
                if !user.is_empty()
                    && !pass.is_empty()
                    && !user.chars().any(char::is_control)
                    && !pass.chars().any(char::is_control)
                    && (p.protocol != "socks5" || (user.len() <= 255 && pass.len() <= 255))
                    && (p.protocol != "http" || !user.contains(':')) => {}
            _ => {
                return Err(Error::Invalid(
                    "代理认证字段无效 / invalid proxy authentication fields".into(),
                ));
            }
        }
        Ok(())
    }

    /// Explicit address ranges resolve collisions without changing routing semantics.
    /// 显式地址范围用于解决冲突，不改变路由语义。
    pub(crate) fn subnets(&self) -> Result<(Option<Ipv4Addr>, Option<Ipv6Addr>)> {
        let v4 = self.ipv4_subnet.as_ref().map(|value| value.strip_suffix("/30")
            .and_then(|v| v.parse::<Ipv4Addr>().ok()).filter(|ip| ip.is_private() && u32::from(*ip) & 3 == 0)
            .ok_or_else(|| Error::Invalid("IPv4 子网必须为私有 /30 网络 / IPv4 subnet must be a private /30 network".into()))).transpose()?;
        let v6 = self
            .ipv6_subnet
            .as_ref()
            .map(|value| {
                value
                    .strip_suffix("/64")
                    .and_then(|v| v.parse::<Ipv6Addr>().ok())
                    .filter(|ip| ip.is_unique_local() && u128::from(*ip) as u64 == 0)
                    .ok_or_else(|| {
                        Error::Invalid(
                            "IPv6 子网必须为 ULA /64 网络 / IPv6 subnet must be a ULA /64 network"
                                .into(),
                        )
                    })
            })
            .transpose()?;
        Ok((v4, v6))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> NetworkConfig {
        NetworkConfig {
            mode: "proxy".into(),
            proxy: ProxyConfig {
                protocol: "socks5".into(),
                address: "127.0.0.1".into(),
                port: 1080,
                username: None,
                password: None,
            },
            dns: DnsConfig {
                address: "9.9.9.9".parse().unwrap(),
                port: 53,
            },
            outbound_mark: 0,
            ipv4_subnet: None,
            ipv6_subnet: None,
        }
    }

    /// Bad input fails locally; credential diagnostics never disclose values.
    /// 错误输入在本地失败；凭据诊断不披露值。
    #[test]
    fn validation_and_redaction() {
        let c = config();
        assert!(c.validate().is_ok());
        for address in ["::1", "proxy.example", "proxy.example."] {
            let mut c = c.clone();
            c.proxy.address = address.into();
            assert!(c.validate().is_ok());
        }
        for address in [
            "",
            "socks5://user:secret@host",
            "host/path",
            "a b",
            "-host",
            "host\n",
            "0.0.0.0",
        ] {
            let mut c = c.clone();
            c.proxy.address = address.into();
            assert!(c.validate().is_err());
        }
        let mut c = c;
        c.proxy.username = Some("secret-user".into());
        assert!(c.validate().is_err());
        c.proxy.password = Some("secret-password".into());
        assert!(c.validate().is_ok());
        assert!(!format!("{c:?}").contains("secret"));
        c.proxy.password = Some("密".repeat(86));
        assert!(c.validate().is_err());
        c.proxy.protocol = "http".into();
        assert!(c.validate().is_ok());
        c.proxy.username = Some("a:b".into());
        assert!(c.validate().is_err());
        c.proxy.port = 0;
        assert!(c.validate().is_err());
    }

    /// Explicit proxy sections never fall back when policy or capabilities are invalid.
    /// 显式代理配置在策略或能力无效时绝不回退。
    #[test]
    fn strict_sections_and_ranges() {
        let c = config();
        for mode in ["host", "auto", ""] {
            let mut c = c.clone();
            c.mode = mode.into();
            assert!(c.validate().is_err());
        }
        let mut c = c;
        c.proxy.protocol = "socks4".into();
        assert!(c.validate().is_err());
        c.proxy.protocol = "http".into();
        for range in ["10.1.0.1/30", "10.1.0.0/24", "8.8.8.0/30"] {
            c.ipv4_subnet = Some(range.into());
            assert!(c.validate().is_err());
        }
        c.ipv4_subnet = Some("10.1.0.0/30".into());
        c.ipv6_subnet = Some("fd12::/64".into());
        assert!(c.validate().is_ok());
        c.ipv6_subnet = Some("fd12::1/64".into());
        assert!(c.validate().is_err());
        assert!(
            toml::from_str::<crate::subsystem::SubsystemConfig>(
                "schema_version=1\n[network]\nmode='proxy'\n"
            )
            .is_err()
        );
        assert!(toml::from_str::<crate::subsystem::SubsystemConfig>("schema_version=1\n[network]\nmode='proxy'\n[network.proxy]\nprotocol='http'\naddress='localhost'\nport=70000\n").is_err());
    }
}
