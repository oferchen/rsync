/// The advertised digest list is the same at every protocol version.
///
/// upstream: compat.c:838-842 `output_daemon_greeting()` renders
/// `get_default_nno_list(&valid_auth_checksums, tmpbuf, ...)` and prints it
/// straight into `@RSYNCD: %d.%d %s\n`. `get_default_nno_list()` (compat.c:462)
/// walks `valid_auth_checksums_items[]` in table order and never consults
/// `protocol_version`, so a real daemon greets with the full list even when
/// forced down to an ancient protocol. Verified against rsync 3.4.4:
/// `rsync --daemon --protocol=28` answers
/// `@RSYNCD: 28.0 sha512 sha256 sha1 md5 md4`.
///
/// This is the property that keeps the codebase honest about having exactly one
/// advertised-list producer: the greeting and the `@ERROR: your client does not
/// support one of our daemon-auth checksums: <list>` refusal must name the same
/// digests, and they can only be relied on to agree if neither filters.
#[test]
fn legacy_daemon_greeting_advertises_the_full_list_at_every_protocol() {
    for (version, head) in [
        (ProtocolVersion::V28, "@RSYNCD: 28.0"),
        (ProtocolVersion::V29, "@RSYNCD: 29.0"),
        (ProtocolVersion::V30, "@RSYNCD: 30.0"),
        (ProtocolVersion::V31, "@RSYNCD: 31.0"),
        (ProtocolVersion::V32, "@RSYNCD: 32.0"),
    ] {
        let greeting = legacy_daemon_greeting_for_protocol(version);

        assert_eq!(
            greeting,
            format!("{head} sha512 sha256 sha1 md5 md4\n"),
            "protocol {} must greet with the unfiltered digest list",
            version.as_u8(),
        );
    }
}

/// The greeting renders exactly what the refusal path renders.
///
/// Two producers that "agree today" is how the protocol-filtered variant went
/// unnoticed: the daemon speaks first at the newest version, where the filter
/// happened to be a no-op. Pin the greeting to the single producer instead.
#[test]
fn legacy_daemon_greeting_digest_list_matches_the_refusal_list() {
    let advertised = core::auth::supported_daemon_digest_list();

    for version in [
        ProtocolVersion::V28,
        ProtocolVersion::V30,
        ProtocolVersion::V32,
    ] {
        let greeting = legacy_daemon_greeting_for_protocol(version);
        let (_, digests) = greeting
            .trim_end()
            .split_once(' ')
            .and_then(|(_, rest)| rest.split_once(' '))
            .expect("greeting must carry a digest list");

        assert_eq!(
            digests,
            advertised,
            "protocol {} greeting must name the same digests as the refusal line",
            version.as_u8(),
        );
    }
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
