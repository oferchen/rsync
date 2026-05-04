#[test]
fn runtime_options_detach_parsed_default_matches_platform() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _config = EnvGuard::remove(BRANDED_CONFIG_ENV);
    let _config_legacy = EnvGuard::remove(LEGACY_CONFIG_ENV);

    // Parse with no detach-related flags to verify the parsed default
    // matches the struct default (platform-dependent).
    let options = RuntimeOptions::parse(&[
        OsString::from("--port"),
        OsString::from("8873"),
    ])
    .expect("parse without detach flags");

    let default_options = RuntimeOptions::default();
    assert_eq!(
        options.detach(),
        default_options.detach(),
        "parsed detach default should match struct default"
    );
}
