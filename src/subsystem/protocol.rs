//! Bounded local requests and lossless application streams.
//! 有界本地请求与无损应用流。
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    io::{self, BufRead, Write},
};

pub const PROTOCOL_VERSION: u32 = 1;
pub const MAX_FRAME_BYTES: usize = 128 * 1024;
pub const STREAM_CHUNK_BYTES: usize = 8192;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub version: u32,
    pub operation: Operation,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Operation {
    List,
    Status {
        id: String,
    },
    Start {
        id: String,
    },
    Stop {
        id: String,
    },
    Restart {
        id: String,
    },
    Exec {
        id: String,
        argv: Vec<String>,
        environment: BTreeMap<String, String>,
        pty: bool,
        window: Option<WindowSize>,
    },
    Open {
        id: String,
        target: String,
        args: Vec<String>,
        environment: BTreeMap<String, String>,
        desktop: bool,
        detach: bool,
    },
    SyncExports {
        id: String,
    },
    Guest {
        action: String,
        path: Option<String>,
        packages: Vec<String>,
    },
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WindowSize {
    pub rows: u16,
    pub cols: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ClientMessage {
    Request { request: Request },
    Input { bytes: Vec<u8> },
    Eof,
    Signal { signal: i32 },
    Resize { window: WindowSize },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StartOrigin {
    Explicit,
    Autostart,
    Demand,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Status {
    pub id: String,
    pub machine: Option<String>,
    pub version: Option<String>,
    pub running: bool,
    pub origin: Option<StartOrigin>,
    pub leases: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Event {
    Ready { machine: String },
    Result { statuses: Vec<Status> },
    Stdout { bytes: Vec<u8> },
    Stderr { bytes: Vec<u8> },
    Exit { code: i32 },
    Error { message: String },
    Message { text: String },
}

/// Wrap an operation in the current protocol version.
/// 将操作包装到当前协议版本。
/// # Arguments
/// - `operation`: Validated desired operation. 经校验目标操作。
pub fn request(operation: Operation) -> ClientMessage {
    ClientMessage::Request {
        request: Request {
            version: PROTOCOL_VERSION,
            operation,
        },
    }
}

/// Read a JSON line before allocating beyond the frame limit.
/// 在超出帧限制分配前读取 JSON 行。
/// # Arguments
/// - `reader`: Buffered Unix transport. 缓冲 Unix 传输。
pub fn read_frame<T: serde::de::DeserializeOwned>(
    reader: &mut impl BufRead,
) -> io::Result<Option<T>> {
    let mut bytes = Vec::with_capacity(256);
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return if bytes.is_empty() {
                Ok(None)
            } else {
                Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "消息截断 / truncated message",
                ))
            };
        }
        let newline = available.iter().position(|v| *v == b'\n');
        let count = newline.map_or(available.len(), |i| i + 1);
        if bytes.len().saturating_add(count) > MAX_FRAME_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "消息过大 / message too large",
            ));
        }
        bytes.extend_from_slice(&available[..count]);
        reader.consume(count);
        if newline.is_some() {
            bytes.pop();
            return serde_json::from_slice(&bytes)
                .map(Some)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e));
        }
    }
}

/// Flush one bounded structured response or input event.
/// 同步一个有界结构响应或输入事件。
/// # Arguments
/// - `writer`: Transport writer. 传输写入器。
/// - `value`: Complete message. 完整消息。
pub fn write_frame<T: Serialize>(writer: &mut impl Write, value: &T) -> io::Result<()> {
    let mut bytes =
        serde_json::to_vec(value).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    if bytes.len() + 1 > MAX_FRAME_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "消息过大 / message too large",
        ));
    }
    bytes.push(b'\n');
    writer.write_all(&bytes)?;
    writer.flush()
}

/// Enforce version and command limits before starting privileged work.
/// 特权操作开始前强制版本与命令限制。
/// # Arguments
/// - `request`: Untrusted peer request. 不可信对端请求。
pub fn validate_request(request: &Request) -> io::Result<()> {
    if request.version != PROTOCOL_VERSION {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "协议版本不支持 / unsupported protocol version",
        ));
    }
    match &request.operation {
        Operation::List => {}
        Operation::Status { id }
        | Operation::Start { id }
        | Operation::Stop { id }
        | Operation::Restart { id }
        | Operation::SyncExports { id } => validate_id(id)?,
        Operation::Exec {
            id,
            argv,
            environment,
            window,
            ..
        } => {
            validate_id(id)?;
            validate_argv(argv)?;
            validate_environment(environment)?;
            if let Some(w) = window {
                validate_window(*w)?;
            }
        }
        Operation::Open {
            id,
            target,
            args,
            environment,
            ..
        } => {
            validate_id(id)?;
            validate_argv(&[vec![target.clone()], args.clone()].concat())?;
            validate_environment(environment)?;
        }
        Operation::Guest {
            action,
            path,
            packages,
        } => {
            if !matches!(
                action.as_str(),
                "add-copy" | "add-mount" | "migrate" | "install" | "rebuild" | "restart"
            ) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "未知 guest 操作 / unknown guest action",
                ));
            }
            if path
                .as_ref()
                .is_some_and(|p| p.len() > 4096 || p.contains('\0'))
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "guest 路径无效 / invalid guest path",
                ));
            }
            if packages.len() > 256
                || packages.iter().any(|p| {
                    p.is_empty() || p.starts_with('-') || p.chars().any(char::is_whitespace)
                })
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "包请求无效 / invalid package request",
                ));
            }
        }
    }
    Ok(())
}

/// Validate a single managed-system component.
/// 校验单个被管理系统分量。
/// # Arguments
/// - `id`: Untrusted system ID. 不可信系统标识。
pub fn validate_id(id: &str) -> io::Result<()> {
    if id.len() > 64 || crate::config::validate_identifier(id).is_err() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "子系统标识无效 / invalid subsystem identifier",
        ));
    }
    Ok(())
}
/// Bound terminal dimensions supplied by the peer.
/// 限制对端提供的终端尺寸。
/// # Arguments
/// - `window`: Desired rows and columns. 目标行列数。
pub fn validate_window(window: WindowSize) -> io::Result<()> {
    if window.rows == 0 || window.cols == 0 || window.rows > 1000 || window.cols > 1000 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "终端尺寸无效 / invalid terminal dimensions",
        ));
    }
    Ok(())
}
/// Bound literal argument vectors without interpreting shell code.
/// 限制字面参数数组，不解析 shell 代码。
/// # Arguments
/// - `argv`: Requested program and arguments. 请求程序与参数。
fn validate_argv(argv: &[String]) -> io::Result<()> {
    if argv.is_empty()
        || argv[0].is_empty()
        || argv.len() > 256
        || argv.iter().any(|s| s.contains('\0'))
        || argv.iter().map(String::len).sum::<usize>() > 32768
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "命令参数无效 / invalid command arguments",
        ));
    }
    Ok(())
}
/// Bound environment data before the session allowlist filters it.
/// 会话允许列表过滤前限制环境数据。
/// # Arguments
/// - `env`: Caller environment. 调用者环境。
fn validate_environment(env: &BTreeMap<String, String>) -> io::Result<()> {
    if env.len() > 32
        || env.iter().any(|(k, v)| {
            k.is_empty()
                || k.len() > 128
                || !k.bytes().enumerate().all(|(i, c)| {
                    c == b'_' || c.is_ascii_alphabetic() || (i > 0 && c.is_ascii_digit())
                })
                || v.len() > 8192
                || v.contains('\0')
        })
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "会话环境无效 / invalid session environment",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    /// Preserve binary output and reject malformed or unbounded input.
    /// 保留二进制输出并拒绝损坏或无界输入。
    /// # Arguments
    /// No parameters. 无参数。
    #[test]
    fn bounded_binary_frames() {
        let event = Event::Stdout {
            bytes: vec![0, 255, 10, 128],
        };
        let mut buf = Vec::new();
        write_frame(&mut buf, &event).unwrap();
        assert_eq!(
            read_frame::<Event>(&mut Cursor::new(buf)).unwrap(),
            Some(event)
        );
        assert!(read_frame::<Event>(&mut Cursor::new(b"{")).is_err());
        assert!(read_frame::<Event>(&mut Cursor::new(vec![b'x'; MAX_FRAME_BYTES + 1])).is_err());
        for id in ["", "..", "../other", "a/b", "-option"] {
            assert!(validate_id(id).is_err());
        }
        assert!(
            validate_request(&Request {
                version: 2,
                operation: Operation::List
            })
            .is_err()
        );
    }
}
