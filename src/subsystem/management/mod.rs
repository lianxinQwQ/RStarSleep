//! Shared configuration workspace and terminal management without a host shell.
//! 不使用宿主 shell 的共享配置工作区及终端管理。
pub mod terminal;
mod workspace;
pub use super::{Error, Result};
pub use workspace::{Upload, Workspace};
#[cfg(test)]
pub(crate) mod tests;
