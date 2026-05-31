# compress

Compression primitives shared across the workspace - provides streaming encoders
and decoders for zlib, zstd, and LZ4 with zero internal output allocations.

## Key Public Types

- `CompressionAlgorithm` - enum of supported algorithms (Zlib, Zstd, Lz4)
- `CompressionStrategy` - trait for algorithm-specific encode/decode behavior
- `ZlibStrategy` / `ZstdStrategy` / `Lz4Strategy` - concrete strategy implementations
- `NegotiationPipeline<S>` - type-state pipeline for protocol-level codec negotiation
- `CompressionStrategySelector` - selects strategy from negotiated parameters
- `ProtocolCompressionProfile` - wire-level algorithm profile
- `AdaptiveLevelStrategy` - trait for dynamic compression level adjustment
- `CountingSink` - discard writer used by counting encoders

## Modules

- `zlib` - streaming zlib encoder/decoder (`CountingZlibEncoder`)
- `zstd` - zstandard codec with optional multi-threaded mode
- `lz4` - LZ4 frame codec
- `strategy` - algorithm selection, negotiation, and adaptive level control
- `skip_compress` - suffix-based skip list (files that compress poorly)
- `algorithm` - `CompressionAlgorithm` enum and default level constants

## Dependencies

- **Upstream:** `flate2` (zlib backend), `zstd` (optional), `lz4_flex` (optional)
- **Downstream:** `protocol`, `engine`

## Features

- `zstd` - Zstandard support
- `zstdmt` - multi-threaded zstd compression
- `lz4` - LZ4 frame support
- `zlib-ng` - C-based zlib with SIMD (SSE2, AVX2, NEON) - fastest
- `zlib-rs` - pure Rust zlib fallback (no C compiler required)
