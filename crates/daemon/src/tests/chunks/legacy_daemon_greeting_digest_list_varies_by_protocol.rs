/// Protocol 32 greeting includes all five supported digest algorithms.
///
/// upstream: csprotocol.txt — protocol 32 requires the digest list and all
/// algorithms introduced through protocol 31 (SHA-512, SHA-256, SHA-1) are
/// present alongside MD5 and MD4.
#[test]
fn legacy_daemon_greeting_protocol_32_includes_all_digests() {
    let greeting = legacy_daemon_greeting_for_protocol(ProtocolVersion::V32);

    assert!(greeting.starts_with("@RSYNCD: 32.0"));
    assert!(greeting.ends_with('\n'));

    let trimmed = greeting.trim_end();
    let after_version = trimmed
        .strip_prefix("@RSYNCD: 32.0 ")
        .expect("expected digest list after version");
    let digests: Vec<&str> = after_version.split_whitespace().collect();

    assert_eq!(
        digests,
        vec!["sha512", "sha256", "sha1", "md5", "md4"],
        "protocol 32 must advertise all five digests in preference order"
    );
}

/// Protocol 31 greeting includes all five supported digest algorithms.
///
/// SHA-family digests were introduced alongside protocol 31 (rsync 3.2.7).
#[test]
fn legacy_daemon_greeting_protocol_31_includes_all_digests() {
    let greeting = legacy_daemon_greeting_for_protocol(ProtocolVersion::V31);

    assert!(greeting.starts_with("@RSYNCD: 31.0"));
    assert!(greeting.ends_with('\n'));

    let trimmed = greeting.trim_end();
    let after_version = trimmed
        .strip_prefix("@RSYNCD: 31.0 ")
        .expect("expected digest list after version");
    let digests: Vec<&str> = after_version.split_whitespace().collect();

    assert_eq!(
        digests,
        vec!["sha512", "sha256", "sha1", "md5", "md4"],
        "protocol 31 must advertise all five digests"
    );
}

/// Protocol 30 greeting includes only MD5 and MD4 — SHA-family digests were
/// not available before protocol 31.
///
/// upstream: csprotocol.txt — protocol 30-31 default to "md5" when the digest
/// list is absent.
#[test]
fn legacy_daemon_greeting_protocol_30_includes_md5_and_md4_only() {
    let greeting = legacy_daemon_greeting_for_protocol(ProtocolVersion::V30);

    assert!(greeting.starts_with("@RSYNCD: 30.0"));
    assert!(greeting.ends_with('\n'));

    let trimmed = greeting.trim_end();
    let after_version = trimmed
        .strip_prefix("@RSYNCD: 30.0 ")
        .expect("expected digest list after version");
    let digests: Vec<&str> = after_version.split_whitespace().collect();

    assert_eq!(
        digests,
        vec!["md5", "md4"],
        "protocol 30 must only advertise md5 and md4"
    );

    assert!(
        !trimmed.contains("sha512"),
        "protocol 30 must not include sha512"
    );
    assert!(
        !trimmed.contains("sha256"),
        "protocol 30 must not include sha256"
    );
    assert!(
        !trimmed.contains("sha1"),
        "protocol 30 must not include sha1"
    );
}

/// Protocol 29 greeting omits the digest list entirely.
///
/// Pre-protocol-30 clients do not expect a digest list in the greeting. They
/// assume MD4 by convention (upstream: csprotocol.txt).
#[test]
fn legacy_daemon_greeting_protocol_29_omits_digest_list() {
    let greeting = legacy_daemon_greeting_for_protocol(ProtocolVersion::V29);

    assert_eq!(
        greeting, "@RSYNCD: 29.0\n",
        "pre-protocol-30 greeting must not include a digest list"
    );
}

/// Protocol 28 greeting omits the digest list entirely.
#[test]
fn legacy_daemon_greeting_protocol_28_omits_digest_list() {
    let greeting = legacy_daemon_greeting_for_protocol(ProtocolVersion::V28);

    assert_eq!(
        greeting, "@RSYNCD: 28.0\n",
        "protocol 28 greeting must not include a digest list"
    );
}

/// The default greeting (no explicit version) uses the newest protocol and
/// matches the output of the version-parameterised variant.
#[test]
fn legacy_daemon_greeting_default_matches_newest_protocol() {
    let default = legacy_daemon_greeting();
    let explicit = legacy_daemon_greeting_for_protocol(ProtocolVersion::NEWEST);

    assert_eq!(
        default, explicit,
        "default greeting must match newest-protocol greeting"
    );
}
