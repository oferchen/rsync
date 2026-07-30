// Persisted self-signed QUIC identity (docs/design/quic-transport-policy.md,
// decision A). These tests pin WHY the identity is persisted rather than
// regenerated: a trust-on-first-use client re-pins on any key change, so a
// restart that rotates the key looks like a MITM. The daemon must therefore
// generate the pair once and reuse the exact same files thereafter, and must
// honour operator-supplied directive paths verbatim without generating anything.

#[test]
fn persist_self_signed_quic_identity_generates_pair_on_first_call() {
    let dir = tempdir().expect("state dir");
    let state_dir = dir.path().join("quic");

    let (cert, key) =
        crate::daemon::persist_self_signed_quic_identity(&state_dir).expect("first call generates");

    assert_eq!(cert, state_dir.join("self-signed.pem"));
    assert_eq!(key, state_dir.join("self-signed.key"));
    assert!(cert.is_file(), "certificate must be written");
    assert!(key.is_file(), "private key must be written");
    assert!(
        fs::read_to_string(&cert)
            .expect("read cert")
            .contains("BEGIN CERTIFICATE"),
        "certificate must be PEM"
    );
    assert!(
        fs::read_to_string(&key)
            .expect("read key")
            .contains("PRIVATE KEY"),
        "key must be PEM"
    );
}

#[test]
fn persist_self_signed_quic_identity_reuses_existing_pair() {
    let dir = tempdir().expect("state dir");
    let state_dir = dir.path().join("quic");

    let (cert1, key1) =
        crate::daemon::persist_self_signed_quic_identity(&state_dir).expect("first call");
    let cert_bytes = fs::read(&cert1).expect("read cert");
    let key_bytes = fs::read(&key1).expect("read key");

    let (cert2, key2) =
        crate::daemon::persist_self_signed_quic_identity(&state_dir).expect("second call");

    // Stable identity: same paths and byte-identical contents, not regenerated.
    assert_eq!(cert1, cert2);
    assert_eq!(key1, key2);
    assert_eq!(fs::read(&cert2).expect("reread cert"), cert_bytes);
    assert_eq!(fs::read(&key2).expect("reread key"), key_bytes);
}

#[test]
fn resolve_quic_identity_persists_default_under_config_dir() {
    let dir = tempdir().expect("config dir");
    let config_path = dir.path().join("rsyncd.conf");
    fs::write(&config_path, "# no quic directives\n").expect("write config");

    let options = RuntimeOptions::parse(&[
        OsString::from("--config"),
        config_path.as_os_str().to_os_string(),
    ])
    .expect("parse config");

    let (cert1, key1) = options.resolve_quic_identity().expect("first resolve");
    assert_eq!(cert1, dir.path().join("quic").join("self-signed.pem"));
    assert_eq!(key1, dir.path().join("quic").join("self-signed.key"));
    assert!(cert1.is_file() && key1.is_file());
    let cert_bytes = fs::read(&cert1).expect("read cert");

    let (cert2, key2) = options.resolve_quic_identity().expect("second resolve");
    assert_eq!(cert1, cert2);
    assert_eq!(key1, key2);
    assert_eq!(
        fs::read(&cert2).expect("reread cert"),
        cert_bytes,
        "identity must be stable across resolves"
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

    let (cert, key) = options.resolve_quic_identity().expect("resolve");
    // Directive paths resolve relative to the config directory and are returned
    // verbatim - no generation, no state subdirectory.
    assert_eq!(cert, dir.path().join("server.pem"));
    assert_eq!(key, dir.path().join("server.key"));
    assert!(
        !dir.path().join("quic").exists(),
        "operator identity must not trigger the persisted default"
    );
}
