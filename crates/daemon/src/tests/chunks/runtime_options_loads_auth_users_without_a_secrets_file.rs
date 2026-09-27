/// A module that requires authentication but has no secrets file still loads.
///
/// upstream: authenticate.c:143-146 check_secret() - every login to it fails
/// with "no secrets file", but the config itself is valid.
#[test]
fn runtime_options_loads_auth_users_without_a_secrets_file() {
    let mut file = NamedTempFile::new().expect("config file");
    writeln!(file, "[secure]\npath = /srv/secure\nauth users = alice\n").expect("write config");

    let options = RuntimeOptions::parse(&[
        OsString::from("--config"),
        file.path().as_os_str().to_os_string(),
    ])
    .expect("auth users without a secrets file must load");

    let module = &options.modules()[0];
    assert!(module.secrets_file.is_none());
    assert_eq!(module.auth_users.len(), 1);
}
