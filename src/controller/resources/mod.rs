//! Freeze archive and directory resources independently of their consumers.
//! 独立于使用方冻结归档及目录资源。
mod archive;
mod directory;
mod session;
pub(crate) use archive::archive_format;
pub(super) use directory::{copy_tree, tree_hash};
use session::relative_path;
pub(super) use session::{Prepared, prepare};
