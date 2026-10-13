//! Plugin metadata, loaded resources, and errors.
//! 插件元数据、已加载资源和错误。
use serde::{Deserialize, Serialize};
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
    #[error(
        "插件 {plugin} 的任务 {operation} 执行失败（{status}）\nstdout:\n{stdout}\nstderr:\n{stderr}"
    )]
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
        /// Captured standard output, including diagnostics emitted there.
        /// 捕获的标准输出，包括发送到该流的诊断信息。
        stdout: String,
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
    /// Additional tools checked only after the predownload environment is initialized.
    #[serde(default)]
    pub predownload: Vec<String>,
}

/// Explicit plugin configuration contract using canonical RSTAR_ parameter names.
/// 使用规范 RSTAR_ 参数名声明的插件配置契约。
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigRequirements {
    /// Settings that must be explicitly configured with nonempty values.
    /// 必须显式配置且值非空的设置。
    pub required: Vec<String>,
    /// Accepted settings whose omission is handled by the plugin or architecture default.
    /// 接受的可选设置；缺省由插件或架构默认值处理。
    pub optional: Vec<String>,
    /// Optional value constraints keyed by canonical parameter names.
    /// 以规范参数名索引的可选值约束。
    #[serde(default)]
    pub constraints: std::collections::BTreeMap<String, ValueConstraint>,
}

/// Strict validation of raw INI assignment lists.
/// 对原始 INI 赋值列表进行严格校验。
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ValueConstraint {
    /// free-single, free-list, enum-single, enum-list, or number.
    /// 自由单项、自由列表、枚举单项、枚举列表或数字。
    pub kind: String,
    /// Allowed values for enum constraints.
    /// 枚举约束允许的值。
    #[serde(default)]
    pub values: Vec<String>,
}

/// Explicit per-stage parameter dependencies; empty lists opt out of field invalidation.
/// 显式阶段参数依赖；空列表表示不按字段使缓存失效。
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CacheRequirements {
    pub base_system: Vec<String>,
    pub software: Vec<String>,
}

/// Distribution boot requirements and ordered artifacts for main-system publication.
/// 主系统发布所需的发行版软件包及按加载顺序排列的启动产物。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BootRequirements {
    /// Concrete packages required in the expanded target installation plan.
    /// 目标安装计划中此能力所需的具体软件包。
    pub packages: Vec<String>,
    /// Target-root-relative files in their boot-loader order.
    /// 按启动加载顺序排列的目标系统根内相对文件。
    pub artifacts: Vec<BootArtifact>,
}

/// A declared boot file and its safe filename on the EFI system partition.
/// 声明的启动文件及其在 EFI 系统分区上的安全文件名。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BootArtifact {
    pub source: String,
    pub destination: String,
    pub kind: BootArtifactKind,
    #[serde(default)]
    pub optional: bool,
}

/// Boot-loader artifact roles independent of the distribution's filename convention.
/// 独立于发行版文件命名约定的启动产物角色。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BootArtifactKind {
    Kernel,
    Initrd,
}

/// Dependencies activated only for one optional plugin action.
/// 仅在调用可选插件操作时启用的依赖。
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActionRequirements {
    /// Concrete packages required in the expanded target package plan.
    /// 展开后的目标软件包计划中必需的具体软件包。
    pub packages: Vec<String>,
    /// Target executables checked by the controller before invoking the action.
    /// 控制器在调用操作前检查的目标可执行工具。
    pub tools: Vec<String>,
}

/// Query capability for target package closures in an independent resolver environment.
/// 在独立解析环境中查询目标软件包闭包的能力。
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackagePlanRequirements {
    /// Initial-system targets included even when the configured package list is empty.
    /// 即使配置包列表为空也包含的初始系统目标。
    pub base_packages: Vec<String>,
    /// Tools needed only while collecting package requirements.
    /// 仅收集软件包需求时需要的工具。
    pub tools: Vec<String>,
}

/// Metadata describing one packaged distribution plugin.
/// 描述一个打包的发行版插件的元数据。
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
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
    /// Supported target architectures.
    /// 支持的目标架构。
    pub architectures: Vec<String>,
    /// Compatibility identity for cached system roots.
    /// 缓存系统根的兼容性标识。
    pub cache_format: String,
    /// Explicit required-tool declaration, including empty lists when appropriate.
    /// 显式的工具需求声明；适用时也需显式提供空列表。
    pub tools: ToolRequirements,
    /// Required and optional settings; both lists must be present, even when empty.
    /// 必选和可选设置；两个列表都必须提供，即使为空。
    pub config: ConfigRequirements,
    /// Parameter changes that invalidate base and software caches.
    /// 使基础与软件缓存失效的参数变化。
    pub cache: CacheRequirements,
    /// Protected explicit base targets.
    /// 受保护的显式基础目标。
    #[serde(default)]
    pub protected_packages: Vec<String>,
    /// Optional single predownload operation and its independent environment scripts.
    #[serde(default)]
    pub predownload: bool,
    /// Optional query of configured targets, base packages and runtime dependencies.
    /// 可选的配置目标、基础包及运行依赖查询。
    #[serde(default)]
    pub package_plan: Option<PackagePlanRequirements>,
    /// Globally named direct subvolumes, optionally followed by :relative/mountpoint.
    #[serde(default)]
    pub persistent_dirs: Vec<String>,
    /// Host-side command:entry pairs, or same-named entries.
    /// 宿主命令:入口对，或同名入口。
    #[serde(default)]
    pub commands: Vec<String>,
    /// Optional distribution-specific inherited-mount configuration operation.
    /// 可选发行版继承挂载配置操作。
    #[serde(default)]
    pub mount_inheritance: bool,
    /// Optional installed-package file index; no arguments queries all owners.
    /// 可选已安装包文件索引；无参数查询全部所属项。
    #[serde(default)]
    pub query_package_files: bool,
    /// Optional main-system boot artifacts and their package requirements.
    /// 可选的主系统启动产物及软件包需求。
    #[serde(default)]
    pub boot: Option<BootRequirements>,
    /// Optional startup-time inheritance preparation requirements.
    /// 可选的启动阶段继承准备需求。
    #[serde(default)]
    pub initramfs_inheritance: Option<ActionRequirements>,
}

/// A loaded plugin whose temporary files live until this value is dropped.
/// 已加载插件；其临时文件保留至此值被释放。
#[derive(Debug)]
pub struct LoadedPlugin {
    /// Validated package metadata.
    /// 已校验的包元数据。
    pub metadata: Metadata,
    /// Original archive or directory location retained for diagnostics.
    /// 为诊断保留的原始压缩包或目录位置。
    pub archive: PathBuf,
    /// Exact archive SHA-256 or deterministic fingerprint of the frozen directory tree.
    /// 本次会话准确归档 SHA-256 或冻结目录树的确定性指纹。
    pub archive_sha256: String,
    /// Diagnostics for invalid plugin resources skipped during automatic discovery.
    /// 自动查找时跳过的无效插件资源的诊断信息。
    pub discovery_warnings: Vec<String>,
    pub(super) root: PathBuf,
    pub(super) temporary: tempfile::TempDir,
}

impl LoadedPlugin {
    pub fn persistent_directories(&self) -> Result<Vec<super::PersistentDirectory>> {
        super::persistent::parse(&self.metadata.persistent_dirs, self.root())
    }
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

    /// Validate and locate the operation's .sh file or unsuffixed executable.
    /// 校验并定位操作的 .sh 文件或无后缀可执行文件。
    ///
    /// # Arguments
    ///
    /// - `self`: Plugin providing operation files directly at its root.
    ///   在根内直接提供操作文件的插件。
    /// - `operation`: Operation identifier; .sh entries take precedence.
    ///   操作标识；优先选择 .sh 入口。
    ///
    /// # Errors
    ///
    /// Returns an error for invalid identifiers, missing/non-regular files, or non-executable unsuffixed entries.
    /// 标识无效、文件缺失或不是普通文件、无后缀入口不可执行时返回错误。
    pub fn operation_path(&self, operation: &str) -> Result<PathBuf> {
        super::validation::operation_path(self.root(), operation)
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
