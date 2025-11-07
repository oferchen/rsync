#[test]
fn configured_fallback_binary_defaults_to_rsync() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::remove(DAEMON_FALLBACK_ENV);
    let _secondary = EnvGuard::remove(CLIENT_FALLBACK_ENV);
    assert_eq!(configured_fallback_binary(), Some(OsString::from("rsync")));
}

