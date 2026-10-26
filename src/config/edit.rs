//! Coordinate editable inputs with configuration freezing across processes.
//! 跨进程协调可编辑输入与配置冻结。
use super::{Error, Result};
use std::{fs::{File, OpenOptions}, os::{fd::AsRawFd, unix::fs::OpenOptionsExt}, path::Path};

/// Hold an exclusive nonblocking configuration transaction lock.
/// 持有排他的非阻塞配置事务锁。
/// # Arguments
/// - `system`: Existing system directory, outside editable inputs. 已有系统目录，位于可编辑输入之外。
/// # Errors
/// Reports active uploads/freezing and filesystem failures. 报告活动上传、冻结和文件系统错误。
pub fn lock_configuration(system: &Path) -> Result<File> {
    let path = system.join("config.lock");
    let file = OpenOptions::new().read(true).write(true).create(true).truncate(false)
        .mode(0o600).custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC).open(&path)
        .map_err(|source| super::io_error(&path, source))?;
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err(Error::Invalid("配置正在上传、编辑提交或冻结，请稍后重试 / configuration is being uploaded, committed or frozen; retry later".into()));
    }
    Ok(file)
}

#[cfg(test)]
mod tests {
    /// Independent opens contend and the lock is released on transaction drop.
    /// 独立打开会争用，事务释放时解锁。
    /// # Arguments
    /// No parameters. 无参数。
    #[test]
    fn freezing_excludes_open_upload_transactions() {
        let directory = tempfile::tempdir().unwrap();
        let transaction = super::lock_configuration(directory.path()).unwrap();
        let error = super::lock_configuration(directory.path()).unwrap_err();
        assert!(error.to_string().contains("configuration is being uploaded"));
        drop(transaction);
        super::lock_configuration(directory.path()).unwrap();
    }
}
