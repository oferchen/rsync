use super::common::*;
use super::*;

// Disabled per native-only execution requirement (CLAUDE.md):
// #[test]
// fn remote_operand_reports_launch_failure_when_fallback_missing() {
//     let _env_lock = ENV_LOCK.lock().expect("env lock");
//     let _rsh_guard = clear_rsync_rsh();
//     let missing = OsString::from("rsync-missing-binary");
//     let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, missing.as_os_str());
//
//     let (code, stdout, stderr) = run_with_args([
//         OsString::from(RSYNC),
//         OsString::from("remote::module"),
//         OsString::from("dest"),
//     ]);
//
//     assert_eq!(code, 1);
//     assert!(stdout.is_empty());
//
//     let rendered = String::from_utf8(stderr).expect("diagnostic is valid UTF-8");
//     assert!(
//         rendered.contains("fallback rsync binary 'rsync-missing-binary' is not available on PATH"),
//         "expected fallback availability diagnostic, got {rendered:?}"
//     );
//     assert!(
//         rendered.contains("set OC_RSYNC_FALLBACK to an explicit path"),
//         "expected fallback guidance in diagnostic, got {rendered:?}"
//     );
//     assert_contains_client_trailer(&rendered);
// }

// Disabled per native-only execution requirement (CLAUDE.md):
// #[test]
// fn remote_operand_reports_disabled_fallback_override() {
//     let _env_lock = ENV_LOCK.lock().expect("env lock");
//     let _rsh_guard = clear_rsync_rsh();
//     let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("no"));
//
//     let (code, stdout, stderr) = run_with_args([
//         OsString::from(RSYNC),
//         OsString::from("remote::module"),
//         OsString::from("dest"),
//     ]);
//
//     assert_eq!(code, 1);
//     assert!(stdout.is_empty());
//
//     let rendered = String::from_utf8(stderr).expect("diagnostic is valid UTF-8");
//     assert!(
//         rendered.contains("remote transfers are unavailable because OC_RSYNC_FALLBACK is disabled")
//     );
//     assert_contains_client_trailer(&rendered);
// }

// Disabled per native-only execution requirement (CLAUDE.md):
// #[test]
// fn remote_operand_rejects_recursive_fallback() {
//     let _env_lock = ENV_LOCK.lock().expect("env lock");
//     let _rsh_guard = clear_rsync_rsh();
//     let current = std::env::current_exe().expect("current exe");
//     let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, current.as_os_str());
//
//     let (code, stdout, stderr) = run_with_args([
//         OsString::from(RSYNC),
//         OsString::from("remote::module"),
//         OsString::from("dest"),
//     ]);
//
//     assert_eq!(code, 1);
//     assert!(stdout.is_empty());
//
//     let rendered = String::from_utf8(stderr).expect("diagnostic is valid UTF-8");
//     assert!(rendered.contains("oc-rsync executable"));
//     assert_contains_client_trailer(&rendered);
// }

// Disabled per native-only execution requirement (CLAUDE.md):
// #[cfg(unix)]
// #[test]
// fn remote_operands_invoke_fallback_binary() {
//     use tempfile::tempdir;
//
//     let _env_lock = ENV_LOCK.lock().expect("env lock");
//     let _rsh_guard = clear_rsync_rsh();
//     let temp = tempdir().expect("tempdir");
//     let script_path = temp.path().join("fallback.sh");
//     let args_path = temp.path().join("args.txt");
//     std::fs::File::create(&args_path).expect("create args file");
//
//     let script = r#"#!/bin/sh
// printf "%s\n" "$@" > "$ARGS_FILE"
// echo fallback-stdout
// echo fallback-stderr >&2
// exit 7
// "#;
//     write_executable_script(&script_path, script);
//
//     let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
//     let _args_guard = EnvGuard::set("ARGS_FILE", args_path.as_os_str());
//
//     let (code, stdout, stderr) = run_with_args([
//         OsString::from(RSYNC),
//         OsString::from("--dry-run"),
//         OsString::from("remote::module/path"),
//         OsString::from("dest"),
//     ]);
//
//     assert_eq!(code, 7);
//     assert_eq!(
//         String::from_utf8(stdout).expect("stdout UTF-8"),
//         "fallback-stdout\n"
//     );
//     assert_eq!(
//         String::from_utf8(stderr).expect("stderr UTF-8"),
//         "fallback-stderr\n"
//     );
//
//     let recorded = std::fs::read_to_string(&args_path).expect("read args file");
//     assert!(recorded.contains("--dry-run"));
//     assert!(recorded.contains("remote::module/path"));
//     assert!(recorded.contains("dest"));
// }

#[test]
fn remote_option_requires_remote_operands() {
    use tempfile::tempdir;

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let dest = temp.path().join("dest.txt");
    std::fs::write(&source, b"content").expect("write source");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--remote-option=--log-file=/tmp/rsync.log"),
        source.into_os_string(),
        dest.clone().into_os_string(),
    ]);

    assert_eq!(code, 1);
    assert!(stdout.is_empty());
    let message = String::from_utf8(stderr).expect("stderr utf8");
    assert!(
        message.contains("the --remote-option option may only be used with remote connections")
    );
    assert!(!dest.exists());
}
