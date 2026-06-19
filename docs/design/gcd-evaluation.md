# GCD-1: `dispatch` crate vs raw libdispatch FFI

Tracking task: GCD-1. Decision input for GCD-2 through GCD-7.

This doc evaluates the two binary options for Grand Central Dispatch (GCD)
bindings on macOS so the implementation tasks (`GCD-2` async reader, `GCD-3`
async writer, `GCD-4` checksum hasher, `GCD-5` parallel local-copy
dispatcher) can proceed against a single committed import path. A longer
three-option treatment (including `objc2-dispatch`) lives at
`docs/design/macos-gcd-evaluation.md`; this document is the focused
crate-vs-FFI matrix called for by GCD-1.

## Scope

GCD-2 through GCD-4 need the `dispatch_io_t` async file-I/O surface:
`dispatch_io_create_with_path`, `dispatch_io_read`, `dispatch_io_write`,
`dispatch_io_set_interval`, `dispatch_io_close`, plus `dispatch_data_t`
reference-counted handles and `dispatch_data_create_map`. GCD-5 needs the
queue primitives: `dispatch_queue_create`, `dispatch_get_global_queue`, and
`dispatch_async` / `dispatch_sync` for closure submission. A handler block
abstraction (`Block_t`) is required across both groups because libdispatch
delivers completion via blocks.

## Current macOS surface in `fast_io`

`crates/fast_io/src/macos_io.rs` is the only macOS-specific I/O module
today. It uses `libc::fcntl(fd, F_NOCACHE, 1)` and `libc::writev` for
synchronous scatter-gather writes. `crates/fast_io/src/kqueue/{mod,timer}.rs`
wire kqueue event sources (`KQ-1` audit, PR #5965). Workspace-wide grep for
`dispatch_*` (`dispatch_queue`, `dispatch_io`, `dispatch_async`) returns no
hits in production code outside of `transfer` crate identifiers that share
the word. No `#[link(name="System")]` attributes exist in `fast_io`; FFI is
limited to `libc` symbols today, and `crates/fast_io/Cargo.toml` has no
`dispatch` or `block2` dependency. GCD bindings would be greenfield.

## Option A - `dispatch` crate

`SSheldon/rust-dispatch` (https://crates.io/crates/dispatch).

- **Published version**: 0.2 series is the long-published line. A 1.0 has
  been discussed but not shipped at the time of evaluation. Live
  crates.io and GitHub checks are required before pinning a version in
  `fast_io/Cargo.toml`.
- **Maintenance signal**: known to have long quiet stretches; bus factor
  is the original author. This is the single biggest risk in the option.
- **MSRV**: not formally pinned. Verify against the workspace 1.88.0
  floor at integration time (GCD-2).
- **API coverage**: queues (`Queue`, `QueueAttribute`, `SerialQueue`,
  `ConcurrentQueue`, `Group`) plus closure submission (`Queue::async`,
  `Queue::sync`, `Queue::after`). **No `dispatch_io_t` or `dispatch_data_t`
  wrapper.** The async file-I/O surface that drives GCD-2 / GCD-3 / GCD-4
  is the part the crate does not cover.
- **Dispatch sources**: not exposed in 0.2.
- **Safety**: queue submission is safe; blocks are handled internally via
  the crate's own block bridge. The crate is not Send/Sync-loose: queues
  are `Clone` reference handles, matching libdispatch refcount semantics.
- **Transitive deps**: thin. No heavy ecosystem pulled in.
- **Licence**: MIT (compatible).

The crate covers what most consumers want (queues + closures) but skips
the async I/O surface that is the entire reason GCD is on the table for
us. Adopting Option A would force us to FFI-declare `dispatch_io_*` and
`dispatch_data_*` ourselves anyway, which collapses A into "a thin queue
wrapper plus our own FFI for the part that matters". The maintenance-risk
dep buys little once that is true.

## Option B - raw FFI via `libc` + locally declared `extern "C"`

`libc` already exposes `dispatch_queue_t`, `dispatch_object_t`, and a subset
of dispatch symbols on Apple targets. The remainder (`dispatch_io_*`,
`dispatch_data_*` map/concat helpers) is declared in a private
`fast_io::macos::dispatch_sys` module with hand-rolled `extern "C"`
signatures. Linking is automatic: libdispatch is re-exported by
`libSystem.dylib`, which Rust links by default on Apple targets, so no
`#[link(name = "dispatch")]` and no `build.rs` are needed.

- **Effort estimate**: ~30 to 50 `extern "C"` declarations to cover
  `dispatch_queue_create`, `dispatch_get_global_queue`,
  `dispatch_async_f`, `dispatch_sync_f`,
  `dispatch_io_create_with_path`, `dispatch_io_read`, `dispatch_io_write`,
  `dispatch_io_close`, `dispatch_io_set_interval`, `dispatch_data_create`,
  `dispatch_data_create_map`, `dispatch_data_get_size`,
  `dispatch_release` / `dispatch_retain` (or libc equivalents on the ARC
  variants), and the `DISPATCH_QUEUE_PRIORITY_*` constants. Plus the small
  block-bridge module needed to pass Rust closures as handler blocks.
- **Block handling**: completion handlers are delivered as
  Objective-C-style blocks. The `block2` crate (small, well-maintained,
  usable standalone from the `objc2` framework family) provides
  `RcBlock`/`StackBlock` wrappers. Using `block2` for blocks plus
  `libc` for queue and dispatch_io symbols is the minimal-dep path. The
  alternative is hand-rolling a `Block_layout` struct, which is more
  unsafe surface for no functional gain.
- **Maintenance burden**: libdispatch is an Apple-maintained system
  library with a stable ABI that has not broken in years. The `extern`
  surface is small and changes infrequently. The maintenance cost after
  initial wiring is close to zero. We already maintain larger FFI
  surfaces for io_uring (`crates/fast_io/src/io_uring/`),
  `clonefile`, `CopyFileExW`, and `landlock`.
- **Unsafe footprint**: localised to `fast_io::macos::dispatch_sys`,
  consistent with the unsafe-code policy that permits FFI under
  `#[allow(unsafe_code)]` in `fast_io`. Safe wrappers (`GcdReader`,
  `GcdWriter`, `GcdQueue`) hide the FFI from `engine`, `transfer`, and
  `daemon`.
- **Error mapping**: `dispatch_io_*` handlers deliver POSIX errno values
  as `i32`. Map through `io::Error::from_raw_os_error` - the same pattern
  used in `io_uring_common.rs`.
- **Cross-platform**: a `dispatch_sys` stub on non-macOS targets returns
  `Ok(None)` from capability probes, matching the `io_uring_stub` and
  `kqueue_stub` patterns already used in `fast_io`.

## Decision matrix

|                                | A: `dispatch` crate | B: raw FFI + `block2` |
|--------------------------------|:-------------------:|:----------------------:|
| `dispatch_io_*` coverage       | absent              | full                   |
| `dispatch_data_*` coverage     | absent              | full                   |
| Queue + closure submission     | covered             | full                   |
| Maintenance signal             | low / uncertain     | high (Apple ABI)       |
| Dependency footprint           | thin                | one small crate (`block2`) |
| Effort to reach GCD-2 readiness| medium (still need FFI for I/O surface) | medium (~30-50 extern fns) |
| Future-proofness               | low (bus factor)    | high (system library)  |
| Unsafe surface scope           | small but split     | localised to one module|
| Project fit                    | medium              | high                   |

## Recommendation

**Adopt Option B - raw FFI via `libc` and a local `dispatch_sys` module,
with `block2` for the handler-block bridge.** GCD-2 / GCD-3 / GCD-4 should
all import from `fast_io::macos::dispatch_sys` only through the safe
wrappers; the FFI itself stays private to `fast_io`.

Reasoning:

- The crate's coverage gap (`dispatch_io_*`) is exactly the surface we
  need. Picking A would mean hand-rolling that FFI anyway, so we would
  pay the maintenance-risk dep cost without skipping any of the FFI work.
- libdispatch's ABI stability is stronger than any third-party crate's
  release cadence. Apple has shipped libdispatch since macOS 10.6 with
  no ABI breaks; the symbols we touch are foundational.
- The unsafe policy already permits this pattern in `fast_io` and the
  long-term direction in CLAUDE.md is to consolidate unsafe code in
  `fast_io`. New macOS FFI fits cleanly.
- `block2` is small, actively maintained, and usable without the rest of
  the `objc2` framework family. It is the minimum incremental dep.
- The risk of A becoming stale forces a future migration; eating that
  cost upfront avoids a second rewrite when 0.2 stops compiling.

## Sequencing for GCD-2 through GCD-7

- **GCD-2 (sender async reader)**: introduces `fast_io::macos::dispatch_sys`
  with the FFI for queues, dispatch_io_create_with_path, dispatch_io_read,
  dispatch_io_close, dispatch_data_create_map. Adds `block2` to
  `crates/fast_io/Cargo.toml` behind a `macos-gcd` Cargo feature
  (default-off until GCD-6 bench justifies the flip). Ships the
  `GcdReader` safe wrapper.
- **GCD-3 (receiver async writer)**: extends `dispatch_sys` with
  dispatch_io_write. Ships `GcdWriter`. Shares the queue primitives from
  GCD-2.
- **GCD-4 (checksum whole-file hasher)**: reuses `GcdReader` from GCD-2.
  No new FFI needed.
- **GCD-5 (parallel local-copy queue)**: extends `dispatch_sys` with
  dispatch_get_global_queue + dispatch_async_f and ships `GcdQueue`.
- **GCD-6**: benches the GCD path against the synchronous baseline and
  the kqueue path for the daemon socket multiplex case.
- **GCD-7**: flip the `macos-gcd` feature to default-on if GCD-6 shows a
  consistent win, mirroring the IUD-5 / RSS-A.LAND flip cadence.

## Risks

- **Bus factor on A**: not material to this recommendation since A is
  rejected.
- **Block-bridge ABI**: `block2`'s RcBlock layout matches Apple's
  Block_layout. A break here would be visible at compile time; runtime
  surprises are unlikely given how widely `block2` is exercised.
- **Cross-arch (Intel vs Apple Silicon)**: libdispatch symbols are
  identical across both. No arch-specific FFI is needed.
- **CI coverage**: macOS runner already exercises `fast_io::macos_io`
  paths. Adding `GcdReader`/`GcdWriter` to that cell is straightforward;
  no new runner is required.

## References

- Apple `dispatch(3)` man pages: `dispatch_io_create`,
  `dispatch_io_read`, `dispatch_io_write`, `dispatch_data`,
  `dispatch_queue_create`.
- `docs/design/macos-gcd-evaluation.md` - extended three-option treatment
  including `objc2-dispatch`.
- `docs/design/macos-kqueue-fast-io.md` - kqueue layer that complements
  the GCD data path.
- `crates/fast_io/src/io_uring/` - reference pattern for platform FFI
  organised behind safe wrappers in `fast_io`.
- `crates/fast_io/src/kqueue/mod.rs` - kqueue FFI pattern, the closest
  existing peer in `fast_io`.
