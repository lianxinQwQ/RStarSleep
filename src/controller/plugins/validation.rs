//! Validate metadata and bounded resource paths.
//! 校验元数据和限定范围的资源路径。
use super::{
    BootArtifactKind, BootRequirements, Error, Metadata, REQUIRED_OPERATIONS, Result,
    SYSTEM_OPERATIONS,
};
use std::{
    collections::BTreeSet,
    fs, io,
    path::{Component, Path, PathBuf},
};
/// Validate compatibility fields and require each same-named operation file at the root.
/// 校验兼容性字段，并要求插件根内存在每个操作的同名文件。
///
/// # Arguments
///
/// - `metadata`: Parsed plugin metadata to validate.
///   待校验的已解析插件元数据。
/// - `root`: Normalized root containing the operation files.
///   包含操作文件的归一化插件根。
pub(super) fn validate_metadata(metadata: &Metadata, root: &Path) -> Result<()> {
    if metadata.schema_version != 1 || !matches!(metadata.api_version, 2 | 3) {
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
    for tool in metadata
        .tools
        .host
        .iter()
        .chain(&metadata.tools.system)
        .chain(&metadata.tools.predownload)
    {
        tool_requirement(tool)?;
    }
    for package in &metadata.protected_packages {
        crate::software::valid_name(package).map_err(|error| Error::Invalid(error.to_string()))?;
    }
    if (metadata.predownload_plans || metadata.package_maintenance)
        && metadata.package_plan.is_none()
    {
        return Err(Error::Invalid(
            "统一软件准备及维护需要声明 [package_plan]".into(),
        ));
    }
    if metadata.package_maintenance {
        operation_path(root, "maintain_packages")?;
    }
    if metadata.post_maintenance {
        operation_path(root, "post_maintenance")?;
    }
    if let Some(plan) = &metadata.package_plan {
        if metadata.api_version != 3 {
            return Err(Error::Invalid(
                "软件包计划解析环境需要 API 3 系统初始化接口".into(),
            ));
        }
        for package in &plan.base_packages {
            crate::software::valid_name(package).map_err(|e| Error::Invalid(e.to_string()))?;
        }
        for tool in &plan.tools {
            tool_requirement(tool)?;
        }
        for operation in [
            "collect_packages",
            "predownload_install",
            "predownload_update",
        ] {
            operation_path(root, operation)?;
        }
    }
    if let Some(boot) = &metadata.boot {
        validate_boot(boot)?;
    }
    if let Some(requirements) = &metadata.initramfs_inheritance {
        for package in &requirements.packages {
            crate::software::valid_name(package)
                .map_err(|error| Error::Invalid(error.to_string()))?;
        }
        for tool in &requirements.tools {
            tool_requirement(tool)?;
        }
    }
    for entry in super::commands::parse(&metadata.commands)?.values() {
        super::commands::entry_path(root, entry)?;
    }
    if metadata.mount_inheritance {
        operation_path(root, "mount_inheritance")?;
    }
    if metadata.query_package_files {
        operation_path(root, "query_package_files")?;
    }
    metadata.config.validate()?;
    metadata.cache.validate(&metadata.config)?;
    for operation in REQUIRED_OPERATIONS {
        operation_path(root, operation)?;
    }
    if metadata.api_version == 3 {
        for operation in SYSTEM_OPERATIONS {
            operation_path(root, operation)?;
        }
    }
    super::persistent::parse(&metadata.persistent_dirs, root)?;
    if metadata.predownload {
        if metadata.api_version != 3 {
            return Err(Error::Invalid("预下载环境需要 API 3 系统初始化接口".into()));
        }
        for operation in ["predownload", "predownload_install", "predownload_update"] {
            operation_path(root, operation)?;
        }
    }
    Ok(())
}

/// Validate concrete target paths and exactly one mandatory kernel before publication.
/// 发布前校验具体目标路径，并要求恰好一个必需内核。
/// # Arguments
/// - `boot`: Main-system package and artifact declarations. 主系统软件包及产物声明。
pub(crate) fn validate_boot(boot: &BootRequirements) -> Result<()> {
    for package in &boot.packages {
        crate::software::valid_name(package).map_err(|error| Error::Invalid(error.to_string()))?;
    }
    let mut destinations = BTreeSet::new();
    for artifact in &boot.artifacts {
        let source = &artifact.source;
        if source.is_empty()
            || source.contains('\\')
            || source.chars().any(|c| c.is_whitespace() || c.is_control())
            || source
                .split('/')
                .any(|part| part.is_empty() || matches!(part, "." | ".."))
            || Path::new(source)
                .components()
                .any(|part| !matches!(part, Component::Normal(_)))
        {
            return Err(Error::Invalid(format!(
                "启动产物 source 必须是目标系统根内的规范相对文件路径: {source:?}"
            )));
        }
        identifier(&artifact.destination)?;
        if artifact.destination.ends_with('.') {
            return Err(Error::Invalid(format!(
                "启动产物 destination 必须是安全的 ESP 文件名: {:?}",
                artifact.destination
            )));
        }
        if !destinations.insert(artifact.destination.to_ascii_lowercase()) {
            return Err(Error::Invalid(format!(
                "启动产物 destination 重复: {}",
                artifact.destination
            )));
        }
    }
    let kernels: Vec<_> = boot
        .artifacts
        .iter()
        .filter(|artifact| artifact.kind == BootArtifactKind::Kernel)
        .collect();
    if kernels.len() != 1 || kernels[0].optional {
        return Err(Error::Invalid(
            "启动产物必须声明恰好一个必需的 kernel".into(),
        ));
    }
    if !boot
        .artifacts
        .iter()
        .any(|artifact| artifact.kind == BootArtifactKind::Initrd && !artifact.optional)
    {
        return Err(Error::Invalid(
            "启动产物必须声明至少一个必需的 initrd".into(),
        ));
    }
    Ok(())
}

/// Validate names and plugin-relative executable declarations shared by all actions.
/// 校验所有操作共用的工具名及插件内相对可执行文件声明。
/// # Arguments
/// - `tool`: Declared executable name or bounded relative path. 声明的工具名或限定相对路径。
fn tool_requirement(tool: &str) -> Result<()> {
    if tool.contains('/') {
        relative_path(Path::new(tool))?;
        if !Path::new(tool)
            .components()
            .any(|part| matches!(part, Component::Normal(_)))
        {
            return Err(Error::Invalid(format!(
                "工具路径必须指向插件内文件: {tool:?}"
            )));
        }
    } else {
        identifier(tool)?;
    }
    if tool.chars().any(char::is_control) || tool.trim() != tool {
        return Err(Error::Invalid(format!("无效工具声明: {tool:?}")));
    }
    Ok(())
}

/// Prefer the same-named .sh file, otherwise require an unsuffixed executable file.
/// 优先使用同名 .sh 文件，否则要求无后缀的可执行普通文件。
///
/// # Arguments
///
/// - `root`: Normalized plugin resource root.
///   归一化插件资源根。
/// - `operation`: Operation identifier used to locate either entry format.
///   用于定位两种入口格式的操作标识。
pub(super) fn operation_path(root: &Path, operation: &str) -> Result<PathBuf> {
    identifier(operation)?;
    let relative = match operation {
        "predownload_install" => "predownload/install",
        "predownload_update" => "predownload/update",
        _ => operation,
    };
    let mut path = root.join(format!("{relative}.sh"));
    if let Some(parent) = path.parent() {
        let info = fs::symlink_metadata(parent).map_err(|e| io_error(parent, e))?;
        if !info.is_dir() || info.file_type().is_symlink() {
            return Err(Error::Invalid("操作资源目录必须是普通目录".into()));
        }
    }
    let (info, script) = match fs::symlink_metadata(&path) {
        Ok(info) => (info, true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            path = root.join(relative);
            let info = fs::symlink_metadata(&path).map_err(|error| {
                if error.kind() == io::ErrorKind::NotFound {
                    Error::Invalid(format!(
                        "插件操作 {operation} 缺少同名普通文件: {}.sh 或 {}",
                        path.display(),
                        path.display()
                    ))
                } else {
                    io_error(&path, error)
                }
            })?;
            (info, false)
        }
        Err(error) => return Err(io_error(&path, error)),
    };
    if !info.is_file() {
        return Err(Error::Invalid(format!(
            "插件操作 {operation} 必须是同名普通文件: {}",
            path.display()
        )));
    }
    #[cfg(unix)]
    if !script {
        use std::os::unix::fs::PermissionsExt;
        if info.permissions().mode() & 0o111 == 0 {
            return Err(Error::Invalid(format!(
                "插件操作 {operation} 的无后缀入口必须具备可执行权限: {}",
                path.display()
            )));
        }
    }
    Ok(path)
}

/// Require a relative archive path without parent traversal or platform prefixes.
/// 要求归档路径为相对路径，且没有父级跳转或平台前缀。
///
/// # Arguments
///
/// - `path`: Entry, link target, or script path to constrain within its resource root.
///   待限定在资源根内的条目、链接目标或脚本路径。
pub(super) fn relative_path(path: &Path) -> Result<()> {
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
pub(crate) fn identifier(value: &str) -> Result<()> {
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
pub(super) fn io_error(path: impl Into<PathBuf>, source: io::Error) -> Error {
    Error::Io {
        path: path.into(),
        source,
    }
}
