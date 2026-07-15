#![deny(unsafe_code)]
//! Server-side protocol handshake utilities.
//!
//! The rsync protocol begins with a version exchange where both sides advertise
//! their supported protocol versions and negotiate a common version to use.
//!
//! For server mode (invoked via `--server`), the sequence is:
//! 1. Write our maximum protocol version to stdout (upstream: compat.c:603)
//! 2. Read the remote's protocol version from stdin (upstream: compat.c:604)
//! 3. Negotiate to the highest mutually supported version
//! 4. Proceed with the negotiated protocol version
//!
//! Both sides write before reading to avoid deadlock when both ends are the
//! same implementation.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::sync::Arc;

use protocol::{
    CompatibilityFlags, NegotiationResult, ProtocolVersion, check_sub_protocol,
    get_subprotocol_version, select_highest_mutual,
};

/// Re-applies an adopted daemon `MSG_IO_TIMEOUT` to the live client socket.
///
/// Upstream `io.c:set_io_timeout` updates the global `io_timeout` so the select
/// loop's stall detection changes immediately after the client adopts a
/// daemon-advertised timeout (upstream: `io.c:1551-1561` `read_a_msg()` case
/// `MSG_IO_TIMEOUT`). The oc client has no global; instead the daemon-pull path
/// installs this hook, which re-applies the adopted value to the socket's read
/// and write timeouts. Only the client receiver of a daemon transfer installs
/// one - every other path leaves it `None`, so the default transfer performs no
/// re-apply and stays wire-identical.
///
/// The wrapped closure takes the adopted timeout in whole seconds (`0` clears
/// the timeout, i.e. infinite) and is best-effort: a transport without a socket
/// timeout (a connect-program pipe) installs no hook at all.
#[derive(Clone)]
pub struct IoTimeoutReapply(pub Arc<dyn Fn(u32) -> io::Result<()> + Send + Sync>);

impl std::fmt::Debug for IoTimeoutReapply {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("IoTimeoutReapply(<fn>)")
    }
}

impl IoTimeoutReapply {
    /// Re-applies `secs` as the live socket's read and write I/O timeout.
    ///
    /// upstream: `io.c:1148-1157` `set_io_timeout()`.
    pub fn apply(&self, secs: u32) -> io::Result<()> {
        (self.0)(secs)
    }
}

/// Result of a successful server-side handshake.
#[derive(Debug, Clone)]
pub struct HandshakeResult {
    /// The negotiated protocol version.
    pub protocol: ProtocolVersion,
    /// Any bytes that were buffered during version detection.
    pub buffered: Vec<u8>,
    /// Whether compatibility flags have already been exchanged on the raw stream.
    /// When true, setup_protocol() should skip the compat flags exchange.
    pub compat_exchanged: bool,
    /// Client arguments for daemon mode (includes -e option with capabilities).
    /// None for SSH mode.
    pub client_args: Option<Vec<String>>,
    /// I/O timeout value in seconds for daemon mode.
    /// When Some(_), daemon mode should send MSG_IO_TIMEOUT for protocol >= 31.
    /// None for SSH mode or when no timeout is configured.
    pub io_timeout: Option<u64>,
    /// Negotiated checksum and compression algorithms from Protocol 30+ capability negotiation.
    /// None for protocols < 30 or when negotiation was skipped.
    pub negotiated_algorithms: Option<NegotiationResult>,
    /// Compatibility flags exchanged during protocol setup.
    /// None for protocols < 30 or when compat exchange was skipped.
    pub compat_flags: Option<CompatibilityFlags>,
    /// Checksum seed for XXHash algorithms.
    /// Generated during setup_protocol() and used when creating XXHash instances.
    pub checksum_seed: i32,
}

/// Performs the server-side protocol version handshake.
///
/// This writes our maximum protocol version, reads the remote's version, and
/// negotiates the highest common version. Returns the negotiated version.
///
/// # Protocol
///
/// Each side sends a 4-byte binary version advertisement:
/// - Byte 0: Protocol version (e.g., 32)
/// - Byte 1: Protocol sub-version (usually 0)
/// - Bytes 2-3: Reserved/compatibility flags
pub fn perform_handshake(
    stdin: &mut dyn Read,
    stdout: &mut dyn Write,
) -> io::Result<HandshakeResult> {
    perform_handshake_with_max(stdin, stdout, ProtocolVersion::NEWEST)
}

/// Performs the server-side version handshake, reconciling a pre-release peer's
/// subprotocol before advertising our protocol version.
///
/// upstream: compat.c:600-602 `setup_protocol()` - on the server side of a
/// non-local (remote-shell) transfer the server runs `check_sub_protocol()`
/// (compat.c:133-160) against the client's advertised `VER.SUB` BEFORE it writes
/// its own protocol version. The client's `VER.SUB` rides its `-e` capability
/// string (`client_info = shell_cmd`, compat.c:163-164), which oc receives in the
/// compact server flag string. When both sides are final releases (subprotocol
/// 0) this is a no-op and the exchange is byte-identical to [`perform_handshake`];
/// only a pre-release peer triggers the one-step downgrade.
///
/// `client_flags` is the compact server flag string the client sent (oc's
/// equivalent of upstream `shell_cmd`). A release peer that advertised no
/// pre-release `VER.SUB` yields no downgrade and the uncapped newest-version
/// handshake stands.
pub fn perform_server_handshake(
    stdin: &mut dyn Read,
    stdout: &mut dyn Write,
    client_flags: &str,
) -> io::Result<HandshakeResult> {
    let effective = reconcile_subprotocol(ProtocolVersion::NEWEST, client_flags);
    perform_handshake_with_max(stdin, stdout, effective)
}

/// Applies upstream `check_sub_protocol()` (compat.c:133-160) to derive the
/// protocol version the server advertises after reconciling the peer's
/// pre-release `VER.SUB`.
///
/// upstream: compat.c:602 - the reconciliation runs against `protocol_version`
/// (here `max_version`, the newest version the server would otherwise advertise)
/// and lowers it by one step when the peer is a pre-release whose subprotocol is
/// incompatible with ours. A stock release peer parses to `(0, 0)` and leaves the
/// version unchanged, so the write remains wire-identical to [`perform_handshake`].
fn reconcile_subprotocol(max_version: ProtocolVersion, client_flags: &str) -> ProtocolVersion {
    let (their_protocol, their_sub) = crate::setup::parse_peer_subprotocol(client_flags);
    // upstream: compat.c:137 `get_subprotocol_version()` - a release oc build
    // (SUBPROTOCOL_VERSION == 0) always advertises subprotocol 0.
    let our_sub = get_subprotocol_version(max_version.as_u8());
    let reconciled = check_sub_protocol(max_version.as_u8(), our_sub, their_protocol, their_sub);
    if reconciled == max_version.as_u8() {
        return max_version;
    }
    // check_sub_protocol never raises the version. Clamp the (unreachable for a
    // release peer) sub-OLDEST result to the floor so the downgrade direction is
    // preserved, mirroring upstream which advertises the lowered value and lets
    // the later MIN_PROTOCOL_VERSION guard reject anything too old.
    let floor = ProtocolVersion::OLDEST.as_u8();
    ProtocolVersion::try_from(reconciled.max(floor)).unwrap_or(max_version)
}

/// Performs the version handshake while advertising at most `max_version`.
///
/// `max_version` caps both the version we advertise and the version we accept
/// as negotiated, mirroring the upstream `--protocol=N` flag. Upstream stores
/// the requested value in `protocol_version` (options.c:846) - already lowered
/// from the `PROTOCOL_VERSION` default - and `setup_protocol()` writes it, reads
/// the peer's version, then clamps with `protocol_version = MIN(protocol_version,
/// remote_protocol)` (compat.c:604-607). Passing `ProtocolVersion::NEWEST` (the
/// default) reproduces the uncapped behaviour.
pub fn perform_handshake_with_max(
    stdin: &mut dyn Read,
    stdout: &mut dyn Write,
    max_version: ProtocolVersion,
) -> io::Result<HandshakeResult> {
    // upstream: compat.c:602-604 - write our max version first, then read.
    // Both sides do this simultaneously; reversing the order deadlocks when
    // both ends are oc-rsync (each waits for the other to write first).
    write_server_version(stdout, max_version)?;
    stdout.flush()?;

    let remote_version = read_client_version(stdin)?;

    // upstream: compat.c:619-623 setup_protocol - a remote protocol version
    // outside [MIN_PROTOCOL_VERSION, MAX_PROTOCOL_VERSION] calls
    // exit_cleanup(RERR_PROTOCOL) (exit 2), not RERR_STREAMIO (12). Tag the
    // negotiation failure so the exit-code mapper reports RERR_PROTOCOL.
    let negotiated = select_highest_mutual([remote_version]).map_err(|e| {
        protocol::protocol_violation(format!(
            "remote protocol version {} is not supported: {e}",
            remote_version.as_u8()
        ))
    })?;
    // upstream: compat.c:606-607 - protocol_version = MIN(protocol_version,
    // remote_protocol). Clamp the mutually-supported version to our advertised
    // ceiling so `--protocol=N` caps the negotiated version.
    let negotiated = negotiated.min(max_version);

    Ok(HandshakeResult {
        protocol: negotiated,
        buffered: Vec::new(),
        compat_exchanged: false,
        client_args: None,           // SSH mode doesn't have daemon client args
        io_timeout: None,            // SSH mode doesn't configure I/O timeouts
        negotiated_algorithms: None, // Will be populated by setup_protocol()
        compat_flags: None,          // Will be populated by setup_protocol()
        checksum_seed: 0,            // Will be populated by setup_protocol()
    })
}

/// Reads the client's protocol version from a 4-byte binary advertisement.
fn read_client_version(stdin: &mut dyn Read) -> io::Result<ProtocolVersion> {
    let mut buf = [0u8; 4];
    stdin.read_exact(&mut buf)?;

    let version_byte = buf[0];
    // upstream: compat.c:619-623 setup_protocol - an out-of-range remote
    // protocol version is a protocol incompatibility (RERR_PROTOCOL, exit 2),
    // distinct from a truncated stream (RERR_STREAMIO, exit 12). The
    // read_exact above keeps its stream-error mapping; only the version-value
    // checks are tagged as protocol violations.
    if version_byte == 0 {
        return Err(protocol::protocol_violation(
            "received invalid protocol version 0",
        ));
    }

    ProtocolVersion::try_from(version_byte)
        .map_err(|e| protocol::protocol_violation(format!("invalid protocol version: {e}")))
}

/// Writes the server's protocol version advertisement.
fn write_server_version(stdout: &mut dyn Write, version: ProtocolVersion) -> io::Result<()> {
    let mut buf = [0u8; 4];
    buf[0] = version.as_u8();
    // buf[1] = sub-version (0 for now)
    // buf[2..4] = reserved/compatibility flags
    stdout.write_all(&buf)
}

/// Performs legacy ASCII-based handshake for older protocol versions.
///
/// Some older rsync clients (protocol < 30) use an ASCII-based greeting format
/// instead of the binary handshake.
pub fn perform_legacy_handshake(
    stdin: &mut dyn Read,
    stdout: &mut dyn Write,
) -> io::Result<HandshakeResult> {
    let mut reader = BufReader::new(stdin);
    let mut line = String::new();
    reader.read_line(&mut line)?;

    let trimmed = line.trim();
    if !trimmed.starts_with("@RSYNCD:") {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("expected @RSYNCD: greeting, got: {trimmed}"),
        ));
    }

    let version_str = trimmed
        .strip_prefix("@RSYNCD:")
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "expected @RSYNCD: prefix in greeting",
            )
        })?
        .split_whitespace()
        .next()
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "missing version in legacy greeting",
            )
        })?;

    let version_number: u8 = version_str
        .split('.')
        .next()
        .unwrap_or("0")
        .parse()
        .map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("invalid legacy protocol version: {version_str}"),
            )
        })?;

    // upstream: compat.c:619-623 setup_protocol - an out-of-range peer protocol
    // version is RERR_PROTOCOL (exit 2), not RERR_STREAMIO (12).
    let client_version = ProtocolVersion::try_from(version_number)
        .map_err(|e| protocol::protocol_violation(format!("unsupported protocol version: {e}")))?;

    let negotiated = select_highest_mutual([client_version]).map_err(|e| {
        protocol::protocol_violation(format!(
            "protocol version {version_number} is not supported: {e}"
        ))
    })?;

    let response = format!("@RSYNCD: {}.0\n", negotiated.as_u8());
    stdout.write_all(response.as_bytes())?;
    stdout.flush()?;

    Ok(HandshakeResult {
        protocol: negotiated,
        buffered: Vec::new(),
        compat_exchanged: false,
        client_args: None,           // SSH mode doesn't have daemon client args
        io_timeout: None,            // SSH mode doesn't configure I/O timeouts
        negotiated_algorithms: None, // Will be populated by setup_protocol()
        compat_flags: None,          // Will be populated by setup_protocol()
        checksum_seed: 0,            // Will be populated by setup_protocol()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn binary_handshake_negotiates_version() {
        // Client sends version 32
        let mut stdin = Cursor::new(vec![32, 0, 0, 0]);
        let mut stdout = Vec::new();

        let result = perform_handshake(&mut stdin, &mut stdout).expect("handshake succeeds");
        assert_eq!(result.protocol.as_u8(), 32);

        // Server should respond with 4-byte version
        assert_eq!(stdout.len(), 4);
        assert_eq!(stdout[0], 32);
    }

    #[test]
    fn max_version_caps_negotiated_below_remote() {
        // Remote advertises 32; we cap to 29 via --protocol=29. The negotiated
        // version must clamp to 29 and we must advertise 29 on the wire so the
        // peer's own MIN(remote, ours) converges to 29 too.
        // upstream: compat.c:604-607
        let mut stdin = Cursor::new(vec![32, 0, 0, 0]);
        let mut stdout = Vec::new();

        let result = perform_handshake_with_max(&mut stdin, &mut stdout, ProtocolVersion::V29)
            .expect("handshake succeeds");
        assert_eq!(result.protocol, ProtocolVersion::V29);
        assert_eq!(stdout[0], 29, "must advertise the capped version");
    }

    #[test]
    fn max_version_does_not_raise_below_remote() {
        // We cap to 32 (default) but the remote only offers 30: negotiate 30.
        let mut stdin = Cursor::new(vec![30, 0, 0, 0]);
        let mut stdout = Vec::new();

        let result = perform_handshake_with_max(&mut stdin, &mut stdout, ProtocolVersion::V32)
            .expect("handshake succeeds");
        assert_eq!(result.protocol, ProtocolVersion::V30);
        assert_eq!(stdout[0], 32);
    }

    #[test]
    fn max_version_30_and_31_are_honored() {
        for (cap, expected) in [(ProtocolVersion::V30, 30u8), (ProtocolVersion::V31, 31u8)] {
            let mut stdin = Cursor::new(vec![32, 0, 0, 0]);
            let mut stdout = Vec::new();
            let result = perform_handshake_with_max(&mut stdin, &mut stdout, cap)
                .expect("handshake succeeds");
            assert_eq!(result.protocol.as_u8(), expected);
            assert_eq!(stdout[0], expected);
        }
    }

    #[test]
    fn default_handshake_advertises_newest() {
        // The no-cap path is byte-identical to advertising NEWEST.
        let mut stdin = Cursor::new(vec![32, 0, 0, 0]);
        let mut stdout = Vec::new();
        let result = perform_handshake(&mut stdin, &mut stdout).expect("handshake succeeds");
        assert_eq!(result.protocol, ProtocolVersion::NEWEST);
        assert_eq!(stdout[0], ProtocolVersion::NEWEST.as_u8());
    }

    #[test]
    fn binary_handshake_caps_to_supported_version() {
        // Client sends a higher version than we support
        let mut stdin = Cursor::new(vec![99, 0, 0, 0]);
        let mut stdout = Vec::new();

        // Should fail because version 99 is not supported
        let result = perform_handshake(&mut stdin, &mut stdout);
        assert!(result.is_err());
    }

    #[test]
    fn binary_handshake_rejects_version_zero() {
        let mut stdin = Cursor::new(vec![0, 0, 0, 0]);
        let mut stdout = Vec::new();

        let result = perform_handshake(&mut stdin, &mut stdout);
        assert!(result.is_err());
    }

    // upstream: compat.c:600-602 + compat.c:156-159 - the non-local server runs
    // check_sub_protocol() before writing its version. A pre-release peer of the
    // SAME protocol whose subprotocol differs from ours forces the one-step
    // downgrade so both sides drop to the last mutually compatible protocol.
    // This is the WHY: without it, an oc server would advertise 32 to a
    // pre-release-32 peer and the two would disagree on wire semantics.
    #[test]
    fn server_handshake_downgrades_against_equal_prerelease_peer() {
        // Peer advertises numeric protocol 32 and, via its `-e` capability
        // string, subprotocol 7 of protocol 32 (a pre-release).
        let mut stdin = Cursor::new(vec![32, 0, 0, 0]);
        let mut stdout = Vec::new();

        let result = perform_server_handshake(&mut stdin, &mut stdout, "-logDtpre32.7LsfxCIvu")
            .expect("handshake succeeds");

        assert_eq!(result.protocol, ProtocolVersion::V31);
        assert_eq!(
            stdout[0], 31,
            "server must advertise the downgraded version"
        );
    }

    // upstream: compat.c:150-154 - a pre-release of an OLDER protocol pins the
    // negotiated version to the last release of that older protocol.
    #[test]
    fn server_handshake_downgrades_against_older_prerelease_peer() {
        // Peer is a pre-release of protocol 30 (subprotocol 5).
        let mut stdin = Cursor::new(vec![30, 0, 0, 0]);
        let mut stdout = Vec::new();

        let result = perform_server_handshake(&mut stdin, &mut stdout, "-logDtpre30.5LsfxCIvu")
            .expect("handshake succeeds");

        assert_eq!(result.protocol, ProtocolVersion::V29);
        assert_eq!(stdout[0], 29, "pins to their_protocol - 1");
    }

    // upstream: compat.c:139-148 - a stock release peer advertises `-e.<caps>`
    // whose leading '.' makes `atoi` return 0, so check_sub_protocol is a no-op.
    // This is the wire-transparency guarantee: current behaviour is unchanged for
    // every real release peer.
    #[test]
    fn server_handshake_noop_against_release_peer() {
        let mut stdin = Cursor::new(vec![32, 0, 0, 0]);
        let mut stdout = Vec::new();

        let result = perform_server_handshake(&mut stdin, &mut stdout, "-logDtpre.LsfxCIvu")
            .expect("handshake succeeds");

        assert_eq!(result.protocol, ProtocolVersion::NEWEST);
        assert_eq!(stdout[0], ProtocolVersion::NEWEST.as_u8());
    }

    // A flag string with no `-e` capability payload carries no VER.SUB, so there
    // is nothing to reconcile and the newest version stands.
    #[test]
    fn server_handshake_noop_without_capability_string() {
        let mut stdin = Cursor::new(vec![32, 0, 0, 0]);
        let mut stdout = Vec::new();

        let result = perform_server_handshake(&mut stdin, &mut stdout, "-logDtpr")
            .expect("handshake succeeds");

        assert_eq!(result.protocol, ProtocolVersion::NEWEST);
        assert_eq!(stdout[0], ProtocolVersion::NEWEST.as_u8());
    }

    // upstream: compat.c:600-601 - check_sub_protocol runs ONLY on the server
    // (am_server && !local_server). The client-side handshake has no client_info
    // channel, so a plain `perform_handshake` never reconciles: even when the
    // peer would be a pre-release, the client leaves the version untouched.
    #[test]
    fn client_handshake_never_reconciles() {
        let mut stdin = Cursor::new(vec![32, 0, 0, 0]);
        let mut stdout = Vec::new();

        let result = perform_handshake(&mut stdin, &mut stdout).expect("handshake succeeds");

        assert_eq!(result.protocol, ProtocolVersion::NEWEST);
        assert_eq!(stdout[0], ProtocolVersion::NEWEST.as_u8());
    }

    // Pins the reconciliation mapping directly, independent of the I/O exchange.
    // upstream: compat.c:133-160 check_sub_protocol() driven from a release side.
    #[test]
    fn reconcile_subprotocol_matches_check_sub_protocol() {
        let newest = ProtocolVersion::NEWEST;
        // Release peer / no VER.SUB -> unchanged.
        assert_eq!(reconcile_subprotocol(newest, "-e.LsfxCIvu"), newest);
        assert_eq!(reconcile_subprotocol(newest, ""), newest);
        // Equal-protocol pre-release peer -> newest - 1.
        assert_eq!(
            reconcile_subprotocol(newest, "-e32.4LsfxCIvu"),
            ProtocolVersion::V31,
        );
        // Older-protocol pre-release peer -> their_protocol - 1.
        assert_eq!(
            reconcile_subprotocol(newest, "-e31.2LsfxCIvu"),
            ProtocolVersion::V30,
        );
    }

    #[test]
    fn legacy_handshake_parses_greeting() {
        let mut stdin = Cursor::new(b"@RSYNCD: 32.0\n".to_vec());
        let mut stdout = Vec::new();

        let result = perform_legacy_handshake(&mut stdin, &mut stdout).expect("handshake succeeds");
        assert_eq!(result.protocol.as_u8(), 32);

        let response = String::from_utf8_lossy(&stdout);
        assert!(response.starts_with("@RSYNCD: 32"));
    }

    /// Asserts that `error` is an `InvalidData` error tagged as a
    /// [`protocol::ProtocolViolation`], which the core exit-code mapper turns
    /// into `RERR_PROTOCOL` (exit 2) rather than `RERR_STREAMIO` (exit 12).
    fn assert_maps_to_rerr_protocol(error: &io::Error) {
        assert_eq!(
            error.kind(),
            io::ErrorKind::InvalidData,
            "kind must stay InvalidData for backward compatibility"
        );
        assert!(
            error
                .get_ref()
                .is_some_and(|inner| inner.is::<protocol::ProtocolViolation>()),
            "out-of-range protocol version must map to RERR_PROTOCOL (2), not RERR_STREAMIO (12)"
        );
    }

    // WHY: upstream compat.c:619-623 exits RERR_PROTOCOL (2) for a peer protocol
    // version outside [MIN_PROTOCOL_VERSION, MAX_PROTOCOL_VERSION]. A drop-in
    // tool or wrapper script keys on that exit code to distinguish a protocol
    // mismatch (2) from a corrupt/truncated data stream (12), so the binary
    // handshake must tag the out-of-range version as a protocol violation.
    #[test]
    fn binary_handshake_out_of_range_version_maps_to_rerr_protocol() {
        let mut stdin = Cursor::new(vec![99, 0, 0, 0]);
        let mut stdout = Vec::new();

        let error = perform_handshake(&mut stdin, &mut stdout)
            .expect_err("version 99 is outside the supported range");
        assert_maps_to_rerr_protocol(&error);
    }

    // WHY: a zero version byte is an invalid protocol version, not a stream
    // error; upstream treats an out-of-range remote_protocol as RERR_PROTOCOL.
    #[test]
    fn binary_handshake_version_zero_maps_to_rerr_protocol() {
        let mut stdin = Cursor::new(vec![0, 0, 0, 0]);
        let mut stdout = Vec::new();

        let error = perform_handshake(&mut stdin, &mut stdout).expect_err("version 0 is invalid");
        assert_maps_to_rerr_protocol(&error);
    }

    // WHY: the legacy `@RSYNCD:` handshake path must classify an out-of-range
    // version identically to the binary path - protocol version 27 is below the
    // supported floor and must exit RERR_PROTOCOL (2), not RERR_STREAMIO (12).
    #[test]
    fn legacy_handshake_out_of_range_version_maps_to_rerr_protocol() {
        let mut stdin = Cursor::new(b"@RSYNCD: 27.0\n".to_vec());
        let mut stdout = Vec::new();

        let error = perform_legacy_handshake(&mut stdin, &mut stdout)
            .expect_err("protocol 27 is below the supported floor");
        assert_maps_to_rerr_protocol(&error);
    }

    // Regression guard: a legitimate in-range version must still negotiate
    // successfully and must NOT be misclassified as a protocol violation.
    #[test]
    fn binary_handshake_in_range_version_still_succeeds() {
        let mut stdin = Cursor::new(vec![30, 0, 0, 0]);
        let mut stdout = Vec::new();

        let result =
            perform_handshake(&mut stdin, &mut stdout).expect("in-range handshake succeeds");
        assert_eq!(result.protocol, ProtocolVersion::V30);
    }
}
