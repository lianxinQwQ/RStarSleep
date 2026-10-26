//! Closed-loop workspace and terminal behavior tests.
//! 工作区及终端行为闭环测试。
use super::{Workspace, terminal::{Action, Terminal}};
use crate::config::ConfigStore;
use std::fs;

/// Create an ordinary directory fixture without privileged storage operations.
/// 创建普通目录测试环境，不执行特权存储操作。
/// # Arguments
/// No parameters. 无参数。
pub(crate) fn fixture() -> (tempfile::TempDir, Workspace) {
    let directory = tempfile::tempdir().unwrap();
    fs::create_dir(directory.path().join("config")).unwrap();
    fs::create_dir(directory.path().join("systems")).unwrap();
    fs::write(directory.path().join("config/rstarsleep.toml"), "schema_version=1\nsubsystems=['apps']\n").unwrap();
    let store = ConfigStore::new(directory.path()).unwrap();
    store.initialize_system("apps").unwrap();
    let workspace = Workspace::new(store, "apps").unwrap();
    (directory, workspace)
}

/// All frontends expose only build/script and reject links and traversal.
/// 全部前端仅暴露 build/script，并拒绝链接及越界。
/// # Arguments
/// No parameters. 无参数。
#[test]
fn view_contains_only_configuration_inputs() {
    let (_directory, workspace) = fixture();
    assert_eq!(workspace.list("/").unwrap(), ["build", "script"]);
    for path in ["/subsystem/config.toml", "../../config", "/build/../../etc", "/script/../subsystem"] { assert!(workspace.path(path).is_err(), "{path}"); }
    assert_eq!(Workspace::normalize("/build", "../script").unwrap(), "/script");
    let link = workspace.path("/script/link").unwrap();
    std::os::unix::fs::symlink("/etc/passwd", &link).unwrap();
    assert!(workspace.read("/script/link").is_err());
    assert!(workspace.mutate("mv", "/build/config.conf", Some("/script")).is_err());
}

/// Open writes are private, block freezing, and never publish after failure.
/// 打开的写入保持私有、阻止冻结，失败后绝不发布。
/// # Arguments
/// No parameters. 无参数。
#[test]
fn uploads_are_transactional_and_failed_writes_are_discarded() {
    let (_directory, workspace) = fixture();
    let mut upload = workspace.upload("/script/data", true, false).unwrap();
    upload.write(0,b"complete\0binary").unwrap();
    assert!(workspace.read("/script/data").is_err());
    assert!(crate::config::lock_configuration(&workspace.store.system_layout("apps").unwrap().root).is_err());
    upload.commit().unwrap();
    assert_eq!(workspace.read("/script/data").unwrap(),b"complete\0binary");
    let mut upload=workspace.upload("/script/data",true,false).unwrap();
    upload.write(0,b"partial").unwrap();
    assert!(upload.write(u64::MAX,b"overflow").is_err());
    assert!(upload.commit().is_err());
    assert_eq!(workspace.read("/script/data").unwrap(),b"complete\0binary");
    let mut upload=workspace.upload("/script/data",true,false).unwrap();upload.write(0,b"disconnected").unwrap();drop(upload);
    assert_eq!(workspace.read("/script/data").unwrap(),b"complete\0binary");
}

/// An editor cannot overwrite a second session's newer save.
/// 编辑器不能覆盖另一会话较新的保存。
/// # Arguments
/// No parameters. 无参数。
#[test]
fn editor_save_compares_original_under_the_write_lock() {
    let (_directory, workspace) = fixture();
    workspace.write("/script/example.sh",b"old").unwrap();
    workspace.write_expected("/script/example.sh",Some(b"old"),b"new").unwrap();
    assert!(workspace.write_expected("/script/example.sh",Some(b"old"),b"lost update").is_err());
    assert_eq!(workspace.read("/script/example.sh").unwrap(),b"new");
}

/// UTF-8 editing, split escape sequences, shell handoff and TUI return work together.
/// UTF-8 编辑、分包转义、shell 交接及 TUI 返回协同工作。
/// # Arguments
/// No parameters. 无参数。
#[test]
fn interactive_terminal_edits_and_restores_its_prompt() {
    let (_directory, workspace) = fixture(); let mut terminal=Terminal::new("apps");
    terminal.input(b"edit /script/example.sh\r",&workspace).unwrap();
    terminal.input("i你好\n".as_bytes(),&workspace).unwrap();
    terminal.input(&[27],&workspace).unwrap();terminal.flush_escape(&workspace).unwrap();
    terminal.input(b":wq\r",&workspace).unwrap();
    assert_eq!(workspace.read("/script/example.sh").unwrap(),"你好\n".as_bytes());
    terminal.input(b"status\r",&workspace).unwrap();
    terminal.input(b"\x1b",&workspace).unwrap();terminal.input(b"[A",&workspace).unwrap();
    let update=terminal.input(b"\r",&workspace).unwrap();assert!(matches!(update.action,Some(Action::Command(words)) if words==["status"]));
    terminal.input(b"tui\r",&workspace).unwrap();let update=terminal.input(b"q",&workspace).unwrap();assert!(String::from_utf8(update.output).unwrap().contains("apps:/"));
    let update=terminal.input(b"shell\rwhoami\r",&workspace).unwrap();assert!(matches!(update.action,Some(Action::Shell)));assert_eq!(terminal.take_pending(),b"whoami\r");
    assert!(super::terminal::parse("status; rm /anything").is_err());
    assert!(super::terminal::parse("cat $(id)").is_err());
    assert_eq!(super::terminal::parse("cat '/script/a b'").unwrap(),["cat","/script/a b"]);
}
