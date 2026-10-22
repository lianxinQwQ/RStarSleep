//! Archive build and inheritance diagnostics independently of system roots.
//! 独立于系统根归档构建和继承诊断。
use super::{Error, Result, fs as disk};
use std::{
    cell::RefCell,
    fmt::Display,
    fs::{File, OpenOptions, Permissions},
    io::{self, Write},
    marker::PhantomData,
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    process::{Command, Output},
    rc::Rc,
    sync::{Arc, Mutex, MutexGuard},
};
use tempfile::TempPath;

/// Preserve the first write failure so ignored diagnostic writes cannot hide it.
/// 保留首次写入失败，避免忽略诊断写入的调用掩盖错误。
struct WriteFailure {
    kind: io::ErrorKind,
    code: Option<i32>,
    message: String,
}

impl WriteFailure {
    fn new(error: io::Error) -> Self {
        Self {
            kind: error.kind(),
            code: error.raw_os_error(),
            message: error.to_string(),
        }
    }

    fn error(&self) -> io::Error {
        match self.code {
            Some(code) => io::Error::from_raw_os_error(code),
            None => io::Error::new(self.kind, self.message.clone()),
        }
    }
}

struct State {
    file: File,
    temporary: Option<TempPath>,
    path: PathBuf,
    failure: Option<WriteFailure>,
    durable: bool,
}

impl State {
    fn remember(&mut self, error: io::Error) -> io::Error {
        self.failure
            .get_or_insert_with(|| WriteFailure::new(error))
            .error()
    }

    fn check(&mut self) -> io::Result<()> {
        // Synchronize any retained bytes even when an earlier write failed.
        // 即使之前写入失败，也同步已经保留的字节。
        if let Err(error) = self.file.sync_data() {
            self.remember(error);
        }
        match &self.failure {
            Some(error) => Err(error.error()),
            None => Ok(()),
        }
    }
}

/// Shared append-only log that survives its temporary path becoming a version record.
/// 共享追加日志，临时路径归档为版本记录后仍可继续写入。
#[derive(Clone)]
pub(super) struct BuildLog(Arc<Mutex<State>>);

impl BuildLog {
    /// Stage diagnostics without allocating a version or changing frozen inputs.
    /// 不分配版本或修改冻结输入，先暂存诊断。
    pub(super) fn new(parent: &Path) -> Result<Self> {
        disk::checked(parent)?;
        let temporary = tempfile::Builder::new()
            .prefix("install-preflight-")
            .permissions(Permissions::from_mode(0o600))
            .tempfile_in(parent)
            .map_err(|error| disk::io(parent, error))?;
        // Set the exact owner-only mode independently of the caller's umask.
        // 独立于调用者的 umask，确保准确的仅所有者读写权限。
        temporary
            .as_file()
            .set_permissions(Permissions::from_mode(0o600))
            .map_err(|error| disk::io(temporary.path(), error))?;
        let path = temporary.path().to_owned();
        let (file, temporary) = temporary.into_parts();
        Ok(Self(Arc::new(Mutex::new(State {
            file,
            temporary: Some(temporary),
            path,
            failure: None,
            durable: false,
        }))))
    }

    /// Append to a private inheritance log and sync each diagnostic before proceeding.
    /// 追加私有继承日志，继续操作前同步每条诊断。
    pub(super) fn append(path: &Path) -> Result<Self> {
        disk::checked(path)?;
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)
            .map_err(|error| disk::io(path, error))?;
        let metadata = file.metadata().map_err(|error| disk::io(path, error))?;
        if !metadata.is_file() || metadata.nlink() != 1 {
            return Err(Error::Invalid(format!(
                "继承日志必须为无硬链接的普通文件: {} / Inheritance log must be a regular file without hard links",
                path.display()
            )));
        }
        file.set_permissions(Permissions::from_mode(0o600))
            .and_then(|()| file.sync_all())
            .map_err(|error| disk::io(path, error))?;
        disk::sync(path.parent().unwrap())?;
        Ok(Self(Arc::new(Mutex::new(State {
            file,
            temporary: None,
            path: path.to_owned(),
            failure: None,
            durable: true,
        }))))
    }

    fn state(&self) -> MutexGuard<'_, State> {
        match self.0.lock() {
            Ok(state) => state,
            Err(poisoned) => {
                let mut state = poisoned.into_inner();
                state.remember(io::Error::other(
                    "安装日志锁损坏 / installation log lock poisoned",
                ));
                state
            }
        }
    }

    /// Retain staged output at the version's fixed log path without replacing entries.
    /// 将暂存输出保留到版本的固定日志路径，不替换已有条目。
    pub(super) fn publish(&self, directory: &Path) -> Result<()> {
        let destination = directory.join("install.log");
        disk::checked(&destination)?;
        let mut state = self.state();
        let temporary = state.temporary.take().ok_or_else(|| {
            Error::Invalid("安装日志已经归档 / installation log already archived".into())
        })?;
        let previous_parent = state.path.parent().map(Path::to_owned);
        // Preserve partial logs too: a prior write error is reported after publication.
        // 同样保留不完整日志：已有写入错误在归档后报告。
        if let Err(error) = state.file.sync_data() {
            state.remember(error);
        }
        if let Err(error) = temporary.persist_noclobber(&destination) {
            state.temporary = Some(error.path);
            return Err(disk::io(&destination, error.error));
        }
        state.path = destination;
        disk::sync(directory)?;
        if let Some(previous_parent) = previous_parent
            && previous_parent != directory
        {
            disk::sync(&previous_parent)?;
        }
        match &state.failure {
            Some(error) => Err(disk::io(&state.path, error.error())),
            None => Ok(()),
        }
    }

    /// Flush retained bytes and report the earliest diagnostic write failure.
    /// 同步保留的字节，并报告最早的诊断写入失败。
    pub(super) fn check(&self) -> Result<()> {
        let mut state = self.state();
        state.check().map_err(|error| disk::io(&state.path, error))
    }

    fn write(&self, bytes: &[u8]) -> io::Result<()> {
        let mut state = self.state();
        if let Some(error) = &state.failure {
            return Err(error.error());
        }
        let result = state.file.write_all(bytes).and_then(|()| {
            if state.durable {
                state.file.sync_data()
            } else {
                Ok(())
            }
        });
        result.map_err(|error| state.remember(error))
    }

    fn flush(&self) -> io::Result<()> {
        let mut state = self.state();
        if let Some(error) = &state.failure {
            return Err(error.error());
        }
        state.file.flush().map_err(|error| state.remember(error))
    }
}

thread_local! {
    static ACTIVE: RefCell<Vec<BuildLog>> = const { RefCell::new(Vec::new()) };
}

/// Restore the previous diagnostic scope on the same thread, including during unwinding.
/// 在同一线程恢复先前的诊断作用域，包括栈展开期间。
pub(super) struct Guard {
    previous: Vec<BuildLog>,
    // A guard must be dropped on the thread whose TLS it changed.
    // 必须在修改其 TLS 的线程中销毁守卫。
    _thread: PhantomData<Rc<()>>,
}

impl Drop for Guard {
    fn drop(&mut self) {
        let previous = std::mem::take(&mut self.previous);
        let _ = ACTIVE.try_with(|active| *active.borrow_mut() = previous);
    }
}

/// Select the logs for this operation; shared preparation may select several versions.
/// 选择本次操作的日志；共享准备阶段可选择多个版本。
pub(super) fn enter(logs: impl IntoIterator<Item = BuildLog>) -> Guard {
    let mut selected: Vec<BuildLog> = Vec::new();
    for log in logs {
        if !selected.iter().any(|other| Arc::ptr_eq(&other.0, &log.0)) {
            selected.push(log);
        }
    }
    let previous = ACTIVE.with(|active| active.replace(selected));
    Guard {
        previous,
        _thread: PhantomData,
    }
}

/// A snapshot of active logs that can be sent to concurrent process-output readers.
/// 当前日志的快照，可发送给并发进程输出读取线程。
#[derive(Clone)]
pub(super) struct LogWriter(Vec<BuildLog>);

/// Capture the current scope before starting reader threads; their TLS is independent.
/// 启动读取线程前捕获当前作用域；各线程的 TLS 相互独立。
pub(super) fn writer() -> LogWriter {
    ACTIVE.with(|active| LogWriter(active.borrow().clone()))
}

impl Write for LogWriter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let mut failure = None;
        for log in &self.0 {
            // A failed log must not stop the other versions from receiving this chunk.
            // 某个日志失败时，其他版本仍应接收当前字节块。
            if let Err(error) = log.write(buffer) {
                failure.get_or_insert(error);
            }
        }
        match failure {
            Some(error) => Err(error),
            None => Ok(buffer.len()),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        let mut failure = None;
        for log in &self.0 {
            if let Err(error) = log.flush() {
                failure.get_or_insert(error);
            }
        }
        match failure {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}

/// Archive one diagnostic line before displaying it through the standard diagnostic stream.
/// 先归档一行诊断，再通过标准诊断流显示。
pub(crate) fn diagnostic(message: impl Display) {
    let line = format!("{message}\n");
    let _ = writer().write_all(line.as_bytes());
    eprint!("{line}");
}

/// Retain native storage commands and both output streams only in inheritance logs.
/// 仅在继承日志中保留原生存储命令及其两个输出流，不改变命令结果。
pub(super) fn command(command: &mut Command) -> io::Result<Output> {
    let mut archive = ACTIVE.with(|active| {
        LogWriter(
            active
                .borrow()
                .iter()
                .filter(|log| log.state().durable)
                .cloned()
                .collect(),
        )
    });
    if archive.0.is_empty() {
        return command.output();
    }
    let _ = writeln!(archive, "命令 / Command: {command:?}");
    let output = command.output();
    match &output {
        Ok(output) => {
            for (stream, bytes) in [("stdout", &output.stdout), ("stderr", &output.stderr)] {
                if !bytes.is_empty() {
                    let _ = writeln!(archive, "[{stream}]");
                    let _ = archive.write_all(bytes);
                    if !bytes.ends_with(b"\n") {
                        let _ = archive.write_all(b"\n");
                    }
                }
            }
            let _ = writeln!(archive, "退出状态 / Exit status: {}", output.status);
        }
        Err(error) => {
            let _ = writeln!(archive, "无法执行命令 / Cannot execute command: {error}");
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, fs::OpenOptions, os::unix::fs::symlink, thread};

    #[test]
    fn inheritance_log_appends_private_history_and_preserves_native_command_results() {
        let parent = tempfile::tempdir().unwrap();
        let path = parent.path().join("inheritance.log");
        fs::write(&path, b"previous attempt\n").unwrap();
        let log = BuildLog::append(&path).unwrap();
        let _scope = enter([log.clone()]);
        diagnostic("retry");
        let output = command(Command::new("sh").args([
            "-c",
            "printf 'stdout\\377'; printf 'stderr\\376' >&2; exit 23",
        ]))
        .unwrap();
        assert_eq!(output.status.code(), Some(23));
        assert_eq!(output.stdout, b"stdout\xff");
        assert_eq!(output.stderr, b"stderr\xfe");
        log.check().unwrap();
        let bytes = fs::read(&path).unwrap();
        assert!(bytes.starts_with(b"previous attempt\nretry\n"));
        for text in [
            b"[stdout]\nstdout\xff\n".as_slice(),
            b"[stderr]\nstderr\xfe\n",
            b"exit status: 23",
        ] {
            assert!(bytes.windows(text.len()).any(|part| part == text));
        }
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        drop(_scope);
        drop(log);
        let log = BuildLog::append(&path).unwrap();
        let _scope = enter([log.clone()]);
        diagnostic("later retry");
        log.check().unwrap();
        assert!(fs::read(&path).unwrap().starts_with(&bytes));
    }

    #[test]
    fn inheritance_log_captures_native_copy_errors_and_command_spawn_failures() {
        use crate::controller::{BuildBackend, SystemBackend};
        let parent = tempfile::tempdir().unwrap();
        let path = parent.path().join("inheritance.log");
        let log = BuildLog::append(&path).unwrap();
        let _scope = enter([log.clone()]);
        let source = parent.path().join("source file");
        fs::write(&source, b"content").unwrap();
        let error = SystemBackend
            .copy_inherited(&source, &parent.path().join("missing parent/target"), false)
            .unwrap_err();
        assert!(error.to_string().contains("cp inheritance"));
        let missing_command = parent.path().join("missing-command");
        assert!(command(&mut Command::new(&missing_command)).is_err());
        log.check().unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("Command: "));
        assert!(text.contains(source.to_str().unwrap()));
        assert!(text.contains("[stderr]"));
        assert!(text.contains("Cannot execute command"));
        assert!(text.contains(missing_command.to_str().unwrap()));
    }

    #[test]
    fn inheritance_log_rejects_links_and_special_files_without_touching_targets() {
        let parent = tempfile::tempdir().unwrap();
        let outside = parent.path().join("outside");
        let log = parent.path().join("inheritance.log");
        fs::write(&outside, b"keep").unwrap();
        symlink(&outside, &log).unwrap();
        assert!(BuildLog::append(&log).is_err());
        fs::remove_file(&log).unwrap();
        fs::hard_link(&outside, &log).unwrap();
        assert!(BuildLog::append(&log).is_err());
        assert_eq!(fs::read(&outside).unwrap(), b"keep");
        fs::remove_file(&log).unwrap();
        let name = std::ffi::CString::new(log.as_os_str().as_encoded_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        assert!(BuildLog::append(&log).is_err());
    }

    #[test]
    fn preserves_staged_bytes_and_continues_writing_after_publication() {
        let parent = tempfile::tempdir().unwrap();
        let version = parent.path().join("v00000001");
        fs::create_dir(&version).unwrap();
        let log = BuildLog::new(parent.path()).unwrap();
        let _scope = enter([log.clone()]);
        let mut output = writer();
        output.write_all(b"before\r\xff\n").unwrap();
        log.publish(&version).unwrap();
        thread::spawn(move || output.write_all(b"after\x00\xfe\n").unwrap())
            .join()
            .unwrap();
        log.check().unwrap();
        let path = version.join("install.log");
        assert_eq!(fs::read(&path).unwrap(), b"before\r\xff\nafter\x00\xfe\n");
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(fs::read_dir(parent.path()).unwrap().count(), 1);
    }

    #[test]
    fn restores_nested_scopes_and_keeps_writer_snapshots_independent() {
        let parent = tempfile::tempdir().unwrap();
        let first = BuildLog::new(parent.path()).unwrap();
        let second = BuildLog::new(parent.path()).unwrap();
        let outer = enter([first.clone()]);
        let mut snapshot = writer();
        writer().write_all(b"first\n").unwrap();
        {
            let _inner = enter([second.clone(), second.clone()]);
            writer().write_all(b"second\n").unwrap();
            thread::spawn(move || snapshot.write_all(b"snapshot\n").unwrap())
                .join()
                .unwrap();
        }
        writer().write_all(b"restored\n").unwrap();
        drop(outer);
        writer().write_all(b"outside\n").unwrap();
        let first_path = first.state().path.clone();
        let second_path = second.state().path.clone();
        assert_eq!(
            fs::read(first_path).unwrap(),
            b"first\nsnapshot\nrestored\n"
        );
        assert_eq!(fs::read(second_path).unwrap(), b"second\n");
    }

    #[test]
    fn sends_shared_diagnostics_to_each_log_without_overwriting_existing_entries() {
        let parent = tempfile::tempdir().unwrap();
        let first_version = parent.path().join("first");
        let second_version = parent.path().join("second");
        fs::create_dir(&first_version).unwrap();
        fs::create_dir(&second_version).unwrap();
        let first = BuildLog::new(parent.path()).unwrap();
        let second = BuildLog::new(parent.path()).unwrap();
        {
            let _scope = enter([first.clone(), second.clone()]);
            writer().write_all(b"shared\n").unwrap();
            diagnostic("message");
        }
        let target = first_version.join("install.log");
        fs::write(&target, b"existing").unwrap();
        assert!(first.publish(&first_version).is_err());
        assert_eq!(fs::read(&target).unwrap(), b"existing");
        fs::remove_file(&target).unwrap();
        symlink(second_version.join("outside"), &target).unwrap();
        assert!(first.publish(&first_version).is_err());
        assert!(!second_version.join("outside").exists());
        fs::remove_file(&target).unwrap();
        first.publish(&first_version).unwrap();
        second.publish(&second_version).unwrap();
        assert_eq!(fs::read(target).unwrap(), b"shared\nmessage\n");
        assert_eq!(
            fs::read(second_version.join("install.log")).unwrap(),
            b"shared\nmessage\n"
        );
        assert!(first.publish(&second_version).is_err());
    }

    #[test]
    fn remembers_write_failures_and_still_writes_other_selected_logs() {
        let parent = tempfile::tempdir().unwrap();
        let broken = BuildLog::new(parent.path()).unwrap();
        let healthy = BuildLog::new(parent.path()).unwrap();
        let broken_path = broken.state().path.clone();
        // A read-only descriptor reliably rejects writes even in tests running as root.
        // 只读描述符能稳定拒绝写入，包括以 root 运行的测试。
        broken.state().file = File::open(&broken_path).unwrap();
        let _scope = enter([broken.clone(), healthy.clone()]);
        assert!(writer().write_all(b"first\n").is_err());
        broken.state().file = OpenOptions::new().append(true).open(&broken_path).unwrap();
        assert!(writer().write_all(b"second\n").is_err());
        assert!(broken.check().is_err());
        healthy.check().unwrap();
        assert_eq!(
            fs::read(healthy.state().path.clone()).unwrap(),
            b"first\nsecond\n"
        );
        let version = parent.path().join("version");
        fs::create_dir(&version).unwrap();
        assert!(broken.publish(&version).is_err());
        assert!(version.join("install.log").is_file());
    }

    #[test]
    fn removes_unpublished_preflight_logs_and_rejects_linked_parent_directories() {
        let parent = tempfile::tempdir().unwrap();
        let log = BuildLog::new(parent.path()).unwrap();
        assert_eq!(fs::read_dir(parent.path()).unwrap().count(), 1);
        drop(log);
        assert_eq!(fs::read_dir(parent.path()).unwrap().count(), 0);
        let linked = parent.path().join("linked");
        symlink(parent.path(), &linked).unwrap();
        assert!(BuildLog::new(&linked).is_err());
    }
}
