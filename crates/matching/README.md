# matching

Block matching and delta generation for rsync transfers - implements the core
rsync delta algorithm by comparing input blocks against file signatures.

## Key Public Types

- `DeltaGenerator` - produces delta tokens by rolling over input against a signature
- `DeltaSignatureIndex` - hash table indexing signatures for O(1) block lookup
- `DeltaScript` - ordered sequence of delta tokens for file reconstruction
- `DeltaToken` - individual copy or literal operation in a delta stream
- `FuzzyMatcher` - finds similar basis files for delta transfers (`--fuzzy`)
- `apply_delta` - reconstructs a target file from a basis plus delta tokens

## Modules

- `generator` - delta generation loop (mirrors upstream `match.c`)
- `index` - two-level hash table for signature block lookup
- `script` - delta script types and application
- `optimized_search` - bithash/sequential-match optimizations
- `ring_buffer` - circular buffer for streaming delta generation
- `fuzzy` - fuzzy basis file matching by name similarity and mtime

## Dependencies

- **Upstream:** `signature` (file signatures), `checksums` (rolling + strong), `logging`
- **Downstream:** `engine` (delta pipeline)

## Features

- `tracing` - structured logging instrumentation for performance analysis
- `bench-internal` - exposes internals for benchmark harnesses (never in release builds)
