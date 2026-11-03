//! Apply explicit locale policy and prepare system-wide timezone data.
//! 应用显式语言策略并准备系统级时区数据。
use super::{Error, Result};
use crate::config::resolve_environment_with;
use std::{collections::BTreeMap, fs, io::Read, os::unix::fs::OpenOptionsExt, path::Path};

const MAX_TIMEZONE_BYTES: u64 = 1024 * 1024;

/// Recognize locale variables collected from the authenticated caller.
/// 识别从已认证调用者采集的语言环境变量。
/// # Arguments
/// - `name`: Case-sensitive variable name. 大小写敏感的变量名。
fn locale(name: &str) -> bool {
    matches!(name, "LANG" | "LANGUAGE")
        || (name.starts_with("LC_")
            && name.len() > 3
            && name.bytes().all(|b| b.is_ascii_uppercase() || b == b'_'))
}

/// Validate the supported runtime subset using the build assignment parser.
/// 使用构建赋值解析器校验支持的运行时子集。
/// # Arguments
/// - `settings`: Raw subsystem Environment table. 原始子系统 Environment 表。
pub(super) fn validate(settings: &BTreeMap<String, String>) -> Result<()> {
    let assignments = settings
        .iter()
        .map(|(name, value)| (name.clone(), vec![value.clone()]))
        .collect();
    resolve_environment_with(&assignments, |_| None)?;
    for (name, value) in settings {
        if name != "TZ" && !locale(name) {
            return Err(Error::Invalid(format!(
                "[Environment] 仅支持 LANG、LANGUAGE、LC_* 和 TZ: {name} / [Environment] supports only LANG, LANGUAGE, LC_* and TZ: {name}"
            )));
        }
        if let Some(source) = value.strip_prefix('$').filter(|s| !s.starts_with('$')) {
            if (name == "TZ" && source != "TZ") || (name != "TZ" && !locale(source)) {
                return Err(Error::Invalid(format!(
                    "[Environment] {name} 的继承来源不受支持: {source} / unsupported inheritance source for [Environment] {name}: {source}"
                )));
            }
        }
        if name == "TZ" && value != "$TZ" {
            timezone_name(value)?;
        }
    }
    Ok(())
}

/// Resolve only configured locale variables against caller hints, without shell evaluation.
/// 仅按调用者提示解析已配置的语言变量，不经 shell 求值。
/// # Arguments
/// - `settings`, `caller`: Validated policy and authenticated caller hints. 经校验策略及认证调用者提示。
pub(super) fn language(
    settings: &BTreeMap<String, String>,
    caller: &BTreeMap<String, String>,
) -> Result<BTreeMap<String, String>> {
    validate(settings)?;
    let assignments = settings
        .iter()
        .filter(|(name, _)| locale(name))
        .map(|(name, value)| (name.clone(), vec![value.clone()]))
        .collect();
    resolve_environment_with(&assignments, |name| caller.get(name).map(Into::into))?
        .into_iter()
        .filter_map(|(name, value)| value.map(|v| (name, v)))
        .map(|(name, value)| {
            value.into_string().map(|value| (name, value)).map_err(|_| {
                Error::Invalid("语言环境不是 UTF-8 / locale environment is not UTF-8".into())
            })
        })
        .collect()
}

/// Accept named timezone database entries without absolute paths or traversal.
/// 接受命名时区数据库条目，拒绝绝对路径和目录跳转。
/// # Arguments
/// - `name`: Literal timezone identifier. 字面时区标识。
fn timezone_name(name: &str) -> Result<()> {
    if name.is_empty()
        || !name.split('/').all(|part| {
            !part.is_empty()
                && !matches!(part, "." | "..")
                && part
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_+-".contains(&b))
        })
    {
        return Err(Error::Invalid(format!(
            "时区必须是有效的数据库名称: {name:?} / timezone must be a valid database name: {name:?}"
        )));
    }
    Ok(())
}

/// Build a fixed-offset UTC TZif file without depending on host timezone packages.
/// 生成固定 UTC 偏移的 TZif 文件，不依赖宿主时区软件包。
fn utc() -> Vec<u8> {
    let mut bytes = b"TZif\0".to_vec();
    bytes.resize(20, 0);
    for count in [0_u32, 0, 0, 0, 1, 4] {
        bytes.extend_from_slice(&count.to_be_bytes());
    }
    bytes.extend_from_slice(&[0; 6]);
    bytes.extend_from_slice(b"UTC\0");
    bytes
}

/// Freeze system timezone bytes before any version finalization or runtime publication.
/// 在版本收尾或运行发布前冻结系统时区数据。
/// # Arguments
/// - `settings`: Validated subsystem Environment table. 经校验的子系统 Environment 表。
pub(super) fn timezone(settings: &BTreeMap<String, String>) -> Result<Vec<u8>> {
    timezone_at(settings, Path::new("/"))
}

/// Read timezone data through an injectable host root, rejecting missing or malformed files.
/// 通过可注入宿主根读取时区数据，拒绝缺失或损坏文件。
/// # Arguments
/// - `settings`, `host`: Runtime policy and host filesystem root. 运行策略与宿主文件系统根。
fn timezone_at(settings: &BTreeMap<String, String>, host: &Path) -> Result<Vec<u8>> {
    validate(settings)?;
    let name = settings.get("TZ").map(String::as_str).unwrap_or("UTC");
    if name == "UTC" {
        return Ok(utc());
    }
    let path = if name == "$TZ" {
        host.join("etc/localtime")
    } else {
        host.join("usr/share/zoneinfo").join(name)
    };
    let mut file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(&path)
        .map_err(|source| Error::Io {
            path: path.clone(),
            source,
        })?;
    if !file.metadata()?.is_file() {
        return Err(Error::Invalid(format!(
            "时区来源必须是普通文件: {} / timezone source must be a regular file: {}",
            path.display(),
            path.display()
        )));
    }
    let mut bytes = Vec::new();
    (&mut file)
        .take(MAX_TIMEZONE_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_TIMEZONE_BYTES as usize || !valid_tzif(&bytes) {
        return Err(Error::Invalid(format!(
            "时区来源不是有效的 TZif 文件: {} / timezone source is not a valid TZif file: {}",
            path.display(),
            path.display()
        )));
    }
    Ok(bytes)
}

/// Check both TZif data blocks and the extended-format footer before using a source.
/// 使用来源前检查两个 TZif 数据块及扩展格式尾部。
/// # Arguments
/// - `bytes`: Bounded timezone file. 大小受限的时区文件。
fn valid_tzif(bytes: &[u8]) -> bool {
    let Some(end) = tzif_block(bytes, 4) else {
        return false;
    };
    if bytes[4] == 0 {
        return end == bytes.len();
    }
    let extended = &bytes[end..];
    let Some(end) = tzif_block(extended, 8) else {
        return false;
    };
    let footer = &extended[end..];
    extended[4] == bytes[4]
        && footer.len() >= 2
        && footer.first() == Some(&b'\n')
        && footer.last() == Some(&b'\n')
        && footer[1..footer.len() - 1]
            .iter()
            .all(|b| b.is_ascii_graphic())
}

/// Bound TZif offsets and verify type indices, abbreviations and boolean indicators.
/// 限制 TZif 偏移，并验证类型索引、缩写及布尔指示位。
/// # Arguments
/// - `bytes`, `time_bytes`: Header/data bytes and timestamp width. 头部及数据字节、时间戳宽度。
fn tzif_block(bytes: &[u8], time_bytes: u64) -> Option<usize> {
    if bytes.len() < 44 || &bytes[..4] != b"TZif" || !matches!(bytes[4], 0 | b'2' | b'3' | b'4') {
        return None;
    }
    let counts: Vec<_> = bytes[20..44]
        .chunks_exact(4)
        .map(|v| u32::from_be_bytes(v.try_into().unwrap()) as u64)
        .collect();
    let [gmt, std, leaps, transitions, types, chars] = counts[..] else {
        return None;
    };
    let indices = 44 + transitions * time_bytes;
    let records = indices + transitions;
    let abbreviations = records + types * 6;
    let indicators = abbreviations + chars + leaps * (time_bytes + 4);
    let length = indicators + std + gmt;
    if types == 0
        || types > 256
        || chars == 0
        || length > bytes.len() as u64
        || (std != 0 && std != types)
        || (gmt != 0 && gmt != types)
    {
        return None;
    }
    let abbreviations = abbreviations as usize;
    let names = &bytes[abbreviations..abbreviations + chars as usize];
    if bytes[indices as usize..records as usize]
        .iter()
        .any(|i| u64::from(*i) >= types)
        || bytes[records as usize..abbreviations]
            .chunks_exact(6)
            .any(|r| r[4] > 1 || u64::from(r[5]) >= chars || !names[r[5] as usize..].contains(&0))
        || bytes[indicators as usize..length as usize]
            .iter()
            .any(|b| *b > 1)
    {
        return None;
    }
    Some(length as usize)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Require explicit locale policy and preserve build assignment semantics.
    /// 要求显式语言策略，并保留构建赋值语义。
    #[test]
    fn locale_defaults_literals_and_inheritance() {
        let caller = BTreeMap::from([
            ("LANG".into(), "zh_CN.UTF-8".into()),
            ("LANGUAGE".into(), "zh_CN:en".into()),
            ("LC_ALL".into(), "C".into()),
        ]);
        assert!(language(&BTreeMap::new(), &caller).unwrap().is_empty());
        let settings = BTreeMap::from([
            ("LANG".into(), "$LANG".into()),
            ("LANGUAGE".into(), "$LANGUAGE".into()),
            ("LC_MESSAGES".into(), "en_US.UTF-8".into()),
            ("LC_CTYPE".into(), "$LC_CTYPE".into()),
            ("LC_TIME".into(), "".into()),
            ("LC_NUMERIC".into(), "$$literal".into()),
            ("TZ".into(), "Asia/Shanghai".into()),
        ]);
        assert_eq!(
            language(&settings, &caller).unwrap(),
            BTreeMap::from([
                ("LANG".into(), "zh_CN.UTF-8".into()),
                ("LANGUAGE".into(), "zh_CN:en".into()),
                ("LC_MESSAGES".into(), "en_US.UTF-8".into()),
                ("LC_TIME".into(), "".into()),
                ("LC_NUMERIC".into(), "$literal".into()),
            ])
        );
    }

    /// Reject unsupported variables and timezone syntax before preparation.
    /// 准备前拒绝不支持的变量及无效时区语法。
    #[test]
    fn rejects_unsupported_environment_and_timezones() {
        for (name, value) in [
            ("PATH", "/host/bin"),
            ("LD_PRELOAD", "/host.so"),
            ("LANG", "$PATH"),
            ("LANG", "$bad-name"),
            ("LANG", "a\0b"),
            ("TZ", "$LANG"),
            ("TZ", ""),
            ("TZ", "/etc/localtime"),
            ("TZ", "../UTC"),
            ("TZ", "Asia//Shanghai"),
            ("TZ", "Asia/./Shanghai"),
            ("TZ", "Asia/Shanghai\n"),
        ] {
            assert!(
                validate(&BTreeMap::from([(name.into(), value.into())])).is_err(),
                "{name}={value}"
            );
        }
    }

    /// UTC needs no database; inheritance and named zones read their exact sources.
    /// UTC 无需数据库；继承和指定时区读取其准确来源。
    #[test]
    fn timezone_defaults_and_sources_fail_closed() {
        let host = tempfile::tempdir().unwrap();
        let default = timezone_at(&BTreeMap::new(), host.path()).unwrap();
        assert!(valid_tzif(&default));
        assert_eq!(&default[44..48], &0_i32.to_be_bytes());
        let inherited = BTreeMap::from([("TZ".into(), "$TZ".into())]);
        let named = BTreeMap::from([("TZ".into(), "Asia/Shanghai".into())]);
        assert!(timezone_at(&inherited, host.path()).is_err());
        assert!(timezone_at(&named, host.path()).is_err());
        let mut shanghai = default.clone();
        shanghai[44..48].copy_from_slice(&28800_i32.to_be_bytes());
        shanghai[50..].copy_from_slice(b"CST\0");
        fs::create_dir(host.path().join("etc")).unwrap();
        fs::create_dir_all(host.path().join("usr/share/zoneinfo/Asia")).unwrap();
        fs::write(host.path().join("etc/localtime"), &shanghai).unwrap();
        let zone = host.path().join("usr/share/zoneinfo/Asia/Shanghai");
        fs::write(&zone, &shanghai).unwrap();
        assert_eq!(timezone_at(&inherited, host.path()).unwrap(), shanghai);
        assert_eq!(timezone_at(&named, host.path()).unwrap(), shanghai);
        fs::write(&zone, b"invalid timezone").unwrap();
        assert!(timezone_at(&named, host.path()).is_err());
        fs::write(&zone, &default[..44]).unwrap();
        assert!(timezone_at(&named, host.path()).is_err());
        let mut extended = default.clone();
        extended[4] = b'2';
        assert!(!valid_tzif(&extended));
        extended.extend_from_within(..);
        extended.extend_from_slice(b"\nUTC0\n");
        assert!(valid_tzif(&extended));
        extended[49] = 255;
        assert!(!valid_tzif(&extended));
    }

    /// Validate installed real TZif versions in addition to the minimal deterministic fixtures.
    /// 除最小确定性夹具外，验证已安装的真实 TZif 版本。
    #[test]
    fn installed_timezone_database_is_accepted() {
        for name in [
            "Asia/Shanghai",
            "America/New_York",
            "Europe/London",
            "Etc/UTC",
        ] {
            if Path::new("/usr/share/zoneinfo").join(name).exists() {
                let settings = BTreeMap::from([("TZ".into(), name.into())]);
                assert!(timezone(&settings).is_ok(), "{name}");
            }
        }
    }
}
