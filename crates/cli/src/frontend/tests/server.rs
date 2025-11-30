use super::common::*;
use super::*;

#[test]
fn server_mode_reports_native_unavailability_and_does_not_delegate() {
    use std::fs;
    use std::io;
    use tempfile::tempdir;

    let _env_lock = ENV_LOCK.lock().expect("env lock");

    let temp = tempdir().expect("tempdir");
    let script_path = temp.path().join("server.sh");
    let marker_path = temp.path().join("marker.txt");

    fs::write(
        &script_path,
        r#"#!/bin/sh
set -eu
echo invoked > "$1"
exit 37
"#,
    )
    .expect("write script");

    let mut stdout = io::sink();
    let mut stderr = Vec::new();
    let exit_code = run(
        [
            OsString::from(RSYNC),
            OsString::from("--server"),
            OsString::from("--sender"),
            OsString::from("."),
            OsString::from("dest"),
        ],
        &mut stdout,
        &mut stderr,
    );

    assert_eq!(exit_code, 1);
    assert!(
        !marker_path.exists(),
        "fallback script must not be executed"
    );

    let stderr_text = String::from_utf8(stderr).expect("stderr utf8");
    assert!(stderr_text.contains("native server mode is not yet implemented"));
    assert_contains_server_trailer(&stderr_text);
}
