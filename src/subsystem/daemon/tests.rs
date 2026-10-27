//! Daemon authorization, concurrency and lease regression checks.
//! Daemon 授权、并发及租约回归检查。
use super::installation::argument;
use super::*;

/// A failed same-version boot refresh is retried and successful refreshes are deduplicated.
/// 同版本启动刷新失败后会重试，成功刷新后不重复执行。
/// # Arguments
/// No parameters. 无参数。
#[test]
fn failed_export_refresh_is_retried_after_same_version_restart() {
    let mut state = State {
        indexed_version: Some("v1".into()),
        ..State::default()
    };
    let calls = std::cell::Cell::new(0);
    let result = refresh_exports(&mut state, "v1", true, || {
        calls.set(calls.get() + 1);
        Err(Error::Invalid("temporary synchronization failure".into()))
    });
    assert!(result.is_err());
    assert_eq!(calls.get(), 1);
    assert!(state.indexed_version.is_none());

    refresh_exports(&mut state, "v1", false, || {
        calls.set(calls.get() + 1);
        Ok(())
    })
    .unwrap();
    assert_eq!(calls.get(), 2);
    assert_eq!(state.indexed_version.as_deref(), Some("v1"));
    refresh_exports(&mut state, "v1", false, || {
        calls.set(calls.get() + 1);
        Err(Error::Invalid("already synchronized".into()))
    })
    .unwrap();
    assert_eq!(calls.get(), 2);

    refresh_exports(&mut state, "v2", false, || {
        calls.set(calls.get() + 1);
        Ok(())
    })
    .unwrap();
    assert_eq!(calls.get(), 3);
    assert_eq!(state.indexed_version.as_deref(), Some("v2"));
}

/// A restarted generation is immune to obsolete lease releases.
/// 重启运行代不受旧租约释放影响。
/// # Arguments
/// No parameters. 无参数。
#[test]
fn generation_leases() {
    let slot = Arc::new(Slot {
        state: Mutex::new(State {
            generation: 2,
            leases: 1,
            ..State::default()
        }),
        monitoring: AtomicBool::new(false),
    });
    drop(Lease {
        slot: slot.clone(),
        generation: 1,
        _activity: None,
    });
    assert_eq!(slot.state.lock().unwrap().leases, 1);
    drop(Lease {
        slot: slot.clone(),
        generation: 2,
        _activity: None,
    });
    assert_eq!(slot.state.lock().unwrap().leases, 0);
}
/// Explicit/autostart instances never satisfy demand idle policy.
/// 显式和自启实例永不满足按需空闲策略。
/// # Arguments
/// No parameters. 无参数。
#[test]
fn idle_origins() {
    let mut state = State {
        origin: Some(StartOrigin::Demand),
        released: Instant::now() - Duration::from_secs(60),
        ..State::default()
    };
    assert!(idle(&state, 30));
    state.leases = 1;
    assert!(!idle(&state, 30));
    state.leases = 0;
    for origin in [StartOrigin::Explicit, StartOrigin::Autostart] {
        state.origin = Some(origin);
        assert!(!idle(&state, 30));
    }
    assert!(!idle(&state, 0));
}

/// Surviving tasks and failed probes refresh the grace instead of consuming it.
/// 存活任务及探测失败刷新宽限时间，避免提前消耗。
/// # Arguments
/// No parameters. 无参数。
#[test]
fn recovered_applications_receive_the_full_idle_grace() {
    let mut state = State {
        origin: Some(StartOrigin::Demand),
        released: Instant::now() - Duration::from_secs(60),
        ..State::default()
    };
    assert!(!idle_after_probe(&mut state, 30, Ok(true)));
    assert!(!idle_after_probe(&mut state, 30, Ok(false)));
    state.released = Instant::now() - Duration::from_secs(60);
    assert!(idle_after_probe(&mut state, 30, Ok(false)));
    assert!(!idle_after_probe(
        &mut state,
        30,
        Err(Error::Invalid("probe unavailable".into()))
    ));
    assert!(!idle_after_probe(&mut state, 30, Ok(false)));
}
/// Sixteen concurrent callers resolve one unique per-system lifecycle mutex.
/// 十六个并发调用者解析同一个系统生命周期互斥锁。
/// # Arguments
/// No parameters. 无参数。
#[test]
fn concurrent_callers_share_exactly_one_slot() {
    let root = tempfile::tempdir().unwrap();
    let manager = Arc::new(Manager {
        store: ConfigStore::new(root.path()).unwrap(),
        directory: root.path().into(),
        slots: Mutex::new(BTreeMap::new()),
        connections: Mutex::new(BTreeMap::new()),
        builds: Arc::new(BuildState::default()),
        supervisor: Arc::new(Systemd),
    });
    let barrier = Arc::new(std::sync::Barrier::new(17));
    let workers = (0..16)
        .map(|_| {
            let manager = manager.clone();
            let barrier = barrier.clone();
            thread::spawn(move || {
                barrier.wait();
                manager.slot("app").unwrap()
            })
        })
        .collect::<Vec<_>>();
    barrier.wait();
    let slots = workers
        .into_iter()
        .map(|worker| worker.join().unwrap())
        .collect::<Vec<_>>();
    assert!(slots.iter().all(|slot| Arc::ptr_eq(slot, &slots[0])));
    assert_eq!(manager.slots.lock().unwrap().len(), 1);
    assert!(!Arc::ptr_eq(&manager.slot("other").unwrap(), &slots[0]));
    for invalid in ["", "../app", "app/other", "--root"] {
        assert!(manager.slot(invalid).is_err());
    }
    assert_eq!(manager.slots.lock().unwrap().len(), 2);
}

/// Systemd paths preserve literal percent/dollar/quote/backslash characters.
/// Systemd 路径保留百分号、美元符号、引号及反斜线字面字符。
/// # Arguments
/// No parameters. 无参数。
#[test]
fn installation_arguments_are_literal() {
    assert_eq!(
        argument(Path::new("/opt/a b/%root/$BIN\"tool\\bin")).unwrap(),
        r#""/opt/a b/%%root/$$BIN\"tool\\bin""#
    );
    assert!(argument(Path::new("/opt/a\nb")).is_err());
    use std::os::unix::ffi::OsStringExt;
    assert!(
        argument(&PathBuf::from(std::ffi::OsString::from_vec(
            b"/opt/app-\xff".to_vec()
        )))
        .is_err()
    );
}

/// Non-root lifecycle requests fail before any manager preparation or file mutation.
/// 非 root 生命周期请求在管理器准备及文件变更前失败。
/// # Arguments
/// No parameters. 无参数。
#[test]
fn unauthorized_lifecycle_request_does_not_prepare() {
    let root = tempfile::tempdir().unwrap();
    let manager = Manager {
        store: ConfigStore::new(root.path()).unwrap(),
        directory: root.path().into(),
        slots: Mutex::new(BTreeMap::new()),
        connections: Mutex::new(BTreeMap::new()),
        builds: Arc::new(BuildState::default()),
        supervisor: Arc::new(Systemd),
    };
    let (stream, response) = UnixStream::pair().unwrap();
    let wire = Arc::new(Wire {
        stream: Mutex::new(response),
        disconnected: AtomicBool::new(false),
    });
    let result = manager.serve_request(
        BufReader::new(stream),
        wire,
        Peer {
            uid: 1000,
            gid: 1000,
            pid: std::process::id() as i32,
        },
        protocol::request(Operation::Restart { id: "app".into() }),
    );
    assert!(matches!(result, Err(Error::Invalid(message)) if message.contains("root")));
    assert!(manager.slots.lock().unwrap().is_empty());
    assert_eq!(fs::read_dir(root.path()).unwrap().count(), 0);
}

/// Bytes arriving within each idle timeout still obey the total initial-frame deadline.
/// 每个空闲时限内到达的字节仍遵守首帧总截止时间。
/// # Arguments
/// No parameters. 无参数。
#[test]
fn partial_frames_cannot_extend_the_initial_deadline() {
    use std::io::Write;
    let (server, mut client) = UnixStream::pair().unwrap();
    server
        .set_read_timeout(Some(Duration::from_secs(1)))
        .unwrap();
    let writer = thread::spawn(move || {
        if client.write_all(b"{").is_err() {
            return;
        }
        for _ in 0..200 {
            thread::sleep(Duration::from_millis(10));
            if client.write_all(b" ").is_err() {
                break;
            }
        }
    });
    let start = Instant::now();
    assert!(
        super::server::read_initial(&mut BufReader::new(server), Duration::from_millis(40))
            .is_err()
    );
    assert!(start.elapsed() < Duration::from_secs(1));
    writer.join().unwrap();
}
