//! Forward and archive build diagnostics as bytes arrive while retaining process results.
//! 在保留进程结果的同时，即时转发并存档构建诊断。
use super::logging;
use std::{
    fs::File,
    io::{self, IsTerminal, Read, Write},
    os::fd::{AsRawFd, FromRawFd, RawFd},
    process::{Command, Output, Stdio},
    thread,
};

/// Stream diagnostics to stderr, keeping query stdout private for protocol parsing.
/// 向 stderr 即时输出诊断，将查询 stdout 保留给协议解析。
pub(super) fn stream(command: &mut Command, query: bool) -> io::Result<Output> {
    // Diagnostics are displayed on stderr, including the child's stdout.
    // 诊断显示在 stderr，包括子进程的 stdout。
    let terminal = io::stderr().is_terminal().then_some(libc::STDERR_FILENO);
    stream_to(command, (!query).then(io::stderr), io::stderr(), terminal)
}

/// Service invocation control on the caller while reader threads drain diagnostics.
pub(super) fn stream_with_control(
    command: &mut Command,
    query: bool,
    poll: &mut dyn FnMut() -> io::Result<()>,
) -> io::Result<Output> {
    let terminal = io::stderr().is_terminal().then_some(libc::STDERR_FILENO);
    stream_to_with_control(
        command,
        (!query).then(io::stderr),
        io::stderr(),
        terminal,
        Some(poll),
    )
}

/// Keep terminal-aware tools producing progress without changing captured bytes.
/// 让感知终端的工具生成进度，同时保持捕获的原始字节。
struct TerminalReader {
    master: File,
    source: RawFd,
}

fn window_size(source: RawFd) -> libc::winsize {
    let mut size = libc::winsize {
        ws_row: 24,
        ws_col: 80,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // A terminal without dimensions still needs a usable progress-bar width.
    // 尚未设置尺寸的终端仍需提供可用的进度条宽度。
    unsafe { libc::ioctl(source, libc::TIOCGWINSZ, &mut size) };
    if size.ws_row == 0 {
        size.ws_row = 24;
    }
    if size.ws_col == 0 {
        size.ws_col = 80;
    }
    size
}

impl Read for TerminalReader {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        loop {
            // Follow resizes even while the child is temporarily silent.
            // 子进程暂时无输出时仍同步终端尺寸变化。
            let size = window_size(self.source);
            if unsafe { libc::ioctl(self.master.as_raw_fd(), libc::TIOCSWINSZ, &size) } < 0 {
                return Err(io::Error::last_os_error());
            }
            let mut ready = libc::pollfd {
                fd: self.master.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            match unsafe { libc::poll(&mut ready, 1, 100) } {
                -1 => return Err(io::Error::last_os_error()),
                0 => continue,
                _ => break,
            }
        }
        match self.master.read(buffer) {
            // Linux reports EIO after the last slave descriptor closes.
            // Linux 在最后一个 slave 描述符关闭后以 EIO 表示结束。
            Err(error) if error.raw_os_error() == Some(libc::EIO) => Ok(0),
            result => result,
        }
    }
}

fn terminal_pipe(source: RawFd) -> io::Result<(TerminalReader, File)> {
    let (mut master, mut slave) = (-1, -1);
    let size = window_size(source);
    if unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null(),
            &size,
        )
    } < 0
    {
        return Err(io::Error::last_os_error());
    }
    let master = unsafe { File::from_raw_fd(master) };
    let slave = unsafe { File::from_raw_fd(slave) };
    for file in [&master, &slave] {
        if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
            return Err(io::Error::last_os_error());
        }
    }
    let mut attributes = unsafe { std::mem::zeroed() };
    if unsafe { libc::tcgetattr(slave.as_raw_fd(), &mut attributes) } < 0 {
        return Err(io::Error::last_os_error());
    }
    // Disable newline translation; stdin remains /dev/null rather than this PTY.
    // 禁止换行转换；stdin 仍为 /dev/null，不接入此 PTY。
    attributes.c_oflag &= !libc::OPOST;
    if unsafe { libc::tcsetattr(slave.as_raw_fd(), libc::TCSANOW, &attributes) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok((TerminalReader { master, source }, slave))
}

/// Drain a pipe even after a destination fails, keeping the other destination active.
/// 即使某一输出目标失败也继续读取管道，并保持另一输出目标工作。
fn drain(
    reader: impl Read,
    mut target: Option<impl Write>,
    mut archive: Option<impl Write>,
) -> io::Result<Vec<u8>> {
    let mut reader = reader;
    let mut captured = Vec::new();
    let mut buffer = [0; 8192];
    let mut write_error = None;
    loop {
        let count = match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(count) => count,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        captured.extend_from_slice(&buffer[..count]);
        if let Some(writer) = &mut target
            && let Err(error) = writer
                .write_all(&buffer[..count])
                .and_then(|()| writer.flush())
        {
            write_error.get_or_insert(error);
            target = None;
        }
        if let Some(writer) = &mut archive
            && let Err(error) = writer
                .write_all(&buffer[..count])
                .and_then(|()| writer.flush())
        {
            write_error.get_or_insert(error);
        }
    }
    match write_error {
        Some(error) => Err(error),
        None => Ok(captured),
    }
}

/// Read both streams concurrently to preserve progress updates and avoid full pipes.
/// 并发读取两个输出流，以保留进度更新并避免管道填满。
fn stream_to(
    command: &mut Command,
    stdout: Option<impl Write + Send>,
    stderr: impl Write + Send,
    terminal: Option<RawFd>,
) -> io::Result<Output> {
    stream_to_with_control(command, stdout, stderr, terminal, None)
}

fn stream_to_with_control(
    command: &mut Command,
    stdout: Option<impl Write + Send>,
    stderr: impl Write + Send,
    terminal: Option<RawFd>,
    mut poll: Option<&mut dyn FnMut() -> io::Result<()>>,
) -> io::Result<Output> {
    // Capture the calling build's log before moving work to reader threads.
    // 在将读取工作移交线程前捕获当前构建的日志。
    let archive = logging::writer();
    let stdout_archive = stdout.as_ref().map(|_| archive.clone());
    let (mut child, child_stdout, child_stderr): (_, Box<dyn Read + Send>, Box<dyn Read + Send>) =
        if let Some(source) = terminal {
            // Query stdout is a protocol pipe; stderr still needs a terminal for
            // download tools whose stdout is redirected there by the plugin.
            // 查询 stdout 使用协议管道；stderr 仍需为重定向到此的软件包工具提供终端。
            let (child_stdout, stdout_stdio) = if stdout.is_some() {
                let (reader, slave) = terminal_pipe(source)?;
                (Some(reader), Stdio::from(slave))
            } else {
                (None, Stdio::piped())
            };
            let (child_stderr, stderr_slave) = terminal_pipe(source)?;
            let spawned = command
                .stdin(Stdio::null())
                .stdout(stdout_stdio)
                .stderr(Stdio::from(stderr_slave))
                .spawn();
            // Command retains its Stdio descriptors after spawn. Close these copies
            // so the readers receive EOF when the child exits, including on failure.
            // Command 在 spawn 后保留 Stdio；关闭其副本，使子进程退出后读取能结束。
            command.stdout(Stdio::null()).stderr(Stdio::null());
            let mut child = spawned?;
            let child_stdout: Box<dyn Read + Send> = match child_stdout {
                Some(reader) => Box::new(reader),
                None => Box::new(child.stdout.take().expect("piped query stdout")),
            };
            (child, child_stdout, Box::new(child_stderr))
        } else {
            let mut child = command
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()?;
            let stdout = child.stdout.take().expect("piped stdout");
            let stderr = child.stderr.take().expect("piped stderr");
            (child, Box::new(stdout), Box::new(stderr))
        };
    thread::scope(|scope| {
        let stdout = scope.spawn(move || drain(child_stdout, stdout, stdout_archive));
        let stderr = scope.spawn(move || drain(child_stderr, Some(stderr), Some(archive)));
        let status = if let Some(poll) = &mut poll {
            loop {
                match child.try_wait() {
                    Ok(Some(status)) => break Ok(status),
                    Ok(None) => (),
                    Err(error) => {
                        let _ = child.kill();
                        let _ = child.wait();
                        break Err(error);
                    }
                }
                if let Err(error) = poll() {
                    let _ = child.kill();
                    let _ = child.wait();
                    break Err(error);
                }
                thread::sleep(std::time::Duration::from_millis(10));
            }
        } else {
            child.wait()
        };
        let stdout = stdout
            .join()
            .map_err(|_| io::Error::other("stdout reader panicked"))??;
        let stderr = stderr
            .join()
            .map_err(|_| io::Error::other("stderr reader panicked"))??;
        Ok(Output {
            status: status?,
            stdout,
            stderr,
        })
    })
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::{
        fs,
        path::PathBuf,
        sync::{Arc, Mutex},
    };

    struct RecordingWriter {
        bytes: Arc<Mutex<Vec<u8>>>,
        acknowledgement: PathBuf,
    }

    struct BrokenWriter;
    impl Write for BrokenWriter {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "destination closed",
            ))
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl Write for RecordingWriter {
        fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
            self.bytes.lock().unwrap().extend_from_slice(buffer);
            fs::write(&self.acknowledgement, b"received")?;
            Ok(buffer.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn control_runs_on_caller_while_large_output_is_drained() {
        use std::{cell::Cell, rc::Rc};
        let temporary = tempfile::tempdir().unwrap();
        let owner = thread::current().id();
        let calls = Rc::new(Cell::new(0));
        let mut command = Command::new("bash");
        command
            .args([
                "-c",
                r#"
head -c 262144 /dev/zero & head -c 262144 /dev/zero >&2 & wait
printf 'ready' > "$1/ready"
for (( i = 0; i < 500; i++ )); do
    [[ -f "$1/saved" ]] && exit 0
    sleep 0.01
done
exit 99
"#,
                "rstar-control-output",
            ])
            .arg(temporary.path());
        let mut poll = || {
            assert_eq!(thread::current().id(), owner);
            if temporary.path().join("ready").exists() && calls.get() == 0 {
                calls.set(1);
                fs::write(temporary.path().join("saved"), "saved")?;
            }
            Ok(())
        };
        let output = stream_to_with_control(
            &mut command,
            Some(io::sink()),
            io::sink(),
            None,
            Some(&mut poll),
        )
        .unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout.len(), 262144);
        assert_eq!(output.stderr.len(), 262144);
        assert_eq!(calls.get(), 1);
    }

    /// The child requires receipt of both partial lines before it can finish.
    /// 子进程在两个不完整行均被接收后才能完成。
    #[test]
    fn forwards_partial_lines_before_exit_and_keeps_failure_output() {
        let temporary = tempfile::tempdir().unwrap();
        let stdout = Arc::new(Mutex::new(Vec::new()));
        let stderr = Arc::new(Mutex::new(Vec::new()));
        let mut command = Command::new("bash");
        command
            .args([
                "-c",
                r#"
printf 'out-progress\r'
printf 'err-progress\r' >&2
for (( attempt = 0; attempt < 500; attempt++ )); do
    if [[ -f "$1/stdout" && -f "$1/stderr" ]]; then
        printf '\377done'
        printf '\376failed' >&2
        exit 7
    fi
    sleep 0.01
done
exit 99
"#,
                "rstarsleep-output-test",
            ])
            .arg(temporary.path());
        let output = stream_to(
            &mut command,
            Some(RecordingWriter {
                bytes: stdout.clone(),
                acknowledgement: temporary.path().join("stdout"),
            }),
            RecordingWriter {
                bytes: stderr.clone(),
                acknowledgement: temporary.path().join("stderr"),
            },
            None,
        )
        .unwrap();
        assert_eq!(output.status.code(), Some(7));
        assert_eq!(output.stdout, b"out-progress\r\xffdone");
        assert_eq!(output.stderr, b"err-progress\r\xfefailed");
        assert_eq!(*stdout.lock().unwrap(), output.stdout);
        assert_eq!(*stderr.lock().unwrap(), output.stderr);
    }

    /// Query records remain captured while diagnostics are visible before exit.
    /// 查询记录仍仅被捕获，诊断在退出前已可见。
    #[test]
    fn captures_query_stdout_and_streams_diagnostics() {
        let temporary = tempfile::tempdir().unwrap();
        let log = logging::BuildLog::new(temporary.path()).unwrap();
        let _logging = logging::enter([log.clone()]);
        let stderr = Arc::new(Mutex::new(Vec::new()));
        let mut command = Command::new("bash");
        command
            .args([
                "-c",
                r#"
[[ ! -t 0 && ! -t 1 && ! -t 2 ]] || exit 98
printf 'package\texplicit\n'
printf 'query-progress\r' >&2
for (( attempt = 0; attempt < 500; attempt++ )); do
    [[ -f "$1" ]] && exit 0
    sleep 0.01
done
exit 99
"#,
                "rstarsleep-query-test",
            ])
            .arg(temporary.path().join("stderr"));
        let output = stream_to(
            &mut command,
            None::<io::Sink>,
            RecordingWriter {
                bytes: stderr.clone(),
                acknowledgement: temporary.path().join("stderr"),
            },
            None,
        )
        .unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout, b"package\texplicit\n");
        assert_eq!(*stderr.lock().unwrap(), b"query-progress\r");
        log.publish(temporary.path()).unwrap();
        assert_eq!(
            fs::read(temporary.path().join("install.log")).unwrap(),
            b"query-progress\r"
        );
    }

    /// Predownload keeps JSON on a pipe while redirected package tools see a terminal.
    /// 预下载将 JSON 保留在管道中，同时让重定向的软件包工具感知终端。
    #[test]
    fn query_keeps_protocol_private_and_streams_terminal_download_progress() {
        let temporary = tempfile::tempdir().unwrap();
        let log = logging::BuildLog::new(temporary.path()).unwrap();
        let _logging = logging::enter([log.clone()]);
        let stderr = Arc::new(Mutex::new(Vec::new()));
        let mut command = Command::new("bash");
        command
            .args([
                "-c",
                r#"
[[ ! -t 0 && ! -t 1 && -t 2 ]] || exit 98
printf '{"schema_version":1,"systems":{}}\n'
bash -c '[[ -t 1 ]] || exit 97; printf "download 50%%\\r"' >&2 || exit $?
for (( attempt = 0; attempt < 500; attempt++ )); do
    if [[ -f "$1" ]]; then
        printf 'download 100%%\n' >&2
        exit 0
    fi
    sleep 0.01
done
exit 99
"#,
                "rstarsleep-query-terminal-test",
            ])
            .arg(temporary.path().join("stderr"));
        let output = stream_to(
            &mut command,
            None::<io::Sink>,
            RecordingWriter {
                bytes: stderr.clone(),
                acknowledgement: temporary.path().join("stderr"),
            },
            Some(libc::STDERR_FILENO),
        )
        .unwrap();
        assert!(output.status.success(), "{:?}", output.status);
        assert_eq!(output.stdout, b"{\"schema_version\":1,\"systems\":{}}\n");
        serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap();
        assert_eq!(output.stderr, b"download 50%\rdownload 100%\n");
        assert_eq!(*stderr.lock().unwrap(), output.stderr);
        log.publish(temporary.path()).unwrap();
        assert_eq!(
            fs::read(temporary.path().join("install.log")).unwrap(),
            output.stderr
        );
    }

    /// Both pipes can exceed their kernel buffers without blocking each other.
    /// 两条管道的内容均可超过内核缓冲区而不会相互阻塞。
    #[test]
    fn drains_large_stdout_and_stderr_concurrently() {
        for terminal in [None, Some(libc::STDERR_FILENO)] {
            let mut command = Command::new("bash");
            command.args([
                "-c",
                "head -c 262144 /dev/zero & head -c 262144 /dev/zero >&2 & wait",
            ]);
            let output = stream_to(&mut command, Some(io::sink()), io::sink(), terminal).unwrap();
            assert!(output.status.success());
            assert_eq!(output.stdout, vec![0; 262144]);
            assert_eq!(output.stderr, vec![0; 262144]);
        }
    }

    /// Terminal failures do not prevent draining or reaping the running child.
    /// 终端失败不会阻止排空管道或回收正在运行的子进程。
    #[test]
    fn drains_and_reaps_after_terminal_write_failure() {
        for terminal in [None, Some(libc::STDERR_FILENO)] {
            let temporary = tempfile::tempdir().unwrap();
            let log = logging::BuildLog::new(temporary.path()).unwrap();
            let _logging = logging::enter([log.clone()]);
            let mut command = Command::new("bash");
            command
                .args([
                    "-c",
                    "head -c 262144 /dev/zero; printf done > \"$1\"",
                    "rstarsleep-broken-output",
                ])
                .arg(temporary.path().join("completed"));
            let error =
                stream_to(&mut command, Some(BrokenWriter), io::sink(), terminal).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);
            assert_eq!(
                fs::read(temporary.path().join("completed")).unwrap(),
                b"done"
            );
            log.publish(temporary.path()).unwrap();
            assert_eq!(
                fs::read(temporary.path().join("install.log")).unwrap(),
                vec![0; 262144]
            );
        }
    }

    /// An archive failure does not interrupt console output or stop draining the pipe.
    /// 存档失败不会中断终端输出，也不会停止排空管道。
    #[test]
    fn drains_and_forwards_after_archive_write_failure() {
        let temporary = tempfile::tempdir().unwrap();
        let bytes = Arc::new(Mutex::new(Vec::new()));
        let expected = vec![0xff; 32769];
        let error = drain(
            expected.as_slice(),
            Some(RecordingWriter {
                bytes: bytes.clone(),
                acknowledgement: temporary.path().join("console"),
            }),
            Some(BrokenWriter),
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);
        assert_eq!(*bytes.lock().unwrap(), expected);
    }

    /// A shared archive writer keeps receiving chunks for its healthy log sinks.
    /// 共享存档写入器仍接收后续数据块，使其中正常的日志目标继续工作。
    #[test]
    fn keeps_sending_chunks_after_archive_write_failure() {
        struct FailsOnceWriter {
            bytes: Arc<Mutex<Vec<u8>>>,
            failed: bool,
        }
        impl Write for FailsOnceWriter {
            fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
                if !self.failed {
                    self.failed = true;
                    return Err(io::Error::new(io::ErrorKind::BrokenPipe, "one sink failed"));
                }
                self.bytes.lock().unwrap().extend_from_slice(buffer);
                Ok(buffer.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let bytes = Arc::new(Mutex::new(Vec::new()));
        let expected = vec![0xff; 32769];
        let error = drain(
            expected.as_slice(),
            Some(io::sink()),
            Some(FailsOnceWriter {
                bytes: bytes.clone(),
                failed: false,
            }),
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);
        assert_eq!(*bytes.lock().unwrap(), expected[8192..]);
    }

    /// The archive retains non-UTF-8 bytes and progress controls with pipes or PTYs.
    /// 管道和 PTY 存档均保留非 UTF-8 字节及进度控制字符。
    #[test]
    fn archives_exact_diagnostic_bytes_with_pipes_and_terminals() {
        for terminal in [None, Some(libc::STDERR_FILENO)] {
            let temporary = tempfile::tempdir().unwrap();
            let log = logging::BuildLog::new(temporary.path()).unwrap();
            let _logging = logging::enter([log.clone()]);
            let mut command = Command::new("bash");
            command.args(["-c", "printf '\\377progress\\r\\n\\000done'"]);
            let output = stream_to(&mut command, Some(io::sink()), io::sink(), terminal).unwrap();
            assert!(output.status.success());
            assert_eq!(output.stdout, b"\xffprogress\r\n\0done");
            assert!(output.stderr.is_empty());
            log.publish(temporary.path()).unwrap();
            assert_eq!(
                fs::read(temporary.path().join("install.log")).unwrap(),
                output.stdout
            );
        }
    }

    /// TTY-sensitive progress is visible before exit, with exact bytes and failure capture.
    /// 依赖 TTY 的进度在退出前可见，并保留原始字节和失败诊断。
    #[test]
    fn streams_terminal_progress_with_dimensions_and_separate_failure_output() {
        let temporary = tempfile::tempdir().unwrap();
        let stdout = Arc::new(Mutex::new(Vec::new()));
        let stderr = Arc::new(Mutex::new(Vec::new()));
        let (_master, terminal) = terminal_pipe(libc::STDERR_FILENO).unwrap();
        let size = libc::winsize {
            ws_row: 37,
            ws_col: 113,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        assert_eq!(
            unsafe { libc::ioctl(terminal.as_raw_fd(), libc::TIOCSWINSZ, &size) },
            0
        );
        let mut command = Command::new("bash");
        command
            .args([
                "-c",
                r#"
[[ ! -t 0 && -t 1 && -t 2 ]] || exit 98
exec 3>&1
[[ $(stty size <&3) == '37 113' ]] || exit 97
[[ $(stty size <&2) == '37 113' ]] || exit 96
exec 3>&-
printf 'download 50%%\r'
printf 'checking\n' >&2
for (( attempt = 0; attempt < 500; attempt++ )); do
    if [[ -f "$1/stdout" && -f "$1/stderr" ]]; then
        printf '\377download 100%%\n'
        printf '\376failed\n' >&2
        exit 7
    fi
    sleep 0.01
done
exit 99
"#,
                "rstarsleep-terminal-output-test",
            ])
            .arg(temporary.path());
        let output = stream_to(
            &mut command,
            Some(RecordingWriter {
                bytes: stdout.clone(),
                acknowledgement: temporary.path().join("stdout"),
            }),
            RecordingWriter {
                bytes: stderr.clone(),
                acknowledgement: temporary.path().join("stderr"),
            },
            Some(terminal.as_raw_fd()),
        )
        .unwrap();
        assert_eq!(
            output.status.code(),
            Some(7),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(output.stdout, b"download 50%\r\xffdownload 100%\n");
        assert_eq!(output.stderr, b"checking\n\xfefailed\n");
        assert_eq!(*stdout.lock().unwrap(), output.stdout);
        assert_eq!(*stderr.lock().unwrap(), output.stderr);
    }

    /// A silent child sees new dimensions without needing to emit another update first.
    /// 静默子进程无需先输出下一次进度即可感知终端尺寸变化。
    #[test]
    fn follows_terminal_resizes_while_child_is_silent() {
        let temporary = tempfile::tempdir().unwrap();
        let ready = temporary.path().join("ready");
        let (_master, terminal) = terminal_pipe(libc::STDERR_FILENO).unwrap();
        let resize_terminal = terminal.try_clone().unwrap();
        let mut command = Command::new("bash");
        command
            .args([
                "-c",
                r#"
exec 3>&1
printf ready > "$1"
for (( attempt = 0; attempt < 500; attempt++ )); do
    if [[ $(stty size <&3) == '42 120' && $(stty size <&2) == '42 120' ]]; then
        printf resized
        exit 0
    fi
    sleep 0.01
done
exit 99
"#,
                "rstarsleep-terminal-resize-test",
            ])
            .arg(&ready);
        let resizer = thread::spawn(move || {
            for _ in 0..500 {
                if ready.is_file() {
                    let size = libc::winsize {
                        ws_row: 42,
                        ws_col: 120,
                        ws_xpixel: 0,
                        ws_ypixel: 0,
                    };
                    assert_eq!(
                        unsafe {
                            libc::ioctl(resize_terminal.as_raw_fd(), libc::TIOCSWINSZ, &size)
                        },
                        0
                    );
                    return;
                }
                thread::sleep(std::time::Duration::from_millis(10));
            }
            panic!("child did not start");
        });
        let output = stream_to(
            &mut command,
            Some(io::sink()),
            io::sink(),
            Some(terminal.as_raw_fd()),
        )
        .unwrap();
        resizer.join().unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout, b"resized");
        assert!(output.stderr.is_empty());
    }
}
