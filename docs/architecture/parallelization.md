# Parallelization Architecture

This document describes what is parallelized in oc-rsync, what is intentionally
serial, and the design constraints behind each decision.

---

## Parallelization Architecture

oc-rsync uses [rayon](https://docs.rs/rayon) for CPU-bound and I/O-bound
parallelism. Parallelism is applied selectively: only operations that are
independent and large enough to amortize thread-pool dispatch overhead are
parallelized.

### What is parallelized

**stat() calls during file list building** (`crates/flist/src/parallel.rs`,
`crates/transfer/src/parallel_io.rs`):
- Directory enumeration is sequential (traversal order must be deterministic
  for protocol compatibility).
- After paths are collected, `stat()` syscalls are issued in parallel via
  rayon's work-stealing pool.
- The `map_blocking` helper in `crates/transfer/src/parallel_io.rs` is the
  single internal API for this pattern.

**Signature generation** (`crates/signature/src/parallel.rs`):
- For files >= 256 KB (`PARALLEL_THRESHOLD_BYTES`), block checksums are
  computed in parallel using `par_chunks(16)`.
- Within each rayon chunk, the SIMD batch API processes multiple blocks
  through multi-lane hashing (AVX2, SSE2, NEON), combining thread-level
  and data-level parallelism.
- Files below the threshold use sequential generation to avoid dispatch
  overhead.

**Quick-check stat() batches** (`crates/transfer/src/receiver/mod.rs`):
- During the generator phase, pending files are stat-checked to find basis
  files. When the batch size reaches `PARALLEL_STAT_THRESHOLD = 64`, the
  stat + signature computation is issued as a `par_iter()` batch.
- Below this threshold, sequential iteration is faster.
- The same threshold is reused for directory metadata application
  (`crates/transfer/src/receiver/directory.rs`).

**Local-copy checksums and metadata**
(`crates/engine/src/local_copy/executor/`):
- Checksum computation for local copies uses rayon parallel iterators when
  the file count exceeds `PARALLEL_STAT_THRESHOLD`.
- Directory metadata application (chmod, chown, utimes, ACLs) runs in
  parallel after sequential `create_dir_all`.

### What is serial

**The wire protocol pipeline** - see the dedicated section below.

**Directory enumeration** - `read_dir` traversal is always sequential to
guarantee the file list order that the rsync protocol requires.

**Wire I/O within a role** - all reads and writes on a single connection are
serialized within each role (sender, receiver, generator). The protocol
requires in-order delivery.

### SPSC disk commit channel

Network I/O (parsing wire tokens) and disk I/O (writing file data) are
decoupled by a lock-free single-producer / single-consumer channel
(`crates/transfer/src/pipeline/spsc.rs`).

- Built on `crossbeam_queue::ArrayQueue` with `AtomicBool` disconnection flags.
- Zero syscalls: pure userspace spin-wait via `std::hint::spin_loop`. No
  futex, no `thread::park`, no condvar.
- Default capacity: `DEFAULT_CHANNEL_CAPACITY = 128` slots
  (`crates/transfer/src/disk_commit.rs`).
  At ~32 KB average chunk size, this is ~4 MB of peak buffered data.
- Protocol: the network thread sends `FileMessage` items (`Begin`, `Chunk`,
  `Commit`, `Abort`, `Shutdown`). The disk thread replies with
  `io::Result<CommitResult>` on a second SPSC channel.
- Buffer recycling: the disk thread returns used `Vec<u8>` chunks on a third
  SPSC channel for reuse by the network thread, eliminating per-chunk
  malloc/free overhead.
- Checksum computation is done on the disk thread to overlap hashing with
  disk I/O and remove the work from the network-critical path.

### Parallelism thresholds

| Threshold | Value | Location |
|-----------|-------|----------|
| `PARALLEL_STAT_THRESHOLD` | 64 items | `crates/transfer/src/receiver/mod.rs` |
| `PARALLEL_THRESHOLD_BYTES` | 256 KB | `crates/signature/src/parallel.rs` |
| Signature batch size | 16 blocks | `crates/signature/src/parallel.rs` |
| SPSC channel capacity | 128 slots | `crates/transfer/src/disk_commit.rs` |

---

## Wire Protocol Pipeline Limitation

### Structure

The rsync wire protocol uses a three-party pipeline:

```
sender (generator) ──[file list + deltas]──▶ receiver
receiver ──[signatures]──▶ generator
```

In a remote transfer, the local process runs as both generator and receiver
on separate threads, while the remote process runs as sender. Each direction
uses one OS thread:

- **Generator thread**: walks the local file tree, issues stat() and signature
  requests, reads receiver acknowledgements.
- **Receiver thread**: reads delta data from the wire, writes to disk via the
  SPSC disk-commit thread.
- **Disk-commit thread**: performs all file I/O, metadata application, and
  rename.

### Why throughput does not scale linearly with cores

File indices must be processed in-order. The protocol assigns each file a
sequential index; the sender sends deltas in that order, the receiver must
acknowledge them in that order, and the generator must process acks in that
order. This ordering requirement means:

- The network-facing part of each role is single-threaded by design.
- Adding cores helps only the CPU-bound work (checksum computation, signature
  generation) that can be batched and parallelized offline before or after the
  serial wire I/O.
- On a 32-core machine, a transfer of many small files will show near-zero
  scaling beyond ~3 threads (generator, receiver, disk-commit) because the
  bottleneck is the wire round-trip per file, not CPU.

### Future options

- **Pipelining within a single file**: overlapping signature transmission for
  file N with delta reception for file N-1. The current implementation
  already does this at the batch level via `pipeline.available_slots()`.
- **Out-of-order transfer with reordering**: the sender could transmit files
  out of order and include a reorder buffer. This would require a protocol
  extension and is not wire-compatible with upstream rsync 3.4.1.
- **Multiple concurrent connections**: rsync has no built-in support for
  splitting a transfer across multiple connections. Tools like `rrsync` or
  split-source scripts are the conventional workaround.

---

## SSH Transport

### Process model

SSH transport uses `std::process::Command::spawn()` to start one OS process
per transfer (`crates/rsync_io/src/ssh/builder.rs`). This matches upstream
rsync's process model exactly: upstream calls `do_cmd()` which forks a child
`ssh` process and communicates over its stdin/stdout pipes.

The `SshCommand::spawn()` method configures `Stdio::piped()` for stdin,
stdout, and stderr, then calls `command.spawn()` on the standard library
`Command`. The resulting `SshConnection` exposes split read/write halves
(`SshConnection::split()`) used by the server infrastructure.

### Implication: no connection multiplexing

One process per transfer means:

- Two concurrent transfers to the same host spawn two independent SSH
  processes, each performing its own handshake and authentication.
- It is not possible to multiplex multiple module transfers over a single
  already-established SSH connection from within oc-rsync.
- The SSH process's lifetime is tied to the transfer. The exit code of the
  child is waited on after the transfer completes and mapped to an rsync exit
  code via `map_child_exit_status()`.

### User-side workaround

OpenSSH `ControlMaster` / `ControlPath` transparently multiplexes multiple
SSH sessions over a single authenticated connection. Users running many
sequential oc-rsync transfers can configure this in `~/.ssh/config`:

```
Host fileserver.example.com
    ControlMaster auto
    ControlPath ~/.ssh/cm-%r@%h:%p
    ControlPersist 10m
```

This is invisible to oc-rsync and requires no code changes.

### Future option: libssh2

An in-process SSH library such as
[libssh2](https://www.libssh2.org/) or [russh](https://docs.rs/russh) would
allow:
- Connection reuse across transfers without OS-level ControlMaster.
- Lower per-transfer setup cost (no fork + exec + handshake).
- Programmatic key management without relying on the user's `ssh-agent`.

This would be a substantial dependency addition and would require careful
integration with the existing read/write split interface used by the server
infrastructure. It is not planned for the current release cycle.

---

## Memory Model

### Buffer pool

`BufferPool` (`crates/engine/src/local_copy/buffer_pool.rs`) provides a
thread-safe pool of reusable I/O buffers:

- Backed by `Mutex<Vec<Vec<u8>>>`. The lock is held only during acquire/release,
  minimizing contention.
- Wrapped in `Arc` so `BufferGuard` (the RAII handle) can hold an owned
  reference without borrow-checker issues when the pool is part of a larger
  context.
- Pool capacity defaults to `std::thread::available_parallelism()` buffers.
  Excess buffers returned when the pool is full are simply dropped.

### Adaptive buffer sizing

`adaptive_buffer_size(file_size)` selects an I/O buffer size scaled to the
file being transferred:

| File size | Buffer size |
|-----------|-------------|
| < 64 KB | 8 KB |
| 64 KB - 1 MB | 32 KB |
| 1 MB - 64 MB | 128 KB (matches `COPY_BUFFER_SIZE`, pool default) |
| 64 MB - 256 MB | 512 KB |
| >= 256 MB | 1 MB |

`BufferPool::acquire_adaptive_from()` uses the pool-default (128 KB) path
when the file size falls in the medium range, so medium files always reuse
pooled buffers. Oversized or undersized buffers are resized to the pool
default on return.

There is no hard cap on how large a buffer can grow - the adaptive sizes above
are the current upper bound, set by `ADAPTIVE_BUFFER_HUGE = 1 MB`.

### Memory-mapped basis files

`MapFile` (`crates/transfer/src/map_file.rs`) provides a sliding-window view
over a basis file during delta application, mirroring upstream rsync's
`map_file()` / `map_ptr()` / `unmap_file()` pattern from `fileio.c`.

Three strategies are available, selected via `AdaptiveMapStrategy`:
- `BufferedMap`: sliding 256 KB window with sequential-access overlap reuse.
- `MmapStrategy` (Unix only): memory-mapped access for zero-copy large file
  reads.
- Threshold: files >= 1 MB (`MMAP_THRESHOLD`) use `MmapStrategy`; smaller
  files use `BufferedMap`.

### Parallel error collection

Operations that run in parallel (rayon stat batches, directory metadata
application) return errors by collecting `Option<(PathBuf, String)>` results
from each rayon worker and flattening them after the parallel section. No
`Arc<Mutex<Vec>>` is used for error accumulation - rayon's `collect()` handles
fan-in without shared mutable state.

The `fast_io::ParallelResult<T>` type (`crates/fast_io/src/parallel.rs`) is
used for parallel I/O operations where both successes and indexed errors need
to be returned to the caller.
