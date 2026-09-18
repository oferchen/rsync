// QUIC listener identity resolution (docs/design/quic-transport-policy.md).
// These tests pin the two outcomes: operator-supplied cert/key files are
// honoured verbatim, and the zero-config default resolves to NONE - there is no
// ephemeral fallback (decision reversed 2026-09-18), so a QUIC request without
// a configured cert is a fatal misconfiguration the daemon rejects at startup
// rather than papering over with a generated identity.

#[test]
fn resolve_quic_identity_without_directives_is_none() {
    let dir = tempdir().expect("config dir");
    let config_path = dir.path().join("rsyncd.conf");
    fs::write(&config_path, "# no quic directives\n").expect("write config");

    let options = RuntimeOptions::parse(&[
        OsString::from("--config"),
        config_path.as_os_str().to_os_string(),
    ])
    .expect("parse config");

    assert!(
        options.resolve_quic_identity().is_none(),
        "no cert/key directives must resolve to no identity - no ephemeral fallback"
    );
    // Nothing is persisted: no state subdirectory, no files.
    assert!(
        !dir.path().join("quic").exists(),
        "the absence of a QUIC identity must not create a state directory"
    );
}

#[test]
fn quic_cert_without_key_is_rejected_at_parse() {
    // A partial configuration (cert without key) is not serviceable and must
    // NOT silently fall back to a generated identity. The cert/key pairing is
    // enforced at config-parse time, so the daemon fails loudly before it ever
    // reaches the listener - the earliest possible rejection.
    let dir = tempdir().expect("config dir");
    let config_path = dir.path().join("rsyncd.conf");
    fs::write(&config_path, "quic cert file = server.pem\n").expect("write config");

    let error = RuntimeOptions::parse(&[
        OsString::from("--config"),
        config_path.as_os_str().to_os_string(),
    ])
    .expect_err("a cert without a key must be rejected at parse time");
    let text = error.to_string();
    assert!(
        text.contains("quic key file"),
        "the parse error must name the missing `quic key file`, got: {text}"
    );
}

#[test]
fn quic_config_error_rejects_request_without_certificate() {
    // Requesting QUIC via `quic port` alone (no cert/key) is fatal: the startup
    // path surfaces this reason and refuses to start. Proves the fail-loud rule
    // fires, and that its message names the missing directives.
    let dir = tempdir().expect("config dir");
    let config_path = dir.path().join("rsyncd.conf");
    fs::write(&config_path, "quic port = 8730\n").expect("write config");

    let options = RuntimeOptions::parse(&[
        OsString::from("--config"),
        config_path.as_os_str().to_os_string(),
    ])
    .expect("parse config");

    let reason = options
        .quic_config_error()
        .expect("a QUIC request without a certificate must be rejected");
    assert!(
        reason.contains("quic cert file") && reason.contains("quic key file"),
        "the rejection must name the missing directives, got: {reason}"
    );
}

#[test]
fn quic_config_error_is_none_when_unconfigured_or_fully_configured() {
    // No QUIC directives: nothing to validate.
    let dir = tempdir().expect("config dir");
    let unconfigured = dir.path().join("none.conf");
    fs::write(&unconfigured, "# no quic directives\n").expect("write config");
    let options = RuntimeOptions::parse(&[
        OsString::from("--config"),
        unconfigured.as_os_str().to_os_string(),
    ])
    .expect("parse config");
    assert!(
        options.quic_config_error().is_none(),
        "an unconfigured daemon has no QUIC error"
    );

    // Both cert and key present: serviceable, no error.
    let configured = dir.path().join("full.conf");
    fs::write(
        &configured,
        "quic cert file = server.pem\nquic key file = server.key\n",
    )
    .expect("write config");
    let options = RuntimeOptions::parse(&[
        OsString::from("--config"),
        configured.as_os_str().to_os_string(),
    ])
    .expect("parse config");
    assert!(
        options.quic_config_error().is_none(),
        "a fully configured QUIC listener has no error"
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
        Some(QuicIdentity {
            cert: dir.path().join("server.pem"),
            key: dir.path().join("server.key"),
            client_ca: None,
        })
    );
    assert!(
        !dir.path().join("quic").exists(),
        "operator identity must not trigger any persisted default"
    );
}

#[test]
fn resolve_quic_identity_carries_client_ca_for_mutual_tls() {
    // With `quic client ca file` set alongside the cert/key, the resolved
    // identity carries the client-auth CA path (config-relative, verbatim) so
    // the listener requires and verifies a client certificate (mutual TLS).
    let dir = tempdir().expect("config dir");
    let config_path = dir.path().join("rsyncd.conf");
    fs::write(
        &config_path,
        "quic cert file = server.pem\nquic key file = server.key\nquic client ca file = clients.pem\n",
    )
    .expect("write config");

    let options = RuntimeOptions::parse(&[
        OsString::from("--config"),
        config_path.as_os_str().to_os_string(),
    ])
    .expect("parse config");

    let identity = options
        .resolve_quic_identity()
        .expect("cert + key must resolve an identity");
    assert_eq!(
        identity.client_ca,
        Some(dir.path().join("clients.pem")),
        "`quic client ca file` must be carried on the resolved identity for mutual TLS"
    );
}

#[test]
fn quic_client_ca_defaults_off_when_unset() {
    // Default off: a QUIC listener configured with only cert/key (no client CA)
    // resolves an identity with `client_ca = None`, so no client certificate is
    // required and the handshake is unchanged from the pre-mutual-TLS listener.
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

    let identity = options
        .resolve_quic_identity()
        .expect("cert + key must resolve an identity");
    assert_eq!(
        identity.client_ca, None,
        "no `quic client ca file` must leave mutual TLS off (client_ca = None)"
    );
}

#[test]
fn quic_client_ca_without_cert_key_is_a_fatal_request() {
    // A `quic client ca file` on its own requests QUIC (mutual TLS) but supplies
    // no server identity. QUIC has no ephemeral fallback, so this is a fatal
    // misconfiguration the startup path must reject - not a silent no-op.
    let dir = tempdir().expect("config dir");
    let config_path = dir.path().join("rsyncd.conf");
    fs::write(&config_path, "quic client ca file = clients.pem\n").expect("write config");

    let options = RuntimeOptions::parse(&[
        OsString::from("--config"),
        config_path.as_os_str().to_os_string(),
    ])
    .expect("parse config");

    let reason = options
        .quic_config_error()
        .expect("a client-CA-only config requests QUIC without an identity and must be rejected");
    assert!(
        reason.contains("quic cert file") && reason.contains("quic key file"),
        "the rejection must name the missing identity directives, got: {reason}"
    );
}
