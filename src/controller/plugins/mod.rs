//! Packaged Bash distribution plugins.
//! 打包的 Bash 发行版插件。
mod archive;
mod container;
mod discovery;
mod model;
mod runner;
mod tools;
mod validation;
pub use archive::load;
pub use discovery::{find, for_system};
pub use model::{Error, LoadedPlugin, Metadata, Result, ToolRequirements};
pub(crate) use validation::identifier as validate_identity;
use validation::{identifier, io_error};
#[cfg(test)]
mod tests;
