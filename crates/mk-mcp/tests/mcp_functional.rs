#![cfg(feature = "mcp")]

use std::process::Command;

#[test]
fn test_mk_mcp_cli_help() {
    let bin_path = env!("CARGO_BIN_EXE_mk-mcp");
    let output = Command::new(bin_path)
        .arg("--help")
        .output()
        .expect("failed to execute mk-mcp --help");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("mk-mcp"));
    assert!(stdout.contains("--address"));
    assert!(stdout.contains("--login"));
    assert!(stdout.contains("--password"));
    assert!(stdout.contains("--policy"));
    assert!(stdout.contains("MIKROTIK_ADDR"));
    assert!(stdout.contains("MIKROTIK_USER"));
    assert!(stdout.contains("MIKROTIK_PASS"));
    assert!(stdout.contains("MIKROTIK_MCP_POLICY"));
}

#[test]
fn test_mk_mcp_requires_required_args() {
    let bin_path = env!("CARGO_BIN_EXE_mk-mcp");
    let output = Command::new(bin_path)
        .env_remove("MIKROTIK_ADDR")
        .env_remove("MIKROTIK_USER")
        .env_remove("MIKROTIK_PASS")
        .output()
        .expect("failed to execute mk-mcp without args");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("required"));
}
