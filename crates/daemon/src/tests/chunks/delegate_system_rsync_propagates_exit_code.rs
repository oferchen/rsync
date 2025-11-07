#[cfg(unix)]
#[test]
fn delegate_system_rsync_propagates_exit_code() {
    let _lock = ENV_LOCK.lock().unwrap();
    let temp = tempdir().expect("tempdir");
    let script_path = temp.path().join("rsync-wrapper.sh");
    write_executable_script(&script_path, "#!/bin/sh\nexit 7\n");
    let _guard = EnvGuard::set(DAEMON_FALLBACK_ENV, script_path.as_os_str());

    let (code, _stdout, stderr) =
        run_with_args([OsStr::new(RSYNCD), OsStr::new("--delegate-system-rsync")]);

    assert_eq!(code, 7);
    let stderr_str = String::from_utf8_lossy(&stderr);
    assert!(stderr_str.contains("system rsync daemon exited"));
}

