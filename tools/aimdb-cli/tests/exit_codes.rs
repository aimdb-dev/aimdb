//! The `aimdb` binary's exit code per failure class (issue #334).

use std::process::Command;

fn aimdb(args: &[&str]) -> i32 {
    let output = Command::new(env!("CARGO_BIN_EXE_aimdb"))
        .args(args)
        .env_remove("AIMDB_CONNECT")
        .output()
        .expect("run aimdb");
    output.status.code().expect("aimdb exited with a code")
}

#[test]
fn unreachable_instance_exits_3() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("missing.sock");
    let endpoint = format!("unix://{}", socket.display());
    assert_eq!(aimdb(&["--connect", &endpoint, "instance", "ping"]), 3);
}

#[test]
fn unsupported_endpoint_exits_2() {
    assert_eq!(aimdb(&["--connect", "bogus://x", "instance", "ping"]), 2);
}

#[test]
fn invalid_json_value_exits_2() {
    assert_eq!(aimdb(&["record", "set", "r", "{not json", "--dry-run"]), 2);
}

#[test]
fn clap_usage_error_exits_2() {
    assert_eq!(aimdb(&["no-such-command"]), 2);
}
