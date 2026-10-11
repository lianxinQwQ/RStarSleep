//! Protect terminal state while forwarding signals and resize events.
//! 转发信号和尺寸事件时保护终端状态。
use super::protocol::WindowSize;
use std::{
    io,
    sync::{
        Mutex, MutexGuard,
        atomic::{AtomicBool, AtomicI32, Ordering},
    },
};
static SIGNAL: AtomicI32 = AtomicI32::new(0);
static RESIZE: AtomicBool = AtomicBool::new(false);
static OWNER: Mutex<()> = Mutex::new(());
pub struct TerminalGuard {
    original: Option<libc::termios>,
    signals: Vec<(i32, libc::sigaction)>,
    _owner: MutexGuard<'static, ()>,
}
/// Record a signal using only atomic signal-safe operations.
/// 仅用信号安全的原子操作记录信号。
/// # Arguments
/// - `signal`: Received OS signal. 收到的系统信号。
extern "C" fn receive(signal: libc::c_int) {
    if signal == libc::SIGWINCH {
        RESIZE.store(true, Ordering::Relaxed);
    } else {
        SIGNAL.store(signal, Ordering::Relaxed);
    }
}
impl TerminalGuard {
    /// Preserve tty attributes and install temporary signal handlers.
    /// 保存终端属性并安装临时信号处理器。
    /// # Arguments
    /// - `raw`: Whether an interactive tty needs raw mode. 交互终端是否需要原始模式。
    pub fn acquire(raw: bool) -> io::Result<Self> {
        let owner = OWNER
            .try_lock()
            .map_err(|_| io::Error::other("another execution owns terminal signal handlers"))?;
        let mut guard = Self {
            original: None,
            signals: Vec::new(),
            _owner: owner,
        };
        SIGNAL.store(0, Ordering::Relaxed);
        RESIZE.store(false, Ordering::Relaxed);
        if raw && unsafe { libc::isatty(0) } == 1 {
            let mut old = unsafe { std::mem::zeroed() };
            if unsafe { libc::tcgetattr(0, &mut old) } != 0 {
                return Err(io::Error::last_os_error());
            }
            let mut new = old;
            unsafe { libc::cfmakeraw(&mut new) };
            if unsafe { libc::tcsetattr(0, libc::TCSANOW, &new) } != 0 {
                return Err(io::Error::last_os_error());
            }
            guard.original = Some(old);
        }
        for sig in [
            libc::SIGINT,
            libc::SIGTERM,
            libc::SIGHUP,
            libc::SIGQUIT,
            libc::SIGWINCH,
        ] {
            let mut old: libc::sigaction = unsafe { std::mem::zeroed() };
            let mut new: libc::sigaction = unsafe { std::mem::zeroed() };
            new.sa_sigaction = receive as *const () as libc::sighandler_t;
            unsafe {
                libc::sigemptyset(&mut new.sa_mask);
            }
            if unsafe { libc::sigaction(sig, &new, &mut old) } != 0 {
                return Err(io::Error::last_os_error());
            }
            guard.signals.push((sig, old));
        }
        Ok(guard)
    }
}
impl Drop for TerminalGuard {
    /// Restore original attributes and handlers after every exit path.
    /// 每个退出路径都恢复原始属性与处理器。
    /// # Arguments
    /// - `self`: Temporary terminal state. 临时终端状态。
    fn drop(&mut self) {
        if let Some(old) = &self.original {
            unsafe {
                libc::tcsetattr(0, libc::TCSANOW, old);
            }
        }
        for (sig, old) in self.signals.drain(..).rev() {
            unsafe {
                libc::sigaction(sig, &old, std::ptr::null_mut());
            }
        }
    }
}
/// Query the local terminal with a bounded fallback.
/// 查询本地终端，使用有界默认值。
/// # Arguments
/// No parameters. 无参数。
pub fn window_size() -> WindowSize {
    let mut w: libc::winsize = unsafe { std::mem::zeroed() };
    if unsafe { libc::ioctl(0, libc::TIOCGWINSZ, &mut w) } == 0 && w.ws_row > 0 && w.ws_col > 0 {
        WindowSize {
            rows: w.ws_row.min(1000),
            cols: w.ws_col.min(1000),
        }
    } else {
        WindowSize { rows: 24, cols: 80 }
    }
}
/// Consume pending signal state.
/// 消费待处理信号状态。
/// # Arguments
/// No parameters. 无参数。
pub fn take_signal() -> Option<i32> {
    match SIGNAL.swap(0, Ordering::Relaxed) {
        0 => None,
        s => Some(s),
    }
}
/// Consume pending terminal resize state.
/// 消费待处理终端尺寸状态。
/// # Arguments
/// No parameters. 无参数。
pub fn take_resize() -> bool {
    RESIZE.swap(false, Ordering::Relaxed)
}
/// Poll stdin while allowing the event receiver to finish.
/// 轮询标准输入，同时允许事件接收器结束。
/// # Arguments
/// - `milliseconds`: Bounded poll delay. 有界轮询延迟。
pub fn stdin_ready(milliseconds: i32) -> io::Result<bool> {
    let mut fd = libc::pollfd {
        fd: 0,
        events: libc::POLLIN,
        revents: 0,
    };
    let rc = unsafe { libc::poll(&mut fd, 1, milliseconds) };
    if rc < 0 {
        let e = io::Error::last_os_error();
        if e.kind() == io::ErrorKind::Interrupted {
            return Ok(false);
        }
        return Err(e);
    }
    Ok(fd.revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR) != 0)
}
/// Read stdin without holding a global stream lock.
/// 不持有全局流锁读取标准输入。
/// # Arguments
/// - `bytes`: Destination buffer. 目标缓冲区。
pub fn read_stdin(bytes: &mut [u8]) -> io::Result<usize> {
    loop {
        let n = unsafe { libc::read(0, bytes.as_mut_ptr().cast(), bytes.len()) };
        if n >= 0 {
            return Ok(n as usize);
        }
        let e = io::Error::last_os_error();
        if e.kind() != io::ErrorKind::Interrupted {
            return Err(e);
        }
    }
}
