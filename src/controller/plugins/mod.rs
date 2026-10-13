//! Archive and directory distribution plugins.
//! 压缩包及目录形式的发行版插件。
mod cache_inputs;
mod commands;
pub(crate) mod configuration;
mod container;
mod discovery;
mod loader;
mod model;
mod persistent;
mod runner;
mod tools;
mod validation;
pub use discovery::{find, for_system};
pub use loader::load;
pub use model::{
    ActionRequirements, BootArtifact, BootArtifactKind, BootRequirements, CacheRequirements,
    ConfigRequirements, Error, LoadedPlugin, Metadata, PackagePlanRequirements, Result,
    ToolRequirements, ValueConstraint,
};
pub use persistent::PersistentDirectory;
/// Operations shared by API 2 and 3, provided as <operation>.sh or <operation>.
/// API 2 和 3 共用操作；由插件根内的 <操作名>.sh 或无后缀文件提供。
pub const REQUIRED_OPERATIONS: &[&str] = &[
    "bootstrap",
    "base_install",
    "update",
    "resolve_packages",
    "query_packages",
    "install",
    "remove",
    "mark_reason",
    "list_orphans",
    "remove_orphans",
];
/// API 3 operation initializing the stable identity of a freshly installed system.
/// API 3 为全新安装系统初始化稳定身份的操作。
pub const SYSTEM_OPERATIONS: &[&str] = &["system_init"];
pub(crate) use validation::{identifier as validate_identity, validate_boot};

/// Operations whose stdout carries records rather than build diagnostics.
/// stdout 用于传输记录而非构建诊断的操作。
pub(crate) fn is_query(operation: &str) -> bool {
    matches!(
        operation,
        "query_packages"
            | "resolve_packages"
            | "collect_packages"
            | "list_orphans"
            | "query_package_files"
            | "maintain_packages"
    )
}
use validation::{identifier, io_error};
#[cfg(test)]
mod tests;
