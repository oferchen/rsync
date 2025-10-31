use super::common::*;
use super::*;

#[test]
fn remote_fallback_cli_rsh_overrides_env() {
    use tempfile::tempdir;

    let _env_lock = ENV_LOCK.lock().expect("env lock");
    let _clear_guard = clear_rsync_rsh();
    let _env_guard = EnvGuard::set("RSYNC_RSH", OsStr::new("ssh -p 2200"));
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
        OsString::from("-e"),
        OsString::from("ssh -p 2222"),
        OsString::from("remote::module"),
        dest_path.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let recorded = std::fs::read_to_string(&args_path).expect("read args file");
    assert!(recorded.lines().any(|line| line == "ssh -p 2222"));
    assert!(!recorded.lines().any(|line| line == "ssh -p 2200"));
}
