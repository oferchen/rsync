use super::common::*;
use super::*;

#[cfg(unix)]
#[test]
fn remote_fallback_forwards_no_compress_when_level_zero() {
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

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--compress-level=0"),
        OsString::from("remote::module"),
        OsString::from("dest"),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let recorded = std::fs::read_to_string(&args_path).expect("read args file");
    assert!(recorded.contains("--no-compress"));
    assert!(recorded.contains("--compress-level"));
    assert!(recorded.contains("0"));
}

#[cfg(unix)]
#[test]
fn remote_fallback_forwards_no_open_noatime_flag() {
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

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--no-open-noatime"),
        OsString::from("remote::module"),
        OsString::from("dest"),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let recorded = std::fs::read_to_string(&args_path).expect("read args file");
    let args: Vec<&str> = recorded.lines().collect();
    assert!(args.contains(&"--no-open-noatime"));
    assert!(!args.contains(&"--open-noatime"));
}

#[cfg(unix)]
#[test]
fn remote_fallback_forwards_no_bwlimit_argument() {
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
        OsString::from("--no-bwlimit"),
        OsString::from("remote::module"),
        OsString::from("dest"),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let recorded = std::fs::read_to_string(&args_path).expect("read args file");
    let mut lines = recorded.lines();
    while let Some(line) = lines.next() {
        if line == "--bwlimit" {
            let value = lines.next().expect("bwlimit value recorded");
            assert_eq!(value, "0");
            return;
        }
    }

    panic!("--bwlimit argument not forwarded to fallback");
}

#[cfg(unix)]
#[test]
fn remote_fallback_forwards_no_human_readable_flag() {
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
        OsString::from("--no-human-readable"),
        OsString::from("remote::module"),
        destination.into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let recorded = std::fs::read_to_string(&args_path).expect("read args file");
    let args: Vec<&str> = recorded.lines().collect();
    assert!(args.contains(&"--no-human-readable"));
    assert!(!args.contains(&"--human-readable"));
    assert!(!args.contains(&"--human-readable=2"));
}

#[cfg(unix)]
#[test]
fn remote_fallback_forwards_no_super_flag() {
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
        OsString::from("--no-super"),
        OsString::from("remote::module"),
        destination.into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let recorded = std::fs::read_to_string(&args_path).expect("read args file");
    let args: Vec<&str> = recorded.lines().collect();
    assert!(args.contains(&"--no-super"));
    assert!(!args.contains(&"--super"));
}

#[cfg(unix)]
#[test]
fn remote_fallback_forwards_no_protect_args_alias() {
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
        OsString::from("--no-secluded-args"),
        OsString::from("remote::module"),
        destination.into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let recorded = std::fs::read_to_string(&args_path).expect("read args file");
    let args: Vec<&str> = recorded.lines().collect();
    assert!(args.contains(&"--no-protect-args"));
    assert!(!args.contains(&"--protect-args"));
}

#[cfg(all(unix, feature = "acl"))]
#[test]
fn remote_fallback_forwards_no_acls_toggle() {
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
        OsString::from("--no-acls"),
        OsString::from("remote::module"),
        dest_path.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let recorded = std::fs::read_to_string(&args_path).expect("read args file");
    assert!(recorded.contains("--no-acls"));
}

#[test]
fn remote_fallback_forwards_no_omit_dir_times_toggle() {
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
        OsString::from("--no-omit-dir-times"),
        OsString::from("remote::module"),
        dest_path.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let recorded = std::fs::read_to_string(&args_path).expect("read args file");
    assert!(recorded.contains("--no-omit-dir-times"));
}

#[test]
fn remote_fallback_forwards_no_omit_link_times_toggle() {
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
        OsString::from("--no-omit-link-times"),
        OsString::from("remote::module"),
        dest_path.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let recorded = std::fs::read_to_string(&args_path).expect("read args file");
    assert!(recorded.contains("--no-omit-link-times"));
}
