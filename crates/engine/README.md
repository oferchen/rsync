# engine

Transfer engine - delta pipeline, local-copy executor, and sparse I/O.

## Purpose

`engine` implements the core transfer primitives sitting between the high-level
`core` orchestration facade and low-level protocol/checksum/metadata crates. Both
the CLI local-copy path and remote sender/receiver/generator roles in `transfer`
drive file operations through this crate.

## Key Public Types

- `DeltaGenerator` - produces `DeltaToken` streams (LITERAL/COPY) via rolling checksum matching
- `DeltaSignatureIndex` - block-signature index for delta matching
- `apply_delta` / `generate_delta` / `generate_file_signature` - end-to-end delta helpers
- `SignatureLayout` / `calculate_signature_layout` - upstream-compatible block-size heuristics
- `LocalCopyPlan` - recursive wire-compatible local transfer executor
- `SparseReader` / `SparseDetector` - sparse file I/O with zero-run detection
- `BufferPool` / `PooledBuffer` - RAII buffer reuse to eliminate per-file heap churn
- `FuzzyMatcher` - basis-file similarity scoring for `--fuzzy`
- `DeleteTiming` - controls before/after deletion passes

## Dependencies (upstream)

`protocol`, `checksums`, `metadata`, `filters`, `compress`, `bandwidth`,
`logging`, `signature`, `matching`, `batch`, `fast_io`

## Dependents (downstream)

`transfer`, `core`, `cli`

## Platform Notes

- macOS: depends on `apple-fs` for clonefile support
- Linux: uses `libc` for `O_NOATIME`, `posix_fadvise`, `syncfs`
- Windows: uses `windows-sys` for `Win32_Storage_FileSystem`
