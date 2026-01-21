# LZ4 Raw Block Format for Wire Protocol Compatibility

## Problem

The existing LZ4 implementation in `crates/compress/src/lz4.rs` used `lz4_flex` frame format, which is incompatible with upstream rsync 3.4.1's wire protocol.

**Upstream rsync LZ4 format** (from `token.c`):
- 2-byte header: `[DEFLATED_DATA + (size >> 8)] [size & 0xFF]`
- Raw LZ4 compressed data (no frame header/footer)
- Max block size: 16383 bytes (14-bit size field)
- Uses `LZ4_compress_default()` / `LZ4_decompress_safe()` APIs

**Previous Rust implementation**:
- LZ4 frame format with magic bytes, checksums, and frame structure
- Wire-incompatible with upstream

## Solution

Split the LZ4 module into two submodules:

```
crates/compress/src/lz4/
├── mod.rs      # Re-exports, backward compatibility
├── frame.rs    # Original frame-based API (moved from lz4.rs)
└── raw.rs      # New rsync wire protocol format
```

### Wire Protocol Constants

Matching upstream `token.c`:

```rust
pub const DEFLATED_DATA: u8 = 0x40;    // Flag byte
pub const MAX_BLOCK_SIZE: usize = 16383; // 14-bit max
pub const HEADER_SIZE: usize = 2;
```

### API Surface

**Encoding:**
- `encode_header(size) -> [u8; 2]` - Create 2-byte rsync header
- `compress_block(input, output) -> Result<usize>` - Buffer-based
- `compress_block_to_vec(input) -> Result<Vec<u8>>` - Allocating
- `write_compressed_block(input, writer) -> Result<usize>` - Streaming

**Decoding:**
- `decode_header([u8; 2]) -> Option<usize>` - Parse header
- `decompress_block(input, output) -> Result<usize>` - Buffer-based
- `decompress_block_to_vec(input, max_size) -> Result<Vec<u8>>` - Allocating
- `read_compressed_block(reader, max_size) -> Result<Vec<u8>>` - Streaming

### Error Handling

Custom `RawLz4Error` with `From<RawLz4Error> for std::io::Error` for ergonomic use with `?` operator.

## Backward Compatibility

Original frame-based types re-exported at module level:
- `lz4::CountingLz4Encoder`
- `lz4::CountingLz4Decoder`
- `lz4::compress_to_vec`
- `lz4::decompress_to_vec`

Code using these types continues to work unchanged.

## Testing

All tests pass:
- Header encode/decode roundtrip
- Compress/decompress roundtrip (small, large, compressible data)
- Empty input handling
- Invalid header rejection
- Buffer-based and streaming APIs
- Maximum block size enforcement

## Files Changed

1. `crates/compress/Cargo.toml` - Added `safe-encode`, `safe-decode` features
2. `crates/compress/src/lz4.rs` - Removed (replaced by directory)
3. `crates/compress/src/lz4/mod.rs` - New module root
4. `crates/compress/src/lz4/frame.rs` - Moved from lz4.rs
5. `crates/compress/src/lz4/raw.rs` - New wire protocol implementation
6. `crates/compress/src/lib.rs` - Updated doc comment
