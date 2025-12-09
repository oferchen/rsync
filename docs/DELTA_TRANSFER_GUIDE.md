# Delta Transfer Developer Guide

**Last Updated**: 2025-12-09
**Status**: Implementation complete, production-ready core functionality

---

## Overview

This guide explains how the rsync delta transfer algorithm is implemented in the Rust rsync server. It covers the complete flow from file list exchange through signature generation, delta creation, delta application, and metadata preservation.

**Target Audience**: Developers working on the server implementation, adding new transfer modes, or debugging delta transfer issues.

---

## Architecture Overview

### Three-Role Model

The rsync protocol uses three logical roles:

1. **Sender (Generator)**: Generates file list and delta operations
2. **Receiver**: Generates signatures and applies deltas
3. **Client**: Coordinates the transfer (may be sender or receiver)

In our implementation:
- Generator role: `crates/core/src/server/generator.rs`
- Receiver role: `crates/core/src/server/receiver.rs`
- Orchestration: `crates/core/src/server/mod.rs`

### Data Flow

```
Generator (Sender)                    Receiver
------------------                    --------

1. Walk filesystem
2. Build file list ──────────────────> Receive file list

                                      3. For each file:
                                         Generate signature from basis
                    <────────────────── Send signature

4. For each file:
   Generate delta from signature
   Send delta ───────────────────────> Receive delta
                                       Apply delta to reconstruct file
                                       Apply metadata (perms, times, owner)
```

---

## Component Documentation

### 1. Receiver Signature Generation

**File**: `crates/core/src/server/receiver.rs` (lines 120-148)

**Purpose**: Generate rolling and strong checksums for existing basis file

```rust
// Check if basis file exists
let basis_file = match fs::File::open(basis_path) {
    Ok(f) => f,
    Err(_) => break 'sig None,  // No basis, request whole file
};

let file_size = basis_file.metadata()?.len();

// Calculate block layout using rsync's square-root heuristic
let params = SignatureLayoutParams::new(
    file_size,
    None, // Use default block size heuristic
    self.protocol,
    checksum_length,
);

let layout = calculate_signature_layout(params)?;

// Generate signature using MD5 for strong checksums (protocol >= 30)
let signature = generate_file_signature(
    basis_file,
    layout,
    SignatureAlgorithm::Md5
)?;
```

**Key Points**:
- Uses `calculate_signature_layout()` from engine for block size heuristics
- MD5 for strong checksums (16 bytes) on protocol 30+
- Falls back to whole-file transfer if basis doesn't exist
- Returns signature with rolling sums (Adler-32 style) and strong sums (MD5)

**Wire Format** (sent to generator):
```
Block count (varint)
Block length (varint)
Strong sum length (varint)
For each block:
  Rolling sum (4 bytes LE)
  Strong sum (variable length, typically 16 bytes for MD5)
```

### 2. Generator Delta Generation

**File**: `crates/core/src/server/generator.rs` (lines 425-512)

**Purpose**: Receive signature, generate delta operations (literals vs copy references)

```rust
// Receive signature from receiver
let (block_length, block_count, strong_sum_length, sig_blocks) =
    read_signature(&mut &mut *reader)?;

// Reconstruct engine signature from wire format
let signature = FileSignature::from_raw_parts(layout, blocks);

// Open source file
let source = fs::File::open(&source_path)?;

if block_count > 0 {
    // Basis exists: generate delta using signature index
    let index = DeltaSignatureIndex::from_signature(
        &signature,
        SignatureAlgorithm::Md5
    )?;

    let generator = DeltaGenerator::new();
    let delta_script = generator.generate(source, &index)?;

    // Convert engine delta script to wire format
    let wire_ops = script_to_wire_delta(delta_script);
    write_delta(&mut &mut *writer, &wire_ops)?;
} else {
    // No basis: send whole file as literals
    let delta_script = generate_whole_file_delta(source)?;
    let wire_ops = script_to_wire_delta(delta_script);
    write_delta(&mut &mut *writer, &wire_ops)?;
}
```

**Key Points**:
- Reconstructs engine signature from wire format using `from_raw_parts()`
- Creates `DeltaSignatureIndex` for O(1) block lookups
- Uses `DeltaGenerator` from engine to create delta script
- Converts engine format to wire format via `script_to_wire_delta()`

**Wire Format** (sent to receiver):
```
Operation count (varint)
For each operation:
  Op code (1 byte): 0x00 = Literal, 0x01 = Copy

  For Literal:
    Length (varint)
    Data bytes

  For Copy:
    Block index (varint)
    Length (varint)
```

### 3. Receiver Delta Application

**File**: `crates/core/src/server/receiver.rs` (lines 176-208)

**Purpose**: Receive delta operations and reconstruct file

```rust
// Receive delta operations from generator
let wire_delta = read_delta(&mut &mut *reader)?;
let delta_script = wire_delta_to_script(wire_delta);

// Atomic file reconstruction using temp file
let temp_path = basis_path.with_extension("oc-rsync.tmp");

if let Some(signature) = signature_opt {
    // Delta transfer: apply delta using basis file
    let index = DeltaSignatureIndex::from_signature(
        &signature,
        SignatureAlgorithm::Md5
    )?;

    // Open basis for reading
    let basis = fs::File::open(basis_path)?;
    let mut output = fs::File::create(&temp_path)?;

    // Apply the delta
    apply_delta(basis, &mut output, &index, &delta_script)?;
    output.sync_all()?;

    // Atomic rename
    fs::rename(&temp_path, basis_path)?;
} else {
    // Whole-file transfer: extract literals only
    apply_whole_file_delta(&temp_path, &delta_script)?;
    fs::rename(&temp_path, basis_path)?;
}
```

**Key Points**:
- Uses temp file for atomic operations (crash safety)
- `apply_delta()` from engine handles copy operations by reading from basis
- Validates that whole-file deltas contain only literals (no copy ops)
- `sync_all()` before rename ensures durability

### 4. Metadata Preservation

**File**: `crates/core/src/server/receiver.rs` (lines 210-220)

**Purpose**: Apply metadata from FileEntry to reconstructed file

```rust
// Build metadata options from server config flags
let metadata_opts = MetadataOptions::new()
    .preserve_permissions(self.config.flags.perms)  // -p flag
    .preserve_times(self.config.flags.times)        // -t flag
    .preserve_owner(self.config.flags.owner)        // -o flag
    .preserve_group(self.config.flags.group)        // -g flag
    .numeric_ids(self.config.flags.numeric_ids);    // --numeric-ids

// Apply metadata from FileEntry (best-effort)
if let Err(meta_err) =
    apply_metadata_from_file_entry(basis_path, file_entry, metadata_opts.clone())
{
    // Log warning but continue - metadata failure shouldn't abort transfer
    eprintln!("[receiver] Warning: failed to apply metadata to {}: {}",
              basis_path.display(), meta_err);
}
```

**Key Points**:
- Uses `MetadataOptions` builder pattern for flexibility
- Best-effort: logs warnings but doesn't abort on metadata failures
- Handles Unix-specific operations (chown) gracefully on non-Unix platforms
- Preserves nanosecond timestamp precision via `FileTime::from_unix_time()`

**Implementation**: `crates/metadata/src/apply.rs` (lines 325-496)
- `apply_metadata_from_file_entry()` - Main entry point
- `apply_ownership_from_entry()` - Unix uid/gid via `chownat()`
- `apply_permissions_from_entry()` - Mode bits via `set_permissions()`
- `apply_timestamps_from_entry()` - Nanosecond mtime via `set_file_times()`

---

## Adding New Functionality

### Example: Add Progress Reporting

**Goal**: Report transfer progress during delta application

**Step 1**: Extend `TransferStats` structure

File: `crates/core/src/server/receiver.rs` (lines 235-244)

```rust
#[derive(Debug, Clone, Default)]
pub struct TransferStats {
    pub files_listed: usize,
    pub files_transferred: usize,
    pub bytes_received: u64,

    // NEW: Add progress fields
    pub bytes_matched: u64,       // Bytes copied from basis
    pub bytes_literal: u64,       // Bytes sent over wire
    pub current_file: Option<PathBuf>,
}
```

**Step 2**: Track progress during delta application

Modify `apply_delta()` call to capture matched vs literal bytes:

```rust
// After apply_delta():
let matched_bytes = delta_script.total_bytes() - delta_script.literal_bytes();
let literal_bytes = delta_script.literal_bytes();

bytes_matched += matched_bytes;
bytes_literal += literal_bytes;
```

**Step 3**: Add progress callback

```rust
pub struct ReceiverContext<F: Fn(&TransferStats)> {
    protocol: ProtocolVersion,
    config: ServerConfig,
    file_list: Vec<FileEntry>,
    progress_callback: Option<F>,  // NEW
}

// In transfer loop:
if let Some(ref callback) = self.progress_callback {
    callback(&stats);
}
```

### Example: Add Compression Support

**Goal**: Compress delta operations before sending

**Step 1**: Add compression wrapper around writer

```rust
use compress::ZlibEncoder;

let mut compressor = ZlibEncoder::new(writer, compression_level);
write_delta(&mut compressor, &wire_ops)?;
compressor.finish()?;
```

**Step 2**: Add decompression wrapper around reader

```rust
use compress::ZlibDecoder;

let mut decompressor = ZlibDecoder::new(reader);
let wire_delta = read_delta(&mut decompressor)?;
```

**Note**: Compression integration requires protocol negotiation to ensure both sides support it. Check `config.flags.compress` before enabling.

---

## Testing Strategy

### Unit Tests

Test helper functions in isolation:

**File**: `crates/core/src/server/receiver.rs` (lines 376-469)

```rust
#[test]
fn wire_delta_to_script_converts_literals() {
    let wire_ops = vec![
        DeltaOp::Literal(vec![1, 2, 3, 4]),
        DeltaOp::Literal(vec![5, 6, 7, 8]),
    ];

    let script = wire_delta_to_script(wire_ops);

    assert_eq!(script.tokens().len(), 2);
    assert_eq!(script.total_bytes(), 8);
    assert_eq!(script.literal_bytes(), 8);
}
```

### Integration Tests

Test end-to-end transfer via CLI:

**File**: `tests/integration_server_delta.rs`

```rust
#[test]
fn delta_transfer_with_modified_middle() {
    let test_dir = TestDir::new().expect("create test dir");

    // Create source: [AAAA] [BBBB] [CCCC]
    let src_content = /* ... */;

    // Create basis: [AAAA] [XXXX] [CCCC] (different middle)
    let basis_content = /* ... */;

    // Run delta transfer
    let mut cmd = RsyncCommand::new();
    cmd.args([src_file, dest_file]);
    cmd.assert_success();

    // Verify reconstructed file matches source exactly
    assert_eq!(fs::read(&dest_file).unwrap(), src_content);
}
```

---

## Debugging Tips

### Enable Trace Logging

Set environment variable:
```bash
export RUST_LOG=core::server=debug
cargo run -- <rsync args>
```

### Inspect Wire Protocol

Use binary diff tools to compare signatures/deltas:
```bash
xxd basis_signature.bin > basis.hex
xxd expected_signature.bin > expected.hex
diff -u basis.hex expected.hex
```

### Verify Signature Correctness

Compute rolling checksum manually:
```rust
use checksums::RollingDigest;

let mut rolling = RollingDigest::new();
rolling.update(&block_data);
let sum = rolling.value();  // Should match signature
```

### Check Delta Application

Log each delta operation:
```rust
for token in script.tokens() {
    match token {
        DeltaToken::Literal(data) => {
            eprintln!("Literal: {} bytes", data.len());
        }
        DeltaToken::Copy { index, len } => {
            eprintln!("Copy: block {} for {} bytes", index, len);
        }
    }
}
```

---

## Performance Considerations

### Block Size Heuristics

Rsync uses square-root-of-filesize heuristic:
- Small files (< 4KB): Whole-file transfer (no delta)
- Medium files: Block size ≈ √(filesize)
- Large files: Capped at max block size (64KB default)

Implementation: `crates/engine/src/delta/mod.rs` - `calculate_signature_layout()`

### Memory Usage

- Signatures stored in memory (one `SignatureBlock` per block)
- Delta script tokens buffered before application
- For very large files (> 1GB), consider streaming approaches

### SIMD Acceleration

Rolling checksums use SIMD when available:
- AVX2 on x86_64 (8x 32-bit lanes)
- NEON on aarch64 (4x 32-bit lanes)
- Scalar fallback for other architectures

Implementation: `crates/checksums/src/rolling/`

---

## Common Patterns

### Atomic File Operations

Always use temp file + rename pattern for crash safety:
```rust
let temp_path = final_path.with_extension("oc-rsync.tmp");
let mut output = fs::File::create(&temp_path)?;

// ... write data ...
output.sync_all()?;

fs::rename(&temp_path, final_path)?;  // Atomic on same filesystem
```

### Wire Protocol Trait Object Reborrowing

Handle `?Sized` trait bounds using double mutable reborrow:
```rust
write_signature(&mut &mut *writer, ...)?;
let delta = read_delta(&mut &mut *reader)?;
```

This creates a concrete reference satisfying function signatures without wrapper functions.

### Best-Effort Metadata Application

Never abort transfers due to metadata failures:
```rust
if let Err(err) = apply_metadata(...) {
    eprintln!("[receiver] Warning: metadata failure: {}", err);
    // Continue with transfer
}
```

---

## References

- **Upstream rsync**: https://github.com/RsyncProject/rsync
- **Protocol spec**: `target/interop/upstream-src/rsync-3.4.1/csprotocol.txt`
- **Wire format docs**: `crates/protocol/src/wire/` inline documentation
- **Engine docs**: `crates/engine/src/delta/` inline documentation
- **Metadata docs**: `crates/metadata/src/apply.rs` inline documentation
- **Implementation summary**: `docs/SERVER_DELTA_IMPLEMENTATION.md`

---

## Glossary

- **Basis file**: Existing file on receiver used for delta generation
- **Block**: Fixed-size chunk of file for checksum calculation
- **Delta script**: Sequence of operations (literals + copy references)
- **Generator**: Role that generates deltas (typically the sender)
- **Literal**: Raw bytes sent over wire (not in basis)
- **Receiver**: Role that applies deltas (typically the destination)
- **Rolling checksum**: Weak checksum (Adler-32 style) for fast block matching
- **Signature**: Set of checksums (rolling + strong) for basis file blocks
- **Strong checksum**: Cryptographic hash (MD5/SHA) to verify block matches
