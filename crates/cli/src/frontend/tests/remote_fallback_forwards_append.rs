use super::common::*;
use super::*;

#[cfg(unix)]
#[test]
fn remote_fallback_forwards_append_flags() {
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

    let dest_path = temp.path().join("dest");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--append"),
        OsString::from("remote::module"),
        dest_path.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let recorded = std::fs::read_to_string(&args_path).expect("read args file");
    let args: Vec<&str> = recorded.lines().collect();
    assert!(args.contains(&"--append"));
    assert!(!args.contains(&"--append-verify"));
    assert!(!args.contains(&"--no-append"));

    std::fs::write(&args_path, b"").expect("truncate args file");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--no-append"),
        OsString::from("remote::module"),
        dest_path.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let recorded = std::fs::read_to_string(&args_path).expect("read args file");
    let args: Vec<&str> = recorded.lines().collect();
    assert!(args.contains(&"--no-append"));
    assert!(!args.contains(&"--append"));
    assert!(!args.contains(&"--append-verify"));

    std::fs::write(&args_path, b"").expect("truncate args file");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--append-verify"),
        OsString::from("remote::module"),
        dest_path.into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let recorded = std::fs::read_to_string(&args_path).expect("read args file");
    let args: Vec<&str> = recorded.lines().collect();
    assert!(args.contains(&"--append-verify"));
    assert!(!args.contains(&"--append"));
    assert!(!args.contains(&"--no-append"));
}
