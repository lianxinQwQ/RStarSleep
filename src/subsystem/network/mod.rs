//! Isolated networking with one fixed upstream and no direct fallback.
//! 独立网络使用唯一固定上游且不直连回退。
mod config;
mod plan;
mod runtime;
pub use config::{DnsConfig, NetworkConfig, ProxyConfig};
pub(crate) use plan::Plan;
pub(crate) use runtime::{boot_arguments, cleanup, preflight, start, verify};
