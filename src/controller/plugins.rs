//! Load distribution archives selected by system type into temporary Bash environments.
//! 按系统类型将发行版压缩包加载到临时 Bash 环境。

use std::{
    fs,
    io::{self, Read, Write},
    path::{Component, Path, PathBuf},
};

use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::config::ConfigStore;

/// Result type for plugin discovery, archive validation, and temporary extraction.
/// 插件查找、压缩包校验和临时解包的结果类型。
pub type Result<T> = std::result::Result<T, Error>;

/// Plugin failures carrying the affected archive or selection context.
/// 包含相关压缩包或选择上下文的插件错误。
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The system's plugin selection could not be read.
    /// 无法读取系统的插件选择配置。
    #[error(transparent)]
    Configuration(#[from] crate::config::Error),
    /// An archive or temporary file operation failed.
    /// 压缩包或临时文件操作失败。
    #[error("无法访问插件路径 {path}: {source}")]
    Io {
        /// Path involved in the operation.
        /// 操作涉及的路径。
        path: PathBuf,
        /// Original I/O failure.
        /// 原始 I/O 错误。
        #[source]
        source: io::Error,
    },
    /// Metadata, archive layout, or plugin selection is invalid.
    /// 元数据、压缩包布局或插件选择无效。
    #[error("插件无效: {0}")]
    Invalid(String),
}

/// Metadata describing one packaged Bash plugin.
/// 描述一个打包的 Bash 插件的元数据。
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Metadata {
    /// Metadata format version supported by the loader.
    /// 加载器支持的元数据格式版本。
    pub schema_version: u32,
    /// Task interface version supported by the controller.
    /// 控制器支持的任务接口版本。
    pub api_version: u32,
    /// Stable plugin identity, independent of its archive filename.
    /// 稳定插件标识，与压缩包文件名无关。
    pub id: String,
    /// Plugin release version retained in build records.
    /// 留存在构建记录中的插件发布版本。
    pub version: String,
    /// System types served by this plugin; omission falls back to its identity.
    /// 此插件支持的系统类型；省略时使用插件标识。
    #[serde(default)]
    pub system_types: Vec<String>,
    /// Bash entry path relative to the normalized plugin root.
    /// 相对于归一化插件根的 Bash 入口路径。
    pub entry: PathBuf,
    /// Supported target architectures.
    /// 支持的目标架构。
    pub architectures: Vec<String>,
    /// Compatibility identity for cached system roots.
    /// 缓存系统根的兼容性标识。
    pub cache_format: String,
}

/// A loaded plugin whose temporary files live until this value is dropped.
/// 已加载插件；其临时文件保留至此值被释放。
#[derive(Debug)]
pub struct LoadedPlugin {
    /// Validated package metadata.
    /// 已校验的包元数据。
    pub metadata: Metadata,
    /// Original archive location retained for diagnostics.
    /// 为诊断保留的原始压缩包位置。
    pub archive: PathBuf,
    /// SHA-256 of the exact archive bytes loaded for this session.
    /// 本次会话实际加载的压缩包字节的 SHA-256。
    pub archive_sha256: String,
    /// Diagnostics for invalid archives skipped during automatic discovery.
    /// 自动查找时跳过的无效压缩包的诊断信息。
    pub discovery_warnings: Vec<String>,
    root: PathBuf,
    temporary: tempfile::TempDir,
}

impl LoadedPlugin {
    /// Return the normalized root containing all packaged resources.
    /// 返回包含全部打包资源的归一化插件根。
    ///
    /// # Arguments
    ///
    /// - `self`: Loaded plugin retaining its temporary session.
    ///   保留临时会话的已加载插件。
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Locate the validated Bash entry script without executing it.
    /// 定位已校验的 Bash 入口脚本，不执行脚本。
    ///
    /// # Arguments
    ///
    /// - `self`: Plugin whose metadata specifies the entry relative to its root.
    ///   元数据指定了根内相对入口路径的插件。
    pub fn entry_path(&self) -> PathBuf {
        self.root.join(&self.metadata.entry)
    }

    /// Return the separate writable temporary directory for plugin operations.
    /// 返回供插件操作写入的独立临时目录。
    ///
    /// # Arguments
    ///
    /// - `self`: Plugin owning the temporary directory's lifetime.
    ///   管理临时目录生命周期的插件。
    pub fn temporary_path(&self) -> PathBuf {
        self.temporary.path().join("tmp")
    }
}

/// Select a plugin using a managed system's current type and optional architecture.
/// 使用被管理系统的当前类型和可选架构选择插件。
///
/// # Arguments
///
/// - `store`: Workspace containing system configuration and the shared plugins directory.
///   包含系统配置和共享插件目录的工作目录。
/// - `id`: Managed-system identifier whose build configuration selects the plugin.
///   构建配置用于选择插件的被管理系统标识。
///
/// # Errors
///
/// Returns an error for invalid configuration, archive failures, no match, or multiple matches.
/// 配置无效、压缩包处理失败、无匹配或存在多个匹配时返回错误。
pub fn for_system(store: &ConfigStore, id: &str) -> Result<LoadedPlugin> {
    let config = store.read_system_type(id)?;
    find(
        &store.plugins_path(),
        &config.system_type,
        config
            .architecture
            .as_deref()
            .unwrap_or(std::env::consts::ARCH),
    )
}

/// Search archive metadata rather than filenames and require one matching plugin.
/// 搜索压缩包元数据而非文件名，并要求恰好一个匹配插件。
///
/// # Arguments
///
/// - `directory`: Existing shared directory containing plugin archives.
///   存放插件压缩包的现有共享目录。
/// - `system_type`: Valid type matched against metadata system_types or the plugin identity.
///   有效系统类型，与元数据 system_types 或插件标识匹配。
/// - `architecture`: Target architecture required by the current system.
///   当前系统要求的目标架构。
///
/// # Errors
///
/// Returns an error for inaccessible directories or an ambiguous/missing match. Invalid
/// archives are retained as diagnostics instead of blocking an otherwise unique valid match.
/// 目录无法访问、匹配不明确或缺失时返回错误。无效压缩包保留为诊断信息，
/// 不阻止其他有效插件的唯一匹配。
pub fn find(directory: &Path, system_type: &str, architecture: &str) -> Result<LoadedPlugin> {
    identifier(system_type)?;
    identifier(architecture)?;
    let mut paths = fs::read_dir(directory)
        .map_err(|source| io_error(directory, source))?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<io::Result<Vec<_>>>()
        .map_err(|source| io_error(directory, source))?;
    paths.sort();
    let mut matches = Vec::new();
    let mut unsupported = Vec::new();
    let mut warnings = Vec::new();
    for path in paths
        .into_iter()
        .filter(|path| archive_format(path).is_some())
    {
        let plugin = match load(&path) {
            Ok(plugin) => plugin,
            Err(error) => {
                warnings.push(format!("跳过插件 {}: {error}", path.display()));
                continue;
            }
        };
        let types = &plugin.metadata.system_types;
        let matches_type = if types.is_empty() {
            plugin.metadata.id == system_type
        } else {
            types.iter().any(|value| value == system_type)
        };
        if matches_type {
            if plugin
                .metadata
                .architectures
                .iter()
                .any(|value| value == architecture)
            {
                matches.push(plugin);
            } else {
                unsupported.push(path);
            }
        }
    }
    if matches.len() > 1 {
        let paths = matches
            .iter()
            .map(|plugin| plugin.archive.display().to_string())
            .collect::<Vec<_>>()
            .join(", ");
        return Err(Error::Invalid(format!(
            "系统类型 {system_type}、架构 {architecture} 匹配多个插件: {paths}"
        )));
    }
    if let Some(mut plugin) = matches.pop() {
        plugin.discovery_warnings = warnings;
        return Ok(plugin);
    }
    Err({
        Error::Invalid(format!(
            "{} 中没有匹配系统类型 {system_type}、架构 {architecture} 的插件{}{}",
            directory.display(),
            if unsupported.is_empty() {
                String::new()
            } else {
                "（存在该类型但不支持目标架构的插件）".into()
            },
            if warnings.is_empty() {
                String::new()
            } else {
                format!("\n{}", warnings.join("\n"))
            }
        ))
    })
}

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

/// Validate compatibility fields and require a real entry file within the plugin root.
/// 校验兼容性字段，并要求插件根内存在真实入口文件。
///
/// # Arguments
///
/// - `metadata`: Parsed plugin metadata to validate.
///   待校验的已解析插件元数据。
/// - `root`: Normalized root restricting the entry script's location.
///   限定入口脚本位置的归一化插件根。
fn validate_metadata(metadata: &Metadata, root: &Path) -> Result<()> {
    if metadata.schema_version != 1 || metadata.api_version != 1 {
        return Err(Error::Invalid(format!(
            "插件 {} 的元数据或任务接口版本不受支持",
            metadata.id
        )));
    }
    identifier(&metadata.id)?;
    identifier(&metadata.cache_format)?;
    if metadata.version.trim().is_empty() || metadata.architectures.is_empty() {
        return Err(Error::Invalid("插件版本和支持架构不能为空".into()));
    }
    for value in metadata.system_types.iter().chain(&metadata.architectures) {
        identifier(value)?;
    }
    relative_path(&metadata.entry)?;
    let entry = root.join(&metadata.entry);
    let resolved = fs::canonicalize(&entry).map_err(|error| io_error(&entry, error))?;
    if !resolved.starts_with(root) || !resolved.is_file() {
        return Err(Error::Invalid(format!(
            "插件入口必须是根内的文件: {}",
            entry.display()
        )));
    }
    Ok(())
}

/// Recognize supported archive suffixes without using their basenames as plugin identities.
/// 识别支持的压缩包后缀，不将基本文件名作为插件标识。
///
/// # Arguments
///
/// - `path`: Candidate archive path whose suffix is inspected.
///   待检查后缀的候选压缩包路径。
fn archive_format(path: &Path) -> Option<&'static str> {
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

/// Require a relative archive path without parent traversal or platform prefixes.
/// 要求归档路径为相对路径，且没有父级跳转或平台前缀。
///
/// # Arguments
///
/// - `path`: Entry, link target, or script path to constrain within its resource root.
///   待限定在资源根内的条目、链接目标或脚本路径。
fn relative_path(path: &Path) -> Result<()> {
    if path.as_os_str().is_empty()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(Error::Invalid(format!(
            "插件路径必须位于资源根内: {}",
            path.display()
        )));
    }
    Ok(())
}

/// Validate a stable ASCII plugin, system-type, or architecture identifier.
/// 校验稳定的 ASCII 插件、系统类型或架构标识。
///
/// # Arguments
///
/// - `value`: Identifier required to start with a letter or digit.
///   要求以字母或数字开头的标识。
fn identifier(value: &str) -> Result<()> {
    let mut bytes = value.bytes();
    if !bytes
        .next()
        .is_some_and(|byte| byte.is_ascii_alphanumeric())
        || !bytes.all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
    {
        return Err(Error::Invalid(format!("无效插件类型或架构标识: {value:?}")));
    }
    Ok(())
}

/// Retain the affected path and original filesystem failure.
/// 保留相关路径和原始文件系统错误。
///
/// # Arguments
///
/// - `path`: Path affected by the failure.
///   失败涉及的路径。
/// - `source`: Original I/O error.
///   原始 I/O 错误。
fn io_error(path: impl Into<PathBuf>, source: io::Error) -> Error {
    Error::Io {
        path: path.into(),
        source,
    }
}

#[cfg(test)]
mod tests {
    //! Exercise discovery, wrapper normalization, and archive rejection without running plugins.
    //! 不运行插件，验证查找、包装目录归一化和非法归档拒绝。

    use super::*;

    /// Create a minimal plugin tar stream with an optional wrapper and test entries.
    /// 创建包含可选包装目录和测试条目的最小插件 tar 数据流。
    ///
    /// # Arguments
    ///
    /// - `wrapper`: Optional directory prefix placed above all plugin resources.
    ///   放在全部插件资源之上的可选目录前缀。
    /// - `extra`: Additional raw entry path to exercise validation, or none.
    ///   用于验证路径校验的额外原始条目路径，或无。
    /// - `api_version`: Task interface version written into metadata.
    ///   写入元数据的任务接口版本。
    fn tar_bytes(wrapper: Option<&str>, extra: Option<&str>, api_version: u32) -> Vec<u8> {
        let metadata = format!(
            "schema_version=1\napi_version={api_version}\nid='test-plugin'\nversion='1'\nsystem_types=['arch']\nentry='plugin.sh'\narchitectures=['x86_64']\ncache_format='test-v1'\n"
        );
        let mut archive = tar::Builder::new(Vec::new());
        for (name, contents) in [
            ("plugin.toml", metadata.as_bytes()),
            ("plugin.sh", b"#!/bin/bash\nexit 0\n".as_slice()),
            (".hidden", b"retained".as_slice()),
            ("bin/helper", b"tool".as_slice()),
        ] {
            let path =
                wrapper.map_or_else(|| name.to_owned(), |wrapper| format!("{wrapper}/{name}"));
            let mut header = tar::Header::new_gnu();
            header.set_size(contents.len() as u64);
            header.set_mode(0o755);
            header.set_cksum();
            archive.append_data(&mut header, path, contents).unwrap();
        }
        if let Some(path) = extra {
            let mut header = tar::Header::new_gnu();
            header.as_mut_bytes()[..100].fill(0);
            header.as_mut_bytes()[..path.len()].copy_from_slice(path.as_bytes());
            header.set_entry_type(tar::EntryType::Regular);
            header.set_size(1);
            header.set_mode(0o644);
            header.set_cksum();
            archive.append(&header, b"x".as_slice()).unwrap();
        }
        archive.into_inner().unwrap()
    }

    /// Write a gzip fixture with a chosen archive layout and interface version.
    /// 写入指定归档布局和接口版本的 gzip 测试包。
    ///
    /// # Arguments
    ///
    /// - `path`: Destination archive path.
    ///   目标压缩包路径。
    /// - `wrapper`: Optional top-level wrapper directory.
    ///   可选顶层包装目录。
    /// - `extra`: Optional additional archive entry.
    ///   可选额外归档条目。
    /// - `api_version`: API version embedded in the fixture metadata.
    ///   测试元数据内的 API 版本。
    fn fixture(path: &Path, wrapper: Option<&str>, extra: Option<&str>, api_version: u32) {
        let file = fs::File::create(path).unwrap();
        let mut gzip = flate2::write::GzEncoder::new(file, flate2::Compression::default());
        gzip.write_all(&tar_bytes(wrapper, extra, api_version))
            .unwrap();
        gzip.finish().unwrap();
    }

    /// Verify direct and single-wrapper packages retain resources and clean up on drop.
    /// 验证直接根及单层包装包保留资源，并在释放时清理临时文件。
    ///
    /// # Arguments
    ///
    /// No parameters.
    /// 无参数。
    #[test]
    fn normalizes_roots_and_retains_hidden_files_and_tools() {
        let temporary = tempfile::tempdir().unwrap();
        for wrapper in [None, Some("arch-release")] {
            let path = temporary.path().join("arbitrary-name.tar.gz");
            fixture(&path, wrapper, None, 1);
            let plugin = load(&path).unwrap();
            assert_eq!(plugin.root().file_name().unwrap(), "plugin");
            assert_eq!(
                fs::read(plugin.root().join(".hidden")).unwrap(),
                b"retained"
            );
            assert_eq!(fs::read(plugin.root().join("bin/helper")).unwrap(), b"tool");
            assert!(plugin.entry_path().is_file());
            assert!(plugin.temporary_path().is_dir());
            assert_eq!(plugin.archive_sha256.len(), 64);
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                assert_ne!(
                    fs::metadata(plugin.root().join("bin/helper"))
                        .unwrap()
                        .permissions()
                        .mode()
                        & 0o111,
                    0
                );
            }
            let root = plugin.root().to_path_buf();
            drop(plugin);
            assert!(!root.exists());
        }
    }

    /// Verify metadata selection ignores filenames and refuses ambiguous matches.
    /// 验证元数据选择不依赖文件名，并拒绝不明确的匹配。
    ///
    /// # Arguments
    ///
    /// No parameters.
    /// 无参数。
    #[test]
    fn selects_by_system_type_and_rejects_duplicates_and_wrong_architecture() {
        let temporary = tempfile::tempdir().unwrap();
        fixture(
            &temporary.path().join("unrelated-name.tar.gz"),
            None,
            None,
            1,
        );
        assert_eq!(
            find(temporary.path(), "arch", "x86_64")
                .unwrap()
                .metadata
                .id,
            "test-plugin"
        );
        assert!(find(temporary.path(), "debian", "x86_64").is_err());
        assert!(find(temporary.path(), "arch", "aarch64").is_err());
        fixture(&temporary.path().join("second.tar.gz"), None, None, 1);
        assert!(
            find(temporary.path(), "arch", "x86_64")
                .unwrap_err()
                .to_string()
                .contains("多个插件")
        );
    }

    /// Verify an unrelated broken archive does not block a unique valid plugin.
    /// 验证无关的损坏压缩包不会阻止唯一有效插件。
    ///
    /// # Arguments
    ///
    /// No parameters.
    /// 无参数。
    #[test]
    fn retains_bad_archive_diagnostics_without_blocking_valid_selection() {
        let temporary = tempfile::tempdir().unwrap();
        fixture(&temporary.path().join("valid.tar.gz"), None, None, 1);
        fs::write(temporary.path().join("broken.tar.gz"), b"invalid").unwrap();
        let plugin = find(temporary.path(), "arch", "x86_64").unwrap();
        assert_eq!(plugin.discovery_warnings.len(), 1);
        assert!(plugin.discovery_warnings[0].contains("broken.tar.gz"));
        let missing = find(temporary.path(), "debian", "x86_64")
            .unwrap_err()
            .to_string();
        assert!(missing.contains("broken.tar.gz"));
    }

    /// Reject multiple roots, recursive wrappers, unsafe paths, and incompatible metadata.
    /// 拒绝多根、多层包装、不安全路径和不兼容元数据。
    ///
    /// # Arguments
    ///
    /// No parameters.
    /// 无参数。
    #[test]
    fn rejects_invalid_layouts_paths_and_api_versions() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("plugin.tar.gz");
        for (wrapper, extra, api) in [
            (Some("one"), Some(".extra"), 1),
            (Some("one/two"), None, 1),
            (None, Some("../escape"), 1),
            (None, Some("/absolute"), 1),
            (None, None, 2),
        ] {
            fixture(&path, wrapper, extra, api);
            assert!(
                load(&path).is_err(),
                "accepted {wrapper:?}, {extra:?}, {api}"
            );
        }
    }

    /// Verify all documented compression formats load the same metadata.
    /// 验证文档中的各压缩格式加载同一元数据。
    ///
    /// # Arguments
    ///
    /// No parameters.
    /// 无参数。
    #[test]
    fn accepts_tar_xz_and_zstd_archives() {
        let temporary = tempfile::tempdir().unwrap();
        let tar = tar_bytes(None, None, 1);
        let plain = temporary.path().join("plugin.tar");
        fs::write(&plain, &tar).unwrap();
        let xz = temporary.path().join("plugin.tar.xz");
        let mut encoder = xz2::write::XzEncoder::new(fs::File::create(&xz).unwrap(), 1);
        encoder.write_all(&tar).unwrap();
        encoder.finish().unwrap();
        let zstd = temporary.path().join("plugin.tar.zst");
        fs::write(&zstd, zstd::stream::encode_all(tar.as_slice(), 1).unwrap()).unwrap();
        for path in [&plain, &xz, &zstd] {
            assert_eq!(load(path).unwrap().metadata.id, "test-plugin");
        }
    }
}
