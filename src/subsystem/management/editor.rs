//! UTF-8 editor with batched insertion and independent key events.
//! 支持批量插入和独立按键事件的 UTF-8 编辑器。
use super::terminal::{clean, clip, window_text};
use super::{Error, Result, Workspace, input::Key};
use unicode_width::UnicodeWidthStr;

pub(super) struct Editor {
    pub path: String,
    pub text: String,
    pub original: Option<Vec<u8>>,
    pub cursor: usize,
    pub insert: bool,
    pub command: Option<String>,
    pub undo: Vec<(String, usize)>,
    pub message: String,
    pub top: usize,
    pub search: String,
}
impl Editor {
    /// Insert a whole text batch as one undoable operation, preserving literal characters.
    /// 将整批文本作为一次可撤销操作插入，保留字面字符。
    pub fn insert_text(&mut self, text: &str) -> Result<()> {
        if self.text.len() + text.len() > 1024 * 1024 {
            return Err(Error::Invalid(
                "编辑器超出 1 MiB 限制，未插入 / editor exceeds 1 MiB; nothing inserted".into(),
            ));
        }
        if !text.is_empty() {
            self.remember();
            self.text.insert_str(self.cursor, text);
            self.cursor += text.len();
        }
        Ok(())
    }

    pub fn key(&mut self, key: Key, workspace: &Workspace) -> Result<bool> {
        match key {
            Key::Text(text) => {
                let mut offset = 0;
                while offset < text.len() {
                    if self.insert && self.command.is_none() {
                        self.insert_text(&text[offset..])?;
                        break;
                    }
                    let ch = text[offset..].chars().next().unwrap();
                    offset += ch.len_utf8();
                    if self.character(ch, workspace)? {
                        return Ok(true);
                    }
                }
            }
            Key::Left => self.movement('h'),
            Key::Right => self.movement('l'),
            Key::Up => self.movement('k'),
            Key::Down => self.movement('j'),
            Key::Home => self.cursor = self.text[..self.cursor].rfind('\n').map_or(0, |i| i + 1),
            Key::End => {
                self.cursor = self.text[self.cursor..]
                    .find('\n')
                    .map_or(self.text.len(), |i| self.cursor + i)
            }
            Key::Delete if self.command.is_none() => {
                if self.cursor < self.text.len() {
                    self.remember();
                    let end =
                        self.cursor + self.text[self.cursor..].chars().next().unwrap().len_utf8();
                    self.text.drain(self.cursor..end);
                }
            }
            Key::Enter if self.insert && self.command.is_none() => self.insert_text("\n")?,
            Key::Tab if self.insert && self.command.is_none() => self.insert_text("\t")?,
            Key::Enter => return self.character('\n', workspace),
            Key::Backspace => return self.character('\x7f', workspace),
            Key::Escape | Key::Control(3) => return self.character('\x1b', workspace),
            _ => {}
        }
        Ok(false)
    }

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
    fn character(&mut self, key: char, workspace: &Workspace) -> Result<bool> {
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
                if self.text.len() == 1024 * 1024 {
                    return Err(Error::Invalid(
                        "编辑器超出 1 MiB 限制，未插入 / editor exceeds 1 MiB; nothing inserted"
                            .into(),
                    ));
                }
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
    pub fn draw(&mut self, cols: usize, rows: usize) -> String {
        if rows < 3 || cols < 2 {
            return format!(
                "\x1b[2J\x1b[H{}\x1b[?25h",
                clip("窗口过小 / Terminal too small", cols.saturating_sub(1))
            );
        }
        let before = &self.text[..self.cursor];
        let row = before.bytes().filter(|b| *b == b'\n').count();
        let column = clean(before.rsplit('\n').next().unwrap_or("")).width();
        let height = rows.saturating_sub(2).max(1);
        if row < self.top {
            self.top = row;
        }
        if row >= self.top + height {
            self.top = row + 1 - height;
        }
        let left = column.saturating_sub(cols.saturating_sub(2));
        let current_line = self.text[..self.cursor].rfind('\n').map_or(0, |i| i + 1);
        let current_line = self.text[current_line..].split('\n').next().unwrap_or("");
        let cursor_left = window_text(&clean(current_line), left, cols.saturating_sub(1)).1;
        let mut output = String::from("\x1b[2J\x1b[H\x1b[?25l");
        let lines = self
            .text
            .split('\n')
            .skip(self.top)
            .take(height)
            .map(|line| window_text(&clean(line), left, cols.saturating_sub(1)).0)
            .collect::<Vec<_>>();
        output.push_str(&lines.join("\r\n"));
        let status = format!(
            "{} {}{} {}:{}",
            if self.insert {
                "INSERT / 插入"
            } else {
                "NORMAL / 普通"
            },
            self.path,
            if self.original.as_deref().unwrap_or_default() != self.text.as_bytes() {
                " [+]"
            } else {
                ""
            },
            row + 1,
            column + 1
        );
        let bottom = self.command.as_deref().unwrap_or(&self.message);
        output.push_str(&format!(
            "\x1b[{};1H\x1b[7m{}\x1b[0m\x1b[{};1H{}\x1b[{};{}H\x1b[?25h",
            rows - 1,
            clip(&clean(&status), cols.saturating_sub(1)),
            rows,
            clip(&clean(bottom), cols.saturating_sub(1)),
            if self.command.is_some() {
                rows
            } else {
                row - self.top + 1
            },
            if self.command.is_some() {
                clean(bottom).width().min(cols - 1) + 1
            } else {
                (column.saturating_sub(cursor_left) + 1).min(cols)
            }
        ));
        output
    }
}
