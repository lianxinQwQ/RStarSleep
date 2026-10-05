//! Data types for the configuration fields currently supported by readers.
//! 当前配置读取接口支持的字段数据类型。

use serde::Deserialize;

use super::{Error, Result};

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
    /// Required in TOML; an explicitly empty list is valid.
    /// TOML 中必须提供此字段；显式的空列表有效。
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
