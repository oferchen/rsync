use std::io;
use std::sync::{Mutex, MutexGuard, PoisonError};

use platform::env::EnvGuard;

use super::algorithms::{SUPPORTED_CHECKSUMS, supported_compressions};
use super::negotiate::{
    choose_checksum_algorithm, choose_compression_algorithm, read_vstring, write_vstring,
};
use super::*;
use crate::ProtocolVersion;

/// Environment variable overriding the checksum negotiation list.
const CHECKSUM_ENV: &str = "RSYNC_CHECKSUM_LIST";
/// Environment variable overriding the compression negotiation list.
const COMPRESS_ENV: &str = "RSYNC_COMPRESS_LIST";

/// Serialises every test that reads or writes the negotiation env overrides.
///
/// `env_list::parse_env` calls `std::env::var` on each negotiation - nothing
/// caches the value - so `negotiate_capabilities` observes whatever
/// `RSYNC_CHECKSUM_LIST`/`RSYNC_COMPRESS_LIST` hold at that instant. Under a
/// thread-parallel harness an override test therefore rewrites the candidate
/// lists of any test negotiating concurrently in the same process. Readers take
/// this lock through [`default_env`], writers through [`env_lock`], so the two
/// kinds never overlap.
static ENV_LOCK: Mutex<()> = Mutex::new(());

/// Acquires [`ENV_LOCK`], ignoring poisoning so one failing test does not
/// cascade into every other env-sensitive test.
fn env_lock() -> MutexGuard<'static, ()> {
    ENV_LOCK.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Holds [`ENV_LOCK`] with both negotiation overrides removed.
///
/// Fields drop in declaration order, so the [`EnvGuard`]s restore the previous
/// values before the lock is released.
struct DefaultEnv {
    _checksum: EnvGuard,
    _compress: EnvGuard,
    _lock: MutexGuard<'static, ()>,
}

/// Pins both negotiation overrides to unset for the caller's scope.
///
/// A negotiating test then sees the built-in default candidate lists regardless
/// of the ambient environment or of a concurrently running override test.
#[must_use]
fn default_env() -> DefaultEnv {
    let lock = env_lock();
    DefaultEnv {
        _checksum: EnvGuard::remove(CHECKSUM_ENV),
        _compress: EnvGuard::remove(COMPRESS_ENV),
        _lock: lock,
    }
}

#[test]
fn test_checksum_algorithm_roundtrip() {
    for &name in &["md4", "md5", "sha1", "xxh64", "xxh128"] {
        let algo = ChecksumAlgorithm::parse(name).unwrap();
        let roundtrip = algo.as_str();
        let reparsed = ChecksumAlgorithm::parse(roundtrip).unwrap();
        assert_eq!(algo, reparsed, "roundtrip failed for {name}");
    }
}

#[test]
fn test_compression_algorithm_roundtrip() {
    for &name in &["none", "zlib", "zlibx", "lz4", "zstd"] {
        let algo = CompressionAlgorithm::parse(name).unwrap();
        let roundtrip = algo.as_str();
        let reparsed = CompressionAlgorithm::parse(roundtrip).unwrap();
        assert_eq!(algo, reparsed, "roundtrip failed for {name}");
    }
}

#[test]
fn test_xxh_is_not_a_valid_name() {
    // upstream: checksum.c:49-65 valid_checksums_items - only "xxh64" and
    // "xxhash" name CSUM_XXH64; a bare "xxh" is unrecognised.
    assert!(ChecksumAlgorithm::parse("xxh").is_err());
}

#[test]
fn test_xxhash_alias() {
    // "xxhash" should parse to XXH64 (upstream: checksum.c valid_checksums_items)
    let algo = ChecksumAlgorithm::parse("xxhash").unwrap();
    assert_eq!(algo, ChecksumAlgorithm::XXH64);
}

#[test]
fn test_negotiate_proto29_uses_defaults() {
    let _env = default_env();

    let protocol = ProtocolVersion::try_from(29).unwrap();
    let mut stdin = &b""[..];
    let mut stdout = Vec::new();

    let result =
        negotiate_capabilities(protocol, &mut stdin, &mut stdout, true, true, false, true).unwrap();

    // Protocol < 30 should use defaults without any I/O
    assert_eq!(result.checksum, ChecksumAlgorithm::MD4);
    assert_eq!(result.compression, CompressionAlgorithm::Zlib);
    assert!(
        stdout.is_empty(),
        "no data should be sent for protocol < 30"
    );
}

#[test]
fn test_negotiate_proto30_md5_zlib() {
    let _env = default_env();

    let protocol = ProtocolVersion::try_from(30).unwrap();

    // Simulate remote choosing md5 and zlib
    // Format: vstring(len) + string, so len byte + "md5" + len byte + "zlib"
    let client_response = b"\x03md5\x04zlib";
    let mut stdin = &client_response[..];
    let mut stdout = Vec::new();

    let result =
        negotiate_capabilities(protocol, &mut stdin, &mut stdout, true, true, false, true).unwrap();

    assert_eq!(result.checksum, ChecksumAlgorithm::MD5);
    assert_eq!(result.compression, CompressionAlgorithm::Zlib);

    // Verify we sent our lists
    let output = String::from_utf8_lossy(&stdout);
    assert!(
        output.contains("md5"),
        "should send checksum list containing md5"
    );
    assert!(
        output.contains("zlib"),
        "should send compression list containing zlib"
    );
}

#[test]
fn test_negotiate_proto32_zlibx() {
    let _env = default_env();

    let protocol = ProtocolVersion::try_from(32).unwrap();

    // Remote sends only zlibx - we support it, so zlibx is selected.
    let client_response = b"\x03md5\x05zlibx";
    let mut stdin = &client_response[..];
    let mut stdout = Vec::new();

    let result =
        negotiate_capabilities(protocol, &mut stdin, &mut stdout, true, true, false, true).unwrap();

    assert_eq!(result.checksum, ChecksumAlgorithm::MD5);
    assert_eq!(result.compression, CompressionAlgorithm::ZlibX);
}

#[test]
fn test_negotiate_proto32_zlib() {
    let _env = default_env();

    let protocol = ProtocolVersion::try_from(32).unwrap();

    // Remote sends zlib - always supported regardless of feature flags.
    let client_response = b"\x03md5\x04zlib";
    let mut stdin = &client_response[..];
    let mut stdout = Vec::new();

    let result =
        negotiate_capabilities(protocol, &mut stdin, &mut stdout, true, true, false, true).unwrap();

    assert_eq!(result.checksum, ChecksumAlgorithm::MD5);
    assert_eq!(result.compression, CompressionAlgorithm::Zlib);
}

#[test]
fn test_vstring_roundtrip() {
    let test_str = "md5 md4 sha1 xxh128";
    let mut buffer = Vec::new();

    write_vstring(&mut buffer, test_str).unwrap();

    let mut reader = &buffer[..];
    let received = read_vstring(&mut reader).unwrap();

    assert_eq!(received, test_str);
}

#[test]
fn test_vstring_length_limit() {
    // Create a vstring that claims 10000 bytes (uses 2-byte format)
    // 10000 = 0x2710, so high byte = 0x27 | 0x80 = 0xA7, low byte = 0x10
    let mut buffer = vec![0xA7, 0x10];
    buffer.extend_from_slice(&[b'x'; 100]); // But only provide 100 bytes

    let mut reader = &buffer[..];
    let result = read_vstring(&mut reader);

    // Should fail because we can't read enough bytes
    assert!(result.is_err());
}

#[test]
fn test_vstring_max_nstr_strlen_limit_rejects_oversized() {
    // MAX_NSTR_STRLEN is 256 (upstream compat.c:91).
    // A string of 257 bytes must be rejected.
    let mut buffer = Vec::new();
    let oversized = "x".repeat(257);
    write_vstring(&mut buffer, &oversized).unwrap();

    let mut reader = &buffer[..];
    let result = read_vstring(&mut reader);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        err.to_string().contains("vstring too long"),
        "error message should mention vstring length: {}",
        err
    );
}

#[test]
fn test_vstring_max_nstr_strlen_limit_accepts_boundary() {
    // The largest accepted vstring is MAX_NSTR_STRLEN - 1 == 255 bytes:
    // upstream io.c:2181 rejects `len >= bufsize` (bufsize == 256), reserving
    // one byte for the NUL terminator.
    let boundary = "x".repeat(255);
    let mut buffer = Vec::new();
    write_vstring(&mut buffer, &boundary).unwrap();

    let mut reader = &buffer[..];
    let received = read_vstring(&mut reader).unwrap();
    assert_eq!(received, boundary);
}

#[test]
fn test_vstring_at_bufsize_is_rejected() {
    // len == MAX_NSTR_STRLEN (256) must be refused: upstream `len >= bufsize`.
    let at_limit = "x".repeat(256);
    let mut buffer = Vec::new();
    write_vstring(&mut buffer, &at_limit).unwrap();

    let mut reader = &buffer[..];
    let result = read_vstring(&mut reader);
    assert!(
        result.is_err(),
        "256-byte vstring must be rejected to match upstream `len >= bufsize`"
    );
}

/// Confirms `--debug=nstr` emits the exact wording used by upstream rsync
/// 3.4.1 (compat.c:373-378, 521-525, 866) for the client-side bidirectional
/// negotiation exchange.
#[test]
fn test_negotiate_nstr_messages_match_upstream_wording_client() {
    let _env = default_env();

    use logging::{DebugFlag, DiagnosticEvent, VerbosityConfig, drain_events, init};

    let mut cfg = VerbosityConfig::default();
    cfg.debug.nstr = 2;
    init(cfg);
    let _ = drain_events();

    let protocol = ProtocolVersion::try_from(32).unwrap();
    let client_response = b"\x03md5\x04zlib";
    let mut stdin = &client_response[..];
    let mut stdout = Vec::new();

    let _ = negotiate_capabilities(protocol, &mut stdin, &mut stdout, true, true, false, false)
        .unwrap();

    let messages: Vec<String> = drain_events()
        .into_iter()
        .filter_map(|event| match event {
            DiagnosticEvent::Debug {
                flag: DebugFlag::Nstr,
                message,
                ..
            } => Some(message),
            _ => None,
        })
        .collect();

    assert!(
        messages
            .iter()
            .any(|m| m.starts_with("Client checksum list (on client): ")),
        "missing upstream client send wording: {messages:?}"
    );
    assert!(
        messages
            .iter()
            .any(|m| m.starts_with("Client compress list (on client): ")),
        "missing upstream client compress send wording: {messages:?}"
    );
    assert!(
        messages
            .iter()
            .any(|m| m == "Server checksum list (on client): md5"),
        "missing upstream server recv wording: {messages:?}"
    );
    assert!(
        messages
            .iter()
            .any(|m| m == "Server compress list (on client): zlib"),
        "missing upstream server compress recv wording: {messages:?}"
    );
}

/// Confirms `--debug=nstr` level 1 emits upstream's per-side "negotiated"
/// summary (checksum.c:206-211, compat.c:213-219) using exact wording,
/// including the `(level <N>)` clause that upstream always renders for the
/// compress summary.
#[test]
fn test_negotiate_nstr_summary_matches_upstream_wording_client() {
    let _env = default_env();

    use logging::{DebugFlag, DiagnosticEvent, VerbosityConfig, drain_events, init};

    let mut cfg = VerbosityConfig::default();
    cfg.debug.nstr = 1;
    init(cfg);
    let _ = drain_events();

    let protocol = ProtocolVersion::try_from(32).unwrap();
    let client_response = b"\x03md5\x04zlib";
    let mut stdin = &client_response[..];
    let mut stdout = Vec::new();

    let _ = negotiate_capabilities(protocol, &mut stdin, &mut stdout, true, true, false, false)
        .unwrap();

    let messages: Vec<String> = drain_events()
        .into_iter()
        .filter_map(|event| match event {
            DiagnosticEvent::Debug {
                flag: DebugFlag::Nstr,
                message,
                ..
            } => Some(message),
            _ => None,
        })
        .collect();

    assert!(
        messages
            .iter()
            .any(|m| m == "Client negotiated checksum: md5"),
        "missing upstream level-1 checksum summary: {messages:?}"
    );
    // upstream: compat.c:215-218 - "(level %d)" renders the resolved
    // do_compression_level. parse_compress_choice(1) calls
    // init_compression_level() (token.c:55) first, which substitutes the zlib
    // def_level (6) for CLVL_NOT_SPECIFIED, so the raw INT_MIN sentinel is
    // never printed.
    assert!(
        messages
            .iter()
            .any(|m| m == "Client negotiated compress: zlib (level 6)"),
        "missing upstream level-1 compress summary: {messages:?}"
    );
}

/// At protocol 30+ without the negotiated 'v' capability
/// (`do_negotiation == false`), upstream still calls `parse_checksum_choice(1)`
/// / `parse_compress_choice(1)` (compat.c:819-820). Both emit the per-side
/// summary with NO " negotiated" qualifier (valid_*.negotiated_nni stays NULL
/// because no vstring exchange ran), and the compress level is resolved via
/// `init_compression_level` (token.c:55). The client must therefore show the
/// zlib fallback the wire actually uses at the resolved def_level (6) - not the
/// modern negotiated table and never the raw CLVL_NOT_SPECIFIED sentinel.
#[test]
fn test_no_negotiation_client_emits_resolved_fallback_summaries() {
    let _env = default_env();

    use logging::{DebugFlag, DiagnosticEvent, VerbosityConfig, drain_events, init};

    let mut cfg = VerbosityConfig::default();
    cfg.debug.nstr = 2;
    init(cfg);
    let _ = drain_events();

    // Protocol 30 uses binary negotiation, but with do_negotiation=false the
    // vstring exchange is skipped and no wire I/O occurs.
    let protocol = ProtocolVersion::try_from(30).unwrap();
    let mut stdin: &[u8] = &[];
    let mut stdout = Vec::new();

    let result = negotiate_capabilities_with_override(
        protocol,
        &mut stdin,
        &mut stdout,
        &NegotiationConfig {
            do_negotiation: false,
            send_compression: true,
            is_daemon_mode: false,
            is_server: false,
            checksum_override: None,
            compression_override: None,
            compression_level: crate::nstr::CLVL_NOT_SPECIFIED,
            write_batch: false,
        },
    )
    .unwrap();

    // Wire falls back to zlib with zero bytes exchanged.
    assert!(
        stdout.is_empty(),
        "do_negotiation=false must not send any negotiation bytes"
    );
    assert_eq!(result.compression, CompressionAlgorithm::Zlib);

    let messages: Vec<String> = drain_events()
        .into_iter()
        .filter_map(|event| match event {
            DiagnosticEvent::Debug {
                flag: DebugFlag::Nstr,
                message,
                ..
            } => Some(message),
            _ => None,
        })
        .collect();

    // Fallback summaries: no " negotiated" qualifier, zlib codec, def_level 6.
    assert!(
        messages.iter().any(|m| m == "Client checksum: md5"),
        "missing fallback checksum summary: {messages:?}"
    );
    assert!(
        messages
            .iter()
            .any(|m| m == "Client compress: zlib (level 6)"),
        "missing fallback compress summary: {messages:?}"
    );
    // Must NOT emit the modern negotiated table, the " negotiated" qualifier,
    // or the raw CLVL_NOT_SPECIFIED sentinel.
    assert!(
        !messages.iter().any(|m| m.contains("list (on")),
        "fallback path must not print the negotiated list table: {messages:?}"
    );
    assert!(
        !messages.iter().any(|m| m.contains("negotiated compress")),
        "fallback compress summary must omit ' negotiated': {messages:?}"
    );
    assert!(
        !messages.iter().any(|m| m.contains("-2147483648")),
        "fallback path must not leak the INT_MIN sentinel: {messages:?}"
    );
}

/// Confirms an explicit `--compress-level=N` renders `(level N)` in the
/// compress summary instead of the `CLVL_NOT_SPECIFIED` sentinel. Regression
/// for the previously hardcoded level argument. upstream: compat.c:215-218 -
/// `do_compression_level` is printed verbatim, so a user-supplied 9 must
/// render as `(level 9)`.
#[test]
fn test_negotiate_nstr_compress_summary_renders_explicit_level() {
    let _env = default_env();

    use logging::{DebugFlag, DiagnosticEvent, VerbosityConfig, drain_events, init};

    let mut cfg = VerbosityConfig::default();
    cfg.debug.nstr = 1;
    init(cfg);
    let _ = drain_events();

    let protocol = ProtocolVersion::try_from(32).unwrap();
    let client_response = b"\x03md5\x04zlib";
    let mut stdin = &client_response[..];
    let mut stdout = Vec::new();

    let _ = negotiate_capabilities_with_override(
        protocol,
        &mut stdin,
        &mut stdout,
        &NegotiationConfig {
            do_negotiation: true,
            send_compression: true,
            is_daemon_mode: false,
            is_server: false,
            checksum_override: None,
            compression_override: None,
            compression_level: 9,
            write_batch: false,
        },
    )
    .unwrap();

    let messages: Vec<String> = drain_events()
        .into_iter()
        .filter_map(|event| match event {
            DiagnosticEvent::Debug {
                flag: DebugFlag::Nstr,
                message,
                ..
            } => Some(message),
            _ => None,
        })
        .collect();

    assert!(
        messages
            .iter()
            .any(|m| m == "Client negotiated compress: zlib (level 9)"),
        "compress summary must render the explicit level: {messages:?}"
    );
}

/// Confirms `--checksum-choice` suppresses the `" negotiated"` qualifier
/// in the per-side summary, mirroring upstream's
/// `valid_checksums.negotiated_nni == NULL` branch (checksum.c:209).
#[test]
fn test_negotiate_nstr_summary_omits_negotiated_when_forced() {
    let _env = default_env();

    use logging::{DebugFlag, DiagnosticEvent, VerbosityConfig, drain_events, init};

    let mut cfg = VerbosityConfig::default();
    cfg.debug.nstr = 1;
    init(cfg);
    let _ = drain_events();

    let protocol = ProtocolVersion::try_from(32).unwrap();
    // With --checksum-choice forced, no checksum vstring is exchanged
    // (upstream compat.c:541 skips send, compat.c:547 skips recv). The peer
    // sends only the compression vstring.
    let client_response = b"\x04zlib";
    let mut stdin = &client_response[..];
    let mut stdout = Vec::new();

    let _ = negotiate_capabilities_with_override(
        protocol,
        &mut stdin,
        &mut stdout,
        &NegotiationConfig {
            do_negotiation: true,
            send_compression: true,
            is_daemon_mode: false,
            is_server: false,
            checksum_override: Some(ChecksumAlgorithm::MD5),
            compression_override: None,
            compression_level: crate::nstr::CLVL_NOT_SPECIFIED,
            write_batch: false,
        },
    )
    .unwrap();

    let messages: Vec<String> = drain_events()
        .into_iter()
        .filter_map(|event| match event {
            DiagnosticEvent::Debug {
                flag: DebugFlag::Nstr,
                message,
                ..
            } => Some(message),
            _ => None,
        })
        .collect();

    // upstream: checksum.c:209 - the " negotiated" qualifier renders only
    // when valid_checksums.negotiated_nni is set. --checksum-choice
    // bypasses negotiate_the_strings() (compat.c:175-187), so the
    // qualifier must render blank here.
    assert!(
        messages.iter().any(|m| m == "Client checksum: md5"),
        "missing forced-choice checksum summary: {messages:?}"
    );
    assert!(
        !messages
            .iter()
            .any(|m| m == "Client negotiated checksum: md5"),
        "forced-choice summary must not include ' negotiated': {messages:?}"
    );
}

/// Confirms an explicit `--checksum-choice=xxh128` override forces the
/// negotiated checksum to xxh128 regardless of automatic list selection.
/// upstream: checksum.c:178-184 - when `checksum_choice` is set,
/// `parse_checksum_choice` resolves the algorithm directly from the name
/// instead of `valid_checksums.negotiated_nni`.
#[test]
fn test_checksum_override_forces_chosen_algorithm() {
    let _env = default_env();

    let protocol = ProtocolVersion::try_from(32).unwrap();
    // A forced checksum skips the checksum vstring exchange entirely
    // (upstream compat.c:541/547). With compression also off, nothing is read
    // from the peer, so `stdin` is empty and `stdout` must carry no checksum
    // vstring.
    let mut stdin = &b""[..];
    let mut stdout = Vec::new();

    let result = negotiate_capabilities_with_override(
        protocol,
        &mut stdin,
        &mut stdout,
        &NegotiationConfig {
            do_negotiation: true,
            send_compression: false,
            is_daemon_mode: false,
            is_server: false,
            checksum_override: Some(ChecksumAlgorithm::XXH128),
            compression_override: None,
            compression_level: crate::nstr::CLVL_NOT_SPECIFIED,
            write_batch: false,
        },
    )
    .unwrap();

    assert_eq!(
        result.checksum,
        ChecksumAlgorithm::XXH128,
        "checksum_override=xxh128 must force the chosen algorithm"
    );
    assert!(
        stdout.is_empty(),
        "a forced checksum must not send a checksum vstring: {stdout:?}"
    );
}

#[test]
fn test_client_checksum_list_omits_none_matches_upstream() {
    let _env = default_env();

    // WHY: upstream get_default_nno_list drops the num == 0 ("none") entry on
    // the client (compat.c:485-486, `!am_server`). The advertised list is
    // framed as a vstring on the wire; an extra " none" token changes the
    // length prefix and payload bytes, diverging from upstream and risking an
    // interop desync at protocol >= 30. Assert the exact client checksum list.
    let protocol = ProtocolVersion::try_from(32).unwrap();
    let client_response = b"\x03md5\x04zlib";
    let mut stdin = &client_response[..];
    let mut stdout = Vec::new();

    // is_server = false (client side).
    negotiate_capabilities(protocol, &mut stdin, &mut stdout, true, true, false, false).unwrap();

    // Decode the first vstring the client emitted: the checksum list.
    let mut sent = &stdout[..];
    let checksum_list = read_vstring(&mut sent).unwrap();
    assert_eq!(checksum_list, "xxh128 xxh3 xxh64 md5 md4 sha1");
    assert!(
        !checksum_list.split(' ').any(|n| n == "none"),
        "client checksum list must omit none: {checksum_list}"
    );

    // The compression list follows and must also omit "none" on the client.
    let compression_list = read_vstring(&mut sent).unwrap();
    assert!(
        !compression_list.split(' ').any(|n| n == "none"),
        "client compression list must omit none: {compression_list}"
    );
}

#[test]
fn test_server_checksum_list_includes_none_matches_upstream() {
    let _env = default_env();

    // WHY: the server (am_server == 1) keeps the "none" entry
    // (compat.c:485-486), so its advertised checksum list ends with " none".
    // This is the complement of the client case and pins the exact server
    // wire bytes so both directions stay byte-identical to upstream.
    let protocol = ProtocolVersion::try_from(32).unwrap();
    let client_response = b"\x03md5\x04zlib";
    let mut stdin = &client_response[..];
    let mut stdout = Vec::new();

    // is_server = true (server side).
    negotiate_capabilities(protocol, &mut stdin, &mut stdout, true, true, false, true).unwrap();

    let mut sent = &stdout[..];
    let checksum_list = read_vstring(&mut sent).unwrap();
    assert_eq!(checksum_list, "xxh128 xxh3 xxh64 md5 md4 sha1 none");
}

#[test]
fn test_vstring_two_byte_format() {
    // Test vstring encoding for length > 127
    let test_str = "x".repeat(200); // 200 bytes > 127, needs 2-byte format
    let mut buffer = Vec::new();

    write_vstring(&mut buffer, &test_str).unwrap();

    // First byte should have high bit set (0xC8 = 0x80 | 0x00, second byte = 0xC8)
    // 200 = 0x00C8, so [0x80, 0xC8]
    assert_eq!(buffer[0], 0x80); // (200 >> 8) | 0x80 = 0 | 0x80 = 0x80
    assert_eq!(buffer[1], 0xC8); // 200 & 0xFF = 0xC8

    let mut reader = &buffer[..];
    let received = read_vstring(&mut reader).unwrap();
    assert_eq!(received, test_str);
}

#[test]
fn test_unsupported_checksum() {
    let result = ChecksumAlgorithm::parse("blake2");
    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("unsupported checksum algorithm")
    );
}

#[test]
fn test_unsupported_compression() {
    let result = CompressionAlgorithm::parse("bzip2");
    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("unsupported compression algorithm")
    );
}

#[test]
fn test_negotiate_do_negotiation_false_uses_defaults_no_io() {
    let _env = default_env();

    // When do_negotiation=false, should return defaults without any I/O
    // This happens when client lacks VARINT_FLIST_FLAGS capability
    let protocol = ProtocolVersion::try_from(31).unwrap();
    let mut stdin = &b""[..]; // Empty input - should not be read
    let mut stdout = Vec::new();

    let result = negotiate_capabilities(
        protocol,
        &mut stdin,
        &mut stdout,
        false, // do_negotiation = false
        true,  // send_compression
        false, // is_daemon_mode
        true,  // is_server
    )
    .unwrap();

    // Should use MD5 (protocol 30+ default) and Zlib when send_compression=true
    // upstream: compat.c:194 defaults to CPRES_ZLIB when -z active without negotiation
    assert_eq!(result.checksum, ChecksumAlgorithm::MD5);
    assert_eq!(result.compression, CompressionAlgorithm::Zlib);
    // No I/O should have occurred
    assert!(
        stdout.is_empty(),
        "no data should be sent when do_negotiation=false"
    );
}

#[test]
fn test_negotiate_compression_disabled() {
    let _env = default_env();

    // When send_compression=false, should only exchange checksum list
    let protocol = ProtocolVersion::try_from(31).unwrap();

    // Only provide checksum list, no compression list
    let client_response = b"\x03md5"; // Just "md5", no compression
    let mut stdin = &client_response[..];
    let mut stdout = Vec::new();

    let result = negotiate_capabilities(
        protocol,
        &mut stdin,
        &mut stdout,
        true,  // do_negotiation
        false, // send_compression = false
        false, // is_daemon_mode
        true,  // is_server
    )
    .unwrap();

    assert_eq!(result.checksum, ChecksumAlgorithm::MD5);
    // Compression should be None when not negotiated
    assert_eq!(result.compression, CompressionAlgorithm::None);

    // Should have sent checksum list but not compression list
    let output = String::from_utf8_lossy(&stdout);
    assert!(output.contains("md5"), "should send checksum list");
    // We can't easily verify compression wasn't sent without parsing,
    // but the test passing means stdin wasn't over-read
}

/// Helper to generate peer algorithm data for tests.
fn test_peer_data(send_compression: bool) -> Vec<u8> {
    let mut data = Vec::new();
    write_vstring(&mut data, &SUPPORTED_CHECKSUMS.join(" ")).unwrap();
    if send_compression {
        write_vstring(&mut data, &supported_compressions().join(" ")).unwrap();
    }
    data
}

#[test]
fn test_daemon_server_sends_and_reads() {
    let _env = default_env();

    // Both daemon server and client do bidirectional exchange
    let protocol = ProtocolVersion::try_from(31).unwrap();
    let peer_data = test_peer_data(true);
    let mut stdin = &peer_data[..];
    let mut stdout = Vec::new();

    let result = negotiate_capabilities(
        protocol,
        &mut stdin,
        &mut stdout,
        true, // do_negotiation
        true, // send_compression
        true, // is_daemon_mode = true
        true, // is_server = true
    )
    .unwrap();

    // Server selects from peer's list
    assert_eq!(result.checksum, ChecksumAlgorithm::XXH128);

    // Verify server sent data (bidirectional)
    assert!(!stdout.is_empty(), "server should send capability lists");
    let output = String::from_utf8_lossy(&stdout);
    assert!(
        output.contains("xxh128"),
        "should send checksum list with xxh128"
    );
}

#[test]
fn test_daemon_client_sends_and_reads() {
    let _env = default_env();

    // Client also sends its lists in bidirectional exchange
    let protocol = ProtocolVersion::try_from(31).unwrap();

    // Server sends "zlibx zlib none" - no zstd/lz4 offered by this server.
    let server_lists = b"\x0Exxh128 md5 md4\x0Fzlibx zlib none";
    let mut stdin = &server_lists[..];
    let mut stdout = Vec::new();

    let result = negotiate_capabilities(
        protocol,
        &mut stdin,
        &mut stdout,
        true,  // do_negotiation
        true,  // send_compression
        true,  // is_daemon_mode = true
        false, // is_server = false (client)
    )
    .unwrap();

    // Client should select first from server's list that it supports
    assert_eq!(result.checksum, ChecksumAlgorithm::XXH128);
    assert_eq!(result.compression, CompressionAlgorithm::ZlibX);

    // Client should also send (bidirectional)
    assert!(
        !stdout.is_empty(),
        "client should also send capability lists (bidirectional)"
    );
}

#[test]
fn test_daemon_mode_round_trip() {
    let _env = default_env();

    // Test that server output can be consumed by client
    let protocol = ProtocolVersion::try_from(31).unwrap();
    let peer_data = test_peer_data(true);

    // Step 1: Server sends and reads
    let mut server_stdin = &peer_data[..];
    let mut server_output = Vec::new();
    let server_result = negotiate_capabilities(
        protocol,
        &mut server_stdin,
        &mut server_output,
        true, // do_negotiation
        true, // send_compression
        true, // is_daemon_mode
        true, // is_server
    )
    .unwrap();

    // Step 2: Client reads server output, also sends
    let mut client_stdin = &server_output[..];
    let mut client_output = Vec::new();
    let client_result = negotiate_capabilities(
        protocol,
        &mut client_stdin,
        &mut client_output,
        true,  // do_negotiation
        true,  // send_compression
        true,  // is_daemon_mode
        false, // is_server = false (client)
    )
    .unwrap();

    // Both should select compatible algorithms
    assert_eq!(client_result.checksum, ChecksumAlgorithm::XXH128);
    assert_eq!(server_result.checksum, ChecksumAlgorithm::XXH128);

    // Client should also have sent data (bidirectional)
    assert!(!client_output.is_empty());
}

#[test]
fn test_daemon_server_without_compression() {
    let _env = default_env();

    // Server with compression disabled
    let protocol = ProtocolVersion::try_from(31).unwrap();
    let peer_data = test_peer_data(false);
    let mut stdin = &peer_data[..];
    let mut stdout = Vec::new();

    let result = negotiate_capabilities(
        protocol,
        &mut stdin,
        &mut stdout,
        true,  // do_negotiation
        false, // send_compression = false
        true,  // is_daemon_mode
        true,  // is_server
    )
    .unwrap();

    assert_eq!(result.checksum, ChecksumAlgorithm::XXH128);
    assert_eq!(result.compression, CompressionAlgorithm::None);

    // Should have sent checksum list only
    assert!(!stdout.is_empty());
}

#[test]
fn test_daemon_client_selects_fallback_algorithm() {
    let _env = default_env();

    // Client receives server list that doesn't include our top choices
    let protocol = ProtocolVersion::try_from(31).unwrap();

    // Server only offers md5 and zlib (not our top preferences)
    let server_lists = b"\x03md5\x04zlib";
    let mut stdin = &server_lists[..];
    let mut stdout = Vec::new();

    let result = negotiate_capabilities(
        protocol,
        &mut stdin,
        &mut stdout,
        true,  // do_negotiation
        true,  // send_compression
        true,  // is_daemon_mode
        false, // is_server = false
    )
    .unwrap();

    // Should fall back to md5 and zlib since that's what server offers
    assert_eq!(result.checksum, ChecksumAlgorithm::MD5);
    assert_eq!(result.compression, CompressionAlgorithm::Zlib);
    // Client also sends its lists (bidirectional)
    assert!(!stdout.is_empty());
}

#[test]
fn test_negotiate_ssh_mode_zlibx() {
    let _env = default_env();

    // SSH mode (is_daemon_mode=false) - bidirectional exchange.
    // Remote sends zlibx which we support.
    let protocol = ProtocolVersion::try_from(31).unwrap();
    let client_response = b"\x06xxh128\x05zlibx";
    let mut stdin = &client_response[..];
    let mut stdout = Vec::new();

    let result = negotiate_capabilities(
        protocol,
        &mut stdin,
        &mut stdout,
        true,  // do_negotiation
        true,  // send_compression
        false, // is_daemon_mode = false (SSH mode)
        true,  // is_server
    )
    .unwrap();

    assert_eq!(result.checksum, ChecksumAlgorithm::XXH128);
    assert_eq!(result.compression, CompressionAlgorithm::ZlibX);
}

#[test]
fn test_negotiate_ssh_mode_zlib() {
    let _env = default_env();

    // SSH mode (is_daemon_mode=false) - bidirectional exchange.
    // Remote sends zlib - always supported.
    let protocol = ProtocolVersion::try_from(31).unwrap();
    let client_response = b"\x06xxh128\x04zlib";
    let mut stdin = &client_response[..];
    let mut stdout = Vec::new();

    let result = negotiate_capabilities(
        protocol,
        &mut stdin,
        &mut stdout,
        true,  // do_negotiation
        true,  // send_compression
        false, // is_daemon_mode = false (SSH mode)
        true,  // is_server
    )
    .unwrap();

    assert_eq!(result.checksum, ChecksumAlgorithm::XXH128);
    assert_eq!(result.compression, CompressionAlgorithm::Zlib);
}

#[test]
fn test_choose_checksum_first_match_wins() {
    // When client sends multiple checksums, we pick the first one we support
    let client_list = "xxh128 xxh64 md5 md4";
    let result = choose_checksum_algorithm(client_list, true).unwrap();
    // xxh128 is first and we support it
    assert_eq!(result, ChecksumAlgorithm::XXH128);
}

#[test]
fn test_choose_checksum_fallback_to_later_match() {
    // If first item is unsupported, pick next supported one
    let client_list = "blake3 sha256 md5 md4";
    let result = choose_checksum_algorithm(client_list, true).unwrap();
    // blake3 and sha256 are not supported, md5 is
    assert_eq!(result, ChecksumAlgorithm::MD5);
}

#[test]
fn test_choose_checksum_empty_list() {
    // upstream: compat.c:383-406 - empty list is a negotiation failure (hard error)
    let result = choose_checksum_algorithm("", true);
    assert!(result.is_err(), "empty list should be a negotiation error");
}

#[test]
fn test_choose_compression_first_match_wins_zstd_or_zlib() {
    // Remote offers zstd and lz4 first, then zlib. Both zstd and lz4 are now in
    // our supported list (wire-format validated against upstream 3.4.4), so the
    // first mutually supported entry wins in preference order zstd > lz4 > zlib.
    let client_list = "zstd lz4 zlib none";
    let result = choose_compression_algorithm(client_list, true).unwrap();
    #[cfg(feature = "zstd")]
    assert_eq!(result, CompressionAlgorithm::Zstd);
    #[cfg(all(not(feature = "zstd"), feature = "lz4"))]
    assert_eq!(result, CompressionAlgorithm::LZ4);
    #[cfg(all(not(feature = "zstd"), not(feature = "lz4")))]
    assert_eq!(result, CompressionAlgorithm::Zlib);
}

#[test]
fn test_choose_compression_first_match_wins_zstd_or_zlibx() {
    // Remote offers zstd, lz4, then zlibx. zstd wins when enabled, otherwise lz4
    // (also validated), otherwise zlibx - preference zstd > lz4 > zlibx.
    let client_list = "zstd lz4 zlibx zlib none";
    let result = choose_compression_algorithm(client_list, true).unwrap();
    #[cfg(feature = "zstd")]
    assert_eq!(result, CompressionAlgorithm::Zstd);
    #[cfg(all(not(feature = "zstd"), feature = "lz4"))]
    assert_eq!(result, CompressionAlgorithm::LZ4);
    #[cfg(all(not(feature = "zstd"), not(feature = "lz4")))]
    assert_eq!(result, CompressionAlgorithm::ZlibX);
}

#[test]
#[cfg(feature = "lz4")]
fn test_lz4_is_advertised_in_preference_order() {
    // lz4 must appear in the advertised list (wire-format validated vs upstream
    // 3.4.4) positioned per upstream valid_compressions_items[]: after zstd,
    // before zlibx/zlib/none. The list is only sent when CF_VARINT_FLIST_FLAGS
    // (the `v` capability) is negotiated, matching upstream's proto-31+ gating.
    let list = supported_compressions();
    let lz4 = list
        .iter()
        .position(|&n| n == "lz4")
        .expect("lz4 advertised");
    let zlibx = list
        .iter()
        .position(|&n| n == "zlibx")
        .expect("zlibx present");
    assert!(lz4 < zlibx, "lz4 must precede zlibx: {list:?}");
    #[cfg(feature = "zstd")]
    {
        let zstd = list
            .iter()
            .position(|&n| n == "zstd")
            .expect("zstd present");
        assert!(zstd < lz4, "zstd must precede lz4: {list:?}");
    }
}

#[test]
fn test_choose_compression_empty_list() {
    // upstream: compat.c:381 `if (len > 0 && parse_negotiate_str(...))` - an
    // empty list never negotiates and falls into the RERR_UNSUPPORTED abort.
    let result = choose_compression_algorithm("", true);
    assert!(result.is_err(), "empty list should be a negotiation error");
}

#[test]
fn test_daemon_client_handles_empty_capabilities() {
    let _env = default_env();

    // Edge case: server sends empty capability lists.
    // Upstream recv_negotiate_str (compat.c:383-406) treats an empty list
    // as a hard error - no fallback to defaults.
    let protocol = ProtocolVersion::try_from(31).unwrap();
    let server_lists = b"\x00\x00"; // Two empty vstrings
    let mut stdin = &server_lists[..];
    let mut stdout = Vec::new();

    let result = negotiate_capabilities(
        protocol,
        &mut stdin,
        &mut stdout,
        true,  // do_negotiation
        true,  // send_compression
        true,  // is_daemon_mode
        false, // is_server = false
    );

    // Empty checksum list from server is a negotiation failure
    assert!(result.is_err());
    assert_eq!(result.unwrap_err().kind(), io::ErrorKind::Unsupported);
}

#[test]
fn test_daemon_client_handles_single_algorithm() {
    let _env = default_env();

    // Server offers only one checksum and compression option
    let protocol = ProtocolVersion::try_from(31).unwrap();
    let server_lists = b"\x03md4\x04zlib";
    let mut stdin = &server_lists[..];
    let mut stdout = Vec::new();

    let result = negotiate_capabilities(
        protocol,
        &mut stdin,
        &mut stdout,
        true,  // do_negotiation
        true,  // send_compression
        true,  // is_daemon_mode
        false, // is_server = false
    )
    .unwrap();

    assert_eq!(result.checksum, ChecksumAlgorithm::MD4);
    assert_eq!(result.compression, CompressionAlgorithm::Zlib);
    // Client also sends its lists (bidirectional)
    assert!(!stdout.is_empty());
}

#[test]
fn test_client_rejects_none_only_compression_from_server() {
    let _env = default_env();

    // upstream: compat.c:485-486 get_default_nno_list - the client skips the
    // zero-numbered "none" entry, so its saw[] cannot accept a server list
    // holding only "none"; recv_negotiate_str aborts with RERR_UNSUPPORTED.
    let protocol = ProtocolVersion::try_from(31).unwrap();
    let server_lists = b"\x03md4\x04none";
    let mut stdin = &server_lists[..];
    let mut stdout = Vec::new();

    let result = negotiate_capabilities(
        protocol,
        &mut stdin,
        &mut stdout,
        true,  // do_negotiation
        true,  // send_compression
        true,  // is_daemon_mode
        false, // is_server = false
    );

    let err = result.unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::Unsupported);
    // upstream: compat.c:381-405 - the client prints the offered lists. The
    // headline and the received `Server list:` are deterministic; the rebuilt
    // `Client list:` order depends on which optional codecs are compiled in, so
    // assert its presence rather than a feature-dependent tail.
    let msg = err.to_string();
    let mut lines = msg.lines();
    assert_eq!(
        lines.next().unwrap(),
        "Failed to negotiate a compress choice."
    );
    assert_eq!(lines.next().unwrap(), "Server list: none");
    assert!(
        lines.next().unwrap().starts_with("Client list: "),
        "missing Client list detail line: {msg:?}"
    );
}

#[test]
fn test_daemon_mode_malformed_input_error() {
    let _env = default_env();

    // Receives malformed vstring (claims more bytes than available)
    let protocol = ProtocolVersion::try_from(31).unwrap();
    let malformed = b"\x0Amd5"; // Claims 10 bytes but only provides 3
    let mut stdin = &malformed[..];
    let mut stdout = Vec::new();

    let result = negotiate_capabilities(
        protocol,
        &mut stdin,
        &mut stdout,
        true,  // do_negotiation
        true,  // send_compression
        true,  // is_daemon_mode
        false, // is_server = false
    );

    // Should fail with I/O error (sends first successfully, then fails reading)
    assert!(result.is_err());
}

#[test]
fn test_all_modes_are_bidirectional() {
    let _env = default_env();

    // Verify both SSH and daemon modes are bidirectional
    let protocol = ProtocolVersion::try_from(31).unwrap();

    let remote_lists = b"\x03md5\x04zlib";

    // SSH mode
    let mut stdin = &remote_lists[..];
    let mut stdout = Vec::new();
    let result = negotiate_capabilities(
        protocol,
        &mut stdin,
        &mut stdout,
        true,
        true,
        false,
        true, // SSH mode
    )
    .unwrap();
    assert_eq!(result.checksum, ChecksumAlgorithm::MD5);
    assert!(!stdout.is_empty(), "SSH mode should send");

    // Daemon mode (same behavior)
    let mut stdin = &remote_lists[..];
    let mut stdout = Vec::new();
    let result = negotiate_capabilities(
        protocol,
        &mut stdin,
        &mut stdout,
        true,
        true,
        true,
        true, // Daemon mode
    )
    .unwrap();
    assert_eq!(result.checksum, ChecksumAlgorithm::MD5);
    assert!(!stdout.is_empty(), "Daemon mode should also send");
}

#[test]
fn test_daemon_server_selects_from_peer_list() {
    let _env = default_env();

    // Server selects from peer's algorithm list
    let protocol = ProtocolVersion::try_from(31).unwrap();
    let peer_data = test_peer_data(true);
    let mut stdin = &peer_data[..];
    let mut stdout = Vec::new();

    let result = negotiate_capabilities(
        protocol,
        &mut stdin,
        &mut stdout,
        true, // do_negotiation
        true, // send_compression
        true, // is_daemon_mode
        true, // is_server
    )
    .unwrap();

    // Should select first in peer's list that we support
    assert_eq!(result.checksum, ChecksumAlgorithm::XXH128);
}

#[test]
fn test_daemon_client_prefers_server_order() {
    let _env = default_env();

    // Client should prefer server's order (first match)
    let protocol = ProtocolVersion::try_from(31).unwrap();

    // Server prefers md5 over xxh128 (opposite of our preference)
    let server_lists = b"\x07md5 md4\x09zlib none";
    let mut stdin = &server_lists[..];
    let mut stdout = Vec::new();

    let result = negotiate_capabilities(
        protocol,
        &mut stdin,
        &mut stdout,
        true,  // do_negotiation
        true,  // send_compression
        true,  // is_daemon_mode
        false, // is_server = false
    )
    .unwrap();

    // Should pick md5 (server's first) even though xxh128 is our preference
    assert_eq!(result.checksum, ChecksumAlgorithm::MD5);
    assert_eq!(result.compression, CompressionAlgorithm::Zlib);
    // Client also sends its lists (bidirectional)
    assert!(!stdout.is_empty());
}

#[test]
fn test_daemon_mode_respects_do_negotiation_false() {
    let _env = default_env();

    // When do_negotiation=false, daemon mode should also return defaults without I/O
    let protocol = ProtocolVersion::try_from(31).unwrap();
    let mut stdin = &b""[..];
    let mut stdout = Vec::new();

    let result = negotiate_capabilities(
        protocol,
        &mut stdin,
        &mut stdout,
        false, // do_negotiation = false
        true,  // send_compression
        true,  // is_daemon_mode
        true,  // is_server
    )
    .unwrap();

    // Should use MD5/Zlib defaults without I/O (send_compression=true)
    assert_eq!(result.checksum, ChecksumAlgorithm::MD5);
    assert_eq!(result.compression, CompressionAlgorithm::Zlib);
    assert!(stdout.is_empty(), "no I/O when do_negotiation=false");
}

#[test]
fn test_upstream_checksum_list_format() {
    // Upstream rsync 3.4.1 sends checksums in this format:
    // "xxh128 xxh3 xxh64 md5 md4 sha1 none"
    let upstream_format = "xxh128 xxh3 xxh64 md5 md4 sha1 none";
    let result = choose_checksum_algorithm(upstream_format, true).unwrap();
    // First match should be xxh128
    assert_eq!(result, ChecksumAlgorithm::XXH128);
}

#[test]
fn test_legacy_rsync_checksum_list() {
    // Older rsync might only offer md4 and md5
    let legacy_format = "md5 md4";
    let result = choose_checksum_algorithm(legacy_format, true).unwrap();
    assert_eq!(result, ChecksumAlgorithm::MD5);
}

#[test]
fn test_minimal_rsync_checksum_list() {
    // Minimal rsync might only offer none
    let minimal_format = "none";
    let result = choose_checksum_algorithm(minimal_format, true).unwrap();
    assert_eq!(result, ChecksumAlgorithm::None);
}

#[test]
fn test_checksum_case_sensitive() {
    // Algorithm names are case-sensitive
    assert!(ChecksumAlgorithm::parse("MD5").is_err());
    assert!(ChecksumAlgorithm::parse("Md5").is_err());
    assert!(ChecksumAlgorithm::parse("md5").is_ok());
}

#[test]
fn test_compression_case_sensitive() {
    assert!(CompressionAlgorithm::parse("ZLIB").is_err());
    assert!(CompressionAlgorithm::parse("Zlib").is_err());
    assert!(CompressionAlgorithm::parse("zlib").is_ok());
}

#[test]
fn test_checksum_with_whitespace() {
    // Lists can have multiple spaces between items
    let list = "md5   md4     sha1";
    let result = choose_checksum_algorithm(list, true).unwrap();
    assert_eq!(result, ChecksumAlgorithm::MD5);
}

#[test]
fn test_compression_with_leading_trailing_space() {
    // split_whitespace handles leading/trailing spaces
    let list = "  zlib   zlibx  none  ";
    let result = choose_compression_algorithm(list, true).unwrap();
    assert_eq!(result, CompressionAlgorithm::Zlib);
}

#[test]
fn test_negotiation_result_debug() {
    let result = NegotiationResult {
        checksum: ChecksumAlgorithm::MD5,
        compression: CompressionAlgorithm::Zlib,
    };
    let debug = format!("{:?}", result);
    assert!(debug.contains("MD5"));
    assert!(debug.contains("Zlib"));
}

#[test]
fn test_negotiation_result_equality() {
    let r1 = NegotiationResult {
        checksum: ChecksumAlgorithm::XXH128,
        compression: CompressionAlgorithm::None,
    };
    let r2 = NegotiationResult {
        checksum: ChecksumAlgorithm::XXH128,
        compression: CompressionAlgorithm::None,
    };
    let r3 = NegotiationResult {
        checksum: ChecksumAlgorithm::MD5,
        compression: CompressionAlgorithm::None,
    };
    assert_eq!(r1, r2);
    assert_ne!(r1, r3);
}

#[test]
fn test_negotiation_result_clone() {
    let r1 = NegotiationResult {
        checksum: ChecksumAlgorithm::SHA1,
        compression: CompressionAlgorithm::ZlibX,
    };
    let r2 = r1;
    assert_eq!(r1.checksum, r2.checksum);
    assert_eq!(r1.compression, r2.compression);
}

#[test]
fn test_vstring_empty_string() {
    let mut buffer = Vec::new();
    write_vstring(&mut buffer, "").unwrap();
    assert_eq!(buffer, vec![0x00]); // Length 0

    let mut reader = &buffer[..];
    let received = read_vstring(&mut reader).unwrap();
    assert_eq!(received, "");
}

#[test]
fn test_vstring_single_byte_boundary() {
    // Length 127 should use single-byte format
    let test_str = "x".repeat(127);
    let mut buffer = Vec::new();
    write_vstring(&mut buffer, &test_str).unwrap();
    assert_eq!(buffer[0], 127); // Single byte length
    assert_eq!(buffer.len(), 1 + 127);
}

#[test]
fn test_vstring_two_byte_boundary() {
    // Length 128 should use two-byte format
    let test_str = "x".repeat(128);
    let mut buffer = Vec::new();
    write_vstring(&mut buffer, &test_str).unwrap();
    assert!(buffer[0] & 0x80 != 0); // Two-byte format indicator
    assert_eq!(buffer.len(), 2 + 128);
}

#[test]
fn test_vstring_max_single_byte() {
    // Maximum single-byte length is 127
    let test_str = "y".repeat(127);
    let mut buffer = Vec::new();
    write_vstring(&mut buffer, &test_str).unwrap();

    let mut reader = &buffer[..];
    let received = read_vstring(&mut reader).unwrap();
    assert_eq!(received, test_str);
}

#[test]
fn test_vstring_moderate_length() {
    // Test a moderate length that uses 2-byte format (within 256 limit)
    let test_str = "z".repeat(200);
    let mut buffer = Vec::new();
    write_vstring(&mut buffer, &test_str).unwrap();

    let mut reader = &buffer[..];
    let received = read_vstring(&mut reader).unwrap();
    assert_eq!(received, test_str);
}

#[test]
fn test_all_supported_versions_negotiate() {
    let _env = default_env();

    for version_num in 28..=32 {
        let protocol = ProtocolVersion::try_from(version_num).unwrap();

        if protocol.uses_fixed_encoding() {
            // Protocol < 30 uses defaults
            let mut stdin = &b""[..];
            let mut stdout = Vec::new();
            let result =
                negotiate_capabilities(protocol, &mut stdin, &mut stdout, true, true, false, true)
                    .unwrap();
            assert_eq!(result.checksum, ChecksumAlgorithm::MD4);
            assert_eq!(result.compression, CompressionAlgorithm::Zlib);
        } else {
            // Protocol >= 30 exchanges lists
            let client_response = b"\x03md5\x04zlib";
            let mut stdin = &client_response[..];
            let mut stdout = Vec::new();
            let result =
                negotiate_capabilities(protocol, &mut stdin, &mut stdout, true, true, false, true)
                    .unwrap();
            assert_eq!(result.checksum, ChecksumAlgorithm::MD5);
            assert_eq!(result.compression, CompressionAlgorithm::Zlib);
        }
    }
}

#[test]
fn test_v28_uses_legacy_defaults() {
    let _env = default_env();

    let protocol = ProtocolVersion::try_from(28).unwrap();
    let mut stdin = &b""[..];
    let mut stdout = Vec::new();

    let result =
        negotiate_capabilities(protocol, &mut stdin, &mut stdout, true, true, false, true).unwrap();

    assert_eq!(result.checksum, ChecksumAlgorithm::MD4);
    assert_eq!(result.compression, CompressionAlgorithm::Zlib);
    assert!(stdout.is_empty());
}

#[test]
fn test_v29_uses_legacy_defaults() {
    let _env = default_env();

    let protocol = ProtocolVersion::try_from(29).unwrap();
    let mut stdin = &b""[..];
    let mut stdout = Vec::new();

    let result =
        negotiate_capabilities(protocol, &mut stdin, &mut stdout, true, true, false, true).unwrap();

    assert_eq!(result.checksum, ChecksumAlgorithm::MD4);
    assert_eq!(result.compression, CompressionAlgorithm::Zlib);
    assert!(stdout.is_empty());
}

#[test]
fn test_v30_requires_exchange() {
    let _env = default_env();

    let protocol = ProtocolVersion::try_from(30).unwrap();
    let client_response = b"\x05xxh64\x05zlibx";
    let mut stdin = &client_response[..];
    let mut stdout = Vec::new();

    let result =
        negotiate_capabilities(protocol, &mut stdin, &mut stdout, true, true, false, true).unwrap();

    assert_eq!(result.checksum, ChecksumAlgorithm::XXH64);
    assert_eq!(result.compression, CompressionAlgorithm::ZlibX);
    assert!(!stdout.is_empty()); // Should have sent our lists
}
//
// The vstring format uses a simple length-prefixed encoding:
// - For lengths 0-127: single byte = length (high bit clear)
// - For lengths 128-32767: two bytes = [(len >> 8) | 0x80, len & 0xFF]
//
// These tests verify the 1-byte length format for strings up to 127 bytes.

/// Tests that empty string uses 1-byte length format.
#[test]
fn phase2_10_vstring_1byte_empty_string() {
    let mut buffer = Vec::new();
    write_vstring(&mut buffer, "").unwrap();

    // Should be: 1 length byte (0x00) + 0 data bytes = 1 byte total
    assert_eq!(buffer.len(), 1);
    assert_eq!(buffer[0], 0x00);

    let mut reader = &buffer[..];
    let received = read_vstring(&mut reader).unwrap();
    assert_eq!(received, "");
}

/// Tests that single-character string uses 1-byte length format.
#[test]
fn phase2_10_vstring_1byte_single_char() {
    let mut buffer = Vec::new();
    write_vstring(&mut buffer, "x").unwrap();

    // Should be: 1 length byte (0x01) + 1 data byte = 2 bytes total
    assert_eq!(buffer.len(), 2);
    assert_eq!(buffer[0], 0x01);
    assert_eq!(buffer[1], b'x');

    let mut reader = &buffer[..];
    let received = read_vstring(&mut reader).unwrap();
    assert_eq!(received, "x");
}

/// Tests all 1-byte length values (0-127).
#[test]
fn phase2_10_vstring_1byte_all_lengths() {
    for len in 0..=127usize {
        let test_str = "a".repeat(len);
        let mut buffer = Vec::new();
        write_vstring(&mut buffer, &test_str).unwrap();

        // Should use 1-byte length format
        assert_eq!(
            buffer[0], len as u8,
            "length {} should encode as single byte",
            len
        );
        assert_eq!(buffer.len(), 1 + len, "total size should be 1 + {}", len);
        // High bit should be clear
        assert!(
            buffer[0] & 0x80 == 0,
            "high bit should be clear for length {}",
            len
        );

        let mut reader = &buffer[..];
        let received = read_vstring(&mut reader).unwrap();
        assert_eq!(received, test_str, "round-trip failed for length {}", len);
    }
}

/// Tests boundary at 127 (maximum 1-byte length).
#[test]
fn phase2_10_vstring_1byte_boundary_127() {
    let test_str = "b".repeat(127);
    let mut buffer = Vec::new();
    write_vstring(&mut buffer, &test_str).unwrap();

    // Should use single-byte format: 0x7F
    assert_eq!(buffer[0], 0x7F);
    assert!(buffer[0] & 0x80 == 0, "high bit should be clear");
    assert_eq!(buffer.len(), 128); // 1 + 127

    let mut reader = &buffer[..];
    let received = read_vstring(&mut reader).unwrap();
    assert_eq!(received, test_str);
}

/// Tests that raw 1-byte length sequences decode correctly.
#[test]
fn phase2_10_vstring_1byte_decode_raw() {
    // Test decoding raw bytes: length byte + content
    for len in 0u8..=127 {
        let mut data = vec![len];
        data.extend(vec![b'x'; len as usize]);

        let mut reader = &data[..];
        let received = read_vstring(&mut reader).unwrap();
        assert_eq!(received.len(), len as usize);
        assert!(received.chars().all(|c| c == 'x'));
    }
}

/// Tests typical algorithm names (all use 1-byte format).
#[test]
fn phase2_10_vstring_1byte_algorithm_names() {
    let names = [
        "md4", "md5", "sha1", "xxh64", "xxh128", "zlib", "zlibx", "zstd", "lz4", "none",
    ];
    for name in names {
        assert!(
            name.len() <= 127,
            "algorithm name should fit in 1-byte format"
        );

        let mut buffer = Vec::new();
        write_vstring(&mut buffer, name).unwrap();

        // Verify 1-byte format
        assert_eq!(buffer[0], name.len() as u8);
        assert!(buffer[0] & 0x80 == 0);

        let mut reader = &buffer[..];
        let received = read_vstring(&mut reader).unwrap();
        assert_eq!(received, name);
    }
}

/// Tests typical space-separated algorithm lists (1-byte format).
#[test]
fn phase2_10_vstring_1byte_algorithm_lists() {
    let lists = [
        "md5 md4 sha1",
        "xxh128 xxh3 xxh64 md5 md4 sha1 none",
        "zstd lz4 zlibx zlib none",
    ];
    for list in lists {
        assert!(list.len() <= 127, "list should fit in 1-byte format");

        let mut buffer = Vec::new();
        write_vstring(&mut buffer, list).unwrap();

        // Verify 1-byte format
        assert_eq!(buffer[0], list.len() as u8);
        assert!(buffer[0] & 0x80 == 0);

        let mut reader = &buffer[..];
        let received = read_vstring(&mut reader).unwrap();
        assert_eq!(received, list);
    }
}
//
// For lengths 128-32767, vstring uses a 2-byte length format:
// - First byte: (len >> 8) | 0x80 (high bit indicates 2-byte format)
// - Second byte: len & 0xFF
//
// This allows encoding strings up to 32767 bytes.

/// Tests boundary at 128 (minimum 2-byte length).
#[test]
fn phase2_11_vstring_2byte_boundary_128() {
    let test_str = "c".repeat(128);
    let mut buffer = Vec::new();
    write_vstring(&mut buffer, &test_str).unwrap();

    // Should use 2-byte format: [0x80, 0x80] for length 128
    // 128 = 0x0080, so high byte = 0x00 | 0x80 = 0x80, low byte = 0x80
    assert_eq!(buffer[0], 0x80);
    assert_eq!(buffer[1], 0x80);
    assert!(
        buffer[0] & 0x80 != 0,
        "high bit should be set for 2-byte format"
    );
    assert_eq!(buffer.len(), 2 + 128); // 2 length bytes + 128 data bytes

    let mut reader = &buffer[..];
    let received = read_vstring(&mut reader).unwrap();
    assert_eq!(received, test_str);
}

/// Tests value 200 (clear case of 2-byte format).
#[test]
fn phase2_11_vstring_2byte_length_200() {
    let test_str = "d".repeat(200);
    let mut buffer = Vec::new();
    write_vstring(&mut buffer, &test_str).unwrap();

    // 200 = 0x00C8, so high byte = 0x00 | 0x80 = 0x80, low byte = 0xC8
    assert_eq!(buffer[0], 0x80);
    assert_eq!(buffer[1], 0xC8);
    assert_eq!(buffer.len(), 2 + 200);

    let mut reader = &buffer[..];
    let received = read_vstring(&mut reader).unwrap();
    assert_eq!(received, test_str);
}

/// Tests value 255 (boundary within first 256-byte range).
#[test]
fn phase2_11_vstring_2byte_length_255() {
    let test_str = "e".repeat(255);
    let mut buffer = Vec::new();
    write_vstring(&mut buffer, &test_str).unwrap();

    // 255 = 0x00FF, so high byte = 0x00 | 0x80 = 0x80, low byte = 0xFF
    assert_eq!(buffer[0], 0x80);
    assert_eq!(buffer[1], 0xFF);

    let mut reader = &buffer[..];
    let received = read_vstring(&mut reader).unwrap();
    assert_eq!(received, test_str);
}

/// Tests value 256 (crosses into second high byte).
///
/// The write side must still emit the 2-byte header `0x81 0x00`, but the read
/// side rejects it: upstream io.c:2181 refuses `len >= bufsize` (256).
#[test]
fn phase2_11_vstring_2byte_length_256() {
    let test_str = "f".repeat(256);
    let mut buffer = Vec::new();
    write_vstring(&mut buffer, &test_str).unwrap();

    // 256 = 0x0100, so high byte = 0x01 | 0x80 = 0x81, low byte = 0x00
    assert_eq!(buffer[0], 0x81);
    assert_eq!(buffer[1], 0x00);

    let mut reader = &buffer[..];
    let result = read_vstring(&mut reader);
    assert!(
        result.is_err(),
        "256-byte vstring must be rejected (upstream `len >= bufsize`)"
    );
}

/// Tests sample values across the 2-byte range.
#[test]
fn phase2_11_vstring_2byte_sample_values() {
    // Values that round-trip use the 2-byte format and stay within the
    // accepted range (<= MAX_NSTR_STRLEN - 1 == 255). 256 is write-only since
    // read rejects it (upstream `len >= bufsize`).
    let lengths = [128, 200, 255];
    for len in lengths {
        let test_str = "g".repeat(len);
        let mut buffer = Vec::new();
        write_vstring(&mut buffer, &test_str).unwrap();

        // Verify 2-byte format
        assert!(
            buffer[0] & 0x80 != 0,
            "high bit should be set for length {}",
            len
        );

        // Verify encoding: len = ((buffer[0] & 0x7F) << 8) | buffer[1]
        let decoded_len = ((buffer[0] & 0x7F) as usize) * 256 + buffer[1] as usize;
        assert_eq!(decoded_len, len, "length encoding mismatch for {}", len);

        let mut reader = &buffer[..];
        let received = read_vstring(&mut reader).unwrap();
        assert_eq!(received, test_str, "round-trip failed for length {}", len);
    }
}

/// Tests decoding raw 2-byte length sequences.
#[test]
fn phase2_11_vstring_2byte_decode_raw() {
    // Test specific 2-byte encoded lengths (all within the accepted range,
    // <= MAX_NSTR_STRLEN - 1 == 255).
    let cases = [
        (128, 0x80u8, 0x80u8), // 128 = 0x0080
        (200, 0x80, 0xC8),     // 200 = 0x00C8
        (255, 0x80, 0xFF),     // 255 = 0x00FF (largest accepted)
    ];

    for (len, high, low) in cases {
        let mut data = vec![high, low];
        data.extend(vec![b'x'; len]);

        let mut reader = &data[..];
        let received = read_vstring(&mut reader).unwrap();
        assert_eq!(received.len(), len, "decode failed for length {}", len);
    }
}

/// Tests truncated 2-byte length (only high byte present).
#[test]
fn phase2_11_vstring_2byte_truncated_length() {
    // Only the high byte, missing the low byte
    let data = [0x80u8];
    let mut reader = &data[..];
    let result = read_vstring(&mut reader);
    assert!(result.is_err(), "should fail on truncated 2-byte length");
}

/// Tests truncated 2-byte vstring (length present but data truncated).
#[test]
fn phase2_11_vstring_2byte_truncated_data() {
    // Length says 200 bytes, but only 50 provided
    let mut data = vec![0x80, 0xC8]; // Length 200
    data.extend(vec![b'x'; 50]); // Only 50 bytes

    let mut reader = &data[..];
    let result = read_vstring(&mut reader);
    assert!(result.is_err(), "should fail on truncated data");
}

/// Tests multiple 2-byte vstrings in sequence.
#[test]
fn phase2_11_vstring_2byte_multiple_in_sequence() {
    let strings = ["h".repeat(128), "i".repeat(200), "j".repeat(250)];
    let mut buffer = Vec::new();

    for s in &strings {
        write_vstring(&mut buffer, s).unwrap();
    }

    let mut reader = &buffer[..];
    for expected in &strings {
        let received = read_vstring(&mut reader).unwrap();
        assert_eq!(received, *expected);
    }
}
//
// The vstring format has limits:
// - Maximum encodable length: 0x7FFF (32767 bytes)
// - Wire/sanity limit in read_vstring: 256 bytes (MAX_NSTR_STRLEN, upstream compat.c:91)
//
// These tests verify boundary conditions and error handling.

/// Tests maximum encodable length (0x7FFF = 32767).
#[test]
fn phase2_12_vstring_max_encodable_length() {
    let test_str = "k".repeat(0x7FFF);
    let mut buffer = Vec::new();
    write_vstring(&mut buffer, &test_str).unwrap();

    // 0x7FFF encoded as [0xFF, 0xFF] (7F | 80 = FF, FF)
    assert_eq!(buffer[0], 0xFF);
    assert_eq!(buffer[1], 0xFF);

    // Verify round-trip (note: exceeds sanity limit, so read will fail)
    // This test specifically verifies the ENCODING works for max length
    assert_eq!(buffer.len(), 2 + 0x7FFF);
}

/// Tests that encoding length > 0x7FFF fails.
#[test]
fn phase2_12_vstring_exceeds_max_encodable() {
    let test_str = "l".repeat(0x8000); // 32768 bytes
    let mut buffer = Vec::new();
    let result = write_vstring(&mut buffer, &test_str);

    assert!(result.is_err(), "should reject strings > 0x7FFF bytes");
    let err = result.unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    assert!(err.to_string().contains("vstring too long"));
}

/// Tests sanity limit in read_vstring (MAX_NSTR_STRLEN = 256 bytes).
#[test]
fn phase2_12_vstring_sanity_limit_exceeded() {
    // Encode a length of 10000 (exceeds MAX_NSTR_STRLEN = 256)
    // 10000 = 0x2710, so high byte = 0x27 | 0x80 = 0xA7, low byte = 0x10
    let data = [0xA7u8, 0x10];

    let mut reader = &data[..];
    let result = read_vstring(&mut reader);

    assert!(
        result.is_err(),
        "should reject vstrings > MAX_NSTR_STRLEN bytes"
    );
    let err = result.unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    assert!(err.to_string().contains("vstring too long"));
}

/// Tests exactly at the upstream reject boundary (len == MAX_NSTR_STRLEN = 256).
///
/// upstream: io.c:2181 `if (len >= bufsize)` with `bufsize == MAX_NSTR_STRLEN`
/// (256, compat.c:99) - upstream reserves one byte for the NUL terminator, so a
/// 256-byte payload is refused with `RERR_UNSUPPORTED`. oc-rsync must reject it
/// too; accepting it would desync negotiation against a real rsync peer.
#[test]
fn phase2_12_vstring_at_sanity_limit_rejected() {
    let test_str = "m".repeat(256);
    let mut buffer = Vec::new();
    write_vstring(&mut buffer, &test_str).unwrap();

    let mut reader = &buffer[..];
    let result = read_vstring(&mut reader);
    assert!(
        result.is_err(),
        "len == MAX_NSTR_STRLEN (256) must be rejected to match upstream `len >= bufsize`"
    );
    let err = result.unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    assert!(err.to_string().contains("vstring too long"));
}

/// Tests just below sanity limit (MAX_NSTR_STRLEN - 1 = 255 bytes).
#[test]
fn phase2_12_vstring_below_sanity_limit() {
    let test_str = "n".repeat(255);
    let mut buffer = Vec::new();
    write_vstring(&mut buffer, &test_str).unwrap();

    let mut reader = &buffer[..];
    let received = read_vstring(&mut reader).unwrap();
    assert_eq!(received, test_str);
}

/// Tests just above sanity limit (MAX_NSTR_STRLEN + 1 = 257 bytes).
#[test]
fn phase2_12_vstring_above_sanity_limit() {
    // Write succeeds (max is 0x7FFF)
    let test_str = "o".repeat(257);
    let mut buffer = Vec::new();
    write_vstring(&mut buffer, &test_str).unwrap();

    // Read fails (MAX_NSTR_STRLEN = 256)
    let mut reader = &buffer[..];
    let result = read_vstring(&mut reader);
    assert!(
        result.is_err(),
        "should reject vstrings > MAX_NSTR_STRLEN bytes"
    );
}

/// Tests the largest accepted vstring (MAX_NSTR_STRLEN - 1 = 255 bytes).
///
/// upstream: io.c:2181 `if (len >= bufsize)` with `bufsize == 256` accepts up to
/// 255 data bytes (one byte reserved for the NUL terminator). This is the
/// boundary a real rsync peer will round-trip, so oc-rsync must accept it.
#[test]
fn phase2_12_vstring_upstream_max_accepted() {
    // upstream: compat.c:99 #define MAX_NSTR_STRLEN 256 -> max data len 255.
    let test_str = "p".repeat(255);
    let mut buffer = Vec::new();
    write_vstring(&mut buffer, &test_str).unwrap();

    let mut reader = &buffer[..];
    let received = read_vstring(&mut reader).unwrap();
    assert_eq!(received, test_str);
}

/// Tests empty input (EOF) handling.
#[test]
fn phase2_12_vstring_empty_input() {
    let data: [u8; 0] = [];
    let mut reader = &data[..];
    let result = read_vstring(&mut reader);

    assert!(result.is_err(), "should fail on empty input");
    assert_eq!(result.unwrap_err().kind(), io::ErrorKind::UnexpectedEof);
}

/// Tests UTF-8 validation (algorithm names are ASCII but we validate).
#[test]
fn phase2_12_vstring_invalid_utf8() {
    // Create a vstring with invalid UTF-8 bytes
    let mut data = vec![0x03]; // Length 3
    data.extend([0xFF, 0xFE, 0x80]); // Invalid UTF-8 sequence

    let mut reader = &data[..];
    let result = read_vstring(&mut reader);

    assert!(result.is_err(), "should reject invalid UTF-8");
    let err = result.unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    assert!(err.to_string().contains("UTF-8"));
}

/// Tests various boundary values around encoding transitions.
#[test]
fn phase2_12_vstring_encoding_transitions() {
    let boundaries = [
        0,   // Minimum
        1,   // Single char
        127, // Max 1-byte
        128, // Min 2-byte
        255, // Max accepted (MAX_NSTR_STRLEN - 1); upstream `len >= bufsize`.
    ];

    for len in boundaries {
        let test_str = "q".repeat(len);
        let mut buffer = Vec::new();
        write_vstring(&mut buffer, &test_str).unwrap();

        let mut reader = &buffer[..];
        let received = read_vstring(&mut reader).unwrap();
        assert_eq!(received.len(), len, "round-trip failed for length {}", len);
    }
}

/// Tests that write_vstring properly handles boundary between 1 and 2 byte formats.
#[test]
fn phase2_12_vstring_format_boundary_exact() {
    // 127 bytes should use 1-byte format
    let s127 = "r".repeat(127);
    let mut buf127 = Vec::new();
    write_vstring(&mut buf127, &s127).unwrap();
    assert!(buf127[0] & 0x80 == 0, "127 should use 1-byte format");

    // 128 bytes should use 2-byte format
    let s128 = "s".repeat(128);
    let mut buf128 = Vec::new();
    write_vstring(&mut buf128, &s128).unwrap();
    assert!(buf128[0] & 0x80 != 0, "128 should use 2-byte format");
}

/// Tests maximum practical negotiation string from upstream.
#[test]
fn phase2_12_vstring_realistic_max_negotiation() {
    // Realistic maximum: all supported checksums + compressions
    // "xxh128 xxh3 xxh64 md5 md4 sha1 none" = 37 chars
    // "zstd lz4 zlibx zlib none" = 24 chars
    // Well under both limits
    let checksum_list = "xxh128 xxh3 xxh64 md5 md4 sha1 none";
    let compression_list = "zstd lz4 zlibx zlib none";

    for list in [checksum_list, compression_list] {
        let mut buffer = Vec::new();
        write_vstring(&mut buffer, list).unwrap();

        // All realistic lists should fit in 1-byte format
        assert!(
            buffer[0] & 0x80 == 0,
            "realistic list should use 1-byte format"
        );

        let mut reader = &buffer[..];
        let received = read_vstring(&mut reader).unwrap();
        assert_eq!(received, list);
    }
}

#[test]
fn phase3_write_vstring_io_error() {
    struct FailWriter;
    impl std::io::Write for FailWriter {
        fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
            Err(io::Error::new(io::ErrorKind::BrokenPipe, "write failed"))
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    let result = write_vstring(&mut FailWriter, "test");
    assert!(result.is_err());
    assert_eq!(result.unwrap_err().kind(), io::ErrorKind::BrokenPipe);
}

#[test]
fn phase3_read_vstring_io_error() {
    struct FailReader;
    impl std::io::Read for FailReader {
        fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::new(
                io::ErrorKind::ConnectionReset,
                "read failed",
            ))
        }
    }

    let result = read_vstring(&mut FailReader);
    assert!(result.is_err());
    assert_eq!(result.unwrap_err().kind(), io::ErrorKind::ConnectionReset);
}

#[test]
fn phase3_negotiate_stdin_io_error() {
    let _env = default_env();

    struct FailReader;
    impl std::io::Read for FailReader {
        fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::new(io::ErrorKind::TimedOut, "read timeout"))
        }
    }

    let protocol = ProtocolVersion::try_from(31).unwrap();
    let mut stdout = Vec::new();

    let result = negotiate_capabilities(
        protocol,
        &mut FailReader,
        &mut stdout,
        true,
        true,
        false,
        true,
    );

    assert!(result.is_err());
    assert_eq!(result.unwrap_err().kind(), io::ErrorKind::TimedOut);
}

#[test]
fn phase3_negotiate_stdout_io_error() {
    let _env = default_env();

    struct FailWriter;
    impl std::io::Write for FailWriter {
        fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
            Err(io::Error::new(io::ErrorKind::WouldBlock, "write blocked"))
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    let protocol = ProtocolVersion::try_from(31).unwrap();
    let input = b"\x03md5\x04zlib";

    let result = negotiate_capabilities(
        protocol,
        &mut &input[..],
        &mut FailWriter,
        true,
        true,
        false,
        true,
    );

    assert!(result.is_err());
    assert_eq!(result.unwrap_err().kind(), io::ErrorKind::WouldBlock);
}

#[test]
fn phase4_checksum_algorithm_as_str() {
    assert_eq!(ChecksumAlgorithm::None.as_str(), "none");
    assert_eq!(ChecksumAlgorithm::MD4.as_str(), "md4");
    assert_eq!(ChecksumAlgorithm::MD5.as_str(), "md5");
    assert_eq!(ChecksumAlgorithm::SHA1.as_str(), "sha1");
    assert_eq!(ChecksumAlgorithm::XXH64.as_str(), "xxh64");
    assert_eq!(ChecksumAlgorithm::XXH3.as_str(), "xxh3");
    assert_eq!(ChecksumAlgorithm::XXH128.as_str(), "xxh128");
}

#[test]
fn phase4_compression_algorithm_as_str() {
    assert_eq!(CompressionAlgorithm::None.as_str(), "none");
    assert_eq!(CompressionAlgorithm::Zlib.as_str(), "zlib");
    assert_eq!(CompressionAlgorithm::ZlibX.as_str(), "zlibx");
    assert_eq!(CompressionAlgorithm::LZ4.as_str(), "lz4");
    assert_eq!(CompressionAlgorithm::Zstd.as_str(), "zstd");
}

#[test]
fn phase4_checksum_algorithm_copy() {
    let algo1 = ChecksumAlgorithm::MD5;
    let algo2 = algo1; // Copy
    assert_eq!(algo1, algo2);
}

#[test]
fn phase4_compression_algorithm_copy() {
    let algo1 = CompressionAlgorithm::Zlib;
    let algo2 = algo1; // Copy
    assert_eq!(algo1, algo2);
}

#[test]
fn phase4_checksum_algorithm_debug() {
    let debug = format!("{:?}", ChecksumAlgorithm::XXH128);
    assert!(debug.contains("XXH128"));
}

#[test]
fn phase4_compression_algorithm_debug() {
    let debug = format!("{:?}", CompressionAlgorithm::Zstd);
    assert!(debug.contains("Zstd"));
}

#[test]
fn phase4_compression_to_compress_algorithm_none() {
    let result = CompressionAlgorithm::None.to_compress_algorithm();
    assert!(result.is_ok());
    assert!(result.unwrap().is_none());
}

#[test]
fn phase4_compression_to_compress_algorithm_zlib() {
    let result = CompressionAlgorithm::Zlib.to_compress_algorithm();
    assert!(result.is_ok());
    let algo = result.unwrap();
    assert!(algo.is_some());
}

#[test]
fn phase4_compression_to_compress_algorithm_zlibx() {
    // ZlibX also maps to Zlib compression
    let result = CompressionAlgorithm::ZlibX.to_compress_algorithm();
    assert!(result.is_ok());
    let algo = result.unwrap();
    assert!(algo.is_some());
}

#[test]
#[cfg(feature = "lz4")]
fn phase4_compression_to_compress_algorithm_lz4() {
    let result = CompressionAlgorithm::LZ4.to_compress_algorithm();
    assert!(result.is_ok());
    let algo = result.unwrap();
    assert!(algo.is_some());
}

#[test]
#[cfg(not(feature = "lz4"))]
fn phase4_compression_lz4_not_available() {
    let result = CompressionAlgorithm::LZ4.to_compress_algorithm();
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    assert!(err.to_string().contains("LZ4"));
}

#[test]
#[cfg(feature = "zstd")]
fn phase4_compression_to_compress_algorithm_zstd() {
    let result = CompressionAlgorithm::Zstd.to_compress_algorithm();
    assert!(result.is_ok());
    let algo = result.unwrap();
    assert!(algo.is_some());
}

#[test]
#[cfg(not(feature = "zstd"))]
fn phase4_compression_zstd_not_available() {
    let result = CompressionAlgorithm::Zstd.to_compress_algorithm();
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    assert!(err.to_string().contains("Zstd"));
}

#[test]
fn phase5_negotiate_only_unsupported_checksums() {
    // upstream: compat.c:383-406 - no common algorithm is a hard error
    let list = "blake3 sha256 sha512 xxh256";
    let result = choose_checksum_algorithm(list, true);
    assert!(
        result.is_err(),
        "no common algorithm should be a negotiation error"
    );
}

#[test]
fn phase5_negotiate_only_unsupported_compressions() {
    // upstream: compat.c:383-406 - no common compression is a hard error
    let list = "bzip2 lzma xz brotli";
    let err = choose_compression_algorithm(list, true).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::Unsupported);
}

#[test]
fn phase5_negotiate_whitespace_only_list() {
    // upstream: compat.c:383-406 - whitespace-only list has no valid algorithms
    let list = "   \t   \n   ";
    let checksum = choose_checksum_algorithm(list, true);
    assert!(
        checksum.is_err(),
        "whitespace-only list should be a negotiation error"
    );

    let compression = choose_compression_algorithm(list, true);
    assert!(
        compression.is_err(),
        "whitespace-only list should be a negotiation error"
    );
}

#[test]
fn phase5_negotiate_mixed_supported_unsupported() {
    // Mix of supported and unsupported, should pick first supported
    let list = "blake3 unsupported xxh128 md5";
    let result = choose_checksum_algorithm(list, true).unwrap();
    assert_eq!(result, ChecksumAlgorithm::XXH128);
}

#[test]
fn phase5_negotiate_order_preference() {
    // First supported algorithm should win
    let list1 = "xxh128 md5 sha1";
    let list2 = "md5 xxh128 sha1";
    let list3 = "sha1 md5 xxh128";

    assert_eq!(
        choose_checksum_algorithm(list1, true).unwrap(),
        ChecksumAlgorithm::XXH128
    );
    assert_eq!(
        choose_checksum_algorithm(list2, true).unwrap(),
        ChecksumAlgorithm::MD5
    );
    assert_eq!(
        choose_checksum_algorithm(list3, true).unwrap(),
        ChecksumAlgorithm::SHA1
    );
}

#[test]
fn phase5_negotiate_xxh3_support() {
    let list = "xxh3 xxh128";
    let result = choose_checksum_algorithm(list, true).unwrap();
    assert_eq!(result, ChecksumAlgorithm::XXH3);
}

#[test]
fn phase5_negotiate_zlibx_vs_zlib() {
    let list = "zlibx zlib";
    let result = choose_compression_algorithm(list, true).unwrap();
    assert_eq!(result, CompressionAlgorithm::ZlibX);
}

#[test]
fn phase6_full_negotiation_all_supported_versions() {
    let _env = default_env();

    for version in 28..=32 {
        let protocol = ProtocolVersion::try_from(version).unwrap();

        if protocol.uses_fixed_encoding() {
            // Legacy: no exchange needed
            let mut stdin = &b""[..];
            let mut stdout = Vec::new();
            let result =
                negotiate_capabilities(protocol, &mut stdin, &mut stdout, true, true, false, true)
                    .unwrap();
            assert_eq!(result.checksum, ChecksumAlgorithm::MD4);
            assert_eq!(result.compression, CompressionAlgorithm::Zlib);
            assert!(stdout.is_empty());
        } else {
            // Modern: exchange required
            let response = b"\x04sha1\x04none";
            let mut stdin = &response[..];
            let mut stdout = Vec::new();
            let result =
                negotiate_capabilities(protocol, &mut stdin, &mut stdout, true, true, false, true)
                    .unwrap();
            assert_eq!(result.checksum, ChecksumAlgorithm::SHA1);
            assert_eq!(result.compression, CompressionAlgorithm::None);
            assert!(!stdout.is_empty());
        }
    }
}

#[test]
fn phase6_full_negotiation_checksum_only() {
    let _env = default_env();

    let protocol = ProtocolVersion::try_from(31).unwrap();
    let response = b"\x04sha1"; // Only checksum, no compression
    let mut stdin = &response[..];
    let mut stdout = Vec::new();

    let result = negotiate_capabilities(
        protocol,
        &mut stdin,
        &mut stdout,
        true,  // do_negotiation
        false, // send_compression = false
        false,
        true,
    )
    .unwrap();

    assert_eq!(result.checksum, ChecksumAlgorithm::SHA1);
    assert_eq!(result.compression, CompressionAlgorithm::None);
}

#[test]
fn phase7_vstring_multiple_sequential() {
    let strings = ["first", "second", "third", "fourth"];
    let mut buffer = Vec::new();

    for s in &strings {
        write_vstring(&mut buffer, s).unwrap();
    }

    let mut reader = &buffer[..];
    for expected in &strings {
        let received = read_vstring(&mut reader).unwrap();
        assert_eq!(received, *expected);
    }
}

#[test]
fn phase7_vstring_mixed_sizes() {
    let strings = [
        "a",              // 1 byte
        "hello world",    // 11 bytes
        &"x".repeat(127), // max 1-byte format
        &"y".repeat(128), // min 2-byte format
        &"z".repeat(200), // larger 2-byte format (within 256 limit)
    ];
    let mut buffer = Vec::new();

    for s in &strings {
        write_vstring(&mut buffer, s).unwrap();
    }

    let mut reader = &buffer[..];
    for expected in &strings {
        let received = read_vstring(&mut reader).unwrap();
        assert_eq!(received, *expected);
    }
}

#[test]
fn phase8_negotiation_result_copy() {
    let r1 = NegotiationResult {
        checksum: ChecksumAlgorithm::XXH128,
        compression: CompressionAlgorithm::ZlibX,
    };
    let r2 = r1; // Copy
    assert_eq!(r1.checksum, r2.checksum);
    assert_eq!(r1.compression, r2.compression);
}

#[test]
fn phase8_negotiation_result_all_combinations() {
    let checksums = [
        ChecksumAlgorithm::None,
        ChecksumAlgorithm::MD4,
        ChecksumAlgorithm::MD5,
        ChecksumAlgorithm::SHA1,
        ChecksumAlgorithm::XXH64,
        ChecksumAlgorithm::XXH3,
        ChecksumAlgorithm::XXH128,
    ];
    let compressions = [
        CompressionAlgorithm::None,
        CompressionAlgorithm::Zlib,
        CompressionAlgorithm::ZlibX,
        CompressionAlgorithm::LZ4,
        CompressionAlgorithm::Zstd,
    ];

    for &checksum in &checksums {
        for &compression in &compressions {
            let result = NegotiationResult {
                checksum,
                compression,
            };
            assert_eq!(result.checksum, checksum);
            assert_eq!(result.compression, compression);
        }
    }
}
//
// These tests verify graceful fallback behavior when:
// 1. Server doesn't support a requested capability
// 2. Client sends unknown capability strings
// 3. Features must degrade gracefully
//
// Upstream rsync (compat.c) implements graceful degradation when
// capabilities cannot be negotiated, falling back to safe defaults.

/// Tests fallback when server offers only algorithms client doesn't prefer.
/// Client wants xxh128, but server only offers md5/md4/sha1.
#[test]
fn capability_fallback_server_missing_preferred_checksum() {
    // Remote only offers legacy checksums, not modern xxhash variants
    let remote_list = "md5 md4 sha1";
    let result = choose_checksum_algorithm(remote_list, true).unwrap();
    // Should fall back to first supported algorithm: md5
    assert_eq!(result, ChecksumAlgorithm::MD5);
}

/// Tests fallback when server offers only legacy MD4.
#[test]
fn capability_fallback_server_only_md4() {
    let remote_list = "md4";
    let result = choose_checksum_algorithm(remote_list, true).unwrap();
    assert_eq!(result, ChecksumAlgorithm::MD4);
}

/// Tests fallback when server offers only 'none' checksum.
#[test]
fn capability_fallback_server_only_none_checksum() {
    let remote_list = "none";
    let result = choose_checksum_algorithm(remote_list, true).unwrap();
    assert_eq!(result, ChecksumAlgorithm::None);
}

/// Tests fallback when server offers compression we don't have compiled in.
#[test]
fn capability_fallback_server_offers_unavailable_compression() {
    // Server offers brotli (not supported) first, then zlib
    let remote_list = "brotli lzma xz zlib";
    let result = choose_compression_algorithm(remote_list, true).unwrap();
    // Should skip unsupported and use zlib
    assert_eq!(result, CompressionAlgorithm::Zlib);
}

/// Tests that a peer list with only unavailable compressions is a hard error.
#[test]
fn capability_fallback_server_only_unavailable_compression() {
    // upstream: compat.c:383-406 recv_negotiate_str - no mutual compression
    // aborts with RERR_UNSUPPORTED; there is no silent "none" fallback.
    let remote_list = "brotli lzma xz";
    let err = choose_compression_algorithm(remote_list, true).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::Unsupported);
    assert_eq!(err.to_string(), "Failed to negotiate a compress choice.");
}

/// Tests fallback when server offers only 'none' compression.
#[test]
fn capability_fallback_server_only_none_compression() {
    let remote_list = "none";
    let result = choose_compression_algorithm(remote_list, true).unwrap();
    assert_eq!(result, CompressionAlgorithm::None);
}

/// Tests handling of completely unknown checksum algorithm names.
#[test]
fn capability_fallback_unknown_checksum_strings() {
    // upstream: compat.c:383-406 - all unknown algorithm names is a hard error
    let remote_list = "blake2b blake3 argon2 scrypt";
    let result = choose_checksum_algorithm(remote_list, true);
    assert!(
        result.is_err(),
        "all unknown algorithms should be a negotiation error"
    );
}

/// Tests handling of completely unknown compression algorithm names.
#[test]
fn capability_fallback_unknown_compression_strings() {
    // upstream: compat.c:383-406 - all unknown algorithm names is a hard error
    let remote_list = "snappy lzo lzf brotli";
    let err = choose_compression_algorithm(remote_list, true).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::Unsupported);
}

/// Tests mixed known and unknown checksums - unknown first.
#[test]
fn capability_fallback_mixed_unknown_known_checksum() {
    let remote_list = "blake3 blake2b sha1 md5";
    let result = choose_checksum_algorithm(remote_list, true).unwrap();
    // Should skip unknown and pick first known (sha1)
    assert_eq!(result, ChecksumAlgorithm::SHA1);
}

/// Tests mixed known and unknown compressions - unknown first.
#[test]
fn capability_fallback_mixed_unknown_known_compression() {
    let remote_list = "snappy lzo zlibx zlib";
    let result = choose_compression_algorithm(remote_list, true).unwrap();
    // Should skip unknown and pick first known (zlibx)
    assert_eq!(result, CompressionAlgorithm::ZlibX);
}

/// Tests handling of malformed algorithm names (typos, punctuation).
#[test]
fn capability_fallback_malformed_algorithm_names() {
    // upstream: compat.c:383-406 - malformed names with no valid match is a hard error
    let remote_list = "md-5 md_5 md55 mdv md5!";
    let result = choose_checksum_algorithm(remote_list, true);
    assert!(
        result.is_err(),
        "all malformed algorithms should be a negotiation error"
    );
}

/// Tests that name matching is case-insensitive, like upstream's
/// `get_nni_by_name()` (compat.c:230-236 `strncasecmp`). A peer whose
/// `RSYNC_CHECKSUM_LIST` carries upper-case names advertises them verbatim
/// (parse_nni_str keeps a non-alias token's original bytes), and upstream
/// still negotiates them.
#[test]
fn capability_fallback_case_insensitive_names() {
    for list in ["MD5", "Md5", "mD5"] {
        for is_server in [true, false] {
            let result = choose_checksum_algorithm(list, is_server).unwrap();
            assert_eq!(result, ChecksumAlgorithm::MD5, "list {list:?}");
        }
    }
    let result = choose_compression_algorithm("ZLIB", true).unwrap();
    assert_eq!(result, CompressionAlgorithm::Zlib);
}

/// Tests handling of empty algorithm string between spaces.
#[test]
fn capability_fallback_empty_between_spaces() {
    let remote_list = "blake3  sha1"; // Double space
    let result = choose_checksum_algorithm(remote_list, true).unwrap();
    // split_whitespace handles this correctly
    assert_eq!(result, ChecksumAlgorithm::SHA1);
}

/// Tests handling of numeric-only strings.
#[test]
fn capability_fallback_numeric_strings() {
    // upstream: compat.c:383-406 - no valid algorithms is a hard error
    let remote_list = "123 456 789";
    let result = choose_checksum_algorithm(remote_list, true);
    assert!(
        result.is_err(),
        "numeric-only list should be a negotiation error"
    );
}

/// Tests handling of special characters in algorithm names.
#[test]
fn capability_fallback_special_chars() {
    // upstream: compat.c:383-406 - no valid algorithms is a hard error
    let remote_list = "md5@ sha1# xxh* md5-v2";
    let result = choose_checksum_algorithm(remote_list, true);
    assert!(
        result.is_err(),
        "special-char names should be a negotiation error"
    );
}

/// Tests handling of very long unknown algorithm names.
#[test]
fn capability_fallback_long_unknown_names() {
    let long_name = "a".repeat(100);
    let remote_list = format!("{} {}", long_name, "md5");
    let result = choose_checksum_algorithm(&remote_list, true).unwrap();
    // Long name is unknown, should use md5
    assert_eq!(result, ChecksumAlgorithm::MD5);
}

/// Tests handling of unicode in algorithm names.
/// Non-breaking space (\u{00A0}) is Unicode whitespace, so split_whitespace()
/// will treat "md5\u{00A0}fake" as two tokens: "md5" and "fake".
/// Therefore "md5" is found and selected.
#[test]
fn capability_fallback_unicode_names() {
    let remote_list = "md5\u{00A0}fake sha1 zlib";
    let result = choose_checksum_algorithm(remote_list, true).unwrap();
    // \u{00A0} is Unicode whitespace, so "md5" is a valid token and matches
    assert_eq!(result, ChecksumAlgorithm::MD5);
}

/// Tests graceful degradation from modern to legacy checksums.
#[test]
fn capability_fallback_graceful_checksum_degradation() {
    // Simulate negotiating with increasingly legacy servers

    // Modern server: full support
    let modern_list = "xxh128 xxh3 xxh64 md5 md4 sha1 none";
    assert_eq!(
        choose_checksum_algorithm(modern_list, true).unwrap(),
        ChecksumAlgorithm::XXH128
    );

    // Intermediate server: no xxh128, has xxh64
    let intermediate_list = "xxh64 md5 md4 sha1 none";
    assert_eq!(
        choose_checksum_algorithm(intermediate_list, true).unwrap(),
        ChecksumAlgorithm::XXH64
    );

    // Legacy server: only md5 and md4
    let legacy_list = "md5 md4 none";
    assert_eq!(
        choose_checksum_algorithm(legacy_list, true).unwrap(),
        ChecksumAlgorithm::MD5
    );

    // Very old server: only md4
    let ancient_list = "md4 none";
    assert_eq!(
        choose_checksum_algorithm(ancient_list, true).unwrap(),
        ChecksumAlgorithm::MD4
    );
}

/// Tests compression negotiation with modern and legacy peers.
///
/// Preference order follows upstream `valid_compressions_items[]`
/// (compat.c:100-112): zstd > lz4 > zlibx > zlib > none, with each codec
/// present only when its feature is compiled in. Both zstd and lz4 wire
/// formats are validated byte-for-byte against upstream 3.4.4.
#[test]
fn capability_compression_negotiation_preference() {
    // Remote offers full modern list - server picks the first entry it also
    // supports, in upstream preference order zstd > lz4 > zlibx.
    let modern_list = "zstd lz4 zlibx zlib none";
    let result = choose_compression_algorithm(modern_list, true).unwrap();
    #[cfg(feature = "zstd")]
    assert_eq!(result, CompressionAlgorithm::Zstd);
    #[cfg(all(not(feature = "zstd"), feature = "lz4"))]
    assert_eq!(result, CompressionAlgorithm::LZ4);
    #[cfg(all(not(feature = "zstd"), not(feature = "lz4")))]
    assert_eq!(result, CompressionAlgorithm::ZlibX);

    // Remote offers lz4 first without zstd - picks lz4 when compiled in,
    // otherwise falls through to zlibx.
    let no_zstd_list = "lz4 zlibx zlib none";
    let result = choose_compression_algorithm(no_zstd_list, true).unwrap();
    #[cfg(feature = "lz4")]
    assert_eq!(result, CompressionAlgorithm::LZ4);
    #[cfg(not(feature = "lz4"))]
    assert_eq!(result, CompressionAlgorithm::ZlibX);

    // Server with only zlib variants
    let zlib_only = "zlibx zlib none";
    assert_eq!(
        choose_compression_algorithm(zlib_only, true).unwrap(),
        CompressionAlgorithm::ZlibX
    );

    // Server preferring classic zlib
    let classic_zlib = "zlib none";
    assert_eq!(
        choose_compression_algorithm(classic_zlib, true).unwrap(),
        CompressionAlgorithm::Zlib
    );
}

/// Tests protocol version fallback behavior.
#[test]
fn capability_fallback_protocol_version_behavior() {
    let _env = default_env();

    // Protocol 28-29: Uses legacy defaults without negotiation
    for version in [28, 29] {
        let protocol = ProtocolVersion::try_from(version).unwrap();
        let mut stdin = &b""[..];
        let mut stdout = Vec::new();

        let result = negotiate_capabilities(
            protocol,
            &mut stdin,
            &mut stdout,
            true,  // do_negotiation
            true,  // send_compression
            false, // is_daemon_mode
            true,  // is_server
        )
        .unwrap();

        // Legacy protocols use MD4 and Zlib as defaults
        assert_eq!(result.checksum, ChecksumAlgorithm::MD4);
        assert_eq!(result.compression, CompressionAlgorithm::Zlib);
        // No I/O should occur for legacy protocols
        assert!(stdout.is_empty());
    }
}

/// Tests do_negotiation=false fallback (client lacks VARINT_FLIST_FLAGS).
#[test]
fn capability_fallback_no_varint_flist_flags() {
    let _env = default_env();

    // When client lacks VARINT_FLIST_FLAGS capability, we skip negotiation
    // and use protocol 30+ defaults without any wire exchange
    let protocol = ProtocolVersion::try_from(31).unwrap();
    let mut stdin = &b""[..]; // No input needed
    let mut stdout = Vec::new();

    let result = negotiate_capabilities(
        protocol,
        &mut stdin,
        &mut stdout,
        false, // do_negotiation = false (client lacks capability)
        true,  // send_compression
        false, // is_daemon_mode
        true,  // is_server
    )
    .unwrap();

    // Should use MD5 (protocol 30+ default) and Zlib (upstream: compat.c:194
    // defaults to CPRES_ZLIB when -z is active but no vstring negotiation)
    assert_eq!(result.checksum, ChecksumAlgorithm::MD5);
    assert_eq!(result.compression, CompressionAlgorithm::Zlib);
    // No data should be sent when do_negotiation is false
    assert!(stdout.is_empty());
}

/// Tests graceful handling when remote sends preference order we disagree with.
#[test]
fn capability_fallback_disagreeing_preference_order() {
    // Remote prefers md4 over md5, but we still respect their order
    let remote_list = "md4 md5 sha1";
    let result = choose_checksum_algorithm(remote_list, true).unwrap();
    // We pick first from THEIR list that we support
    assert_eq!(result, ChecksumAlgorithm::MD4);
}

/// Tests that we handle duplicate algorithm names gracefully.
#[test]
fn capability_fallback_duplicate_algorithms() {
    let remote_list = "md5 md5 md5 sha1 sha1";
    let result = choose_checksum_algorithm(remote_list, true).unwrap();
    // Should still work, picks first md5
    assert_eq!(result, ChecksumAlgorithm::MD5);
}

/// Tests full negotiation where remote only supports legacy checksums.
#[test]
fn capability_fallback_full_negotiation_legacy_remote() {
    let _env = default_env();

    let protocol = ProtocolVersion::try_from(31).unwrap();

    // Remote is a legacy server that only knows md5 and zlib
    let remote_response = b"\x03md5\x04zlib";
    let mut stdin = &remote_response[..];
    let mut stdout = Vec::new();

    let result = negotiate_capabilities(
        protocol,
        &mut stdin,
        &mut stdout,
        true,  // do_negotiation
        true,  // send_compression
        false, // is_daemon_mode
        true,  // is_server
    )
    .unwrap();

    // We should accept their capabilities
    assert_eq!(result.checksum, ChecksumAlgorithm::MD5);
    assert_eq!(result.compression, CompressionAlgorithm::Zlib);
}

/// Tests full negotiation where remote only supports 'none' for both.
#[test]
fn capability_fallback_full_negotiation_none_only() {
    let _env = default_env();

    let protocol = ProtocolVersion::try_from(31).unwrap();

    // Remote disables both checksum and compression
    let remote_response = b"\x04none\x04none";
    let mut stdin = &remote_response[..];
    let mut stdout = Vec::new();

    let result = negotiate_capabilities(
        protocol,
        &mut stdin,
        &mut stdout,
        true,  // do_negotiation
        true,  // send_compression
        false, // is_daemon_mode
        true,  // is_server
    )
    .unwrap();

    assert_eq!(result.checksum, ChecksumAlgorithm::None);
    assert_eq!(result.compression, CompressionAlgorithm::None);
}

/// Tests negotiation fallback with compression disabled.
#[test]
fn capability_fallback_compression_disabled() {
    let _env = default_env();

    let protocol = ProtocolVersion::try_from(31).unwrap();

    // Only checksum negotiation, no compression
    let remote_response = b"\x04sha1";
    let mut stdin = &remote_response[..];
    let mut stdout = Vec::new();

    let result = negotiate_capabilities(
        protocol,
        &mut stdin,
        &mut stdout,
        true,  // do_negotiation
        false, // send_compression = false
        false, // is_daemon_mode
        true,  // is_server
    )
    .unwrap();

    assert_eq!(result.checksum, ChecksumAlgorithm::SHA1);
    // Compression should be None when not negotiated
    assert_eq!(result.compression, CompressionAlgorithm::None);
}

/// Tests that ChecksumAlgorithm::parse returns error for unknown names.
#[test]
fn capability_fallback_checksum_parse_unknown() {
    let result = ChecksumAlgorithm::parse("blake2");
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    assert!(err.to_string().contains("unsupported checksum algorithm"));
    assert!(err.to_string().contains("blake2"));
}

/// Tests that CompressionAlgorithm::parse returns error for unknown names.
#[test]
fn capability_fallback_compression_parse_unknown() {
    let result = CompressionAlgorithm::parse("bzip2");
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    assert!(
        err.to_string()
            .contains("unsupported compression algorithm")
    );
    assert!(err.to_string().contains("bzip2"));
}

/// Tests alias handling in a peer list: "xxhash" resolves to xxh64
/// (upstream checksum.c:55-56, both names share CSUM_XXH64 and the alias
/// resolves through `main_nni`), while a bare "xxh" is not in
/// `valid_checksums_items[]` and is skipped as unknown.
#[test]
fn capability_fallback_xxh_alias_in_list() {
    for is_server in [true, false] {
        let result = choose_checksum_algorithm("xxhash md5", is_server).unwrap();
        assert_eq!(result, ChecksumAlgorithm::XXH64);

        let result = choose_checksum_algorithm("xxh md5", is_server).unwrap();
        assert_eq!(result, ChecksumAlgorithm::MD5, "bare xxh must be skipped");
    }
}

/// Pins forward compatibility with checksum names upstream may add later,
/// e.g. sha256 as a transfer checksum (proposed upstream PR #1007). A newer
/// peer advertising such a name must not break negotiation: upstream
/// `parse_negotiate_str()` skips names `get_nni_by_name()` does not recognise
/// (compat.c:350) and falls through to the best mutual algorithm.
#[test]
fn forward_compat_unknown_sha256_in_peer_list_is_skipped() {
    for is_server in [true, false] {
        let result = choose_checksum_algorithm("sha256 xxh128 md5", is_server).unwrap();
        assert_eq!(result, ChecksumAlgorithm::XXH128, "server={is_server}");

        // A future-name-first list still lands on the best mutual choice.
        let result = choose_checksum_algorithm("sha512 sha256 md5", is_server).unwrap();
        assert_eq!(result, ChecksumAlgorithm::MD5, "server={is_server}");
    }
}

/// A peer list holding ONLY unknown names fails negotiation with upstream's
/// hard error - `recv_negotiate_str` prints "Failed to negotiate a checksum
/// choice." and exits with RERR_UNSUPPORTED (compat.c:387,406; errcode.h:28
/// `RERR_UNSUPPORTED 4`). There is no silent md5/md4 fallback on the
/// negotiated path; that fallback only exists for peers that cannot negotiate
/// at all (compat.c:550-554).
#[test]
fn forward_compat_only_unknown_names_hard_error() {
    for is_server in [true, false] {
        let err = choose_checksum_algorithm("sha256 sha512 blake3", is_server).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::Unsupported, "server={is_server}");
        // upstream: compat.c:382 - the offered lists print only on the client;
        // the server (`am_server && do_negotiated_strings`) aborts on the
        // headline alone.
        let expected = if is_server {
            "Failed to negotiate a checksum choice.".to_string()
        } else {
            "Failed to negotiate a checksum choice.\n\
             Server list: sha256 sha512 blake3\n\
             Client list: xxh128 xxh3 xxh64 md5 md4 sha1"
                .to_string()
        };
        assert_eq!(err.to_string(), expected, "server={is_server}");
    }
}

/// Full negotiation round against a newer peer whose checksum list leads with
/// a future name: the exchange completes and lands on the best mutual
/// algorithm without error.
#[test]
fn forward_compat_full_negotiation_with_future_peer() {
    let _env = default_env();

    let protocol = ProtocolVersion::try_from(31).unwrap();
    let mut peer = Vec::new();
    write_vstring(&mut peer, "sha256 xxh128 xxh3 xxh64 md5 md4 sha1").unwrap();
    let mut stdin = &peer[..];
    let mut stdout = Vec::new();

    let result = negotiate_capabilities(
        protocol,
        &mut stdin,
        &mut stdout,
        true,  // do_negotiation
        false, // send_compression
        false, // is_daemon_mode
        false, // is_server
    )
    .unwrap();

    assert_eq!(result.checksum, ChecksumAlgorithm::XXH128);
}

/// Tests that algorithm names must be whole-token matches. Case variants DO
/// match (upstream compat.c:233 `strncasecmp`), but prefixes/suffixes do not:
/// `get_nni_by_name()` requires `nni->name[len] == '\0'`.
#[test]
fn capability_fallback_exact_match_required() {
    // These should NOT match any algorithm - upstream compat.c:383-406
    // rejects lists with no common algorithm as a hard error.
    let invalid_names = [
        "md5-hmac",   // suffix
        "prefix-md5", // prefix
        "md",         // truncation
        "xxh",        // truncation of xxh64/xxhash
    ];

    for name in invalid_names {
        let result = choose_checksum_algorithm(name, true);
        assert!(
            result.is_err(),
            "'{name}' should not match any supported algorithm"
        );
    }
}

/// Tests behavior with extremely long algorithm lists.
#[test]
fn capability_fallback_very_long_list() {
    // Generate a list with 1000 unknown algorithms followed by md5
    let mut list = Vec::new();
    for i in 0..1000 {
        list.push(format!("unknown_{}", i));
    }
    list.push("md5".to_string());
    let remote_list = list.join(" ");

    let result = choose_checksum_algorithm(&remote_list, true).unwrap();
    assert_eq!(result, ChecksumAlgorithm::MD5);
}

/// Tests that whitespace-only lists produce an error (no algorithms to match).
///
/// Upstream `recv_negotiate_str` (compat.c:383-406) treats empty/no-match as
/// a hard error. Whitespace-only strings contain zero algorithm names after
/// `split_whitespace()`, so they must fail.
#[test]
fn capability_fallback_whitespace_only_lists_error() {
    let whitespace_only = ["   ", "\t\t\t", "  \t  \n  "];

    for list in whitespace_only {
        let result = choose_checksum_algorithm(list, true);
        assert!(
            result.is_err(),
            "whitespace-only list '{}' should produce an error",
            list.escape_debug()
        );
    }
}

/// Tests that valid algorithms surrounded by whitespace are found correctly.
///
/// `split_whitespace()` handles leading/trailing/mixed whitespace, so
/// algorithm names embedded in whitespace must still match.
#[test]
fn capability_fallback_whitespace_padded_valid_lists() {
    let valid_lists = [
        ("   md5   ", ChecksumAlgorithm::MD5),
        ("  \t md5 \n sha1  ", ChecksumAlgorithm::MD5), // first match wins
    ];

    for (list, expected) in valid_lists {
        let result = choose_checksum_algorithm(list, true).unwrap();
        assert_eq!(
            result,
            expected,
            "list '{}' should produce {:?}",
            list.escape_debug(),
            expected
        );
    }
}

/// Tests that negotiation handles truncated input gracefully.
#[test]
fn capability_fallback_truncated_vstring() {
    let _env = default_env();

    let protocol = ProtocolVersion::try_from(31).unwrap();

    // Truncated input - claims 10 bytes but only provides 3
    let truncated = [0x0A, b'm', b'd', b'5']; // Length 10, but only 3 bytes follow
    let mut stdin = &truncated[..];
    let mut stdout = Vec::new();

    let result =
        negotiate_capabilities(protocol, &mut stdin, &mut stdout, true, false, false, true);

    // Should fail with UnexpectedEof
    assert!(result.is_err());
    assert_eq!(result.unwrap_err().kind(), io::ErrorKind::UnexpectedEof);
}

/// Tests handling of empty vstring (length 0).
#[test]
fn capability_fallback_empty_vstring() {
    let _env = default_env();

    let protocol = ProtocolVersion::try_from(31).unwrap();

    // Empty checksum list vstring: length=0
    let empty_vstring = [0x00]; // Zero-length vstring
    let mut stdin = &empty_vstring[..];
    let mut stdout = Vec::new();

    let result = negotiate_capabilities(
        protocol,
        &mut stdin,
        &mut stdout,
        true,
        false, // no compression
        false,
        true,
    );

    // upstream: compat.c:383-406 - empty remote checksum list is a hard error
    assert!(result.is_err());
}

/// Tests that all protocol versions handle fallback consistently.
#[test]
fn capability_fallback_all_protocol_versions() {
    let _env = default_env();

    for version in 28..=32 {
        let protocol = ProtocolVersion::try_from(version).unwrap();

        // For legacy protocols, no exchange happens
        if protocol.uses_fixed_encoding() {
            let mut stdin = &b""[..];
            let mut stdout = Vec::new();
            let result =
                negotiate_capabilities(protocol, &mut stdin, &mut stdout, true, true, false, true)
                    .unwrap();
            assert_eq!(
                result.checksum,
                ChecksumAlgorithm::MD4,
                "Protocol {} should use MD4",
                version
            );
        } else {
            // For modern protocols, test with a fallback scenario
            let remote = b"\x03md5\x04zlib";
            let mut stdin = &remote[..];
            let mut stdout = Vec::new();
            let result =
                negotiate_capabilities(protocol, &mut stdin, &mut stdout, true, true, false, true)
                    .unwrap();
            assert_eq!(
                result.checksum,
                ChecksumAlgorithm::MD5,
                "Protocol {} should accept MD5",
                version
            );
        }
    }
}

#[test]
fn supported_compressions_includes_validated_algorithms() {
    // Both zstd and lz4 wire framings are validated byte-for-byte against
    // upstream 3.4.4, so each is advertised when its feature is compiled in.
    let list = supported_compressions();
    #[cfg(feature = "zstd")]
    assert!(
        list.contains(&"zstd"),
        "zstd must be advertised when enabled"
    );
    #[cfg(feature = "lz4")]
    assert!(list.contains(&"lz4"), "lz4 must be advertised when enabled");
    #[cfg(not(feature = "lz4"))]
    assert!(
        !list.contains(&"lz4"),
        "lz4 must be absent when its feature is disabled"
    );
    assert!(list.contains(&"zlibx"));
    assert!(list.contains(&"zlib"));
    assert!(list.contains(&"none"));
}

#[test]
fn supported_compressions_order_matches_upstream() {
    // upstream: compat.c:100-112 - preference order is
    // zstd > lz4 > zlibx > zlib > none, each present only when compiled in.
    let list = supported_compressions();
    let zlibx_pos = list.iter().position(|&s| s == "zlibx").unwrap();
    let zlib_pos = list.iter().position(|&s| s == "zlib").unwrap();
    let none_pos = list.iter().position(|&s| s == "none").unwrap();
    // upstream: compat.c:101-108 - zstd, then lz4, precede zlibx when available.
    #[cfg(feature = "lz4")]
    let lz4_pos = {
        let lz4_pos = list.iter().position(|&s| s == "lz4").unwrap();
        assert!(lz4_pos < zlibx_pos);
        lz4_pos
    };
    #[cfg(feature = "zstd")]
    {
        let zstd_pos = list.iter().position(|&s| s == "zstd").unwrap();
        assert!(zstd_pos < zlibx_pos);
        #[cfg(feature = "lz4")]
        assert!(zstd_pos < lz4_pos);
    }
    assert!(zlibx_pos < zlib_pos);
    assert!(zlib_pos < none_pos);
}

#[test]
fn negotiate_picks_best_validated_algorithm() {
    // Remote offers full modern list; server picks the first entry it also
    // supports in upstream preference order zstd > lz4 > zlibx. Both zstd and
    // lz4 wire formats are validated byte-for-byte against upstream 3.4.4.
    let list = "zstd lz4 zlibx zlib none";
    let result = choose_compression_algorithm(list, true).unwrap();
    #[cfg(feature = "zstd")]
    assert_eq!(result, CompressionAlgorithm::Zstd);
    #[cfg(all(not(feature = "zstd"), feature = "lz4"))]
    assert_eq!(result, CompressionAlgorithm::LZ4);
    #[cfg(all(not(feature = "zstd"), not(feature = "lz4")))]
    assert_eq!(result, CompressionAlgorithm::ZlibX);
}

#[test]
fn test_choose_checksum_server_picks_first_client_match() {
    // Server iterates remote (client's) list, picks first entry in server's local list.
    // upstream: compat.c:353 `if (best == 1 || am_server) break;`
    // Client sends: md5 xxh128 xxh3
    // Server supports: xxh128 xxh3 xxh64 md5 md4 sha1 none
    // Server iterates client list: md5 is first match → picks md5
    let result = choose_checksum_algorithm("md5 xxh128 xxh3", true).unwrap();
    assert_eq!(result, ChecksumAlgorithm::MD5);
}

#[test]
fn test_choose_checksum_client_picks_best_local_preference() {
    // Client iterates its own local list, picks first item also in server's list.
    // upstream: compat.c:349-354 - client finds lowest local position among matches.
    // Server sends: md5 xxh128 xxh3
    // Client supports: xxh128 xxh3 xxh64 md5 md4 sha1 none
    // Client iterates local list: xxh128 is first local item in server's list → picks xxh128
    let result = choose_checksum_algorithm("md5 xxh128 xxh3", false).unwrap();
    assert_eq!(result, ChecksumAlgorithm::XXH128);
}

#[test]
fn test_choose_checksum_server_client_converge_when_same_order() {
    // When both sides have the same preference order, server and client agree.
    let list = "xxh128 xxh3 xxh64 md5";
    let server_result = choose_checksum_algorithm(list, true).unwrap();
    let client_result = choose_checksum_algorithm(list, false).unwrap();
    assert_eq!(server_result, client_result);
    assert_eq!(server_result, ChecksumAlgorithm::XXH128);
}

#[test]
fn test_choose_compression_server_picks_first_client_match() {
    // Server iterates remote (client's) list, picks first entry in server's local list.
    let result = choose_compression_algorithm("zlib zlibx none", true).unwrap();
    assert_eq!(result, CompressionAlgorithm::Zlib);
}

#[test]
fn test_choose_compression_client_picks_best_local_preference() {
    // Client iterates its own local list, picks first item also in server's list.
    // Server sends: zlib zlibx none
    // Client local list: [zstd, lz4, zlibx, zlib, none] (feature-dependent)
    // Client iterates local: zlibx appears before zlib → picks zlibx
    let result = choose_compression_algorithm("zlib zlibx none", false).unwrap();
    assert_eq!(result, CompressionAlgorithm::ZlibX);
}

// -- compression_override tests for negotiate_capabilities_with_override --

#[test]
fn compression_override_used_on_legacy_protocol() {
    let _env = default_env();

    // upstream: compat.c:194-195 - compression_override is honoured even on
    // legacy protocols where no vstring exchange occurs.
    let protocol = ProtocolVersion::try_from(29).unwrap();
    let mut stdin = &b""[..];
    let mut stdout = Vec::new();

    let result = negotiate_capabilities_with_override(
        protocol,
        &mut stdin,
        &mut stdout,
        &NegotiationConfig {
            do_negotiation: true,
            send_compression: true,
            is_daemon_mode: false,
            is_server: true,
            checksum_override: None,
            compression_override: Some(CompressionAlgorithm::Zstd),
            compression_level: crate::nstr::CLVL_NOT_SPECIFIED,
            write_batch: false,
        },
    )
    .unwrap();

    assert_eq!(result.compression, CompressionAlgorithm::Zstd);
    assert!(stdout.is_empty(), "no wire data on legacy protocol");
}

#[test]
fn compression_override_used_without_negotiation() {
    let _env = default_env();

    // When do_negotiation=false and compression_override is set, the override
    // should be used directly without any wire exchange.
    let protocol = ProtocolVersion::try_from(31).unwrap();
    let mut stdin = &b""[..];
    let mut stdout = Vec::new();

    let result = negotiate_capabilities_with_override(
        protocol,
        &mut stdin,
        &mut stdout,
        &NegotiationConfig {
            do_negotiation: false,
            send_compression: false, // override bypasses this
            is_daemon_mode: false,
            is_server: true,
            checksum_override: None,
            compression_override: Some(CompressionAlgorithm::LZ4),
            compression_level: crate::nstr::CLVL_NOT_SPECIFIED,
            write_batch: false,
        },
    )
    .unwrap();

    assert_eq!(result.compression, CompressionAlgorithm::LZ4);
    assert!(stdout.is_empty(), "no wire data without negotiation");
}

#[test]
fn compression_override_none_falls_through_to_normal_negotiation() {
    let _env = default_env();

    // When compression_override is None, normal vstring negotiation is used.
    // Protocol 29 defaults to Zlib when no override is present.
    let protocol = ProtocolVersion::try_from(29).unwrap();
    let mut stdin = &b""[..];
    let mut stdout = Vec::new();

    let result = negotiate_capabilities_with_override(
        protocol,
        &mut stdin,
        &mut stdout,
        &NegotiationConfig {
            do_negotiation: true,
            send_compression: true,
            is_daemon_mode: false,
            is_server: true,
            checksum_override: None,
            compression_override: None,
            compression_level: crate::nstr::CLVL_NOT_SPECIFIED,
            write_batch: false,
        },
    )
    .unwrap();

    assert_eq!(
        result.compression,
        CompressionAlgorithm::Zlib,
        "without override, legacy protocol defaults to Zlib"
    );
}

/// Tests for the `RSYNC_CHECKSUM_LIST` / `RSYNC_COMPRESS_LIST` env overrides
/// (upstream compat.c:409-533).
///
/// The environment is process-global, so every test here serialises on
/// [`ENV_LOCK`] via [`env_lock`] and mutates through [`EnvGuard`], which
/// restores the previous value on drop.
mod env_list_overrides {
    use std::ffi::OsStr;

    use super::super::env_list;
    use super::super::negotiate::{choose_checksum_algorithm_in, read_vstring, write_vstring};
    use super::*;

    /// Reads the first vstring from a captured `negotiate` output buffer.
    fn first_vstring(bytes: &[u8]) -> String {
        let mut cursor = bytes;
        read_vstring(&mut cursor).unwrap()
    }

    // (a) Unset env - the default candidate order is untouched. This is the
    // regression guard that the default wire bytes never change.
    #[test]
    fn unset_env_keeps_default_order() {
        let _lock = env_lock();
        let _cs = EnvGuard::remove(CHECKSUM_ENV);
        let _cp = EnvGuard::remove(COMPRESS_ENV);

        assert!(env_list::checksum_candidates(false, false, 32).is_none());
        assert!(env_list::checksum_candidates(true, false, 32).is_none());
        assert!(env_list::compression_candidates(false, false).is_none());
        assert!(env_list::compression_candidates(true, false).is_none());
    }

    // (a') Unset env - a full negotiation advertises the built-in default list
    // byte-for-byte (client drops "none").
    #[test]
    fn unset_env_advertises_default_checksum_list() {
        let _lock = env_lock();
        let _cs = EnvGuard::remove(CHECKSUM_ENV);
        let _cp = EnvGuard::remove(COMPRESS_ENV);

        let protocol = ProtocolVersion::try_from(31).unwrap();
        let peer = test_peer_data(false);
        let mut stdin = &peer[..];
        let mut stdout = Vec::new();

        negotiate_capabilities(protocol, &mut stdin, &mut stdout, true, false, false, false)
            .unwrap();

        // Client omits the num == 0 ("none") entry, matching get_default_nno_list.
        assert_eq!(first_vstring(&stdout), "xxh128 xxh3 xxh64 md5 md4 sha1");
    }

    // (b) A checksum list restricts and reorders the candidate set.
    #[test]
    fn checksum_env_restricts_and_reorders() {
        let _lock = env_lock();
        let _cs = EnvGuard::set(CHECKSUM_ENV, OsStr::new("md5 xxh3"));

        let client = env_list::checksum_candidates(false, false, 32).unwrap();
        assert_eq!(client.candidates, vec!["md5", "xxh3"]);
        assert_eq!(client.advertised, "md5 xxh3");

        let server = env_list::checksum_candidates(true, false, 32).unwrap();
        assert_eq!(server.candidates, vec!["md5", "xxh3"]);
        assert_eq!(server.advertised, "md5 xxh3");
    }

    // (b') The restricted list flows onto the wire and drives selection: the
    // client picks its own first candidate that the server also offers.
    #[test]
    fn checksum_env_drives_client_selection() {
        let _lock = env_lock();
        let _cs = EnvGuard::set(CHECKSUM_ENV, OsStr::new("md5 xxh3"));
        let _cp = EnvGuard::remove(COMPRESS_ENV);

        let protocol = ProtocolVersion::try_from(31).unwrap();
        // Server advertises the full default checksum list.
        let peer = test_peer_data(false);
        let mut stdin = &peer[..];
        let mut stdout = Vec::new();

        let result =
            negotiate_capabilities(protocol, &mut stdin, &mut stdout, true, false, false, false)
                .unwrap();

        // Advertised list is the env order, not the built-in default.
        assert_eq!(first_vstring(&stdout), "md5 xxh3");
        // Client preference order (env order) wins: md5 before xxh3.
        assert_eq!(result.checksum, ChecksumAlgorithm::MD5);
    }

    // (c) A compression list likewise restricts and reorders candidates.
    #[test]
    fn compress_env_restricts_and_reorders() {
        let _lock = env_lock();
        let _cp = EnvGuard::set(COMPRESS_ENV, OsStr::new("zlib zlibx"));

        let client = env_list::compression_candidates(false, false).unwrap();
        assert_eq!(client.candidates, vec!["zlib", "zlibx"]);
        assert_eq!(client.advertised, "zlib zlibx");
    }

    // (c') The compression override flows onto the wire and drives selection.
    #[test]
    fn compress_env_drives_client_selection() {
        let _lock = env_lock();
        let _cs = EnvGuard::remove(CHECKSUM_ENV);
        let _cp = EnvGuard::set(COMPRESS_ENV, OsStr::new("zlib zlibx"));

        let protocol = ProtocolVersion::try_from(31).unwrap();
        // Server offers both zlib and zlibx (plus none).
        let server_lists = b"\x0Exxh128 md5 md4\x0Fzlibx zlib none";
        let mut stdin = &server_lists[..];
        let mut stdout = Vec::new();

        let result =
            negotiate_capabilities(protocol, &mut stdin, &mut stdout, true, true, false, false)
                .unwrap();

        // Second vstring is the compression list.
        let mut cursor = &stdout[..];
        let _checksum = read_vstring(&mut cursor).unwrap();
        let compress = read_vstring(&mut cursor).unwrap();
        assert_eq!(compress, "zlib zlibx");
        // Env order puts zlib first, so zlib wins over the server's zlibx-first list.
        assert_eq!(result.compression, CompressionAlgorithm::Zlib);
    }

    // (d) Unknown names are dropped; a value with a mix keeps only valid names.
    #[test]
    fn unknown_names_are_dropped() {
        let _lock = env_lock();
        let _cs = EnvGuard::set(CHECKSUM_ENV, OsStr::new("bogus md5 alsobad xxh3"));

        let over = env_list::checksum_candidates(false, false, 32).unwrap();
        assert_eq!(over.candidates, vec!["md5", "xxh3"]);
        assert_eq!(over.advertised, "md5 xxh3");
    }

    // (d') A value whose names are all unknown collapses to the INVALID
    // sentinel (upstream compat.c:327-328), which then fails negotiation.
    #[test]
    fn all_unknown_names_yield_invalid_sentinel() {
        let _lock = env_lock();
        let _cs = EnvGuard::set(CHECKSUM_ENV, OsStr::new("bogus notreal"));

        let over = env_list::checksum_candidates(false, false, 32).unwrap();
        assert!(over.candidates.is_empty());
        assert_eq!(over.advertised, "INVALID");

        // An INVALID list cannot match any remote offer, so selection errors -
        // upstream exits with RERR_UNSUPPORTED here. With no surviving own
        // candidate, upstream rebuilds the `Client list:` as " INVALID"
        // (compat.c:403-404).
        let err =
            choose_checksum_algorithm_in("xxh128 md5 md4", false, &over.candidates).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::Unsupported);
        assert_eq!(
            err.to_string(),
            "Failed to negotiate a checksum choice.\n\
             Server list: xxh128 md5 md4\n\
             Client list: INVALID"
        );
    }

    // Disjoint RSYNC_CHECKSUM_LIST on each side: the client offers only md5
    // while the server offers only md4, so there is no mutual choice. Upstream's
    // recv_negotiate_str prints the full three-line block on the client and
    // aborts with RERR_UNSUPPORTED (compat.c:381-406). This is the observable
    // remainder of the exit-4 negotiation failure.
    #[test]
    fn disjoint_checksum_lists_emit_full_failure_block() {
        let _lock = env_lock();
        let _cs = EnvGuard::set(CHECKSUM_ENV, OsStr::new("md5"));
        let _cp = EnvGuard::remove(COMPRESS_ENV);

        // The client's selection candidates come from its env override (md5).
        let over = env_list::checksum_candidates(false, false, 32).unwrap();
        assert_eq!(over.candidates, vec!["md5"]);

        // The server advertised only md4 - no mutual algorithm.
        let err = choose_checksum_algorithm_in("md4", false, &over.candidates).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::Unsupported);
        assert_eq!(
            err.to_string(),
            "Failed to negotiate a checksum choice.\n\
             Server list: md4\n\
             Client list: md5"
        );

        // upstream: compat.c:382 - the server side of the same mismatch keeps
        // the offered lists to itself (am_server && do_negotiated_strings) and
        // aborts on the headline alone.
        let server_err = choose_checksum_algorithm_in("md4", true, &over.candidates).unwrap_err();
        assert_eq!(
            server_err.to_string(),
            "Failed to negotiate a checksum choice."
        );
    }

    // The same disjoint-list failure driven through the full `negotiate_the_strings`
    // wire path: the client advertises its md5-only list, reads the server's
    // md4-only vstring, finds no match and surfaces upstream's three-line block.
    #[test]
    fn disjoint_checksum_lists_fail_through_negotiate() {
        let _lock = env_lock();
        let _cs = EnvGuard::set(CHECKSUM_ENV, OsStr::new("md5"));
        let _cp = EnvGuard::remove(COMPRESS_ENV);

        let protocol = crate::ProtocolVersion::try_from(32).unwrap();
        let mut server_list = Vec::new();
        write_vstring(&mut server_list, "md4").unwrap();
        let mut stdin = &server_list[..];
        let mut stdout = Vec::new();

        let err = crate::negotiate_capabilities(
            protocol,
            &mut stdin,
            &mut stdout,
            true,  // do_negotiation
            false, // send_compression
            false, // is_daemon_mode (SSH)
            false, // is_server = client
        )
        .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::Unsupported);
        assert_eq!(
            err.to_string(),
            "Failed to negotiate a checksum choice.\n\
             Server list: md4\n\
             Client list: md5"
        );
    }

    // (d'') Forward compatibility: an env list naming a checksum this build
    // does not know (e.g. sha256, proposed upstream PR #1007) drops the
    // unknown name and negotiates the surviving mutual choice - upstream
    // parse_nni_str() drops names get_nni_by_name() rejects (compat.c:295-306),
    // so `RSYNC_CHECKSUM_LIST="sha256 md5"` behaves exactly like "md5".
    #[test]
    fn env_list_with_unknown_sha256_negotiates_survivor() {
        let _lock = env_lock();
        let _cs = EnvGuard::set(CHECKSUM_ENV, OsStr::new("sha256 md5"));
        let _cp = EnvGuard::remove(COMPRESS_ENV);

        let over = env_list::checksum_candidates(false, false, 32).unwrap();
        assert_eq!(over.candidates, vec!["md5"]);
        assert_eq!(over.advertised, "md5");

        // Full round against a peer advertising the stock upstream list:
        // negotiation lands on md5 without error.
        let protocol = ProtocolVersion::try_from(31).unwrap();
        let peer = test_peer_data(false);
        let mut stdin = &peer[..];
        let mut stdout = Vec::new();
        let result =
            negotiate_capabilities(protocol, &mut stdin, &mut stdout, true, false, false, false)
                .unwrap();
        assert_eq!(result.checksum, ChecksumAlgorithm::MD5);
        assert_eq!(first_vstring(&stdout), "md5");
    }

    // Empty / whitespace-only values are treated as unset (default order).
    #[test]
    fn whitespace_only_env_is_treated_as_unset() {
        let _lock = env_lock();
        let _cs = EnvGuard::set(CHECKSUM_ENV, OsStr::new("   \t "));
        assert!(env_list::checksum_candidates(false, false, 32).is_none());
    }

    // Duplicate names are removed, keeping first occurrence (upstream dedup).
    #[test]
    fn duplicate_names_are_deduped() {
        let _lock = env_lock();
        let _cs = EnvGuard::set(CHECKSUM_ENV, OsStr::new("md5 xxh3 md5 xxh3"));
        let over = env_list::checksum_candidates(false, false, 32).unwrap();
        assert_eq!(over.candidates, vec!["md5", "xxh3"]);
    }

    // The "xxhash" alias is canonicalised to "xxh64" on the wire, matching
    // upstream's main_nni rewrite.
    #[test]
    fn xxhash_alias_is_canonicalised() {
        let _lock = env_lock();
        let _cs = EnvGuard::set(CHECKSUM_ENV, OsStr::new("xxhash md5"));
        let over = env_list::checksum_candidates(false, false, 32).unwrap();
        assert_eq!(over.candidates, vec!["xxh64", "md5"]);
        assert_eq!(over.advertised, "xxh64 md5");
    }

    // A mixed-case non-alias name keeps its original bytes on the wire while
    // still resolving case-insensitively for selection - upstream parse_nni_str
    // only rewrites recognised aliases, so "MD5" stays "MD5" (not "md5").
    #[test]
    fn mixed_case_non_alias_preserves_original_bytes() {
        let _lock = env_lock();
        let _cs = EnvGuard::set(CHECKSUM_ENV, OsStr::new("MD5 XXH3"));
        let over = env_list::checksum_candidates(false, false, 32).unwrap();
        // Advertised bytes preserve the operator's casing.
        assert_eq!(over.advertised, "MD5 XXH3");
        // Candidates are canonical for case-insensitive selection.
        assert_eq!(over.candidates, vec!["md5", "xxh3"]);

        // A mixed-case alias still canonicalises (case-insensitive match).
        let _cs2 = EnvGuard::set(CHECKSUM_ENV, OsStr::new("XxHaSh"));
        let alias = env_list::checksum_candidates(false, false, 32).unwrap();
        assert_eq!(alias.advertised, "xxh64");
        assert_eq!(alias.candidates, vec!["xxh64"]);
    }

    // The '&' separator scopes the value: client uses names before it, server
    // uses names after it (upstream getenv_nstr + parse_nni_str terminator).
    #[test]
    fn ampersand_scopes_client_and_server() {
        let _lock = env_lock();
        let _cs = EnvGuard::set(CHECKSUM_ENV, OsStr::new("md5 & xxh3 xxh128"));

        let client = env_list::checksum_candidates(false, false, 32).unwrap();
        assert_eq!(client.candidates, vec!["md5"]);

        let server = env_list::checksum_candidates(true, false, 32).unwrap();
        assert_eq!(server.candidates, vec!["xxh3", "xxh128"]);
    }

    // -- validate_choice_vs_env (upstream compat.c:426-449) --
    //
    // The server refuses a client-forced --checksum-choice/--compress-choice
    // whose algorithm is absent from RSYNC_CHECKSUM_LIST/RSYNC_COMPRESS_LIST.

    // (a) Unset env - any forced choice is accepted. Regression guard that the
    // default path performs no validation. upstream: compat.c:432-433 returns
    // early when list_str is NULL.
    #[test]
    fn validate_checksum_choice_accepts_when_env_unset() {
        let _lock = env_lock();
        let _cs = EnvGuard::remove(CHECKSUM_ENV);
        env_list::validate_checksum_choice("md5").expect("unset env accepts any choice");
        env_list::validate_checksum_choice("xxh128").expect("unset env accepts any choice");
    }

    // Whitespace-only env is treated as unset (upstream compat.c:435-436).
    #[test]
    fn validate_checksum_choice_accepts_when_env_blank() {
        let _lock = env_lock();
        let _cs = EnvGuard::set(CHECKSUM_ENV, OsStr::new("   "));
        env_list::validate_checksum_choice("md5").expect("blank env accepts any choice");
    }

    // (b) Env list set and the forced choice is a member - accepted.
    #[test]
    fn validate_checksum_choice_accepts_when_in_list() {
        let _lock = env_lock();
        let _cs = EnvGuard::set(CHECKSUM_ENV, OsStr::new("md5 xxh3"));
        env_list::validate_checksum_choice("md5").expect("md5 is in the list");
        env_list::validate_checksum_choice("xxh3").expect("xxh3 is in the list");
    }

    // (c) Env list set and the forced choice is NOT a member - refused with the
    // byte-exact upstream message and ErrorKind::Unsupported. The core exit-code
    // mapper turns Unsupported into RERR_UNSUPPORTED (exit 4), matching upstream
    // exit_cleanup(RERR_UNSUPPORTED) at compat.c:449. WHY exact text: it is
    // observable stderr forwarded to the client, so a drop-in must match it.
    #[test]
    fn validate_checksum_choice_refuses_when_not_in_list() {
        let _lock = env_lock();
        let _cs = EnvGuard::set(CHECKSUM_ENV, OsStr::new("xxh3 xxh128"));
        let err = env_list::validate_checksum_choice("md5").unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::Unsupported);
        assert_eq!(
            err.to_string(),
            "Your --checksum-choice value (md5) was refused by the server."
        );
    }

    // A value whose names are all unrecognised collapses to the INVALID
    // sentinel (empty candidate set), so every choice is refused - upstream
    // parse_nni_str yields "INVALID" and saw[num] is never set.
    #[test]
    fn validate_checksum_choice_refuses_when_list_all_invalid() {
        let _lock = env_lock();
        let _cs = EnvGuard::set(CHECKSUM_ENV, OsStr::new("bogus nope"));
        let err = env_list::validate_checksum_choice("md5").unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::Unsupported);
    }

    // (d) The compress counterpart: refusal message says "compress".
    #[test]
    fn validate_compress_choice_refuses_when_not_in_list() {
        let _lock = env_lock();
        let _cp = EnvGuard::set(COMPRESS_ENV, OsStr::new("zstd"));
        let err = env_list::validate_compress_choice("zlib").unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::Unsupported);
        assert_eq!(
            err.to_string(),
            "Your --compress-choice value (zlib) was refused by the server."
        );
    }

    #[test]
    fn validate_compress_choice_accepts_when_in_list() {
        let _lock = env_lock();
        let _cp = EnvGuard::set(COMPRESS_ENV, OsStr::new("zlib zlibx"));
        env_list::validate_compress_choice("zlib").expect("zlib is in the list");
        env_list::validate_compress_choice("zlibx").expect("zlibx is in the list");
    }

    // (e) MD4 special case. upstream compat.c:443-444 marks the archaic/busted/
    // old MD4 slots as seen when "md4" is in the list; oc-rsync collapses the
    // whole MD4 family into a single "md4", so a forced md4 choice is accepted
    // iff "md4" is present and refused otherwise.
    #[test]
    fn validate_checksum_choice_md4_in_list_accepts_md4() {
        let _lock = env_lock();
        let _cs = EnvGuard::set(CHECKSUM_ENV, OsStr::new("md4 md5"));
        env_list::validate_checksum_choice("md4").expect("md4 is in the list");
    }

    #[test]
    fn validate_checksum_choice_md4_absent_refuses_md4() {
        let _lock = env_lock();
        let _cs = EnvGuard::set(CHECKSUM_ENV, OsStr::new("md5 xxh3"));
        let err = env_list::validate_checksum_choice("md4").unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::Unsupported);
        assert_eq!(
            err.to_string(),
            "Your --checksum-choice value (md4) was refused by the server."
        );
    }

    // The '&' split applies during validation too: the server checks against the
    // portion after '&' (upstream getenv_nstr, compat.c:417-421). Here the
    // client half ("md5") would accept md5, but the server half ("xxh3") refuses.
    #[test]
    fn validate_uses_server_half_of_ampersand_scope() {
        let _lock = env_lock();
        let _cs = EnvGuard::set(CHECKSUM_ENV, OsStr::new("md5 & xxh3"));
        env_list::validate_checksum_choice("xxh3").expect("server half contains xxh3");
        let err = env_list::validate_checksum_choice("md5").unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::Unsupported);
    }

    // End-to-end: a server negotiating a client-forced --checksum-choice that
    // its RSYNC_CHECKSUM_LIST excludes aborts negotiation with the exact
    // refusal and ErrorKind::Unsupported (exit 4 in core).
    #[test]
    fn server_negotiation_refuses_forced_checksum_not_in_env() {
        let _lock = env_lock();
        let _cs = EnvGuard::set(CHECKSUM_ENV, OsStr::new("xxh3"));
        let _cp = EnvGuard::remove(COMPRESS_ENV);

        let protocol = ProtocolVersion::try_from(32).unwrap();
        // Forced checksum skips the checksum vstring; compression off means
        // nothing is exchanged, so stdin is empty.
        let mut stdin = &b""[..];
        let mut stdout = Vec::new();

        let err = negotiate_capabilities_with_override(
            protocol,
            &mut stdin,
            &mut stdout,
            &NegotiationConfig {
                do_negotiation: true,
                send_compression: false,
                is_daemon_mode: false,
                is_server: true,
                checksum_override: Some(ChecksumAlgorithm::MD5),
                compression_override: None,
                compression_level: crate::nstr::CLVL_NOT_SPECIFIED,
                write_batch: false,
            },
        )
        .unwrap_err();

        assert_eq!(err.kind(), std::io::ErrorKind::Unsupported);
        assert_eq!(
            err.to_string(),
            "Your --checksum-choice value (md5) was refused by the server."
        );
    }

    // The client never validates - only the server refuses (upstream am_server
    // guard). A client with the env set and a forced choice not in the list
    // still completes negotiation with the forced algorithm.
    #[test]
    fn client_does_not_validate_forced_checksum() {
        let _lock = env_lock();
        let _cs = EnvGuard::set(CHECKSUM_ENV, OsStr::new("xxh3"));
        let _cp = EnvGuard::remove(COMPRESS_ENV);

        let protocol = ProtocolVersion::try_from(32).unwrap();
        let mut stdin = &b""[..];
        let mut stdout = Vec::new();

        let result = negotiate_capabilities_with_override(
            protocol,
            &mut stdin,
            &mut stdout,
            &NegotiationConfig {
                do_negotiation: true,
                send_compression: false,
                is_daemon_mode: false,
                is_server: false,
                checksum_override: Some(ChecksumAlgorithm::MD5),
                compression_override: None,
                compression_level: crate::nstr::CLVL_NOT_SPECIFIED,
                write_batch: false,
            },
        )
        .expect("client does not validate the choice against the env list");

        assert_eq!(result.checksum, ChecksumAlgorithm::MD5);
    }

    // -- fallback-default validation on the non-negotiated path --
    //
    // upstream compat.c:541-565: even when the peer cannot negotiate strings,
    // negotiate_the_strings populates the env-restricted saw list and validates
    // the prefilled "md5"/"md4"/"zlib" default against it, aborting with
    // RERR_UNSUPPORTED when the operator's env excludes the default.

    // (a) Unit: env unset - the fallback default is accepted (strict no-op that
    // guards the common case). upstream: getenv_nstr returns NULL, saw stays the
    // built-in default order, which always contains md5/md4/zlib.
    #[test]
    fn validate_default_accepts_when_env_unset() {
        let _lock = env_lock();
        let _cs = EnvGuard::remove(CHECKSUM_ENV);
        let _cp = EnvGuard::remove(COMPRESS_ENV);
        env_list::validate_default_checksum("md5", false).expect("unset env accepts md5");
        env_list::validate_default_checksum("md5", true).expect("unset env accepts md5");
        env_list::validate_default_checksum("md4", true).expect("unset env accepts md4");
        env_list::validate_default_compress("zlib", true).expect("unset env accepts zlib");
    }

    // (b) Unit: env includes the default - accepted.
    #[test]
    fn validate_default_accepts_when_default_in_list() {
        let _lock = env_lock();
        let _cs = EnvGuard::set(CHECKSUM_ENV, OsStr::new("xxh3 md5"));
        let _cp = EnvGuard::set(COMPRESS_ENV, OsStr::new("zlib zlibx"));
        env_list::validate_default_checksum("md5", true).expect("md5 is in the list");
        env_list::validate_default_compress("zlib", true).expect("zlib is in the list");
    }

    // (c) Unit: env excludes the default - refused with upstream's
    // recv_negotiate_str wording and ErrorKind::Unsupported (exit 4 in core).
    #[test]
    fn validate_default_refuses_checksum_when_excluded() {
        let _lock = env_lock();
        let _cs = EnvGuard::set(CHECKSUM_ENV, OsStr::new("xxh3 xxh128"));
        let err = env_list::validate_default_checksum("md5", true).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::Unsupported);
        // upstream: compat.c:381-405 - the non-negotiated path prints the lists
        // on both sides; on the server tmpbuf ("md5") is the `Client list:` and
        // the env-restricted saw is the `Server list:`.
        assert_eq!(
            err.to_string(),
            "Failed to negotiate a checksum choice.\n\
             Client list: md5\n\
             Server list: xxh3 xxh128"
        );
    }

    #[test]
    fn validate_default_refuses_compress_when_excluded() {
        let _lock = env_lock();
        // `zlibx` is always compiled in (unlike the optional zstd/lz4 codecs),
        // so the rebuilt own-list line is deterministic across feature sets.
        let _cp = EnvGuard::set(COMPRESS_ENV, OsStr::new("zlibx"));
        let err = env_list::validate_default_compress("zlib", true).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::Unsupported);
        assert_eq!(
            err.to_string(),
            "Failed to negotiate a compress choice.\n\
             Client list: zlib\n\
             Server list: zlibx"
        );
    }

    /// Drives the non-negotiated branch (`do_negotiation = false`, peer lacks
    /// the 'v' capability). No wire I/O runs, so an empty stdin suffices.
    fn negotiate_nonego(is_server: bool, send_compression: bool) -> io::Result<NegotiationResult> {
        let protocol = ProtocolVersion::try_from(31).unwrap();
        let mut stdin = &b""[..];
        let mut stdout = Vec::new();
        negotiate_capabilities_with_override(
            protocol,
            &mut stdin,
            &mut stdout,
            &NegotiationConfig {
                do_negotiation: false,
                send_compression,
                is_daemon_mode: false,
                is_server,
                checksum_override: None,
                compression_override: None,
                compression_level: crate::nstr::CLVL_NOT_SPECIFIED,
                write_batch: false,
            },
        )
    }

    // (d) End-to-end common case: env unset, non-negotiated path returns the
    // md5 default unchanged. This is the strict no-op regression guard.
    #[test]
    fn nonego_returns_md5_default_when_env_unset() {
        let _lock = env_lock();
        let _cs = EnvGuard::remove(CHECKSUM_ENV);
        let _cp = EnvGuard::remove(COMPRESS_ENV);
        let result = negotiate_nonego(true, false).expect("unset env is a no-op");
        assert_eq!(result.checksum, ChecksumAlgorithm::MD5);
        assert_eq!(result.compression, CompressionAlgorithm::None);
    }

    // (e) End-to-end: server env excludes md5 + no forced choice + non-negotiated
    // path aborts with RERR_UNSUPPORTED (exit 4), where before it proceeded with
    // md5. WHY this matters: an operator RSYNC_CHECKSUM_LIST restriction must be
    // honoured against an old peer exactly as upstream honours it.
    #[test]
    fn nonego_refuses_md5_default_when_env_excludes_it() {
        let _lock = env_lock();
        let _cs = EnvGuard::set(CHECKSUM_ENV, OsStr::new("xxh3 xxh128"));
        let _cp = EnvGuard::remove(COMPRESS_ENV);
        let err = negotiate_nonego(true, false).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::Unsupported);
        assert_eq!(
            err.to_string(),
            "Failed to negotiate a checksum choice.\n\
             Client list: md5\n\
             Server list: xxh3 xxh128"
        );
    }

    // (f) End-to-end: env includes md5 - the default is returned.
    #[test]
    fn nonego_returns_md5_default_when_env_includes_it() {
        let _lock = env_lock();
        let _cs = EnvGuard::set(CHECKSUM_ENV, OsStr::new("md5 xxh3"));
        let _cp = EnvGuard::remove(COMPRESS_ENV);
        let result = negotiate_nonego(true, false).expect("md5 is in the list");
        assert_eq!(result.checksum, ChecksumAlgorithm::MD5);
    }

    // (g) End-to-end compress: with -z active, a server env excluding zlib aborts
    // the non-negotiated path.
    #[test]
    fn nonego_refuses_zlib_default_when_env_excludes_it() {
        let _lock = env_lock();
        let _cs = EnvGuard::remove(CHECKSUM_ENV);
        // `zlibx` is unconditionally compiled in, keeping the own-list line
        // deterministic regardless of the optional zstd/lz4 features.
        let _cp = EnvGuard::set(COMPRESS_ENV, OsStr::new("zlibx"));
        let err = negotiate_nonego(true, true).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::Unsupported);
        assert_eq!(
            err.to_string(),
            "Failed to negotiate a compress choice.\n\
             Client list: zlib\n\
             Server list: zlibx"
        );
    }

    // (h) End-to-end compress: env includes zlib - the default is returned.
    #[test]
    fn nonego_returns_zlib_default_when_env_includes_it() {
        let _lock = env_lock();
        let _cs = EnvGuard::remove(CHECKSUM_ENV);
        let _cp = EnvGuard::set(COMPRESS_ENV, OsStr::new("zlib zlibx"));
        let result = negotiate_nonego(true, true).expect("zlib is in the list");
        assert_eq!(result.compression, CompressionAlgorithm::Zlib);
    }

    // (i) A compress-list restriction that excludes zlib does NOT abort when
    // compression is off (send_compression = false): upstream gates the compress
    // recv on do_compression (compat.c:544), so no validation runs.
    #[test]
    fn nonego_ignores_compress_env_when_compression_off() {
        let _lock = env_lock();
        let _cs = EnvGuard::remove(CHECKSUM_ENV);
        let _cp = EnvGuard::set(COMPRESS_ENV, OsStr::new("zstd"));
        negotiate_nonego(true, false).expect("compress env is not checked when -z is off");
    }

    // (j) The check runs on the client too (upstream negotiate_the_strings is not
    // am_server-gated): a client whose env excludes md5 aborts against an old
    // peer just like the server.
    #[test]
    fn nonego_refuses_md5_default_on_client_side() {
        let _lock = env_lock();
        let _cs = EnvGuard::set(CHECKSUM_ENV, OsStr::new("xxh3"));
        let _cp = EnvGuard::remove(COMPRESS_ENV);
        let err = negotiate_nonego(false, false).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::Unsupported);
    }
}
