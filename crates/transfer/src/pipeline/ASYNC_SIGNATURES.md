# Async Signature Generation During Pipeline Wait

This document describes the async signature pre-computation feature for pipelined transfers.

## Overview

During pipelined transfers, the receiver waits for delta responses from the sender. This creates idle CPU time that can be utilized to pre-compute signatures for upcoming files. Async signature generation overlaps this CPU-intensive work with network I/O, reducing total transfer time.

## Architecture

```text
┌─────────────────────────────────────────────────────────────────────────┐
│                    Pipeline with Async Signatures                       │
├─────────────────────────────────────────────────────────────────────────┤
│                                                                         │
│  Request Phase              Wait Phase              Worker Threads      │
│  ┌──────────────┐          ┌──────────────┐        ┌──────────────┐    │
│  │ File N:      │          │ Wait for     │        │ Generate     │    │
│  │ - Find basis │          │ response N   │        │ signature    │    │
│  │ - Queue sig  │    ──▶   │              │   ◀──  │ for N+1      │    │
│  │ - Send req   │          │ Poll results │        │              │    │
│  └──────────────┘          └──────────────┘        └──────────────┘    │
│                                    │                                    │
│                                    ▼                                    │
│  ┌──────────────────────────────────────┐                              │
│  │ Use pre-computed signature N+1       │                              │
│  │ (if available, else generate sync)   │                              │
│  └──────────────────────────────────────┘                              │
│                                                                         │
└─────────────────────────────────────────────────────────────────────────┘
```

## Performance Benefits

### Without Async Signatures
```
File 0: [Find basis][Gen sig][Send req][Wait for response]
File 1: [Find basis][Gen sig][Send req][Wait for response]
File 2: [Find basis][Gen sig][Send req][Wait for response]
```

### With Async Signatures
```
File 0: [Find basis][Gen sig][Send req][Wait + Gen sig 1]
File 1: [Find basis][Use sig][Send req][Wait + Gen sig 2]
File 2: [Find basis][Use sig][Send req][Wait + Gen sig 3]
```

For transfers with:
- Many files (hundreds to thousands)
- CPU-intensive checksums (MD4, MD5, SHA1)
- Network latency (daemon transfers over WAN)

Async signatures can provide 10-30% speedup by utilizing idle CPU during network waits.

## Configuration

The feature is controlled via `PipelineConfig::async_signatures`:

```rust
use transfer::pipeline::PipelineConfig;

// Enabled by default
let config = PipelineConfig::default();
assert!(config.async_signatures);

// Disable explicitly
let config = PipelineConfig::default()
    .with_async_signatures(false);

// Configure with custom pipeline window
let config = PipelineConfig::default()
    .with_window_size(128)
    .with_async_signatures(true);
```

## Implementation Details

### SignatureCache

The `SignatureCache` manages async signature generation:

```rust
use transfer::pipeline::async_signature::{SignatureCache, AsyncSignatureConfig};

// Create with default thread count (min(num_cpus, 4))
let config = AsyncSignatureConfig::default();
let mut cache = SignatureCache::new(config);

// Request signature generation
cache.request_signature(
    file_path,
    basis_size,
    protocol,
    checksum_length,
    checksum_algorithm,
)?;

// Poll for completed signatures (non-blocking)
cache.poll_results();

// Try to get a pre-computed signature
if let Some(signature) = cache.get_signature(&file_path) {
    // Use pre-computed signature
} else {
    // Generate synchronously as fallback
}

// Shutdown when done
cache.shutdown()?;
```

### Thread Configuration

By default, the async generator uses `min(num_cpus, 4)` threads:

```rust
use signature::async_gen::AsyncSignatureConfig;

// Custom thread count
let config = AsyncSignatureConfig::default()
    .with_threads(2)
    .with_max_pending(32);
```

**Why cap at 4 threads?**
- Signature generation is disk I/O bound (reading basis files)
- More threads don't provide linear speedup
- Reduces memory and CPU overhead

## Memory Usage

Memory is bounded by:
- Request queue: `max_pending` × signature request metadata (~200 bytes each)
- Completed signatures: Pipeline window size × signature size (~300 bytes per file)
- Worker threads: `num_threads` × stack size (~2MB per thread)

**Total**: ~20-50 MB for typical configurations

## When NOT to Use

Async signatures may reduce performance when:

1. **Fast checksums**: XXH3/XXH64 are so fast that async overhead dominates
2. **Small files**: Signature generation is trivial, not worth parallelizing
3. **Local transfers**: No network latency to hide
4. **Limited CPU**: Single-core systems see no benefit

For these cases, disable via:
```rust
PipelineConfig::default().with_async_signatures(false)
```

## Integration with Pipelined Receiver

The feature integrates seamlessly with the existing pipeline:

```rust
// In run_pipelined():

// 1. Create signature cache
let mut sig_cache = if pipeline_config.async_signatures {
    SignatureCache::new(AsyncSignatureConfig::default())
} else {
    SignatureCache::disabled()
};

// 2. Pre-queue signatures for upcoming files
while pipeline.can_send() {
    if let Some((file_idx, file_entry)) = file_iter.peek() {
        if sig_cache.is_enabled() {
            // Queue signature generation
            if let Some((file, size)) = try_open_file(&file_path) {
                sig_cache.request_signature(
                    file_path, size, protocol, checksum_length, checksum_algorithm
                )?;
            }
        }
    }
}

// 3. Poll for results while processing responses
sig_cache.poll_results();

// 4. Use pre-computed signature if available
let signature = sig_cache.get_signature(&file_path)
    .or_else(|| generate_synchronously(&file_path));
```

## Testing

Run tests:
```bash
# Signature async_gen module
cargo test -p signature --lib async_gen

# Transfer async_signature module
cargo test -p transfer --lib pipeline::async_signature

# Full pipeline integration
cargo test -p transfer --lib pipeline
```

## Future Enhancements

Potential improvements:
1. **Adaptive thread count**: Scale workers based on CPU usage
2. **Priority queue**: Prioritize signatures for files about to be sent
3. **Cancellation**: Cancel pending signatures when files are skipped
4. **Metrics**: Track hit rate and performance gains
5. **LRU cache**: Keep signatures for fuzzy matching across multiple files

## References

- Pipeline documentation: `crates/transfer/src/pipeline/mod.rs`
- Signature generation: `crates/signature/src/generation.rs`
- Parallel signatures: `crates/signature/src/parallel.rs`
- Receiver implementation: `crates/transfer/src/receiver.rs`
