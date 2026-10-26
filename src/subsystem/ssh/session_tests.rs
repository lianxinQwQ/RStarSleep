//! Async ownership, backpressure, EOF and dashboard-output regressions.
//! 异步所有权、背压、EOF 及面板输出回归测试。
use super::*;
use crate::subsystem::management::tests::fixture;
use std::os::fd::AsRawFd;

#[tokio::test]
async fn resize_bursts_keep_the_final_window_size() {
    let (sender, mut receiver) = watch::channel((80, 24));
    for width in 81..150 {
        sender.send_replace((width, 30));
    }
    assert_eq!(window(&mut receiver).await, (149, 30));
    drop(sender);
    assert!(
        tokio::time::timeout(Duration::from_millis(10), window(&mut receiver))
            .await
            .is_err()
    );
}

fn job(workspace: &Workspace, state: &str, code: Option<i32>) -> admin::Job {
    let directory = admin::directory(&workspace.store, &workspace.id).unwrap();
    fs::create_dir_all(&directory).unwrap();
    let job = admin::Job {
        task: "build-test".into(),
        operation: "build".into(),
        state: state.into(),
        started: 1,
        finished: code.map(|_| 2),
        exit_code: code,
        error: None,
    };
    super::super::super::write_record(&directory.join("build-test.toml"), &job).unwrap();
    job
}

#[test]
fn cancellation_preserves_typeahead_and_ignores_paste_controls() {
    let mut state = FollowInput::default();
    let mut pending = b"draft\x1b[20".to_vec();
    assert!(!state.cancelled(&mut pending));
    pending.extend_from_slice(b"0~\x03\x1b[201~more\x03status\r");
    assert!(state.cancelled(&mut pending));
    assert_eq!(pending, b"draft\x1b[200~\x03\x1b[201~morestatus\r");
}

#[tokio::test]
async fn pending_cancel_works_even_when_output_is_blocked() {
    let (_dir, w) = fixture();
    let mut input = tokio::io::empty();
    let (mut output, _reader) = tokio::io::duplex(1);
    let mut pending = b"draft\x03status\r".to_vec();
    let result = tokio::time::timeout(
        Duration::from_secs(1),
        follow(&w, &mut input, &mut output, None, 0, None, &mut pending),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(result, 0);
    assert_eq!(pending, b"draftstatus\r");
}

#[tokio::test]
async fn eof_waits_for_job_completion_and_drains_final_log() {
    let (_dir, w) = fixture();
    job(&w, "running", None);
    let directory = admin::directory(&w.store, &w.id).unwrap();
    let lock = fs::File::create(directory.join("build-test.lock")).unwrap();
    assert_eq!(
        unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
        0
    );
    let path = directory.join("build-test.log");
    fs::write(&path, b"before\n").unwrap();
    let mut output = Vec::new();
    let mut input = tokio::io::empty();
    let mut pending = Vec::new();
    let update = async {
        tokio::time::sleep(Duration::from_millis(80)).await;
        fs::write(
            &path,
            [b"before\n".as_slice(), &vec![b'x'; 30000], b"\nFINAL\n"].concat(),
        )
        .unwrap();
        job(&w, "complete", Some(7));
    };
    let read = follow(
        &w,
        &mut input,
        &mut output,
        Some("build-test".into()),
        0,
        Some(path.clone()),
        &mut pending,
    );
    let (result, ()) =
        tokio::time::timeout(Duration::from_secs(3), async { tokio::join!(read, update) })
            .await
            .unwrap();
    assert_eq!(result.unwrap(), 7);
    assert!(output.ends_with(b"FINAL\n"));
    assert_eq!(output, fs::read(path).unwrap());
}

#[tokio::test]
async fn follow_accepts_cancellation_under_backpressure_without_swallowing_text() {
    let (_dir, w) = fixture();
    job(&w, "complete", Some(0));
    let path = admin::directory(&w.store, &w.id)
        .unwrap()
        .join("build-test.log");
    fs::write(&path, vec![b'x'; 20000]).unwrap();
    let (mut input, mut sender) = tokio::io::duplex(128);
    let (mut output, _reader) = tokio::io::duplex(1);
    let mut pending = Vec::new();
    let cancel = async {
        tokio::time::sleep(Duration::from_millis(80)).await;
        sender.write_all(b"status\r\x03jobs\r").await.unwrap();
    };
    let read = follow(
        &w,
        &mut input,
        &mut output,
        Some("build-test".into()),
        0,
        Some(path),
        &mut pending,
    );
    let (result, ()) =
        tokio::time::timeout(Duration::from_secs(2), async { tokio::join!(read, cancel) })
            .await
            .unwrap();
    assert_eq!(result.unwrap(), 0);
    assert_eq!(pending, b"status\rjobs\r");
}

#[tokio::test]
async fn dashboard_capture_is_bounded_and_utf8_is_decoded_after_all_writes() {
    let mut output = Vec::new();
    let mut capture = ViewOutput::new(&mut output, true, false);
    capture.write_all(&[0xe4, 0xbd]).await.unwrap();
    capture.write_all(&[0xa0]).await.unwrap();
    assert_eq!(capture.text(), "你");
    capture
        .write_all(&vec![b'x'; 4 * 1024 * 1024 - 3])
        .await
        .unwrap();
    let error = capture.write_all(b"x").await.unwrap_err().to_string();
    assert!(error.contains("超出"));
    assert!(error.contains("exceeds"));
    assert!(output.is_empty());
    let mut capture = ViewOutput::new(&mut output, false, true);
    capture.write_all(&vec![b'x'; 70000]).await.unwrap();
    assert!(capture.text().contains("Only the last 64 KiB"));
    assert_eq!(output.len(), 70000);
}
