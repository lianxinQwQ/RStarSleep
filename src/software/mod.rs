//! Resolve desired targets, reconcile install reasons, and remove orphan dependencies.
//! 解析所需目标、协调安装原因并删除孤儿依赖。
mod model;
mod reconcile;
mod session;
#[cfg(test)]
mod test_support;
#[cfg(test)]
mod tests;
pub use model::{InstalledPackage, PackagePlan, SoftwareState};
pub(crate) use model::{valid_name, valid_target};
pub use session::PackageSession;
