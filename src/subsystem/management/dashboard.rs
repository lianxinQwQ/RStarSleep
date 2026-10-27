//! Responsive management dashboard with real actions, a file browser and scrollable results.
//! 自适应管理面板，提供实际操作、文件浏览及可滚动结果。
use super::log_view::LogView;
use super::terminal::{clean, clip};
use super::{Result, Workspace, input::Key};
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LiveView {
    Build,
    Console,
}

pub(super) enum Selection {
    Command(Vec<String>),
    Edit(String),
    Cli,
}

#[derive(Default)]
pub(super) struct Dashboard {
    files: bool,
    selected: usize,
    directory: String,
    entries: Vec<(String, bool)>,
    result: String,
    scroll: usize,
    confirm: Option<String>,
    result_rows: usize,
    wrapped: Vec<String>,
    wrapped_width: usize,
    live: Option<LiveView>,
    log: Option<LogView>,
    status: String,
    notice: String,
    tail: bool,
}

const ACTIONS: &[(&str, &str)] = &[
    ("status", "状态 / Status"),
    ("check", "检查配置 / Check configuration"),
    ("jobs", "后台任务 / Jobs"),
    ("build status", "构建 / Build"),
    ("start", "启动 / Start"),
    ("stop", "停止 / Stop"),
    ("restart", "重启 / Restart"),
    ("console", "实时控制台 / Live console"),
    ("shell", "Root shell"),
];

impl Dashboard {
    pub fn live_view(&self) -> Option<LiveView> {
        if self.files { None } else { self.live }
    }

    pub fn update_live(
        &mut self,
        source: Option<(PathBuf, u64)>,
        status: &str,
        width: usize,
    ) -> Result<bool> {
        let mut changed = self.status != status;
        self.status = status.into();
        if self
            .log
            .as_ref()
            .map(|l| l.source())
            .map(|(p, o)| (p.clone(), o))
            != source
        {
            self.log = source.map(|(path, base)| LogView::new(path, base));
            self.scroll = 0;
            self.tail = true;
            changed = true;
        }
        if let Some(log) = &mut self.log {
            changed |= log.refresh(width)?;
        }
        Ok(changed)
    }

    pub fn finish(&mut self, text: &str) {
        if self.live.is_some() {
            self.notice = clean(text);
            return;
        }
        self.result = if text.is_empty() {
            "操作完成 / Operation completed".into()
        } else {
            clean(text)
        };
        self.scroll = 0;
        self.wrapped_width = 0;
    }

    pub fn error(&mut self, text: &str) {
        self.finish(text);
        self.confirm = None;
    }

    fn refresh(&mut self, workspace: &Workspace) -> Result<()> {
        if self.directory.is_empty() {
            self.directory = "/".into();
        }
        let mut entries = Vec::new();
        for name in workspace.list(&self.directory)? {
            let path = Workspace::normalize(&self.directory, &name)?;
            let metadata = std::fs::symlink_metadata(workspace.path(&path)?)?;
            if metadata.is_dir() || metadata.is_file() {
                entries.push((name, metadata.is_dir()));
            }
        }
        entries.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        self.entries = entries;
        self.selected = self.selected.min(self.entries.len().saturating_sub(1));
        Ok(())
    }

    pub fn key(&mut self, key: Key, workspace: &Workspace) -> Result<Option<Selection>> {
        if let Some(command) = self.confirm.take() {
            if matches!(key, Key::Text(ref text) if text == "y" || text == "Y") {
                return Ok(Some(Selection::Command(vec![command])));
            }
            self.finish("已取消 / Cancelled");
            return Ok(None);
        }
        match key {
            Key::Control(3) if self.live_view() == Some(LiveView::Build) => {
                return Ok(Some(Selection::Command(vec![
                    "build".into(),
                    "stop".into(),
                ])));
            }
            Key::Control(3) if self.live_view().is_some() => {
                self.live = None;
                self.log = None;
                self.finish("已关闭实时视图 / Live view closed");
            }
            Key::Escape | Key::Control(3) => return Ok(Some(Selection::Cli)),
            Key::Text(ref s) if s == "q" => return Ok(Some(Selection::Cli)),
            Key::Tab => {
                self.files = !self.files;
                self.selected = 0;
                if self.files {
                    self.refresh(workspace)?;
                }
            }
            Key::Text(ref s) if s == "e" => {
                self.files = true;
                self.selected = 0;
                self.refresh(workspace)?;
            }
            Key::Text(ref s) if s == "f" => {
                if self.files {
                    self.refresh(workspace)?;
                }
            }
            Key::Text(ref s) if s == "n" && self.live_view() == Some(LiveView::Build) => {
                self.notice.clear();
                return Ok(Some(Selection::Command(vec!["build".into()])));
            }
            Key::Up => self.selected = self.selected.saturating_sub(1),
            Key::Down => {
                self.selected = (self.selected + 1).min(
                    if self.files {
                        self.entries.len()
                    } else {
                        ACTIONS.len()
                    }
                    .saturating_sub(1),
                )
            }
            Key::PageUp => {
                self.tail = false;
                self.scroll = self.scroll.saturating_sub(8);
            }
            Key::PageDown => {
                self.scroll = (self.scroll + 8).min(self.result_rows.saturating_sub(1));
                self.tail = false;
            }
            Key::Home => {
                self.scroll = 0;
                self.tail = false;
            }
            Key::End => {
                self.scroll = self.result_rows.saturating_sub(1);
                self.tail = true;
            }
            Key::Backspace if self.files => {
                if self.directory != "/" {
                    self.directory = Workspace::normalize(&self.directory, "..")?;
                    self.selected = 0;
                    self.refresh(workspace)?;
                }
            }
            Key::Enter if self.files => {
                if let Some((name, directory)) = self.entries.get(self.selected).cloned() {
                    let path = Workspace::normalize(&self.directory, &name)?;
                    if directory {
                        self.directory = path;
                        self.selected = 0;
                        self.refresh(workspace)?;
                    } else {
                        return Ok(Some(Selection::Edit(path)));
                    }
                }
            }
            Key::Enter => return Ok(self.select(ACTIONS[self.selected].0)),
            Key::Text(ref text) => {
                let command = match text.as_str() {
                    "s" => "status",
                    "v" => "check",
                    "j" => "jobs",
                    "b" | "l" => "build status",
                    "u" => "start",
                    "d" => "stop",
                    "r" => "restart",
                    "c" => "console",
                    "h" => "shell",
                    _ => return Ok(None),
                };
                return Ok(self.select(command));
            }
            _ => {}
        }
        Ok(None)
    }

    fn select(&mut self, command: &str) -> Option<Selection> {
        let live = match command {
            "build status" => Some(LiveView::Build),
            "console" => Some(LiveView::Console),
            _ => None,
        };
        if self.live != live {
            self.log = None;
            self.status.clear();
            self.scroll = 0;
            self.tail = true;
        }
        self.live = live;
        self.files = false;
        self.notice.clear();
        if matches!(command, "stop" | "restart") {
            self.confirm = Some(command.into());
            None
        } else {
            Some(Selection::Command(
                command.split_whitespace().map(str::to_owned).collect(),
            ))
        }
    }

    pub fn draw(&mut self, id: &str, cols: usize, rows: usize) -> Vec<u8> {
        let width = cols.saturating_sub(1);
        let mut lines: Vec<(String, &str)> = vec![
            (format!(" RStarSleep  /  {id}"), "\x1b[1;36m"),
            (" 管理工作区 / Management workspace".into(), "\x1b[90m"),
            (
                if self.files {
                    format!(" 文件 / Files  {}   [Tab] 操作 / Actions", self.directory)
                } else {
                    " 操作 / Actions   [Tab] 文件 / Files".into()
                },
                "\x1b[1m",
            ),
        ];
        if rows < 10 || cols < 30 {
            lines.push((" 窗口较小 / Small terminal".into(), "\x1b[33m"));
            lines.push((" q: CLI   s: status   e: files".into(), ""));
        } else {
            let count = if self.files {
                self.entries.len()
            } else {
                ACTIONS.len()
            };
            let menu_height = (rows / 3).clamp(2, 10).min(count.max(1));
            let top = self.selected.saturating_sub(menu_height - 1);
            for index in top..(top + menu_height).min(count) {
                let label = if self.files {
                    let (name, directory) = &self.entries[index];
                    format!("{}{}", clean(name), if *directory { "/" } else { "" })
                } else {
                    ACTIONS[index].1.to_string()
                };
                lines.push((
                    format!(
                        " {} {label}",
                        if index == self.selected { "›" } else { " " }
                    ),
                    if index == self.selected {
                        "\x1b[30;46m"
                    } else {
                        ""
                    },
                ));
            }
            if count == 0 {
                lines.push((" 空目录 / Empty directory".into(), "\x1b[90m"));
            }
            lines.push((
                match self.live {
                    Some(LiveView::Build) => " ── 构建 / Build ──  PgUp/PgDn  Home/End".into(),
                    Some(LiveView::Console) => {
                        " ── 实时控制台 / Live console ──  PgUp/PgDn  Home/End".into()
                    }
                    None => " ── 结果 / Result ──  PgUp/PgDn 滚动 / Scroll".into(),
                },
                "\x1b[36m",
            ));
            if self.live.is_some() {
                lines.push((format!(" {}", clean(&self.status)), "\x1b[33m"));
                if !self.notice.is_empty() {
                    lines.push((format!(" {}", self.notice), "\x1b[33m"));
                }
            }
            let available = rows.saturating_sub(lines.len() + 2);
            if self.live.is_some() {
                if let Some(log) = &self.log {
                    self.result_rows = log.rows();
                    let last = self.result_rows.saturating_sub(available.max(1));
                    self.scroll = if self.tail {
                        last
                    } else {
                        self.scroll.min(last)
                    };
                    match log.page(self.scroll, available) {
                        Ok(page) => lines.extend(page.into_iter().map(|line| (line, ""))),
                        Err(error) => lines.push((
                            format!("读取日志失败 / Failed to read log: {error}"),
                            "\x1b[31m",
                        )),
                    }
                } else {
                    lines.push((" 暂无日志 / No log yet".into(), "\x1b[90m"));
                }
            } else {
                let result = if self.result.is_empty() {
                    "选择操作并按 Enter / Select an action and press Enter"
                } else {
                    &self.result
                };
                if self.wrapped_width != width {
                    self.wrapped = result.lines().flat_map(|line| wrap(line, width)).collect();
                    self.wrapped_width = width;
                }
                self.result_rows = self.wrapped.len();
                let top = self
                    .scroll
                    .min(self.wrapped.len().saturating_sub(available.max(1)));
                for line in self.wrapped.iter().skip(top).take(available) {
                    lines.push((line.clone(), ""));
                }
            }
        }
        let mut output = String::from("\x1b[?25l\x1b[2J\x1b[H");
        for (index, (line, color)) in lines.into_iter().take(rows.saturating_sub(1)).enumerate() {
            if index > 0 {
                output.push_str("\r\n");
            }
            output.push_str(color);
            output.push_str(&clip(&line, width));
            output.push_str("\x1b[0m");
        }
        let footer = if let Some(command) = &self.confirm {
            format!(" 确认 {command}？y 确认，其他键取消 / Confirm {command}? y / cancel")
        } else {
            match self.live {
                Some(LiveView::Build) => {
                    " b 构建页 / Build  n 开始 / Start  Ctrl+C 停止 / Stop  c Console  q CLI".into()
                }
                Some(LiveView::Console) => {
                    " b 构建 / Build  c 控制台 / Console  Ctrl+C 关闭视图 / Close view  q CLI"
                        .into()
                }
                None => " ↑↓ 选择 / Select  Enter 打开 / Open  b Build  c Console  e Files  q CLI"
                    .into(),
            }
        };
        output.push_str(&format!(
            "\x1b[{rows};1H\x1b[7m{}\x1b[0m",
            clip(&footer, width)
        ));
        output.into_bytes()
    }
}

/// Wrap long output so every result remains readable in a narrow panel.
/// 折行较长输出，使窄面板中的每条结果仍可阅读。
fn wrap(text: &str, width: usize) -> Vec<String> {
    use unicode_width::UnicodeWidthChar;
    let mut lines = vec![String::new()];
    let mut cells = 0;
    for ch in text.chars() {
        let size = ch.width().unwrap_or(1);
        if cells + size > width.max(2) {
            lines.push(String::new());
            cells = 0;
        }
        lines.last_mut().unwrap().push(ch);
        cells += size;
    }
    lines
}
