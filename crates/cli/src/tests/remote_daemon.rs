use super::common::*;
use super::*;

#[test]
fn remote_daemon_listing_prints_modules() {
    let (addr, handle) = spawn_stub_daemon(vec![
        "@RSYNCD: MOTD Welcome to the test daemon\n",
        "@RSYNCD: OK\n",
        "first\tFirst module\n",
        "second\n",
        "@RSYNCD: EXIT\n",
    ]);

    let url = format!("rsync://{}:{}/", addr.ip(), addr.port());
    let (code, stdout, stderr) = run_with_args([OsString::from(RSYNC), OsString::from(url)]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());

    let rendered = String::from_utf8(stdout).expect("output is UTF-8");
    assert!(rendered.contains("Welcome to the test daemon"));
    assert!(rendered.contains("first\tFirst module"));
    assert!(rendered.contains("second"));

    handle.join().expect("server thread");
}

#[test]
fn remote_daemon_listing_suppresses_motd_with_flag() {
    let (addr, handle) = spawn_stub_daemon(vec![
        "@RSYNCD: MOTD Welcome to the test daemon\n",
        "@RSYNCD: OK\n",
        "module\n",
        "@RSYNCD: EXIT\n",
    ]);

    let url = format!("rsync://{}:{}/", addr.ip(), addr.port());
    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--no-motd"),
        OsString::from(url),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());

    let rendered = String::from_utf8(stdout).expect("output is UTF-8");
    assert!(!rendered.contains("Welcome to the test daemon"));
    assert!(rendered.contains("module"));

    handle.join().expect("server thread");
}

#[test]
fn remote_daemon_listing_with_rsync_path_does_not_spawn_fallback() {
    use tempfile::tempdir;

    let (addr, handle) = spawn_stub_daemon(vec!["@RSYNCD: OK\n", "module\n", "@RSYNCD: EXIT\n"]);

    let url = format!("rsync://{}:{}/", addr.ip(), addr.port());

    let _env_lock = ENV_LOCK.lock().expect("env lock");
    let temp = tempdir().expect("tempdir");
    let script_path = temp.path().join("fallback.sh");
    let marker_path = temp.path().join("marker.txt");

    let script = r#"#!/bin/sh
printf "%s\n" "invoked" > "$MARKER_FILE"
exit 99
"#;
    write_executable_script(&script_path, script);

    let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
    let _marker_guard = EnvGuard::set("MARKER_FILE", marker_path.as_os_str());

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--rsync-path=/opt/custom/rsync"),
        OsString::from(url.clone()),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    let rendered = String::from_utf8(stdout).expect("output is UTF-8");
    assert!(rendered.contains("module"));
    assert!(!marker_path.exists());

    handle.join().expect("server thread");
}

#[test]
fn remote_daemon_listing_respects_protocol_cap() {
    let (addr, handle) = spawn_stub_daemon_with_protocol(
        vec!["@RSYNCD: OK\n", "module\n", "@RSYNCD: EXIT\n"],
        "29.0",
    );

    let url = format!("rsync://{}:{}/", addr.ip(), addr.port());
    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--protocol=29"),
        OsString::from(url),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());

    let rendered = String::from_utf8(stdout).expect("output is UTF-8");
    assert!(rendered.contains("module"));

    handle.join().expect("server thread");
}

#[test]
fn remote_daemon_listing_renders_warnings() {
    let (addr, handle) = spawn_stub_daemon(vec![
        "@WARNING: Maintenance\n",
        "@RSYNCD: OK\n",
        "module\n",
        "@RSYNCD: EXIT\n",
    ]);

    let url = format!("rsync://{}:{}/", addr.ip(), addr.port());
    let (code, stdout, stderr) = run_with_args([OsString::from(RSYNC), OsString::from(url)]);

    assert_eq!(code, 0);
    assert!(
        String::from_utf8(stdout)
            .expect("modules")
            .contains("module")
    );

    let rendered_err = String::from_utf8(stderr).expect("warnings are UTF-8");
    assert!(rendered_err.contains("@WARNING: Maintenance"));

    handle.join().expect("server thread");
}

#[test]
fn remote_daemon_error_is_reported() {
    let (addr, handle) = spawn_stub_daemon(vec!["@ERROR: unavailable\n", "@RSYNCD: EXIT\n"]);

    let url = format!("rsync://{}:{}/", addr.ip(), addr.port());
    let (code, stdout, stderr) = run_with_args([OsString::from(RSYNC), OsString::from(url)]);

    assert_eq!(code, 23);
    assert!(stdout.is_empty());

    let rendered = String::from_utf8(stderr).expect("diagnostic is valid UTF-8");
    assert!(rendered.contains("unavailable"));

    handle.join().expect("server thread");
}
