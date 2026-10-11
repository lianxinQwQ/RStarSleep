//! Controlled package requests and asynchronous rebuild job logs.
//! 受控软件包请求与异步重建任务日志。
use super::*;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct BuildJob {
    schema_version: u32,
    system_id: String,
    pid: u32,
    log: PathBuf,
    state: String,
    exit_code: Option<i32>,
}

/// Append only controlled package assignments, leaving other files byte-for-byte intact.
/// 只追加受控软件包赋值，其他文件保持原样。
/// # Arguments
/// - `store`, `id`, `packages`: Fixed system and validated package names. 固定系统及已校验包名。
pub(super) fn install_packages(store: &ConfigStore, id: &str, packages: &[String]) -> Result<()> {
    if packages.is_empty() {
        return Err(Error::Invalid("guest install requires packages".into()));
    }
    for package in packages {
        crate::software::valid_name(package)?;
    }
    let layout = store.system_layout(id)?;
    let _system = controller::lock_system_directory(&layout.root)?;
    let directory = layout.config.join("build");
    checked(&directory)?;
    let path = directory.join("guest-packages.conf");
    let mut ini_present = false;
    for entry in fs::read_dir(&directory)? {
        let entry = entry?;
        if entry.file_name() == "config.toml" || entry.path() == path {
            continue;
        }
        if !entry.file_type()?.is_file() {
            return Err(Error::Invalid("build config requires regular files".into()));
        }
        let text = fs::read_to_string(entry.path())?;
        if text.lines().any(|l| l.trim() == "[Config]") {
            ini_present = true;
        }
    }
    if !ini_present {
        return Err(Error::Invalid(
            "guest install requires existing INI build configuration; migrate legacy TOML first"
                .into(),
        ));
    }
    store.read_build_config(id)?;
    let mut requested = Vec::<String>::new();
    if path.try_exists()? {
        checked(&path)?;
        let text = fs::read_to_string(&path)?;
        if text.len() > 1024 * 1024 {
            return Err(Error::Invalid("guest package requests exceed limit".into()));
        }
        let mut section = false;
        for line in text.lines().map(str::trim) {
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if line == "[Content]" {
                section = true;
                continue;
            }
            let names = line
                .strip_prefix("Packages=")
                .filter(|_| section)
                .ok_or_else(|| {
                    Error::Invalid("guest-packages.conf contains unmanaged settings".into())
                })?;
            for name in names.split_ascii_whitespace() {
                crate::software::valid_name(name)?;
                if !requested.iter().any(|v| v == name) {
                    requested.push(name.into());
                }
            }
        }
    }
    for package in packages {
        if !requested.contains(package) {
            requested.push(package.clone());
        }
    }
    if requested.len() > 4096 {
        return Err(Error::Invalid(
            "guest package request limit exceeded".into(),
        ));
    }
    let text = format!(
        "# Managed guest package requests.\n# 容器内登记的软件包请求。\n[Content]\nPackages={}\n",
        requested.join(" ")
    );
    atomic_write(&path, text.as_bytes(), 0o600)
}

/// Spawn one rebuild with durable logs; failures never replace the current artifact.
/// 启动单项重建并保留日志；失败不会替换当前产物。
/// # Arguments
/// - `store`, `id`: Endpoint's fixed manager and system. 端点固定管理器及系统。
pub(super) fn rebuild(store: &ConfigStore, id: &str) -> Result<String> {
    trust::store(store, Some(id))?;
    let directory = store.system_layout(id)?.root.join("runtime/guest");
    checked(&directory)?;
    fs::create_dir_all(&directory)?;
    let job_lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(directory.join("build.lock"))?;
    if unsafe { libc::flock(job_lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err(Error::Invalid("a guest rebuild is already running".into()));
    }
    let binary = fs::canonicalize(std::env::current_exe()?)?;
    trust::path(&binary)?;
    let serial = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| Error::Invalid(e.to_string()))?
        .as_nanos();
    let log = directory.join(format!("build-{serial}-{}.log", std::process::id()));
    let output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&log)?;
    let mut command = Command::new(binary);
    command
        .arg("build")
        .arg(id)
        .arg("--root")
        .arg(store.root())
        .stdin(Stdio::null())
        .stdout(Stdio::from(output.try_clone()?))
        .stderr(Stdio::from(output));
    let descriptor = job_lock.as_raw_fd();
    // The child keeps the job lock across daemon exit and exec.
    // 子进程跨 daemon 退出和 exec 保持任务锁。
    unsafe {
        command.pre_exec(move || {
            if libc::fcntl(descriptor, libc::F_SETFD, 0) < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command.spawn()?;
    let status_path = directory.join("build-current.toml");
    let job = BuildJob {
        schema_version: 1,
        system_id: id.into(),
        pid: child.id(),
        log: log.clone(),
        state: "running".into(),
        exit_code: None,
    };
    if let Err(error) = write_record(&status_path, &job) {
        let _ = child.kill();
        let _ = child.wait();
        return Err(error);
    }
    thread::spawn(move || {
        let _lock = job_lock;
        let result = child.wait();
        let mut job = job;
        match result {
            Ok(status) => {
                job.state = if status.success() {
                    "complete"
                } else {
                    "failed"
                }
                .into();
                job.exit_code = status.code();
            }
            Err(error) => {
                job.state = "failed".into();
                eprintln!("guest rebuild wait: {error}");
            }
        }
        if let Err(error) = write_record(&status_path, &job) {
            eprintln!("guest rebuild record: {error}");
        }
    });
    Ok(format!(
        "已安排构建；日志：{}；成功产物在下次启动或重启生效",
        log.display()
    ))
}
