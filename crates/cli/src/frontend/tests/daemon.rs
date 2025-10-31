use super::common::*;
use super::*;

#[test]
fn daemon_flag_delegates_to_daemon_help() {
    let mut expected_stdout = Vec::new();
    let mut expected_stderr = Vec::new();
    let expected_code = daemon_cli::run(
        [OsStr::new(RSYNCD), OsStr::new("--help")],
        &mut expected_stdout,
        &mut expected_stderr,
    );

    assert_eq!(expected_code, 0);
    assert!(expected_stderr.is_empty());

    let (code, stdout, stderr) = run_with_args([
        OsStr::new(RSYNC),
        OsStr::new("--daemon"),
        OsStr::new("--help"),
    ]);

    assert_eq!(code, expected_code);
    assert_eq!(stdout, expected_stdout);
    assert_eq!(stderr, expected_stderr);
}

#[test]
fn daemon_flag_delegates_to_daemon_version() {
    let mut expected_stdout = Vec::new();
    let mut expected_stderr = Vec::new();
    let expected_code = daemon_cli::run(
        [OsStr::new(RSYNCD), OsStr::new("--version")],
        &mut expected_stdout,
        &mut expected_stderr,
    );

    assert_eq!(expected_code, 0);
    assert!(expected_stderr.is_empty());

    let (code, stdout, stderr) = run_with_args([
        OsStr::new(RSYNC),
        OsStr::new("--daemon"),
        OsStr::new("--version"),
    ]);

    assert_eq!(code, expected_code);
    assert_eq!(stdout, expected_stdout);
    assert_eq!(stderr, expected_stderr);
}

#[test]
fn oc_daemon_flag_delegates_to_oc_daemon_version() {
    let mut expected_stdout = Vec::new();
    let mut expected_stderr = Vec::new();
    let expected_code = daemon_cli::run(
        [OsStr::new(OC_RSYNC_D), OsStr::new("--version")],
        &mut expected_stdout,
        &mut expected_stderr,
    );

    assert_eq!(expected_code, 0);
    assert!(expected_stderr.is_empty());

    let (code, stdout, stderr) = run_with_args([
        OsStr::new(OC_RSYNC),
        OsStr::new("--daemon"),
        OsStr::new("--version"),
    ]);

    assert_eq!(code, expected_code);
    assert_eq!(stdout, expected_stdout);
    assert_eq!(stderr, expected_stderr);
}

#[test]
fn daemon_mode_arguments_ignore_operands_after_double_dash() {
    let args = vec![
        OsString::from(RSYNC),
        OsString::from("--"),
        OsString::from("--daemon"),
        OsString::from("dest"),
    ];

    assert!(server::daemon_mode_arguments(&args).is_none());
}
