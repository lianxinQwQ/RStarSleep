//! Read inheritance independently of mkosi-style build options from merged INI inputs.
//! 从合并后的 INI 输入独立读取继承选项，不作为 mkosi 构建选项处理。
use super::{Error, PluginParameters, Result};
use serde::{Deserialize, Serialize};
const DEFAULT_MANAGER: &str = "/rstarsleep";
use std::{
    collections::BTreeMap,
    path::{Component, Path, PathBuf},
};

/// Explicit handling of NOCOW data that cannot use the ordinary inheritance copy.
/// 无法直接继承的 NOCOW 数据处理策略；不在策略之间自动降级。
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NocowHandling {
    #[default]
    Error,
    Reflink,
    Copy,
    MoveSnapshot,
}

impl std::fmt::Display for NocowHandling {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Error => "error",
            Self::Reflink => "reflink",
            Self::Copy => "copy",
            Self::MoveSnapshot => "move-snapshot",
        })
    }
}

/// Current-running-system inheritance requests.
/// 当前运行系统的继承请求。
#[derive(Debug, Clone)]
pub struct InheritanceConfig {
    /// Relative exceptions owned by each explicitly configured CoW directory.
    /// 各显式配置 CoW 目录所属的相对例外路径。
    pub preserve: BTreeMap<PathBuf, Vec<PathBuf>>,
    /// Absolute paths copied into script-root after package cache commit,
    /// before post-processing and scripts.
    /// 软件缓存提交后、插件后处理及脚本执行前，复制到 script-root 的运行系统绝对路径。
    pub copy_on_write_build: Vec<PathBuf>,
    /// Absolute paths captured when each version is first started.
    /// 每个版本首次启动时继承的绝对路径。
    pub copy_on_write_boot: Vec<PathBuf>,
    /// Absolute paths backed by this system's persistent objects.
    /// 由本系统持久对象支持的绝对路径。
    pub mounts: Vec<PathBuf>,
    /// Give inherited directories independent subvolumes for subsequent snapshots.
    pub directory_subvolume: bool,
    pub nocow_handling: NocowHandling,
}

impl Default for InheritanceConfig {
    fn default() -> Self {
        Self {
            preserve: BTreeMap::new(),
            copy_on_write_build: Vec::new(),
            copy_on_write_boot: Vec::new(),
            mounts: Vec::new(),
            directory_subvolume: true,
            nocow_handling: NocowHandling::Error,
        }
    }
}

fn scalar<'a>(key: &str, values: &'a [String]) -> Result<&'a str> {
    match values {
        [value] => Ok(value),
        _ => Err(Error::Invalid(format!(
            "{key} 必须只赋值一次 / {key} must be assigned exactly once"
        ))),
    }
}

/// Encode slashes after removing the directory type marker from persistent object names.
/// 去掉声明目录的末尾斜杠后，对其余斜杠编码，包括开头的斜杠。
/// # Arguments
/// - `path`: Normalized absolute inheritance source. 规范绝对继承来源。
/// # Errors
/// Rejects non-UTF-8, non-normalized, relative or root paths.
/// 拒绝非 UTF-8、非规范、相对或根路径。
pub fn inheritance_name(path: &Path) -> Result<String> {
    let text = path.to_str().ok_or_else(|| {
        Error::Invalid("继承路径必须为 UTF-8 / inheritance path must be UTF-8".into())
    })?;
    if !text.starts_with('/')
        || path == Path::new("/")
        || text.chars().any(char::is_control)
        || path
            .components()
            .any(|part| !matches!(part, Component::RootDir | Component::Normal(_)))
        || text.contains("//")
        || text.contains("/./")
        || text.ends_with("/.")
    {
        return Err(Error::Invalid(format!(
            "继承项目必须为规范绝对路径且不能为 /: {text:?} / inheritance entry must be a canonical absolute path other than /: {text:?}"
        )));
    }
    Ok(text.trim_end_matches('/').replace('/', "-"))
}

/// Check the configured directory marker against the object used for inheritance.
/// 按继承时实际使用的对象类型核验配置中的目录标记，不访问文件系统。
pub(crate) fn validate_inheritance_type(path: &Path, directory: bool) -> Result<()> {
    let text = path.to_str().ok_or_else(|| {
        Error::Invalid("继承路径必须为 UTF-8 / inheritance path must be UTF-8".into())
    })?;
    if directory != text.ends_with('/') {
        return Err(Error::Invalid(format!(
            "{}: {text:?}",
            if directory {
                "继承目录路径必须以 / 结尾 / inherited directory path must end with /"
            } else {
                "继承文件路径不能以 / 结尾 / inherited file path must not end with /"
            }
        )));
    }
    Ok(())
}

/// Validate literal relative exception paths and reject overlapping selections.
/// 校验字面相对例外路径并拒绝重叠选择。
pub(crate) fn validate_preserve_paths(paths: &[PathBuf]) -> Result<()> {
    for (index, path) in paths.iter().enumerate() {
        let text = path.to_str().ok_or_else(|| {
            Error::Invalid("例外路径必须为 UTF-8 / Preserve path must be UTF-8".into())
        })?;
        let unmarked = text.strip_suffix('/').unwrap_or(text);
        if unmarked.is_empty()
            || text.starts_with('/')
            || text.chars().any(char::is_control)
            || unmarked
                .split('/')
                .any(|part| matches!(part, "" | "." | ".."))
        {
            return Err(Error::Invalid(format!(
                "例外必须为规范相对路径: {text:?} / Preserve entry must be a normalized relative path: {text:?}"
            )));
        }
        for other in &paths[..index] {
            if path.starts_with(other) || other.starts_with(path) {
                return Err(Error::Invalid(format!(
                    "例外路径不能重复或重叠: {0} 与 {1} / Preserve paths must not repeat or overlap: {0} and {1}",
                    path.display(),
                    other.display()
                )));
            }
        }
    }
    Ok(())
}

impl InheritanceConfig {
    /// Return the exceptions explicitly attached to this inheritance entry.
    /// 返回显式绑定到此继承项的例外。
    pub fn preserve_for(&self, path: &Path) -> &[PathBuf] {
        self.preserve
            .get(path)
            .map(Vec::as_slice)
            .unwrap_or_default()
    }
    /// Extract controller-owned inheritance settings while rejecting overlaps and name collisions.
    /// 提取控制器所属继承设置，拒绝路径重叠及名称冲突。
    /// # Arguments
    /// - `parameters`: All merged assignments. 全部合并赋值。
    /// # Errors
    /// Returns unknown inheritance keys or invalid/conflicting paths.
    /// 返回未知继承键或无效及冲突路径。
    pub fn read(parameters: &PluginParameters) -> Result<Self> {
        let mut config = Self::default();
        let mut names = BTreeMap::new();
        for (key, values) in &parameters.lists {
            if !key.starts_with("RSTAR_INHERITANCE_") {
                continue;
            }
            match key.as_str() {
                "RSTAR_INHERITANCE_TIMING" | "RSTAR_INHERITANCE_COPYONWRITE" => {
                    return Err(Error::Invalid(
                        "继承配置不再支持 Timing 或 CopyOnWrite；请将构建时路径迁移到 CopyOnWriteBuild，首次启动时路径迁移到 CopyOnWriteBoot / inheritance configuration no longer supports Timing or CopyOnWrite; move build-time paths to CopyOnWriteBuild and first-boot paths to CopyOnWriteBoot".into(),
                    ));
                }
                "RSTAR_INHERITANCE_DIRECTORYSUBVOLUME" => {
                    config.directory_subvolume = match scalar(key, values)? {
                        "yes" => true,
                        "no" => false,
                        _ => {
                            return Err(Error::Invalid(
                                "DirectorySubvolume 必须为 yes 或 no / DirectorySubvolume must be yes or no".into(),
                            ));
                        }
                    };
                    continue;
                }
                "RSTAR_INHERITANCE_NOCOWHANDLING" => {
                    config.nocow_handling = match scalar(key, values)? {
                        "error" => NocowHandling::Error,
                        "reflink" => NocowHandling::Reflink,
                        "copy" => NocowHandling::Copy,
                        "move-snapshot" => NocowHandling::MoveSnapshot,
                        _ => {
                            return Err(Error::Invalid(
                                "NocowHandling 必须为 error、reflink、copy 或 move-snapshot / NocowHandling must be error, reflink, copy, or move-snapshot".into(),
                            ));
                        }
                    };
                    continue;
                }
                _ => (),
            }
            let target = match key.as_str() {
                "RSTAR_INHERITANCE_COPYONWRITEBUILD" => &mut config.copy_on_write_build,
                "RSTAR_INHERITANCE_COPYONWRITEBOOT" => &mut config.copy_on_write_boot,
                "RSTAR_INHERITANCE_MOUNT" => &mut config.mounts,
                _ => {
                    return Err(Error::Invalid(format!(
                        "未知继承配置: {key} / unknown inheritance setting: {key}"
                    )));
                }
            };
            for value in values {
                for line in value.lines().filter(|line| !line.is_empty()) {
                    let path = PathBuf::from(line);
                    let name = inheritance_name(&path)?;
                    if key == "RSTAR_INHERITANCE_MOUNT"
                        && (line.contains([':', '\\'])
                            || path.starts_with("/run/rstarsleep")
                            || Path::new("/run/rstarsleep").starts_with(&path)
                            || path.starts_with(DEFAULT_MANAGER)
                            || Path::new(DEFAULT_MANAGER).starts_with(&path))
                    {
                        return Err(Error::Invalid(format!(
                            "Mount 路径与管理器运行环境冲突或含不支持的绑定分隔符: {line} / Mount path conflicts with the manager runtime or contains an unsupported bind separator: {line}"
                        )));
                    }
                    if let Some(previous) = names.insert(name, path.clone()) {
                        return Err(Error::Invalid(format!(
                            "继承路径重复或编码冲突: {0} 与 {1} / duplicate inheritance paths or encoding collision: {0} and {1}",
                            previous.display(),
                            path.display()
                        )));
                    }
                    target.push(path);
                }
            }
        }
        for (section, fields) in &parameters.inheritance_sections {
            let parts: Vec<_> = section.split('.').collect();
            if parts.len() != 3 || parts[0] != "Inheritance" {
                return Err(Error::Invalid(format!(
                    "无效继承分节: {section} / Invalid inheritance section: {section}"
                )));
            }
            super::validate_identifier(parts[2])?;
            let target = match parts[1] {
                "CopyOnWriteBuild" => &mut config.copy_on_write_build,
                "CopyOnWriteBoot" => &mut config.copy_on_write_boot,
                _ => {
                    return Err(Error::Invalid(format!(
                        "未知继承类型: {section} / Unknown inheritance type: {section}"
                    )));
                }
            };
            for key in fields.keys() {
                if !matches!(key.as_str(), "Path" | "Preserve") {
                    return Err(Error::Invalid(format!(
                        "未知继承字段: {section}.{key} / Unknown inheritance field: {section}.{key}"
                    )));
                }
            }
            let path = PathBuf::from(scalar(
                "Path",
                fields.get("Path").map(Vec::as_slice).unwrap_or_default(),
            )?);
            let name = inheritance_name(&path)?;
            if let Some(previous) = names.insert(name, path.clone()) {
                return Err(Error::Invalid(format!(
                    "继承路径重复或编码冲突: {0} 与 {1} / Duplicate inheritance paths or encoding collision: {0} and {1}",
                    previous.display(),
                    path.display()
                )));
            }
            let preserve: Vec<_> = fields
                .get("Preserve")
                .into_iter()
                .flatten()
                .flat_map(|value| value.lines())
                .filter(|line| !line.is_empty())
                .map(PathBuf::from)
                .collect();
            validate_preserve_paths(&preserve)?;
            if !preserve.is_empty() {
                validate_inheritance_type(&path, true)?;
                config.preserve.insert(path.clone(), preserve);
            }
            target.push(path);
        }
        if config.nocow_handling == NocowHandling::MoveSnapshot && !config.directory_subvolume {
            return Err(Error::Invalid(
                "NocowHandling=move-snapshot 要求 DirectorySubvolume=yes / NocowHandling=move-snapshot requires DirectorySubvolume=yes".into(),
            ));
        }
        let paths: Vec<_> = config
            .copy_on_write_build
            .iter()
            .chain(&config.copy_on_write_boot)
            .chain(&config.mounts)
            .collect();
        for (index, path) in paths.iter().enumerate() {
            for other in &paths[index + 1..] {
                if path.starts_with(other) || other.starts_with(path) {
                    return Err(Error::Invalid(format!(
                        "继承路径不能重叠: {0} 与 {1} / inheritance paths must not overlap: {0} and {1}",
                        path.display(),
                        other.display()
                    )));
                }
            }
        }
        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Keep named sections structured while combining file and directory exceptions.
    /// 保持具名分节结构，同时组合文件和目录例外。
    #[test]
    fn named_entries_bind_relative_preserves_and_do_not_leak_to_plugins() {
        let mut document = crate::config::ini::Document::parse(
            "[Inheritance]\nCopyOnWriteBuild=/etc/hostname\n[Inheritance.CopyOnWriteBuild.app]\nPath=/opt/app/\nPreserve=config\n[Inheritance.CopyOnWriteBoot.data]\nPath=/var/lib/app/\nPreserve=initial/\n", Path::new("first.conf")
        ).unwrap();
        document.append(
            crate::config::ini::Document::parse(
                "[Inheritance.CopyOnWriteBuild.app]\nPreserve=\nPreserve=generated/\n",
                Path::new("second.conf"),
            )
            .unwrap(),
        );
        let parameters = document.plugin_parameters().unwrap();
        assert_eq!(parameters.inheritance_sections.len(), 2);
        assert!(
            !parameters
                .values
                .keys()
                .any(|key| key.contains("APP") || key.contains("PRESERVE"))
        );
        let config = InheritanceConfig::read(&parameters).unwrap();
        assert_eq!(
            config.preserve_for(Path::new("/opt/app/")),
            [PathBuf::from("config"), PathBuf::from("generated/")]
        );
        assert_eq!(
            config.preserve_for(Path::new("/var/lib/app/")),
            [PathBuf::from("initial/")]
        );
        assert_eq!(config.copy_on_write_build.len(), 2);
    }

    /// Reject malformed ownership and literal paths before running build operations.
    /// 在执行构建操作前拒绝无效归属及字面路径。
    #[test]
    fn named_entries_reject_invalid_sections_fields_and_overlaps() {
        for text in [
            "[Inheritance.Mount.app]\nPath=/opt/app/\n",
            "[Inheritance.CopyOnWriteBuild.app.extra]\nPath=/opt/app/\n",
            "[Inheritance.CopyOnWriteBuild.]\nPath=/opt/app/\n",
            "[Inheritance.CopyOnWriteBuild.app]\nPreserve=config\n",
            "[Inheritance.CopyOnWriteBuild.app]\nPath=/opt/app/\nPath=/opt/app/\n",
            "[Inheritance.CopyOnWriteBuild.app]\nPath=/opt/app/\nUnknown=yes\n",
            "[Inheritance.CopyOnWriteBuild.app]\nPath=/opt/file\nPreserve=config\n",
            "[Inheritance]\nCopyOnWriteBuild=/opt/app/\n[Inheritance.CopyOnWriteBoot.app]\nPath=/opt/app/\n",
        ] {
            let document =
                crate::config::ini::Document::parse(text, Path::new("invalid.conf")).unwrap();
            assert!(
                InheritanceConfig::read(&document.plugin_parameters().unwrap()).is_err(),
                "{text}"
            );
        }
        for path in [
            "/absolute",
            ".",
            "..",
            "a/../b",
            "a/./b",
            "a//b",
            "a//",
            "a/.",
            "a\nb",
        ] {
            assert!(validate_preserve_paths(&[path.into()]).is_err(), "{path:?}");
        }
        for (first, second) in [
            ("app/", "app/file"),
            ("app", "app/"),
            ("app/file", "app/file"),
        ] {
            assert!(validate_preserve_paths(&[first.into(), second.into()]).is_err());
        }
        assert!(validate_preserve_paths(&["app/".into(), "application/".into()]).is_ok());
    }

    #[test]
    fn nocow_policy_is_one_strict_scalar_and_move_requires_subvolumes() {
        let mut parameters = PluginParameters::default();
        assert_eq!(
            InheritanceConfig::read(&parameters).unwrap().nocow_handling,
            NocowHandling::Error
        );
        for (text, policy) in [
            ("error", NocowHandling::Error),
            ("reflink", NocowHandling::Reflink),
            ("copy", NocowHandling::Copy),
            ("move-snapshot", NocowHandling::MoveSnapshot),
        ] {
            parameters
                .lists
                .insert("RSTAR_INHERITANCE_NOCOWHANDLING".into(), vec![text.into()]);
            parameters.lists.insert(
                "RSTAR_INHERITANCE_DIRECTORYSUBVOLUME".into(),
                vec!["yes".into()],
            );
            assert_eq!(
                InheritanceConfig::read(&parameters).unwrap().nocow_handling,
                policy
            );
            parameters.lists.insert(
                "RSTAR_INHERITANCE_DIRECTORYSUBVOLUME".into(),
                vec!["no".into()],
            );
            assert_eq!(
                InheritanceConfig::read(&parameters).is_err(),
                policy == NocowHandling::MoveSnapshot
            );
            assert_eq!(policy.to_string(), text);
        }
        for invalid in [
            vec![],
            vec!["".into()],
            vec!["auto".into()],
            vec!["COPY".into()],
            vec!["copy".into(), "copy".into()],
            vec!["copy\nreflink".into()],
        ] {
            parameters
                .lists
                .insert("RSTAR_INHERITANCE_NOCOWHANDLING".into(), invalid);
            assert!(InheritanceConfig::read(&parameters).is_err());
        }
    }

    #[test]
    fn directory_markers_preserve_names_and_require_matching_types() {
        for (text, directory, name) in [
            ("/home/alice/", true, "-home-alice"),
            ("/etc/hostname", false, "-etc-hostname"),
        ] {
            let path = Path::new(text);
            assert_eq!(inheritance_name(path).unwrap(), name);
            assert!(validate_inheritance_type(path, directory).is_ok());
            assert!(validate_inheritance_type(path, !directory).is_err());
        }
        for text in [
            "relative/",
            "/",
            "//",
            "/home//alice/",
            "/home/./alice/",
            "/home/alice/./",
            "/home/alice/.",
            "/home/../alice/",
            "/home/alice\n/",
        ] {
            assert!(
                inheritance_name(Path::new(text)).is_err(),
                "accepted {text:?}"
            );
        }
        assert_eq!(
            inheritance_name(Path::new("/home/alice/")).unwrap(),
            inheritance_name(Path::new("/home/alice")).unwrap(),
        );
    }

    #[test]
    fn directory_option_is_scalar_and_both_lists_allow_kernel_paths() {
        let default = InheritanceConfig::read(&PluginParameters::default()).unwrap();
        assert!(default.copy_on_write_build.is_empty());
        assert!(default.copy_on_write_boot.is_empty());
        assert!(default.directory_subvolume);
        let mut parameters = PluginParameters::default();
        parameters.lists.insert(
            "RSTAR_INHERITANCE_DIRECTORYSUBVOLUME".into(),
            vec!["no".into()],
        );
        parameters.lists.insert(
            "RSTAR_INHERITANCE_COPYONWRITEBUILD".into(),
            vec!["/boot/".into()],
        );
        parameters.lists.insert(
            "RSTAR_INHERITANCE_COPYONWRITEBOOT".into(),
            vec!["/usr/lib/modules/".into()],
        );
        let parsed = InheritanceConfig::read(&parameters).unwrap();
        assert!(!parsed.directory_subvolume);
        assert_eq!(parsed.copy_on_write_build, [PathBuf::from("/boot/")]);
        assert_eq!(
            parsed.copy_on_write_boot,
            [PathBuf::from("/usr/lib/modules/")]
        );
        for invalid in [
            vec![],
            vec!["".into()],
            vec!["invalid".into()],
            vec!["yes".into(), "no".into()],
        ] {
            parameters
                .lists
                .insert("RSTAR_INHERITANCE_DIRECTORYSUBVOLUME".into(), invalid);
            assert!(InheritanceConfig::read(&parameters).is_err());
        }
    }

    #[test]
    fn build_and_boot_lists_accept_repeated_multiline_and_empty_assignments() {
        let document = crate::config::ini::Document::parse(
            "[Inheritance]\nCopyOnWriteBuild=/etc/hostname\nCopyOnWriteBuild=\n    /opt/app/\n    /usr/local/\nCopyOnWriteBoot=/home/alice/\nCopyOnWriteBoot=\n    /var/lib/myapp/\nMount=/srv/data/\n",
            Path::new("inherit.conf"),
        ).unwrap();
        let config = InheritanceConfig::read(&document.plugin_parameters().unwrap()).unwrap();
        assert_eq!(
            config.copy_on_write_build,
            ["/etc/hostname", "/opt/app/", "/usr/local/"].map(PathBuf::from)
        );
        assert_eq!(
            config.copy_on_write_boot,
            ["/home/alice/", "/var/lib/myapp/"].map(PathBuf::from)
        );
        assert_eq!(config.mounts, [PathBuf::from("/srv/data/")]);
        for empty in ["CopyOnWriteBuild", "CopyOnWriteBoot"] {
            let document = crate::config::ini::Document::parse(
                &format!("[Inheritance]\n{empty}=\n"),
                Path::new("inherit.conf"),
            )
            .unwrap();
            let config = InheritanceConfig::read(&document.plugin_parameters().unwrap()).unwrap();
            assert!(config.copy_on_write_build.is_empty());
            assert!(config.copy_on_write_boot.is_empty());
        }
    }

    #[test]
    fn old_timing_and_unscheduled_copy_keys_require_migration() {
        for (key, value) in [
            ("RSTAR_INHERITANCE_TIMING", "build"),
            ("RSTAR_INHERITANCE_TIMING", "initramfs"),
            ("RSTAR_INHERITANCE_COPYONWRITE", "/home/alice/"),
        ] {
            let parameters = PluginParameters {
                lists: BTreeMap::from([(key.into(), vec![value.into()])]),
                ..Default::default()
            };
            let error = InheritanceConfig::read(&parameters)
                .unwrap_err()
                .to_string();
            assert!(
                error.contains("CopyOnWriteBuild") && error.contains("CopyOnWriteBoot"),
                "{error}"
            );
        }
    }

    #[test]
    fn all_lists_share_duplicate_overlap_and_encoded_name_checks() {
        let keys = [
            "RSTAR_INHERITANCE_COPYONWRITEBUILD",
            "RSTAR_INHERITANCE_COPYONWRITEBOOT",
            "RSTAR_INHERITANCE_MOUNT",
        ];
        for (index, first) in keys.iter().enumerate() {
            for second in &keys[index + 1..] {
                for (left, right) in [
                    ("/home/alice/", "/home/alice/"),
                    ("/home/", "/home/alice/"),
                    ("/home/alice/", "/home/"),
                    ("/a-b/c/", "/a/b-c/"),
                ] {
                    let parameters = PluginParameters {
                        lists: BTreeMap::from([
                            ((*first).into(), vec![left.into()]),
                            ((*second).into(), vec![right.into()]),
                        ]),
                        ..Default::default()
                    };
                    assert!(
                        InheritanceConfig::read(&parameters).is_err(),
                        "accepted {first}={left}, {second}={right}"
                    );
                }
            }
        }
    }

    /// Reject overlapping roots, lossy persistent-name collisions and protected runtime paths.
    /// 拒绝重叠根、有损持久名称冲突及受保护运行路径。
    /// # Arguments
    /// No parameters. 无参数。
    #[test]
    fn inheritance_requests_fail_before_storage_mutations() {
        for paths in [
            vec!["relative"],
            vec!["/"],
            vec!["/home/./user"],
            vec!["/home/", "/home/user/"],
            vec!["/home/user/", "/home/user"],
            vec!["/a-b/c/", "/a/b-c/"],
            vec!["/run"],
            vec!["/rstarsleep"],
            vec!["/srv/data:other"],
        ] {
            let parameters = PluginParameters {
                lists: BTreeMap::from([(
                    "RSTAR_INHERITANCE_MOUNT".into(),
                    paths.iter().map(|p| p.to_string()).collect(),
                )]),
                ..Default::default()
            };
            assert!(
                InheritanceConfig::read(&parameters).is_err(),
                "accepted {paths:?}"
            );
        }
        let parameters = PluginParameters {
            lists: BTreeMap::from([(
                "RSTAR_INHERITANCE_COPYONWRITEBUILD".into(),
                vec!["\n/home/user/\n/etc/hostname".into()],
            )]),
            ..Default::default()
        };
        assert_eq!(
            InheritanceConfig::read(&parameters)
                .unwrap()
                .copy_on_write_build,
            [PathBuf::from("/home/user/"), PathBuf::from("/etc/hostname")]
        );
    }
}
