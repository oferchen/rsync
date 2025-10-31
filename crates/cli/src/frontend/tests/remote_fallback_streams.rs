use super::common::*;
use super::*;

#[cfg(unix)]
#[test]
fn remote_fallback_streams_process_output() {
    use tempfile::tempdir;

    let _env_lock = ENV_LOCK.lock().expect("env lock");
    let _rsh_guard = clear_rsync_rsh();
    let temp = tempdir().expect("tempdir");
    let script_path = temp.path().join("fallback.sh");

    let script = r#"#!/bin/sh
echo "fallback stdout"
echo "fallback stderr" 1>&2
exit 0
"#;
    write_executable_script(&script_path, script);

    let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("remote::module"),
        OsString::from("dest"),
    ]);

    assert_eq!(code, 0);
    assert_eq!(stdout, b"fallback stdout\n");
    assert_eq!(stderr, b"fallback stderr\n");
}
