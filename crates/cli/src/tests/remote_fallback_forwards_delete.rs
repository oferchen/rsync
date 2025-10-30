use super::common::*;
use super::*;

#[cfg(unix)]
#[test]
fn remote_fallback_forwards_delete_after_flag() {
    use tempfile::tempdir;

    let _env_lock = ENV_LOCK.lock().expect("env lock");
    let _rsh_guard = clear_rsync_rsh();
    let temp = tempdir().expect("tempdir");
    let script_path = temp.path().join("fallback.sh");
    let args_path = temp.path().join("args.txt");

    let script = r#"#!/bin/sh
printf "%s\n" "$@" > "$ARGS_FILE"
exit 0
"#;
    write_executable_script(&script_path, script);

    let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
    let _args_guard = EnvGuard::set("ARGS_FILE", args_path.as_os_str());

    let destination = temp.path().join("dest");
    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--delete-after"),
        OsString::from("remote::module"),
        destination.into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let recorded = std::fs::read_to_string(&args_path).expect("read args file");
    let args: Vec<&str> = recorded.lines().collect();
    assert!(args.contains(&"--delete"));
    assert!(args.contains(&"--delete-after"));
    assert!(!args.contains(&"--delete-delay"));
}

#[cfg(unix)]
#[test]
fn remote_fallback_forwards_delete_before_flag() {
    use tempfile::tempdir;

    let _env_lock = ENV_LOCK.lock().expect("env lock");
    let _rsh_guard = clear_rsync_rsh();
    let temp = tempdir().expect("tempdir");
    let script_path = temp.path().join("fallback.sh");
    let args_path = temp.path().join("args.txt");

    let script = r#"#!/bin/sh
printf "%s\n" "$@" > "$ARGS_FILE"
exit 0
"#;
    write_executable_script(&script_path, script);

    let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
    let _args_guard = EnvGuard::set("ARGS_FILE", args_path.as_os_str());

    let destination = temp.path().join("dest");
    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--delete-before"),
        OsString::from("remote::module"),
        destination.into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let recorded = std::fs::read_to_string(&args_path).expect("read args file");
    let args: Vec<&str> = recorded.lines().collect();
    assert!(args.contains(&"--delete"));
    assert!(args.contains(&"--delete-before"));
    assert!(!args.contains(&"--delete-after"));
}

#[cfg(unix)]
#[test]
fn remote_fallback_forwards_delete_during_flag() {
    use tempfile::tempdir;

    let _env_lock = ENV_LOCK.lock().expect("env lock");
    let _rsh_guard = clear_rsync_rsh();
    let temp = tempdir().expect("tempdir");
    let script_path = temp.path().join("fallback.sh");
    let args_path = temp.path().join("args.txt");

    let script = r#"#!/bin/sh
printf "%s\n" "$@" > "$ARGS_FILE"
exit 0
"#;
    write_executable_script(&script_path, script);

    let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
    let _args_guard = EnvGuard::set("ARGS_FILE", args_path.as_os_str());

    let destination = temp.path().join("dest");
    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--delete-during"),
        OsString::from("remote::module"),
        destination.into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let recorded = std::fs::read_to_string(&args_path).expect("read args file");
    let args: Vec<&str> = recorded.lines().collect();
    assert!(args.contains(&"--delete"));
    assert!(args.contains(&"--delete-during"));
    assert!(!args.contains(&"--delete-delay"));
    assert!(!args.contains(&"--delete-before"));
    assert!(!args.contains(&"--delete-after"));
}

#[cfg(unix)]
#[test]
fn remote_fallback_forwards_delete_delay_flag() {
    use tempfile::tempdir;

    let _env_lock = ENV_LOCK.lock().expect("env lock");
    let _rsh_guard = clear_rsync_rsh();
    let temp = tempdir().expect("tempdir");
    let script_path = temp.path().join("fallback.sh");
    let args_path = temp.path().join("args.txt");

    let script = r#"#!/bin/sh
printf "%s\n" "$@" > "$ARGS_FILE"
exit 0
"#;
    write_executable_script(&script_path, script);

    let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
    let _args_guard = EnvGuard::set("ARGS_FILE", args_path.as_os_str());

    let destination = temp.path().join("dest");
    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--delete-delay"),
        OsString::from("remote::module"),
        destination.into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let recorded = std::fs::read_to_string(&args_path).expect("read args file");
    let args: Vec<&str> = recorded.lines().collect();
    assert!(args.contains(&"--delete"));
    assert!(args.contains(&"--delete-delay"));
    assert!(!args.contains(&"--delete-before"));
    assert!(!args.contains(&"--delete-after"));
    assert!(!args.contains(&"--delete-during"));
}
