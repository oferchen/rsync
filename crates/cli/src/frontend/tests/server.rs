use super::common::*;
use super::*;

#[cfg(unix)]
#[test]
fn server_mode_invokes_fallback_binary() {
    use std::fs;
    use std::io;
    use std::os::unix::fs::PermissionsExt;
    use tempfile::tempdir;

    let _env_lock = ENV_LOCK.lock().expect("env lock");

    let temp = tempdir().expect("tempdir");
    let script_path = temp.path().join("server.sh");
    let marker_path = temp.path().join("marker.txt");

    fs::write(
        &script_path,
        r#"#!/bin/sh
set -eu
: "${SERVER_MARKER:?}"
printf 'invoked' > "$SERVER_MARKER"
exit 37
"#,
    )
    .expect("write script");

    let mut perms = fs::metadata(&script_path)
        .expect("script metadata")
        .permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&script_path, perms).expect("set script perms");

    let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
    let _marker_guard = EnvGuard::set("SERVER_MARKER", marker_path.as_os_str());

    let mut stdout = io::sink();
    let mut stderr = io::sink();
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

    assert_eq!(exit_code, 37);
    assert_eq!(fs::read(&marker_path).expect("read marker"), b"invoked");
}

#[test]
fn server_invocation_parses_flag_string_and_placeholder() {
    let invocation = ServerInvocation::parse(&[
        OsString::from(RSYNC),
        OsString::from("--server"),
        OsString::from("--sender"),
        OsString::from("-logDtpre.iLsfxC"),
        OsString::from("."),
        OsString::from("src"),
        OsString::from("dest"),
    ])
    .expect("parse invocation");

    assert_eq!(invocation.role, RoleKind::Generator);
    assert_eq!(invocation.raw_flag_string, "-logDtpre.iLsfxC");
    assert_eq!(
        invocation.args,
        vec![OsString::from("src"), OsString::from("dest")]
    );
}

#[test]
fn server_invocation_rejects_missing_flag_string() {
    let err = ServerInvocation::parse(&[OsString::from(RSYNC), OsString::from("--server")])
        .expect_err("expected missing flag string error");

    assert!(err.contains("missing rsync server flag string"));
}

#[cfg(unix)]
#[test]
fn server_mode_forwards_output_to_provided_handles() {
    use tempfile::tempdir;

    let _env_lock = ENV_LOCK.lock().expect("env lock");

    let temp = tempdir().expect("tempdir");
    let script_path = temp.path().join("server_output.sh");

    let script = r#"#!/bin/sh
set -eu
printf 'fallback stdout line\n'
printf 'fallback stderr line\n' >&2
exit 0
"#;
    write_executable_script(&script_path, script);

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

    assert_eq!(exit_code, 0);
    assert!(stdout.ends_with(b"fallback stdout line\n"));
    assert!(stderr.ends_with(b"fallback stderr line\n"));
}

#[cfg(unix)]
#[test]
fn server_mode_reports_disabled_fallback_override() {
    use std::io;

    let _env_lock = ENV_LOCK.lock().expect("env lock");
    let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("no"));

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
    let stderr_text = String::from_utf8(stderr).expect("stderr utf8");
    assert!(stderr_text.contains(&format!(
        "remote server mode is unavailable because {CLIENT_FALLBACK_ENV} is disabled"
    )));
}

#[test]
fn server_mode_reports_missing_fallback_binary() {
    use std::io;
    use tempfile::tempdir;

    let _env_lock = ENV_LOCK.lock().expect("env lock");

    let temp = tempdir().expect("tempdir");
    let missing_path = temp.path().join("server-missing-fallback");
    let missing_display = missing_path.display().to_string();
    let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, missing_path.as_os_str());

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
    let stderr_text = String::from_utf8(stderr).expect("stderr utf8");
    assert!(stderr_text.contains("fallback rsync binary"));
    assert!(stderr_text.contains(&missing_display));
    assert_contains_server_trailer(&stderr_text);
}

#[test]
fn server_mode_rejects_recursive_fallback() {
    use std::env;
    use std::io;

    let _env_lock = ENV_LOCK.lock().expect("env lock");
    let current = env::current_exe().expect("current exe");
    let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, current.as_os_str());

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
    let stderr_text = String::from_utf8(stderr).expect("stderr utf8");
    assert!(stderr_text.contains("resolves to this oc-rsync executable"));
    assert_contains_server_trailer(&stderr_text);
}
