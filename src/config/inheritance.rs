//! Read inheritance independently of mkosi-style build options from merged INI inputs.
//! 从合并后的 INI 输入独立读取继承选项，不作为 mkosi 构建选项处理。
use super::{Error, PluginParameters, Result};
const DEFAULT_MANAGER: &str = "/rstarsleep";
use std::{
    collections::BTreeMap,
    path::{Component, Path, PathBuf},
};

/// Current-running-system inheritance requests.
/// 当前运行系统的继承请求。
#[derive(Debug, Default)]
pub struct InheritanceConfig {
    /// Absolute paths copied into every version from the running system.
    /// 每个版本从运行系统复制的绝对路径。
    pub copy_on_write: Vec<PathBuf>,
    /// Absolute paths backed by this system's persistent objects.
    /// 由本系统持久对象支持的绝对路径。
    pub mounts: Vec<PathBuf>,
}

/// Encode every slash, including the leading one, in persistent object names.
/// 对持久对象名称中的全部斜杠编码，包括开头的斜杠。
/// # Arguments
/// - `path`: Normalized absolute inheritance source. 规范绝对继承来源。
/// # Errors
/// Rejects non-UTF-8, non-normalized, relative or root paths.
/// 拒绝非 UTF-8、非规范、相对或根路径。
pub fn inheritance_name(path: &Path) -> Result<String> {
    let text = path
        .to_str()
        .ok_or_else(|| Error::Invalid("继承路径必须为 UTF-8".into()))?;
    if !path.is_absolute()
        || path == Path::new("/")
        || text.chars().any(char::is_control)
        || path
            .components()
            .any(|part| !matches!(part, Component::RootDir | Component::Normal(_)))
        || text.contains("//")
        || text.contains("/./")
        || text.ends_with("/.")
        || text.ends_with('/')
    {
        return Err(Error::Invalid(format!(
            "继承项目必须为规范绝对路径且不能为 /: {text:?}"
        )));
    }
    Ok(text.replace('/', "-"))
}

impl InheritanceConfig {
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
            let target = match key.as_str() {
                "RSTAR_INHERITANCE_COPYONWRITE" => &mut config.copy_on_write,
                "RSTAR_INHERITANCE_MOUNT" => &mut config.mounts,
                _ => return Err(Error::Invalid(format!("未知继承配置: {key}"))),
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
                            "Mount 路径与管理器运行环境冲突或含不支持的绑定分隔符: {line}"
                        )));
                    }
                    if let Some(previous) = names.insert(name, path.clone()) {
                        return Err(Error::Invalid(format!(
                            "继承路径重复或编码冲突: {} 与 {}",
                            previous.display(),
                            path.display()
                        )));
                    }
                    target.push(path);
                }
            }
        }
        let paths: Vec<_> = config.copy_on_write.iter().chain(&config.mounts).collect();
        for (index, path) in paths.iter().enumerate() {
            for other in &paths[index + 1..] {
                if path.starts_with(other) || other.starts_with(path) {
                    return Err(Error::Invalid(format!(
                        "继承路径不能重叠: {} 与 {}",
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
            vec!["/home", "/home/user"],
            vec!["/a-b/c", "/a/b-c"],
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
                "RSTAR_INHERITANCE_COPYONWRITE".into(),
                vec!["\n/home/user\n/etc/hostname".into()],
            )]),
            ..Default::default()
        };
        assert_eq!(
            InheritanceConfig::read(&parameters).unwrap().copy_on_write,
            [PathBuf::from("/home/user"), PathBuf::from("/etc/hostname")]
        );
    }
}
