use super::common::*;
use super::*;

#[cfg(unix)]
#[test]
fn remote_fallback_forwards_connection_options() {
    use tempfile::tempdir;

    let _env_lock = ENV_LOCK.lock().expect("env lock");
    let _rsh_guard = clear_rsync_rsh();
    let temp = tempdir().expect("tempdir");
    let script_path = temp.path().join("fallback.sh");
    let args_path = temp.path().join("args.txt");
    let password_path = temp.path().join("password.txt");
    std::fs::write(&password_path, b"secret\n").expect("write password");
    let mut permissions = std::fs::metadata(&password_path)
        .expect("password metadata")
        .permissions();
    permissions.set_mode(0o600);
    std::fs::set_permissions(&password_path, permissions).expect("set password perms");

    let script = r#"#!/bin/sh
printf "%s\n" "$@" > "$ARGS_FILE"
exit 0
"#;
    write_executable_script(&script_path, script);

    let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
    let _args_guard = EnvGuard::set("ARGS_FILE", args_path.as_os_str());

    let dest_path = temp.path().join("dest");
    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from(format!("--password-file={}", password_path.display())),
        OsString::from("--protocol=30"),
        OsString::from("--timeout=120"),
        OsString::from("--contimeout=75"),
        OsString::from("--sockopts=SO_SNDBUF=16384"),
        OsString::from("--blocking-io"),
        OsString::from("rsync://remote/module"),
        dest_path.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let password_display = password_path.display().to_string();
    let dest_display = dest_path.display().to_string();
    let recorded = std::fs::read_to_string(&args_path).expect("read args file");
    assert!(recorded.contains("--password-file"));
    assert!(recorded.contains(&password_display));
    assert!(recorded.contains("--protocol"));
    assert!(recorded.contains("30"));
    assert!(recorded.contains("--timeout"));
    assert!(recorded.contains("120"));
    assert!(recorded.contains("--contimeout"));
    assert!(recorded.contains("75"));
    assert!(recorded.contains("--sockopts=SO_SNDBUF=16384"));
    assert!(recorded.contains("--blocking-io"));
    assert!(recorded.contains("rsync://remote/module"));
    assert!(recorded.contains(&dest_display));
}

#[cfg(unix)]
#[test]
fn remote_fallback_forwards_no_blocking_io_flag() {
    use tempfile::tempdir;

    let _env_lock = ENV_LOCK.lock().expect("env lock");
    let _rsh_guard = clear_rsync_rsh();
    let temp = tempdir().expect("tempdir");
    let script_path = temp.path().join("fallback.sh");
    let args_path = temp.path().join("args.txt");
    std::fs::File::create(&args_path).expect("create args file");

    let script = r#"#!/bin/sh
printf "%s\n" "$@" > "$ARGS_FILE"
exit 0
"#;
    write_executable_script(&script_path, script);

    let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
    let _args_guard = EnvGuard::set("ARGS_FILE", args_path.as_os_str());

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--no-blocking-io"),
        OsString::from("remote::module"),
        OsString::from("dest"),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let recorded = std::fs::read_to_string(&args_path).expect("read args file");
    assert!(recorded.contains("--no-blocking-io"));
    assert!(!recorded.contains("--blocking-io"));
}
