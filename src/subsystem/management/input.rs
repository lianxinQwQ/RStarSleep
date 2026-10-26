//! Transport-independent key decoding with literal UTF-8 and atomic bracketed paste.
//! 与传输无关的按键解码，保留 UTF-8 原文并原子处理括号粘贴。
use super::{Error, Result};

#[derive(Debug, PartialEq)]
pub(super) enum Key {
    Text(String),
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    Delete,
    PageUp,
    PageDown,
    Enter,
    Tab,
    Backspace,
    Escape,
    Control(u8),
    PasteStart,
    Ignore,
}

/// Decode one complete event; incomplete UTF-8 and escape sequences remain buffered.
/// 解码一个完整事件；不完整的 UTF-8 和转义序列留在缓冲区。
pub(super) fn decode(bytes: &[u8]) -> Result<Option<(usize, Key)>> {
    let Some(&first) = bytes.first() else {
        return Ok(None);
    };
    if first == 27 {
        if bytes.len() == 1 {
            return Ok(None);
        }
        if bytes[1] == b'[' || bytes[1] == b'O' {
            let Some(end) = bytes
                .iter()
                .enumerate()
                .skip(2)
                .find(|(_, b)| (b'@'..=b'~').contains(b))
                .map(|(i, _)| i)
            else {
                if bytes.len() > 32 {
                    return Err(Error::Invalid(
                        "终端转义序列过长 / terminal escape sequence is too long".into(),
                    ));
                }
                return Ok(None);
            };
            let key = match &bytes[..=end] {
                b"\x1b[A" | b"\x1bOA" => Key::Up,
                b"\x1b[B" | b"\x1bOB" => Key::Down,
                b"\x1b[C" | b"\x1bOC" => Key::Right,
                b"\x1b[D" | b"\x1bOD" => Key::Left,
                b"\x1b[H" | b"\x1bOH" | b"\x1b[1~" | b"\x1b[7~" => Key::Home,
                b"\x1b[F" | b"\x1bOF" | b"\x1b[4~" | b"\x1b[8~" => Key::End,
                b"\x1b[3~" => Key::Delete,
                b"\x1b[5~" => Key::PageUp,
                b"\x1b[6~" => Key::PageDown,
                b"\x1b[200~" => Key::PasteStart,
                _ => Key::Ignore,
            };
            return Ok(Some((end + 1, key)));
        }
        return Ok(Some((1, Key::Escape)));
    }
    let control = match first {
        b'\r' => Some((if bytes.get(1) == Some(&b'\n') { 2 } else { 1 }, Key::Enter)),
        b'\n' => Some((1, Key::Enter)),
        b'\t' => Some((1, Key::Tab)),
        8 | 127 => Some((1, Key::Backspace)),
        0..=31 => Some((1, Key::Control(first))),
        _ => None,
    };
    if let Some(event) = control {
        return Ok(Some(event));
    }
    let end = bytes
        .iter()
        .position(|b| *b < 32 || *b == 127)
        .unwrap_or(bytes.len());
    let text = match std::str::from_utf8(&bytes[..end]) {
        Ok(text) => text,
        Err(error) if error.error_len().is_none() => {
            if error.valid_up_to() == 0 {
                return Ok(None);
            }
            std::str::from_utf8(&bytes[..error.valid_up_to()]).unwrap()
        }
        Err(_) => {
            return Err(Error::Invalid(
                "终端输入不是 UTF-8 / terminal input is not UTF-8".into(),
            ));
        }
    };
    Ok(Some((text.len(), Key::Text(text.into()))))
}
