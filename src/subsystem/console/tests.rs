//! Recorder identity, recovery and output completion regression tests.
//! 记录器身份、恢复及输出完成回归测试。
use super::*;
use crate::subsystem::{SubsystemConfig, supervision::Phase};
use std::sync::atomic::AtomicUsize;

/// Child exit must end PTY capture after every final stdout/stderr byte is read.
/// 子进程退出后，必须在读完标准输出和错误输出的所有末尾字节后结束 PTY 采集。
#[test]
fn exited_console_drains_final_output_without_hanging() {
    let mut command = Command::new("/bin/sh");
    command.args([
        "-c",
        "i=0; while [ \"$i\" -lt 1024 ]; do printf 'last bytes\\n'; i=$((i+1)); done; printf 'Powering off.\\n' >&2; exit 7",
    ]);
    let (mut child, mut master) = spawn_console(command).unwrap();
    let captured = (|| -> std::io::Result<Vec<u8>> {
        let flags = unsafe { libc::fcntl(master.as_raw_fd(), libc::F_GETFL) };
        if flags < 0
            || unsafe { libc::fcntl(master.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) }
                < 0
        {
            return Err(std::io::Error::last_os_error());
        }
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let mut output = Vec::new();
        let mut bytes = [0; 8192];
        loop {
            if std::time::Instant::now() >= deadline {
                return Err(std::io::Error::other(
                    "子进程退出后控制台未结束 / console did not finish after child exit",
                ));
            }
            match master.read(&mut bytes) {
                Ok(0) => return Ok(output),
                Ok(n) => output.extend_from_slice(&bytes[..n]),
                Err(e) if e.raw_os_error() == Some(libc::EIO) => return Ok(output),
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(10));
                }
                Err(e) => return Err(e),
            }
        }
    })();
    if captured.is_err() {
        let _ = child.kill();
    }
    let status = child.wait().unwrap();
    let expected = "last bytes\r\n".repeat(1024) + "Powering off.\r\n";
    assert_eq!(captured.unwrap(), expected.as_bytes());
    assert_eq!(status.code(), Some(7));
}

struct Existing {
    status: Status,
    starts: AtomicUsize,
}
impl Supervisor for Existing {
    fn query(&self, _: &str) -> Result<Status> {
        Ok(self.status.clone())
    }
    fn start(&self, _: &Service) -> Result<()> {
        self.starts.fetch_add(1, Ordering::SeqCst);
        Err(Error::Invalid(
            "不应重复启动 / duplicate start was not expected".into(),
        ))
    }
}

fn runtime(store: &ConfigStore) -> PreparedRuntime {
    let root = store.root().join("guest");
    fs::create_dir_all(&root).unwrap();
    PreparedRuntime {
        system_id: "apps".into(),
        version: "v1".into(),
        root,
        machine: "test-machine".into(),
        config: SubsystemConfig::default(),
        boot_args: Vec::new(),
    }
}

fn status(recorder: &Recorder, phase: Phase) -> Status {
    Status {
        phase,
        identity: recorder.identity.clone(),
        invocation: "0123456789abcdef0123456789abcdef".into(),
        result: "success".into(),
        exit_status: "0".into(),
    }
}

fn record(service: &Status) -> Run {
    Run {
        task: "boot-test".into(),
        machine: "test-machine".into(),
        offset: 0,
        shutdown_offset: None,
        finished: false,
        exit_code: None,
        error: None,
        invocation: Some(service.invocation.clone()),
        origin: Some(StartOrigin::Autostart),
    }
}

/// Root aliases share one service; separate managers or systems never collide.
/// 管理根别名共享同一服务；不同管理器或系统不会冲突。
#[test]
fn stable_scoped_service_identity() {
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    let store = ConfigStore::new(first.path()).unwrap();
    let other = ConfigStore::new(second.path()).unwrap();
    assert_eq!(
        unit_name(&store, "apps").unwrap(),
        unit_name(&ConfigStore::new(first.path().join(".")).unwrap(), "apps").unwrap()
    );
    assert_ne!(
        unit_name(&store, "apps").unwrap(),
        unit_name(&other, "apps").unwrap()
    );
    assert_ne!(
        unit_name(&store, "apps").unwrap(),
        unit_name(&store, "other").unwrap()
    );
    assert!(unit_name(&store, "../apps").is_err());
    let mut guest = runtime(&store);
    let original = Recorder::new(&store, "apps", &guest).unwrap();
    guest.version = "v2".into();
    let changed = Recorder::new(&store, "apps", &guest).unwrap();
    assert_eq!(original.unit, changed.unit);
    assert!(
        status(&original, Phase::Running)
            .verify(&changed.identity)
            .is_err()
    );
}

/// Recovery during startup reuses the existing service before preparing any new launch files.
/// 启动期间的恢复在准备任何新启动文件前复用已有服务。
#[test]
fn concurrent_recovery_reuses_starting_recorder() {
    let root = tempfile::tempdir().unwrap();
    let store = ConfigStore::new(root.path()).unwrap();
    let guest = runtime(&store);
    let recorder = Recorder::new(&store, "apps", &guest).unwrap();
    let supervisor = Existing {
        status: status(&recorder, Phase::Starting),
        starts: AtomicUsize::new(0),
    };
    thread::scope(|scope| {
        for _ in 0..16 {
            scope.spawn(|| {
                let restored = Recorder::new(&store, "apps", &guest).unwrap();
                restored
                    .launch(
                        &supervisor,
                        &store,
                        "apps",
                        &guest,
                        StartOrigin::Explicit,
                        "/unused".into(),
                    )
                    .unwrap();
            });
        }
    });
    assert_eq!(supervisor.starts.load(Ordering::SeqCst), 0);
    assert!(!directory(&store, "apps").unwrap().exists());
    let wrong = Existing {
        status: Status {
            identity: "another runtime".into(),
            ..supervisor.status.clone()
        },
        starts: AtomicUsize::new(0),
    };
    assert!(
        recorder
            .launch(
                &wrong,
                &store,
                "apps",
                &guest,
                StartOrigin::Explicit,
                "/unused".into()
            )
            .is_err()
    );
    assert!(!directory(&store, "apps").unwrap().exists());
}

/// Old task metadata cannot prove readiness for a new service invocation.
/// 旧任务元数据不能证明新的服务启动已就绪。
#[test]
fn invocation_scopes_runtime_metadata() {
    let root = tempfile::tempdir().unwrap();
    let store = ConfigStore::new(root.path()).unwrap();
    let guest = runtime(&store);
    let recorder = Recorder::new(&store, "apps", &guest).unwrap();
    let service = status(&recorder, Phase::Running);
    let directory = directory(&store, "apps").unwrap();
    fs::create_dir_all(&directory).unwrap();
    let mut run = record(&service);
    run.invocation = Some("old-invocation".into());
    super::super::write_record(&directory.join("current.toml"), &run).unwrap();
    assert!(recorder.run(&store, "apps", &service).unwrap().is_none());
    run.invocation = Some(service.invocation.clone());
    super::super::write_record(&directory.join("current.toml"), &run).unwrap();
    assert!(recorder.run(&store, "apps", &service).unwrap().is_some());
    run.origin = None;
    super::super::write_record(&directory.join("current.toml"), &run).unwrap();
    assert!(recorder.run(&store, "apps", &service).is_err());
    run.origin = Some(StartOrigin::Autostart);
    run.machine = "wrong-machine".into();
    super::super::write_record(&directory.join("current.toml"), &run).unwrap();
    assert!(recorder.run(&store, "apps", &service).is_err());
}

/// A stop issued before run publication survives retries and never targets the next invocation.
/// 运行记录发布前发出的停止请求可跨重试保留，且不影响下一次运行。
#[test]
fn pending_stop_precedes_machine_registration() {
    let root = tempfile::tempdir().unwrap();
    let store = ConfigStore::new(root.path()).unwrap();
    let guest = runtime(&store);
    let recorder = Recorder::new(&store, "apps", &guest).unwrap();
    let service = status(&recorder, Phase::Starting);
    let directory = directory(&store, "apps").unwrap();
    fs::create_dir_all(&directory).unwrap();
    recorder.request_shutdown(&store, "apps", &service).unwrap();
    assert!(metadata(&store, "apps").unwrap().is_none());
    let restored = Recorder::new(&store, "apps", &guest).unwrap();
    assert!(
        restored
            .shutdown_requested(&store, "apps", &service)
            .unwrap()
    );
    let next = Status {
        invocation: "next-invocation".into(),
        ..service
    };
    assert!(!restored.shutdown_requested(&store, "apps", &next).unwrap());
}

/// Publishing a shutdown marker never races to overwrite the recorder's finished flag.
/// 发布关机标记不会竞争覆盖记录器完成标记。
#[test]
fn shutdown_marker_is_independent_of_run_completion() {
    let root = tempfile::tempdir().unwrap();
    let store = ConfigStore::new(root.path()).unwrap();
    let guest = runtime(&store);
    let recorder = Recorder::new(&store, "apps", &guest).unwrap();
    let service = status(&recorder, Phase::Running);
    let directory = directory(&store, "apps").unwrap();
    fs::create_dir_all(&directory).unwrap();
    let mut run = record(&service);
    super::super::write_record(&directory.join("current.toml"), &run).unwrap();
    fs::write(directory.join("console.log"), b"before shutdown\n").unwrap();
    mark_shutdown(&store, "apps").unwrap();
    run.finished = true;
    run.exit_code = Some(0);
    super::super::write_record(&directory.join("current.toml"), &run).unwrap();
    super::super::write_record(&directory.join("boot-test.toml"), &run).unwrap();
    fs::write(
        directory.join("console.log"),
        b"before shutdown\nlast bytes\n",
    )
    .unwrap();
    mark_shutdown(&store, "apps").unwrap();
    let current = current(&store, "apps").unwrap();
    assert!(current.finished);
    assert_eq!(current.shutdown_offset, Some(16));
    assert_eq!(
        task(&store, "apps", "boot-test").unwrap().shutdown_offset,
        Some(16)
    );
    assert!(task(&store, "apps", "../boot-test").is_err());
}

/// Shutdown cannot complete before the service exits and the last output is committed.
/// 服务退出且最后输出提交之前，关机不能完成。
#[test]
fn shutdown_waits_for_service_and_durable_output() {
    let root = tempfile::tempdir().unwrap();
    let store = ConfigStore::new(root.path()).unwrap();
    let guest = runtime(&store);
    let recorder = Recorder::new(&store, "apps", &guest).unwrap();
    let mut service = status(&recorder, Phase::Running);
    let directory = directory(&store, "apps").unwrap();
    fs::create_dir_all(&directory).unwrap();
    let mut run = record(&service);
    super::super::write_record(&directory.join("current.toml"), &run).unwrap();
    assert!(!recorder.drained(&store, "apps", &service, false).unwrap());
    service.phase = Phase::Inactive;
    assert!(recorder.drained(&store, "apps", &service, false).is_err());
    fs::write(directory.join("console.log"), b"last shutdown bytes\r\n").unwrap();
    run.finished = true;
    run.exit_code = Some(0);
    super::super::write_record(&directory.join("current.toml"), &run).unwrap();
    assert!(!recorder.drained(&store, "apps", &service, true).unwrap());
    service.phase = Phase::Running;
    assert!(!recorder.drained(&store, "apps", &service, false).unwrap());
    service.phase = Phase::Missing;
    assert!(recorder.drained(&store, "apps", &service, false).unwrap());
    assert_eq!(
        read(&store, "apps", &mut 0).unwrap(),
        b"last shutdown bytes\r\n"
    );
}

/// Exiting a recorder releases its lock even while its executed child survives.
/// 即使执行的子进程存活，记录器退出也会释放锁。
#[test]
fn defensive_lock_is_not_inherited_or_deleted() {
    let root = tempfile::tempdir().unwrap();
    let guard = capture_lock(root.path(), true).unwrap();
    assert!(capture_lock(root.path(), false).is_err());
    let mut child = Command::new("/bin/sleep").arg("10").spawn().unwrap();
    drop(guard);
    // Concurrent tests may fork with this descriptor briefly before their own exec.
    // 并发测试可能在各自 exec 前短暂 fork 并继承此描述符。
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let second = loop {
        match capture_lock(root.path(), false) {
            Ok(lock) => break lock,
            Err(error) if std::time::Instant::now() >= deadline => panic!("{error}"),
            Err(_) => thread::sleep(Duration::from_millis(10)),
        }
    };
    assert!(second.is_some());
    assert!(child.try_wait().unwrap().is_none());
    assert!(root.path().join("capture.lock").exists());
    child.kill().unwrap();
    child.wait().unwrap();
}
