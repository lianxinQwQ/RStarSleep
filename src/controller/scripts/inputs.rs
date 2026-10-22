//! Discover ordered script entries and normalize archives, directories, and loose shell files.
//! 发现有序脚本条目，并归一化压缩包、目录及独立 shell 文件。
use crate::controller::{
    Error, Result, fs as disk,
    resources::{self, Prepared},
};
use std::{
    ffi::OsString,
    fs, io,
    path::{Path, PathBuf},
};

/// One frozen payload and its root-relative entrypoint.
/// 一项冻结资源及其相对根入口。
pub(super) struct Script {
    /// Original configuration entry identifying the script in progress messages.
    /// 在进度提示中标识脚本的原始配置条目。
    pub name: OsString,
    /// Private payload root.
    /// 私有资源根。
    pub root: PathBuf,
    /// main.sh or the original loose shell filename.
    /// main.sh 或原始独立 shell 文件名。
    pub entrypoint: PathBuf,
    /// Owner keeping the payload alive until execution finishes.
    /// 保持资源存活至执行结束的所属对象。
    _temporary: tempfile::TempDir,
}

/// Prepare all entries in filename order, treating a missing directory as an empty step.
/// 按文件名顺序准备所有条目，目录缺失时视为空步骤。
/// # Arguments
/// - `directory`: Frozen system config/script directory.
///   冻结系统的 config/script 目录。
pub(super) fn load(directory: &Path) -> Result<Vec<Script>> {
    match fs::symlink_metadata(directory) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(disk::io(directory, e)),
        Ok(info) if !info.is_dir() => {
            return Err(Error::Invalid(format!(
                "script 必须是普通目录: {0} / script must be a regular directory: {0}",
                directory.display()
            )));
        }
        Ok(_) => (),
    }
    let mut entries = fs::read_dir(directory)
        .map_err(|e| disk::io(directory, e))?
        .collect::<io::Result<Vec<_>>>()
        .map_err(|e| disk::io(directory, e))?;
    entries.sort_by_key(|entry| entry.file_name());
    entries
        .into_iter()
        .map(|entry| {
            let path = entry.path();
            let info = fs::symlink_metadata(&path).map_err(|e| disk::io(&path, e))?;
            if info.is_file() && path.extension().is_some_and(|extension| extension == "sh") {
                let temporary = tempfile::Builder::new()
                    .prefix("rstarsleep-script-input-")
                    .tempdir()
                    .map_err(|e| disk::io(&std::env::temp_dir(), e))?;
                let entrypoint = PathBuf::from(entry.file_name());
                fs::copy(&path, temporary.path().join(&entrypoint))
                    .map_err(|e| disk::io(&path, e))?;
                Ok(Script {
                    name: entry.file_name(),
                    root: temporary.path().into(),
                    entrypoint,
                    _temporary: temporary,
                })
            } else if info.is_dir()
                || (info.is_file() && resources::archive_format(&path).is_some())
            {
                let Prepared {
                    root, temporary, ..
                } = resources::prepare(&path, "main.sh", "script")?;
                Ok(Script {
                    name: entry.file_name(),
                    root,
                    entrypoint: "main.sh".into(),
                    _temporary: temporary,
                })
            } else {
                Err(Error::Invalid(format!(
                    "script 条目必须是压缩包、目录或 *.sh 普通文件: {0} / script entry must be an archive, a directory, or a regular *.sh file: {0}",
                    path.display()
                )))
            }
        })
        .collect()
}
