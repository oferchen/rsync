#[test]
fn configured_fallback_binary_respects_secondary_disable() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::remove(DAEMON_FALLBACK_ENV);
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("no"));
    assert!(configured_fallback_binary().is_none());
}

