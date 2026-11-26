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

/// An instance that completes the handshake and then never answers: the
/// request times out with exit code 3 instead of hanging (#474).
#[test]
fn silent_instance_times_out_with_exit_3() {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixListener;
    use std::time::{Duration, Instant};

    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("silent.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut write = stream.try_clone().unwrap();
        for line in BufReader::new(stream).lines() {
            let Ok(line) = line else { return };
            let frame: serde_json::Value = serde_json::from_str(&line).unwrap();
            if frame["method"] == "hello" {
                let reply = serde_json::json!({
                    "t": "reply", "id": frame["id"], "ok": {
                        "version": "4.0", "server": "aimdb",
                        "permissions": ["read"], "writable_records": [],
                    },
                });
                writeln!(write, "{reply}").unwrap();
            } else if frame["t"] == "ping" {
                writeln!(write, r#"{{"t":"pong"}}"#).unwrap();
            }
            // Anything else gets no reply.
        }
    });

    let endpoint = format!("unix://{}", socket.display());
    let started = Instant::now();
    let code = aimdb(&[
        "--connect",
        &endpoint,
        "--request-timeout",
        "1",
        "record",
        "get",
        "x",
    ]);
    assert_eq!(code, 3);
    assert!(started.elapsed() < Duration::from_secs(10));
}

#[test]
fn zero_request_timeout_is_a_usage_error() {
    assert_eq!(aimdb(&["--request-timeout", "0", "instance", "list"]), 2);
}
