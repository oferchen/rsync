# Interoperability Testing Guide

This document describes the comprehensive interoperability test suite for oc-rsync, covering both directions of client-daemon communication.

## Overview

The interop test suite consists of two complementary test files:

1. **`daemon_client_interop.rs`** - Tests oc-rsync client connecting to upstream rsync daemon
2. **`upstream_client_to_oc_daemon_interop.rs`** - Tests upstream rsync client connecting to oc-rsync daemon ⭐ NEW

Together, these tests ensure complete protocol compatibility with upstream rsync across multiple versions.

## Test Architecture

### Test Infrastructure

Both test suites use similar infrastructure:

- **Daemon Management**: `UpstreamDaemon` and `OcDaemon` helper structs manage daemon lifecycle
- **Port Allocation**: Each test uses a unique port (18873+) to avoid conflicts
- **Temporary Directories**: All tests use `tempfile::tempdir()` for isolation
- **Graceful Cleanup**: Daemon processes are killed on drop, ports are released

### Supported Upstream Versions

Tests verify compatibility with:

- **rsync 3.4.1** - Latest stable, protocol 31
- **rsync 3.1.3** - Common production version, protocol 30/31
- **rsync 3.0.9** - Older widely-deployed version, protocol 30

## Prerequisites

### Building Upstream rsync Binaries

Upstream rsync binaries must be built and installed in the repository:

```bash
# Create interop directory structure
mkdir -p target/interop/{upstream-src,upstream-install}

# Build rsync 3.4.1
cd target/interop/upstream-src
wget https://download.samba.org/pub/rsync/src/rsync-3.4.1.tar.gz
tar xzf rsync-3.4.1.tar.gz
cd rsync-3.4.1
./configure --prefix=/home/ofer/rsync/target/interop/upstream-install/3.4.1
make
make install

# Build rsync 3.1.3
cd ../
wget https://download.samba.org/pub/rsync/src/rsync-3.1.3.tar.gz
tar xzf rsync-3.1.3.tar.gz
cd rsync-3.1.3
./configure --prefix=/home/ofer/rsync/target/interop/upstream-install/3.1.3
make
make install

# Build rsync 3.0.9
cd ../
wget https://download.samba.org/pub/rsync/src/rsync-3.0.9.tar.gz
tar xzf rsync-3.0.9.tar.gz
cd rsync-3.0.9
./configure --prefix=/home/ofer/rsync/target/interop/upstream-install/3.0.9
make
make install

cd ../../../../
```

### Building oc-rsync

```bash
# Release build (preferred for performance testing)
cargo build --release

# Debug build (fallback)
cargo build
```

## Running Tests

### Quick Start

```bash
# Run all upstream client → oc-rsync daemon tests
bash tools/run_upstream_client_interop_tests.sh

# List all available tests
bash tools/run_upstream_client_interop_tests.sh --list

# Run specific test
bash tools/run_upstream_client_interop_tests.sh --test test_upstream_3_4_1_client_handshake

# Verbose output
bash tools/run_upstream_client_interop_tests.sh --verbose
```

### Manual Test Execution

```bash
# Run all tests (with ignored tests included)
cargo test --package core --test upstream_client_to_oc_daemon_interop -- --ignored --show-output

# Run specific test
cargo test --package core --test upstream_client_to_oc_daemon_interop test_upstream_3_4_1_client_handshake -- --ignored --show-output

# Run only tests that don't require binaries (basic checks)
cargo test --package core --test upstream_client_to_oc_daemon_interop
```

## Test Categories

### 1. Daemon Startup Tests

Verify basic daemon functionality:

- `test_oc_daemon_starts_and_accepts_connections` - Smoke test for daemon startup
- `test_oc_daemon_sends_protocol_greeting` - Verify @RSYNCD: greeting format
- `test_oc_daemon_shutdown_cleanup` - Clean process termination

### 2. Client Compatibility Tests

Protocol negotiation with different upstream versions:

- `test_upstream_3_4_1_client_handshake` - Latest upstream client (protocol 31)
- `test_upstream_3_1_3_client_handshake` - Common production version (protocol 30/31)
- `test_upstream_3_0_9_client_handshake` - Older version (protocol 30)

### 3. File Transfer Tests (Pull)

Upstream client pulling files from oc-rsync daemon:

- `test_pull_single_file_from_oc_daemon` - Single file transfer
- `test_pull_directory_tree_from_oc_daemon` - Recursive directory transfer
- `test_pull_large_file_from_oc_daemon` - 1MB+ file transfer
- `test_pull_files_with_special_chars_from_oc_daemon` - Path encoding

### 4. File Transfer Tests (Push)

Upstream client pushing files to oc-rsync daemon:

- `test_push_single_file_to_oc_daemon` - Single file upload
- `test_push_directory_tree_to_oc_daemon` - Recursive upload

### 5. Metadata Preservation Tests

Verify file attributes are preserved:

- `test_pull_preserves_permissions` - Unix permissions (Unix only)
- `test_pull_preserves_mtime` - Modification times

### 6. Protocol-Level Tests

Low-level protocol verification:

- `test_module_listing_from_upstream_client` - #list request/response
- `test_manual_protocol_handshake_with_oc_daemon` - Raw protocol handshake

### 7. Error Handling Tests

Verify proper error responses:

- `test_error_nonexistent_module_from_upstream_client` - @ERROR for unknown module
- `test_error_connection_refused` - Connection timeout handling

### 8. Compression and Checksum Tests

Algorithm negotiation:

- `test_pull_with_compression` - zlib/zstd compression
- `test_pull_with_checksum_algorithm` - Checksum algorithm selection

### 9. Stress and Edge Case Tests

Boundary conditions:

- `test_pull_many_small_files` - 100+ small files
- `test_pull_empty_file` - Zero-length file
- `test_pull_whitespace_only_file` - Whitespace content

## Test Patterns

### Daemon Lifecycle Pattern

```rust
let daemon = OcDaemon::start(port).expect("start oc-rsync daemon");
daemon.wait_ready(Duration::from_secs(5)).expect("daemon ready");

// Create test files in daemon.module_path()
create_test_file(&daemon.module_path().join("test.txt"), b"content");

// Run upstream client
let status = Command::new(UPSTREAM_3_4_1)
    .arg("-av")
    .arg(format!("{}/test.txt", daemon.url()))
    .arg(dest_dir.path())
    .status()
    .expect("run upstream client");

assert!(status.success());
// Daemon automatically cleaned up on drop
```

### Manual Protocol Testing Pattern

```rust
let stream = TcpStream::connect(format!("127.0.0.1:{}", daemon.port))
    .expect("connect to daemon");

let reader_stream = stream.try_clone().expect("clone stream");
let mut writer_stream = stream;
let mut reader = BufReader::new(reader_stream);

// Read greeting
let mut greeting = String::new();
reader.read_line(&mut greeting).expect("read greeting");
assert!(greeting.starts_with("@RSYNCD: "));

// Send client version
writer_stream.write_all(b"@RSYNCD: 31.0 sha512 sha256 sha1 md5 md4\n")
    .expect("send version");
```

## Debugging Test Failures

### Check Daemon Logs

```rust
// In test code
if !status.success() {
    eprintln!("Daemon log:\n{}", daemon.log_contents().unwrap());
}
```

### Run with Verbose Output

```bash
bash tools/run_upstream_client_interop_tests.sh --verbose --test test_name
```

### Check Binary Availability

```bash
# Verify all binaries exist
ls -la target/release/oc-rsync
ls -la target/interop/upstream-install/3.4.1/bin/rsync
ls -la target/interop/upstream-install/3.1.3/bin/rsync
ls -la target/interop/upstream-install/3.0.9/bin/rsync
```

### Manual Daemon Testing

Start daemon manually for debugging:

```bash
# Create test config
cat > /tmp/test.conf << EOF
port = 8873
use chroot = false
numeric ids = yes

[testmodule]
    path = /tmp/testdata
    read only = false
    list = yes
EOF

# Start daemon
target/release/oc-rsync --daemon --config=/tmp/test.conf --no-detach

# In another terminal, test with upstream client
target/interop/upstream-install/3.4.1/bin/rsync -av rsync://localhost:8873/testmodule/ /tmp/dest/
```

## Common Issues

### Port Already in Use

If tests fail with "address already in use":

```bash
# Find process using port
lsof -i :18873

# Kill stuck daemon
pkill -f "oc-rsync --daemon"
```

### Missing Binary

Tests are marked with `#[ignore]` and skip gracefully if binaries are missing:

```
test test_upstream_3_4_1_client_handshake ... ignored
```

Build missing binaries following the prerequisites section.

### Timeout Errors

If daemon doesn't become ready:

1. Check daemon stderr in test output
2. Verify port is not blocked by firewall
3. Increase timeout in test code if needed

### Protocol Mismatch

If handshake fails:

1. Check protocol version in greeting
2. Verify auth digest list format (protocol 30+)
3. Compare with upstream clientserver.c handshake sequence

## CI Integration

Add to `.github/workflows/ci.yml`:

```yaml
- name: Run interop tests
  run: |
    # Build upstream binaries (cached)
    bash tools/build_upstream_binaries.sh

    # Run interop tests
    cargo test --package core --test upstream_client_to_oc_daemon_interop -- --ignored --show-output
```

## Test Maintenance

### Adding New Tests

1. Follow existing test pattern
2. Use unique port number (increment from highest)
3. Add descriptive doc comment
4. Mark with `#[ignore]` if requires binaries
5. Update this documentation

### Updating for New Protocol Versions

When upstream rsync releases new protocol version:

1. Add new binary to prerequisites
2. Add constant for binary path
3. Create handshake test for new version
4. Verify all existing tests pass with new version

### Verifying Test Coverage

```bash
# Check which protocol versions are tested
grep -r "UPSTREAM_" crates/core/tests/upstream_client_to_oc_daemon_interop.rs

# Check test count
grep -c "^fn test_" crates/core/tests/upstream_client_to_oc_daemon_interop.rs
```

## Related Documentation

- [CLAUDE.md](../CLAUDE.md) - Overall architecture and conventions
- [Protocol Documentation](../crates/protocol/README.md) - Wire protocol details
- [Daemon Implementation](../crates/daemon/README.md) - Daemon internals

## Upstream References

Key upstream rsync source files for protocol reference:

- `clientserver.c` - Daemon protocol implementation
- `main.c:1267-1384` - Client `client_run()` function
- `compat.c` - Compatibility flags and negotiation
- `authenticate.c` - Authentication protocol
- `checksum.c` - Checksum algorithm negotiation
- `io.c` - Multiplexed I/O

Clone upstream source for reference:

```bash
git clone https://github.com/RsyncProject/rsync.git target/interop/upstream-reference
cd target/interop/upstream-reference
git checkout v3.4.1
```
