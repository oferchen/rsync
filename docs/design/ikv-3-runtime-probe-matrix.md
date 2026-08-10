# IKV-3 - Runtime probe matrix for io_uring opcodes

Design spec for a unified runtime probe of io_uring opcode availability on the
running kernel. Sits beside the static per-opcode minimum-kernel constants
shipped by IKV-2 and the audit inventory shipped by IKV-1
([PR #4899](https://github.com/oferchen/oc-rsync/pull/4899)).

This document specifies the API surface, caching strategy, dispatch-hook
integration, telemetry, failure modes, cross-platform layout, and performance
impact. The implementation PR follows; this task ships the design only.

Memory: `[[project_iouring_kernel_version_floor]]`.

## 1. Scope

IKV-3 specs the **runtime** probe matrix that detects per-opcode availability
on whatever kernel oc-rsync is actually executing on. It complements IKV-2's
**static** per-opcode minimum-kernel constants (e.g. `LINKAT_MIN_KERNEL = (5,
15)`) by reconciling expected-vs-actual support at startup.

In scope:

- API surface for a `ProbeMatrix` cached in a process-wide `OnceLock`.
- A single call to `IORING_REGISTER_PROBE` at first access; result cached.
- Dispatch-hook pattern for opcode sites that want runtime probing.
- Telemetry surface (a single `tracing::info!` at first call).
- Failure-mode enumeration: pre-5.6 kernel, EINVAL probe, partial results.
- Cross-platform stub so non-Linux callers compile against the same API.
- Performance impact analysis: one syscall once, sub-nanosecond per lookup.

Out of scope:

- The implementation itself - this is a spec.
- Changes to existing per-opcode `OnceLock<bool>` probes in `linkat.rs`,
  `statx.rs`, `renameat2.rs`, `send_zc.rs`, `shared_ring.rs`, `buffer_ring/`.
  Migrating those onto the matrix is a follow-up; the matrix lands alongside
  them first and replaces them opportunistically.
- CI matrix expansion across LTS kernels (IKV-7..9) and synthesis (IKV-10).

## 2. Pre-conditions

What's already in master that this spec assumes:

- **Hard floor.** `crates/fast_io/src/io_uring/config.rs` defines
  `MIN_KERNEL_VERSION = (5, 6)`. Below that, io_uring is disabled wholesale.
- **Kernel version detection.** `config::parse_kernel_version()` and
  `config::get_kernel_release()` (via `libc::uname`) are already wired and
  exercised by `IoUringProbeResult` and `check_io_uring_reason()`.
- **Probe scaffolding.** `config::count_supported_ops()` already calls
  `Submitter::register_probe(&mut io_uring::Probe::new())` and walks
  `probe.is_supported(op)` for every `op` in `0..=u8::MAX`. The matrix
  generalises that scan into a typed, cached map.
- **Per-opcode minimum-kernel constants.** `crates/fast_io/src/io_uring_common.rs`
  exports `LINKAT_MIN_KERNEL`, `STATX_MIN_KERNEL`, `ASYNC_CANCEL_MIN_KERNEL`,
  `ASYNC_CANCEL_FD_MIN_KERNEL`, and the opcode numerics
  (`IORING_OP_LINKAT = 39`, `IORING_OP_RENAMEAT = 35`, `IORING_OP_STATX = 21`,
  `IORING_OP_ASYNC_CANCEL = 14`). IKV-2's `kernel_floor` module, once it
  lands, becomes the canonical source for the full opcode/min-kernel table
  consumed by the matrix.
- **Existing per-opcode probes** (each its own `OnceLock<bool>` and throwaway
  ring): `linkat::probe_linkat_support`, `statx::probe_statx_support`,
  `renameat2::probe_renameat2_support`, `send_zc::probe_send_zc_support`,
  `shared_ring::probe_poll_add`, `buffer_ring::registration::is_supported`.
  These confirm the design pattern; the matrix consolidates them.

## 3. Probe API surface

New module: `crates/fast_io/src/io_uring/probe.rs` (Linux-gated, with a
non-Linux stub - see section 9).

```rust
// crates/fast_io/src/io_uring/probe.rs
use std::sync::OnceLock;

/// Runtime support map for io_uring opcodes on the executing kernel.
///
/// Built once per process from a single `IORING_REGISTER_PROBE` call and
/// cached for the lifetime of the process. Dispatch sites consult the matrix
/// to decide between an io_uring opcode and its fallback path.
pub struct ProbeMatrix {
    /// `true` at index `op` iff the kernel reports the opcode as supported.
    /// Indexed by raw opcode byte; 256 entries covers the entire `u8` space
    /// `IORING_REGISTER_PROBE` reports against. `false` everywhere if the
    /// probe failed or the kernel is below the 5.6 floor.
    per_opcode: [bool; 256],
    /// Detected kernel major.minor (from `libc::uname`). `None` if the kernel
    /// release string could not be read or parsed.
    kernel_version: Option<(u32, u32)>,
    /// `true` iff `IORING_REGISTER_PROBE` succeeded. Distinguishes "probe
    /// ran and reported zero opcodes" (a real, weird kernel) from "probe
    /// failed, treat everything as unsupported" (the empty-matrix path).
    probe_ok: bool,
}

/// Per-opcode support combining the runtime probe with the static floor.
#[derive(Debug, Clone, Copy)]
pub struct OpcodeSupport {
    /// `true` if `IORING_REGISTER_PROBE` reports the opcode as available on
    /// the running kernel.
    pub available: bool,
    /// Minimum kernel version that ships the opcode, from IKV-2's
    /// `kernel_floor` table. `None` for opcodes without a tracked floor.
    pub expected_min_kernel: Option<(u32, u32)>,
}

impl ProbeMatrix {
    /// Returns `true` iff the kernel reports support for the named opcode.
    ///
    /// Always returns `false` when the probe failed (e.g. pre-5.6 kernel,
    /// EINVAL, or io_uring setup blocked by seccomp).
    pub fn supports(&self, opcode: u8) -> bool {
        self.per_opcode[opcode as usize]
    }

    /// Returns combined runtime and static-floor support information for
    /// `opcode`. `expected_min_kernel` is populated from IKV-2's
    /// `kernel_floor` map; opcodes not in that table return `None` there.
    pub fn support(&self, opcode: u8) -> OpcodeSupport { /* ... */ }

    /// Detected kernel major.minor, or `None` if undetectable.
    pub fn kernel_version(&self) -> Option<(u32, u32)> { self.kernel_version }

    /// Number of opcodes reported as supported.
    pub fn supported_count(&self) -> usize {
        self.per_opcode.iter().filter(|&&b| b).count()
    }

    /// Iterator over opcodes that IKV-2's `kernel_floor` table expects to be
    /// available on the detected kernel version but that the runtime probe
    /// reports as unsupported. Drives the startup telemetry log.
    pub fn unsupported_but_expected(&self) -> impl Iterator<Item = (u8, OpcodeSupport)> + '_ { /* ... */ }

    /// Returns the cached process-wide probe. Runs the probe lazily on the
    /// first call; every subsequent call hits the `OnceLock` cache.
    pub fn cached() -> &'static ProbeMatrix {
        static CACHED: OnceLock<ProbeMatrix> = OnceLock::new();
        CACHED.get_or_init(Self::build)
    }

    /// Test-only constructor letting unit tests drive a known matrix.
    #[cfg(test)]
    pub(crate) fn from_raw(
        per_opcode: [bool; 256],
        kernel_version: Option<(u32, u32)>,
        probe_ok: bool,
    ) -> Self { /* ... */ }

    /// Internal builder: runs `uname`, then `IORING_REGISTER_PROBE`. Returns
    /// the empty matrix (every opcode unsupported) on any failure - see
    /// section 7 for the failure-mode matrix.
    fn build() -> ProbeMatrix { /* ... */ }
}
```

The opcode argument is a raw `u8` to stay compatible with both the constants
in `io_uring_common` (`IORING_OP_LINKAT = 39`, etc.) and the typed
`opcode::*::CODE` constants surfaced by the `io-uring` crate (e.g.
`io_uring::opcode::ReadFixed::CODE`). The dispatch hook pattern in section 5
shows both call styles.

## 4. Caching strategy

- One `OnceLock<ProbeMatrix>` lives inside `ProbeMatrix::cached()`. First
  caller drives `Self::build()`; every other caller reads the already-init
  reference.
- `build()` runs exactly once per process. Subsequent calls cost a single
  acquire-load and a pointer return - no syscall, no allocation.
- Probe failure produces an **empty matrix** (`per_opcode = [false; 256]`,
  `probe_ok = false`). Subsequent `supports(...)` calls return `false`,
  matching the existing per-opcode probes' contract.
- The matrix is immutable. There is no invalidation, no refresh, no
  hot-reload. Kernel capabilities do not change mid-process; pretending
  otherwise would introduce a race window for no real benefit.
- The cache is process-wide, not per-thread. The matrix is `Sync` (only
  `[bool; 256]`, `Option<(u32, u32)>`, `bool`), so a single `&'static`
  reference is safe to share.

## 5. Dispatch hook integration

The matrix is **opt-in** at each dispatch site. Existing sites that already
gate on the wholesale `MIN_KERNEL_VERSION` (5.6) plus their own per-opcode
`OnceLock<bool>` keep working unchanged; the matrix is for new sites or for
migrating the scattered per-opcode probes onto a single cache.

### Direct opcode dispatch

```rust
use crate::io_uring::probe::ProbeMatrix;
use io_uring::opcode;

if ProbeMatrix::cached().supports(opcode::ReadFixed::CODE) {
    // io_uring submission path
} else {
    // fallback to read(2) / readv(2)
}
```

### Dispatch by named constant

```rust
use crate::io_uring::probe::ProbeMatrix;
use crate::io_uring_common::IORING_OP_LINKAT;

if ProbeMatrix::cached().supports(IORING_OP_LINKAT) {
    submit_linkat_sqe(...)
} else {
    nix::unistd::linkat(...)
}
```

### Migrating an existing per-opcode probe

The current pattern in `linkat.rs` (`OnceLock<bool>` + throwaway ring) becomes
a one-line read once the matrix lands:

```rust
// Before
pub fn linkat_supported() -> bool {
    *LINKAT_SUPPORTED.get_or_init(probe_linkat_support)
}

// After
pub fn linkat_supported() -> bool {
    ProbeMatrix::cached().supports(IORING_OP_LINKAT)
}
```

Migration drops one `OnceLock`, one throwaway-ring construction, and one
`register_probe` call per opcode. Total saved at full migration: six
throwaway rings + six `register_probe` syscalls at startup, replaced by one
of each.

The hook is **optional**: sites that already check `MIN_KERNEL_VERSION` at
module init are correct on conformant kernels. The probe matters when an
opcode is theoretically present (kernel >= floor) but actually missing - the
vendor-backport / container-runtime case enumerated in section 7.

## 6. Telemetry surface

On first call to `ProbeMatrix::cached()`, emit one `tracing::info!` for
observability. Format:

```rust
tracing::info!(
    target: "fast_io::io_uring::probe",
    kernel_version = ?matrix.kernel_version(),
    supported = matrix.supported_count(),
    total_tracked = matrix.tracked_opcode_count(),
    unsupported_but_expected = ?matrix.unsupported_but_expected()
        .map(|(op, _)| op).collect::<Vec<_>>(),
    "io_uring runtime probe matrix initialised"
);
```

Fields:

- `kernel_version`: detected `(major, minor)` or `None`.
- `supported`: count of opcodes reported by `IORING_REGISTER_PROBE`.
- `total_tracked`: number of opcodes in IKV-2's `kernel_floor` table (the
  set oc-rsync actually cares about).
- `unsupported_but_expected`: opcodes whose `expected_min_kernel` is `<=`
  the detected kernel but that the probe reports as missing. Empty on a
  conformant kernel; non-empty on a vendor-patched / container / backported
  kernel.

Users on weird kernels (custom build, vendor backport, restrictive container
runtime, seccomp filter that hides specific opcodes) see exactly which
opcodes the runtime probe disagreed with the static table on. That output
turns "io_uring is silently slow" into a one-line diagnosis.

The log is fired exactly once, from inside `build()`. No per-call logging on
the hot path.

## 7. Failure modes

| Scenario | `ProbeMatrix::build()` behaviour | `supports(_)` returns |
|---|---|---|
| `uname` succeeds, parses to `< (5, 6)` | Empty matrix; `kernel_version = Some(major, minor)`; `probe_ok = false`. No ring constructed. | `false` for every opcode. |
| `uname` succeeds, parses to `>= (5, 6)`, `io_uring_setup` blocked (seccomp / container) | Empty matrix; `kernel_version = Some(major, minor)`; `probe_ok = false`. | `false` for every opcode. |
| `uname` fails / unparseable | Empty matrix; `kernel_version = None`; `probe_ok = false`. | `false` for every opcode. |
| `io_uring_setup` succeeds but `IORING_REGISTER_PROBE` returns `EINVAL` (some patched 5.6 builds have the opcode but a buggy probe protocol) | Empty matrix; `kernel_version = Some(...)`; `probe_ok = false`. | `false` for every opcode. |
| `IORING_REGISTER_PROBE` succeeds with partial support (e.g. opcodes A and B reported, opcode C absent) | Populated matrix; `per_opcode[A] = true`, `per_opcode[B] = true`, `per_opcode[C] = false`; `probe_ok = true`. | `supports(A) -> true`, `supports(B) -> true`, `supports(C) -> false`. |
| `IORING_REGISTER_PROBE` reports every opcode in the tracked table | Fully populated matrix; `probe_ok = true`. | `true` per opcode in the table; `false` for opcodes the kernel does not know. |

Empty-matrix-on-failure means every existing dispatch site falls back exactly
as it does today when its own `OnceLock<bool>` probe returns `false`. The
matrix never widens the io_uring blast radius.

## 8. Acceptance criteria

For the IKV-3 implementation PR (not this doc):

1. `crates/fast_io/src/io_uring/probe.rs` exists with the API specified in
   section 3.
2. `mod probe;` is declared from `crates/fast_io/src/io_uring/mod.rs`.
3. `ProbeMatrix::cached()` initialises via `OnceLock` on the first call and
   returns the same `&'static` reference on every subsequent call.
4. `ProbeMatrix::build()` runs `libc::uname`, applies the 5.6 floor, then
   calls `Submitter::register_probe`. Every error path returns the empty
   matrix (see section 7). No `panic!`, no `expect`, no `unwrap` on a
   fallible path.
5. The startup `tracing::info!` from section 6 fires exactly once, inside
   `build()`.
6. Unit test (cross-platform): construct a `ProbeMatrix` via `from_raw` with
   known opcode bits; assert `supports(...)` returns the bit at the right
   index for several opcode codes (zero, one, a tracked opcode, 255).
7. Unit test (cross-platform): assert that the empty matrix returns `false`
   for every opcode and that `supported_count() == 0`.
8. Unit test (cross-platform): assert that `unsupported_but_expected()`
   correctly flags opcodes whose `expected_min_kernel <= kernel_version` but
   whose `per_opcode` bit is `false`.
9. Integration test (Linux-only, `#[cfg(target_os = "linux")]`): assert
   `ProbeMatrix::cached()` runs without panic. On Linux >= 5.6 with a
   functioning io_uring, assert `supported_count() > 0`. The test must
   degrade gracefully (skip, not fail) when `io_uring_setup` is blocked, so
   it works under restrictive CI runners.
10. `cargo build --release -p fast_io` succeeds on Linux, macOS, and Windows.
    The non-Linux stub from section 9 keeps the API surface portable.
11. No new `#[allow(unsafe_code)]` outside what `fast_io` already permits.

## 9. Cross-platform layout

The Linux module is gated:

```rust
// crates/fast_io/src/io_uring/mod.rs
#[cfg(target_os = "linux")]
pub mod probe;
```

The non-Linux stub re-exports a compatible API from
`crates/fast_io/src/io_uring_stub.rs` (the existing pattern for every
Linux-only module in the subtree, e.g. `io_uring_stub/linkat.rs`):

```rust
// crates/fast_io/src/io_uring_stub/probe.rs (new)
pub struct ProbeMatrix { _private: () }
pub struct OpcodeSupport {
    pub available: bool,
    pub expected_min_kernel: Option<(u32, u32)>,
}
impl ProbeMatrix {
    pub fn supports(&self, _opcode: u8) -> bool { false }
    pub fn support(&self, _opcode: u8) -> OpcodeSupport {
        OpcodeSupport { available: false, expected_min_kernel: None }
    }
    pub fn kernel_version(&self) -> Option<(u32, u32)> { None }
    pub fn supported_count(&self) -> usize { 0 }
    pub fn unsupported_but_expected(&self) -> std::iter::Empty<(u8, OpcodeSupport)> {
        std::iter::empty()
    }
    pub fn cached() -> &'static ProbeMatrix {
        static EMPTY: ProbeMatrix = ProbeMatrix { _private: () };
        &EMPTY
    }
}
```

Cross-platform callers compile against the same `supports(...)` /
`cached()` surface; on non-Linux the matrix is constant-empty and the call
folds to a `false` return that the optimiser hoists out of the dispatch
site.

## 10. Performance impact

Zero on the hot path:

- **Init cost.** One `uname(2)` (already amortised by
  `config::get_kernel_release`'s usage), one `io_uring_setup(2)` on a
  4-entry throwaway ring, one `IORING_REGISTER_PROBE`. Approximately three
  syscalls + a one-page `mmap` for the ring, all paid once on first call.
- **Per-call cost.** A single `OnceLock` acquire-load (atomic) plus an
  array index into `[bool; 256]`. Sub-nanosecond on any commodity CPU.
- **Memory.** `[bool; 256] + Option<(u32, u32)> + bool` is < 280 bytes
  including padding. One allocation, never freed, never duplicated.

`[bool; 256]` is preferred over a `BTreeMap<u8, OpcodeSupport>` because:

- Lookup is O(1) instead of O(log n).
- The byte indices used by `IORING_REGISTER_PROBE` already span the full
  `u8` space; sparseness offers no win.
- Branch prediction stays clean: the matrix is read-mostly after init.

Compared to today's pattern (one `OnceLock<bool>` + one throwaway-ring
probe per opcode, scattered across six modules), the matrix replaces six
syscall-and-ring constructions with one, and six `OnceLock` reads per
opcode lookup with zero (the matrix `OnceLock` is read once per call site,
not once per opcode).

## 11. Cross-references

- IKV-1 audit (merged): [PR #4899](https://github.com/oferchen/oc-rsync/pull/4899)
  - opcode inventory feeding the matrix's tracked-opcode set.
- IKV-2 spec (in flight): `kernel_floor` per-opcode minimum-kernel constants
  module. This spec consumes that table for `expected_min_kernel` and for
  `unsupported_but_expected()`.
- IKV-4 README kernel-tier table (merged):
  [PR #4902](https://github.com/oferchen/oc-rsync/pull/4902).
- IKV-5 man-page (merged):
  [PR #4907](https://github.com/oferchen/oc-rsync/pull/4907).
- IKV-6 release-notes scaffold (merged):
  [PR #4904](https://github.com/oferchen/oc-rsync/pull/4904).
- IKV-7/8/9 (pending): CI cells exercising 5.10, 5.15, and 6.1 kernels
  against the runtime probe matrix.
- IKV-10 (pending): synthesize per-kernel matrix results from IKV-7..9.
- Existing per-opcode probes (consolidation targets):
  - `crates/fast_io/src/io_uring/linkat.rs::probe_linkat_support`
  - `crates/fast_io/src/io_uring/statx.rs::probe_statx_support`
  - `crates/fast_io/src/io_uring/renameat2.rs::probe_renameat2_support`
  - `crates/fast_io/src/io_uring/send_zc.rs` (`is_supported`)
  - `crates/fast_io/src/io_uring/shared_ring.rs::probe_poll_add`
  - `crates/fast_io/src/io_uring/buffer_ring/registration.rs::is_supported`
- Existing scan helper: `crates/fast_io/src/io_uring/config.rs::count_supported_ops`.
- Memory note: `[[project_iouring_kernel_version_floor]]`.
