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
fn legacy_daemon_invocation_without_flag_delegates_to_daemon() {
    let mut expected_stdout = Vec::new();
    let mut expected_stderr = Vec::new();
    let expected_code = daemon_cli::run(
        [OsStr::new(RSYNCD), OsStr::new("--version")],
        &mut expected_stdout,
        &mut expected_stderr,
    );

    assert_eq!(expected_code, 0);
    assert!(expected_stderr.is_empty());

    let (code, stdout, stderr) = run_with_args([OsStr::new(RSYNCD), OsStr::new("--version")]);

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

#[test]
fn daemon_config_flag_is_passed_through() {
    let args = vec![
        OsString::from(RSYNC),
        OsString::from("--daemon"),
        OsString::from("--config=/tmp/test.conf"),
        OsString::from("--help"),
    ];

    let daemon_args = server::daemon_mode_arguments(&args).expect("daemon mode detected");
    assert!(
        daemon_args.iter().any(|a| a == "--config=/tmp/test.conf"),
        "expected --config flag to be forwarded, got: {daemon_args:?}"
    );
}

#[test]
fn daemon_config_flag_separate_value_is_passed_through() {
    let args = vec![
        OsString::from(RSYNC),
        OsString::from("--daemon"),
        OsString::from("--config"),
        OsString::from("/tmp/test.conf"),
        OsString::from("--help"),
    ];

    let daemon_args = server::daemon_mode_arguments(&args).expect("daemon mode detected");
    assert!(
        daemon_args.iter().any(|a| a == "--config"),
        "expected --config flag to be forwarded, got: {daemon_args:?}"
    );
    assert!(
        daemon_args.iter().any(|a| a == "/tmp/test.conf"),
        "expected config path to be forwarded, got: {daemon_args:?}"
    );
}
