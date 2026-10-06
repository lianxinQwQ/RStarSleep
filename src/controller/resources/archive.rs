//! Decode supported tar archives and reject escaping or special entries.
//! 解码受支持的 tar 归档，拒绝越界及特殊条目。
use super::relative_path;
use crate::controller::{Error, Result, fs as disk};
use std::{
    io::{self, Read},
    path::Path,
};

/// Select a decoder from the archive suffix.
/// 按归档后缀选择解码器。
/// # Arguments
/// - `file`: Frozen archive bytes.
///   冻结归档文件。
/// - `path`: Original path identifying the format.
///   标识格式的原始路径。
pub(super) fn reader(file: std::fs::File, path: &Path) -> Result<Box<dyn Read>> {
    Ok(match archive_format(path) {
        Some("gz") => Box::new(flate2::read::MultiGzDecoder::new(file)),
        Some("xz") => Box::new(xz2::read::XzDecoder::new(file)),
        Some("zst") => {
            Box::new(zstd::stream::read::Decoder::new(file).map_err(|e| disk::io(path, e))?)
        }
        Some("tar") => Box::new(file),
        _ => {
            return Err(Error::Invalid(format!(
                "不支持的资源压缩格式: {}",
                path.display()
            )));
        }
    })
}

/// Extract bounded entries and verify the complete compressed stream.
/// 解包限定范围的条目，并校验完整压缩流。
/// # Arguments
/// - `reader`: Decoded stream.
///   解码后的数据流。
/// - `destination`: Empty extraction directory.
///   空解包目录。
/// - `source`: Original archive path for diagnostics.
///   用于诊断的原始归档路径。
pub(super) fn unpack(reader: Box<dyn Read>, destination: &Path, source: &Path) -> Result<()> {
    let mut archive = tar::Archive::new(reader);
    archive.set_preserve_ownerships(false);
    archive.set_preserve_permissions(false);
    archive.set_mask(0o7000);
    for entry in archive.entries().map_err(|error| disk::io(source, error))? {
        let mut entry = entry.map_err(|error| disk::io(source, error))?;
        let path = entry
            .path()
            .map_err(|error| disk::io(source, error))?
            .into_owned();
        relative_path(&path)?;
        let kind = entry.header().entry_type();
        if !(kind.is_file() || kind.is_dir() || kind.is_symlink() || kind.is_hard_link()) {
            return Err(Error::Invalid(format!(
                "不支持的插件归档条目: {}",
                path.display()
            )));
        }
        if let Some(target) = entry.link_name().map_err(|error| disk::io(source, error))? {
            relative_path(&target)?;
        }
        if !entry
            .unpack_in(destination)
            .map_err(|error| disk::io(source, error))?
        {
            return Err(Error::Invalid(format!("插件路径越界: {}", path.display())));
        }
    }
    // Drain the decoder so compression checksums are verified even after tar's end marker.
    // 读取解压器至结尾，确保 tar 结束标记之后的压缩校验和也经过验证。
    io::copy(&mut archive.into_inner(), &mut io::sink())
        .map_err(|error| disk::io(source, error))?;
    Ok(())
}

/// Recognize supported tar compression suffixes.
/// 识别受支持的 tar 压缩后缀。
/// # Arguments
/// - `path`: Candidate resource path.
///   候选资源路径。
pub(crate) fn archive_format(path: &Path) -> Option<&'static str> {
    let name = path.file_name()?.to_str()?;
    if name.ends_with(".tar.gz") || name.ends_with(".tgz") {
        Some("gz")
    } else if name.ends_with(".tar.xz") {
        Some("xz")
    } else if name.ends_with(".tar.zst") {
        Some("zst")
    } else if name.ends_with(".tar") {
        Some("tar")
    } else {
        None
    }
}
