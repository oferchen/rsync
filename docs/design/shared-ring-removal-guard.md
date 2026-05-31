# Shared-Ring Removal Guard (IUR-6.c)

## Problem

The IUR-1 audit (`docs/audits/io-uring-shared-ring-audit.md`) identified that
three io_uring factories - `file_writer`, `file_reader`, and `socket_writer` -
serialized parallel submissions through a single `Arc<Mutex<IoUring>>` ring.
Under rayon-parallel callers, this caused contention on the mutex and
effectively negated the batching benefit of io_uring.

IUR-3 migrated these factories to per-thread rings via `per_thread_ring.rs`,
eliminating the serialization. Without a regression guard, future contributors
could reintroduce the pattern unknowingly.

## Solution

A grep-based CI lint (`tools/ci/check_shared_ring_removal.sh`) runs in the
`fmt + clippy` job and fails the build if `Arc<Mutex` or
`static Mutex<IoUring>` patterns appear in the guarded factory modules.

### Guarded modules

| File | Role |
|------|------|
| `crates/fast_io/src/io_uring/file_writer.rs` | Per-file disk write SQE submission |
| `crates/fast_io/src/io_uring/file_reader.rs` | Per-file read SQE submission |
| `crates/fast_io/src/io_uring/socket_writer.rs` | Network send SQE submission |
| `crates/fast_io/src/io_uring/file_factory.rs` | Factory dispatching reader/writer creation |
| `crates/fast_io/src/io_uring/socket_factory.rs` | Factory dispatching socket reader/writer creation |

### Allowlisted modules

- `send_zc.rs` - Uses `Arc<Mutex<IoUring>>` for per-connection zero-copy
  sender. This is pinned to a single connection, not a contention point for
  parallel file I/O.
- `shared_ring.rs` - Implements a per-session reader+writer ring topology
  (two fds sharing one ring). This is architecturally distinct from the old
  global bottleneck; a session ring is not shared across threads.

## Checks performed

1. **Arc<Mutex pattern** - Matches `Arc<Mutex` in any guarded file. Catches
   `Arc<Mutex<IoUring>>`, `Arc<Mutex<RawIoUring>>`, and renamed variants.
2. **Static mutex pattern** - Matches `static.*Mutex.*IoUring`,
   `OnceLock.*Mutex.*IoUring`, or `lazy_static.*Mutex.*IoUring` to catch
   global shared ring singletons.

## Correct alternative

Hot-path factories must use `per_thread_ring::with_ring()` which provides a
thread-local `IoUring` instance. The ring is lazily constructed per thread and
dropped on thread exit. See `docs/design/iur-2-per-thread-rings.md` for the
full design.

## Integration

The check runs as a step in the `lint` job of `.github/workflows/ci.yml`,
after clippy and before the test matrix. It requires no additional
dependencies (uses grep) and adds negligible CI time.

## Future work

When IUR-6.b removes `shared_ring.rs` entirely, the guard can be extended to
reject any `mod shared_ring` declaration in `mod.rs`, preventing the module
from being reintroduced.
