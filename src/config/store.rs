//! Resolve workspace paths and read the supported package configuration fields.
//! 定位工作目录路径，并读取已支持的软件包配置字段。

use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

use super::{
    Error, PackageConfig, Result, SystemTypeConfig,
    build::{BUILD_CONFIG_PATH, LEGACY_BUILD_CONFIG_PATH, read_packages_at, read_system_type_at},
    init::{BUILD_DEFAULT, GLOBAL_DEFAULT, SUBSYSTEM_DEFAULT},
    io_error, validate_identifier,
};

/// Access configuration through an explicit workspace path independent of any system root.
/// 通过显式工作目录路径访问配置，不依附任何系统根目录。
#[derive(Debug, Clone)]
pub struct ConfigStore {
    /// Absolute access path, either a subvolume path or its runtime mount point.
    /// 绝对访问路径，可以是子卷路径或其运行时挂载点。
    root: PathBuf,
}

/// Paths owned by one managed system, independent of its current runtime role.
/// 一个被管理系统所属的路径，与其当前运行角色无关。
#[derive(Debug, Clone)]
pub struct SystemLayout {
    /// Parent directory for all data owned by the system.
    /// 此系统所有数据的父目录。
    pub root: PathBuf,
    /// Current editable build and subsystem configuration.
    /// 当前可编辑的构建配置和子系统配置目录。
    pub config: PathBuf,
    /// Version directories containing configuration records and system roots.
    /// 保存配置留档和系统根目录的版本目录。
    pub versions: PathBuf,
    /// Disposable build caches organized by version and stage.
    /// 按版本和流程组织的可清理构建缓存目录。
    pub cache: PathBuf,
    /// Persistent data shared across versions of this system.
    /// 此系统各版本共享的持久化数据目录。
    pub persistent: PathBuf,
}

impl ConfigStore {
    /// Select an absolute workspace path without accessing or creating it.
    /// 指定绝对工作目录路径，不访问或创建该目录。
    ///
    /// `root` may be the subvolume path or its mount point; existence and Btrfs type are not checked.
    /// `root` 可以是子卷路径或挂载点；此接口不检查其存在性和 Btrfs 类型。
    ///
    /// # Arguments
    ///
    /// - `root`: Absolute workspace access path, either the dedicated subvolume or its mount point.
    ///   工作目录的绝对访问路径，可以是专用子卷路径或其挂载点。
    ///
    /// # Errors
    ///
    /// Returns [`Error::Invalid`] if `root` is relative.
    /// `root` 为相对路径时，返回 [`Error::Invalid`]。
    pub fn new(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        if !root.is_absolute() {
            return Err(Error::Invalid("工作目录必须是绝对路径".into()));
        }
        Ok(Self { root })
    }

    /// Return the workspace access path supplied to [`Self::new`].
    /// 返回调用 [`Self::new`] 时提供的工作目录访问路径。
    ///
    /// # Arguments
    ///
    /// - `self`: Configuration store containing the selected workspace path.
    ///   保存所选工作目录路径的配置存储对象。
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Locate the global configuration without reading its contents.
    /// 定位全局配置文件，不读取其内容。
    ///
    /// # Arguments
    ///
    /// - `self`: Configuration store used to locate the global file beneath its workspace.
    ///   用于在所属工作目录下定位全局配置文件的配置存储对象。
    pub fn global_config_path(&self) -> PathBuf {
        self.root.join("config/rstarsleep.toml")
    }

    /// Locate the shared directory where users place distribution plugin archives or directories.
    /// 定位用户存放发行版插件压缩包或目录的共享目录。
    ///
    /// This only resolves a path; it does not access or create the directory.
    /// 此方法仅计算路径，不访问或创建目录。
    ///
    /// # Arguments
    ///
    /// - `self`: Store whose workspace owns the plugin directory.
    ///   所属工作目录包含插件目录的配置存储对象。
    pub fn plugins_path(&self) -> PathBuf {
        self.root.join("plugins")
    }

    /// Global direct persistent subvolumes, shared independently of plugin identities.
    pub fn persistent_path(&self) -> PathBuf {
        self.root.join("persistent")
    }

    /// Download-environment caches keyed by complete plugin digest and architecture.
    pub fn predownload_path(&self) -> PathBuf {
        self.root.join("predownload")
    }

    /// Read the current system type without interpreting package or runtime settings.
    /// 读取当前系统类型，不解释软件包及运行配置。
    ///
    /// # Arguments
    ///
    /// - `self`: Store containing the managed system's build configuration.
    ///   包含被管理系统构建配置的配置存储对象。
    /// - `id`: Valid managed-system identifier whose plugin selection is read.
    ///   待读取插件选择配置的有效被管理系统标识。
    ///
    /// # Errors
    ///
    /// Returns an error for missing, malformed, or invalid type settings.
    /// 类型配置缺失、格式错误或值无效时返回错误。
    pub fn read_system_type(&self, id: &str) -> Result<SystemTypeConfig> {
        read_system_type_at(&self.system_layout(id)?.config)
    }

    /// Resolve a system's paths without requiring the directories to exist.
    /// 计算一个系统的各项路径，不要求这些目录已经存在。
    ///
    /// # Arguments
    ///
    /// - `self`: Configuration store defining the parent workspace.
    ///   指定所属工作目录的配置存储对象。
    /// - `id`: Stable managed-system identifier matching a directory name under systems/.
    ///   稳定的被管理系统标识，对应 systems/ 下的目录名。
    ///
    /// # Errors
    ///
    /// Returns [`Error::Invalid`] if `id` is not an accepted identifier.
    /// `id` 不符合标识规则时，返回 [`Error::Invalid`]。
    pub fn system_layout(&self, id: &str) -> Result<SystemLayout> {
        validate_identifier(id)?;
        let root = self.root.join("systems").join(id);
        Ok(SystemLayout {
            config: root.join("config"),
            versions: root.join("versions"),
            cache: root.join("cache"),
            persistent: root.join("persistent"),
            root,
        })
    }

    /// Create manager directories and write the global template only when absent.
    /// 创建管理器目录，仅在全局配置不存在时写入模板。
    ///
    /// # Arguments
    ///
    /// - `self`: Configuration store whose workspace directories and global template are initialized.
    ///   需要初始化管理器目录和全局配置模板的配置存储对象。
    pub(super) fn initialize_directories(&self) -> Result<()> {
        create_dir_all(&self.root.join("config"))?;
        create_dir_all(&self.root.join("systems"))?;
        create_dir_all(&self.plugins_path())?;
        create_dir_all(&self.persistent_path())?;
        create_dir_all(&self.predownload_path())?;
        write_default(&self.global_config_path(), GLOBAL_DEFAULT)?;
        super::presets::install(&self.plugins_path())
    }

    /// Atomically install or replace embedded preset archives in the workspace.
    /// 在工作目录中原子安装或替换内嵌的预设压缩包。
    ///
    /// Existing files at preset paths are replaced, including user-modified archives.
    /// 覆盖预设路径的已有文件，包括用户修改过的压缩包。
    /// Configuration files and plugins at other paths are preserved.
    /// 保留配置文件及其他路径的插件。
    ///
    /// # Arguments
    /// - `self`: Store with an existing plugin directory. 已有插件目录的配置存储对象。
    ///
    /// # Errors
    /// Returns an error for a non-file preset path or an archive write failure.
    /// 预设路径不是普通文件或压缩包写入失败时返回错误。
    pub fn update_presets(&self) -> Result<()> {
        super::presets::update(&self.plugins_path())
    }

    /// Create a managed system's ordinary directories and missing configuration templates.
    /// 创建被管理系统的普通目录，并补充缺失的配置模板。
    ///
    /// The workspace must already contain the global configuration created by initialization.
    /// 工作目录中必须已有初始化时生成的全局配置文件。
    ///
    /// Existing files are preserved and not parsed; no role, version, or system-root subvolume is created.
    /// 保留且不解析已有文件；不分配角色，也不创建版本或系统根子卷。
    ///
    /// # Arguments
    ///
    /// - `self`: Configuration store for an already initialized workspace.
    ///   指向已初始化工作目录的配置存储对象。
    /// - `id`: Stable identifier of the managed system to initialize.
    ///   需要初始化的被管理系统的稳定标识。
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid identifier, missing global file, or filesystem failure.
    /// 标识无效、缺少全局配置文件或文件系统操作失败时返回错误。
    ///
    /// Completed steps are retained if a later step fails; initialization can be retried.
    /// 后续步骤失败时保留已完成的步骤，可以再次执行初始化。
    pub fn initialize_system(&self, id: &str) -> Result<SystemLayout> {
        let layout = self.system_layout(id)?;
        if !self.global_config_path().is_file() {
            return Err(Error::Invalid(
                "请先初始化并挂载 RStarSleep 工作目录".into(),
            ));
        }
        for path in [
            layout.config.join("build"),
            layout.config.join("subsystem"),
            layout.config.join("script"),
            layout.versions.clone(),
            layout.cache.clone(),
            layout.persistent.clone(),
        ] {
            create_dir_all(&path)?;
        }
        // Preserve legacy inputs rather than shadowing their packages with an empty INI default.
        // 保留旧输入，避免新的空 INI 模板遮蔽其软件包设置。
        if !layout
            .config
            .join(LEGACY_BUILD_CONFIG_PATH)
            .try_exists()
            .map_err(|e| io_error(layout.config.join(LEGACY_BUILD_CONFIG_PATH), e))?
        {
            write_default(&layout.config.join(BUILD_CONFIG_PATH), BUILD_DEFAULT)?;
            write_default(
                &layout.config.join("build/inherit.conf"),
                "# Inherit data from the running system after scripts.\n# 脚本后从运行系统继承数据。\n[Inheritance]\n# Timing=build\n# DirectorySubvolume=yes\n# CopyOnWrite=/absolute/path\n# Mount=/absolute/path\n",
            )?;
        }
        write_default(
            &layout.config.join("subsystem/config.toml"),
            SUBSYSTEM_DEFAULT,
        )?;
        Ok(layout)
    }

    /// Read `packages` from the system's current editable build configuration.
    /// 从系统当前可编辑的构建配置中读取 `packages`。
    ///
    /// Other build fields are ignored; global and subsystem runtime files are not read.
    /// 忽略其他构建字段；不读取全局配置及子系统运行配置文件。
    ///
    /// # Arguments
    ///
    /// - `self`: Configuration store defining the workspace containing the system.
    ///   指定系统所属工作目录的配置存储对象。
    /// - `id`: Managed-system identifier whose current build configuration is read.
    ///   需要读取当前构建配置的被管理系统标识。
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid ID, unreadable file, malformed INI, missing Packages,
    /// or invalid package tokens. Legacy TOML is accepted when INI is absent.
    /// 标识无效、文件无法读取、INI 格式错误、Packages 缺失或包名无效时
    /// 返回错误；INI 缺失时接受旧 TOML，不会将缺失配置静默转换为空列表。
    pub fn read_packages(&self, id: &str) -> Result<PackageConfig> {
        read_packages_at(&self.system_layout(id)?.config)
    }

    /// Read `packages` from a version's recorded build configuration.
    /// 从某个版本留档的构建配置中读取 `packages`。
    ///
    /// Records will be created by the build module; this method does not create a record or
    /// fall back to the current editable configuration.
    /// 配置留档由后续构建模块生成；此接口不创建留档，也不会回退到当前可编辑配置。
    ///
    /// # Arguments
    ///
    /// - `self`: Configuration store defining the workspace containing the recorded configuration.
    ///   指定配置留档所属工作目录的配置存储对象。
    /// - `id`: Managed-system identifier owning the version record.
    ///   版本留档所属的被管理系统标识。
    /// - `version`: Version identifier selecting `versions/<version>/config/` within that system.
    ///   版本标识，用于选定该系统下的 `versions/<version>/config/` 目录。
    ///
    /// # Errors
    ///
    /// Returns errors under the same conditions as [`Self::read_packages`], or for an invalid version ID.
    /// 返回与 [`Self::read_packages`] 相同条件下的错误，版本标识无效时也返回错误。
    pub fn read_version_packages(&self, id: &str, version: &str) -> Result<PackageConfig> {
        validate_identifier(version)?;
        let path = self
            .system_layout(id)?
            .versions
            .join(version)
            .join("config");
        read_packages_at(&path)
    }

    /// Resolve a build cache path for one system, version, and stage without creating it.
    /// 计算一个系统、版本及流程对应的构建缓存路径，不创建目录。
    ///
    /// # Arguments
    ///
    /// - `self`: Configuration store defining the parent workspace.
    ///   指定所属工作目录的配置存储对象。
    /// - `id`: Managed-system identifier owning the cache.
    ///   缓存所属的被管理系统标识。
    /// - `version`: Version identifier used to group this build cache.
    ///   用于划分构建缓存的版本标识。
    /// - `stage`: Build-stage identifier selecting the cache directory within the version.
    ///   构建流程标识，用于选定该版本内的缓存目录。
    ///
    /// # Errors
    ///
    /// Returns [`Error::Invalid`] if any supplied identifier is invalid.
    /// 任意传入的标识无效时，返回 [`Error::Invalid`]。
    pub fn build_cache_path(&self, id: &str, version: &str, stage: &str) -> Result<PathBuf> {
        validate_identifier(version)?;
        validate_identifier(stage)?;
        Ok(self.system_layout(id)?.cache.join(version).join(stage))
    }
}

/// Create missing parent directories and attach the requested path to I/O failures.
/// 创建缺失的父目录，并在 I/O 失败时附加请求路径。
///
/// # Arguments
///
/// - `path`: Directory to create together with any missing parents.
///   需要创建的目录路径，缺失的父目录也会一并创建。
fn create_dir_all(path: &Path) -> Result<()> {
    fs::create_dir_all(path).map_err(|e| io_error(path, e))
}

/// Exclusively create a missing file so repeated initialization preserves user edits.
/// 以独占创建方式写入缺失文件，使重复初始化保留用户修改。
///
/// An existing entry is left untouched; a failed write may leave a partial file.
/// 已有目录项保持原样；写入失败时可能留下内容不完整的文件。
///
/// # Arguments
///
/// - `path`: Destination file created only when no entry already exists.
///   目标文件路径；仅在该路径不存在任何目录项时创建文件。
/// - `content`: Default UTF-8 configuration text written verbatim into a newly created file.
///   默认的 UTF-8 配置文本，原样写入新创建的文件。
fn write_default(path: &Path, content: &str) -> Result<()> {
    match fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
    {
        Ok(mut file) => file
            .write_all(content.as_bytes())
            .map_err(|e| io_error(path, e)),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(e) => Err(io_error(path, e)),
    }
}

#[cfg(test)]
#[path = "store_tests.rs"]
mod tests;
