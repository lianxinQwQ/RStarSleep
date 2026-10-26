//! A shell-like management terminal and small UTF-8 vi-style editor.
//! 类 shell 管理终端及小型 UTF-8 vi 风格编辑器。
use super::{Error, Result, Workspace};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

pub enum Action {
    Command(Vec<String>),
    Shell,
    Exit,
}
pub struct Update {
    pub output: Vec<u8>,
    pub action: Option<Action>,
}
enum Mode {
    Cli,
    Tui,
    Edit(Editor),
}
struct Editor {
    path: String,
    text: String,
    original: Option<Vec<u8>>,
    cursor: usize,
    insert: bool,
    command: Option<String>,
    undo: Vec<(String, usize)>,
    message: String,
    top: usize,
    search: String,
}
pub struct Terminal {
    id: String,
    cwd: String,
    line: String,
    history: Vec<String>,
    history_index: usize,
    pending: Vec<u8>,
    mode: Mode,
    cols: usize,
    rows: usize,
    tui_message: String,
    paste: bool,
}
pub const HELP: &str = "help | pwd | cd [path] | ls [path] | cat path | mkdir path | rm path | rmdir path | mv source target\nedit path (vi/vim) | check | build | jobs | status\nstart [--follow] | stop [--follow] | restart [--follow]\nlogs [boot|shutdown|build] [--follow] [--lines N] [--task ID] | console\nshell | tui | exit\n仅访问 build/script，保存后需显式构建；不支持管道、重定向或变量展开。\nOnly build/script are accessible; saving requires an explicit build. No pipes, redirection or expansion.\n编辑 / Edit: i/a 插入 / insert, Esc 普通 / normal, h/j/k/l 移动 / move, x 删除 / delete, u 撤销 / undo, / 搜索 / search, n 下一个 / next, :w :q :wq :q!";

/// Parse literal arguments, rejecting shell operators and incomplete quoting.
/// 解析字面参数，拒绝 shell 操作符及未闭合引号。
/// # Arguments
/// - `line`: UTF-8 management command. UTF-8 管理命令。
pub fn parse(line: &str) -> Result<Vec<String>> {
    let (mut args, mut word, mut quote, mut escaped, mut started) =
        (Vec::new(), String::new(), None, false, false);
    if line.len() > 32768 {
        return Err(Error::Invalid(
            "命令超出限制 / command exceeds limit".into(),
        ));
    }
    for ch in line.chars() {
        if escaped {
            word.push(ch);
            escaped = false;
            started = true;
            continue;
        }
        if quote == Some('\'') {
            if ch == '\'' {
                quote = None;
            } else {
                word.push(ch);
            }
            continue;
        }
        if ch == '\\' {
            escaped = true;
            started = true;
            continue;
        }
        if quote == Some('"') {
            match ch {
                '"' => quote = None,
                '$' | '`' => {
                    return Err(Error::Invalid(
                        "不支持展开 / expansion is unsupported".into(),
                    ));
                }
                _ => word.push(ch),
            }
            continue;
        }
        match ch {
            '\'' | '"' => {
                quote = Some(ch);
                started = true;
            }
            '|' | '&' | ';' | '<' | '>' | '$' | '`' | '\n' | '\r' => {
                return Err(Error::Invalid(
                    "不支持 shell 操作符或展开 / shell operators and expansion are unsupported"
                        .into(),
                ));
            }
            c if c.is_whitespace() => {
                if started {
                    args.push(std::mem::take(&mut word));
                    started = false;
                }
            }
            _ => {
                word.push(ch);
                started = true;
            }
        }
    }
    if escaped || quote.is_some() {
        return Err(Error::Invalid(
            "引号或转义未闭合 / unterminated quote or escape".into(),
        ));
    }
    if started {
        args.push(word);
    }
    Ok(args)
}
impl Terminal {
    /// Initialize an independent management session.
    /// 初始化独立管理会话。
    /// # Arguments
    /// - `id`: Authenticated subsystem. 已认证子系统。
    pub fn new(id: &str) -> Self {
        Self {
            id: id.into(),
            cwd: "/".into(),
            line: String::new(),
            history: Vec::new(),
            history_index: 0,
            pending: Vec::new(),
            mode: Mode::Cli,
            cols: 80,
            rows: 24,
            tui_message: String::new(),
            paste: false,
        }
    }
    /// Show the initial shell-like prompt.
    /// 显示初始类 shell 提示符。
    /// # Arguments
    /// - `self`: Management terminal. 管理终端。
    pub fn welcome(&self) -> Vec<u8> {
        format!("\x1b[?2004hRStarSleep {}\r\n管理终端；输入 help / Management terminal; type help\r\n{}",self.id,self.prompt()).into_bytes()
    }
    /// Return the current virtual directory.
    /// 返回当前虚拟目录。
    /// # Arguments
    /// - `self`: Management terminal. 管理终端。
    pub fn cwd(&self) -> &str {
        &self.cwd
    }
    /// Drain bytes already received after a command, for nested shell handoff.
    /// 排空命令后已接收字节，用于嵌套 shell 交接。
    /// # Arguments
    /// - `self`: Management terminal. 管理终端。
    pub fn take_pending(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.pending)
    }
    /// Apply terminal dimensions and redraw the active view.
    /// 应用终端尺寸并重绘活动视图。
    /// # Arguments
    /// - `self`, `cols`, `rows`: Terminal and dimensions. 终端及尺寸。
    pub fn resize(&mut self, cols: u32, rows: u32) -> Vec<u8> {
        self.cols = (cols as usize).clamp(20, 1000);
        self.rows = (rows as usize).clamp(6, 1000);
        self.draw()
    }
    /// Display command output and restore the current management view.
    /// 显示命令输出并恢复当前管理视图。
    /// # Arguments
    /// - `self`, `text`: Terminal and operation result. 终端及操作结果。
    pub fn finish(&mut self, text: &str) -> Vec<u8> {
        if matches!(self.mode, Mode::Tui) {
            self.tui_message = clean(text);
            self.draw()
        } else {
            format!(
                "\x1b[?2004h{}{}{}",
                clean(text).replace('\n', "\r\n"),
                if text.is_empty() || text.ends_with('\n') {
                    ""
                } else {
                    "\r\n"
                },
                self.prompt()
            )
            .into_bytes()
        }
    }
    /// Interpret terminal bytes without invoking a host shell.
    /// 解释终端字节，不调用宿主 shell。
    /// # Arguments
    /// - `self`, `data`, `workspace`: Terminal, bytes and shared file view. 终端、字节及共享文件视图。
    pub fn input(&mut self, data: &[u8], workspace: &Workspace) -> Result<Update> {
        if self.pending.len() + data.len() > 65536 {
            return Err(Error::Invalid(
                "终端输入超出限制 / terminal input exceeds limit".into(),
            ));
        }
        self.pending.extend_from_slice(data);
        let mut output = Vec::new();
        loop {
            if self.pending.is_empty() {
                break;
            }
            let (size, mut key) = if self.pending[0] == 27 {
                if self.pending.len() == 1 {
                    break;
                }
                if self.pending[1] == b'[' {
                    let Some(end) = self
                        .pending
                        .iter()
                        .enumerate()
                        .skip(2)
                        .find(|(_, b)| (b'@'..=b'~').contains(b))
                        .map(|(i, _)| i)
                    else {
                        if self.pending.len() < 32 {
                            break;
                        }
                        self.pending.remove(0);
                        continue;
                    };
                    let key = match &self.pending[..=end] {
                        b"\x1b[A" => '↑',
                        b"\x1b[B" => '↓',
                        b"\x1b[C" => '→',
                        b"\x1b[D" => '←',
                        b"\x1b[200~" => {
                            self.paste = true;
                            '\0'
                        }
                        b"\x1b[201~" => {
                            self.paste = false;
                            '\0'
                        }
                        _ => '\0',
                    };
                    (end + 1, key)
                } else {
                    (1, '\x1b')
                }
            } else {
                let n = match self.pending[0] {
                    0..=127 => 1,
                    0xc2..=0xdf => 2,
                    0xe0..=0xef => 3,
                    0xf0..=0xf4 => 4,
                    _ => 1,
                };
                if self.pending.len() < n {
                    break;
                }
                let key = std::str::from_utf8(&self.pending[..n])
                    .map_err(|_| {
                        Error::Invalid("终端输入不是 UTF-8 / terminal input is not UTF-8".into())
                    })?
                    .chars()
                    .next()
                    .unwrap();
                (n, key)
            };
            self.pending.drain(..size);
            if key == '\0' {
                continue;
            }
            if key == '\r' {
                if self.pending.first() == Some(&b'\n') {
                    self.pending.remove(0);
                }
                key = '\n';
            }
            if self.paste && matches!(self.mode, Mode::Cli) {
                if key == '\n' || key == '\t' {
                    key = ' ';
                } else if key.is_control() {
                    continue;
                }
            }
            let update = self.key(key, workspace)?;
            output.extend(update.output);
            if update.action.is_some() {
                return Ok(Update {
                    output,
                    action: update.action,
                });
            }
        }
        Ok(Update {
            output,
            action: None,
        })
    }
    /// Resolve a standalone Escape only after the transport's short ambiguity timeout.
    /// 仅在传输的短歧义超时后解释独立 Escape。
    /// # Arguments
    /// - `self`, `workspace`: Terminal and shared file view. 终端及共享文件视图。
    pub fn flush_escape(&mut self, workspace: &Workspace) -> Result<Update> {
        if self.pending == [27] {
            self.pending.clear();
            self.key('\x1b', workspace)
        } else {
            Ok(Update {
                output: Vec::new(),
                action: None,
            })
        }
    }
    /// Render the current prompt.
    /// 渲染当前提示符。
    /// # Arguments
    /// - `self`: Management terminal. 管理终端。
    fn prompt(&self) -> String {
        format!("{}:{}> ", self.id, self.cwd)
    }
    /// Process one decoded key in the active mode.
    /// 在活动模式处理单个已解码按键。
    /// # Arguments
    /// - `self`, `key`, `workspace`: Terminal, key and file view. 终端、按键及文件视图。
    fn key(&mut self, key: char, workspace: &Workspace) -> Result<Update> {
        if let Mode::Edit(editor) = &mut self.mode {
            let quit = editor.key(key, workspace)?;
            if quit {
                self.mode = Mode::Cli;
                return Ok(Update {
                    output: [
                        b"\x1b[?1049l\x1b[?25h\x1b[0m".as_slice(),
                        self.finish("").as_slice(),
                    ]
                    .concat(),
                    action: None,
                });
            }
            return Ok(Update {
                output: self.draw(),
                action: None,
            });
        }
        if matches!(self.mode, Mode::Tui) {
            let command = match key {
                'q' | '\x1b' | '\x03' => {
                    self.mode = Mode::Cli;
                    return Ok(Update {
                        output: format!("\x1b[?1049l\x1b[?25h{}", self.prompt()).into_bytes(),
                        action: None,
                    });
                }
                's' => Some("status"),
                'b' => Some("build"),
                'u' => Some("start"),
                'd' => Some("stop"),
                'r' => Some("restart"),
                'c' => Some("console"),
                'l' => Some("logs build"),
                'h' => Some("shell"),
                'e' => {
                    self.mode = Mode::Cli;
                    self.line = "edit ".into();
                    return Ok(Update {
                        output: format!("\x1b[?1049l\x1b[?25h{}edit ", self.prompt()).into_bytes(),
                        action: None,
                    });
                }
                _ => None,
            };
            if let Some(command) = command {
                let action = if command == "shell" {
                    Action::Shell
                } else {
                    Action::Command(parse(command)?)
                };
                return Ok(Update {
                    output: b"\x1b[?1049l\x1b[?25h".to_vec(),
                    action: Some(action),
                });
            }
            return Ok(Update {
                output: Vec::new(),
                action: None,
            });
        }
        match key {
            '\n' => {
                let line = std::mem::take(&mut self.line);
                if !line.trim().is_empty() {
                    self.history.push(line.clone());
                    if self.history.len() > 200 {
                        self.history.remove(0);
                    }
                }
                self.history_index = self.history.len();
                let mut update = self.command(&line, workspace)?;
                update.output.splice(0..0, b"\r\n".iter().copied());
                return Ok(update);
            }
            '\x03' => {
                self.line.clear();
                return Ok(Update {
                    output: self.finish("^C"),
                    action: None,
                });
            }
            '\x04' if self.line.is_empty() => {
                return Ok(Update {
                    output: b"\x1b[?2004l".to_vec(),
                    action: Some(Action::Exit),
                });
            }
            '\x7f' | '\x08' => {
                self.line.pop();
            }
            '↑' => {
                self.history_index = self.history_index.saturating_sub(1);
                self.line = self
                    .history
                    .get(self.history_index)
                    .cloned()
                    .unwrap_or_default();
            }
            '↓' => {
                self.history_index = (self.history_index + 1).min(self.history.len());
                self.line = self
                    .history
                    .get(self.history_index)
                    .cloned()
                    .unwrap_or_default();
            }
            '\t' => {
                let word = self.line.split_whitespace().last().unwrap_or("");
                let commands = [
                    "help", "status", "start", "stop", "restart", "build", "check", "console",
                    "shell", "tui", "edit", "logs", "jobs", "exit",
                ];
                let choices: Vec<String> = if !self.line.contains(' ') {
                    commands
                        .iter()
                        .filter(|c| c.starts_with(word))
                        .map(|c| c.to_string())
                        .collect()
                } else {
                    let (parent, prefix) = word
                        .rsplit_once('/')
                        .map(|(p, n)| (format!("{p}/"), n))
                        .unwrap_or((String::new(), word));
                    let path = Workspace::normalize(
                        &self.cwd,
                        if parent.is_empty() { "." } else { &parent },
                    )?;
                    workspace
                        .list(&path)?
                        .into_iter()
                        .filter(|name| name.starts_with(prefix))
                        .map(|name| format!("{parent}{name}"))
                        .collect()
                };
                if choices.len() == 1 {
                    let len = self.line.len() - word.len();
                    self.line.truncate(len);
                    self.line.push_str(&choices[0]);
                } else if !choices.is_empty() {
                    return Ok(Update {
                        output: format!(
                            "\r\n{}\r\n{}{}",
                            choices.join("  "),
                            self.prompt(),
                            self.line
                        )
                        .into_bytes(),
                        action: None,
                    });
                }
            }
            ch if !ch.is_control() && self.line.len() < 32768 => self.line.push(ch),
            _ => {}
        }
        Ok(Update {
            output: format!("\r\x1b[2K{}{}", self.prompt(), self.line).into_bytes(),
            action: None,
        })
    }
    /// Execute local navigation or request a fixed backend operation.
    /// 执行本地导航或请求固定后端操作。
    /// # Arguments
    /// - `self`, `line`, `workspace`: Terminal, command and file view. 终端、命令及文件视图。
    fn command(&mut self, line: &str, workspace: &Workspace) -> Result<Update> {
        let mut words = match parse(line) {
            Ok(words) => words,
            Err(error) => {
                return Ok(Update {
                    output: self.finish(&error.to_string()),
                    action: None,
                });
            }
        };
        let mut action = None;
        let mut text = String::new();
        match words
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .as_slice()
        {
            [] => {}
            ["help"] => text = HELP.into(),
            ["pwd"] => text = self.cwd.clone(),
            ["cd"] => self.cwd = "/".into(),
            ["cd", path] => {
                let path = Workspace::normalize(&self.cwd, path)?;
                workspace.list(&path)?;
                self.cwd = path;
            }
            ["ls"] => text = workspace.list(&self.cwd)?.join("\n"),
            ["ls", path] => {
                text = workspace
                    .list(&Workspace::normalize(&self.cwd, path)?)?
                    .join("\n")
            }
            ["cat", path] => {
                text = String::from_utf8(workspace.read(&Workspace::normalize(&self.cwd, path)?)?)
                    .map_err(|_| {
                    Error::Invalid("二进制文件请使用 SFTP / use SFTP for binary files".into())
                })?
            }
            ["edit" | "vi" | "vim", path] => {
                let path = Workspace::normalize(&self.cwd, path)?;
                let original = match workspace.read(&path) {
                    Ok(bytes) => Some(bytes),
                    Err(Error::Io { source, .. })
                        if source.kind() == std::io::ErrorKind::NotFound =>
                    {
                        None
                    }
                    Err(error) => return Err(error),
                };
                if original.as_ref().is_some_and(|b| b.len() > 1024 * 1024) {
                    return Err(Error::Invalid(
                        "编辑器仅支持 1 MiB 以内文本 / editor supports text up to 1 MiB".into(),
                    ));
                }
                let text =
                    String::from_utf8(original.clone().unwrap_or_default()).map_err(|_| {
                        Error::Invalid(
                            "编辑器仅支持 UTF-8 文本 / editor supports UTF-8 text only".into(),
                        )
                    })?;
                self.mode = Mode::Edit(Editor {
                    path,
                    text,
                    original,
                    cursor: 0,
                    insert: false,
                    command: None,
                    undo: Vec::new(),
                    message: String::new(),
                    top: 0,
                    search: String::new(),
                });
                return Ok(Update {
                    output: [b"\x1b[?1049h".as_slice(), self.draw().as_slice()].concat(),
                    action: None,
                });
            }
            ["tui"] => {
                self.mode = Mode::Tui;
                return Ok(Update {
                    output: self.draw(),
                    action: None,
                });
            }
            ["shell"] => action = Some(Action::Shell),
            ["exit" | "quit"] => action = Some(Action::Exit),
            _ => {
                if matches!(words[0].as_str(), "mkdir" | "rm" | "rmdir" | "mv") {
                    for path in words.iter_mut().skip(1) {
                        *path = Workspace::normalize(&self.cwd, path)?;
                    }
                }
                action = Some(Action::Command(words));
            }
        }
        Ok(Update {
            output: if action.is_some() {
                Vec::new()
            } else {
                self.finish(&text)
            },
            action,
        })
    }
    /// Paint a bounded full-screen TUI or editor.
    /// 绘制有界全屏 TUI 或编辑器。
    /// # Arguments
    /// - `self`: Active terminal. 活动终端。
    fn draw(&mut self) -> Vec<u8> {
        match &mut self.mode {
            Mode::Cli => format!("\r\x1b[2K{}{}", self.prompt(), self.line).into_bytes(),
            Mode::Tui => {
                let text = format!(
                    "RStarSleep {}\n\ns 状态/status  b 构建/build  e 编辑/edit\nu 启动/start  d 关机/stop  r 重启/restart\nc 控制台/console  l 构建日志/build log  h root shell\nq 返回 CLI / Return to CLI\n\n/build/\n/script/\n\n{}",
                    self.id, self.tui_message
                );
                format!(
                    "\x1b[?1049h\x1b[2J\x1b[H\x1b[?25l{}",
                    text.lines()
                        .take(self.rows)
                        .map(|l| clip(l, self.cols))
                        .collect::<Vec<_>>()
                        .join("\r\n")
                )
                .into_bytes()
            }
            Mode::Edit(editor) => editor.draw(self.cols, self.rows).into_bytes(),
        }
    }
}
impl Editor {
    /// Save only when the on-disk original still matches this editor session.
    /// 仅在磁盘原始内容仍匹配本编辑会话时保存。
    /// # Arguments
    /// - `self`, `workspace`: Editor and shared file view. 编辑器及共享文件视图。
    fn save(&mut self, workspace: &Workspace) -> Result<()> {
        workspace.write_expected(&self.path, self.original.as_deref(), self.text.as_bytes())?;
        self.original = Some(self.text.as_bytes().to_vec());
        self.message = "已保存 / saved".into();
        Ok(())
    }
    /// Keep a bounded undo history before changing text.
    /// 修改文本前保留有界撤销历史。
    /// # Arguments
    /// - `self`: Editor. 编辑器。
    fn remember(&mut self) {
        if self.undo.len() >= 32 {
            self.undo.remove(0);
        }
        self.undo.push((self.text.clone(), self.cursor));
    }
    /// Move by Unicode boundaries or retain column across lines.
    /// 按 Unicode 边界移动，或跨行保留列。
    /// # Arguments
    /// - `self`, `direction`: Editor and h/j/k/l direction. 编辑器及移动方向。
    fn movement(&mut self, direction: char) {
        match direction {
            'h' => {
                self.cursor = self.text[..self.cursor]
                    .char_indices()
                    .next_back()
                    .map_or(0, |(i, _)| i)
            }
            'l' => {
                self.cursor += self.text[self.cursor..]
                    .chars()
                    .next()
                    .map_or(0, char::len_utf8)
            }
            'j' | 'k' => {
                let start = self.text[..self.cursor].rfind('\n').map_or(0, |i| i + 1);
                let column = self.text[start..self.cursor].chars().count();
                let target = if direction == 'j' {
                    self.text[self.cursor..]
                        .find('\n')
                        .map(|i| self.cursor + i + 1)
                } else if start > 0 {
                    Some(self.text[..start - 1].rfind('\n').map_or(0, |i| i + 1))
                } else {
                    None
                };
                if let Some(target) = target {
                    self.cursor = target
                        + self.text[target..]
                            .split('\n')
                            .next()
                            .unwrap_or("")
                            .char_indices()
                            .nth(column)
                            .map_or_else(
                                || self.text[target..].split('\n').next().unwrap_or("").len(),
                                |(i, _)| i,
                            );
                }
            }
            _ => {}
        }
    }
    /// Handle vi-style normal, insert, search and command modes.
    /// 处理 vi 风格普通、插入、搜索及命令模式。
    /// # Arguments
    /// - `self`, `key`, `workspace`: Editor, key and file view. 编辑器、按键及文件视图。
    fn key(&mut self, key: char, workspace: &Workspace) -> Result<bool> {
        if key == '\x1b' || key == '\x03' {
            self.insert = false;
            self.command = None;
            return Ok(false);
        }
        if let Some(mut command) = self.command.take() {
            match key {
                '\n' => {
                    if let Some(search) = command.strip_prefix('/') {
                        self.search = search.into();
                        self.find();
                    } else {
                        match command.as_str() {
                            ":w" => {
                                if let Err(e) = self.save(workspace) {
                                    self.message = e.to_string();
                                }
                            }
                            ":wq" => match self.save(workspace) {
                                Ok(()) => return Ok(true),
                                Err(e) => self.message = e.to_string(),
                            },
                            ":q!" => return Ok(true),
                            ":q" => {
                                if self.original.as_deref() == Some(self.text.as_bytes())
                                    || self.original.is_none() && self.text.is_empty()
                                {
                                    return Ok(true);
                                }
                                self.message =
                                    "有未保存内容；使用 :w 或 :q! / unsaved changes; use :w or :q!"
                                        .into();
                            }
                            _ => {
                                self.message =
                                    "不支持的编辑命令 / unsupported editor command".into()
                            }
                        }
                    }
                }
                '\x7f' | '\x08' => {
                    command.pop();
                    self.command = Some(command);
                }
                ch if !ch.is_control() => {
                    command.push(ch);
                    self.command = Some(command);
                }
                _ => self.command = Some(command),
            }
            return Ok(false);
        }
        if matches!(key, '↑' | '↓' | '←' | '→') {
            self.movement(match key {
                '↑' => 'k',
                '↓' => 'j',
                '←' => 'h',
                _ => 'l',
            });
            return Ok(false);
        }
        if self.insert {
            match key {
                '\x7f' | '\x08' => {
                    if self.cursor > 0 {
                        self.remember();
                        let end = self.cursor;
                        self.movement('h');
                        self.text.drain(self.cursor..end);
                    }
                }
                ch if (!ch.is_control() || matches!(ch, '\n' | '\t'))
                    && self.text.len() + ch.len_utf8() <= 1024 * 1024 =>
                {
                    self.remember();
                    self.text.insert(self.cursor, ch);
                    self.cursor += ch.len_utf8();
                }
                _ => {}
            }
            return Ok(false);
        }
        match key {
            'i' => self.insert = true,
            'a' => {
                self.movement('l');
                self.insert = true;
            }
            'o' => {
                self.remember();
                self.cursor = self.text[self.cursor..]
                    .find('\n')
                    .map_or(self.text.len(), |i| self.cursor + i);
                self.text.insert(self.cursor, '\n');
                self.cursor += 1;
                self.insert = true;
            }
            'h' | 'j' | 'k' | 'l' => self.movement(key),
            '0' => self.cursor = self.text[..self.cursor].rfind('\n').map_or(0, |i| i + 1),
            '$' => {
                self.cursor = self.text[self.cursor..]
                    .find('\n')
                    .map_or(self.text.len(), |i| self.cursor + i)
            }
            'x' => {
                if self.cursor < self.text.len() {
                    self.remember();
                    let end =
                        self.cursor + self.text[self.cursor..].chars().next().unwrap().len_utf8();
                    self.text.drain(self.cursor..end);
                }
            }
            'u' => {
                if let Some((text, cursor)) = self.undo.pop() {
                    self.text = text;
                    self.cursor = cursor;
                }
            }
            ':' | '/' => self.command = Some(key.to_string()),
            'n' => self.find(),
            _ => {}
        }
        Ok(false)
    }
    /// Search forward with wraparound using a literal UTF-8 pattern.
    /// 使用字面 UTF-8 模式向前循环搜索。
    /// # Arguments
    /// - `self`: Editor with search pattern. 持有搜索模式的编辑器。
    fn find(&mut self) {
        if self.search.is_empty() {
            return;
        }
        let start = self.cursor
            + self.text[self.cursor..]
                .chars()
                .next()
                .map_or(0, char::len_utf8);
        if let Some(index) = self.text[start..]
            .find(&self.search)
            .map(|i| start + i)
            .or_else(|| self.text[..start].find(&self.search))
        {
            self.cursor = index;
        } else {
            self.message = "未找到 / not found".into();
        }
    }
    /// Draw text with Unicode terminal widths and visible control characters.
    /// 按 Unicode 终端宽度绘制文本，控制字符可见化。
    /// # Arguments
    /// - `self`, `cols`, `rows`: Editor and terminal dimensions. 编辑器及终端尺寸。
    fn draw(&mut self, cols: usize, rows: usize) -> String {
        let before = &self.text[..self.cursor];
        let row = before.bytes().filter(|b| *b == b'\n').count();
        let column = before.rsplit('\n').next().unwrap_or("").width();
        let height = rows - 2;
        if row < self.top {
            self.top = row;
        }
        if row >= self.top + height {
            self.top = row + 1 - height;
        }
        let left = column.saturating_sub(cols - 1);
        let mut output = String::from("\x1b[2J\x1b[H\x1b[?25l");
        let lines = self
            .text
            .split('\n')
            .skip(self.top)
            .take(height)
            .map(|line| {
                let mut width = 0;
                let visible: String = line
                    .chars()
                    .filter(|ch| {
                        let start = width;
                        width += ch.width().unwrap_or(1);
                        start >= left
                    })
                    .collect();
                clip(&clean(&visible).replace('\t', "    "), cols)
            })
            .collect::<Vec<_>>();
        output.push_str(&lines.join("\r\n"));
        let status = format!(
            "{} {} {}:{}",
            if self.insert {
                "INSERT / 插入"
            } else {
                "NORMAL / 普通"
            },
            self.path,
            row + 1,
            column + 1
        );
        let bottom = self.command.as_deref().unwrap_or(&self.message);
        output.push_str(&format!(
            "\x1b[{};1H\x1b[7m{}\x1b[0m\x1b[{};1H{}\x1b[{};{}H\x1b[?25h",
            rows - 1,
            clip(&status, cols),
            rows,
            clip(&clean(bottom), cols),
            if self.command.is_some() {
                rows
            } else {
                row - self.top + 1
            },
            if self.command.is_some() {
                bottom.width().min(cols - 1) + 1
            } else {
                column.saturating_sub(left) + 1
            }
        ));
        output
    }
}
/// Keep management text from injecting terminal controls.
/// 防止管理文本注入终端控制字符。
/// # Arguments
/// - `text`: Untrusted textual output. 不可信文本输出。
pub fn clean(text: &str) -> String {
    text.chars()
        .flat_map(|ch| {
            if ch.is_control() && !matches!(ch, '\n' | '\t') {
                ch.escape_default().collect::<Vec<_>>()
            } else {
                vec![ch]
            }
        })
        .collect()
}
/// Clip rendered text by terminal cells without splitting UTF-8.
/// 按终端单元裁剪文本，不拆分 UTF-8。
/// # Arguments
/// - `text`, `columns`: Text and maximum visible width. 文本及最大可见宽度。
fn clip(text: &str, columns: usize) -> String {
    let mut width = 0;
    text.chars()
        .take_while(|ch| {
            width += ch.width().unwrap_or(1);
            width <= columns
        })
        .collect()
}
