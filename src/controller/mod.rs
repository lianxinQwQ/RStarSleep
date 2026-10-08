//! Shared infrastructure for building and maintaining managed systems.
//! 构建和维护被管理系统的共享基础设施。

/// Discover and freeze Bash distribution plugin archives or directories.
/// 查找并冻结 Bash 发行版插件压缩包或目录。
pub mod plugins;

mod backend;
mod boot;
mod cache;
mod clean;
mod container;
mod frozen;
mod fs;
mod identity;
mod inheritance;
mod model;
mod mounts;
mod persistent;
mod predownload;
mod records;
mod resources;
mod scoped_backend;
mod scripts;
mod stages;
mod workflow;

pub use backend::SystemBackend;
pub use clean::{clean, clean_with_backend};
pub use model::{BuildBackend, BuildContext, BuildOutcome, Error, PersistentMount, Result};
pub use workflow::{build, build_with_backend};
