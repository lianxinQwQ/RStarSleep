//! Read frozen build inputs and resolve the optional build workspace.
//! 读取冻结构建输入并解析可选构建工作目录。

use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Component, Path, PathBuf},
};

use super::{ConfigStore, Error, Result, io_error, validate_identifier};

/// Complete supported system-build inputs; other fields remain in the archived TOML.
/// 完整的已支持系统构建输入；其他字段保留在留档 TOML 中。
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct BuildConfig {
    /// Supported build configuration schema.
    /// 支持的构建配置格式版本。
    pub schema_version: u32,
    /// Distribution type used for automatic plugin selection.
    /// 用于自动选择插件的发行版类型。
    #[serde(alias = "distribution")]
    pub system_type: String,
    /// Optional architecture; omission selects the host architecture.
    /// 可选架构；省略时使用宿主架构。
    pub architecture: Option<String>,
    /// Required configured targets, retaining order and duplicates in the archive.
    /// 必需的配置目标，留档中保留顺序和重复项。
    pub packages: Vec<String>,
}

/// Global fields relevant to temporary build-root placement.
/// 与临时构建根放置位置相关的全局字段。
#[derive(Deserialize)]
struct GlobalBuildOptions {
    /// Supported serialized record format.
    /// 受支持序列化记录格式。
    schema_version: u32,
    /// Optional scratch directory relative to the manager root or absolute.
    /// 可选临时目录，相对于管理根或为绝对路径。
    work_dir: Option<PathBuf>,
}

impl BuildConfig {
    /// Validate supported schema, distribution identity, and concrete package targets.
    /// 校验支持的格式版本、发行版标识和具体包目标。
    ///
    /// # Arguments
    ///
    /// - `self`: Supported inputs read from current or archived configuration.
    ///   从当前或留档配置读取的已支持输入。
    ///
    /// # Errors
    ///
    /// Returns an error for unsupported schemas or invalid identifiers and package names.
    /// 格式版本不支持、标识或包名无效时返回错误。
    pub fn validate(&self) -> Result<()> {
        if self.schema_version != 1 {
            return Err(Error::Invalid("仅支持 schema_version = 1".into()));
        }
        validate_identifier(&self.system_type)?;
        if let Some(architecture) = &self.architecture {
            validate_identifier(architecture)?;
        }
        for package in &self.packages {
            crate::software::valid_name(package)
                .map_err(|error| Error::Invalid(error.to_string()))?;
        }
        Ok(())
    }
}

/// Read one complete build file beneath a current or recorded configuration directory.
/// 读取当前或留档配置目录下的完整构建文件。
///
/// # Arguments
///
/// - `directory`: Existing configuration directory containing build/config.toml.
///   包含 build/config.toml 的现有配置目录。
///
/// # Errors
///
/// Returns errors for malformed, missing, or unsupported configuration.
/// 配置格式错误、缺失或不支持时返回错误。
pub fn read_build_config_at(directory: &Path) -> Result<BuildConfig> {
    let path = directory.join("build/config.toml");
    let text = fs::read_to_string(&path).map_err(|source| io_error(&path, source))?;
    let config: BuildConfig =
        toml::from_str(&text).map_err(|source| Error::Parse { path, source })?;
    config.validate()?;
    Ok(config)
}

impl ConfigStore {
    /// Read one system's editable complete build configuration.
    /// 读取一个系统当前可编辑的完整构建配置。
    ///
    /// # Arguments
    ///
    /// - `self`: Workspace owning the system.
    ///   系统所属工作目录。
    /// - `id`: Managed-system identifier.
    ///   被管理系统标识。
    ///
    /// # Errors
    ///
    /// Returns identifier, read, parse, and input validation failures.
    /// 返回标识、读取、解析和输入校验错误。
    pub fn read_build_config(&self, id: &str) -> Result<BuildConfig> {
        read_build_config_at(&self.system_layout(id)?.config)
    }

    /// Read build inputs exclusively from a previous version's configuration archive.
    /// 仅从过往版本配置留档读取构建输入。
    ///
    /// # Arguments
    ///
    /// - `self`: Workspace owning the version.
    ///   版本所属工作目录。
    /// - `id`: Managed-system identifier.
    ///   被管理系统标识。
    /// - `version`: Valid previous version identifier.
    ///   有效过往版本标识。
    ///
    /// # Errors
    ///
    /// Returns errors without falling back to editable configuration.
    /// 返回错误，不回退到可编辑配置。
    pub fn read_recorded_build_config(&self, id: &str, version: &str) -> Result<BuildConfig> {
        validate_identifier(version)?;
        read_build_config_at(
            &self
                .system_layout(id)?
                .versions
                .join(version)
                .join("config"),
        )
    }

    /// Resolve build scratch roots without moving configuration, plugins, or system archives.
    /// 解析构建临时根，不移动配置、插件或系统留档。
    ///
    /// # Arguments
    ///
    /// - `self`: Initialized manager root containing global configuration.
    ///   包含全局配置的已初始化管理根。
    ///
    /// # Errors
    ///
    /// Returns an error for invalid global TOML, schema, or work-directory paths.
    /// 全局 TOML、格式版本或工作目录路径无效时返回错误。
    pub fn build_work_dir(&self) -> Result<PathBuf> {
        let path = self.global_config_path();
        let text = fs::read_to_string(&path).map_err(|source| io_error(&path, source))?;
        let options: GlobalBuildOptions =
            toml::from_str(&text).map_err(|source| Error::Parse { path, source })?;
        if options.schema_version != 1 {
            return Err(Error::Invalid("仅支持全局 schema_version = 1".into()));
        }
        let selected = options.work_dir.unwrap_or_else(|| PathBuf::from("work"));
        if selected.as_os_str().is_empty()
            || selected
                .components()
                .any(|part| matches!(part, Component::ParentDir))
        {
            return Err(Error::Invalid("work_dir 不能为空或包含父级跳转".into()));
        }
        Ok(if selected.is_absolute() {
            selected
        } else {
            self.root().join(selected)
        })
    }

    /// Enumerate managed system directories deterministically without parsing their build inputs.
    /// 确定性列举被管理系统目录，不解析其构建输入。
    ///
    /// # Arguments
    ///
    /// - `self`: Manager root owning systems/.
    ///   拥有 systems/ 的管理根。
    ///
    /// # Errors
    ///
    /// Returns an error for directory access or invalid system entries.
    /// 目录访问或系统条目无效时返回错误。
    pub fn list_systems(&self) -> Result<Vec<String>> {
        let path = self.root().join("systems");
        let mut systems = Vec::new();
        for entry in fs::read_dir(&path).map_err(|source| io_error(&path, source))? {
            let entry = entry.map_err(|source| io_error(&path, source))?;
            let id = entry
                .file_name()
                .into_string()
                .map_err(|_| Error::Invalid("系统标识必须是 UTF-8".into()))?;
            validate_identifier(&id)?;
            let info = entry
                .file_type()
                .map_err(|source| io_error(entry.path(), source))?;
            if !info.is_dir() || info.is_symlink() {
                return Err(Error::Invalid(format!("系统条目必须是普通目录: {id}")));
            }
            systems.push(id);
        }
        systems.sort();
        Ok(systems)
    }
}

#[cfg(test)]
#[path = "build_tests.rs"]
mod tests;
