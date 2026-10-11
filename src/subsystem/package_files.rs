//! Validate optional plugin package-file indices as bounded candidate lists.
//! 将可选插件包文件索引校验为有界候选列表。
use super::{Error, Result};
use std::{
    collections::BTreeSet,
    path::{Component, Path},
};
pub(crate) const MAX_BYTES: usize = 64 * 1024 * 1024;

/// Parse owner/path records without stripping spaces from filenames.
/// 解析所属包及路径记录，不移除文件名中的空格。
/// # Arguments
/// - `bytes`: UTF-8 TSV from the target plugin. 目标插件提供的 UTF-8 TSV。
pub(crate) fn parse(bytes: &[u8]) -> Result<BTreeSet<(String, String)>> {
    if bytes.len() > MAX_BYTES {
        return Err(Error::Invalid("package file index too large".into()));
    }
    let text = std::str::from_utf8(bytes)
        .map_err(|_| Error::Invalid("package file index is not UTF-8".into()))?;
    let mut result = BTreeSet::new();
    for line in text.split_terminator('\n') {
        let (package, path) = line
            .split_once('\t')
            .ok_or_else(|| Error::Invalid("package index requires owner TAB path".into()))?;
        crate::software::valid_name(package).map_err(|e| Error::Invalid(e.to_string()))?;
        if line.len() > 8192
            || path.contains(['\t', '\r', '\0'])
            || !Path::new(path).is_absolute()
            || path.contains("//")
            || path.contains("/./")
            || path.ends_with("/.")
            || Path::new(path)
                .components()
                .any(|p| !matches!(p, Component::RootDir | Component::Normal(_)))
        {
            return Err(Error::Invalid("invalid package index path".into()));
        }
        result.insert((package.into(), path.into()));
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    /// Preserve spaces and multiple owners; reject malformed records.
    /// 保留空格及多个所属包；拒绝格式错误记录。
    /// # Arguments
    /// No parameters. 无参数。
    #[test]
    fn strict_tsv() {
        let index =
            parse(b"a\t/usr/share/a file\nb\t/usr/share/a file\na\t/usr/share/a file\n").unwrap();
        assert_eq!(index.len(), 2);
        for bytes in [
            b"a /x\n".as_slice(),
            b"a\t/../host\n",
            b"a\t/x\tother\n",
            b"a\t/x\n\n",
            b"a\trelative\n",
        ] {
            assert!(parse(bytes).is_err());
        }
    }
}
