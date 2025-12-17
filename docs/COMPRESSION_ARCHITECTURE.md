# Compression Stream Architecture Design

**Date**: 2025-12-17
**Status**: Design Phase
**Target**: Protocol 30+ compression negotiation and application

---

## Executive Summary

This document defines the architecture for applying negotiated compression algorithms
to rsync protocol streams. The design adds a compression layer to the server-side
stream stack while maintaining backward compatibility and mirroring upstream rsync's
behavior.

### Current Status

✅ **Negotiation**: Compression algorithms are negotiated during capability exchange
✅ **Infrastructure**: `compress` crate provides zlib, LZ4, and zstd encoders/decoders
✅ **Engine Integration**: `ActiveCompressor` exists for local copy operations
❌ **Stream Application**: Server streams don't apply negotiated compression

---

## Architecture Overview

### Stream Stack Evolution

**Current Stack** (Plain → Multiplex only):
```
Application Data
      ↓
ServerWriter::Plain(TcpStream)
      ↓ activate_multiplex()
ServerWriter::Multiplex(MultiplexWriter)
      ↓
TcpStream
```

**Proposed Stack** (Plain → Multiplex → Compress):
```
Application Data
      ↓
ServerWriter::Plain(TcpStream)
      ↓ activate_multiplex()
ServerWriter::Multiplex(MultiplexWriter)
      ↓ activate_compression() [NEW]
ServerWriter::Compressed(CompressedWriter)  [NEW]
      ↓
TcpStream
```

### Key Design Principles

1. **Mirror Upstream**: Match rsync 3.4.1's compression activation order and behavior
2. **Layered Streams**: Compression wraps multiplex, not the other way around
3. **Negotiated Behavior**: Only activate compression when both peers agree
4. **Zero-Copy**: Reuse buffers across compression/multiplex boundaries
5. **Explicit Lifecycle**: Flush and finish compression streams explicitly

---

## Component Design

### 1. ServerWriter Extension

**File**: `crates/core/src/server/writer.rs`

#### New Enum Variant

```rust
pub enum ServerWriter<W: Write> {
    /// Plain mode - write data directly without framing
    Plain(W),
    /// Multiplex mode - wrap data in MSG_DATA frames
    Multiplex(MultiplexWriter<W>),
    /// Compressed+Multiplex mode - compress then multiplex [NEW]
    Compressed(CompressedWriter<MultiplexWriter<W>>),
}
```

#### New Activation Method

```rust
impl<W: Write> ServerWriter<W> {
    /// Activates compression on top of multiplex mode
    ///
    /// This must be called AFTER activate_multiplex() to match upstream behavior.
    /// Upstream rsync activates compression in io.c:io_start_buffering_out()
    /// which wraps the already-multiplexed stream.
    pub fn activate_compression(
        self,
        algorithm: CompressionAlgorithm,
        level: CompressionLevel,
    ) -> io::Result<Self> {
        match self {
            Self::Multiplex(mux) => {
                let compressed = CompressedWriter::new(mux, algorithm, level)?;
                Ok(Self::Compressed(compressed))
            }
            Self::Plain(_) => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "compression requires multiplex mode first",
            )),
            Self::Compressed(_) => Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "compression already active",
            )),
        }
    }
}
```

#### Write Implementation

```rust
impl<W: Write> Write for ServerWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Self::Plain(w) => w.write(buf),
            Self::Multiplex(w) => w.write(buf),
            Self::Compressed(w) => w.write(buf),  // NEW
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            Self::Plain(w) => w.flush(),
            Self::Multiplex(w) => w.flush(),
            Self::Compressed(w) => w.flush(),  // NEW
        }
    }
}
```

### 2. CompressedWriter Implementation

**File**: `crates/core/src/server/compressed_writer.rs` (NEW)

```rust
use std::io::{self, Write};
use compress::algorithm::CompressionAlgorithm;
use compress::zlib::CompressionLevel;
use crate::engine::local_copy::compressor::ActiveCompressor;

/// Wraps a writer with compression, buffering compressed output.
///
/// Mirrors upstream rsync's io.c:io_start_buffering_out() behavior where
/// compression is applied on top of the multiplexed stream.
pub struct CompressedWriter<W: Write> {
    /// The underlying multiplexed writer
    inner: W,
    /// Active compression encoder
    compressor: ActiveCompressor,
    /// Buffer for compressed output before writing to inner
    output_buffer: Vec<u8>,
}

impl<W: Write> CompressedWriter<W> {
    /// Creates a new compressed writer wrapping the given writer.
    ///
    /// The compressor is initialized based on the negotiated algorithm.
    pub fn new(
        inner: W,
        algorithm: CompressionAlgorithm,
        level: CompressionLevel,
    ) -> io::Result<Self> {
        Ok(Self {
            inner,
            compressor: ActiveCompressor::new(algorithm, level)?,
            output_buffer: Vec::with_capacity(8192),
        })
    }

    /// Flushes compressed data to the underlying writer.
    fn flush_compressed(&mut self) -> io::Result<()> {
        if !self.output_buffer.is_empty() {
            self.inner.write_all(&self.output_buffer)?;
            self.output_buffer.clear();
        }
        self.inner.flush()
    }

    /// Finishes the compression stream and flushes all data.
    ///
    /// This MUST be called before dropping the writer to ensure all
    /// compressed data (including trailer bytes) is written.
    pub fn finish(mut self) -> io::Result<W> {
        self.compressor.finish()?;
        self.flush_compressed()?;
        Ok(self.inner)
    }
}

impl<W: Write> Write for CompressedWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        // Compress input data into output buffer
        self.compressor.write(buf)?;

        // Get compressed bytes (ActiveCompressor should write to output_buffer)
        // Note: May need to adjust ActiveCompressor API to write to provided buffer

        // Flush if buffer is getting large
        if self.output_buffer.len() > 4096 {
            self.flush_compressed()?;
        }

        Ok(buf.len())  // Always report full write to match upstream
    }

    fn flush(&mut self) -> io::Result<()> {
        self.flush_compressed()
    }
}
```

### 3. ServerReader Extension

**File**: `crates/core/src/server/reader.rs`

Similar architecture for decompression on the read side:

```rust
pub enum ServerReader<R: Read> {
    Plain(R),
    Multiplex(MultiplexReader<R>),
    Compressed(CompressedReader<MultiplexReader<R>>),  // NEW
}
```

**Decompressor**: Use corresponding decoder from `compress` crate
(`CountingZlibDecoder`, `CountingLz4Decoder`, `CountingZstdDecoder`).

---

## Integration Points

### 1. Server Setup

**File**: `crates/core/src/server/setup.rs`

After capability negotiation and multiplex activation:

```rust
pub fn setup_server_streams(
    handshake: &HandshakeResult,
    mut writer: ServerWriter<TcpStream>,
    mut reader: ServerReader<TcpStream>,
) -> io::Result<(ServerWriter<TcpStream>, ServerReader<TcpStream>)> {
    // 1. Activate multiplex (existing)
    if handshake.protocol.as_u8() >= 23 {
        writer = writer.activate_multiplex()?;
        reader = reader.activate_multiplex()?;
    }

    // 2. Activate compression if negotiated (NEW)
    if let Some(negotiated) = &handshake.negotiated_algorithms {
        if negotiated.compression != CompressionAlgorithm::None {
            let level = /* get from config or use default */;
            writer = writer.activate_compression(
                negotiated.compression,
                level,
            )?;
            reader = reader.activate_compression(
                negotiated.compression,
            )?;
        }
    }

    Ok((writer, reader))
}
```

### 2. Generator Context

**File**: `crates/core/src/server/generator.rs`

Ensure compression is active before sending file lists:

```rust
impl GeneratorContext {
    pub fn run<W: Write>(&self, writer: &mut W) -> io::Result<()> {
        // Compression is already activated by setup_server_streams
        // Just send data normally - it will be compressed automatically
        self.send_file_list(writer)?;
        // ... rest of generator logic
        Ok(())
    }
}
```

### 3. Receiver Context

**File**: `crates/core/src/server/receiver.rs`

Compression is transparent to receiver - just read normally:

```rust
impl ReceiverContext {
    pub fn receive_file_list<R: Read>(&mut self, reader: &mut R) -> io::Result<usize> {
        // Decompression happens automatically in ServerReader
        // Just read normally
        let mut flist_reader = FileListReader::new(self.protocol);
        // ... rest of receiver logic
    }
}
```

---

## Implementation Phases

### Phase 1: Core Infrastructure ✅

**Status**: COMPLETE

- [x] Compression encoders/decoders in `compress` crate
- [x] `ActiveCompressor` in engine layer
- [x] Compression algorithm negotiation
- [x] Algorithm storage in `NegotiationResult`

### Phase 2: Stream Wrappers (Next Step)

**Estimated effort**: 2-3 days

1. Create `CompressedWriter` wrapper
   - Wrap `MultiplexWriter` with compression
   - Buffer management and flushing
   - Finish/finalize support

2. Create `CompressedReader` wrapper
   - Wrap `MultiplexReader` with decompression
   - Handle compressed frame boundaries
   - Error handling for corrupt data

3. Extend `ServerWriter`/`ServerReader` enums
   - Add `Compressed` variant
   - Add `activate_compression()` method
   - Update `Write`/`Read` trait implementations

### Phase 3: Integration (Following Week)

**Estimated effort**: 1-2 days

1. Wire compression activation in `setup_server_streams()`
   - Get compression level from configuration
   - Activate after multiplex
   - Handle None algorithm (no compression)

2. Update role contexts
   - Ensure Generator/Receiver work with compressed streams
   - No changes needed if compression is transparent

3. Add configuration
   - `--compress-level` support
   - `--skip-compress` patterns
   - Default compression level

### Phase 4: Testing & Validation (Final Week)

**Estimated effort**: 2-3 days

1. Unit tests
   - CompressedWriter/Reader round-trip
   - Different algorithms (zlib, LZ4, zstd)
   - Different compression levels
   - Edge cases (empty data, large data)

2. Integration tests
   - Full server session with compression
   - Verify negotiated algorithm is used
   - Test compression level changes
   - Test with different file types

3. Interop tests
   - oc-rsync server ↔ upstream rsync client
   - oc-rsync client ↔ upstream rsync server
   - Different compression algorithms
   - Verify wire format compatibility

---

## Challenges & Considerations

### 1. ActiveCompressor API Gap

**Issue**: `ActiveCompressor::write()` doesn't allow caller to provide output buffer.

**Current**:
```rust
pub fn write(&mut self, chunk: &[u8]) -> io::Result<()>
```

**Needed**:
```rust
pub fn write_to_buf(&mut self, chunk: &[u8], out: &mut Vec<u8>) -> io::Result<()>
```

**Solution**: Extend `ActiveCompressor` API or redesign `CompressedWriter` to use
internal compression state differently.

### 2. Compression Level Configuration

**Issue**: Where does compression level come from?

**Options**:
1. Hard-coded default (level 6 for zlib, matching upstream)
2. From `--compress-level` CLI option
3. From daemon configuration
4. Negotiated during capability exchange (not in upstream)

**Recommendation**: Start with hard-coded default, add configuration in Phase 3.

### 3. Skip-Compress Patterns

**Issue**: Upstream rsync skips compression for already-compressed files (`.gz`, `.zip`, etc.).

**Implementation**:
- `crates/engine/src/local_copy/skip_compress.rs` already exists
- Need to integrate with file transfer decision
- Check file extension against skip list before compressing

**Defer to**: Post-Phase 4 optimization

### 4. Error Handling

**Issue**: What happens if compression fails mid-stream?

**Strategy**:
1. Detect corruption early (decompressor will error)
2. Surface io::Error to higher layers
3. Abort transfer with clear error message
4. Match upstream error codes and messages

---

## Testing Strategy

### Unit Tests

**File**: `crates/core/src/server/compressed_writer.rs`

```rust
#[cfg(test)]
mod tests {
    #[test]
    fn compress_round_trip_zlib() {
        let data = b"test data";
        let mut buf = Vec::new();
        let mut writer = CompressedWriter::new(
            &mut buf,
            CompressionAlgorithm::Zlib,
            CompressionLevel::Default,
        ).unwrap();

        writer.write_all(data).unwrap();
        writer.finish().unwrap();

        // Verify compressed data is smaller
        assert!(buf.len() < data.len() || buf.len() <= data.len() + 20);

        // Decompress and verify
        // ...
    }
}
```

### Integration Tests

**File**: `crates/core/src/server/tests/compression_streams.rs` (NEW)

```rust
#[test]
fn server_session_with_zlib_compression() {
    // Set up server with compression negotiation
    let negotiated = NegotiationResult {
        checksum: ChecksumAlgorithm::MD5,
        compression: CompressionAlgorithm::Zlib,
    };

    // Run full session
    // Verify data is transmitted correctly
    // Verify compression was actually used (check bytes_written)
}

#[test]
fn server_session_with_no_compression() {
    // Negotiation with None algorithm
    // Verify plain multiplex is used
}
```

### Interop Tests

**File**: `tests/interop/compression.rs` (NEW)

Test with upstream rsync binaries in `target/interop/upstream-install/`.

---

## Performance Considerations

### Buffering Strategy

- **MultiplexWriter**: 4KB buffer (matches upstream `IO_BUFFER_SIZE`)
- **CompressedWriter**: 8KB output buffer (compressed data)
- **Flush policy**: Flush compressed buffer when > 4KB

### Compression Overhead

Expected compression ratios for typical rsync data:
- **Text files**: 60-70% reduction (zlib)
- **Binary files**: 10-30% reduction (varies)
- **Already compressed**: 0-5% reduction (skip these)

### CPU vs. Bandwidth Trade-off

- **LAN**: Compression overhead > network savings (disable by default)
- **WAN**: Compression savings > CPU overhead (enable by default)
- **Very slow links**: Even modest compression helps significantly

---

## Upstream References

### rsync 3.4.1 Source Files

- **`io.c:io_start_buffering_out()`**: Compression activation
- **`io.c:io_end_buffering_out()`**: Compression finalization
- **`compress.c:send_compressed_token()`**: Compression logic
- **`compat.c:setup_protocol()`**: Negotiation integration

### Behavior Parity Checklist

- [ ] Compression activated after multiplex
- [ ] Compression level matches upstream defaults
- [ ] Skip-compress patterns honored
- [ ] Compression finalized before stream close
- [ ] Error messages match upstream
- [ ] Wire format byte-for-byte compatible

---

## Conclusion

This architecture provides a clean, layered approach to compression that:

1. **Mirrors upstream**: Matches rsync 3.4.1's compression behavior
2. **Is extensible**: Easy to add new algorithms
3. **Is testable**: Clear boundaries for unit/integration tests
4. **Is maintainable**: Follows existing ServerWriter/Reader patterns
5. **Is correct**: Proper lifecycle management and error handling

**Next Step**: Implement Phase 2 (Stream Wrappers) starting with `CompressedWriter`.

---

## Appendix: API Changes Required

### `ActiveCompressor` Enhancement

**File**: `crates/engine/src/local_copy/compressor.rs`

Add method to write to caller-provided buffer:

```rust
impl ActiveCompressor {
    /// Writes compressed data to the provided output buffer.
    pub fn write_to_buf(&mut self, chunk: &[u8], out: &mut Vec<u8>) -> io::Result<()> {
        match self {
            Self::Zlib(encoder) => encoder.write_to_buf(chunk, out),
            // ... other variants
        }
    }
}
```

This requires corresponding changes in `compress` crate encoders.

---

**Document Version**: 1.0
**Last Updated**: 2025-12-17
**Author**: Architecture Planning
**Status**: Ready for Implementation
