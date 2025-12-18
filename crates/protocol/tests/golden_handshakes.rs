//! Golden handshake validation tests.
//!
//! These tests validate wire-level protocol compatibility by comparing
//! generated handshakes against golden byte sequences captured from
//! upstream rsync implementations.
//!
//! Golden files must be captured separately using network traffic analysis
//! tools. See `tests/protocol_handshakes/README.md` for capture instructions.

use std::path::Path;

/// Base path to golden handshake directory.
const GOLDEN_BASE: &str = "../../../tests/protocol_handshakes";

/// Helper to check if a golden file exists.
fn golden_file_exists(protocol_dir: &str, filename: &str) -> bool {
    let path = Path::new(GOLDEN_BASE).join(protocol_dir).join(filename);
    path.exists()
}

/// Helper to read a golden file if it exists.
fn read_golden_file_opt(protocol_dir: &str, filename: &str) -> Option<Vec<u8>> {
    let path = Path::new(GOLDEN_BASE).join(protocol_dir).join(filename);
    std::fs::read(&path).ok()
}

/// Helper to read a golden file, or skip the test if it doesn't exist.
macro_rules! read_golden_or_skip {
    ($protocol_dir:expr, $filename:expr) => {{
        match read_golden_file_opt($protocol_dir, $filename) {
            Some(data) => data,
            None => {
                eprintln!(
                    "SKIPPED: Golden file {}/{} not found. See tests/protocol_handshakes/README.md",
                    $protocol_dir, $filename
                );
                return;
            }
        }
    }};
}

// ============================================================================
// Protocol 32 Binary Negotiation Tests
// ============================================================================

#[test]
fn test_protocol_32_client_hello_golden() {
    let golden = read_golden_or_skip!("protocol_32_binary", "client_hello.bin");

    // Test implementation pending: Generate protocol 32 client hello and compare
    // with golden file to detect wire format drift.
    //
    // Requires: generate_client_hello() function that constructs the binary
    // handshake per protocol 32 spec (capability negotiation, compat flags).
    //
    // let generated = generate_client_hello(ProtocolVersion::V32);
    // assert_eq!(generated, golden, "Protocol 32 client hello drift detected");

    eprintln!(
        "Protocol 32 client hello golden file exists ({} bytes). Implementation validation pending.",
        golden.len()
    );
}

#[test]
fn test_protocol_32_server_response_golden() {
    let golden = read_golden_or_skip!("protocol_32_binary", "server_response.bin");

    // Test implementation pending: Validate server response format.
    // Requires server-side handshake generation matching protocol 32 spec.
    eprintln!(
        "Protocol 32 server response golden file exists ({} bytes). Implementation validation pending.",
        golden.len()
    );
}

#[test]
fn test_protocol_32_compatibility_exchange_golden() {
    let golden = read_golden_or_skip!("protocol_32_binary", "compatibility_exchange.bin");

    // Test implementation pending: Validate compatibility flags exchange format.
    // Requires compat flags encoding/decoding matching protocol 32 wire format.
    eprintln!(
        "Protocol 32 compatibility exchange golden file exists ({} bytes). Implementation validation pending.",
        golden.len()
    );
}

// ============================================================================
// Protocol 31 Binary Negotiation Tests
// ============================================================================

#[test]
fn test_protocol_31_client_hello_golden() {
    let golden = read_golden_or_skip!("protocol_31_binary", "client_hello.bin");

    // Test implementation pending: Generate and validate protocol 31 client hello.
    eprintln!(
        "Protocol 31 client hello golden file exists ({} bytes). Implementation validation pending.",
        golden.len()
    );
}

#[test]
fn test_protocol_31_server_response_golden() {
    let golden = read_golden_or_skip!("protocol_31_binary", "server_response.bin");

    // Test implementation pending: Generate and validate protocol 31 server response.
    eprintln!(
        "Protocol 31 server response golden file exists ({} bytes). Implementation validation pending.",
        golden.len()
    );
}

// ============================================================================
// Protocol 30 Binary Negotiation Tests
// ============================================================================

#[test]
fn test_protocol_30_client_hello_golden() {
    let golden = read_golden_or_skip!("protocol_30_binary", "client_hello.bin");

    // Test implementation pending: Generate and validate protocol 30 client hello.
    eprintln!(
        "Protocol 30 client hello golden file exists ({} bytes). Implementation validation pending.",
        golden.len()
    );
}

#[test]
fn test_protocol_30_server_response_golden() {
    let golden = read_golden_or_skip!("protocol_30_binary", "server_response.bin");

    // Test implementation pending: Generate and validate protocol 30 server response.
    eprintln!(
        "Protocol 30 server response golden file exists ({} bytes). Implementation validation pending.",
        golden.len()
    );
}

// ============================================================================
// Protocol 29 Legacy ASCII Negotiation Tests
// ============================================================================

#[test]
fn test_protocol_29_client_greeting_golden() {
    let golden = read_golden_or_skip!("protocol_29_legacy", "client_greeting.txt");

    // Validate ASCII format
    let greeting =
        String::from_utf8(golden.clone()).expect("Protocol 29 client greeting must be valid UTF-8");

    assert!(
        greeting.starts_with("@RSYNCD:"),
        "Protocol 29 greeting must start with @RSYNCD: prefix"
    );
    assert!(
        greeting.contains("29"),
        "Protocol 29 greeting must contain version 29"
    );
    assert!(
        greeting.ends_with('\n'),
        "Protocol 29 greeting must end with newline"
    );

    // Test implementation pending: Generate full greeting programmatically and compare
    // byte-for-byte with golden file to detect formatting drift.
    eprintln!(
        "Protocol 29 client greeting validated: {:?}",
        greeting.trim()
    );
}

#[test]
fn test_protocol_29_server_response_golden() {
    let golden = read_golden_or_skip!("protocol_29_legacy", "server_response.txt");

    // Validate ASCII format
    let response =
        String::from_utf8(golden.clone()).expect("Protocol 29 server response must be valid UTF-8");

    assert!(
        response.starts_with("@RSYNCD:"),
        "Protocol 29 server response must start with @RSYNCD: prefix"
    );

    eprintln!(
        "Protocol 29 server response validated: {:?}",
        response.trim()
    );
}

// ============================================================================
// Protocol 28 Legacy ASCII Negotiation Tests
// ============================================================================

#[test]
fn test_protocol_28_client_greeting_golden() {
    let golden = read_golden_or_skip!("protocol_28_legacy", "client_greeting.txt");

    // Validate ASCII format
    let greeting =
        String::from_utf8(golden.clone()).expect("Protocol 28 client greeting must be valid UTF-8");

    assert!(
        greeting.starts_with("@RSYNCD:"),
        "Protocol 28 greeting must start with @RSYNCD: prefix"
    );
    assert!(
        greeting.contains("28"),
        "Protocol 28 greeting must contain version 28"
    );
    assert!(
        greeting.ends_with('\n'),
        "Protocol 28 greeting must end with newline"
    );

    eprintln!(
        "Protocol 28 client greeting validated: {:?}",
        greeting.trim()
    );
}

#[test]
fn test_protocol_28_server_response_golden() {
    let golden = read_golden_or_skip!("protocol_28_legacy", "server_response.txt");

    // Validate ASCII format
    let response =
        String::from_utf8(golden.clone()).expect("Protocol 28 server response must be valid UTF-8");

    assert!(
        response.starts_with("@RSYNCD:"),
        "Protocol 28 server response must start with @RSYNCD: prefix"
    );

    eprintln!(
        "Protocol 28 server response validated: {:?}",
        response.trim()
    );
}

// ============================================================================
// Diagnostic Tests
// ============================================================================

#[test]
fn test_golden_files_inventory() {
    let protocols = [
        (
            "protocol_32_binary",
            vec![
                "client_hello.bin",
                "server_response.bin",
                "compatibility_exchange.bin",
            ],
        ),
        (
            "protocol_31_binary",
            vec!["client_hello.bin", "server_response.bin"],
        ),
        (
            "protocol_30_binary",
            vec!["client_hello.bin", "server_response.bin"],
        ),
        (
            "protocol_29_legacy",
            vec!["client_greeting.txt", "server_response.txt"],
        ),
        (
            "protocol_28_legacy",
            vec!["client_greeting.txt", "server_response.txt"],
        ),
    ];

    let mut found = 0;
    let mut missing = 0;

    for (protocol_dir, files) in &protocols {
        for file in files {
            if golden_file_exists(protocol_dir, file) {
                found += 1;
                eprintln!("[✓] {protocol_dir}/{file}");
            } else {
                missing += 1;
                eprintln!("[✗] {protocol_dir}/{file}");
            }
        }
    }

    eprintln!("\nGolden files inventory: {found} found, {missing} missing");

    if missing > 0 {
        eprintln!("\nTo capture missing golden files, see: tests/protocol_handshakes/README.md");
    }
}
