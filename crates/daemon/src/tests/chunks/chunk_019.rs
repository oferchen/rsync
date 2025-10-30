#[test]
fn configured_fallback_binary_respects_primary_disable() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::remove(CLIENT_FALLBACK_ENV);
    assert!(configured_fallback_binary().is_none());
}

