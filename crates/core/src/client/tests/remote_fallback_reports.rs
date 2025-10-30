use super::prelude::*;


#[test]
fn remote_fallback_reports_launch_errors() {
    let _lock = env_lock().lock().expect("env mutex poisoned");
    let temp = tempdir().expect("tempdir created");
    let missing = temp.path().join("missing-rsync");

    let mut stdout = Vec::new();
    let mut stderr = Vec::new();

    let mut args = baseline_fallback_args();
    args.fallback_binary = Some(missing.into_os_string());

    let error = run_remote_transfer_fallback(&mut stdout, &mut stderr, args)
        .expect_err("spawn failure reported");

    assert_eq!(error.exit_code(), 1);
    let message = format!("{error}");
    assert!(message.contains("failed to launch fallback rsync binary"));
}


#[cfg(unix)]
#[test]
fn remote_fallback_reports_stdout_forward_errors() {
    let _lock = env_lock().lock().expect("env mutex poisoned");
    let temp = tempdir().expect("tempdir created");
    let script = write_fallback_script(temp.path());

    let mut stdout = FailingWriter;
    let mut stderr = Vec::new();

    let mut args = baseline_fallback_args();
    args.fallback_binary = Some(script.into_os_string());

    let error = run_remote_transfer_fallback(&mut stdout, &mut stderr, args)
        .expect_err("stdout forwarding failure surfaces");

    assert_eq!(error.exit_code(), 1);
    let message = format!("{error}");
    assert!(message.contains("failed to forward fallback stdout"));
}


#[test]
fn remote_fallback_reports_disabled_override() {
    let _lock = env_lock().lock().expect("env mutex poisoned");
    let _guard = EnvGuard::set_os(CLIENT_FALLBACK_ENV, OsStr::new("no"));
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();

    let args = baseline_fallback_args();
    let error = run_remote_transfer_fallback(&mut stdout, &mut stderr, args)
        .expect_err("disabled override prevents fallback execution");

    assert_eq!(error.exit_code(), 1);
    let message = format!("{error}");
    assert!(message.contains(&format!(
        "remote transfers are unavailable because {env} is disabled",
        env = CLIENT_FALLBACK_ENV,
    )));
}

