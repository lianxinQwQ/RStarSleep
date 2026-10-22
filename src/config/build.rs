//! Read frozen build inputs and resolve the optional build workspace.
//! 读取冻结构建输入并解析可选构建工作目录。

use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs,
    path::{Component, Path, PathBuf},
};

use super::{
    ConfigStore, Error, PackageConfig, Result, SystemTypeConfig, ini::Document, io_error,
    validate_identifier,
};

/// Current mkosi-style build configuration path beneath a config root.
/// 配置根下当前 mkosi 风格构建配置路径。
pub(crate) const BUILD_CONFIG_PATH: &str = "build/config.conf";
/// Historical TOML path retained for existing systems and archived versions.
/// 为已有系统和版本留档保留的历史 TOML 路径。
pub(crate) const LEGACY_BUILD_CONFIG_PATH: &str = "build/config.toml";

/// Complete supported system-build inputs; other settings remain in the archived file.
/// 完整的已支持系统构建输入；其他设置保留在留档文件中。
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
    /// Maximum age for skipping completed stages; zero keeps normal maintenance.
    /// 跳过已完成阶段的最长分钟数；零保留常规维护。
    #[serde(default)]
    cache_reuse_minutes: u64,
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
            return Err(Error::Invalid(
                "仅支持构建格式版本 1（INI: [Config] SchemaVersion=1） / only build format version 1 is supported (INI: [Config] SchemaVersion=1)".into(),
            ));
        }
        validate_identifier(&self.system_type)?;
        if let Some(architecture) = &self.architecture {
            validate_identifier(architecture)?;
        }
        for package in &self.packages {
            crate::software::valid_target(package)
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
/// - `directory`: Configuration root containing build/config.conf or a legacy build/config.toml.
///   包含 build/config.conf 或旧 build/config.toml 的配置根。
///
/// # Errors
///
/// Returns errors for malformed, missing, or unsupported configuration.
/// 配置格式错误、缺失或不支持时返回错误。
pub fn read_build_config_at(directory: &Path) -> Result<BuildConfig> {
    let config = match Input::read(directory)? {
        Input::Ini(document) => {
            let distribution = document.system_type()?;
            BuildConfig {
                schema_version: document.schema_version()?,
                system_type: distribution.system_type,
                architecture: distribution.architecture,
                packages: document.packages()?.packages,
            }
        }
        Input::Toml { path, text } => {
            toml::from_str(&text).map_err(|source| Error::Parse { path, source })?
        }
    };
    config.validate()?;
    Ok(config)
}

/// Read the opt-in package deduplication policy from frozen system inputs.
/// 从冻结系统配置读取默认关闭的软件包去重策略。
/// # Arguments
/// - `directory`: Current or archived configuration root. 当前或留档配置根。
pub fn read_deduplicate_packages_at(directory: &Path) -> Result<bool> {
    match Input::read(directory)? {
        Input::Ini(document) => {
            let parameters = document.plugin_parameters()?;
            let Some(values) = parameters.lists.get("RSTAR_CONFIG_DEDUPLICATEPACKAGES") else {
                return Ok(false);
            };
            let [value] = values.as_slice() else {
                return Err(Error::Invalid(
                    "DeduplicatePackages 必须恰好配置一个布尔值 / DeduplicatePackages must specify exactly one boolean value".into(),
                ));
            };
            match value.to_ascii_lowercase().as_str() {
                "yes" | "true" | "1" | "on" => Ok(true),
                "no" | "false" | "0" | "off" => Ok(false),
                _ => Err(Error::Invalid(
                    "DeduplicatePackages 必须是 yes/no 或 true/false / DeduplicatePackages must be yes/no or true/false".into(),
                )),
            }
        }
        Input::Toml { path, text } => {
            let value: toml::Value =
                toml::from_str(&text).map_err(|source| Error::Parse { path, source })?;
            match value.get("deduplicate_packages") {
                None => Ok(false),
                Some(value) => value
                    .as_bool()
                    .ok_or_else(|| Error::Invalid("deduplicate_packages 必须是布尔值 / deduplicate_packages must be a boolean".into())),
            }
        }
    }
}

/// Read plugin options from [Distribution], preserving names and scalar values.
/// 从 [Distribution] 读取插件选项，保留名称和标量值。
///
/// Legacy TOML exposes architecture as Architecture and accepts a distribution_options table.
/// 旧 TOML 将 architecture 映射为 Architecture，并支持 distribution_options 表。
/// # Arguments
/// - `directory`: Current or frozen configuration root.
///   当前或冻结配置根。
/// # Errors
/// Returns an error for unreadable input or non-string legacy plugin options.
/// 输入无法读取或旧格式插件选项不是字符串时返回错误。
pub fn read_distribution_options_at(directory: &Path) -> Result<BTreeMap<String, String>> {
    match Input::read(directory)? {
        Input::Ini(document) => Ok(document.distribution_options()),
        Input::Toml { path, text } => {
            let value: toml::Value = toml::from_str(&text).map_err(|source| Error::Parse {
                path: path.clone(),
                source,
            })?;
            let mut options = BTreeMap::new();
            if let Some(architecture) = value.get("architecture").and_then(toml::Value::as_str) {
                options.insert("Architecture".into(), architecture.into());
            }
            if let Some(custom) = value.get("distribution_options") {
                let table = custom.as_table().ok_or_else(|| {
                    Error::Invalid(format!(
                        "{0}: distribution_options 必须是字符串表 / {0}: distribution_options must be a string table",
                        path.display()
                    ))
                })?;
                for (name, value) in table {
                    let value = value.as_str().ok_or_else(|| {
                        Error::Invalid(format!(
                            "{0}: distribution_options.{name} 必须是字符串 / {0}: distribution_options.{name} must be a string",
                            path.display()
                        ))
                    })?;
                    if name == "Distribution" || name == "Architecture" {
                        return Err(Error::Invalid(format!(
                            "旧 TOML 的 {name} 应使用顶层字段 / legacy TOML {name} must use a top-level field"
                        )));
                    }
                    options.insert(name.clone(), value.into());
                }
            }
            Ok(options)
        }
    }
}

/// Read every effective build parameter using the unified RSTAR_ namespace.
/// 使用统一 RSTAR_ 命名空间读取所有生效构建参数。
pub fn read_plugin_parameters_at(directory: &Path) -> Result<super::PluginParameters> {
    match Input::read(directory)? {
        Input::Ini(document) => document.plugin_parameters(),
        Input::Toml { path, text } => {
            let value: toml::Value =
                toml::from_str(&text).map_err(|source| Error::Parse { path, source })?;
            let mut parameters = super::PluginParameters::default();
            for (name, value) in value.as_table().ok_or_else(|| {
                Error::Invalid("构建配置不是表 / build configuration is not a table".into())
            })? {
                match name.as_str() {
                    "schema_version" => {
                        parameters.insert("Config", "SchemaVersion", value.to_string())?
                    }
                    "system_type" | "distribution" => parameters.insert(
                        "Distribution",
                        "Distribution",
                        legacy_parameter(value)?,
                    )?,
                    "architecture" => parameters.insert(
                        "Distribution",
                        "Architecture",
                        legacy_parameter(value)?,
                    )?,
                    "packages" => {
                        parameters.insert("Content", "Packages", legacy_parameter(value)?)?
                    }
                    // Controller-owned environment is resolved separately, like inheritance.
                    // 控制器所属环境独立解析，与继承设置类似。
                    "Environment" => (),
                    "distribution_options" => {
                        let table = value.as_table().ok_or_else(|| {
                            Error::Invalid("distribution_options 必须是字符串表 / distribution_options must be a string table".into())
                        })?;
                        for (key, value) in table {
                            if matches!(key.as_str(), "Distribution" | "Architecture")
                                || !value.is_str()
                            {
                                return Err(Error::Invalid(format!(
                                    "无效旧 TOML 插件选项: {key} / invalid legacy TOML plugin option: {key}"
                                )));
                            }
                            parameters.insert("Distribution", key, legacy_parameter(value)?)?;
                        }
                    }
                    _ if value.is_table() => {
                        parameters
                            .sections
                            .insert(super::parameters::component(name)?);
                        for (key, value) in value.as_table().unwrap() {
                            parameters.insert_list(name, key, legacy_list(value)?)?;
                        }
                    }
                    _ => parameters.insert_list("", name, legacy_list(value)?)?,
                }
            }
            for (name, list) in &mut parameters.lists {
                if name == "RSTAR_CONTENT_PACKAGES" {
                    *list = parameters.values[name].lines().map(String::from).collect();
                }
            }
            Ok(parameters)
        }
    }
}

/// Resolve [Environment] from the same merged INI or legacy TOML build inputs.
/// 从相同的合并 INI 或旧 TOML 构建输入解析 [Environment]。
/// # Arguments
/// - `directory`: Current or archived system configuration root containing build inputs.
///   包含构建输入的当前或留档系统配置根。
/// # Errors
/// Returns normal configuration read errors or invalid environment assignments.
/// 返回常规配置读取错误或无效环境赋值。
pub fn read_environment_at(directory: &Path) -> Result<super::Environment> {
    match Input::read(directory)? {
        Input::Ini(document) => document.environment(),
        Input::Toml { path, text } => {
            let value: toml::Value =
                toml::from_str(&text).map_err(|source| Error::Parse { path, source })?;
            let mut assignments = BTreeMap::new();
            if let Some(environment) = value.get("Environment") {
                let table = environment.as_table().ok_or_else(|| {
                    Error::Invalid("[Environment] 必须是表 / [Environment] must be a table".into())
                })?;
                for (name, value) in table {
                    assignments.insert(name.clone(), legacy_list(value)?);
                }
            }
            super::environment::resolve(&assignments)
        }
    }
}

/// Preserve each historical array element as its own string value.
/// 将历史数组的每个元素保留为独立字符串值。
/// # Arguments
/// - `value`: Historical TOML value. 历史 TOML 值。
fn legacy_list(value: &toml::Value) -> Result<Vec<String>> {
    if let toml::Value::Array(values) = value {
        values.iter().map(legacy_parameter).collect()
    } else {
        Ok(vec![legacy_parameter(value)?])
    }
}

/// Represent historical scalar and list values without TOML quoting.
/// 将历史标量与列表表示为不带 TOML 引号的值。
fn legacy_parameter(value: &toml::Value) -> Result<String> {
    match value {
        toml::Value::String(value) => Ok(value.clone()),
        toml::Value::Array(values) => values
            .iter()
            .map(legacy_parameter)
            .collect::<Result<Vec<_>>>()
            .map(|values| values.join("\n")),
        toml::Value::Table(_) => Err(Error::Invalid(
            "插件参数不支持嵌套表 / plugin parameters do not support nested tables".into(),
        )),
        _ => Ok(value.to_string()),
    }
}

/// Current INI input or a historical TOML input selected only when INI is absent.
/// 当前 INI 输入，或仅在 INI 缺失时选择的历史 TOML 输入。
enum Input {
    /// Current sectioned INI document.
    /// 当前分节 INI 文档。
    Ini(Document),
    /// Historical TOML contents and their diagnostic path.
    /// 历史 TOML 内容及其诊断路径。
    Toml { path: PathBuf, text: String },
}

impl Input {
    /// Read the current format preferentially without hiding errors behind legacy fallback.
    /// 优先读取当前格式，不通过旧格式回退掩盖错误。
    /// # Arguments
    /// - `directory`: Current or recorded config root.
    ///   当前或留档配置根。
    fn read(directory: &Path) -> Result<Self> {
        let build = directory.join("build");
        let mut files = fs::read_dir(&build)
            .map_err(|e| io_error(&build, e))?
            .map(|entry| entry.map(|e| e.path()).map_err(|e| io_error(&build, e)))
            .collect::<Result<Vec<_>>>()?;
        files.sort();
        let mut merged: Option<Document> = None;
        for path in files {
            if path.ends_with(LEGACY_BUILD_CONFIG_PATH) {
                continue;
            }
            let info = fs::symlink_metadata(&path).map_err(|e| io_error(&path, e))?;
            if !info.is_file() {
                return Err(Error::Invalid(format!(
                    "build 配置必须为普通文件: {0} / build configuration must be a regular file: {0}",
                    path.display()
                )));
            }
            let text = fs::read_to_string(&path).map_err(|e| io_error(&path, e))?;
            let next = Document::parse(&text, &path)?;
            if let Some(document) = &mut merged {
                document.append(next);
            } else {
                merged = Some(next);
            }
        }
        if let Some(document) = merged
            && (document.has_assignments() || !directory.join(LEGACY_BUILD_CONFIG_PATH).exists())
        {
            return Ok(Self::Ini(document));
        }
        let path = directory.join(LEGACY_BUILD_CONFIG_PATH);
        let text = fs::read_to_string(&path).map_err(|e| io_error(&path, e))?;
        Ok(Self::Toml { path, text })
    }
}

/// Read package settings independently of distribution, schema, and runtime options.
/// 独立于发行版、格式版本和运行选项读取软件包设置。
/// # Arguments
/// - `directory`: Current or recorded config root.
///   当前或留档配置根。
pub(super) fn read_packages_at(directory: &Path) -> Result<PackageConfig> {
    let config = match Input::read(directory)? {
        Input::Ini(document) => document.packages()?,
        Input::Toml { path, text } => {
            toml::from_str(&text).map_err(|source| Error::Parse { path, source })?
        }
    };
    config.validate()?;
    Ok(config)
}

/// Read distribution settings independently of packages and runtime options.
/// 独立于软件包和运行选项读取发行版设置。
/// # Arguments
/// - `directory`: Current config root.
///   当前配置根。
pub(super) fn read_system_type_at(directory: &Path) -> Result<SystemTypeConfig> {
    let config = match Input::read(directory)? {
        Input::Ini(document) => document.system_type()?,
        Input::Toml { path, text } => {
            toml::from_str(&text).map_err(|source| Error::Parse { path, source })?
        }
    };
    config.validate()?;
    Ok(config)
}

/// Normalize build options while retaining the historical TOML hash representation.
/// 规范化构建选项，并保留历史 TOML 摘要表示。
/// # Arguments
/// - `text`: Complete UTF-8 build file.
///   完整 UTF-8 构建文件。
/// - `path`: Build file path identifying its format.
///   标识格式的构建文件路径。
pub(crate) fn normalized_build_config(text: &str, path: &Path) -> Result<Vec<u8>> {
    if !path.ends_with(LEGACY_BUILD_CONFIG_PATH) {
        return Document::parse(text, path)?.normalized();
    }
    let value: toml::Value = toml::from_str(text).map_err(|source| Error::Parse {
        path: path.into(),
        source,
    })?;
    toml::to_string(&value)
        .map(String::into_bytes)
        .map_err(|e| Error::Invalid(e.to_string()))
}

impl ConfigStore {
    /// Resolve the global cache lifetime with a frozen per-system override.
    /// 读取全局缓存有效期，并应用冻结的系统配置覆盖。
    /// # Arguments
    /// - `self`: Initialized manager containing global policy. 包含全局策略的已初始化管理器。
    /// - `directory`: Frozen system configuration root. 冻结的系统配置根。
    /// # Errors
    /// Reject malformed, negative or overflowing minute counts before building.
    /// 在构建前拒绝格式错误、负值或溢出的分钟数。
    pub fn cache_reuse_minutes_at(&self, directory: &Path) -> Result<u64> {
        let path = self.global_config_path();
        let text = fs::read_to_string(&path).map_err(|e| io_error(&path, e))?;
        let options: GlobalBuildOptions =
            toml::from_str(&text).map_err(|source| Error::Parse { path, source })?;
        if options.schema_version != 1 {
            return Err(Error::Invalid(
                "仅支持全局 schema_version = 1 / only global schema_version = 1 is supported"
                    .into(),
            ));
        }
        let parameters = read_plugin_parameters_at(directory)?;
        let system = parameters
            .lists
            .get("RSTAR_CONFIG_CACHEREUSEMINUTES")
            .or_else(|| parameters.lists.get("RSTAR_CACHE_REUSE_MINUTES"));
        let minutes = match system {
            None => options.cache_reuse_minutes,
            Some(values) => {
                let [value] = values.as_slice() else {
                    return Err(Error::Invalid(
                        "CacheReuseMinutes 必须是单个非负整数 / CacheReuseMinutes must be a single nonnegative integer".into(),
                    ));
                };
                if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
                    return Err(Error::Invalid("CacheReuseMinutes 必须是非负整数 / CacheReuseMinutes must be a nonnegative integer".into()));
                }
                value.parse::<u64>().map_err(|_| {
                    Error::Invalid(
                        "CacheReuseMinutes 超出范围 / CacheReuseMinutes is out of range".into(),
                    )
                })?
            }
        };
        minutes
            .checked_mul(60)
            .ok_or_else(|| Error::Invalid("cache_reuse_minutes 换算秒数溢出 / cache_reuse_minutes overflows when converted to seconds".into()))?;
        Ok(minutes)
    }

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
            return Err(Error::Invalid(
                "仅支持全局 schema_version = 1 / only global schema_version = 1 is supported"
                    .into(),
            ));
        }
        let selected = options.work_dir.unwrap_or_else(|| PathBuf::from("work"));
        if selected.as_os_str().is_empty()
            || selected
                .components()
                .any(|part| matches!(part, Component::ParentDir))
        {
            return Err(Error::Invalid("work_dir 不能为空或包含父级跳转 / work_dir must not be empty or contain parent traversal".into()));
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
            let id = entry.file_name().into_string().map_err(|_| {
                Error::Invalid("系统标识必须是 UTF-8 / system identifier must be UTF-8".into())
            })?;
            validate_identifier(&id)?;
            let info = entry
                .file_type()
                .map_err(|source| io_error(entry.path(), source))?;
            if !info.is_dir() || info.is_symlink() {
                return Err(Error::Invalid(format!(
                    "系统条目必须是普通目录: {id} / system entry must be a regular directory: {id}"
                )));
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
