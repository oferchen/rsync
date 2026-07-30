// QUIC listener identity is a certificate/key pair (docs/design/quic-transport-
// policy.md, decision A): the listener needs both files to present an identity,
// so naming only one is an incomplete configuration. These tests pin that the
// merged daemon global config rejects a half-configured pair and accepts a
// complete one. The paths need not exist - like `pid file` / `lock file`, the
// directives only resolve and store a path; loading happens at listener build.

#[test]
fn runtime_options_quic_cert_without_key_is_config_error() {
    let dir = tempdir().expect("config dir");
    let config_path = dir.path().join("rsyncd.conf");
    fs::write(&config_path, "quic cert file = quic/server.pem\n").expect("write config");

    let error = RuntimeOptions::parse(&[
        OsString::from("--config"),
        config_path.as_os_str().to_os_string(),
    ])
    .expect_err("cert without key is a config error");
    assert!(
        error.to_string().contains("quic key file"),
        "unexpected error: {error}"
    );
}

#[test]
fn runtime_options_quic_key_without_cert_is_config_error() {
    let dir = tempdir().expect("config dir");
    let config_path = dir.path().join("rsyncd.conf");
    fs::write(&config_path, "quic key file = quic/server.key\n").expect("write config");

    let error = RuntimeOptions::parse(&[
        OsString::from("--config"),
        config_path.as_os_str().to_os_string(),
    ])
    .expect_err("key without cert is a config error");
    assert!(
        error.to_string().contains("quic cert file"),
        "unexpected error: {error}"
    );
}

#[test]
fn runtime_options_quic_cert_and_key_pair_loads() {
    let dir = tempdir().expect("config dir");
    let config_path = dir.path().join("rsyncd.conf");
    fs::write(
        &config_path,
        "quic cert file = quic/server.pem\nquic key file = quic/server.key\n",
    )
    .expect("write config");

    RuntimeOptions::parse(&[
        OsString::from("--config"),
        config_path.as_os_str().to_os_string(),
    ])
    .expect("a complete cert/key pair loads");
}
