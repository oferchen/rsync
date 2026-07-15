/// Builds the legacy `@RSYNCD:` greeting for the given protocol version.
///
/// The digest list appended to the version announcement is protocol-version-aware:
///
/// - **Protocol >= 31**: all supported digests (sha512, sha256, sha1, md5, md4).
/// - **Protocol 30**: md5 and md4. SHA-family digests were not available before
///   protocol 31.
/// - **Protocol < 30**: md4 only. MD5 was introduced in protocol 30; no digest
///   list is appended since legacy clients do not expect one.
///
/// upstream: csprotocol.txt - the daemon greeting carries the digest list that
/// informs the client which challenge/response algorithms the server accepts.
pub(crate) fn legacy_daemon_greeting_for_protocol(version: ProtocolVersion) -> String {
    let mut greeting =
        format_legacy_daemon_message(LegacyDaemonMessage::Version(version));
    debug_assert!(greeting.ends_with('\n'));

    let digests = digests_for_protocol(version);

    // Pre-protocol-30 greetings omit the digest list entirely; clients assume
    // MD4 by convention (upstream: csprotocol.txt).
    if digests.is_empty() || version.as_u8() < 30 {
        return greeting;
    }

    greeting.pop();

    for digest in digests {
        greeting.push(' ');
        greeting.push_str(digest.name());
    }

    greeting.push('\n');
    greeting
}

/// Builds the legacy `@RSYNCD:` greeting at the server's newest protocol version.
///
/// This is the default greeting emitted by the daemon listener. The digest list
/// is populated according to the protocol-version rules documented on
/// [`legacy_daemon_greeting_for_protocol`].
pub(crate) fn legacy_daemon_greeting() -> String {
    legacy_daemon_greeting_for_protocol(ProtocolVersion::NEWEST)
}

/// Returns the cached newest-protocol greeting bytes for direct wire emission.
///
/// The greeting at `ProtocolVersion::NEWEST` is a pure function of the
/// compiled-in digest list, so it is rendered once per process and reused for
/// every accepted connection. This removes the per-accept `format!`, `pop`,
/// and `push_str` chain identified by the DIS-4.a audit while keeping the wire
/// bytes byte-identical to the previous per-accept builder.
///
/// upstream: clientserver.c:455 `output_daemon_greeting` writes the same
/// `@RSYNCD: <ver>.<sub> <digests>\n` line from a stack buffer that is
/// initialized once on the server.
pub(crate) fn cached_legacy_daemon_greeting() -> &'static [u8] {
    static GREETING: OnceLock<Box<[u8]>> = OnceLock::new();
    GREETING.get_or_init(|| legacy_daemon_greeting().into_bytes().into_boxed_slice())
}

/// Validates a client's `@RSYNCD:` version greeting the way an rsync daemon
/// does, returning the fatal `@ERROR:` line to emit when it is malformed.
///
/// Returns `Some(payload)` (including the `@ERROR:` prefix) when the greeting
/// announces a protocol version but omits a token the protocol requires: the
/// `.subprotocol` suffix for protocol >= 30, or the digest-name list for
/// protocol > 31. The caller must write the payload, close the connection, and
/// stop. Returns `None` when the line is a well-formed greeting or is not a
/// version banner at all, in which case normal parsing proceeds.
///
/// upstream: clientserver.c:180-211 `exchange_protocols()` with `am_client == 0`.
/// The daemon reads the protocol number with `sscanf(buf, "@RSYNCD: %d.%d", ...)`;
/// a missing `.subprotocol` leaves `remote_sub < 0` and, for `remote_protocol >= 30`,
/// yields `@ERROR: your client omitted the subprotocol value: %s`. A missing digest
/// list (`strchr(buf + 9, ' ')` is NULL) yields, for `remote_protocol > 31`,
/// `@ERROR: your client omitted the digest name list: %s`. The `%s` echoes the raw
/// greeting line.
///
/// The presence gates themselves live in [`missing_greeting_token`] so the
/// daemon (`am_client == 0`) and the client (`am_client == 1`) enforce the exact
/// same upstream thresholds; only the diagnostic wording differs by role.
pub(crate) fn reject_malformed_client_greeting(line: &str) -> Option<String> {
    let token = missing_greeting_token(line)?;
    Some(format!(
        "@ERROR: your client omitted the {}: {line}",
        token.description()
    ))
}

/// Reads one line from `reader`, stripping trailing `\r` and `\n`.
///
/// Returns `Ok(None)` on EOF, or `Ok(Some(line))` with the stripped content.
pub(crate) fn read_trimmed_line<R: BufRead>(reader: &mut R) -> io::Result<Option<String>> {
    let mut line = String::new();
    let bytes = reader.read_line(&mut line)?;

    if bytes == 0 {
        return Ok(None);
    }

    while line.ends_with('\n') || line.ends_with('\r') {
        line.pop();
    }

    Ok(Some(line))
}

/// Returns the `@RSYNCD: capabilities` lines to advertise to the client.
///
/// Emits `modules` unconditionally when modules are present, and appends
/// `authlist` when at least one module requires authentication. Returns an
/// empty vec when no modules are configured.
#[cfg(test)]
pub(crate) fn advertised_capability_lines(modules: &[ModuleRuntime]) -> Vec<String> {
    if modules.is_empty() {
        return Vec::new();
    }

    let mut features = Vec::with_capacity(2);
    features.push(String::from("modules"));

    if modules
        .iter()
        .any(|module| module.requires_authentication())
    {
        features.push(String::from("authlist"));
    }

    vec![features.join(" ")]
}

#[cfg(test)]
mod greeting_validation_tests {
    use super::reject_malformed_client_greeting;

    // upstream: clientserver.c:188-197 - a protocol >= 30 greeting without a
    // ".subprotocol" suffix leaves remote_sub < 0 and is fatal. The @ERROR line
    // must echo the raw greeting so the client can report what it sent.
    #[test]
    fn rejects_greeting_missing_subprotocol() {
        assert_eq!(
            reject_malformed_client_greeting("@RSYNCD: 32").as_deref(),
            Some("@ERROR: your client omitted the subprotocol value: @RSYNCD: 32"),
        );
        assert_eq!(
            reject_malformed_client_greeting("@RSYNCD: 30").as_deref(),
            Some("@ERROR: your client omitted the subprotocol value: @RSYNCD: 30"),
        );
    }

    // upstream: clientserver.c:199-211 - protocol > 31 must carry a digest name
    // list; its absence is fatal even when the subprotocol value is present.
    #[test]
    fn rejects_greeting_missing_digest_list() {
        assert_eq!(
            reject_malformed_client_greeting("@RSYNCD: 32.0").as_deref(),
            Some("@ERROR: your client omitted the digest name list: @RSYNCD: 32.0"),
        );
    }

    // upstream: clientserver.c:205 - the digest gate is `remote_protocol > 31`, so
    // protocol 31 needs the subprotocol but not a digest list.
    #[test]
    fn protocol_31_requires_subprotocol_not_digest() {
        assert_eq!(
            reject_malformed_client_greeting("@RSYNCD: 31").as_deref(),
            Some("@ERROR: your client omitted the subprotocol value: @RSYNCD: 31"),
        );
        assert_eq!(reject_malformed_client_greeting("@RSYNCD: 31.0"), None);
    }

    // upstream: clientserver.c:196 - protocol < 30 defaults remote_sub to 0 and
    // needs no digest list, so a bare legacy version is accepted.
    #[test]
    fn accepts_legacy_greeting_without_subprotocol_or_digest() {
        assert_eq!(reject_malformed_client_greeting("@RSYNCD: 29"), None);
    }

    // A fully-formed modern greeting (subprotocol suffix + digest list), exactly
    // what upstream and oc-rsync clients send, is accepted unchanged.
    #[test]
    fn accepts_well_formed_modern_greeting() {
        assert_eq!(
            reject_malformed_client_greeting("@RSYNCD: 32.0 sha512 sha256 sha1 md5 md4"),
            None,
        );
    }

    // Non-version lines (module names, control keywords) are not greetings and
    // pass through so the normal message parser handles them.
    #[test]
    fn ignores_non_version_lines() {
        assert_eq!(reject_malformed_client_greeting("module"), None);
        assert_eq!(reject_malformed_client_greeting("@RSYNCD: OK"), None);
        assert_eq!(reject_malformed_client_greeting("#list"), None);
    }

    // Regression for #6604: the daemon refusal now delegates to the shared
    // `missing_greeting_token` gate, but the @ERROR wording and the exact set of
    // refused greetings must stay byte-identical. A greeting the gate accepts
    // must return `None` here so `parse_legacy_daemon_message` handles it next -
    // the daemon must never double-reject a greeting the shared gate cleared.
    #[test]
    fn daemon_refusal_agrees_with_shared_gate() {
        for line in [
            "@RSYNCD: 32.0 sha512 sha256 sha1 md5 md4",
            "@RSYNCD: 31.0",
            "@RSYNCD: 29",
            "@RSYNCD: OK",
            "module",
        ] {
            assert_eq!(
                protocol::missing_greeting_token(line).is_none(),
                reject_malformed_client_greeting(line).is_none(),
                "daemon refusal must track the shared gate for {line:?}",
            );
        }
    }
}
