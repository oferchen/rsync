#![no_main]

//! Fuzz target for legacy daemon greeting parsing.
//!
//! Tests the protocol version negotiation message parsing.
//! A malicious server could send crafted greetings to exploit
//! parsing vulnerabilities.

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // Test byte-based greeting parsing (re-exported at crate root)
    let _ = protocol::parse_legacy_daemon_greeting_bytes(data);
    let _ = protocol::parse_legacy_daemon_greeting_bytes_details(data);
    let _ = protocol::parse_legacy_daemon_greeting_bytes_owned(data);

    // Test error/warning message parsing
    let _ = protocol::parse_legacy_error_message_bytes(data);
    let _ = protocol::parse_legacy_warning_message_bytes(data);

    // Test string-based parsing if data is valid UTF-8
    if let Ok(s) = std::str::from_utf8(data) {
        let _ = protocol::parse_legacy_daemon_greeting(s);
        let _ = protocol::parse_legacy_daemon_message(s);
        let _ = protocol::parse_legacy_error_message(s);
        let _ = protocol::parse_legacy_warning_message(s);
    }
});
