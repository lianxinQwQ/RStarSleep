//! Plugin metadata, loaded resources, and errors.
//! 插件元数据、已加载资源和错误。
use serde::Deserialize;
use std::{
    io,
    path::{Path, PathBuf},
};
/// Result type for plugin discovery, archive validation, and temporary extraction.
/// 插件查找、压缩包校验和临时解包的结果类型。
pub type Result<T> = std::result::Result<T, Error>;

/// Plugin failures carrying the affected archive or selection context.
/// 包含相关压缩包或选择上下文的插件错误。
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The system's plugin selection could not be read.
    /// 无法读取系统的插件选择配置。
    #[error(transparent)]
    Configuration(#[from] crate::config::Error),
    /// An archive or temporary file operation failed.
    /// 压缩包或临时文件操作失败。
    #[error("无法访问插件路径 {path}: {source}")]
    Io {
        /// Path involved in the operation.
        /// 操作涉及的路径。
        path: PathBuf,
        /// Original I/O failure.
        /// 原始 I/O 错误。
        #[source]
        source: io::Error,
    },
    /// Metadata, archive layout, or plugin selection is invalid.
    /// 元数据、压缩包布局或插件选择无效。
    #[error("插件无效: {0}")]
    Invalid(String),
    /// Required executables are unavailable before a plugin operation starts.
    /// 插件操作开始前，所需可执行工具不可用。
    #[error("插件 {plugin} 的任务 {operation} 在{environment}缺少工具: {}", .tools.join(", "))]
    MissingTools {
        /// Plugin identity from its metadata.
        /// 元数据中的插件标识。
        plugin: String,
        /// Operation blocked by the missing dependencies.
        /// 因依赖缺失而被阻止的操作。
        operation: String,
        /// Host or target system where executables were checked.
        /// 检查可执行工具的宿主或目标系统。
        environment: String,
        /// Missing executable names or plugin-relative paths.
        /// 缺失的可执行工具名或插件内相对路径。
        tools: Vec<String>,
    },
    /// The target preflight probe or plugin operation failed to execute successfully.
    /// 目标环境预检查或插件操作执行失败。
    #[error("插件 {plugin} 的任务 {operation} 执行失败（{status}）: {stderr}")]
    Execution {
        /// Plugin identity.
        /// 插件标识。
        plugin: String,
        /// Failed operation or dependency probe.
        /// 失败的操作或依赖探测。
        operation: String,
        /// Process exit status, including signal termination.
        /// 进程退出状态，包括信号终止。
        status: String,
        /// Process diagnostics retained for the caller.
        /// 为调用方保留的进程诊断信息。
        stderr: String,
    },
}

/// Executables required in the host and target plugin environments.
/// 宿主和目标插件环境所需的可执行工具。
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolRequirements {
    /// Host executables needed by bootstrap; paths are relative to the plugin root.
    /// bootstrap 所需的宿主可执行工具；路径相对于插件根。
    pub host: Vec<String>,
    /// Executables needed by target-system tasks inside nspawn.
    /// nspawn 内目标系统任务所需的可执行工具。
    pub system: Vec<String>,
}

/// Metadata describing one packaged Bash plugin.
/// 描述一个打包的 Bash 插件的元数据。
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Metadata {
    /// Metadata format version supported by the loader.
    /// 加载器支持的元数据格式版本。
    pub schema_version: u32,
    /// Task interface version supported by the controller.
    /// 控制器支持的任务接口版本。
    pub api_version: u32,
    /// Stable plugin identity, independent of its archive filename.
    /// 稳定插件标识，与压缩包文件名无关。
    pub id: String,
    /// Plugin release version retained in build records.
    /// 留存在构建记录中的插件发布版本。
    pub version: String,
    /// System types served by this plugin; omission falls back to its identity.
    /// 此插件支持的系统类型；省略时使用插件标识。
    #[serde(default)]
    pub system_types: Vec<String>,
    /// Bash entry path relative to the normalized plugin root.
    /// 相对于归一化插件根的 Bash 入口路径。
    pub entry: PathBuf,
    /// Supported target architectures.
    /// 支持的目标架构。
    pub architectures: Vec<String>,
    /// Compatibility identity for cached system roots.
    /// 缓存系统根的兼容性标识。
    pub cache_format: String,
    /// Explicit required-tool declaration, including empty lists when appropriate.
    /// 显式的工具需求声明；适用时也需显式提供空列表。
    pub tools: ToolRequirements,
    /// Protected explicit base targets.
    /// 受保护的显式基础目标。
    #[serde(default)]
    pub protected_packages: Vec<String>,
}

/// A loaded plugin whose temporary files live until this value is dropped.
/// 已加载插件；其临时文件保留至此值被释放。
#[derive(Debug)]
pub struct LoadedPlugin {
    /// Validated package metadata.
    /// 已校验的包元数据。
    pub metadata: Metadata,
    /// Original archive location retained for diagnostics.
    /// 为诊断保留的原始压缩包位置。
    pub archive: PathBuf,
    /// SHA-256 of the exact archive bytes loaded for this session.
    /// 本次会话实际加载的压缩包字节的 SHA-256。
    pub archive_sha256: String,
    /// Diagnostics for invalid archives skipped during automatic discovery.
    /// 自动查找时跳过的无效压缩包的诊断信息。
    pub discovery_warnings: Vec<String>,
    pub(super) root: PathBuf,
    pub(super) temporary: tempfile::TempDir,
}

impl LoadedPlugin {
    /// Return the normalized root containing all packaged resources.
    /// 返回包含全部打包资源的归一化插件根。
    ///
    /// # Arguments
    ///
    /// - `self`: Loaded plugin retaining its temporary session.
    ///   保留临时会话的已加载插件。
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Locate the validated Bash entry script without executing it.
    /// 定位已校验的 Bash 入口脚本，不执行脚本。
    ///
    /// # Arguments
    ///
    /// - `self`: Plugin whose metadata specifies the entry relative to its root.
    ///   元数据指定了根内相对入口路径的插件。
    pub fn entry_path(&self) -> PathBuf {
        self.root.join(&self.metadata.entry)
    }

    /// Return the separate writable temporary directory for plugin operations.
    /// 返回供插件操作写入的独立临时目录。
    ///
    /// # Arguments
    ///
    /// - `self`: Plugin owning the temporary directory's lifetime.
    ///   管理临时目录生命周期的插件。
    pub fn temporary_path(&self) -> PathBuf {
        self.temporary.path().join("tmp")
    }
}
