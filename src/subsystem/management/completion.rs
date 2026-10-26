//! Quote-aware command and path completion at the actual cursor position.
//! 识别引号并按实际光标位置补全命令和路径。
use super::{Error, Result, Workspace};

const COMMANDS: &[&str] = &[
    "help", "clear", "history", "pwd", "cd", "ls", "cat", "mkdir", "rm", "rmdir", "mv", "edit",
    "vi", "vim", "check", "build", "jobs", "status", "start", "stop", "restart", "logs", "console",
    "shell", "tui", "exit", "quit",
];
pub(super) struct Completion {
    pub line: String,
    pub cursor: usize,
    pub choices: Vec<String>,
}
struct Token {
    start: usize,
    end: usize,
    value: String,
}

/// Decode an unfinished token without interpreting expansion or executing anything.
/// 解码尚未完成的参数，不解释展开，也不执行任何内容。
fn value(raw: &str) -> String {
    let (mut result, mut quote, mut escaped) = (String::new(), None, false);
    for ch in raw.chars() {
        if escaped {
            result.push(ch);
            escaped = false;
        } else if ch == '\\' && quote != Some('\'') {
            escaped = true;
        } else if let Some(current) = quote {
            if ch == current {
                quote = None;
            } else {
                result.push(ch);
            }
        } else if ch == '\'' || ch == '"' {
            quote = Some(ch);
        } else {
            result.push(ch);
        }
    }
    result
}
fn tokenize(line: &str) -> Vec<Token> {
    let (mut tokens, mut start, mut quote, mut escaped) = (Vec::new(), None, None, false);
    for (i, ch) in line.char_indices() {
        if start.is_none() {
            if ch.is_whitespace() {
                continue;
            }
            start = Some(i);
        }
        if escaped {
            escaped = false;
        } else if ch == '\\' && quote != Some('\'') {
            escaped = true;
        } else if let Some(current) = quote {
            if ch == current {
                quote = None;
            }
        } else if ch == '\'' || ch == '"' {
            quote = Some(ch);
        } else if ch.is_whitespace() {
            let begin = start.take().unwrap();
            tokens.push(Token {
                start: begin,
                end: i,
                value: value(&line[begin..i]),
            });
        }
    }
    if let Some(start) = start {
        tokens.push(Token {
            start,
            end: line.len(),
            value: value(&line[start..]),
        });
    }
    tokens
}
fn escape(text: &str) -> Result<String> {
    let mut result = String::new();
    for ch in text.chars() {
        if ch.is_control() {
            return Err(Error::Invalid(
                "补全包含控制字符 / completion contains control characters".into(),
            ));
        }
        if ch.is_whitespace() || "\\'\"$`;&|<>(){}[]!*?~#".contains(ch) {
            result.push('\\');
        }
        result.push(ch);
    }
    Ok(result)
}
fn common(a: &str, b: &str) -> String {
    let n = a
        .chars()
        .zip(b.chars())
        .take_while(|(a, b)| a == b)
        .map(|(ch, _)| ch.len_utf8())
        .sum::<usize>();
    a[..n].into()
}

pub(super) fn complete(
    line: &str,
    cursor: usize,
    cwd: &str,
    workspace: &Workspace,
) -> Result<Completion> {
    if cursor > line.len() || !line.is_char_boundary(cursor) {
        return Err(Error::Invalid(
            "补全光标无效 / invalid completion cursor".into(),
        ));
    }
    let tokens = tokenize(line);
    let active = tokens
        .iter()
        .position(|t| t.start <= cursor && cursor <= t.end);
    let index = active.unwrap_or_else(|| tokens.iter().filter(|t| t.end < cursor).count());
    let (start, end, prefix, suffix) = if let Some(i) = active {
        let token = &tokens[i];
        let prefix = value(&line[token.start..cursor]);
        let suffix = token.value.strip_prefix(&prefix).unwrap_or("").to_string();
        (token.start, token.end, prefix, suffix)
    } else {
        (cursor, cursor, String::new(), String::new())
    };
    let command = tokens.first().map(|t| t.value.as_str()).unwrap_or("");
    let path = match command {
        "cd" | "ls" | "cat" | "mkdir" | "rm" | "rmdir" | "edit" | "vi" | "vim" => index == 1,
        "mv" => index == 1 || index == 2,
        _ => false,
    };
    let mut candidates: Vec<(String, bool)> = Vec::new();
    if index == 0 {
        candidates.extend(COMMANDS.iter().map(|s| (s.to_string(), false)));
    } else if path {
        let (parent, leaf) = prefix
            .rsplit_once('/')
            .map(|(p, n)| (format!("{p}/"), n))
            .unwrap_or((String::new(), prefix.as_str()));
        let directory = Workspace::normalize(cwd, if parent.is_empty() { "." } else { &parent })?;
        for name in workspace.list(&directory)? {
            if !name.starts_with(leaf) {
                continue;
            }
            let normalized = Workspace::normalize(&directory, &name)?;
            let resolved = workspace.path(&normalized)?;
            let directory = std::fs::symlink_metadata(&resolved)?.is_dir();
            if command == "cd" && !directory {
                continue;
            }
            candidates.push((
                format!("{parent}{name}{}", if directory { "/" } else { "" }),
                directory,
            ));
        }
    } else if matches!(command, "start" | "stop" | "restart" | "build") && index == 1 {
        candidates.push(("--follow".into(), false));
    } else if command == "logs" {
        let mut needs_value = false;
        for token in tokens.iter().take(index).skip(1) {
            if needs_value {
                needs_value = false;
            } else if matches!(token.value.as_str(), "--lines" | "--task") {
                needs_value = true;
            }
        }
        if !needs_value {
            if index == 1 {
                candidates.extend(["boot", "shutdown", "build"].map(|s| (s.into(), false)));
            }
            for option in ["--follow", "--lines", "--task"] {
                if !tokens
                    .iter()
                    .enumerate()
                    .any(|(i, t)| Some(i) != active && t.value == option)
                {
                    candidates.push((option.into(), false));
                }
            }
        }
    }
    candidates.retain(|(s, _)| {
        s.len() >= prefix.len() + suffix.len() && s.starts_with(&prefix) && s.ends_with(&suffix)
    });
    candidates.sort();
    candidates.dedup();
    let mut result = Completion {
        line: line.into(),
        cursor,
        choices: Vec::new(),
    };
    if candidates.is_empty() {
        return Ok(result);
    }
    if candidates.len() > 1 {
        result.choices = candidates
            .iter()
            .map(|(s, _)| escape(s))
            .collect::<Result<Vec<_>>>()?;
    }
    let mut head = candidates[0].0[..candidates[0].0.len() - suffix.len()].to_string();
    for (s, _) in candidates.iter().skip(1) {
        head = common(&head, &s[..s.len() - suffix.len()]);
    }
    if candidates.len() > 1 && head == prefix {
        return Ok(result);
    }
    let escaped = escape(&format!("{head}{suffix}"))?;
    result.line = format!("{}{}{}", &line[..start], escaped, &line[end..]);
    result.cursor = start + escape(&head)?.len();
    if candidates.len() == 1 && !candidates[0].1 && suffix.is_empty() {
        if end == line.len() {
            result.line.push(' ');
            result.cursor += 1;
        } else if let Some(ch) = line[end..].chars().next().filter(|ch| ch.is_whitespace()) {
            result.cursor += ch.len_utf8();
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::subsystem::management::{terminal::parse, tests::fixture};
    #[test]
    fn completion_preserves_argument_values_and_directories() {
        let (_dir, w) = fixture();
        w.write("/script/a b'\"$", b"x").unwrap();
        for line in ["cat /script/a", "cat '/script/a", "cat /script/a\\ "] {
            let result = complete(line, line.len(), "/", &w).unwrap();
            assert_eq!(parse(&result.line).unwrap(), ["cat", "/script/a b'\"$"]);
        }
        assert_eq!(complete("cd scr", 6, "/", &w).unwrap().line, "cd script/");
        let r = complete("cat ", 4, "/", &w).unwrap();
        assert_eq!(r.choices, ["build/", "script/"]);
        assert_eq!(
            complete("start --f", 9, "/", &w).unwrap().line,
            "start --follow "
        );
        assert_eq!(
            complete("logs --lines ", 13, "/", &w).unwrap().line,
            "logs --lines "
        );
    }
    #[test]
    fn completion_keeps_unrelated_suffix_and_other_arguments() {
        let (_dir, w) = fixture();
        w.write("/script/readme", b"x").unwrap();
        let line = "cat /script/reKEEP";
        assert_eq!(complete(line, 14, "/", &w).unwrap().line, line);
        assert!(COMMANDS.contains(&"clear"));
        assert!(COMMANDS.contains(&"ls"));
    }
}
