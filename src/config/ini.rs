//! Parse mkosi-style sections, repeated assignments, and indented continuation lines.
//! 解析 mkosi 风格的分节、重复赋值及缩进续行。

use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

use super::{Error, PackageConfig, Result, SystemTypeConfig};

/// One assignment, including its source location for diagnostics.
/// 单次赋值，保留源位置以提供诊断。
struct Assignment {
    /// Case-sensitive section name.
    /// 大小写敏感的分节名。
    section: String,
    /// Case-sensitive setting name.
    /// 大小写敏感的设置名。
    key: String,
    /// Trimmed value including indented continuation lines.
    /// 已去除首尾空白的值，包括缩进续行。
    value: String,
    /// One-based assignment line.
    /// 从 1 开始的赋值行号。
    line: usize,
}

/// Parsed build settings with assignment order preserved.
/// 保留赋值顺序的构建设置。
pub(super) struct Document {
    /// Source path retained for diagnostics.
    /// 为诊断保留的源路径。
    path: PathBuf,
    /// Declared sections, including sections with no settings.
    /// 已声明分节，包括没有设置的分节。
    sections: BTreeSet<String>,
    /// Assignments in source order, including repeated keys and sections.
    /// 按源顺序保存的赋值，包括重复键和分节。
    assignments: Vec<Assignment>,
}

impl Document {
    /// Parse sectioned settings; indentation extends the previous value and # starts a comment.
    /// 解析分节设置；缩进延续上一值，# 开始注释。
    /// # Arguments
    /// - `text`: UTF-8 INI contents.
    ///   UTF-8 INI 内容。
    /// - `path`: Source file used in diagnostics.
    ///   用于诊断的源文件。
    pub(super) fn parse(text: &str, path: &Path) -> Result<Self> {
        let mut document = Self {
            path: path.into(),
            sections: BTreeSet::new(),
            assignments: Vec::new(),
        };
        let mut section = String::new();
        let mut previous = None;
        let margin = indentation(text);
        for (index, original) in text.lines().enumerate() {
            let line = index + 1;
            let original = original.strip_prefix(margin).unwrap_or(original);
            let content = original.split('#').next().unwrap_or_default();
            let value = content.trim();
            if value.is_empty() {
                continue;
            }
            if content.starts_with(char::is_whitespace)
                && let Some(index) = previous
            {
                let entry: &mut Assignment = &mut document.assignments[index];
                entry.value.push('\n');
                entry.value.push_str(value);
                continue;
            }
            previous = None;
            if value.starts_with('[') {
                section = value
                    .strip_prefix('[')
                    .and_then(|s| s.strip_suffix(']'))
                    .filter(|s| !s.trim().is_empty() && !s.contains(['[', ']']))
                    .ok_or_else(|| document.error(line, "分节标题必须为 [Section]"))?
                    .trim()
                    .into();
                document.sections.insert(section.clone());
                continue;
            }
            if section.is_empty() {
                return Err(document.error(line, "设置必须位于分节内"));
            }
            let (key, value) = value
                .split_once('=')
                .ok_or_else(|| document.error(line, "设置必须包含 ="))?;
            let key = key.trim();
            if key.is_empty() || key.chars().any(char::is_whitespace) {
                return Err(document.error(line, "设置名称不能为空或包含空白"));
            }
            document.assignments.push(Assignment {
                section: section.clone(),
                key: key.into(),
                value: value.trim().into(),
                line,
            });
            previous = Some(document.assignments.len() - 1);
        }
        Ok(document)
    }

    /// Attach the INI path and line number to a parsing or value error.
    /// 为解析或值错误附加 INI 路径及行号。
    /// # Arguments
    /// - `self`: Document owning the source path.
    ///   持有源路径的文档。
    /// - `line`: One-based source line; zero denotes a missing setting.
    ///   从 1 开始的源行号；0 表示缺失设置。
    /// - `message`: Error description.
    ///   错误说明。
    fn error(&self, line: usize, message: &str) -> Error {
        Error::Ini {
            path: self.path.clone(),
            line,
            message: message.into(),
        }
    }

    /// Resolve a scalar using the last assignment, as mkosi does.
    /// 与 mkosi 一样，使用最后一次赋值解析标量。
    /// # Arguments
    /// - `self`: Ordered build settings.
    ///   有序构建设置。
    /// - `section`: Case-sensitive section name.
    ///   大小写敏感的分节名。
    /// - `key`: Case-sensitive setting name.
    ///   大小写敏感的设置名。
    fn scalar(&self, section: &str, key: &str) -> Option<&Assignment> {
        self.assignments
            .iter()
            .rev()
            .find(|a| a.section == section && a.key == key)
    }

    /// Require a nonempty scalar value with its section named in errors.
    /// 要求非空标量值，错误中包含所属分节。
    /// # Arguments
    /// - `self`: Parsed build settings.
    ///   已解析构建设置。
    /// - `section`: Required section.
    ///   必需分节。
    /// - `key`: Required setting.
    ///   必需设置。
    fn required(&self, section: &str, key: &str) -> Result<&Assignment> {
        let entry = self
            .scalar(section, key)
            .ok_or_else(|| self.error(0, &format!("缺少 [{section}] {key}")))?;
        if entry.value.is_empty() {
            return Err(self.error(entry.line, &format!("[{section}] {key} 不能为空")));
        }
        Ok(entry)
    }

    /// Read the supported build schema without interpreting other settings.
    /// 读取支持的构建格式版本，不解释其他设置。
    /// # Arguments
    /// - `self`: Parsed build settings.
    ///   已解析构建设置。
    pub(super) fn schema_version(&self) -> Result<u32> {
        let entry = self.required("Config", "SchemaVersion")?;
        entry
            .value
            .parse()
            .map_err(|_| self.error(entry.line, "[Config] SchemaVersion 必须是整数"))
    }

    /// Read plugin selection independently of packages and the schema marker.
    /// 独立于软件包和格式标记读取插件选择。
    /// # Arguments
    /// - `self`: Parsed build settings.
    ///   已解析构建设置。
    pub(super) fn system_type(&self) -> Result<SystemTypeConfig> {
        let distribution = self.required("Distribution", "Distribution")?;
        super::validate_identifier(&distribution.value)
            .map_err(|e| self.error(distribution.line, &e.to_string()))?;
        let architecture = self.scalar("Distribution", "Architecture");
        if let Some(entry) = architecture {
            super::validate_identifier(&entry.value)
                .map_err(|e| self.error(entry.line, &e.to_string()))?;
        }
        Ok(SystemTypeConfig {
            system_type: distribution.value.clone(),
            architecture: architecture.map(|a| a.value.clone()),
        })
    }

    /// Read the last value of every plugin setting, excluding the distribution selector.
    /// 读取每个插件设置的最后一次赋值，排除发行版选择字段。
    /// # Arguments
    /// - `self`: Parsed configuration retaining case-sensitive setting names.
    ///   保留大小写敏感设置名的已解析配置。
    pub(super) fn distribution_options(&self) -> BTreeMap<String, String> {
        self.assignments
            .iter()
            .filter(|entry| entry.section == "Distribution" && entry.key != "Distribution")
            .map(|entry| (entry.key.clone(), entry.value.clone()))
            .collect()
    }

    /// Append package assignments, resetting on empty values, and split on commas or whitespace.
    /// 追加软件包赋值，空值清空列表，并按逗号或空白分隔。
    /// # Arguments
    /// - `self`: Parsed build settings preserving assignment order.
    ///   保留赋值顺序的已解析构建设置。
    pub(super) fn packages(&self) -> Result<PackageConfig> {
        let mut packages = Vec::new();
        let mut found = false;
        for entry in self
            .assignments
            .iter()
            .filter(|a| a.section == "Content" && a.key == "Packages")
        {
            found = true;
            let names: Vec<_> = entry
                .value
                .split(|c: char| c.is_whitespace() || c == ',')
                .filter(|s| !s.is_empty())
                .collect();
            if names.is_empty() {
                packages.clear();
            }
            for name in names {
                crate::software::valid_name(name)
                    .map_err(|e| self.error(entry.line, &e.to_string()))?;
                packages.push(name.into());
            }
        }
        if !found {
            return Err(self.error(0, "缺少 [Content] Packages；空列表请显式写 Packages="));
        }
        Ok(PackageConfig { packages })
    }

    /// Normalize parsed inputs for provenance, optionally omitting all package assignments.
    /// 规范化解析输入以记录来源，可选忽略全部软件包赋值。
    /// # Arguments
    /// - `self`: Complete parsed document, including unknown settings.
    ///   完整解析文档，包括未知设置。
    /// - `exclude_packages`: Whether package changes remain cache-compatible.
    ///   软件包变化是否保持缓存兼容。
    pub(super) fn normalized(&self, exclude_packages: bool) -> Result<Vec<u8>> {
        let mut values: BTreeMap<(&str, &str), Vec<&str>> = BTreeMap::new();
        for entry in &self.assignments {
            if entry.section == "Content" && entry.key == "Packages" {
                continue;
            }
            values
                .entry((&entry.section, &entry.key))
                .or_default()
                .push(&entry.value);
        }
        let packages;
        if !exclude_packages && self.scalar("Content", "Packages").is_some() {
            packages = self.packages()?;
            values.insert(
                ("Content", "Packages"),
                packages.packages.iter().map(String::as_str).collect(),
            );
        }
        let mut bytes = b"rstarsleep-ini-v1".to_vec();
        let sections: Vec<_> = self
            .sections
            .iter()
            .filter(|section| {
                !exclude_packages
                    || section.as_str() != "Content"
                    || values.keys().any(|(section, _)| *section == "Content")
            })
            .collect();
        bytes.extend_from_slice(&(sections.len() as u64).to_le_bytes());
        for section in sections {
            field(&mut bytes, section);
        }
        for ((section, key), values) in values {
            field(&mut bytes, section);
            field(&mut bytes, key);
            bytes.extend_from_slice(&(values.len() as u64).to_le_bytes());
            for value in values {
                field(&mut bytes, value);
            }
        }
        Ok(bytes)
    }
}

/// Find common leading spaces and tabs, matching mkosi's dedented configuration parsing.
/// 查找公共前导空格及制表符，对齐 mkosi 去除统一缩进后的配置解析。
/// # Arguments
/// - `text`: Complete INI contents, including blank lines.
///   完整 INI 内容，包括空行。
fn indentation(text: &str) -> &str {
    let mut common: Option<&str> = None;
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        let prefix = &line[..line.len() - line.trim_start_matches([' ', '\t']).len()];
        common = Some(match common {
            None => prefix,
            Some(previous) => {
                let length = previous
                    .bytes()
                    .zip(prefix.bytes())
                    .take_while(|(a, b)| a == b)
                    .count();
                &previous[..length]
            }
        });
    }
    common.unwrap_or_default()
}

/// Encode one string with an unambiguous length boundary.
/// 编码单个字符串并附加明确的长度边界。
/// # Arguments
/// - `bytes`: Accumulated canonical input.
///   累积的规范化输入。
/// - `value`: Next UTF-8 value.
///   下一个 UTF-8 值。
fn field(bytes: &mut Vec<u8>, value: &str) {
    bytes.extend_from_slice(&(value.len() as u64).to_le_bytes());
    bytes.extend_from_slice(value.as_bytes());
}
