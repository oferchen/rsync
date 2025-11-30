use super::common::*;
use super::*;

#[cfg(unix)]
#[test]
fn server_mode_reports_unavailable_and_does_not_spawn() {
    use tempfile::tempdir;

    let _env_lock = ENV_LOCK.lock().expect("env lock");

    let temp = tempdir().expect("tempdir");
    let script_path = temp.path().join("server.sh");
    let marker_path = temp.path().join("marker.txt");
    let script = format!(
        "#!/bin/sh\nset -eu\nprintf 'spawned' > {}\n",
        marker_path.display()
    );
    write_executable_script(&script_path, &script);

    // Even if a fallback environment is set, the implementation must not spawn it.
    let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());

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

    assert_eq!(exit_code, 4);
    assert!(stdout.is_empty(), "server mode should not write to stdout");
    assert!(!stderr.is_empty(), "server mode should emit diagnostics");
    assert!(
        !marker_path.exists(),
        "fallback helper must not be executed"
    );

    let stderr_text = String::from_utf8(stderr).expect("stderr utf8");
    assert!(
        stderr_text.contains("remote server mode is unavailable"),
        "stderr should describe missing native server support"
    );
    assert_contains_server_trailer(&stderr_text);
}

#[test]
fn server_mode_sets_server_role_in_trailer() {
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

    assert_eq!(exit_code, 4);
    assert!(stdout.is_empty());

    let stderr_text = String::from_utf8(stderr).expect("stderr utf8");
    assert_contains_server_trailer(&stderr_text);
}
