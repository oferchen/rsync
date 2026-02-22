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
/// upstream: csprotocol.txt — the daemon greeting carries the digest list that
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

fn advertise_capabilities(
    stream: &mut TcpStream,
    modules: &[ModuleRuntime],
    messages: &LegacyMessageCache,
) -> io::Result<()> {
    for payload in advertised_capability_lines(modules) {
        let message = messages.render(LegacyDaemonMessage::Capabilities {
            flags: payload.as_str(),
        });
        stream.write_all(message.as_bytes())?;
    }

    if modules.is_empty() {
        Ok(())
    } else {
        stream.flush()
    }
}

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
