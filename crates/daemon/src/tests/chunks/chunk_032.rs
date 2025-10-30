#[cfg(unix)]
#[test]
fn delegate_system_rsync_invokes_fallback_binary() {
    let _lock = ENV_LOCK.lock().unwrap();
    let temp = tempdir().expect("tempdir");
    let script_path = temp.path().join("rsync-wrapper.sh");
    let log_path = temp.path().join("invocation.log");
    let script = format!("#!/bin/sh\necho \"$@\" > {}\nexit 0\n", log_path.display());
    write_executable_script(&script_path, &script);
    let _guard = EnvGuard::set(DAEMON_FALLBACK_ENV, script_path.as_os_str());

    let (code, _stdout, stderr) = run_with_args([
        OsStr::new(RSYNCD),
        OsStr::new("--delegate-system-rsync"),
        OsStr::new("--config"),
        OsStr::new(branding::OC_DAEMON_CONFIG_PATH),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    let recorded = fs::read_to_string(&log_path).expect("read invocation log");
    assert!(recorded.contains("--daemon"));
    assert!(recorded.contains("--no-detach"));
    assert!(recorded.contains(&format!("--config {}", branding::OC_DAEMON_CONFIG_PATH)));
}

