#[test]
fn configured_fallback_binary_supports_auto_value() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("auto"));
    let _secondary = EnvGuard::remove(CLIENT_FALLBACK_ENV);
    assert_eq!(configured_fallback_binary(), Some(OsString::from("rsync")));
}

