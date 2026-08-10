# macOS GCD integration: `dispatch` crate vs raw libdispatch FFI (GCD-1)

Tracking task: GCD-1. Decision input for GCD-2..GCD-7.

## Goal

Pick the Rust binding for Apple's Grand Central Dispatch (`libdispatch.dylib`)
that `fast_io` will use to build a GCD-backed async file reader (GCD-2) and
writer (GCD-3) on macOS. The target API surface is `dispatch_io_create_with_path`
/ `dispatch_io_read` / `dispatch_io_write` / `dispatch_queue_create`, plus the
`dispatch_data_t` and `dispatch_io_t` reference-counted handles. This is the
macOS peer of the Linux io_uring path; see `docs/design/macos-kqueue-fast-io.md`
for the kqueue layer that complements it.

Decision is binding for GCD-2 through GCD-7. Whatever GCD-1 picks becomes the
single import path; downstream tasks do not get to re-litigate per call site.

## Options under evaluation

- **Option A** — the `dispatch` crate (https://crates.io/crates/dispatch).
- **Option B** — `objc2-dispatch` from the `objc2` ecosystem.
- **Option C** — raw FFI through `libc::dispatch_*` symbols, or hand-rolled
  `extern "C"` declarations in a private `fast_io::macos::dispatch_sys` module.

`fast_io` already permits `#[allow(unsafe_code)]` on functions that touch
platform FFI (CLAUDE.md: "fast_io ... platform copy dispatch"), so any of the
three is policy-compatible.

## Option A — the `dispatch` crate

`SSheldon/rust-dispatch`. Originally written by the `objc` crate author.

- **Version**: needs live crates.io check. The 0.2 line is the long-published
  series; a 1.0 rewrite has been discussed. Do not quote a version number in
  follow-up work without re-checking.
- **Last commit**: needs live GitHub check. The crate has historically had
  long quiet stretches; this is a known concern.
- **MSRV**: not pinned in the published 0.2 series. Treat as "tracks recent
  stable" and verify against our 1.88.0 floor before integration (GCD-2).
- **Type surface**: `Queue`, `QueueAttribute`, `Group`, `SerialQueue`,
  `ConcurrentQueue`, and closure submission via `Queue::async`/`sync`/
  `after`. **No `dispatch_io_t` wrapper in 0.2.** This is the binding gap
  that matters for us: the type we actually need (`dispatch_io_t`,
  `dispatch_data_t`) is not exposed, only the queue-and-block primitives.
- **Licence**: MIT.
- **Dependency footprint**: thin. No transitive heavy deps.

The crate covers what most consumers want from GCD (queues + closures), but
the async-I/O surface — `dispatch_io_create_with_path`, `dispatch_io_read`,
`dispatch_io_write`, `dispatch_io_set_interval`, `dispatch_io_close` — would
have to be added by us. That makes "use the dispatch crate" effectively
"use the dispatch crate's queue type and FFI the rest", which is not a
clean win over Option C.

## Option B — `objc2-dispatch`

`madsmtm/objc2`'s dispatch binding. Part of the `objc2` framework binding
family, used widely for Cocoa/Foundation Rust interop.

- **Maintenance**: well-maintained alongside the larger `objc2` ecosystem;
  releases happen on a regular cadence. Needs live crates.io check for the
  current version; do not quote a version without re-checking.
- **Type surface**: covers the dispatch object hierarchy via the `objc2`
  ARC/retain semantics (`DispatchObject`, `DispatchQueue`, `DispatchGroup`,
  `DispatchSource`, `DispatchData`, `DispatchIO`). The full `dispatch_io_*`
  surface is the part of the API we care about and is the reason this option
  is on the list.
- **Dependency cost**: heavier than Option A. Brings in `objc2`,
  `objc2-foundation`-adjacent infra, and the `objc2` ARC machinery. We do
  not currently depend on the `objc2` stack anywhere else; this would be
  a new transitive footprint just for one macOS file-I/O path.
- **Project fit**: clean API but the dep footprint is hard to justify when
  our usage is exclusively the C-style `dispatch_io_*` calls. `objc2` shines
  when you also want NSString/NSURL/etc — we don't.

## Option C — raw FFI

`libc` exposes the GCD symbols on Apple targets (`libc::dispatch_queue_t`,
`libc::dispatch_io_t`, `libc::dispatch_io_create_with_path`, etc.) as part of
its Apple-platform surface. We can also declare them locally in a
`fast_io::macos::dispatch_sys` module and link `-ldispatch` (in practice via
`libSystem`, which already re-exports it; no extra build.rs).

Sketch of the read wrapper:

```rust
// SAFETY: queue is a non-null dispatch_queue_t obtained from
// dispatch_queue_create; io is a dispatch_io_t obtained from
// dispatch_io_create_with_path; the trailing handler block is dropped
// after completion per libdispatch semantics.
unsafe fn dispatch_read(
    io: dispatch_io_t,
    offset: off_t,
    length: usize,
    queue: dispatch_queue_t,
    handler: &Block<dyn Fn(bool, dispatch_data_t, i32) + Send + 'static>,
) -> Result<(), io::Error> {
    // dispatch_io_read returns void; errors surface through the handler's
    // `error: i32` parameter (errno-style) and the `done: bool` flag.
    libc::dispatch_io_read(io, offset, length, queue, handler.as_ptr());
    Ok(())
}
```

The block / closure wrapper is the only non-trivial piece. The `block2` crate
(part of `objc2` but usable standalone) gives us `RcBlock` and `StackBlock`
to pass Rust closures across the C boundary. That is one small, focused dep
versus pulling the whole `objc2-dispatch` surface.

Error mapping: `dispatch_io_*` handlers report POSIX errno values in the
trailing `i32`. Map straight through `io::Error::from_raw_os_error`.

## Decision matrix

|                      | Option A: `dispatch` | Option B: `objc2-dispatch` | Option C: raw FFI + `block2` |
|----------------------|:---:|:---:|:---:|
| Maintenance          | LOW (uncertain) | HIGH | HIGH (libc + block2) |
| Type safety          | MED (queues only) | HIGH | MED (we own SAFETY) |
| Dep cost             | LOW | MED-HIGH | LOW |
| FFI completeness     | LOW (no dispatch_io) | HIGH | HIGH (all of libdispatch) |
| Project fit          | MED | LOW | HIGH |

## Recommendation

**Go with Option C — raw FFI through `libc` and `block2`.**

Reasons:

- The Rust API we actually need (`dispatch_io_t` + `dispatch_data_t`) is the
  surface Option A does not cover. Using A would force us to add the FFI
  declarations ourselves anyway, so we would carry both a maintenance-risk
  third-party dep *and* hand-rolled FFI.
- Option B covers the surface but brings the `objc2` ecosystem in for a
  single non-Objective-C macOS feature. We use no other Cocoa/Foundation
  Rust APIs today; the dep is disproportionate.
- `fast_io` already holds platform FFI under `#[allow(unsafe_code)]` for
  io_uring, IOCP, `clonefile`, and `CopyFileExW`. Adding GCD FFI fits the
  same pattern and keeps the unsafe surface in one crate, consistent with
  CLAUDE.md's "consolidate unsafe in fast_io" direction.
- `block2` is small, well-maintained, and used independently of the full
  `objc2` stack.

## What GCD-2..GCD-7 will assume

- New module `crates/fast_io/src/macos/dispatch_sys.rs` holding the FFI
  declarations not in `libc`, plus thin safe wrappers over
  `dispatch_queue_create`, `dispatch_io_create_with_path`,
  `dispatch_io_{read,write,close}`, and `dispatch_data_create_map`.
- `block2 = "<version>"` added to `crates/fast_io/Cargo.toml` behind a
  `macos-gcd` Cargo feature (default-off until GCD-6 bench justifies a flip).
- Public API (used by GCD-2 reader, GCD-3 writer, GCD-4 hasher) exposes
  only Rust types: `GcdReader`, `GcdWriter`, `GcdQueue`. The FFI never
  leaks past `dispatch_sys`.
- Non-macOS targets get a stub module returning `Ok(None)` for capability
  probes, matching the io_uring stub pattern in `fast_io::io_uring_stub`.

## References

- Apple `dispatch(3)` man page: `man 3 dispatch_io` (see also
  `dispatch_io_create`, `dispatch_io_read`, `dispatch_io_write`,
  `dispatch_data`). Available locally on any macOS install.
- `docs/design/macos-kqueue-fast-io.md` for the kqueue layer that
  complements the dispatch_io data path.
- `crates/fast_io/src/io_uring_stub.rs` as the cross-platform stub pattern
  GCD will follow on non-macOS targets.
