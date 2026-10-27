//! Behavioral regressions for editing, paste, rendering and dashboard ownership.
//! 编辑、粘贴、绘制及面板所有权的行为回归测试。
use super::{
    terminal::{Action, Terminal},
    tests::fixture,
};

fn command(action: Option<Action>, expected: &[&str]) {
    match action {
        Some(Action::Command(words)) => assert_eq!(words, expected),
        _ => panic!("未收到命令动作 / expected a command action"),
    }
}
fn escape(terminal: &mut Terminal, workspace: &super::Workspace) {
    terminal.input(b"\x1b", workspace).unwrap();
    terminal.flush_escape(workspace).unwrap();
}

#[test]
fn cursor_keys_and_control_shortcuts_edit_utf8() {
    let (_dir, w) = fixture();
    for bytes in [
        &b"staus\x1b[D\x1b[Dt\r"[..],
        &b"tatus\x1b[Hs\x1b[F\r"[..],
        &b"stxatus\x1b[H\x1b[C\x1b[C\x1b[3~\r"[..],
        &b"staus\x01\x06\x06\x06t\x05\r"[..],
        &b"staus\x05\x02\x02t\r"[..],
        &b"junkstatus\x01\x06\x06\x06\x06\x15\x05\r"[..],
        &b"statusjunk\x01\x06\x06\x06\x06\x06\x06\x0b\r"[..],
        &b"status discarded\x17\r"[..],
    ] {
        command(
            Terminal::new("apps").input(bytes, &w).unwrap().action,
            &["status"],
        );
    }
    let mut t = Terminal::new("apps");
    command(
        t.input("status 界→字\x1b[D文\r".as_bytes(), &w)
            .unwrap()
            .action,
        &["status", "界→文字"],
    );
}

#[test]
fn fragmented_keys_and_unicode_are_preserved() {
    let (_dir, w) = fixture();
    let mut t = Terminal::new("apps");
    for bytes in [
        &b"status a"[..],
        &b"\xe2"[..],
        &b"\x86"[..],
        &b"\x92"[..],
        &b"\x1b"[..],
        &b"["[..],
        &b"D"[..],
    ] {
        assert!(t.input(bytes, &w).unwrap().action.is_none());
    }
    command(t.input(b"b\r", &w).unwrap().action, &["status", "ab→"]);
}

#[test]
fn history_restores_draft_and_cursor() {
    let (_dir, w) = fixture();
    let mut t = Terminal::new("apps");
    command(t.input(b"status\x1b[B\r", &w).unwrap().action, &["status"]);
    command(t.input(b"check\r", &w).unwrap().action, &["check"]);
    command(
        t.input(b"staus\x1b[D\x1b[D\x1b[A\x1b[Bt\r", &w)
            .unwrap()
            .action,
        &["status"],
    );
}

#[test]
fn invalid_paste_and_overflow_never_execute_a_truncated_prefix() {
    let (_dir, w) = fixture();
    for payload in [
        b"status\x1b[200~\ncheck\x1b[201~\r".to_vec(),
        b"status\x1b[200~x\x01y\x1b[201~\r".to_vec(),
        [vec![b'x'; 32769], b"TAIL\r".to_vec()].concat(),
    ] {
        let mut t = Terminal::new("apps");
        let update = t.input(&payload, &w).unwrap();
        assert!(update.action.is_none());
        assert!(!update.output.is_empty());
        command(t.input(b"status\r", &w).unwrap().action, &["status"]);
    }
    let mut t = Terminal::new("apps");
    t.input(&vec![b'x'; 32769], &w).unwrap();
    command(t.input(b"\x03status\r", &w).unwrap().action, &["status"]);
}

#[test]
fn fragmented_paste_is_atomic_and_literal_in_editor_normal_mode() {
    let (_dir, w) = fixture();
    let mut t = Terminal::new("apps");
    t.input(b"edit /script/example\r", &w).unwrap();
    for part in [
        &b"\x1b[20"[..],
        &b"0~xx\n\t"[..],
        "a→b↑↓←".as_bytes(),
        &b"\x1b[2"[..],
        &b"01~"[..],
    ] {
        assert!(t.input(part, &w).unwrap().action.is_none());
    }
    t.input(b":wq\r", &w).unwrap();
    assert_eq!(
        w.read("/script/example").unwrap(),
        "xx\n\ta→b↑↓←".as_bytes()
    );
    let mut t = Terminal::new("apps");
    for part in [
        &b"\x1b[2"[..],
        &b"00~sta"[..],
        &b"tus\x1b[20"[..],
        &b"1~"[..],
    ] {
        t.input(part, &w).unwrap();
    }
    command(t.input(b"\r", &w).unwrap().action, &["status"]);
}

#[test]
fn batches_redraw_once_and_editor_undoes_a_paste_once() {
    let (_dir, w) = fixture();
    let mut t = Terminal::new("apps");
    let u = t.input(&vec![b'x'; 8192], &w).unwrap();
    assert!(u.output.len() < 65536);
    let mut t = Terminal::new("apps");
    t.input(b"edit /script/example\r", &w).unwrap();
    let u = t
        .input(&[vec![b'i'], vec![b'x'; 8192]].concat(), &w)
        .unwrap();
    assert!(u.output.len() < 65536);
    escape(&mut t, &w);
    t.input(b":wq\r", &w).unwrap();
    assert_eq!(w.read("/script/example").unwrap(), vec![b'x'; 8192]);
    t.input(b"edit /script/example\r\x1b[200~xx\x1b[201~u:wq\r", &w)
        .unwrap();
    assert_eq!(w.read("/script/example").unwrap(), vec![b'x'; 8192]);
}

#[test]
fn long_lines_stay_on_one_physical_row_in_narrow_terminals() {
    use unicode_width::UnicodeWidthStr;
    let (_dir, w) = fixture();
    for width in [2, 20, 80] {
        let mut t = Terminal::new("apps");
        t.resize(width, 6);
        let out =
            String::from_utf8(t.input("界".repeat(100).as_bytes(), &w).unwrap().output).unwrap();
        assert!(!out.contains('\n'));
        let visible = out
            .split("\x1b[2K")
            .nth(1)
            .unwrap()
            .split('\r')
            .next()
            .unwrap();
        assert!(visible.width() < width as usize);
    }
}

#[test]
fn clear_and_errors_preserve_the_current_view_and_prompt() {
    let (_dir, w) = fixture();
    let mut t = Terminal::new("apps");
    let clear = t.input(b"clear\r", &w).unwrap();
    assert!(
        String::from_utf8(clear.output)
            .unwrap()
            .contains("\x1b[2J\x1b[H")
    );
    for bytes in [
        &b"cd /script/missing\r"[..],
        &b"cat /script/missing\r"[..],
        &b"status; bad\r"[..],
    ] {
        let u = t.input(bytes, &w).unwrap();
        assert!(u.action.is_none());
        let out = String::from_utf8(u.output).unwrap();
        assert!(out.contains("apps:/>"));
        assert!(!out.contains("\x1b[H"));
        assert!(out.contains("\r\n"));
        assert!(out.ends_with('C'));
    }
    t.input(b"edit /script/example\rihello", &w).unwrap();
    escape(&mut t, &w);
    w.write("/script/example", b"other session").unwrap();
    let out = String::from_utf8(t.input(b":w\r", &w).unwrap().output).unwrap();
    assert!(out.contains("file changed"));
    assert!(!out.contains("apps:/>"));
    assert!(out.contains("hello"));
    t.input(b":q!\r", &w).unwrap();
    command(t.input(b"status\r", &w).unwrap().action, &["status"]);
}

#[test]
fn dashboard_actions_file_browser_results_and_confirmation_work() {
    let (_dir, w) = fixture();
    let mut t = Terminal::new("apps");
    t.input(b"tui\r", &w).unwrap();
    assert!(t.is_tui());
    command(t.input(b"j", &w).unwrap().action, &["jobs"]);
    let out = String::from_utf8(t.finish("a long result\nsecond line")).unwrap();
    assert!(out.contains("second line"));
    assert!(t.input(b"d", &w).unwrap().action.is_none());
    assert!(
        String::from_utf8(t.resize(100, 30))
            .unwrap()
            .contains("Confirm stop")
    );
    assert!(t.input(b"n", &w).unwrap().action.is_none());
    t.input(b"d", &w).unwrap();
    command(t.input(b"y", &w).unwrap().action, &["stop"]);
    t.finish("");
    t.input(b"e\r\r", &w).unwrap();
    let out = String::from_utf8(t.resize(90, 25)).unwrap();
    assert!(out.contains("NORMAL"));
    t.input(b":q\r", &w).unwrap();
    assert!(t.is_tui());
    t.input(b"q", &w).unwrap();
    assert!(!t.is_tui());
}

#[test]
fn build_and_console_pages_keep_the_menu_and_do_not_start_a_build_on_entry() {
    use super::terminal::LiveView;
    let (_dir, w) = fixture();
    let mut t = Terminal::new("apps");
    t.input(b"tui\r", &w).unwrap();
    command(t.input(b"b", &w).unwrap().action, &["build", "status"]);
    assert_eq!(t.live_view(), Some(LiveView::Build));
    let out = String::from_utf8(
        t.update_live(None, "当前系统不在构建 / This system is not building")
            .unwrap(),
    )
    .unwrap();
    assert!(out.contains("Management workspace"));
    assert!(out.contains("This system is not building"));
    command(t.input(b"n", &w).unwrap().action, &["build"]);
    command(t.input(b"\x03", &w).unwrap().action, &["build", "stop"]);
    assert!(t.is_tui());
    command(t.input(b"c", &w).unwrap().action, &["console"]);
    let path = w
        .store
        .system_layout("apps")
        .unwrap()
        .root
        .join("console-test.log");
    std::fs::write(&path, b"FIRST\n\x1b[2J\x1b[HSECOND\n").unwrap();
    let out = String::from_utf8(t.update_live(Some((path, 0)), "Live output").unwrap()).unwrap();
    assert!(out.contains("Management workspace"));
    assert!(out.contains("SECOND"));
    assert!(!out.contains("\x1b[?1049l"));
    assert_eq!(t.live_view(), Some(LiveView::Console));
    assert!(t.input(b"\x03", &w).unwrap().action.is_none());
    assert!(t.is_tui());
    assert_eq!(t.live_view(), None);
}
