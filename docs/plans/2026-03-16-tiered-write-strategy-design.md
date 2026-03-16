# Tiered Write Strategy Design

## Context

oc-rsync's local copy path uses a single write strategy: temp file + rename for
all transfers. This matches upstream rsync's behavior (receiver.c) but leaves
performance on the table. The rsync wire protocol does not dictate how the
receiver writes data to disk - these are client-side optimizations invisible to
the remote end.

Benchmarks show a 1.86x regression for 1GB initial sync vs upstream. While the
root cause requires profiling (Track 1), the write path architecture should use
the best available platform primitives (Track 2).

## Design

### Two Orthogonal Concerns

**Destination handling** - how the output file is opened and finalized:

| Strategy | Open | Finalize | When |
|----------|------|----------|------|
| `Direct` | `create_new` | cleanup on failure | No existing destination |
| `AnonymousTempFile` | `O_TMPFILE` | `linkat` | Existing dest, Linux 3.11+ |
| `TempFileRename` | `mkstemp` | `rename` | Fallback, non-Linux, --partial, --delay-updates, --temp-dir |

**Data transfer** - how bytes move from source to destination fd:

| Method | Data copies | Syscalls (1GB) | Platform |
|--------|------------|----------------|----------|
| Zero-copy (`copy_file_range`) | 0 | 1 | Linux 4.5+ |
| io_uring batched writes | 1 (userspace to kernel) | ~16 | Linux 5.6+ |
| Buffered read/write | 1 (userspace to kernel) | ~1024 (1MB buf) | All |

These concerns are independent. `WriteStrategy` governs destination handling in
`execute_transfer`. The data transfer fallback chain lives in
`copy_file_contents_buffered` in `fast_io`.

### Robustness Model

Each tier provides robustness appropriate to its scenario:

- **Existing destination**: temp file (anonymous or named) protects the old file
  from corruption on interruption. The old file survives until atomic
  rename/linkat replaces it.
- **No existing destination**: nothing to protect. `create_new(true)` prevents
  races (EEXIST). `remove_incomplete_destination()` cleans up on failure.
  Equivalent to upstream's temp-file discard since there is no prior file.
- **--partial / --delay-updates / --temp-dir**: user explicitly requested staging
  semantics. Always use `TempFileRename`.

### WriteStrategy Enum

```rust
/// Destination handling strategy for file writes.
///
/// Selected once per file in `execute_transfer` based on destination state,
/// platform, and option flags. Orthogonal to data transfer method
/// (copy_file_range / io_uring / buffered), which is handled in `fast_io`.
enum WriteStrategy {
    /// Write directly to final path. Used when destination does not exist.
    /// Guarded by: no --partial, no --delay-updates, no --temp-dir.
    /// create_new(true) prevents races with concurrent writers.
    Direct,

    /// Anonymous temp file via O_TMPFILE + linkat. Linux 3.11+.
    /// No orphan temp files on crash. Metadata set via fd before file is
    /// visible in the namespace.
    AnonymousTempFile,

    /// Named temp file via mkstemp + rename. Upstream rsync default.
    /// Universal fallback for non-Linux, older kernels, or when user
    /// options require explicit staging.
    TempFileRename,
}
```

### Selection Logic

```rust
fn select_write_strategy(
    existing_metadata: Option<&Metadata>,
    partial_enabled: bool,
    delay_updates_enabled: bool,
    temp_dir_configured: bool,
) -> WriteStrategy {
    // User options that require staging override everything
    if partial_enabled || delay_updates_enabled || temp_dir_configured {
        return WriteStrategy::TempFileRename;
    }
    // No existing destination - direct write is safe and optimal
    if existing_metadata.is_none() {
        return WriteStrategy::Direct;
    }
    // Existing destination on Linux - try anonymous temp file
    #[cfg(target_os = "linux")]
    if o_tmpfile_available() {
        return WriteStrategy::AnonymousTempFile;
    }
    // Fallback
    WriteStrategy::TempFileRename
}
```

`o_tmpfile_available()` probes once via `OnceLock`, same pattern as io_uring
kernel detection in `fast_io/src/io_uring/config.rs`.

### Data Transfer Fallback Chain

```rust
// fast_io/src/copy_file_range.rs
pub fn copy_file_contents_buffered(
    src: &File, dst: &File, len: u64, buf: &mut [u8],
) -> io::Result<u64> {
    // Tier 1: zero-copy (Linux 4.5+)
    if let Ok(copied) = try_copy_file_range(src, dst, len) {
        return Ok(copied);
    }
    // Tier 2: io_uring batched writes (Linux 5.6+)
    #[cfg(all(target_os = "linux", feature = "io_uring"))]
    if let Ok(copied) = try_io_uring_copy(src, dst, len, buf) {
        return Ok(copied);
    }
    // Tier 3: buffered read/write (universal)
    copy_file_contents_readwrite_with_buffer(src, dst, len, buf)
}
```

### v0.6.0 Platform Abstraction (Future)

The current implementation prepares for a `PlatformCopy` trait in v0.6.0 that
abstracts per-platform I/O:

```rust
trait PlatformCopy {
    /// Zero-copy transfer. Returns None if unsupported on this fs/kernel.
    fn try_zero_copy(src: &File, dst: &File, len: u64) -> io::Result<Option<u64>>;

    /// Batched or buffered fallback.
    fn copy_buffered(src: &File, dst: &File, len: u64, buf: &mut [u8]) -> io::Result<u64>;

    /// Open destination with platform-optimal strategy.
    fn open_destination(path: &Path, strategy: WriteStrategy) -> io::Result<DestinationHandle>;

    /// Finalize destination (linkat, rename, or no-op).
    fn finalize(handle: DestinationHandle, final_path: &Path) -> io::Result<()>;
}
```

Platform implementations:

| | `try_zero_copy` | `copy_buffered` | `open_destination` |
|---|---|---|---|
| **Linux** | `copy_file_range` | io_uring, then read/write | `O_TMPFILE` or direct |
| **macOS** | `clonefile` / `fcopyfile` | read/write | temp + rename |
| **Windows** | `CopyFileEx` | read/write | temp + rename |

Today's code maps directly onto this future trait:
- `try_copy_file_range()` becomes `Linux::try_zero_copy()`
- `try_io_uring_copy()` becomes part of `Linux::copy_buffered()`
- `copy_file_contents_readwrite_with_buffer()` is the universal fallback
- `WriteStrategy` selection maps to `open_destination()` + `finalize()`

No traits or new modules are introduced now. The fallback chain in
`copy_file_contents_buffered` and the `WriteStrategy` enum in `execute_transfer`
are positioned so v0.6.0 extraction is a refactor, not a rewrite.

## Implementation Scope

### Now (v0.5.9 series)

1. **Done (PR #2738)**: `Direct` write strategy, 1MB buffer tier
2. **Next**: `try_io_uring_copy()` in fallback chain
3. **Next**: `AnonymousTempFile` strategy (O_TMPFILE + linkat)
4. **Next**: Instrumentation to profile 1GB regression root cause

### v0.6.0

5. Extract `PlatformCopy` trait
6. macOS `fcopyfile` / `clonefile` integration
7. Windows `CopyFileEx` integration

## Files Modified

| File | Change |
|------|--------|
| `crates/engine/src/local_copy/executor/file/copy/transfer.rs` | `WriteStrategy` enum, `select_write_strategy()` |
| `crates/fast_io/src/copy_file_range.rs` | `try_io_uring_copy()` in fallback chain |
| `crates/fast_io/src/io_uring/file_writer.rs` | Expose copy helper for fallback use |

No wire format changes. No protocol impact. Pure client-side optimization.
