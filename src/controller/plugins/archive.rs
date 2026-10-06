//! Extract and normalize distribution archives.
//! 解包和归一化发行版压缩包。
use super::validation::{relative_path, validate_metadata};
use super::{Error, LoadedPlugin, Metadata, Result, io_error};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
};
/// Extract, normalize, and validate one archive without executing its scripts.
/// 解包、归一化并校验一个压缩包，不执行其中的脚本。
///
/// # Arguments
///
/// - `path`: Existing regular tar, gzip, xz, or zstd archive file.
///   已存在的普通 tar、gzip、xz 或 zstd 压缩包文件。
///
/// # Errors
///
/// Returns an error for unsafe entries, invalid layout/metadata, unsupported versions, or I/O.
/// 条目不安全、布局或元数据无效、版本不支持或 I/O 失败时返回错误。
pub fn load(path: &Path) -> Result<LoadedPlugin> {
    let info = fs::symlink_metadata(path).map_err(|source| io_error(path, source))?;
    if !info.is_file() || info.file_type().is_symlink() {
        return Err(Error::Invalid(format!(
            "插件压缩包必须是普通文件: {}",
            path.display()
        )));
    }
    let temporary = tempfile::Builder::new()
        .prefix("rstarsleep-plugin-")
        .tempdir()
        .map_err(|source| io_error(std::env::temp_dir(), source))?;
    let staging = temporary.path().join("unpacked");
    fs::create_dir(&staging).map_err(|source| io_error(&staging, source))?;
    let mut source_file = fs::File::open(path).map_err(|source| io_error(path, source))?;
    let frozen_path = temporary.path().join("archive");
    let mut file = fs::File::options()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&frozen_path)
        .map_err(|source| io_error(&frozen_path, source))?;
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 65536];
    loop {
        let count = source_file
            .read(&mut buffer)
            .map_err(|source| io_error(path, source))?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
        file.write_all(&buffer[..count])
            .map_err(|source| io_error(&frozen_path, source))?;
    }
    use std::io::Seek;
    file.rewind().map_err(|source| io_error(path, source))?;
    let reader: Box<dyn Read> = match archive_format(path) {
        Some("gz") => Box::new(flate2::read::MultiGzDecoder::new(file)),
        Some("xz") => Box::new(xz2::read::XzDecoder::new(file)),
        Some("zst") => Box::new(
            zstd::stream::read::Decoder::new(file).map_err(|source| io_error(path, source))?,
        ),
        Some("tar") => Box::new(file),
        _ => {
            return Err(Error::Invalid(format!(
                "不支持的插件压缩格式: {}",
                path.display()
            )));
        }
    };
    unpack(reader, &staging, path)?;
    let root = normalize_root(&staging, temporary.path())?;
    let metadata_path = root.join("plugin.toml");
    let text =
        fs::read_to_string(&metadata_path).map_err(|source| io_error(&metadata_path, source))?;
    let metadata: Metadata = toml::from_str(&text)
        .map_err(|error| Error::Invalid(format!("{}: {error}", metadata_path.display())))?;
    validate_metadata(&metadata, &root)?;
    let scratch = temporary.path().join("tmp");
    fs::create_dir(&scratch).map_err(|source| io_error(&scratch, source))?;
    Ok(LoadedPlugin {
        metadata,
        archive: path.to_path_buf(),
        archive_sha256: format!("{:x}", digest.finalize()),
        discovery_warnings: Vec::new(),
        root,
        temporary,
    })
}

/// Unpack bounded relative entries while retaining executable bits for bundled tools.
/// 解包限定在根内的相对条目，保留随包工具的可执行位。
///
/// # Arguments
///
/// - `reader`: Decompressed tar stream.
///   已解压的 tar 数据流。
/// - `destination`: Empty temporary extraction directory.
///   空临时解包目录。
/// - `source`: Archive path used to contextualize read failures.
///   用于补充读取失败上下文的压缩包路径。
fn unpack(reader: Box<dyn Read>, destination: &Path, source: &Path) -> Result<()> {
    let mut archive = tar::Archive::new(reader);
    archive.set_preserve_ownerships(false);
    archive.set_preserve_permissions(false);
    archive.set_mask(0o7000);
    for entry in archive.entries().map_err(|error| io_error(source, error))? {
        let mut entry = entry.map_err(|error| io_error(source, error))?;
        let path = entry
            .path()
            .map_err(|error| io_error(source, error))?
            .into_owned();
        relative_path(&path)?;
        let kind = entry.header().entry_type();
        if !(kind.is_file() || kind.is_dir() || kind.is_symlink() || kind.is_hard_link()) {
            return Err(Error::Invalid(format!(
                "不支持的插件归档条目: {}",
                path.display()
            )));
        }
        if let Some(target) = entry.link_name().map_err(|error| io_error(source, error))? {
            relative_path(&target)?;
        }
        if !entry
            .unpack_in(destination)
            .map_err(|error| io_error(source, error))?
        {
            return Err(Error::Invalid(format!("插件路径越界: {}", path.display())));
        }
    }
    // Drain the decoder so compression checksums are verified even after tar's end marker.
    // 读取解压器至结尾，确保 tar 结束标记之后的压缩校验和也经过验证。
    io::copy(&mut archive.into_inner(), &mut io::sink())
        .map_err(|error| io_error(source, error))?;
    Ok(())
}

/// Promote a direct root or exactly one wrapper directory into the session plugin root.
/// 将直接根或恰好一个包装目录提升为会话插件根。
///
/// # Arguments
///
/// - `staging`: Directory containing all extracted entries, including hidden files.
///   包含全部解包条目（包括隐藏文件）的目录。
/// - `session`: Temporary session directory receiving the normalized plugin root.
///   接收归一化插件根的临时会话目录。
fn normalize_root(staging: &Path, session: &Path) -> Result<PathBuf> {
    let direct = staging.join("plugin.toml");
    let source = match fs::symlink_metadata(&direct) {
        Ok(info) if info.is_file() && !info.file_type().is_symlink() => staging.to_path_buf(),
        Ok(_) => {
            return Err(Error::Invalid(
                "plugin.toml 必须是插件根中的普通文件".into(),
            ));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let entries = fs::read_dir(staging)
                .map_err(|error| io_error(staging, error))?
                .collect::<io::Result<Vec<_>>>()
                .map_err(|error| io_error(staging, error))?;
            if entries.len() != 1 {
                return Err(Error::Invalid(
                    "插件根缺少 plugin.toml，且顶层不是单个包装目录".into(),
                ));
            }
            let wrapper = entries[0].path();
            let info = fs::symlink_metadata(&wrapper).map_err(|error| io_error(&wrapper, error))?;
            if !info.is_dir() || info.file_type().is_symlink() {
                return Err(Error::Invalid("插件顶层包装项必须是普通目录".into()));
            }
            let metadata = wrapper.join("plugin.toml");
            let info =
                fs::symlink_metadata(&metadata).map_err(|error| io_error(&metadata, error))?;
            if !info.is_file() || info.file_type().is_symlink() {
                return Err(Error::Invalid(
                    "包装目录内必须直接包含普通 plugin.toml 文件".into(),
                ));
            }
            wrapper
        }
        Err(error) => return Err(io_error(&direct, error)),
    };
    let root = session.join("plugin");
    fs::rename(&source, &root).map_err(|error| io_error(&source, error))?;
    Ok(root)
}

/// Recognize supported archive suffixes without using their basenames as plugin identities.
/// 识别支持的压缩包后缀，不将基本文件名作为插件标识。
///
/// # Arguments
///
/// - `path`: Candidate archive path whose suffix is inspected.
///   待检查后缀的候选压缩包路径。
pub(super) fn archive_format(path: &Path) -> Option<&'static str> {
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
