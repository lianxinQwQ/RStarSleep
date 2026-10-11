//! Run the current built artifact without selecting historical versions.
//! 运行当前构建产物，不选择历史版本。
pub mod client;
mod commands;
mod config;
pub mod daemon;
pub mod desktop;
mod execution;
pub mod exports;
mod filesystem;
pub mod guest;
mod identity;
mod model;
pub(crate) mod package_files;
mod preparation;
pub mod protocol;
pub mod session;
pub mod terminal;
mod trust;

pub use config::{ExportConfig, RuntimeMount, SubsystemConfig};
pub(crate) use filesystem::open_guest;
pub(crate) use filesystem::{atomic_write, checked, guest_path, read_record, write_record};
pub use model::{Error, PreparedRuntime, Result, RuntimeRecord};
pub use preparation::prune_with_backend;
pub use preparation::{load_prepared, machine_name, prepare, prepare_with_backend};
