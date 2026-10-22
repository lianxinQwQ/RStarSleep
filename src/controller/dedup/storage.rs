//! Kernel-verified sharing of Btrfs extents without replacing either inode.
//! 由内核校验并共享 Btrfs 数据块，不替换任一 inode。
use super::super::{Error, Result};
use std::{fs::File, io, os::fd::AsRawFd, path::Path};

/// Result of comparing and sharing one candidate file.
/// 比较并共享单个候选文件的结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DedupOutcome {
    /// All ranges matched; bytes are logical shared bytes, not freed disk space.
    /// 全部范围一致；字节数表示逻辑共享量，并非实际释放空间。
    Shared(u64),
    /// At least one range differs; another source may still match.
    /// 至少一个范围不同；其它来源仍可能匹配。
    Different,
    /// The filesystem or inode does not support this operation.
    /// 文件系统或 inode 不支持本操作。
    Unsupported,
}

#[repr(C)]
#[derive(Default)]
struct RangeInfo {
    destination: i64,
    offset: u64,
    bytes: u64,
    status: i32,
    reserved: u32,
}

#[repr(C)]
struct Range {
    offset: u64,
    length: u64,
    destinations: u16,
    reserved1: u16,
    reserved2: u32,
    info: RangeInfo,
}

// EINVAL also covers incompatible inode flags/alignment on supported filesystems.
// 支持去重的文件系统也可能因 inode 标志或对齐限制返回 EINVAL。
fn unsupported(errno: i32) -> bool {
    matches!(
        errno,
        libc::EOPNOTSUPP | libc::ENOTTY | libc::EXDEV | libc::EINVAL
    )
}

/// Deduplicate equal ranges through FIDEDUPERANGE; the kernel checks every byte.
/// 通过 FIDEDUPERANGE 去重相同范围；内核逐字节校验。
/// # Arguments
/// - `source`, `target`: Pinned regular files; target is writable. 固定普通文件，目标可写。
pub(in crate::controller) fn share(source: &File, target: &File) -> Result<DedupOutcome> {
    let fail = |source| Error::Io {
        path: Path::new("FIDEDUPERANGE").into(),
        source,
    };
    let source_metadata = source.metadata().map_err(fail)?;
    let target_metadata = target.metadata().map_err(fail)?;
    if !source_metadata.is_file()
        || !target_metadata.is_file()
        || source_metadata.len() != target_metadata.len()
    {
        return Ok(DedupOutcome::Different);
    }
    let mut stat = std::mem::MaybeUninit::<libc::statfs>::uninit();
    if unsafe { libc::fstatfs(source.as_raw_fd(), stat.as_mut_ptr()) } != 0 {
        return Err(fail(io::Error::last_os_error()));
    }
    let stat = unsafe { stat.assume_init() };
    if stat.f_type != 0x9123683e {
        return Ok(DedupOutcome::Unsupported);
    }
    // Btrfs limits individual extent-same requests; aligned chunks also bound memory use.
    // Btrfs 限制单次去重范围；对齐分段也限制内核比较的内存开销。
    let block = u64::try_from(stat.f_bsize).unwrap_or(4096).max(4096);
    let chunk = (16 * 1024 * 1024 / block).max(1) * block;
    let mut offset = 0;
    let mut bytes = 0;
    while offset < source_metadata.len() {
        let length = chunk.min(source_metadata.len() - offset);
        let mut range = Range {
            offset,
            length,
            destinations: 1,
            reserved1: 0,
            reserved2: 0,
            info: RangeInfo {
                destination: i64::from(target.as_raw_fd()),
                offset,
                ..Default::default()
            },
        };
        // _IOWR(0x94, 54, file_dedupe_range): encoded size excludes its flexible array.
        // ioctl 编码中的大小不包含尾部可变数组。
        const FIDEDUPERANGE: libc::c_ulong = 0xc0189436;
        let result = unsafe { libc::ioctl(source.as_raw_fd(), FIDEDUPERANGE, &mut range) };
        if result < 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error().is_some_and(unsupported) {
                return Ok(DedupOutcome::Unsupported);
            }
            return Err(fail(error));
        }
        match range.info.status {
            0 => {
                if range.info.bytes > length {
                    return Err(Error::Invalid("去重返回字节数超过请求范围 / Deduplication returned more bytes than requested".into()));
                }
                if range.info.bytes < length {
                    // Never mark a partially processed file as fully matched.
                    // 不将仅处理部分范围的文件标记为完整匹配。
                    return Ok(DedupOutcome::Unsupported);
                }
                bytes += range.info.bytes;
            }
            1 => return Ok(DedupOutcome::Different),
            status if status < 0 && unsupported(-status) => return Ok(DedupOutcome::Unsupported),
            status if status < 0 => return Err(fail(io::Error::from_raw_os_error(-status))),
            status => {
                return Err(Error::Invalid(format!(
                    "无效去重返回状态: {status} / Invalid deduplication return status: {status}"
                )));
            }
        }
        offset += length;
    }
    Ok(DedupOutcome::Shared(bytes))
}
