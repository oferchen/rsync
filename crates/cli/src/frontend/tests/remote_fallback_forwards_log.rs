use super::common::*;
use super::*;

#[cfg(unix)]
#[test]
fn remote_fallback_forwards_log_file_arguments() {
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

    let log_path = temp.path().join("remote.log");
    let destination = temp.path().join("dest");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--log-file"),
        log_path.clone().into_os_string(),
        OsString::from("--log-file-format=%f %l"),
        OsString::from("remote::module"),
        destination.into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let recorded = std::fs::read_to_string(&args_path).expect("read args file");
    let args: Vec<&str> = recorded.lines().collect();
    let log_path_arg = log_path.to_string_lossy().to_string();
    assert!(
        args.windows(2)
            .any(|window| window == ["--log-file", log_path_arg.as_str()]),
        "args missing log file pair: {args:?}"
    );
    assert!(
        args.windows(2)
            .any(|window| window == ["--log-file-format", "%f %l"]),
        "args missing log format pair: {args:?}"
    );
}
