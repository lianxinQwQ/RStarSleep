//! Archive and directory Bash distribution plugins.
//! 压缩包及目录形式的 Bash 发行版插件。
mod container;
mod discovery;
mod loader;
mod model;
mod runner;
mod tools;
mod validation;
pub use discovery::{find, for_system};
pub use loader::load;
pub use model::{Error, LoadedPlugin, Metadata, Result, ToolRequirements};
/// Operations shared by API 2 and 3, each provided as <operation>.sh at the plugin root.
/// API 2 和 3 共用操作；每个操作由插件根内的 <操作名>.sh 文件提供。
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
/// API 3 operations required to create independent systems from shared templates.
/// API 3 从共享模板创建独立系统所需的操作。
pub const TEMPLATE_OPERATIONS: &[&str] = &["template_clean", "system_init"];
pub(crate) use validation::identifier as validate_identity;
use validation::{identifier, io_error};
#[cfg(test)]
mod tests;
