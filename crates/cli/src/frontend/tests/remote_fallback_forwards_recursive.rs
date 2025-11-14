use super::common::*;
use super::*;

#[cfg(unix)]
fn capture_remote_fallback_args(flags: &[OsString]) -> Vec<String> {
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
    let mut invocation = Vec::with_capacity(flags.len() + 3);
    invocation.push(OsString::from(RSYNC));
    invocation.extend_from_slice(flags);
    invocation.push(OsString::from("remote::module"));
    invocation.push(destination.into_os_string());

    let (code, stdout, stderr) = run_with_args(invocation);
    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let recorded = std::fs::read_to_string(&args_path).expect("read args");
    recorded.lines().map(|line| line.to_string()).collect()
}

#[cfg(unix)]
#[test]
fn remote_fallback_forwards_recursive_flag() {
    let args = capture_remote_fallback_args(&[OsString::from("-r")]);
    assert!(args.contains(&"--recursive".to_string()));
}

#[cfg(unix)]
#[test]
fn remote_fallback_forwards_no_recursive_flag() {
    let args = capture_remote_fallback_args(&[OsString::from("--no-recursive")]);
    assert!(args.contains(&"--no-recursive".to_string()));
    assert!(!args.contains(&"--recursive".to_string()));
}

#[cfg(unix)]
#[test]
fn remote_fallback_forwards_inc_recursive_flag() {
    let args = capture_remote_fallback_args(&[OsString::from("--inc-recursive")]);
    assert!(args.contains(&"--inc-recursive".to_string()));
    assert!(!args.contains(&"--no-inc-recursive".to_string()));
}

#[cfg(unix)]
#[test]
fn remote_fallback_forwards_no_inc_recursive_flag() {
    let args = capture_remote_fallback_args(&[OsString::from("--no-inc-recursive")]);
    assert!(args.contains(&"--no-inc-recursive".to_string()));
    assert!(!args.contains(&"--inc-recursive".to_string()));
}

#[cfg(unix)]
#[test]
fn remote_fallback_forwards_dirs_flag() {
    let args = capture_remote_fallback_args(&[OsString::from("--dirs")]);
    assert!(args.contains(&"--dirs".to_string()));
    assert!(!args.contains(&"--no-dirs".to_string()));
}

#[cfg(unix)]
#[test]
fn remote_fallback_forwards_no_dirs_flag() {
    let args = capture_remote_fallback_args(&[OsString::from("--no-dirs")]);
    assert!(args.contains(&"--no-dirs".to_string()));
    assert!(!args.contains(&"--dirs".to_string()));
}
