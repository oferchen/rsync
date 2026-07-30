// QUIC listener identity resolution (docs/design/quic-transport-policy.md,
// decision A). These tests pin the two sources: operator-supplied cert/key files
// are honoured verbatim, and the zero-config default is EPHEMERAL - a fresh
// in-memory self-signed certificate generated at bind time, never written to
// disk. The ephemeral case must not create any state directory, so a restart
// deliberately rotates the identity (the documented trust-on-first-use tradeoff).

#[test]
fn resolve_quic_identity_without_directives_is_ephemeral() {
    let dir = tempdir().expect("config dir");
    let config_path = dir.path().join("rsyncd.conf");
    fs::write(&config_path, "# no quic directives\n").expect("write config");

    let options = RuntimeOptions::parse(&[
        OsString::from("--config"),
        config_path.as_os_str().to_os_string(),
    ])
    .expect("parse config");

    assert_eq!(options.resolve_quic_identity(), QuicIdentity::Ephemeral);
    // Ephemeral means nothing is persisted: no state subdirectory, no files.
    assert!(
        !dir.path().join("quic").exists(),
        "ephemeral identity must not create a state directory"
    );
}

#[test]
fn resolve_quic_identity_uses_directive_paths_verbatim() {
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

    // Directive paths resolve relative to the config directory and are returned
    // verbatim - no generation, no state subdirectory.
    assert_eq!(
        options.resolve_quic_identity(),
        QuicIdentity::Files {
            cert: dir.path().join("server.pem"),
            key: dir.path().join("server.key"),
        }
    );
    assert!(
        !dir.path().join("quic").exists(),
        "operator identity must not trigger any persisted default"
    );
}
