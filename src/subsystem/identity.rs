//! Match guest accounts to authenticated host callers.
//! 将容器账户与已认证宿主调用者匹配。
use super::{Error, PreparedRuntime, Result, commands};
static ACCOUNTS: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[derive(Debug, Clone, Copy)]
pub(crate) struct Peer {
    pub uid: u32,
    pub gid: u32,
    pub pid: i32,
}

/// Obtain credentials from the kernel, never from a request.
/// 从内核获取凭据，绝不从请求获取。
/// # Arguments
/// - `fd`: Connected Unix descriptor. 已连接 Unix 描述符。
pub(crate) fn credentials(fd: i32) -> Result<Peer> {
    let mut c: libc::ucred = unsafe { std::mem::zeroed() };
    let mut n = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    if unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut c as *mut libc::ucred).cast(),
            &mut n,
        )
    } != 0
    {
        return Err(std::io::Error::last_os_error().into());
    }
    if n as usize != std::mem::size_of::<libc::ucred>() || c.pid <= 0 {
        return Err(Error::Invalid(
            "对端凭据无效 / invalid peer credentials".into(),
        ));
    }
    Ok(Peer {
        uid: c.uid,
        gid: c.gid,
        pid: c.pid,
    })
}
/// Require the host or guest administrative identity.
/// 要求宿主或容器管理员身份。
/// # Arguments
/// - `peer`: Kernel credentials. 内核凭据。
pub(crate) fn admin(peer: Peer) -> Result<()> {
    if peer.uid != 0 {
        return Err(Error::Invalid(
            "此操作需要 root / this operation requires root".into(),
        ));
    }
    Ok(())
}

/// Ensure exact UID/GID and a guest-local home, using guest NSS tools.
/// 使用容器 NSS 工具确保准确 UID/GID 和容器本地 home。
/// # Arguments
/// - `runtime`: Running machine. 运行机器。
/// - `peer`: Authenticated caller. 已认证调用者。
pub(crate) fn ensure(runtime: &PreparedRuntime, peer: Peer) -> Result<(String, String)> {
    if !runtime.config.allows(peer.uid) {
        return Err(Error::Invalid(
            "子系统身份访问被拒绝 / subsystem identity access denied".into(),
        ));
    }
    let _accounts = ACCOUNTS
        .lock()
        .map_err(|_| Error::Invalid("客体账户锁损坏 / guest account lock poisoned".into()))?;
    // Lookup failures are distinguished from missing accounts by getent status 2.
    // 通过 getent 状态 2 区分查找失败和缺失账户。
    let lookup = |database: &str, key: String| -> Result<Option<String>> {
        let argv = vec!["/usr/bin/getent".into(), database.into(), key];
        let mut command = std::process::Command::new(commands::SYSTEMD_RUN);
        command
            .arg(format!("--machine={}", runtime.machine))
            .args(["--uid=0", "--wait", "--pipe", "--quiet", "--collect", "--"])
            .args(argv);
        let output = commands::output(command, std::time::Duration::from_secs(10))?;
        if output.status.code() == Some(2) {
            return Ok(None);
        }
        if !output.status.success() {
            return Err(Error::Invalid(
                "客体 NSS 查询失败 / guest NSS lookup failed".into(),
            ));
        }
        Ok(Some(String::from_utf8(output.stdout).map_err(|_| {
            Error::Invalid("客体 NSS 记录无效 / invalid guest NSS record".into())
        })?))
    };
    if lookup("group", peer.gid.to_string())?.is_none() {
        commands::guest(
            runtime,
            &[
                "/usr/bin/groupadd".into(),
                "--gid".into(),
                peer.gid.to_string(),
                format!("rstarsleep-{}", peer.gid),
            ],
        )?;
    }
    if lookup("passwd", peer.uid.to_string())?.is_none() {
        commands::guest(
            runtime,
            &[
                "/usr/bin/useradd".into(),
                "--uid".into(),
                peer.uid.to_string(),
                "--gid".into(),
                peer.gid.to_string(),
                "--create-home".into(),
                "--shell".into(),
                "/bin/sh".into(),
                format!("rstarsleep-{}", peer.uid),
            ],
        )?;
    }
    let record = lookup("passwd", peer.uid.to_string())?
        .ok_or_else(|| Error::Invalid("客体账户缺失 / guest account missing".into()))?;
    let fields: Vec<_> = record.trim_end().split(':').collect();
    if fields.len() != 7
        || fields[2].parse::<u32>().ok() != Some(peer.uid)
        || fields[3].parse::<u32>().ok() != Some(peer.gid)
        || fields[0].is_empty()
        || fields[0].chars().any(char::is_control)
    {
        return Err(Error::Invalid(
            "客体账户的 UID/GID 与已认证调用者冲突 / guest account UID/GID conflicts with the authenticated caller".into(),
        ));
    }
    super::config::absolute(std::path::Path::new(fields[5]), false)?;
    commands::guest(
        runtime,
        &[
            "/usr/bin/install".into(),
            "--directory".into(),
            "--mode=0700".into(),
            format!("--owner={}", peer.uid),
            format!("--group={}", peer.gid),
            format!("/run/user/{}", peer.uid),
        ],
    )?;
    Ok((fields[0].into(), fields[5].into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::AsRawFd;
    /// Credentials are kernel authenticated and invalid descriptors fail closed.
    /// 凭据来自内核，无效描述符封闭失败。
    /// # Arguments
    /// No parameters. 无参数。
    #[test]
    fn kernel_credentials() {
        let (a, _b) = std::os::unix::net::UnixStream::pair().unwrap();
        let peer = credentials(a.as_raw_fd()).unwrap();
        assert_eq!(peer.uid, unsafe { libc::geteuid() });
        assert_eq!(peer.gid, unsafe { libc::getegid() });
        assert!(peer.pid > 0);
        assert!(credentials(-1).is_err());
        assert!(
            admin(Peer {
                uid: 1000,
                gid: 1000,
                pid: 1
            })
            .is_err()
        );
    }
}
