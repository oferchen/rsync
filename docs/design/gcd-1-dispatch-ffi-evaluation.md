# GCD-1: libdispatch FFI strategy for fast_io

Tracking task: GCD-1. Decision input for GCD-2 through GCD-7.

This doc picks the FFI strategy `fast_io` will use to call Apple's
`libdispatch` (Grand Central Dispatch) on macOS. The macOS I/O acceleration
plan (parent KQ-GCD, #4217) layers kqueue event sources on top of
`dispatch_io_*` async file I/O. GCD-2 implements the async reader, GCD-3 the
writer, GCD-4 the whole-file checksum hasher, GCD-5 the parallel local-copy
queue. This task chooses how those tasks reach libdispatch.

Two prior decision docs cover adjacent material:

- `docs/design/macos-gcd-evaluation.md` weighs `dispatch` vs `objc2-dispatch`
  vs raw FFI.
- `docs/design/gcd-evaluation.md` focuses on the crate-vs-FFI matrix without
  treating `dispatch2` separately.

Neither names `dispatch2` distinctly. This doc closes that gap: it treats the
modern `dispatch2` crate as a first-class option alongside the legacy
`dispatch` crate and the raw-FFI baseline, then picks the same direction the
prior docs converge on for consistency.

## Existing libdispatch surface in the workspace

Workspace grep for `dispatch_` returns no hits in production code that bind
the libdispatch C surface. All matches refer to:

- `transfer` crate symbols that reuse the word in unrelated identifiers
  (`dispatch_message`, `segment_dispatch_totals`, `parallel_dispatch_*`).
- `fast_io` policy tests (`dispatch_uses_mmap_below_threshold` etc.) that
  exercise backend selection, not libdispatch.
- `engine` tests and benches with `parallel_dispatch_overhead` /
  `concurrent_register_and_dispatch_*` covering parallel-apply scheduling.
- `daemon::async_listener::log_dispatch_error` for accept-loop error logging.

`crates/fast_io/Cargo.toml` has no `dispatch`, `dispatch2`, or `block2`
dependency. `crates/fast_io/src/` has no `macos/` subdirectory yet, only
flat modules (`macos_io.rs` for `F_NOCACHE` + `writev`, `sendfile_macos.rs`
for the BSD `sendfile(2)` path). GCD bindings are greenfield.

## Scope of the FFI surface

GCD-2 through GCD-5 need this surface:

- Queues: `dispatch_queue_create`, `dispatch_get_global_queue`,
  `dispatch_async_f`, `dispatch_sync_f`, `dispatch_release` /
  `dispatch_retain` (or the ARC variants).
- Async file I/O: `dispatch_io_create_with_path`, `dispatch_io_read`,
  `dispatch_io_write`, `dispatch_io_close`, `dispatch_io_set_interval`.
- Data handles: `dispatch_data_create`, `dispatch_data_create_map`,
  `dispatch_data_get_size`, `dispatch_data_empty`.
- Constants: `DISPATCH_QUEUE_PRIORITY_DEFAULT` and the
  `DISPATCH_IO_STREAM` / `DISPATCH_IO_RANDOM` channel types.
- Block bridge: completion handlers are Objective-C-style blocks. Either a
  `block2` dep or a hand-rolled `Block_layout` struct is required to ship
  Rust closures across the C boundary.

That is ~12 free functions plus 2 constants plus the block bridge. Small
and well-bounded.

## Option A: `dispatch` crate

`SSheldon/rust-dispatch` (https://crates.io/crates/dispatch).

- **Status**: 0.2 series is the long-published line; a 1.0 has been
  discussed but never shipped. The crate has historically had long quiet
  stretches. Bus factor is one author.
- **API coverage**: `Queue`, `QueueAttribute`, `SerialQueue`,
  `ConcurrentQueue`, `Group`, plus closure submission (`Queue::async`,
  `Queue::sync`, `Queue::after`). **No `dispatch_io_t` or `dispatch_data_t`
  wrapper.** The entire async file-I/O surface that GCD-2/3/4 need is
  absent.
- **MSRV**: not pinned. Verify against the workspace 1.88.0 floor at
  integration time.
- **Dep footprint**: thin. No transitive heavy deps.
- **Licence**: MIT.

The crate covers queues and closures but not async file I/O. Adopting A
would force us to add the `dispatch_io_*` and `dispatch_data_*` FFI
ourselves anyway. The result is a thin queue wrapper plus our own FFI for
the part that actually matters.

## Option B: `dispatch2` crate

`madsmtm`'s `dispatch2` (https://crates.io/crates/dispatch2). Part of the
`objc2` ecosystem.

- **Status**: actively maintained alongside `objc2`. Regular release
  cadence. This is the modern replacement for both the original `dispatch`
  crate and the older `objc2-dispatch` crate name. Live crates.io check
  is required before pinning a version in `fast_io/Cargo.toml`.
- **API coverage**: covers the full dispatch object hierarchy through
  `objc2`-style ARC retain semantics: `DispatchObject`, `DispatchQueue`,
  `DispatchGroup`, `DispatchSource`, `DispatchData`, `DispatchIO`. The
  `DispatchIO` surface is the reason this option is on the list - it gives
  GCD-2/3/4 a safe wrapper out of the box.
- **Dep footprint**: pulls in `objc2` and `block2`. We do not currently
  depend on the `objc2` stack anywhere in the workspace. `objc2` itself
  is small, but the dep chain adds runtime ARC machinery for a single
  macOS file-I/O path that does not need Cocoa/Foundation interop.
- **MSRV**: tracks `objc2`; recent versions require relatively recent
  stable Rust. Verify against 1.88.0 floor.
- **Unsafe surface**: hidden behind safe `objc2`-style wrappers. The
  caller never writes FFI.
- **Licence**: MIT / Apache-2.0 dual.

`dispatch2` covers the surface cleanly, but the dep chain is
disproportionate to the use case: we want C-style `dispatch_io_*` calls,
not the broader Objective-C runtime. Adopting B introduces the `objc2`
stack as a new transitive footprint for one platform path.

## Option C: raw FFI in `fast_io::macos::dispatch`

`libc` exposes `dispatch_queue_t`, `dispatch_object_t`, and a partial set
of dispatch symbols on Apple targets. The remainder is declared in a
private `fast_io::macos::dispatch` module with hand-rolled `extern "C"`
signatures. Linking is automatic: libdispatch ships inside `libSystem.dylib`,
which the Rust toolchain links by default on Apple targets. No
`#[link(name = "dispatch")]`, no `build.rs`.

- **Effort**: ~30 `extern "C"` declarations plus ~4 opaque type aliases
  (`dispatch_io_t`, `dispatch_data_t`, `dispatch_queue_t`, `dispatch_block_t`).
  Plus the small block-bridge module. The `block2` crate, usable
  standalone from the rest of `objc2`, provides `RcBlock` / `StackBlock`
  and removes the need to hand-roll `Block_layout`. One small focused dep
  in place of the `objc2` chain.
- **Maintenance**: libdispatch's ABI has been stable since macOS 10.6. The
  symbols GCD-2/3/4/5 need are foundational and Apple-maintained. The
  long-term carry cost is close to zero.
- **Unsafe footprint**: localised to `fast_io::macos::dispatch`. Safe
  wrappers (`GcdReader`, `GcdWriter`, `GcdQueue`) hide the FFI from
  `engine`, `transfer`, and `daemon`. Consistent with the existing
  io_uring (`crates/fast_io/src/io_uring/`), `clonefile`, and
  `CopyFileExW` patterns in `fast_io`.
- **Error mapping**: `dispatch_io_*` handlers deliver POSIX errno as `i32`.
  Map through `io::Error::from_raw_os_error`, identical to the io_uring
  completion-queue error path in `io_uring_common.rs`.
- **Cross-platform**: a `dispatch_stub` module on non-Apple targets
  returns `Ok(None)` from capability probes, matching the `io_uring_stub`
  and `kqueue_stub` patterns already used in `fast_io`.

## Decision matrix

|                                | A: `dispatch` | B: `dispatch2` | C: raw FFI + `block2` |
|--------------------------------|:-------------:|:--------------:|:---------------------:|
| `dispatch_io_*` coverage       | absent        | full           | full                  |
| `dispatch_data_*` coverage     | absent        | full           | full                  |
| Queue + closure submission     | covered       | full           | full                  |
| Maintenance signal             | low           | high           | high (Apple ABI)      |
| Dependency footprint           | thin          | `objc2` chain  | one small crate (`block2`) |
| Effort to reach GCD-2 readiness| medium (still need FFI for I/O surface) | low | medium (~30 extern fns) |
| Unsafe surface scope           | split         | hidden         | localised to one module |
| Project fit                    | low           | medium         | high                  |

## Recommendation

**Adopt Option C - raw FFI in `crates/fast_io/src/macos/dispatch.rs`, with
`block2` for the handler-block bridge.** GCD-2 through GCD-5 import from
`fast_io::macos::dispatch` only through safe wrappers; the FFI stays
private to `fast_io`.

Reasons:

1. `fast_io` is the designated home for platform FFI and `#[allow(unsafe_code)]`.
   GCD fits the same pattern as io_uring, IOCP, kqueue, `clonefile`,
   `CopyFileExW`. The workspace's long-term "consolidate unsafe in fast_io"
   direction reinforces this placement.
2. The libdispatch surface we need is small (~12 functions). A 100-line
   FFI wrapper is cheaper to maintain than tracking a third-party crate's
   release cadence, MSRV drift, or breaking-change pace. Option A leaves
   `dispatch_io_*` uncovered so it does not actually skip any FFI work.
   Option B covers everything but pulls the `objc2` stack in for one
   platform path.
3. libdispatch's ABI is stable across macOS releases since 10.6. The
   carry cost after initial wiring is near zero. Bus-factor risk on
   Option A and dep-bloat risk on Option B are both avoided.
4. Established precedent in `fast_io`: io_uring, kqueue, `clonefile`,
   `CopyFileExW` all live as raw FFI behind safe wrappers in the same
   crate. GCD follows the same template.

## FFI surface to bind in GCD-2

The first implementation task (GCD-2) introduces
`crates/fast_io/src/macos/dispatch.rs` with these declarations:

```rust
// All extern fns inside an unsafe extern "C" block. Safe wrappers in the
// surrounding module own the SAFETY contracts.
type dispatch_object_t = *mut c_void;
type dispatch_queue_t = *mut c_void;
type dispatch_io_t = *mut c_void;
type dispatch_data_t = *mut c_void;
type dispatch_block_t = *mut c_void;

extern "C" {
    fn dispatch_queue_create(label: *const c_char, attr: *mut c_void) -> dispatch_queue_t;
    fn dispatch_get_global_queue(identifier: isize, flags: usize) -> dispatch_queue_t;
    fn dispatch_async_f(queue: dispatch_queue_t, ctx: *mut c_void,
                        work: extern "C" fn(*mut c_void));
    fn dispatch_release(object: dispatch_object_t);
    fn dispatch_retain(object: dispatch_object_t);

    fn dispatch_io_create_with_path(io_type: u64, path: *const c_char,
                                    oflag: c_int, mode: mode_t,
                                    queue: dispatch_queue_t,
                                    cleanup: dispatch_block_t) -> dispatch_io_t;
    fn dispatch_io_read(channel: dispatch_io_t, offset: off_t, length: usize,
                        queue: dispatch_queue_t, handler: dispatch_block_t);
    fn dispatch_io_write(channel: dispatch_io_t, offset: off_t,
                         data: dispatch_data_t, queue: dispatch_queue_t,
                         handler: dispatch_block_t);
    fn dispatch_io_close(channel: dispatch_io_t, flags: u64);
    fn dispatch_io_set_interval(channel: dispatch_io_t, interval: u64, flags: u64);

    fn dispatch_data_create(buffer: *const c_void, size: usize,
                            queue: dispatch_queue_t,
                            destructor: dispatch_block_t) -> dispatch_data_t;
    fn dispatch_data_create_map(data: dispatch_data_t,
                                buffer_ptr: *mut *const c_void,
                                size_ptr: *mut usize) -> dispatch_data_t;
    fn dispatch_data_get_size(data: dispatch_data_t) -> usize;
}
```

Each safe wrapper that crosses the FFI boundary carries a `// SAFETY:`
comment citing the relevant `dispatch(3)` man page and the invariants the
wrapper enforces (non-null queue, channel ownership transferred to the
cleanup block, errno mapping for the `i32` parameter delivered to the
handler block). Block lifetimes are bridged through `block2::RcBlock`.

## Cross-platform cfg gate

The new module sits under `#[cfg(target_os = "macos")]` only. Non-Apple
targets receive `crates/fast_io/src/macos/dispatch_stub.rs` exposing the
same public API (`GcdReader::probe() -> io::Result<Option<...>>`,
`GcdWriter::probe()`, `GcdQueue::probe()`) returning `Ok(None)`. The
parent `mod macos;` in `fast_io/src/lib.rs` selects between them with
`#[cfg(target_os = "macos")]` / `#[cfg(not(target_os = "macos"))]`,
mirroring `io_uring` vs `io_uring_stub` in the same crate.

The `macos-gcd` Cargo feature gates inclusion in the default build. GCD-2
ships it default-off. GCD-7 decides default-on after GCD-6 bench evidence,
the same cadence used for IUD-5 / RSS-A.LAND.

## Follow-ups

- **GCD-2**: introduce `fast_io::macos::dispatch` with the FFI above plus
  the `GcdReader` safe wrapper for async file reads. Add `block2` to
  `crates/fast_io/Cargo.toml` behind the `macos-gcd` Cargo feature.
- **GCD-3**: ship `GcdWriter` on top of `dispatch_io_write`. Shares the
  queue primitives from GCD-2.
- **GCD-4**: whole-file checksum hasher built on `GcdReader`. No new FFI.
- **GCD-5**: parallel local-copy queue built on `dispatch_get_global_queue`
  + `dispatch_async_f`. Exposes `GcdQueue`.
- **GCD-6**: bench GCD reader/writer/queue against synchronous baseline
  and the kqueue path. Required before any default-on flip.
- **GCD-7**: flip `macos-gcd` to default-on if GCD-6 shows a consistent
  win across the daemon push, daemon pull, and local-copy paths.

## References

- Apple `dispatch(3)` man pages: `dispatch_io_create`, `dispatch_io_read`,
  `dispatch_io_write`, `dispatch_data`, `dispatch_queue_create`.
- `docs/design/macos-gcd-evaluation.md` - prior three-option treatment
  including `objc2-dispatch` (now superseded as `dispatch2`).
- `docs/design/gcd-evaluation.md` - prior focused crate-vs-FFI matrix
  that omitted `dispatch2`.
- `docs/design/macos-kqueue-fast-io.md` - kqueue layer that pairs with
  the dispatch_io data path.
- `crates/fast_io/src/io_uring/` - reference pattern for platform FFI
  behind safe wrappers in `fast_io`.
- `crates/fast_io/src/kqueue/mod.rs` - kqueue FFI, the closest existing
  peer to GCD in `fast_io`.
