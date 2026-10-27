//! Build ownership, cancellation and process-group cleanup regressions.
//! 构建所有权、取消与进程组清理的回归测试。
use super::*;
use crate::subsystem::management::tests::fixture;
use std::sync::mpsc;

fn running(workspace: &super::super::management::Workspace) -> (File, File) {
    let path = directory(&workspace.store, &workspace.id).unwrap();
    fs::create_dir_all(&path).unwrap();
    let job = Job {
        task: "build-test".into(),
        operation: "build".into(),
        state: "running".into(),
        started: 1,
        finished: None,
        exit_code: None,
        error: None,
    };
    super::super::write_record(&path.join("build-test.toml"), &job).unwrap();
    let lock = File::create(path.join("build-test.lock")).unwrap();
    assert_eq!(
        unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
        0
    );
    let supervisor = File::create(path.join("build-test.supervisor")).unwrap();
    assert_eq!(
        unsafe { libc::flock(supervisor.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
        0
    );
    (lock, supervisor)
}

#[test]
fn queued_build_does_not_misreport_a_supervisor_still_acquiring_its_lock() {
    let (_dir, workspace) = fixture();
    let (_worker, supervisor) = running(&workspace);
    let path = directory(&workspace.store, &workspace.id)
        .unwrap()
        .join("build-test.toml");
    let mut job: Job = super::super::read_record(&path).unwrap();
    job.state = "queued".into();
    super::super::write_record(&path, &job).unwrap();
    drop(supervisor);
    let job = active_build(&workspace.store, &workspace.id)
        .unwrap()
        .unwrap();
    assert_eq!(job.state, "queued");
    assert!(job.error.is_none());
    assert!(
        stop_build(&workspace.store, &workspace.id)
            .unwrap()
            .is_some()
    );
}

#[test]
fn missing_supervisor_keeps_build_busy_and_reports_cancellation_failure() {
    let (_dir, workspace) = fixture();
    let (_worker, supervisor) = running(&workspace);
    drop(supervisor);
    let job = active_build(&workspace.store, &workspace.id)
        .unwrap()
        .unwrap();
    assert!(job.error.unwrap().contains("supervisor was lost"));
    let error = stop_build(&workspace.store, &workspace.id)
        .unwrap_err()
        .to_string();
    assert!(error.contains("无法安全停止"));
    assert!(error.contains("cannot stop safely"));
}

#[test]
fn active_build_requires_a_live_worker_and_stop_is_task_specific() {
    let (_dir, workspace) = fixture();
    let lock = running(&workspace);
    assert_eq!(
        active_build(&workspace.store, &workspace.id)
            .unwrap()
            .unwrap()
            .task,
        "build-test"
    );
    assert!(
        stop_build(&workspace.store, &workspace.id)
            .unwrap()
            .is_some()
    );
    assert!(
        directory(&workspace.store, &workspace.id)
            .unwrap()
            .join("build-test.cancel")
            .exists()
    );
    drop(lock);
    assert!(
        active_build(&workspace.store, &workspace.id)
            .unwrap()
            .is_none()
    );
    assert!(
        stop_build(&workspace.store, &workspace.id)
            .unwrap()
            .is_none()
    );
}

fn output_closes(mut output: std::process::ChildStdout) -> mpsc::Receiver<Vec<u8>> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        output.read_to_end(&mut bytes).unwrap();
        tx.send(bytes).unwrap();
    });
    rx
}

#[test]
fn cancellation_terminates_term_resistant_workers_and_descendants() {
    let dir = tempfile::tempdir().unwrap();
    let cancel = dir.path().join("cancel");
    let ready = dir.path().join("ready");
    let mut child = Command::new("sh")
        .args([
            "-c",
            "trap '' TERM; sleep 30 & echo ready > \"$TASK_READY\"; wait",
        ])
        .env("TASK_READY", &ready)
        .process_group(0)
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let output = output_closes(child.stdout.take().unwrap());
    let deadline = Instant::now() + Duration::from_secs(3);
    while !ready.exists() {
        assert!(
            Instant::now() < deadline,
            "工作进程未就绪 / worker was not ready"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    fs::write(&cancel, "stop").unwrap();
    let (_, cancelled) = supervise(child, &cancel).unwrap();
    assert!(cancelled);
    output
        .recv_timeout(Duration::from_secs(3))
        .expect("子进程仍持有日志管道 / descendant still holds log pipe");
}

#[test]
fn normal_exit_keeps_its_status_and_cleans_background_children() {
    let dir = tempfile::tempdir().unwrap();
    let mut child = Command::new("sh")
        .args(["-c", "sleep 30 & exit 7"])
        .process_group(0)
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let output = output_closes(child.stdout.take().unwrap());
    let (status, cancelled) = supervise(child, &dir.path().join("cancel")).unwrap();
    assert!(!cancelled);
    assert_eq!(status.code(), Some(7));
    output
        .recv_timeout(Duration::from_secs(3))
        .expect("后台子进程未退出 / background descendant did not exit");
}
