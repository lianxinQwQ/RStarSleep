//! Shared infrastructure for building and maintaining managed systems.
//! 构建和维护被管理系统的共享基础设施。

/// Discover and freeze Bash distribution plugin archives or directories.
/// 查找并冻结 Bash 发行版插件压缩包或目录。
pub mod plugins;

mod accounts;
mod backend;
mod boot;
mod cache;
mod clean;
mod container;
mod current;
mod dedup;
mod deferred;
mod frozen;
mod fs;
mod identity;
mod inheritance;
mod inheritance_inputs;
mod inheritance_migration;
mod initramfs;
mod logging;
mod model;
mod mounts;
mod nocow;
mod output;
pub(crate) use output::with_terminal_progress;
mod package_watch;
mod persistent;
mod predownload;
mod preflight;
pub(crate) mod progress;
mod records;
mod resources;
mod reuse;
mod scoped_backend;
mod scripts;
mod stages;
mod workflow;

pub use backend::SystemBackend;
pub use clean::{clean, clean_with_backend};
pub use current::{CurrentBuild, read_current_build};
pub use dedup::DedupOutcome;
pub use deferred::{finalize_inheritance, finalize_inheritance_with_backend};
pub use model::{BuildBackend, BuildContext, BuildOutcome, Error, PersistentMount, Result};
pub use workflow::{
    BuildOptions, build, build_with_backend, build_with_backend_and_warning, build_with_options,
    build_with_options_and_backend, build_with_options_and_backend_and_warning,
    check_configuration,
};
