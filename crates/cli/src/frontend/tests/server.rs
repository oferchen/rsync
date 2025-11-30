use super::common::*;
use super::*;

use core::fallback::CLIENT_FALLBACK_ENV;
use std::fs;
use tempfile::tempdir;

#[test]
fn server_mode_reports_unavailability_with_trailer() {
    let _env_lock = ENV_LOCK.lock().expect("env lock");

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
    assert!(stdout.is_empty(), "server mode should not write stdout");

    let stderr_text = String::from_utf8(stderr).expect("stderr utf8");
    assert!(
        stderr_text.contains("native server mode is not yet available"),
        "diagnostic should describe unimplemented native server"
    );
    assert_contains_server_trailer(&stderr_text);
}

#[test]
fn server_mode_does_not_delegate_to_fallback_binary() {
    let _env_lock = ENV_LOCK.lock().expect("env lock");

    let temp = tempdir().expect("tempdir");
    let script_path = temp.path().join("server.sh");
    let marker_path = temp.path().join("marker.txt");

    fs::write(
        &script_path,
        b"#!/bin/sh\nset -eu\nprintf 'should not run' > \"$SERVER_MARKER\"\n",
    )
    .expect("write script");
    let mut perms = fs::metadata(&script_path).expect("metadata").permissions();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        perms.set_mode(0o755);
    }
    fs::set_permissions(&script_path, perms).expect("set perms");

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
    assert!(
        fs::read(&marker_path).is_err(),
        "fallback binary should never be executed"
    );
    assert!(stdout.is_empty());

    let stderr_text = String::from_utf8(stderr).expect("stderr utf8");
    assert!(
        stderr_text.contains("native server mode is not yet available"),
        "diagnostic should explain native-only behaviour"
    );
    assert_contains_server_trailer(&stderr_text);
}
