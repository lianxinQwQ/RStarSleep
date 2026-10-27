//! Process supervision independent of the service manager implementation.
//! 独立于服务管理器实现的进程监督。
use super::{Error, Result};
use std::{ffi::OsString, path::PathBuf};

pub(crate) mod systemd;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Phase {
    Missing,
    Inactive,
    Starting,
    Running,
    Stopping,
    Failed,
}

#[derive(Debug, Clone)]
pub(crate) struct Status {
    pub phase: Phase,
    pub identity: String,
    pub invocation: String,
    pub result: String,
    pub exit_status: String,
}

impl Status {
    /// Retain ownership throughout activation and output draining.
    /// 在激活和输出排空期间保留所有权。
    pub fn live(&self) -> bool {
        matches!(
            self.phase,
            Phase::Starting | Phase::Running | Phase::Stopping
        )
    }

    /// Reject an occupied service belonging to another runtime.
    /// 拒绝属于其他运行环境的已占用服务。
    pub fn verify(&self, identity: &str) -> Result<()> {
        if self.live() && self.identity != identity {
            return Err(Error::Invalid(
                "控制台服务身份不匹配 / console service identity mismatch".into(),
            ));
        }
        Ok(())
    }
}

pub(crate) struct Service {
    pub name: String,
    pub identity: String,
    pub executable: PathBuf,
    pub arguments: Vec<OsString>,
}

pub(crate) trait Supervisor: Send + Sync {
    /// Query one deterministic service without interpreting absence as a probe failure.
    /// 查询确定服务，不把服务不存在混同为探测失败。
    fn query(&self, name: &str) -> Result<Status>;

    /// Start once; the backend must atomically reject an occupied service name.
    /// 启动一次；后端必须原子拒绝已占用的服务名。
    fn start(&self, service: &Service) -> Result<()>;
}
