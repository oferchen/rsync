// QUIC listener enablement (QUIC-6c). The daemon opens a UDP/QUIC listener only
// when QUIC is configured. Until the dedicated `quic = yes` enable directive
// lands, "configured" means any QUIC directive is present: a cert path, a key
// path, or an explicit `quic port`. A config with no QUIC directives leaves the
// listener off so a default `--all-features` daemon stays TCP-only.

#[test]
fn quic_listener_disabled_without_directives() {
    let dir = tempdir().expect("config dir");
    let config_path = dir.path().join("rsyncd.conf");
    fs::write(&config_path, "# no quic directives\n").expect("write config");

    let options = RuntimeOptions::parse(&[
        OsString::from("--config"),
        config_path.as_os_str().to_os_string(),
    ])
    .expect("parse config");

    assert!(
        !options.quic_listener_enabled(),
        "a daemon with no QUIC directives must not open a QUIC listener"
    );
}

#[test]
fn quic_listener_enabled_by_cert_key_directives() {
    let dir = tempdir().expect("config dir");
    let config_path = dir.path().join("rsyncd.conf");
    fs::write(
        &config_path,
        "quic cert file = server.pem\nquic key file = server.key\n",
    )
    .expect("write config");

    let options = RuntimeOptions::parse(&[
        OsString::from("--config"),
        config_path.as_os_str().to_os_string(),
    ])
    .expect("parse config");

    assert!(
        options.quic_listener_enabled(),
        "configured cert/key directives must enable the QUIC listener"
    );
}

#[test]
fn quic_listener_enabled_by_port_directive() {
    let dir = tempdir().expect("config dir");
    let config_path = dir.path().join("rsyncd.conf");
    fs::write(&config_path, "quic port = 8873\n").expect("write config");

    let options = RuntimeOptions::parse(&[
        OsString::from("--config"),
        config_path.as_os_str().to_os_string(),
    ])
    .expect("parse config");

    assert!(
        options.quic_listener_enabled(),
        "an explicit `quic port` must enable the QUIC listener"
    );
    assert_eq!(
        options.effective_quic_port(),
        8873,
        "the QUIC listener must bind the configured port"
    );
}
