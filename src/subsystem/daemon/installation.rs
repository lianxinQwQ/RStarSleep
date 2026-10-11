//! Stable executable installation and literal systemd argument escaping.
//! 稳定可执行程序安装与 systemd 字面参数转义。
use super::*;

/// Escape paths for systemd's quoting, environment and specifier rules.
/// 按 systemd 引用、环境及说明符规则转义路径。
/// # Arguments
/// - `path`: Literal UTF-8 path. 字面 UTF-8 路径。
pub(super) fn argument(path: &Path) -> Result<String> {
    let s = path
        .to_str()
        .ok_or_else(|| Error::Invalid("service paths must be UTF-8".into()))?;
    if s.chars().any(char::is_control) {
        return Err(Error::Invalid("invalid service path".into()));
    }
    Ok(format!(
        "\"{}\"",
        s.replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('%', "%%")
            .replace('$', "$$")
    ))
}
/// Install a fixed-root service using an already installed trusted executable.
/// 使用已安装可信可执行程序安装固定管理根服务。
/// # Arguments
/// - `store`: Fixed management root. 固定管理根。
/// # Errors
/// Requires root-owned executable/configuration and working systemd.
/// 要求 root 所属可执行程序及配置和可用 systemd。
pub fn install(store: &ConfigStore) -> Result<()> {
    identity::admin(Peer {
        uid: unsafe { libc::geteuid() },
        gid: unsafe { libc::getegid() },
        pid: std::process::id() as i32,
    })?;
    trust::store(store, None)?;
    let binary = fs::canonicalize(std::env::current_exe()?)?;
    trust::path(&binary)?;
    let root = fs::canonicalize(store.root())?;
    let socket = client::socket_for_root(&root)?;
    let digest = socket
        .parent()
        .unwrap()
        .file_name()
        .unwrap()
        .to_string_lossy();
    let unit = format!("rstarsleep-subsystem-{digest}.service");
    let directory = Path::new("/etc/systemd/system");
    trust::path(directory)?;
    let contents = format!(
        "[Unit]\nDescription=RStarSleep subsystem manager\nAfter=systemd-machined.service\nRequires=systemd-machined.service\n\n[Service]\nType=simple\nUser=root\nGroup=root\nEnvironment=PATH=/usr/sbin:/usr/bin:/sbin:/bin\nExecStart={} daemon run --root {}\nRestart=on-failure\nRestartSec=2\nDelegate=yes\nKillMode=process\n\n[Install]\nWantedBy=multi-user.target\n",
        argument(&binary)?,
        argument(&root)?
    );
    let path = directory.join(&unit);
    super::atomic_write(&path, contents.as_bytes(), 0o644)?;
    let mut command = Command::new(commands::SYSTEMCTL);
    command.arg("daemon-reload");
    if !commands::output(command, Duration::from_secs(10))?
        .status
        .success()
    {
        return Err(Error::Invalid("daemon-reload failed".into()));
    }
    println!("{}\nsystemctl enable --now {unit}", path.display());
    Ok(())
}
