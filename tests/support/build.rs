//! Expose build test resources and small filesystem-record helpers.
//! 导出构建测试资源和小型文件系统记录辅助函数。

use std::{fs, path::Path};

pub use super::{
    fixture::Fixture,
    simulation::{MARKER, SimulatedBackend},
};

/// Read a persisted status or completion sidecar as a generic TOML table.
/// 将持久化状态或完成旁记录读取为通用 TOML 表。
/// # Arguments
///
/// - `path`: Record path.
///   记录路径。
pub fn record(path: &Path) -> toml::Value {
    toml::from_str(&fs::read_to_string(path).unwrap()).unwrap()
}

/// Create a Linux test FIFO without forking and inheriting parallel tests' lock files.
/// 不派生进程而创建 Linux 测试 FIFO，避免继承并行测试的锁文件。
/// # Arguments
///
/// - `path`: Absent sidecar path in an isolated fixture.
///   隔离测试中不存在的旁记录路径。
#[cfg(target_os = "linux")]
pub fn create_fifo(path: &Path) {
    use std::{
        ffi::{CString, c_char, c_int},
        os::unix::ffi::OsStrExt,
    };
    unsafe extern "C" {
        /// Create a named pipe using the platform C library.
        /// 使用平台 C 库创建命名管道。
        /// # Arguments
        ///
        /// - `path`: Valid NUL-terminated path.
        ///   有效 NUL 终止路径。
        /// - `mode`: Requested permission bits.
        ///   所需权限位。
        fn mkfifo(path: *const c_char, mode: u32) -> c_int;
    }
    let native = CString::new(path.as_os_str().as_bytes()).unwrap();
    // CString remains alive and Linux mode_t is an unsigned 32-bit integer.
    // CString 保持存活，且 Linux mode_t 为 32 位无符号整数。
    let result = unsafe { mkfifo(native.as_ptr(), 0o600) };
    assert_eq!(result, 0, "{}", std::io::Error::last_os_error());
}
