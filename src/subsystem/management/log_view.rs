//! Disk-backed, incrementally indexed log pages; terminal controls never escape the panel.
//! 由磁盘支持、增量索引的日志页；终端控制符绝不越出面板。
use super::Result;
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::PathBuf,
};
use unicode_width::UnicodeWidthStr;

pub(super) struct LogView {
    path: PathBuf,
    base: u64,
    offset: u64,
    rows: Vec<u64>,
    width: usize,
    cells: usize,
    text_bytes: usize,
    scanner: Scanner,
    skip_start: Option<u64>,
    hidden: Vec<Hidden>,
}

struct Hidden {
    start: u64,
    end: u64,
    scanner: Scanner,
}

#[derive(Clone, Default)]
struct Scanner {
    escape: u8,
    cr: bool,
}
enum Token {
    Skip,
    Newline,
    Text(String),
}

impl Scanner {
    fn next(&mut self, bytes: &[u8]) -> Option<(usize, Token)> {
        let b = *bytes.first()?;
        if self.escape != 0 {
            self.escape = match self.escape {
                1 => match b {
                    b'[' => 2,
                    b']' | b'P' | b'^' | b'_' => 3,
                    _ => 0,
                },
                2 => {
                    if (0x40..=0x7e).contains(&b) {
                        0
                    } else {
                        2
                    }
                }
                3 => match b {
                    7 => 0,
                    27 => 4,
                    _ => 3,
                },
                _ => {
                    if b == b'\\' {
                        0
                    } else {
                        3
                    }
                }
            };
            return Some((1, Token::Skip));
        }
        let cr = self.cr;
        self.cr = b == b'\r';
        match b {
            27 => {
                self.escape = 1;
                Some((1, Token::Skip))
            }
            b'\r' => Some((1, Token::Newline)),
            b'\n' => Some((1, if cr { Token::Skip } else { Token::Newline })),
            b'\t' => Some((1, Token::Text("\t".into()))),
            0..=31 | 127 => Some((1, Token::Text(format!("\\x{b:02x}")))),
            _ => {
                let n = match b {
                    0..=127 => 1,
                    0xc2..=0xdf => 2,
                    0xe0..=0xef => 3,
                    0xf0..=0xf4 => 4,
                    _ => 1,
                };
                if bytes.len() < n {
                    return None;
                }
                match std::str::from_utf8(&bytes[..n]) {
                    Ok(s) => Some((
                        n,
                        Token::Text(if s.chars().any(char::is_control) {
                            s.escape_default().to_string()
                        } else {
                            s.into()
                        }),
                    )),
                    Err(_) => Some((1, Token::Text(format!("\\x{b:02x}")))),
                }
            }
        }
    }
}

impl LogView {
    pub fn new(path: PathBuf, base: u64) -> Self {
        Self {
            path,
            base,
            offset: base,
            rows: vec![base],
            width: 0,
            cells: 0,
            text_bytes: 0,
            scanner: Scanner::default(),
            skip_start: None,
            hidden: Vec::new(),
        }
    }
    pub fn source(&self) -> (&PathBuf, u64) {
        (&self.path, self.base)
    }
    pub fn rows(&self) -> usize {
        self.rows.len()
    }

    /// Index at most 512 KiB per tick, retaining offsets instead of log contents.
    /// 每次最多索引 512 KiB，只保留偏移而非日志内容。
    pub fn refresh(&mut self, width: usize) -> Result<bool> {
        let width = width.max(4);
        let mut changed = false;
        if self.width != width {
            self.offset = self.base;
            self.rows = vec![self.base];
            self.cells = 0;
            self.text_bytes = 0;
            self.scanner = Scanner::default();
            self.skip_start = None;
            self.hidden.clear();
            self.width = width;
            changed = true;
        }
        let mut file = match File::open(&self.path) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(changed),
            Err(e) => return Err(e.into()),
        };
        if file.metadata()?.len() < self.offset {
            return Err(super::Error::Invalid(
                "日志被截断 / log was truncated".into(),
            ));
        }
        file.seek(SeekFrom::Start(self.offset))?;
        let mut bytes = Vec::new();
        file.take(512 * 1024).read_to_end(&mut bytes)?;
        let mut index = 0;
        while let Some((used, token)) = self.scanner.next(&bytes[index..]) {
            let position = self.offset + index as u64;
            if !matches!(token, Token::Skip) {
                self.skip_start = None;
            }
            match token {
                Token::Newline => {
                    self.rows.push(position + used as u64);
                    self.cells = 0;
                    self.text_bytes = 0;
                }
                Token::Text(text) => {
                    let mut size = if text == "\t" {
                        4 - self.cells % 4
                    } else {
                        text.width()
                    };
                    if self.cells + size > width || self.text_bytes > width * 16 + 64 {
                        self.rows.push(position);
                        self.cells = 0;
                        self.text_bytes = 0;
                        if text == "\t" {
                            size = 4;
                        }
                    }
                    self.cells += size;
                    self.text_bytes += used;
                }
                Token::Skip => {
                    // Seek past long hidden sequences instead of rescanning them on every redraw.
                    // 直接跳过较长的隐藏序列，避免每次重绘都重新扫描。
                    let start = *self.skip_start.get_or_insert(position);
                    let end = position + used as u64;
                    if end - start > 4096 {
                        if let Some(hidden) = self.hidden.last_mut().filter(|h| h.start == start) {
                            hidden.end = end;
                            hidden.scanner = self.scanner.clone();
                        } else {
                            self.hidden.push(Hidden {
                                start,
                                end,
                                scanner: self.scanner.clone(),
                            });
                        }
                    }
                    if bytes[index] == b'\n' && self.rows.last().copied() == Some(position) {
                        *self.rows.last_mut().unwrap() += 1;
                    }
                }
            }
            index += used;
        }
        self.offset += index as u64;
        Ok(changed || index > 0)
    }

    pub fn page(&self, top: usize, height: usize) -> Result<Vec<String>> {
        let top = top.min(self.rows.len().saturating_sub(1));
        let end = self.rows.get(top + height).copied().unwrap_or(self.offset);
        let mut file = match File::open(&self.path) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };
        file.seek(SeekFrom::Start(self.rows[top]))?;
        let mut input = std::io::BufReader::new(file);
        let mut scanner = Scanner::default();
        let mut lines = vec![String::new()];
        let mut cells = 0;
        let mut offset = self.rows[top];
        let mut row_index = top + 1;
        let mut pending = Vec::new();
        let mut buffer = [0; 8192];
        let mut read_offset = offset;
        let mut hidden_index = self.hidden.partition_point(|h| h.end <= offset);
        while read_offset < end {
            let hidden = self.hidden.get(hidden_index);
            if let Some(hidden) = hidden.filter(|h| h.start <= read_offset) {
                read_offset = hidden.end.min(end);
                input.seek(SeekFrom::Start(read_offset))?;
                offset = read_offset;
                scanner = hidden.scanner.clone();
                hidden_index += 1;
                continue;
            }
            let limit = hidden.map_or(end, |h| h.start.min(end)) - read_offset;
            let n = input.by_ref().take(limit).read(&mut buffer)?;
            if n == 0 {
                break;
            }
            read_offset += n as u64;
            pending.extend_from_slice(&buffer[..n]);
            let mut used = 0;
            while let Some((count, token)) = scanner.next(&pending[used..]) {
                if self.rows.get(row_index).copied() == Some(offset + used as u64) {
                    if !matches!(token, Token::Newline) && !lines.last().unwrap().is_empty() {
                        lines.push(String::new());
                        cells = 0;
                    }
                    row_index += 1;
                }
                match token {
                    Token::Newline => {
                        lines.push(String::new());
                        cells = 0;
                    }
                    Token::Text(mut text) => {
                        let tab = text == "\t";
                        if tab {
                            text = " ".repeat(4 - cells % 4);
                        }
                        if cells + text.width() > self.width {
                            lines.push(String::new());
                            cells = 0;
                            if tab {
                                text = "    ".into();
                            }
                        }
                        cells += text.width();
                        lines.last_mut().unwrap().push_str(&text);
                    }
                    Token::Skip => {}
                }
                used += count;
            }
            pending.drain(..used);
            offset += used as u64;
        }
        lines.truncate(height);
        Ok(lines)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn paging_wraps_unicode_and_strips_terminal_controls_without_losing_rows() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all("\x1b[31m第一行\x1b[0m\r\nsecond-line\nlast".as_bytes())
            .unwrap();
        let mut view = LogView::new(file.path().into(), 0);
        view.refresh(6).unwrap();
        assert_eq!(
            view.page(0, 8).unwrap(),
            ["第一行", "second", "-line", "last"]
        );
        assert_eq!(view.page(1, 2).unwrap(), ["second", "-line"]);
        assert_eq!(view.page(2, 2).unwrap(), ["-line", "last"]);
        assert_eq!(view.page(3, 1).unwrap(), ["last"]);
    }

    #[test]
    fn tabs_wrap_consistently_when_a_page_starts_at_the_tab() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(b"12345\tABC").unwrap();
        let mut view = LogView::new(file.path().into(), 0);
        view.refresh(6).unwrap();
        assert_eq!(view.page(0, 3).unwrap(), ["12345", "    AB", "C"]);
        assert_eq!(view.page(1, 1).unwrap(), ["    AB"]);
        assert_eq!(view.page(2, 1).unwrap(), ["C"]);
    }

    #[test]
    fn long_hidden_sequences_are_seekable_without_creating_empty_log_rows() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(b"before\x1b]").unwrap();
        file.write_all(&vec![b'x'; 2 * 1024 * 1024]).unwrap();
        let mut view = LogView::new(file.path().into(), 0);
        while view.refresh(80).unwrap() {}
        assert_eq!(view.page(0, 2).unwrap(), ["before"]);
        file.write_all(b"\x1b\\after\nlast").unwrap();
        view.refresh(80).unwrap();
        assert_eq!(view.hidden.len(), 1);
        assert_eq!(view.page(0, 3).unwrap(), ["beforeafter", "last"]);
        assert_eq!(view.page(1, 1).unwrap(), ["last"]);
    }

    #[test]
    fn fragmented_utf8_and_escape_sequences_resume_after_append() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(b"first\r").unwrap();
        let mut view = LogView::new(file.path().into(), 0);
        view.refresh(80).unwrap();
        file.write_all(b"\n\x1b[3").unwrap();
        view.refresh(80).unwrap();
        file.write_all(b"2m\xe4\xbd").unwrap();
        view.refresh(80).unwrap();
        file.write_all(b"\xa0\x1b[0m\nlast").unwrap();
        view.refresh(80).unwrap();
        assert_eq!(view.page(0, 10).unwrap(), ["first", "你", "last"]);
        assert_eq!(view.page(1, 2).unwrap(), ["你", "last"]);
    }

    #[test]
    fn large_logs_retain_the_beginning_and_the_end_on_disk() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(b"BEGIN\n").unwrap();
        for _ in 0..100_000 {
            file.write_all(b"0123456789012345678901234567890123456789\n")
                .unwrap();
        }
        file.write_all(b"END").unwrap();
        let mut view = LogView::new(file.path().into(), 0);
        while view.refresh(80).unwrap() {}
        assert_eq!(view.page(0, 1).unwrap(), ["BEGIN"]);
        assert_eq!(view.page(view.rows() - 1, 1).unwrap(), ["END"]);
        assert!(view.offset > 4_000_000);
    }
}
