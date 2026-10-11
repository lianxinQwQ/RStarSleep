//! Desktop Entry parsing and literal argument expansion.
//! Desktop Entry 解析及字面参数展开。
use super::{
    Error, Result,
    filesystem::{open_at_guest, open_root},
};
use std::io::Read;
use std::{collections::BTreeMap, os::unix::fs::PermissionsExt, path::Path};

#[derive(Debug, Clone)]
pub struct DesktopEntry {
    pub values: BTreeMap<String, String>,
    pub name: String,
    pub icon: Option<String>,
    pub exec: String,
    pub terminal: bool,
    pub hidden: bool,
    pub no_display: bool,
}

/// Parse only the main application group and retain localized metadata.
/// 只解析主应用组并保留本地化元数据。
/// # Arguments
/// - `text`: Complete UTF-8 desktop contents. 完整 UTF-8 desktop 内容。
pub fn parse(text: &str) -> Result<DesktopEntry> {
    if text.len() > 1024 * 1024 || text.contains('\0') {
        return Err(Error::Invalid("desktop 内容无效".into()));
    }
    let mut values = BTreeMap::new();
    let mut main = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('#') || line.is_empty() {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            main = line == "[Desktop Entry]";
            continue;
        }
        if main
            && let Some((k, v)) = line.split_once('=')
            && values.insert(k.trim().to_owned(), v.to_owned()).is_some()
        {
            return Err(Error::Invalid("desktop 主组重复字段".into()));
        }
    }
    if values.get("Type").map(String::as_str) != Some("Application") {
        return Err(Error::Invalid("desktop Type 必须为 Application".into()));
    }
    let name = values
        .get("Name")
        .filter(|v| !v.is_empty())
        .ok_or_else(|| Error::Invalid("desktop 缺少 Name".into()))?
        .clone();
    let exec = values
        .get("Exec")
        .filter(|v| !v.is_empty())
        .ok_or_else(|| Error::Invalid("desktop 缺少 Exec".into()))?
        .clone();
    let boolean = |k: &str| -> Result<bool> {
        match values.get(k).map(String::as_str) {
            None | Some("false") => Ok(false),
            Some("true") => Ok(true),
            _ => Err(Error::Invalid(format!("desktop 布尔字段无效: {k}"))),
        }
    };
    let terminal = boolean("Terminal")?;
    let hidden = boolean("Hidden")?;
    let no_display = boolean("NoDisplay")?;
    Ok(DesktopEntry {
        name,
        icon: values.get("Icon").cloned(),
        exec,
        terminal,
        hidden,
        no_display,
        values,
    })
}

/// Read a bounded desktop file from a guest-contained path.
/// 从容器根内路径读取有界 desktop 文件。
/// # Arguments
/// - `root`: Current artifact root. 当前产物根。
/// - `target`: Absolute guest desktop path. 绝对容器 desktop 路径。
pub fn read(root: &Path, target: &str) -> Result<DesktopEntry> {
    read_at(&open_root(root)?, target)
}
/// Read a desktop entry through a pinned live or artifact root.
/// 通过固定运行根或产物根读取 desktop 入口。
/// # Arguments
/// - `root`, `target`: Pinned root and absolute guest path. 固定根及绝对容器路径。
pub(crate) fn read_at(root: &std::fs::File, target: &str) -> Result<DesktopEntry> {
    if !target.ends_with(".desktop") {
        return Err(Error::Invalid("入口必须为 desktop 文件".into()));
    }
    let file = open_at_guest(root, Path::new(target), libc::O_RDONLY | libc::O_NONBLOCK)?;
    let meta = file.metadata()?;
    if !meta.is_file() || meta.len() > 1024 * 1024 {
        return Err(Error::Invalid("desktop 必须为有界普通文件".into()));
    }
    let mut text = String::new();
    file.take(1024 * 1024 + 1).read_to_string(&mut text)?;
    parse(&text)
}

/// Split Desktop Exec quoting without evaluating shell syntax.
/// 拆分 Desktop Exec 引用，不执行 shell 语法。
/// # Arguments
/// - `value`: Raw Exec value. 原始 Exec 值。
fn words(value: &str) -> Result<Vec<String>> {
    // Decode the Desktop Entry string before interpreting Exec quoting.
    // 先解码 Desktop Entry 字符串，再解释 Exec 引用。
    let mut decoded = String::new();
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            decoded.push(c);
            continue;
        }
        let escaped = chars
            .next()
            .ok_or_else(|| Error::Invalid("desktop 字符串转义不完整".into()))?;
        decoded.push(match escaped {
            's' => ' ',
            'n' => '\n',
            't' => '\t',
            'r' => '\r',
            '\\' => '\\',
            _ => {
                return Err(Error::Invalid(format!(
                    "不支持的 desktop 字符串转义: \\{escaped}"
                )));
            }
        });
    }
    let mut result = Vec::new();
    let mut word = String::new();
    let mut quote = false;
    let mut started = false;
    let mut chars = decoded.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                quote = !quote;
                started = true;
            }
            '\\' => {
                let escaped = chars
                    .next()
                    .ok_or_else(|| Error::Invalid("Exec 转义不完整".into()))?;
                if !quote || !matches!(escaped, '"' | '\\' | '$' | '`') {
                    return Err(Error::Invalid("Exec 转义必须引用受支持的特殊字符".into()));
                }
                word.push(escaped);
                started = true;
            }
            c if c.is_whitespace() && !quote => {
                if started {
                    result.push(std::mem::take(&mut word));
                    started = false;
                }
            }
            c => {
                word.push(c);
                started = true;
            }
        }
    }
    if quote {
        return Err(Error::Invalid("Exec 引号不完整".into()));
    }
    if started {
        result.push(word);
    }
    Ok(result)
}

/// Find at most one desktop file/URL field code.
/// 查找至多一个 desktop 文件或 URL 字段码。
/// # Arguments
/// - `entry`: Parsed application. 已解析应用。
pub fn file_field(entry: &DesktopEntry) -> Result<Option<&'static str>> {
    let mut found = None;
    for word in words(&entry.exec)? {
        let mut chars = word.chars();
        while let Some(c) = chars.next() {
            if c == '%' {
                let code = chars
                    .next()
                    .ok_or_else(|| Error::Invalid("Exec 字段码不完整".into()))?;
                let field = match code {
                    'f' => Some("%f"),
                    'F' => Some("%F"),
                    'u' => Some("%u"),
                    'U' => Some("%U"),
                    _ => None,
                };
                if let Some(field) = field {
                    if found.is_some() {
                        return Err(Error::Invalid("Exec 文件字段码重复".into()));
                    }
                    found = Some(field);
                }
            }
        }
    }
    Ok(found)
}

/// Expand supported desktop fields into literal argv members.
/// 将支持的 desktop 字段展开为字面 argv 成员。
/// # Arguments
/// - `entry`: Parsed application metadata. 已解析应用元数据。
/// - `target`: Guest desktop path for %k. 用于 %k 的容器 desktop 路径。
/// - `args`: File or URL values. 文件或 URL 值。
pub fn expand(entry: &DesktopEntry, target: &str, args: &[String]) -> Result<Vec<String>> {
    let field = file_field(entry)?;
    let mut result = Vec::new();
    for word in words(&entry.exec)? {
        match word.as_str() {
            "%i" => {
                if let Some(icon) = &entry.icon {
                    result.extend(["--icon".into(), icon.clone()]);
                }
            }
            "%f" | "%u" => {
                if let Some(a) = args.first() {
                    result.push(a.clone());
                }
            }
            "%F" | "%U" => result.extend(args.iter().cloned()),
            "%d" | "%D" | "%n" | "%N" | "%v" | "%m" => {}
            _ => {
                let mut expanded = String::new();
                let mut chars = word.chars();
                while let Some(c) = chars.next() {
                    if c != '%' {
                        expanded.push(c);
                        continue;
                    }
                    match chars
                        .next()
                        .ok_or_else(|| Error::Invalid("Exec 字段码不完整".into()))?
                    {
                        '%' => expanded.push('%'),
                        'c' => expanded.push_str(&entry.name),
                        'k' => expanded.push_str(target),
                        'd' | 'D' | 'n' | 'N' | 'v' | 'm' => {}
                        c => {
                            return Err(Error::Invalid(format!("不支持或非独立 Exec 字段码 %{c}")));
                        }
                    }
                }
                if !expanded.is_empty() || word.is_empty() {
                    result.push(expanded);
                }
            }
        }
    }
    if field.is_none() {
        result.extend(args.iter().cloned());
    }
    if result.first().is_none_or(String::is_empty) || result.iter().any(|v| v.contains('\0')) {
        return Err(Error::Invalid("desktop 程序无效".into()));
    }
    Ok(result)
}

/// Resolve a binary or desktop entry using the actual final runtime root.
/// 根据实际最终运行根解析二进制或 desktop 入口。
/// # Arguments
/// - `root`: Writable or completed guest root. 可写或已完成容器根。
/// - `target`: Executable name, path or desktop path. 可执行名称、路径或 desktop 路径。
/// - `args`: Literal launch arguments. 字面启动参数。
pub fn resolve(root: &Path, target: &str, args: &[String]) -> Result<Vec<String>> {
    resolve_at(&open_root(root)?, target, args)
}
/// Resolve against live guest-visible mounts using a stable descriptor.
/// 使用稳定描述符对实际容器可见挂载解析。
/// # Arguments
/// - `root`, `target`, `args`: Pinned root, entry and arguments. 固定根、入口及参数。
pub(crate) fn resolve_at(
    root: &std::fs::File,
    target: &str,
    args: &[String],
) -> Result<Vec<String>> {
    let desktop = if target.ends_with(".desktop") {
        if target.starts_with('/') {
            Some(target.to_owned())
        } else {
            Some(format!("/usr/share/applications/{target}"))
        }
    } else {
        None
    };
    if let Some(target) = desktop {
        let entry = read_at(root, &target)?;
        if entry.hidden {
            return Err(Error::Invalid("desktop 已隐藏".into()));
        }
        if let Some(try_exec) = entry.values.get("TryExec") {
            executable_at(root, try_exec)?;
        }
        let mut argv = expand(&entry, &target, args)?;
        if let Some(dir) = entry.values.get("Path").filter(|v| !v.is_empty()) {
            if !open_at_guest(root, Path::new(dir), libc::O_PATH)?
                .metadata()?
                .is_dir()
            {
                return Err(Error::Invalid("desktop 工作目录不存在".into()));
            }
            argv.splice(
                0..0,
                ["/usr/bin/env".into(), format!("--chdir={dir}"), "--".into()],
            );
        }
        return Ok(argv);
    }
    match executable_at(root, target) {
        Ok(path) => Ok([vec![path], args.to_vec()].concat()),
        Err(error) => {
            if target.contains('/') {
                return Err(error);
            }
            crate::config::validate_identifier(target)?;
            resolve_at(
                root,
                &format!("/usr/share/applications/{target}.desktop"),
                args,
            )
        }
    }
}

/// Locate an executable through a pinned root descriptor.
/// 通过固定根描述符定位可执行文件。
/// # Arguments
/// - `root`, `target`: Stable root and program. 稳定根及程序。
pub(crate) fn executable_at(root: &std::fs::File, target: &str) -> Result<String> {
    let candidates = if target.starts_with('/') {
        vec![target.to_owned()]
    } else {
        if target.contains('/') || target.is_empty() {
            return Err(Error::Invalid("程序名无效".into()));
        }
        ["/usr/local/bin", "/usr/bin", "/bin"]
            .map(|dir| format!("{dir}/{target}"))
            .to_vec()
    };
    for path in candidates {
        match open_at_guest(root, Path::new(&path), libc::O_PATH) {
            Ok(file)
                if file.metadata()?.is_file()
                    && file.metadata()?.permissions().mode() & 0o111 != 0 =>
            {
                return Ok(path);
            }
            Ok(_) => {}
            Err(Error::Io { source, .. }) if source.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
    }
    Err(Error::Invalid(format!("容器程序不存在: {target}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    /// Expand metadata and file arguments without shell interpolation.
    /// 不经 shell 插值展开元数据与文件参数。
    /// # Arguments
    /// No parameters. 无参数。
    #[test]
    fn expands_literal_arguments() {
        let e=parse("[Desktop Entry]\nType=Application\nName=A name\nIcon=my-icon\nExec=runner %i \"%c\" %k %F\n").unwrap();
        assert_eq!(
            expand(&e, "/a.desktop", &["one two".into()]).unwrap(),
            [
                "runner",
                "--icon",
                "my-icon",
                "A name",
                "/a.desktop",
                "one two"
            ]
        );
        let e = parse(
            "[Desktop Entry]\nType=Application\nName=A\nExec=runner \"$HOME\" \"x ; touch bad\"\n",
        )
        .unwrap();
        assert_eq!(
            expand(&e, "/a.desktop", &[]).unwrap(),
            ["runner", "$HOME", "x ; touch bad"]
        );
    }
    /// Decode standard string escapes and Exec escapes in two distinct passes.
    /// 以两个独立步骤解码标准字符串转义及 Exec 转义。
    /// # Arguments
    /// No parameters. 无参数。
    #[test]
    fn decodes_both_escape_layers() {
        let exec = r#"runner "a\\"b" "a\\\\b" "\\$HOME" "\\`literal\\`" "" "space\sword" "line\nnext" "tab\tvalue" "carriage\rreturn" "%%""#;
        let entry = parse(&format!(
            "[Desktop Entry]\nType=Application\nName=App\nExec={exec}\n"
        ))
        .unwrap();
        assert_eq!(
            expand(&entry, "/app.desktop", &[]).unwrap(),
            [
                "runner",
                "a\"b",
                "a\\b",
                "$HOME",
                "`literal`",
                "",
                "space word",
                "line\nnext",
                "tab\tvalue",
                "carriage\rreturn",
                "%",
            ]
        );
    }
    /// Malformed escapes in either layer must fail instead of changing arguments.
    /// 任一层的畸形转义都必须失败，不能悄悄改变参数。
    /// # Arguments
    /// No parameters. 无参数。
    #[test]
    fn rejects_incomplete_and_unknown_escapes() {
        for exec in [
            r#"runner "\q""#,
            r#"runner "a\\q""#,
            "runner \\",
            r#"runner "\\"#,
            r#"runner "unterminated"#,
        ] {
            let entry = parse(&format!(
                "[Desktop Entry]\nType=Application\nName=App\nExec={exec}\n"
            ))
            .unwrap();
            assert!(expand(&entry, "/app.desktop", &[]).is_err(), "{exec:?}");
        }
    }
    /// Reject ambiguous multi-file expansion and preserve only the main group.
    /// 拒绝有歧义的多文件展开并只保留主组。
    /// # Arguments
    /// No parameters. 无参数。
    #[test]
    fn validates_fields_and_groups() {
        for exec in ["runner %f %U", "runner prefix%F", "runner \"unterminated"] {
            let e = parse(&format!(
                "[Desktop Entry]\nType=Application\nName=A\nExec={exec}\n"
            ))
            .unwrap();
            assert!(expand(&e, "/a.desktop", &[]).is_err());
        }
        let e=parse("[Desktop Entry]\nType=Application\nName=Main\nExec=main\n[Desktop Action Other]\nName=Other\nExec=other\n").unwrap();
        assert_eq!(e.name, "Main");
    }
}
