//! Runtime values independent of distribution package managers.
//! 独立于发行版包管理器的运行数据。
use super::SubsystemConfig;
use serde::{Deserialize, Serialize};
use std::{ffi::OsString, io, path::PathBuf};

/// Subsystem operation result.
/// 子系统操作结果。
pub type Result<T> = std::result::Result<T, Error>;

/// Context retained across storage, protocol and application failures.
/// 保存存储、协议与应用错误的上下文。
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Config(#[from] crate::config::Error),
    #[error(transparent)]
    Controller(#[from] crate::controller::Error),
    #[error("无法访问 {path} / cannot access {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("子系统无效 / invalid subsystem: {0}")]
    Invalid(String),
    #[error("子系统协议错误 / subsystem protocol error: {0}")]
    Protocol(String),
    #[error("应用退出码 {0} / application exit code {0}")]
    Exit(i32),
}

impl From<io::Error> for Error {
    /// Attach unspecified I/O failures to the runtime boundary.
    /// 为未指定路径的 I/O 错误附加运行边界。
    /// # Arguments
    /// - `source`: Original operating-system error. 原始系统错误。
    fn from(source: io::Error) -> Self {
        Self::Io {
            path: "<subsystem>".into(),
            source,
        }
    }
}

/// A prepared writable root and its frozen launch arguments.
/// 已准备的可写根及其冻结启动参数。
#[derive(Debug, Clone)]
pub struct PreparedRuntime {
    pub system_id: String,
    pub version: String,
    pub root: PathBuf,
    pub machine: String,
    pub config: SubsystemConfig,
    pub boot_args: Vec<OsString>,
}

/// Durable references to one owned runtime generation.
/// 指向一个所属运行代的持久引用。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeRecord {
    pub schema_version: u32,
    pub system_id: String,
    pub version: String,
    pub generation: String,
    pub config_sha256: String,
    pub machine: String,
}
