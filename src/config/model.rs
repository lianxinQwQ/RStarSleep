//! Data types for the configuration fields currently supported by readers.
//! 当前配置读取接口支持的字段数据类型。

use serde::Deserialize;

use super::{Error, Result, validate_identifier};

/// System identity used to select a distribution plugin from the workspace.
/// 用于从工作目录选择发行版插件的系统类型配置。
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct SystemTypeConfig {
    /// Distribution type matched against plugin metadata, such as `arch`.
    /// 与插件元数据匹配的发行版类型，例如 `arch`。
    #[serde(alias = "distribution")]
    pub system_type: String,
    /// Optional target architecture; omission uses the host architecture.
    /// 可选目标架构；省略时使用宿主架构。
    pub architecture: Option<String>,
}

impl SystemTypeConfig {
    /// Validate the system type and any explicitly selected architecture.
    /// 校验系统类型和显式指定的目标架构。
    ///
    /// # Arguments
    ///
    /// - `self`: Configuration selecting a plugin and its target architecture.
    ///   用于选择插件及其目标架构的配置。
    ///
    /// # Errors
    ///
    /// Returns an error if either value is not a valid identifier.
    /// 任一值不符合标识规则时返回错误。
    pub fn validate(&self) -> Result<()> {
        validate_identifier(&self.system_type)?;
        if let Some(architecture) = &self.architecture {
            validate_identifier(architecture)?;
        }
        Ok(())
    }
}

/// Package options read from the build configuration.
/// 从构建配置中读取的软件包选项。
///
/// Unknown fields are ignored until their corresponding features are implemented.
/// 其他字段暂不解释，待对应功能实现时再添加读取逻辑。
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct PackageConfig {
    /// Requested packages in their original order, including duplicate entries.
    /// 按原始顺序保存的待安装软件包列表，包含重复项。
    ///
    /// Required as [Content] Packages in INI or packages in legacy TOML; an empty list is valid.
    /// INI 中必须提供 [Content] Packages，旧 TOML 中为 packages；显式的空列表有效。
    pub packages: Vec<String>,
}

impl PackageConfig {
    /// Check package values without modifying them or querying a repository.
    /// 校验软件包值，不修改其内容，也不查询软件包仓库。
    ///
    /// # Arguments
    ///
    /// - `self`: Package configuration whose package names are checked without modification.
    ///   待校验的软件包配置；校验过程不修改包名。
    ///
    /// # Errors
    ///
    /// Returns [`Error::Invalid`] if any package name is empty or whitespace only.
    /// 任意包名为空或仅包含空白字符时，返回 [`Error::Invalid`]。
    pub fn validate(&self) -> Result<()> {
        if self
            .packages
            .iter()
            .any(|package| package.trim().is_empty())
        {
            return Err(Error::Invalid("软件包名称不能为空".into()));
        }
        Ok(())
    }
}
