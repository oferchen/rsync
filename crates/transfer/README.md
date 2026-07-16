# transfer

Server-side transfer coordination between sender, receiver, and generator roles.

## Purpose

`transfer` provides the `--server` mode entry points, implementing the rsync
delta transfer algorithm with full protocol 32 compatibility. It orchestrates
the pipeline of protocol phases - handshake, setup, multiplex activation, and
role-based transfer - while overlapping network I/O with disk operations for
maximum throughput.

## Key Public Types

- `GeneratorContext` - sender/generator role: walks file tree, sends file list,
  generates and transmits delta streams
- `ReceiverContext` - receiver role: receives file list, produces signatures,
  applies deltas, commits metadata
- `ServerRole` - enum selecting generator or receiver mode
- `TransferConfig` / `TransferConfigBuilder` - transfer parameter assembly
- `DiskCommitChannel` - SPSC channel decoupling network receives from disk writes
- `Pipeline` - bounded-concurrency request pipeline overlapping I/O with processing
- `TokenReader` / `TokenBuffer` - delta token stream decoding

## Architecture

```
Handshake -> Protocol Setup -> Multiplex Activation -> Role Dispatch
                                                        |
                                          Generator (sender) or Receiver
```

## Dependencies (upstream)

`protocol`, `metadata`, `engine`, `signature`, `filters`, `compress`,
`logging`, `fast_io`, `checksums`

## Dependents (downstream)

`core`

## Features

- `io_uring` / `iocp` - async I/O forwarded to `fast_io`
- `zstd` / `lz4` / `zlib-ng` - compression codec selection
- `incremental-flist` - incremental file list processing (INC_RECURSE)
- `iconv` - filename charset transcoding

## Platform Notes

- Unix: `O_NOATIME` for source file opens, `openat` for temp-file sandbox
- macOS: `apple-fs` for clonefile during local operations
