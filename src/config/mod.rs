//! Workspace layout, Btrfs initialization, and package configuration access.
//! 工作目录布局、Btrfs 初始化和软件包配置访问接口。
//!
//! Global roles and subsystem runtime settings are placeholders for future modules.
//! 全局角色与子系统运行设置目前只保留模板，读取逻辑由后续模块实现。
#![doc = include_str!("layout.md")]

mod build;
mod inheritance;
mod ini;
mod init;
mod model;
mod parameters;
mod presets;
mod runtime;
mod store;

pub(crate) use build::normalized_build_config;
pub use build::{
    BuildConfig, read_build_config_at, read_distribution_options_at, read_plugin_parameters_at,
};
pub use inheritance::{InheritanceConfig, InheritanceTiming, inheritance_name};
pub use init::initialize;
pub use model::{PackageConfig, SystemTypeConfig};
pub use parameters::PluginParameters;
pub use runtime::RuntimeConfig;
pub use store::{ConfigStore, SystemLayout};

use std::path::PathBuf;

/// Dedicated workspace subvolume name beneath the Btrfs top level.
/// Btrfs 顶层下的专用工作目录子卷名称。
pub const SUBVOLUME_NAME: &str = "@RStarSleep";
/// Recommended runtime mount point; this module does not mount the subvolume.
/// 建议的运行时挂载点；本模块不会执行子卷挂载。
pub const DEFAULT_MOUNT_POINT: &str = "/rstarsleep";

/// Result type shared by configuration and initialization operations.
/// 配置与初始化操作共用的结果类型。
pub type Result<T> = std::result::Result<T, Error>;

/// Configuration failures with filesystem paths or validation context.
/// 包含文件系统路径或校验上下文的配置操作错误。
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A filesystem operation failed.
    /// 文件系统操作失败。
    #[error("无法访问 {path}: {source}")]
    Io {
        /// Path involved in the failed operation.
        /// 失败操作涉及的路径。
        path: PathBuf,
        #[source]
        /// Original operating-system error.
        /// 原始操作系统错误。
        source: std::io::Error,
    },
    /// A configuration file is not valid TOML or has incompatible field types.
    /// 配置文件不是有效的 TOML，或字段类型不兼容。
    #[error("无法解析 {path}: {source}")]
    Parse {
        /// Configuration file that could not be parsed.
        /// 无法解析的配置文件路径。
        path: PathBuf,
        #[source]
        /// Original TOML deserialization error.
        /// 原始 TOML 反序列化错误。
        source: toml::de::Error,
    },
    /// An INI build configuration has invalid syntax or a missing or invalid value.
    /// INI 构建配置的语法错误，或值缺失、无效。
    #[error("无法解析 {path}:{line}: {message}")]
    Ini {
        /// Source configuration path.
        /// 源配置路径。
        path: PathBuf,
        /// One-based line number; zero denotes a missing setting.
        /// 从 1 开始的行号；0 表示缺失设置。
        line: usize,
        /// Syntax or value diagnostic.
        /// 语法或值的诊断。
        message: String,
    },
    /// A path, identifier, prerequisite, or package value failed validation.
    /// 路径、标识、前置条件或软件包值未通过校验。
    #[error("配置无效: {0}")]
    Invalid(String),
    /// A Btrfs command could not run, failed, or returned an unexpected tree ID.
    /// Btrfs 命令无法启动、执行失败，或返回了不符合要求的树标识。
    #[error("Btrfs 操作失败: {0}")]
    Btrfs(String),
}

/// Attach the affected path without discarding the original I/O error.
/// 在保留原始 I/O 错误的同时附加相关路径。
///
/// # Arguments
///
/// - `path`: Path involved in the failed filesystem operation.
///   文件系统操作失败时涉及的路径。
/// - `source`: Original I/O error to retain as the error source.
///   需要保留为错误来源的原始 I/O 错误。
fn io_error(path: impl Into<PathBuf>, source: std::io::Error) -> Error {
    Error::Io {
        path: path.into(),
        source,
    }
}

/// Accept a single ASCII path component starting with a letter or digit.
/// 接受以字母或数字开头的单个 ASCII 路径分量。
///
/// Remaining characters may be letters, digits, dots, underscores, or hyphens.
/// 后续字符可以是字母、数字、点、下划线或连字符。
///
/// # Arguments
///
/// - `id`: System, version, or stage identifier to validate as a single path component.
///   待校验的系统、版本或流程标识，必须符合单个路径分量的规则。
pub(crate) fn validate_identifier(id: &str) -> Result<()> {
    let mut bytes = id.bytes();
    if !bytes.next().is_some_and(|b| b.is_ascii_alphanumeric())
        || !bytes.all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
    {
        return Err(Error::Invalid(format!(
            "标识 {id:?} 必须以 ASCII 字母或数字开头，其余字符只能为字母、数字、点、下划线或连字符"
        )));
    }
    Ok(())
}
