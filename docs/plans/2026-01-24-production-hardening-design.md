# Production Hardening & Code Quality Design

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Fix compression negotiation bug, achieve 95% test coverage on critical modules, and extract parameter structs for maintainability.

**Architecture:** Fix daemon mode unidirectional negotiation flow, then expand test coverage for protocol-critical and engine execution modules, followed by parameter struct extraction for functions with 8+ arguments.

**Tech Stack:** Rust, proptest for property-based testing, tempfile for filesystem mocking

---

## Phase 1: Compression Bug Fix

### Task 1.1: Create Feature Branch
**Files:** None (git operation)

**Step 1:** Create and checkout feature branch
```bash
git checkout -b fix/compression-negotiation
```

**Step 2:** Verify clean state
```bash
git status
```

---

### Task 1.2: Fix negotiate_capabilities Daemon Mode

**Files:**
- Modify: `crates/protocol/src/negotiation/capabilities.rs:265-376`

**Step 1:** Read current implementation to understand structure

**Step 2:** Replace lines 323-376 with daemon-mode-aware logic:

```rust
// Daemon mode: unidirectional flow
// SSH mode: bidirectional flow
if is_daemon_mode {
    if is_server {
        // Daemon server: SEND lists only, use defaults for local selection
        let checksum_list = SUPPORTED_CHECKSUMS.join(" ");
        debug_log!(Proto, 2, "daemon server sending checksum list: {}", checksum_list);
        write_vstring(stdout, &checksum_list)?;

        if send_compression {
            let compression_list = supported_compressions().join(" ");
            debug_log!(Proto, 2, "daemon server sending compression list: {}", compression_list);
            write_vstring(stdout, &compression_list)?;
        }

        stdout.flush()?;

        // Server uses first algorithm from its own list as default
        let checksum = ChecksumAlgorithm::XXH128;
        let compression = if send_compression {
            supported_compressions()
                .first()
                .and_then(|s| CompressionAlgorithm::parse(s).ok())
                .unwrap_or(CompressionAlgorithm::None)
        } else {
            CompressionAlgorithm::None
        };

        debug_log!(
            Proto, 1,
            "daemon server using checksum={}, compression={}",
            checksum.as_str(), compression.as_str()
        );
        return Ok(NegotiationResult { checksum, compression });
    } else {
        // Daemon client: READ lists only, don't send
        let remote_checksum_list = read_vstring(stdin)?;
        debug_log!(Proto, 2, "daemon client received checksum list: {}", remote_checksum_list);

        let remote_compression_list = if send_compression {
            let list = read_vstring(stdin)?;
            debug_log!(Proto, 2, "daemon client received compression list: {}", list);
            Some(list)
        } else {
            None
        };

        let checksum = choose_checksum_algorithm(&remote_checksum_list)?;
        let compression = if let Some(ref list) = remote_compression_list {
            choose_compression_algorithm(list)?
        } else {
            CompressionAlgorithm::None
        };

        debug_log!(
            Proto, 1,
            "daemon client selected checksum={}, compression={}",
            checksum.as_str(), compression.as_str()
        );
        return Ok(NegotiationResult { checksum, compression });
    }
}

// SSH mode: bidirectional exchange (existing behavior)
let checksum_list = SUPPORTED_CHECKSUMS.join(" ");
debug_log!(Proto, 2, "sending checksum list: {}", checksum_list);
write_vstring(stdout, &checksum_list)?;

if send_compression {
    let compression_list = supported_compressions().join(" ");
    debug_log!(Proto, 2, "sending compression list: {}", compression_list);
    write_vstring(stdout, &compression_list)?;
}

stdout.flush()?;

let remote_checksum_list = read_vstring(stdin)?;
debug_log!(Proto, 2, "received checksum list: {}", remote_checksum_list);

let remote_compression_list = if send_compression {
    let list = read_vstring(stdin)?;
    debug_log!(Proto, 2, "received compression list: {}", list);
    Some(list)
} else {
    None
};

let checksum = choose_checksum_algorithm(&remote_checksum_list)?;
let compression = if let Some(ref list) = remote_compression_list {
    choose_compression_algorithm(list)?
} else {
    CompressionAlgorithm::None
};

debug_log!(
    Proto, 1,
    "negotiated checksum={}, compression={}",
    checksum.as_str(), compression.as_str()
);
Ok(NegotiationResult { checksum, compression })
```

**Step 3:** Run tests
```bash
cargo test -p protocol --all-features
```

**Step 4:** Commit
```bash
git add crates/protocol/src/negotiation/capabilities.rs
git commit -m "fix(protocol): implement unidirectional negotiation for daemon mode"
```

---

### Task 1.3: Add Unit Tests for Daemon Mode Negotiation

**Files:**
- Modify: `crates/protocol/src/negotiation/capabilities.rs` (tests module)

**Step 1:** Add daemon server test (sends only, no read)

```rust
#[test]
fn test_daemon_mode_server_sends_only() {
    let protocol = ProtocolVersion::try_from(31).unwrap();
    let mut stdin = &b""[..]; // Empty - should NOT be read
    let mut stdout = Vec::new();

    let result = negotiate_capabilities(
        protocol,
        &mut stdin,
        &mut stdout,
        true,  // do_negotiation
        true,  // send_compression
        true,  // is_daemon_mode
        true,  // is_server
    )
    .unwrap();

    // Server should have sent lists
    assert!(!stdout.is_empty(), "daemon server should send lists");

    // Verify vstring format in output
    let output = String::from_utf8_lossy(&stdout);
    assert!(output.contains("xxh128") || output.contains("md5"),
            "should contain checksum algorithms");

    // Server uses defaults for its own selection
    assert_eq!(result.checksum, ChecksumAlgorithm::XXH128);
}
```

**Step 2:** Add daemon client test (reads only, no send)

```rust
#[test]
fn test_daemon_mode_client_reads_only() {
    let protocol = ProtocolVersion::try_from(31).unwrap();

    // Server sends these lists
    let server_data = {
        let mut buf = Vec::new();
        write_vstring(&mut buf, "xxh128 xxh3 md5 md4").unwrap();
        write_vstring(&mut buf, "zstd zlibx zlib none").unwrap();
        buf
    };

    let mut stdin = &server_data[..];
    let mut stdout = Vec::new();

    let result = negotiate_capabilities(
        protocol,
        &mut stdin,
        &mut stdout,
        true,  // do_negotiation
        true,  // send_compression
        true,  // is_daemon_mode
        false, // is_server (client)
    )
    .unwrap();

    // Client should NOT have sent anything
    assert!(stdout.is_empty(), "daemon client should not send data");

    // Client selects first mutually supported algorithm
    assert_eq!(result.checksum, ChecksumAlgorithm::XXH128);
}
```

**Step 3:** Add test for daemon client without compression

```rust
#[test]
fn test_daemon_mode_client_no_compression() {
    let protocol = ProtocolVersion::try_from(31).unwrap();

    // Server sends only checksum list
    let server_data = {
        let mut buf = Vec::new();
        write_vstring(&mut buf, "md5 md4 sha1").unwrap();
        buf
    };

    let mut stdin = &server_data[..];
    let mut stdout = Vec::new();

    let result = negotiate_capabilities(
        protocol,
        &mut stdin,
        &mut stdout,
        true,  // do_negotiation
        false, // send_compression = false
        true,  // is_daemon_mode
        false, // is_server (client)
    )
    .unwrap();

    assert!(stdout.is_empty());
    assert_eq!(result.checksum, ChecksumAlgorithm::MD5);
    assert_eq!(result.compression, CompressionAlgorithm::None);
}
```

**Step 4:** Run tests
```bash
cargo test -p protocol --all-features
```

**Step 5:** Commit
```bash
git add crates/protocol/src/negotiation/capabilities.rs
git commit -m "test(protocol): add unit tests for daemon mode negotiation"
```

---

### Task 1.4: Add Integration Test for Daemon Compression

**Files:**
- Create: `crates/protocol/tests/daemon_negotiation.rs`

**Step 1:** Create integration test file

```rust
//! Integration tests for daemon mode capability negotiation.

use std::io::{Read, Write};
use std::thread;
use std::sync::mpsc;

use protocol::{negotiate_capabilities, ProtocolVersion};

/// Simulates daemon server and client negotiation over a pipe.
#[test]
fn test_daemon_negotiation_end_to_end() {
    let (mut server_tx, mut client_rx) = std::io::pipe().unwrap();
    let (mut client_tx, mut server_rx) = std::io::pipe().unwrap();

    let protocol = ProtocolVersion::try_from(31).unwrap();

    // Server thread
    let server_handle = thread::spawn(move || {
        negotiate_capabilities(
            protocol,
            &mut server_rx,
            &mut server_tx,
            true,  // do_negotiation
            true,  // send_compression
            true,  // is_daemon_mode
            true,  // is_server
        )
    });

    // Client in main thread
    let client_result = negotiate_capabilities(
        protocol,
        &mut client_rx,
        &mut client_tx,
        true,  // do_negotiation
        true,  // send_compression
        true,  // is_daemon_mode
        false, // is_server (client)
    );

    let server_result = server_handle.join().unwrap();

    assert!(client_result.is_ok());
    assert!(server_result.is_ok());

    // Both should have valid algorithms selected
    let client = client_result.unwrap();
    let server = server_result.unwrap();

    // Client reads server's list and selects; server uses defaults
    // They may differ but both should be valid
    assert_ne!(client.checksum.as_str(), "");
}
```

**Step 2:** Run integration tests
```bash
cargo test -p protocol --test daemon_negotiation
```

**Step 3:** Commit
```bash
git add crates/protocol/tests/daemon_negotiation.rs
git commit -m "test(protocol): add integration test for daemon negotiation"
```

---

### Task 1.5: Verify Against Real Rsync Servers

**Files:** None (manual verification)

**Step 1:** Build release binary
```bash
cargo build --release --all-features
```

**Step 2:** Test against kernel.org with compression
```bash
./target/release/oc-rsync -avz rsync://rsync.kernel.org/pub/README /tmp/test_compression/
```

**Step 3:** Test against Alpine mirrors
```bash
./target/release/oc-rsync -avz rsync://rsync.alpinelinux.org/alpine/v3.21/releases/x86_64/ /tmp/alpine_test/
```

**Step 4:** Verify no "unknown multiplexed message code" errors

**Step 5:** Commit verification notes
```bash
git commit --allow-empty -m "test: verified compression works against kernel.org and Alpine mirrors"
```

---

## Phase 2: Protocol Test Coverage (95%)

### Task 2.1: Add NDX Codec Tests

**Files:**
- Modify: `crates/protocol/src/codec/ndx.rs`

Add comprehensive tests for:
- NDX_DONE encoding/decoding (protocol < 30 vs >= 30)
- Positive index delta encoding
- Negative index encoding
- State tracking across multiple calls
- Boundary values (0, 1, 127, 128, 255, 65535)

---

### Task 2.2: Add Varint/Vstring Codec Tests

**Files:**
- Modify: `crates/protocol/src/varint.rs`
- Modify: `crates/protocol/src/negotiation/capabilities.rs`

Add tests for:
- varint boundaries (127, 128, 16383, 16384)
- vstring 1-byte vs 2-byte format
- Maximum length handling
- Invalid UTF-8 rejection

---

### Task 2.3: Add Message Code Tests

**Files:**
- Modify: `crates/protocol/src/envelope/message_code.rs`

Add tests for all 18 message codes:
- Parse from u8
- Convert to u8
- Display formatting
- Unknown code handling

---

### Task 2.4: Add Protocol Version Selection Tests

**Files:**
- Modify: `crates/protocol/src/version/select.rs`

Add tests for:
- Version negotiation (client advertises X, server responds Y)
- Minimum/maximum version enforcement
- Invalid version rejection

---

### Task 2.5: Add Property-Based Roundtrip Tests

**Files:**
- Create: `crates/protocol/tests/codec_properties.rs`

Use proptest for:
- Any valid NDX value roundtrips correctly
- Any valid varint roundtrips correctly
- Any valid vstring roundtrips correctly

---

## Phase 3: Engine Test Coverage (95%)

### Task 3.1: Extract Test Helpers

**Files:**
- Create: `crates/engine/src/local_copy/tests/helpers.rs`

Extract common patterns:
- `TestFixture` for temp directory setup
- `nz64(val)` helper for NonZeroU64
- `assert_file_eq()` for content comparison
- `create_test_tree()` for standard structures

---

### Task 3.2-3.6: Add Tests for Execute Modules

For each of: `execute_basic.rs`, `execute_skip.rs`, `execute_directories.rs`, `execute_special.rs`, `execute_sparse.rs`

- Unit tests for each public function
- Error handling paths
- Platform-specific behavior (`#[cfg(unix)]`)

---

## Phase 4: Code Quality

### Task 4.1: Extract ProtocolSetupConfig

**Files:**
- Modify: `crates/transfer/src/setup.rs`

Create struct to replace 8 parameters.

---

### Task 4.2: Extract BasisFileConfig

**Files:**
- Modify: `crates/transfer/src/receiver.rs`

Create struct for `find_basis_file()` parameters.

---

### Task 4.3: Extract DeltaGeneratorConfig

**Files:**
- Modify: `crates/transfer/src/generator.rs`

Create struct for `generate_delta_from_signature()` parameters.

---

### Task 4.4: Update Rustdoc for Modified APIs

**Files:**
- All files modified in Phase 4

Add comprehensive rustdoc for new structs and modified functions.

---

## Verification

After each phase:
```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --no-deps -- -D warnings
cargo nextest run --workspace --all-features
```

After all phases:
```bash
cargo llvm-cov --workspace --all-features --html
# Verify 95% coverage in target/llvm-cov/html/index.html
```
