//! Shared infrastructure for building and maintaining managed systems.
//! 构建和维护被管理系统的共享基础设施。

/// Discover and unpack Bash distribution plugin archives.
/// 查找并解包 Bash 发行版插件压缩包。
pub mod plugins;

mod backend;
mod cache;
mod frozen;
mod fs;
mod identity;
mod model;
mod records;
mod stages;
mod templates;
mod workflow;

pub use backend::SystemBackend;
pub use model::{BuildBackend, BuildContext, BuildOutcome, Error, Result};
pub use workflow::{build, build_with_backend};
