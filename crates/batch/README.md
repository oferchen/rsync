# batch

Batch mode support for offline/disconnected rsync transfer workflows - enables
recording a transfer to a file and replaying it later on a different machine.

## Key Public Types

- `BatchConfig` - configuration for capture or replay operation
- `BatchMode` - enum selecting `Write` (capture) or `Read` (replay)
- `BatchHeader` - wire-format header (stream flags, protocol version, compat flags, checksum seed)
- `BatchStats` - transfer statistics appended at end of batch file
- `BatchFlags` - bitmap controlling which protocol features are active
- `BatchWriter` - captures protocol stream to a batch file
- `BatchReader` - replays a previously captured batch file
- `BatchError` / `BatchResult` - error types for batch operations

## Modules

- `format` - batch file wire format parsing and serialization
- `reader` - batch file reading and validation
- `writer` - batch file creation
- `script` - companion shell script generation for replay
- `replay` - replay orchestration

## Dependencies

- **Upstream:** `protocol` (wire format types), `metadata` (file entry types), `filetime`
- **Downstream:** `core` (orchestration facade)

## Features

- `zstd` - enables `protocol`'s zstd codec so the test suite can build a
  zstd-framed batch body. Batch replay is pinned to zlib (upstream
  compat.c:417-418) and never decodes zstd; this feature adds no replay support.
