//! Forward build diagnostics as bytes arrive while retaining process results.
//! 在保留进程结果的同时，即时转发构建诊断。
use std::{
    io::{self, Read, Write},
    process::{Command, Output, Stdio},
    thread,
};

/// Stream diagnostics to stderr, keeping query stdout private for protocol parsing.
/// 向 stderr 即时输出诊断，将查询 stdout 保留给协议解析。
pub(super) fn stream(command: &mut Command, query: bool) -> io::Result<Output> {
    stream_to(command, (!query).then(io::stderr), io::stderr())
}

/// Drain a pipe even after a terminal write fails so the child cannot block on it.
/// 即使终端写入失败也继续读取管道，避免子进程因管道填满而阻塞。
fn drain(reader: impl Read, mut target: Option<impl Write>) -> io::Result<Vec<u8>> {
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
        if let Some(writer) = &mut target {
            if let Err(error) = writer
                .write_all(&buffer[..count])
                .and_then(|()| writer.flush())
            {
                write_error = Some(error);
                target = None;
            }
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
) -> io::Result<Output> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let child_stdout = child.stdout.take().expect("piped stdout");
    let child_stderr = child.stderr.take().expect("piped stderr");
    thread::scope(|scope| {
        let stdout = scope.spawn(move || drain(child_stdout, stdout));
        let stderr = scope.spawn(move || drain(child_stderr, Some(stderr)));
        let status = child.wait();
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
        let stderr = Arc::new(Mutex::new(Vec::new()));
        let mut command = Command::new("bash");
        command
            .args([
                "-c",
                r#"
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
        )
        .unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout, b"package\texplicit\n");
        assert_eq!(*stderr.lock().unwrap(), b"query-progress\r");
    }

    /// Both pipes can exceed their kernel buffers without blocking each other.
    /// 两条管道的内容均可超过内核缓冲区而不会相互阻塞。
    #[test]
    fn drains_large_stdout_and_stderr_concurrently() {
        let mut command = Command::new("bash");
        command.args([
            "-c",
            "head -c 262144 /dev/zero & head -c 262144 /dev/zero >&2 & wait",
        ]);
        let output = stream_to(&mut command, Some(io::sink()), io::sink()).unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout, vec![0; 262144]);
        assert_eq!(output.stderr, vec![0; 262144]);
    }

    /// Terminal failures do not prevent draining or reaping the running child.
    /// 终端失败不会阻止排空管道或回收正在运行的子进程。
    #[test]
    fn drains_and_reaps_after_terminal_write_failure() {
        struct BrokenWriter;
        impl Write for BrokenWriter {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::Error::new(io::ErrorKind::BrokenPipe, "terminal closed"))
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let temporary = tempfile::tempdir().unwrap();
        let mut command = Command::new("bash");
        command
            .args([
                "-c",
                "head -c 262144 /dev/zero; printf done > \"$1\"",
                "rstarsleep-broken-output",
            ])
            .arg(temporary.path().join("completed"));
        let error = stream_to(&mut command, Some(BrokenWriter), io::sink()).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);
        assert_eq!(
            fs::read(temporary.path().join("completed")).unwrap(),
            b"done"
        );
    }
}
