#[test]
fn delegate_system_rsync_reports_missing_binary_diagnostic() {
    let _lock = ENV_LOCK.lock().unwrap();
    let temp = tempdir().expect("tempdir");
    let missing = temp.path().join("delegate-missing-rsync");
    let _fallback = EnvGuard::set(DAEMON_FALLBACK_ENV, missing.as_os_str());

    let (code, _stdout, stderr) = run_with_args([
        OsStr::new(RSYNCD),
        OsStr::new("--delegate-system-rsync"),
        OsStr::new("--config"),
        OsStr::new(branding::OC_DAEMON_CONFIG_PATH),
    ]);

    assert_eq!(code, 1);
    let rendered = String::from_utf8_lossy(&stderr);
    assert!(
        rendered.contains("fallback rsync binary"),
        "missing diagnostic: {rendered}"
    );
    assert!(rendered.contains("OC_RSYNC_DAEMON_FALLBACK"));
    assert!(rendered.contains("OC_RSYNC_FALLBACK"));
}
