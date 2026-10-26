//! Shared configuration workspace and terminal management without a host shell.
//! 不使用宿主 shell 的共享配置工作区及终端管理。
mod completion;
mod dashboard;
mod editor;
mod input;
pub mod terminal;
mod workspace;
pub use super::{Error, Result};
pub use workspace::{Upload, Workspace};
#[cfg(test)]
mod terminal_tests;
#[cfg(test)]
pub(crate) mod tests;
