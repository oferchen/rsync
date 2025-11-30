use super::common::*;
use super::*;

#[test]
fn server_mode_reports_unimplemented_without_delegating() {
    use std::fs;
    use tempfile::tempdir;

    let _env_lock = ENV_LOCK.lock().expect("env lock");

    let temp = tempdir().expect("tempdir");
    let script_path = temp.path().join("server.sh");
    let marker_path = temp.path().join("marker.txt");

    fs::write(
        &script_path,
        r#"#!/bin/sh
set -eu
printf 'invoked' > "${SERVER_MARKER:?}" 
"#,
    )
    .expect("write script");

    let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
    let _marker_guard = EnvGuard::set("SERVER_MARKER", marker_path.as_os_str());

    let mut stdout = Vec::new();
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
    assert!(!marker_path.exists(), "server marker should not be created");
    assert!(stdout.is_empty(), "server mode should not write to stdout");

    let stderr_text = String::from_utf8(stderr).expect("stderr utf8");
    assert!(
        stderr_text.contains("remote server mode is not yet implemented"),
        "diagnostic should explain native server gap"
    );
    assert_contains_server_trailer(&stderr_text);
}
