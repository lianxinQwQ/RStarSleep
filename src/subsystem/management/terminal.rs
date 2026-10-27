//! Management terminal with literal text, bounded rendering and explicit view ownership.
//! 管理终端，保留文本原义，限制绘制开销，并明确界面所有权。
pub use super::dashboard::LiveView;
use super::{
    Error, Result, Workspace, completion,
    dashboard::{Dashboard, Selection},
    editor::Editor,
    input::{self, Key},
};
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
struct Paste {
    bytes: Vec<u8>,
    rejected: bool,
}
pub struct Terminal {
    id: String,
    cwd: String,
    line: String,
    cursor: usize,
    history: Vec<String>,
    history_index: usize,
    draft: (String, usize),
    pending: Vec<u8>,
    mode: Mode,
    cols: usize,
    rows: usize,
    dashboard: Dashboard,
    paste: Option<Paste>,
    invalid: bool,
    dirty: bool,
    editor_return: bool,
    skip_lf: bool,
}
pub const HELP: &str = "help | clear (Ctrl+L) | history | pwd | cd [path] | ls [path] | cat path\nmkdir path | rm path | rmdir path | mv source target\nedit path (vi/vim) | check | build [status|stop] | jobs | status\nstart [--follow] | stop [--follow] | restart [--follow]\nlogs [boot|shutdown|build] [--follow] [--lines N] [--task ID] | console\nshell | tui | exit\n仅访问 build/script；保存后显式构建；不支持管道、重定向或展开。\nOnly build/script are accessible; build explicitly after saving. No pipes, redirection or expansion.\n方向键、Home/End、Delete、Ctrl+A/E/B/F/U/K/W 编辑；Tab 补全。\nArrows, Home/End, Delete and Ctrl+A/E/B/F/U/K/W edit; Tab completes.\n编辑 / Edit: i/a/o 插入 / insert, Esc 普通 / normal, h/j/k/l 移动 / move, x 删除 / delete, u 撤销 / undo, / 搜索 / search, n 下一处 / next, :w :q :wq :q!\n粘贴按文本处理；CLI 拒绝多行粘贴。 / Paste is literal; CLI rejects multiline paste.";

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
    /// Start an independent session; history never crosses authentication boundaries.
    /// 创建独立会话；历史记录不跨越认证边界。
    pub fn new(id: &str) -> Self {
        Self {
            id: id.into(),
            cwd: "/".into(),
            line: String::new(),
            cursor: 0,
            history: Vec::new(),
            history_index: 0,
            draft: (String::new(), 0),
            pending: Vec::new(),
            mode: Mode::Cli,
            cols: 80,
            rows: 24,
            dashboard: Dashboard::default(),
            paste: None,
            invalid: false,
            dirty: false,
            editor_return: false,
            skip_lf: false,
        }
    }
    pub fn welcome(&mut self) -> Vec<u8> {
        [format!("\x1b[?2004h\x1b[36mRStarSleep / {}\x1b[0m\r\n管理终端 / Management terminal  ·  help / tui\r\n", self.id).into_bytes(), self.draw()].concat()
    }
    pub fn cwd(&self) -> &str {
        &self.cwd
    }
    pub fn is_tui(&self) -> bool {
        matches!(self.mode, Mode::Tui)
    }
    pub fn live_view(&self) -> Option<LiveView> {
        if self.is_tui() {
            self.dashboard.live_view()
        } else {
            None
        }
    }
    pub fn update_live(
        &mut self,
        source: Option<(std::path::PathBuf, u64)>,
        status: &str,
    ) -> Result<Vec<u8>> {
        if self
            .dashboard
            .update_live(source, status, self.cols.saturating_sub(1))?
        {
            Ok(self.draw())
        } else {
            Ok(Vec::new())
        }
    }
    pub fn take_pending(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.pending)
    }
    /// Update dimensions even while another view owns the terminal.
    /// 即使终端由其他界面接管，仍更新尺寸。
    pub fn set_size(&mut self, cols: u32, rows: u32) {
        self.cols = (cols as usize).clamp(1, 1000);
        self.rows = (rows as usize).clamp(1, 1000);
    }
    pub fn resize(&mut self, cols: u32, rows: u32) -> Vec<u8> {
        self.set_size(cols, rows);
        self.draw()
    }
    /// Restore the active view without moving a CLI error to the screen origin.
    /// 恢复当前界面，不把 CLI 错误移到屏幕原点。
    pub fn finish(&mut self, text: &str) -> Vec<u8> {
        match &mut self.mode {
            Mode::Tui => {
                self.dashboard.finish(text);
                self.draw()
            }
            Mode::Edit(editor) => {
                editor.message = clean(text);
                self.draw()
            }
            Mode::Cli => {
                let mut out = b"\x1b[?2004h\x1b[0m\x1b[?25h".to_vec();
                if !text.is_empty() {
                    out.extend(clean(text).replace('\n', "\r\n").as_bytes());
                    if !text.ends_with('\n') {
                        out.extend(b"\r\n");
                    }
                }
                out.extend(self.draw());
                out
            }
        }
    }
    /// Display errors within their owning view and preserve editable state.
    /// 在所属界面显示错误，并保留可编辑状态。
    pub fn error(&mut self, text: &str) -> Vec<u8> {
        if matches!(self.mode, Mode::Cli) {
            let mut out = self.draw();
            out.extend(b"\r\n");
            out.extend(self.finish(text));
            out
        } else {
            self.finish(text)
        }
    }
    pub fn input(&mut self, data: &[u8], workspace: &Workspace) -> Result<Update> {
        if self.pending.len() + data.len() > 2 * 1024 * 1024 {
            self.pending.clear();
            self.invalid = true;
            return Ok(Update { output: self.error("终端输入超出限制；本条命令已作废 / terminal input exceeds limit; command invalidated"), action: None });
        }
        self.pending.extend_from_slice(data);
        let mut consumed = 0;
        let mut output = Vec::new();
        while consumed < self.pending.len() {
            if self.paste.is_some() {
                const END: &[u8] = b"\x1b[201~";
                let remaining = &self.pending[consumed..];
                let end = remaining.windows(END.len()).position(|v| v == END);
                let count = end.unwrap_or_else(|| remaining.len().saturating_sub(END.len() - 1));
                let paste = self.paste.as_mut().unwrap();
                if paste.bytes.len() + count > 1024 * 1024 {
                    paste.rejected = true;
                    paste.bytes.clear();
                }
                if !paste.rejected {
                    paste.bytes.extend_from_slice(&remaining[..count]);
                }
                consumed += count;
                if end.is_some() {
                    consumed += END.len();
                    let paste = self.paste.take().unwrap();
                    let result = self.apply_paste(paste);
                    if let Err(error) = result {
                        output.extend(self.reject(&error.to_string()));
                    }
                    self.dirty = true;
                    continue;
                }
                break;
            }
            let decoded = input::decode(&self.pending[consumed..]);
            let mut event = match decoded {
                Ok(Some(event)) => event,
                Ok(None) => break,
                Err(error) => {
                    consumed = self.pending.len();
                    output.extend(self.reject(&error.to_string()));
                    break;
                }
            };
            if matches!(self.mode, Mode::Tui) {
                if let Key::Text(text) = &event.1 {
                    let ch = text.chars().next().unwrap();
                    event = (ch.len_utf8(), Key::Text(ch.to_string()));
                }
            }
            let was_cr = self.pending[consumed] == b'\r';
            if self.skip_lf && self.pending[consumed] == b'\n' {
                consumed += event.0;
                self.skip_lf = false;
                continue;
            }
            self.skip_lf = was_cr && event.0 == 1;
            consumed += event.0;
            if event.1 == Key::PasteStart {
                self.paste = Some(Paste {
                    bytes: Vec::new(),
                    rejected: false,
                });
                continue;
            }
            let update = match self.key(event.1, workspace) {
                Ok(update) => update,
                Err(error) => Update {
                    output: self.error(&format!("操作失败 / operation failed: {error}")),
                    action: None,
                },
            };
            output.extend(update.output);
            if update.action.is_some() {
                self.pending.drain(..consumed);
                return Ok(Update {
                    output,
                    action: update.action,
                });
            }
        }
        self.pending.drain(..consumed);
        if self.dirty {
            output.extend(self.draw());
        }
        Ok(Update {
            output,
            action: None,
        })
    }
    pub fn flush_escape(&mut self, workspace: &Workspace) -> Result<Update> {
        if self.paste.is_none() && self.pending == [27] {
            self.pending.clear();
            let mut update = self.key(Key::Escape, workspace)?;
            if self.dirty {
                update.output.extend(self.draw());
            }
            Ok(update)
        } else {
            Ok(Update {
                output: Vec::new(),
                action: None,
            })
        }
    }
    fn reject(&mut self, message: &str) -> Vec<u8> {
        if matches!(self.mode, Mode::Cli) {
            self.invalid = true;
            self.line.clear();
            self.cursor = 0;
        }
        self.error(message)
    }
    fn apply_paste(&mut self, paste: Paste) -> Result<()> {
        if paste.rejected {
            return Err(Error::Invalid(
                "粘贴超出 1 MiB 限制，未插入 / paste exceeds 1 MiB; nothing inserted".into(),
            ));
        }
        let text = String::from_utf8(paste.bytes).map_err(|_| {
            Error::Invalid("粘贴不是 UTF-8，未插入 / paste is not UTF-8; nothing inserted".into())
        })?;
        match &mut self.mode {
            Mode::Cli => {
                if text.chars().any(char::is_control) {
                    return Err(Error::Invalid("CLI 不支持多行或控制字符粘贴；本条命令已作废 / CLI rejects multiline or control-character paste; command invalidated".into()));
                }
                self.insert_line(&text)?;
            }
            Mode::Edit(editor) => {
                if text.chars().any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t')) {
                    return Err(Error::Invalid("粘贴包含不支持的控制字符，未插入 / paste contains unsupported controls; nothing inserted".into()));
                }
                editor.insert_text(&text)?;
            }
            Mode::Tui => return Err(Error::Invalid("面板不执行粘贴快捷键，请在 CLI 或编辑器粘贴 / dashboard does not execute pasted shortcuts; paste in CLI or editor".into())),
        }
        Ok(())
    }
    fn prompt(&self) -> String {
        format!("{}:{}> ", self.id, self.cwd)
    }
    fn insert_line(&mut self, text: &str) -> Result<()> {
        if self.invalid {
            return Ok(());
        }
        if text.chars().any(char::is_control) {
            self.invalid = true;
            self.line.clear();
            self.cursor = 0;
            return Err(Error::Invalid("命令包含控制字符；本条命令已作废 / command contains control characters; command invalidated".into()));
        }
        if self.line.len() + text.len() > 32768 {
            self.invalid = true;
            self.line.clear();
            self.cursor = 0;
            return Err(Error::Invalid("命令超出 32768 字节；本条命令已作废 / command exceeds 32768 bytes; command invalidated".into()));
        }
        self.line.insert_str(self.cursor, text);
        self.cursor += text.len();
        self.dirty = true;
        Ok(())
    }
    fn key(&mut self, key: Key, workspace: &Workspace) -> Result<Update> {
        let empty = || Update {
            output: Vec::new(),
            action: None,
        };
        if let Mode::Edit(editor) = &mut self.mode {
            let quit = editor.key(key, workspace)?;
            self.dirty = true;
            if quit {
                self.mode = if self.editor_return {
                    Mode::Tui
                } else {
                    Mode::Cli
                };
                self.editor_return = false;
                return Ok(Update {
                    output: if self.is_tui() {
                        Vec::new()
                    } else {
                        b"\x1b[?1049l\x1b[?25h\x1b[0m".to_vec()
                    },
                    action: None,
                });
            }
            return Ok(empty());
        }
        if matches!(self.mode, Mode::Tui) {
            let selection = match self.dashboard.key(key, workspace) {
                Ok(selection) => selection,
                Err(error) => {
                    self.dashboard
                        .error(&format!("操作失败 / operation failed: {error}"));
                    None
                }
            };
            self.dirty = true;
            return match selection {
                Some(Selection::Cli) => {
                    self.mode = Mode::Cli;
                    Ok(Update {
                        output: b"\x1b[?1049l\x1b[?25h".to_vec(),
                        action: None,
                    })
                }
                Some(Selection::Edit(path)) => {
                    let command = format!("edit '{}'", path.replace('\'', "'\\''"));
                    let result = self.command(&command, workspace);
                    if matches!(self.mode, Mode::Edit(_)) {
                        self.editor_return = true;
                    }
                    result
                }
                Some(Selection::Command(words)) => {
                    if self.dashboard.live_view().is_none() {
                        self.dashboard
                            .finish(&format!("正在执行 / Running: {}", words.join(" ")));
                    }
                    let output = self.draw();
                    Ok(Update {
                        output,
                        action: Some(if words == ["shell"] {
                            Action::Shell
                        } else {
                            Action::Command(words)
                        }),
                    })
                }
                None => Ok(empty()),
            };
        }
        if self.invalid && !matches!(key, Key::Enter | Key::Control(3)) {
            return Ok(empty());
        }
        match key {
            Key::Enter => {
                let mut output = self.draw();
                output.extend(b"\r\n");
                let line = std::mem::take(&mut self.line);
                self.cursor = 0;
                if self.invalid {
                    self.invalid = false;
                    output.extend(self.finish(
                        "本条命令已作废，请重新输入 / command invalidated; enter it again",
                    ));
                    return Ok(Update {
                        output,
                        action: None,
                    });
                }
                if !line.trim().is_empty() && self.history.last() != Some(&line) {
                    self.history.push(line.clone());
                    if self.history.len() > 200 {
                        self.history.remove(0);
                    }
                }
                self.history_index = self.history.len();
                self.draft = (String::new(), 0);
                let update = match self.command(&line, workspace) {
                    Ok(update) => update,
                    Err(error) => Update {
                        output: self.finish(&format!("操作失败 / operation failed: {error}")),
                        action: None,
                    },
                };
                output.extend(update.output);
                return Ok(Update {
                    output,
                    action: update.action,
                });
            }
            Key::Control(3) => {
                self.line.clear();
                self.cursor = 0;
                self.invalid = false;
                self.history_index = self.history.len();
                self.draft = (String::new(), 0);
                return Ok(Update {
                    output: self.error("^C"),
                    action: None,
                });
            }
            Key::Control(4) if self.line.is_empty() => {
                return Ok(Update {
                    output: Vec::new(),
                    action: Some(Action::Exit),
                });
            }
            Key::Text(text) => self.insert_line(&text)?,
            Key::Left | Key::Control(2) => self.cursor = previous(&self.line, self.cursor),
            Key::Right | Key::Control(6) => self.cursor = next(&self.line, self.cursor),
            Key::Home | Key::Control(1) => self.cursor = 0,
            Key::End | Key::Control(5) => self.cursor = self.line.len(),
            Key::Backspace if self.cursor > 0 => {
                let start = previous(&self.line, self.cursor);
                self.line.drain(start..self.cursor);
                self.cursor = start;
            }
            Key::Delete | Key::Control(4) if self.cursor < self.line.len() => {
                self.line.drain(self.cursor..next(&self.line, self.cursor));
            }
            Key::Control(21) => {
                self.line.drain(..self.cursor);
                self.cursor = 0;
            }
            Key::Control(11) => self.line.truncate(self.cursor),
            Key::Control(23) => {
                let mut start = self.cursor;
                while start > 0
                    && self.line[..start]
                        .chars()
                        .next_back()
                        .unwrap()
                        .is_whitespace()
                {
                    start = previous(&self.line, start);
                }
                while start > 0
                    && !self.line[..start]
                        .chars()
                        .next_back()
                        .unwrap()
                        .is_whitespace()
                {
                    start = previous(&self.line, start);
                }
                self.line.drain(start..self.cursor);
                self.cursor = start;
            }
            Key::Control(12) => {
                self.dirty = true;
                return Ok(Update {
                    output: b"\x1b[2J\x1b[H".to_vec(),
                    action: None,
                });
            }
            Key::Up => {
                if self.history_index == self.history.len() {
                    self.draft = (self.line.clone(), self.cursor);
                }
                self.history_index = self.history_index.saturating_sub(1);
                if let Some(line) = self.history.get(self.history_index) {
                    self.line = line.clone();
                    self.cursor = self.line.len();
                }
            }
            Key::Down => {
                if self.history_index == self.history.len() {
                    return Ok(empty());
                }
                self.history_index = (self.history_index + 1).min(self.history.len());
                if self.history_index == self.history.len() {
                    self.line = self.draft.0.clone();
                    self.cursor = self.draft.1;
                } else {
                    self.line = self.history[self.history_index].clone();
                    self.cursor = self.line.len();
                }
            }
            Key::Tab => {
                let completion =
                    completion::complete(&self.line, self.cursor, &self.cwd, workspace)?;
                self.line = completion.line;
                self.cursor = completion.cursor;
                if self.line.len() > 32768 {
                    self.invalid = true;
                    self.line.clear();
                    self.cursor = 0;
                    return Err(Error::Invalid("补全后命令超出限制，命令已作废 / completed command exceeds limit; command invalidated".into()));
                }
                if completion.choices.len() > 1 {
                    let mut output = self.draw();
                    output.extend(b"\r\n");
                    for choice in completion.choices {
                        output.extend(clean(&choice).as_bytes());
                        output.extend(b"\r\n");
                    }
                    self.dirty = true;
                    return Ok(Update {
                        output,
                        action: None,
                    });
                }
            }
            _ => {}
        }
        self.dirty = true;
        Ok(empty())
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
                let original = match workspace.read_limited(&path, 1024 * 1024) {
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
                self.dirty = true;
                return Ok(Update {
                    output: b"\x1b[?1049h".to_vec(),
                    action: None,
                });
            }
            ["tui"] => {
                self.mode = Mode::Tui;
                self.dirty = true;
                return Ok(Update {
                    output: b"\x1b[?1049h".to_vec(),
                    action: None,
                });
            }
            ["clear"] => {
                self.dirty = true;
                return Ok(Update {
                    output: b"\x1b[2J\x1b[H".to_vec(),
                    action: None,
                });
            }
            ["history"] => {
                text = self
                    .history
                    .iter()
                    .enumerate()
                    .map(|(i, value)| format!("{}  {}", i + 1, value))
                    .collect::<Vec<_>>()
                    .join("\n")
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
    fn draw(&mut self) -> Vec<u8> {
        self.dirty = false;
        match &mut self.mode {
            Mode::Tui => self.dashboard.draw(&self.id, self.cols, self.rows),
            Mode::Edit(editor) => editor.draw(self.cols, self.rows).into_bytes(),
            Mode::Cli => {
                let prompt = clip(&self.prompt(), self.cols.saturating_sub(3));
                let available = self.cols.saturating_sub(prompt.width() + 1).max(1);
                let column = self.line[..self.cursor].width();
                let left = column.saturating_sub(available - 1);
                let (visible, actual_left) = window_text(&self.line, left, available);
                let cursor = prompt.width() + column.saturating_sub(actual_left);
                format!(
                    "\r\x1b[2K{prompt}{visible}\r{}",
                    if cursor > 0 {
                        format!("\x1b[{cursor}C")
                    } else {
                        String::new()
                    }
                )
                .into_bytes()
            }
        }
    }
}
fn previous(text: &str, cursor: usize) -> usize {
    text[..cursor]
        .char_indices()
        .next_back()
        .map_or(0, |(i, _)| i)
}
fn next(text: &str, cursor: usize) -> usize {
    cursor + text[cursor..].chars().next().map_or(0, char::len_utf8)
}

/// Escape terminal controls without changing source bytes.
/// 转义终端控制字符，不修改源字节。
pub fn clean(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        if ch.is_control() && ch != '\n' {
            out.extend(ch.escape_default());
        } else {
            out.push(ch);
        }
    }
    out
}
/// Clip by display cells without splitting UTF-8 or wrapping the physical row.
/// 按显示单元截断，不拆分 UTF-8，也不折到下一物理行。
pub(super) fn clip(text: &str, columns: usize) -> String {
    let mut width = 0;
    text.chars()
        .take_while(|ch| {
            width += ch.width().unwrap_or(1);
            width <= columns
        })
        .collect()
}
/// A horizontal viewport uses character boundaries and returns its actual left cell.
/// 水平视口按字符边界裁剪，并返回实际起始显示单元。
pub(super) fn window_text(text: &str, left: usize, columns: usize) -> (String, usize) {
    let mut width = 0;
    let mut actual_left = 0;
    let mut output = String::new();
    let mut started = false;
    for ch in text.chars() {
        let size = ch.width().unwrap_or(1);
        if !started && width < left {
            width += size;
            actual_left = width;
            continue;
        }
        started = true;
        if width + size > actual_left + columns {
            break;
        }
        output.push(ch);
        width += size;
    }
    (output, actual_left)
}
