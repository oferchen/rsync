#![no_main]

//! Fuzz target for the daemon pre-auth response verifier and secrets parser.
//!
//! The daemon authentication path exposes two parsers to untrusted input
//! before any credentials are validated:
//!
//! - [`verify_client_response`] consumes the base64 response string sent by
//!   the client in reply to an `@RSYNCD: AUTHREQD` challenge. The verifier
//!   length-disambiguates the response across MD4, MD5, SHA-1, SHA-256, and
//!   SHA-512 digests and selects the protocol-appropriate algorithm.
//! - [`SecretsFile::parse`] reads `username:password` entries from an admin
//!   secrets file. Malformed lines must surface as `io::Error` rather than
//!   panic the daemon at startup.
//!
//! Coverage is split between the two parsers based on the first input byte
//! so libFuzzer can fan out across both attack surfaces under a single
//! corpus. Any panic on malformed bytes is a finding.
//!
//! # Running
//!
//! ```bash
//! cargo +nightly fuzz run auth_response
//! ```

use libfuzzer_sys::fuzz_target;

use daemon::auth::{SecretsFile, verify_client_response};

fuzz_target!(|data: &[u8]| {
    if data.is_empty() {
        return;
    }

    // Split the input: first byte selects which parser to hit, the rest is
    // the fuzz payload. This keeps both surfaces on the same corpus while
    // letting libFuzzer reward coverage on either path independently.
    let (selector, payload) = data.split_first().expect("non-empty checked above");

    match selector & 0b11 {
        0 => fuzz_verify_response(payload),
        1 => fuzz_secrets_parse(payload),
        _ => {
            // Exercise both for higher-arity bytes so unused selector
            // bits still drive useful coverage.
            fuzz_verify_response(payload);
            fuzz_secrets_parse(payload);
        }
    }
});

/// Drives [`verify_client_response`] with arbitrary password, challenge, and
/// response substrings sliced out of the payload, plus a fuzzed protocol
/// version selector.
fn fuzz_verify_response(payload: &[u8]) {
    // Need at least one byte to seed the protocol selector.
    let Some((proto_byte, rest)) = payload.split_first() else {
        return;
    };

    // Split the remainder into three roughly equal slices for password,
    // challenge, and response so the fuzzer can independently mutate each.
    let n = rest.len();
    let third = n / 3;
    let (password, after_pw) = rest.split_at(third);
    let split = after_pw.len() / 2;
    let (challenge_bytes, response_bytes) = after_pw.split_at(split);

    // The challenge and response are expected to be `&str` on the wire.
    let Ok(challenge) = std::str::from_utf8(challenge_bytes) else {
        return;
    };
    let Ok(response) = std::str::from_utf8(response_bytes) else {
        return;
    };

    // Cycle through legitimate protocol versions (28..=32) plus an
    // out-of-range probe to exercise the unknown-protocol branch.
    let protocol_version = match proto_byte % 8 {
        0 => None,
        1 => Some(28),
        2 => Some(29),
        3 => Some(30),
        4 => Some(31),
        5 => Some(32),
        6 => Some(0),
        _ => Some(255),
    };

    let _ = verify_client_response(password, challenge, response, protocol_version);
}

/// Drives [`SecretsFile::parse`] with arbitrary UTF-8 input. The parser is
/// reached when the daemon loads `/etc/rsyncd.secrets` and must surface
/// malformed lines as errors rather than panic.
fn fuzz_secrets_parse(payload: &[u8]) {
    let Ok(text) = std::str::from_utf8(payload) else {
        return;
    };
    let _ = SecretsFile::parse(text);
}
