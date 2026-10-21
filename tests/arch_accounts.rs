#[test]
fn isolated_account_tables() {
    let status = std::process::Command::new("python3")
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/arch_accounts.py"
        ))
        .status()
        .unwrap();
    assert!(status.success());
}
