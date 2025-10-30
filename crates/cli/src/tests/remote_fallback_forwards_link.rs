use super::common::*;
use super::*;

#[cfg(unix)]
#[test]
fn remote_fallback_forwards_link_dest_arguments() {
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
        OsString::from("--link-dest"),
        OsString::from("baseline"),
        OsString::from("--link-dest"),
        OsString::from("/var/cache"),
        OsString::from("--link-dest=link"),
        OsString::from("remote::module"),
        dest_path.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let recorded = std::fs::read_to_string(&args_path).expect("read args file");
    assert!(
        recorded
            .lines()
            .filter(|line| *line == "--link-dest")
            .count()
            >= 2
    );
    assert!(recorded.lines().any(|line| line == "baseline"));
    assert!(recorded.lines().any(|line| line == "/var/cache"));
    assert!(recorded.lines().any(|line| line == "--link-dest=link"));
    assert!(recorded.lines().any(|line| line == "--link-dest"));
    assert!(recorded.lines().any(|line| line == "link"));
    assert!(
        recorded
            .lines()
            .any(|line| line == dest_path.display().to_string())
    );
}
