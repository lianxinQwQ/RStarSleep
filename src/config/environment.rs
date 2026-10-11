//! Resolve the controller-owned [Environment] group after normal configuration merging.
//! 在常规配置合并后解析控制器所属的 [Environment] 组。
use super::{Error, Result};
use std::{collections::BTreeMap, ffi::OsString};

/// Resolved execution variables; `None` explicitly removes a target variable.
/// 已解析的执行变量；`None` 显式移除目标变量。
pub type Environment = BTreeMap<String, Option<OsString>>;

/// Resolve merged assignments against this process without changing its environment.
/// 按本进程环境解析合并赋值，不修改进程环境。
/// # Arguments
/// - `assignments`: Case-sensitive names and original assignment lists.
///   大小写敏感的名称及原始赋值列表。
pub(super) fn resolve(assignments: &BTreeMap<String, Vec<String>>) -> Result<Environment> {
    resolve_with(assignments, |name| std::env::var_os(name))
}

/// Accept portable environment names, including a leading underscore.
/// 接受可移植环境变量名，允许下划线开头。
/// # Arguments
/// - `name`: Target or source environment variable name. 目标或来源环境变量名。
fn valid_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    bytes
        .next()
        .is_some_and(|b| b.is_ascii_alphabetic() || b == b'_')
        && bytes.all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// Resolve once through an injectable lookup, preserving inherited native values.
/// 通过可注入查询一次性解析，保留继承的原生值。
/// # Arguments
/// - `assignments`: Merged assignment lists. 合并后的赋值列表。
/// - `lookup`: Source-variable reader. 来源变量读取函数。
fn resolve_with(
    assignments: &BTreeMap<String, Vec<String>>,
    mut lookup: impl FnMut(&str) -> Option<OsString>,
) -> Result<Environment> {
    let mut environment = Environment::new();
    for (name, values) in assignments {
        if !valid_name(name) {
            return Err(Error::Invalid(format!(
                "[Environment] 无效环境变量名: {name:?}"
            )));
        }
        if name.starts_with("RSTAR_") || name.starts_with("RSTAT_") {
            return Err(Error::Invalid(format!(
                "[Environment] 环境变量名由控制器保留: {name}"
            )));
        }
        let [value] = values.as_slice() else {
            return Err(Error::Invalid(format!(
                "[Environment] {name} 必须只赋值一次"
            )));
        };
        let resolved = if let Some(literal) = value.strip_prefix("$$") {
            Some(OsString::from(format!("${literal}")))
        } else if let Some(source) = value.strip_prefix('$') {
            if !valid_name(source) {
                return Err(Error::Invalid(format!(
                    "[Environment] {name} 无效继承环境变量名: {source:?}"
                )));
            }
            lookup(source)
        } else {
            Some(OsString::from(value))
        };
        if resolved
            .as_ref()
            .is_some_and(|value| value.as_encoded_bytes().contains(&0))
        {
            return Err(Error::Invalid(format!(
                "[Environment] 环境变量 {name} 包含 NUL"
            )));
        }
        environment.insert(name.clone(), resolved);
    }
    Ok(environment)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Construct raw assignment lists for resolution tests.
    /// 为解析测试创建原始赋值列表。
    /// # Arguments
    /// - `input`: Variable names and values. 变量名称及值。
    fn assignments(input: &[(&str, &[&str])]) -> BTreeMap<String, Vec<String>> {
        input
            .iter()
            .map(|(name, values)| {
                (
                    name.to_string(),
                    values.iter().map(|v| v.to_string()).collect(),
                )
            })
            .collect()
    }

    /// Preserve literal syntax, empty values and missing inheritance.
    /// 保留字面值语法、空值及缺失继承状态。
    /// # Arguments
    /// No parameters. 无参数。
    #[test]
    fn resolves_literals_and_inheritance_without_shell_interpretation() {
        let values = resolve_with(
            &assignments(&[
                ("http_proxy", &["http://127.0.0.1:10808"]),
                ("HTTP_PROXY", &["$proxy"]),
                ("MISSING", &["$absent"]),
                ("EMPTY", &[""]),
                ("DOLLAR", &["$$proxy"]),
                ("PERCENT", &["%proxy"]),
                ("RAW", &["a=b ; $(touch sentinel) 'quoted'\ncontinued"]),
            ]),
            |name| (name == "proxy").then(|| "socks5h://localhost:1080".into()),
        )
        .unwrap();
        assert_eq!(values["http_proxy"], Some("http://127.0.0.1:10808".into()));
        assert_eq!(
            values["HTTP_PROXY"],
            Some("socks5h://localhost:1080".into())
        );
        assert_eq!(values["MISSING"], None);
        assert_eq!(values["EMPTY"], Some("".into()));
        assert_eq!(values["DOLLAR"], Some("$proxy".into()));
        assert_eq!(values["PERCENT"], Some("%proxy".into()));
        assert_eq!(
            values["RAW"],
            Some("a=b ; $(touch sentinel) 'quoted'\ncontinued".into())
        );
    }

    /// Reject invalid names, duplicated scalars and process-incompatible values.
    /// 拒绝无效名称、重复标量及进程不支持的值。
    /// # Arguments
    /// No parameters. 无参数。
    #[test]
    fn rejects_invalid_names_repetitions_and_nul() {
        for input in [
            assignments(&[("1NAME", &["x"])]),
            assignments(&[("A-B", &["x"])]),
            assignments(&[("RSTAR_ROOT", &["/"])]),
            assignments(&[("RSTAT_ROOT", &["/"])]),
            assignments(&[("A", &["$"])]),
            assignments(&[("A", &["$bad-name"])]),
            assignments(&[("A", &["a\0b"])]),
            assignments(&[("A", &["1", "2"])]),
        ] {
            assert!(resolve_with(&input, |_| None).is_err());
        }
        assert!(
            resolve_with(&assignments(&[("A", &["$SOURCE"])]), |_| Some(
                "bad\0value".into()
            ))
            .is_err()
        );
    }

    /// Retain inherited bytes without lossy Unicode conversion.
    /// 保留继承字节，不进行有损 Unicode 转换。
    /// # Arguments
    /// No parameters. 无参数。
    #[cfg(unix)]
    #[test]
    fn inherited_values_preserve_native_bytes() {
        use std::os::unix::ffi::OsStringExt;
        let native = OsString::from_vec(vec![b'x', 0xff]);
        let values = resolve_with(&assignments(&[("A", &["$SOURCE"])]), |_| {
            Some(native.clone())
        })
        .unwrap();
        assert_eq!(values["A"], Some(native));
    }
}
