# Architecture Walkthrough

This document provides a comprehensive overview of the oc-rsync codebase for
new contributors. It covers the crate structure, data flow, key abstractions,
and where to find things.

## High-Level Data Flow

A typical client-to-server transfer (push via SSH) flows through these layers:

```
User invokes CLI
       │
       ▼
┌─────────────┐   parse args, build ClientConfig
│     cli     │──────────────────────────────────────┐
└─────────────┘                                      │
       │                                             │
       ▼                                             ▼
┌─────────────┐   orchestrate session          ┌───────────┐
│    core     │◀──────────────────────────────▶│  rsync_io │  transport
└─────────────┘   (local/SSH/daemon dispatch)  └───────────┘  adapters
       │
       ├───── local copy ────▶ engine (delta pipeline, file I/O)
       │
       └───── remote ────────▶ transfer (sender/receiver/generator roles)
                                   │
                                   ├──▶ protocol (wire framing, multiplex)
                                   ├──▶ engine   (delta gen/apply)
                                   ├──▶ fast_io  (platform I/O optimizations)
                                   └──▶ metadata (perms, timestamps, ACLs)
```

For daemon mode (`oc-rsync --daemon`), the `daemon` crate replaces `cli` as
the entry point, binding a TCP listener and spawning per-connection threads (or
tokio tasks in async mode) that feed into the same `transfer` pipeline.

## Crate Responsibility Matrix

| Crate | Responsibility | Key Entry Points |
|-------|---------------|------------------|
| `cli` | CLI parsing (Clap v4), output formatting, progress display | `run()` |
| `core` | Orchestration facade - dispatches local/SSH/daemon transfers | `client::run_client()` |
| `engine` | Delta pipeline, local-copy executor, sparse I/O, buffer pool | `local_copy::LocalCopyPlan`, `delta::generate_delta()` |
| `transfer` | Sender/receiver/generator roles, request pipelining, disk commit | `run_server_stdio()`, `GeneratorContext`, `ReceiverContext` |
| `protocol` | Wire protocol v28-32, multiplex MSG_* framing, varint codec | `MplexReader`, `MplexWriter`, `ProtocolVersion` |
| `daemon` | TCP listener, `@RSYNCD:` negotiation, auth, module config | `run_daemon()`, `DaemonConfig` |
| `rsync_io` | Transport adapters - SSH subprocess, TCP streams, negotiation | `SessionHandshake`, `NegotiatedStream` |
| `checksums` | Rolling Adler-32 + strong checksums (MD4/MD5/XXH3), SIMD | `RollingChecksum`, `strong::strategy` |
| `signature` | Block-size heuristics and file signature generation | `calculate_signature_layout()`, `generate_file_signature()` |
| `matching` | Block matching and delta token generation | `DeltaGenerator`, `DeltaSignatureIndex`, `FuzzyMatcher` |
| `filters` | Include/exclude/protect rule evaluation (Chain of Responsibility) | `FilterSet`, `FilterChain` |
| `compress` | Streaming zlib/zstd/lz4 codecs | `zlib::CountingZlibEncoder`, `zstd`, `lz4` |
| `metadata` | Permission bits, timestamps, uid/gid, ACLs, xattrs | `apply_file_metadata()`, `apply_directory_metadata()` |
| `bandwidth` | Token-bucket rate limiting for `--bwlimit` | `BandwidthLimiter` |
| `fast_io` | Platform I/O - io_uring, IOCP, copy_file_range, splice, mmap | Safe public APIs wrapping unsafe platform code |
| `flist` | File list generation and depth-first traversal | `FileListBuilder`, `FileListWalker` |
| `batch` | Offline batch-mode write/replay | `BatchWriter`, `BatchReader` |
| `logging` | Thread-local verbosity flags mirroring upstream `-v`/`--info`/`--debug` | `info_log!`, `debug_log!`, `VerbosityConfig` |
| `logging-sink` | Log destination abstraction (file, syslog, stderr) | - |
| `branding` | Binary name, version strings, env var prefixes | `branding()`, `workspace()` |
| `platform` | Platform detection and capability queries | - |
| `apple-fs` | macOS-specific filesystem operations (clonefile, F_NOCACHE) | - |
| `embedding` | Library embedding entry point (no CLI dependency) | - |
| `test-support` | Shared test utilities and fixtures | - |

## Dependency Graph

```
cli ──▶ core ──┬──▶ engine ──┬──▶ protocol ──▶ checksums
               │             ├──▶ signature ──▶ checksums
               │             ├──▶ matching  ──▶ checksums, signature
               │             ├──▶ compress
               │             ├──▶ metadata
               │             ├──▶ filters
               │             ├──▶ bandwidth
               │             ├──▶ batch
               │             └──▶ fast_io
               │
               ├──▶ transfer ──▶ engine, protocol, fast_io, metadata
               │
               ├──▶ daemon ──▶ transfer, protocol
               │
               ├──▶ rsync_io ──▶ protocol
               │
               └──▶ flist, logging, branding

daemon ──▶ transfer ──▶ (same as above)
```

The key invariant: `protocol` is the lowest layer that touches the wire.
Everything above composes `protocol` primitives into higher-level operations.

## Key Abstractions and Traits

### Checksum Strategy Pattern

```rust
// checksums crate - runtime algorithm selection
trait StrongDigest { fn digest(data: &[u8]) -> Vec<u8>; }

// Implementations: Md4, Md5, Xxh3, Sha256, etc.
// Selected at runtime based on protocol version and negotiated capabilities.
```

The `RollingChecksum` provides O(1) sliding-window updates for block matching.
SIMD-accelerated paths (AVX2, SSE2, NEON) are selected at runtime via
`OnceLock`-cached feature detection.

### Compression Codecs

The `compress` crate uses the Strategy pattern - `zlib`, `zstd`, and `lz4`
modules each expose streaming encoder/decoder types with identical API shapes.
The negotiated algorithm is selected during protocol setup.

### Filter Chain of Responsibility

```rust
// filters crate - first-match-wins evaluation
FilterChain evaluates rules in order:
  Include → path is transferred
  Exclude → path is skipped
  Protect → path cannot be deleted
  No match → default include
```

### Protocol Codec (Version-Aware Encoding)

```rust
// protocol crate - Strategy pattern for wire encoding
ProtocolCodec   // general encoding (varint vs fixed-width based on version)
NdxCodec        // file-list index encoding
MplexReader     // multiplexed input with MSG_* frame demuxing
MplexWriter     // multiplexed output with frame tagging
```

### Builder Pattern

Used extensively for complex configuration:
- `ClientConfig` / `ClientConfigBuilder` - transfer parameters
- `FileListBuilder` - traversal options
- `FilterChain` builder - rule accumulation
- `DiskCommitConfig` - disk thread parameters
- `SignatureLayoutParams` - block-size calculation inputs

### State Machine (Protocol Phases)

The `protocol::state` module defines type-safe connection states with validated
transitions:

```
Greeting → ModuleSelect → Authenticating → Transferring → Closing
```

## Transfer Lifecycle

A remote transfer proceeds through these phases:

### 1. Handshake

The client connects (SSH subprocess or TCP) and exchanges protocol version
numbers. The `transfer::handshake` module handles both binary (protocol >= 30)
and legacy ASCII (`@RSYNCD: 30.0\n`) negotiation styles.

### 2. Protocol Setup

After version agreement, peers exchange:
- Compatibility flags (incremental recursion, checksum seeds, etc.)
- Checksum algorithm negotiation (XXH3 preferred, MD5 fallback)
- Compression codec selection
- Random seed for checksum salting

Handled by `transfer::setup`.

### 3. Filter Exchange

The sender transmits filter rules so the receiver can apply the same
include/exclude logic. The receiver activates multiplex framing after reading
filters.

### 4. File List Transfer

The generator (sender side) walks the source tree via `flist` and transmits
the file list using `protocol::flist` wire encoding. With incremental recursion
(INC_RECURSE), sub-lists are sent lazily as directories are entered.

### 5. Delta Transfer

For each file that needs updating:

```
Generator (sender)                     Receiver
─────────────────                      ────────
                                       Generate signature from basis file
                     ◀── signature ──  (rolling + strong checksums per block)
Match blocks against
source file
                     ── delta stream ──▶
                                       Apply delta: COPY tokens reference
                                       basis blocks, LITERAL tokens carry
                                       new data. Write to temp file.
                                       Verify whole-file checksum.
                                       Atomic rename temp → destination.
                                       Apply metadata (perms, mtime, owner).
```

The receiver uses a **request pipeline** (`transfer::pipeline`) to overlap
signature generation with network I/O, and a **disk commit thread**
(`transfer::disk_commit`) to decouple network receives from disk writes.

### 6. Finalization

- Phase 2 redo pass for files that failed checksum verification
- Delete statistics exchange (`NDX_DEL_STATS`)
- Goodbye message (`NDX_DONE`)
- Exit code propagation

## Platform Abstraction Layer (fast_io)

The `fast_io` crate isolates all unsafe platform code behind safe public APIs.
It implements a fallback chain - each mechanism independently degrades:

| Priority | Mechanism | Platform | Benefit |
|----------|-----------|----------|---------|
| 1 | FICLONE | Linux 4.5+ (Btrfs/XFS) | Instant CoW clone |
| 2 | io_uring | Linux 5.6+ | Batched async syscalls |
| 3 | copy_file_range | Linux 4.5+ | Zero-copy kernel transfer |
| 4 | sendfile | Linux | Zero-copy file-to-socket |
| 5 | splice | Linux 2.6.17+ | Zero-copy socket-to-file |
| 6 | IOCP | Windows Vista+ | Overlapped async I/O |
| 7 | clonefile | macOS | APFS instant copy |
| 8 | Standard I/O | All | BufReader/BufWriter fallback |

Design rules:
- All unsafe blocks live in `fast_io` (or `checksums` for SIMD, `metadata` for
  POSIX FFI).
- Consumer crates (`engine`, `transfer`, `core`, `cli`, `daemon`) are
  `#![deny(unsafe_code)]`.
- Every optimization has a safe fallback path - NFS, FUSE, old kernels, and
  seccomp-restricted environments all work via standard I/O.

## Threading Model

oc-rsync uses OS threads (not async/await) for the transfer pipeline:

```
┌─────────────────────────────────────────────────────────────┐
│                    Transfer Session                          │
├─────────────────────────────────────────────────────────────┤
│                                                             │
│  Main thread (network I/O)                                  │
│  ├── Reads/writes multiplexed protocol stream               │
│  ├── Drives pipeline: sends signature requests,             │
│  │   receives delta streams                                 │
│  └── Enqueues completed deltas to disk commit channel       │
│                                                             │
│  Disk commit thread                                         │
│  ├── Dedicated std::thread via crossbeam bounded channel    │
│  ├── Opens files, writes chunks, fsync, atomic rename       │
│  └── Never blocks the network thread on disk latency        │
│                                                             │
│  Parallel operations (rayon thread pool)                    │
│  ├── File stat() calls above PARALLEL_STAT_THRESHOLD (64)   │
│  ├── Directory metadata application                         │
│  ├── Checksum computation for large file sets               │
│  └── Delta verification in batch mode                       │
│                                                             │
└─────────────────────────────────────────────────────────────┘
```

For the daemon, each connection gets its own thread (sync mode) or tokio task
(async mode). Panics are caught via `catch_unwind` to isolate connection
failures.

Key concurrency primitives:
- `crossbeam-channel` - bounded SPSC between network and disk threads
- `rayon` - parallel iterators for batch CPU-bound work
- `DashMap` - concurrent hash map for shared daemon state
- `BufferPool` with `Mutex<Vec<Vec<u8>>>` - RAII buffer reuse
- `OnceLock` - cached SIMD feature detection

## Where to Find Things

### "I want to understand how files are transferred"

Start with `crates/transfer/src/lib.rs` for the overview, then:
- Receiver role: `crates/transfer/src/receiver/`
- Generator (sender) role: `crates/transfer/src/generator/`
- Delta application: `crates/transfer/src/delta_apply/`
- Request pipelining: `crates/transfer/src/pipeline/`
- Disk commit thread: `crates/transfer/src/disk_commit/`

### "I want to understand the wire protocol"

- Protocol overview: `crates/protocol/src/lib.rs`
- Multiplex framing: `crates/protocol/src/multiplex/` or `envelope`
- File list encoding: `crates/protocol/src/flist/`
- Golden byte tests: `crates/protocol/tests/golden/`
- Varint codec: `crates/protocol/src/varint.rs`

### "I want to understand the delta algorithm"

- Block matching: `crates/matching/src/` (mirrors upstream `match.c`)
- Signature layout: `crates/signature/src/`
- Delta generation: `crates/engine/src/delta/`
- Rolling checksum: `crates/checksums/src/rolling.rs`
- Strong checksums: `crates/checksums/src/strong/`

### "I want to understand local copies (no network)"

- Local copy executor: `crates/engine/src/local_copy/`
- Sparse file I/O: `crates/engine/src/local_copy/` (SparseWriter/SparseReader)
- Filter evaluation: `crates/filters/src/`
- Metadata application: `crates/metadata/src/`

### "I want to understand CLI argument parsing"

- Entry point: `crates/cli/src/frontend/`
- Argument definitions: `crates/cli/src/frontend/arguments/`
- Output formatting: `crates/cli/src/frontend/itemize.rs`, `stats_format.rs`

### "I want to understand daemon mode"

- Daemon entry: `crates/daemon/src/`
- Config parsing (`oc-rsyncd.conf`): `crates/daemon/src/daemon/`
- Authentication: `crates/core/src/auth/` and daemon auth module
- TLS termination: behind `daemon-tls` feature flag

### "I want to add platform-specific I/O optimizations"

- All unsafe I/O: `crates/fast_io/src/`
- io_uring: `crates/fast_io/src/io_uring/`
- IOCP (Windows): `crates/fast_io/src/iocp/`
- copy_file_range: `crates/fast_io/src/copy_file_range/`
- macOS (clonefile, F_NOCACHE): `crates/apple-fs/src/`

### "I want to understand how errors are reported"

- Exit codes: `crates/core/src/exit_code.rs`
- Client errors: `crates/core/src/client/error.rs`
- Role trailers (`[sender]`, `[receiver]`): `crates/transfer/src/role_trailer.rs`
- Message formatting: `crates/core/src/message/`

### "I want to understand testing patterns"

- Golden byte tests (wire format): `crates/protocol/tests/golden/`
- Interop tests (upstream rsync): `tools/ci/run_interop.sh`
- Property tests: scattered in checksum and filter crates
- Test fixtures: `tempfile::TempDir` with `setup_test_dirs()` pattern
- Test support utilities: `crates/test-support/src/`

## Upstream Reference

The upstream C source at `target/interop/upstream-src/rsync-3.4.1/` is the
single source of truth for protocol behaviour. Key files to cross-reference:

| Upstream File | Corresponds To |
|---------------|---------------|
| `main.c` | `core::client`, `transfer` entry points |
| `flist.c` | `flist` crate, `protocol::flist` |
| `generator.c` | `transfer::generator` |
| `receiver.c` | `transfer::receiver` |
| `sender.c` | `transfer::generator` (sender side) |
| `match.c` | `matching` crate |
| `token.c` | `transfer::token_reader`, `token_buffer` |
| `io.c` | `rsync_io` crate |
| `options.c` | `cli::frontend::arguments` |
| `exclude.c` | `filters` crate |
| `rsync.h` | `protocol` constants and types |
| `compat.c` | `transfer::setup` |
| `checksum.c` | `checksums` crate |
