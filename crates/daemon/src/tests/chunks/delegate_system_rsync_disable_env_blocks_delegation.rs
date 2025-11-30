use core::fallback::DISABLE_FALLBACK_ENV;

#[cfg(unix)]
#[test]
fn delegate_system_rsync_disable_env_blocks_delegation() {
    let _lock = ENV_LOCK.lock().unwrap();
    let temp = tempdir().expect("tempdir");
    let script_path = temp.path().join("rsync-wrapper.sh");
    let log_path = temp.path().join("invocation.log");
    let script = format!("#!/bin/sh\necho invoked > {}\nexit 0\n", log_path.display());
    write_executable_script(&script_path, &script);
    let _fallback = EnvGuard::set(DAEMON_FALLBACK_ENV, script_path.as_os_str());
    let _disable = EnvGuard::set(DISABLE_FALLBACK_ENV, OsStr::new("1"));

    let (code, _stdout, stderr) =
        run_with_args([OsStr::new(RSYNCD), OsStr::new("--delegate-system-rsync")]);

    assert_eq!(code, 1);
    assert!(!log_path.exists());
    let stderr_text = String::from_utf8_lossy(&stderr);
    assert!(stderr_text.contains(DISABLE_FALLBACK_ENV));
}

