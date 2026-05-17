# Zero-copy file copy: Chain-of-Responsibility evaluation (#2133)

This note evaluates whether the platform-specific zero-copy dispatch in
`crates/fast_io/src/platform_copy/` should be refactored from its current
`#[cfg]`-gated cascade into an explicit Chain-of-Responsibility composed of
trait objects.

The architecture patterns catalogue lists Chain-of-Responsibility as a
registered pattern (used today by `FilterChain` for include/exclude rule
evaluation). Whole-file copy dispatch is the only other place in the tree
that walks a sequence of conditional handlers, so the question is reasonable.
The conclusion is: do not refactor. Keep the cfg-cascade. Apply two targeted,
small improvements (memoise the last winner, dedupe the cleanup helper)
without introducing the trait.

## 1. Current state: inventory of zero-copy paths

The crate currently implements eight distinct kernel zero-copy or CoW
primitives plus the portable `std::fs::copy` fallback. The table below is
exhaustive against `crates/fast_io/src/`.

| Primitive | File:line | Direction | Platform | Threshold | Method tag |
|-----------|-----------|-----------|----------|-----------|------------|
| `FICLONE` ioctl | `platform_copy/dispatch.rs:693` (`try_ficlone_impl`) | file -> file (CoW) | Linux 4.5+, Btrfs/XFS/bcachefs | none (O(1)) | `CopyMethod::Ficlone` |
| `copy_file_range(2)` | `copy_file_range.rs:142` (`try_copy_file_range`, Linux); see also `copy_file_contents` at `copy_file_range.rs:86` | file -> file | Linux 4.5+ same-fs / 5.3+ cross-fs | `>= 64 KiB` (`COPY_FILE_RANGE_THRESHOLD`, `copy_file_range.rs:46`) | `CopyMethod::CopyFileRange` |
| io_uring batched read/write | `copy_file_range.rs:142` (`try_io_uring_copy`) | file -> file | Linux 5.6+ + `io_uring` feature | `>= 256 KiB` (`IO_URING_COPY_THRESHOLD`, `copy_file_range.rs:41`) | n/a (returns bytes) |
| `clonefile(2)` | `platform_copy/dispatch.rs:138` (`clonefile_impl`) | file -> file (CoW) | macOS, APFS only | none (O(1)) | `CopyMethod::Clonefile` |
| `fcopyfile(3)` (`COPYFILE_DATA`) | `platform_copy/dispatch.rs:173` (`fcopyfile_impl`) | file -> file | macOS, all FS | none | `CopyMethod::Copyfile` |
| `CopyFileExW` | `platform_copy/dispatch.rs:217` (`try_copy_file_ex`) | file -> file | Windows | `> 4 MiB` enables `COPY_FILE_NO_BUFFERING` (`platform_copy/dispatch.rs:95`) | `CopyMethod::CopyFileEx` |
| `FSCTL_DUPLICATE_EXTENTS_TO_FILE` | `platform_copy/dispatch.rs:283` (`try_refs_reflink_impl`); range variant at `platform_copy/dispatch.rs:499` | file -> file (CoW) | Windows + ReFS | none (O(1), cluster-aligned) | `CopyMethod::ReFsReflink` |
| `std::fs::copy` | `platform_copy/dispatch.rs:45`, `:82`, `:115`, `:128` (every fall-through site) | file -> file | all | always | `CopyMethod::StandardCopy` |

Two further zero-copy primitives live on a different axis (file <-> socket)
and are outside the file-to-file chain considered here:

- `sendfile(2)` - `sendfile.rs::send_file_to_fd` (file -> socket).
- `splice(2)` via pipe - `splice.rs::try_splice_to_file`, `recv_fd_to_file`
  (socket -> file).

These have their own single-tier-with-fallback structure and do not share
the file-to-file dispatch shape.

The IOCP and io_uring file factories under `iocp/` and `io_uring/` are
batched-async I/O pipelines, not single-call zero-copy primitives. They sit
above the chain (the chain is invoked per-file inside whatever
reader/writer pair the factory hands back) and are also outside the scope.

### Current dispatch structure

The file-to-file dispatch is three platform-gated functions inside
`crates/fast_io/src/platform_copy/dispatch.rs`:

- `platform_copy_impl` for Linux (`dispatch.rs:17`): tries `FICLONE`, then
  (above 64 KiB) `copy_file_range`, then `std::fs::copy`.
- `platform_copy_impl` for macOS (`dispatch.rs:55`): tries `clonefile`, then
  `fcopyfile`, then `std::fs::copy`.
- `platform_copy_impl` for Windows (`dispatch.rs:92`): probes ReFS via
  `is_refs_filesystem`, conditionally tries `FSCTL_DUPLICATE_EXTENTS_TO_FILE`,
  then `CopyFileExW`, then `std::fs::copy`. Above 4 MiB it sets
  `COPY_FILE_NO_BUFFERING` on the `CopyFileExW` call.
- `platform_copy_impl` for other platforms (`dispatch.rs:122`): always
  `std::fs::copy`.

The dispatch is reached through the `PlatformCopy` trait
(`platform_copy/types.rs:122`) - already a Strategy Pattern with
`DefaultPlatformCopy`, `NoCowPlatformCopy`, and `NoZeroCopyPlatformCopy`
implementations swappable at runtime. The `--no-cow` CLI flag selects
`NoCowPlatformCopy`; `ZeroCopyPolicy::Disabled` selects
`NoZeroCopyPlatformCopy`.

A second linear chain lives in `copy_file_range.rs::copy_file_contents`
(`copy_file_range.rs:86-98`), ordered by size threshold:

```
io_uring (>= 256 KiB) -> copy_file_range (>= 64 KiB) -> readwrite fallback
```

The two chains are independent: the platform copy chain calls into the
size-tier chain only on Linux when the destination handle and source handle
have been opened by `platform_copy_impl` itself.

## 2. Decision points and fallback testing

| Step | Trigger | Cleanup on fail | Next step |
|------|---------|-----------------|-----------|
| Linux: `FICLONE` (`dispatch.rs:22`) | always tried | `std::fs::remove_file(dst)` (`dispatch.rs:25`) | `copy_file_range` (if size >= 64 KiB) |
| Linux: `copy_file_range` (`dispatch.rs:35`) | `size_hint >= 64 KiB` | `std::fs::remove_file(dst)` (`dispatch.rs:40`) | `std::fs::copy` |
| Linux: `std::fs::copy` (`dispatch.rs:45`) | always | propagates error | terminal |
| macOS: `clonefile` (`dispatch.rs:61`) | always tried | `std::fs::remove_file(dst)` (`dispatch.rs:66`) | `fcopyfile` |
| macOS: `fcopyfile` (`dispatch.rs:72`) | always tried | `std::fs::remove_file(dst)` (`dispatch.rs:78`) | `std::fs::copy` |
| macOS: `std::fs::copy` (`dispatch.rs:82`) | always | propagates error | terminal |
| Windows: `is_refs_filesystem` probe (`dispatch.rs:97`) | always | n/a | gates ReFS reflink |
| Windows: `FSCTL_DUPLICATE_EXTENTS_TO_FILE` (`dispatch.rs:101`) | on ReFS only | `std::fs::remove_file(dst)` (`dispatch.rs:104`) | `CopyFileExW` |
| Windows: `CopyFileExW` (`dispatch.rs:111`) | always | `std::fs::remove_file(dst)` + std::fs::copy (`dispatch.rs:114-117`) | `std::fs::copy` |
| Windows: `std::fs::copy` (`dispatch.rs:116`) | on `CopyFileExW` fail | propagates error | terminal |

### Are fallback edges tested?

`crates/fast_io/src/platform_copy/tests.rs` covers:

- `platform_copy_falls_through_ficlone_failure` (`tests.rs:317`) - runs
  `DefaultPlatformCopy::copy_file` on tmpfs and asserts the result is one of
  `Ficlone | CopyFileRange | StandardCopy`. This exercises the FICLONE ->
  copy_file_range -> std::fs::copy edges as observed externally.
- `ficlone_returns_unsupported_on_non_linux` (`tests.rs:265`).
- `ficlone_graceful_fallback_on_tmpfs` (`tests.rs:275`).
- `ficlone_fails_on_missing_source` (`tests.rs:306`).
- `clonefile_returns_unsupported_on_non_macos` (`tests.rs:348`).
- `fcopyfile_returns_unsupported_on_non_macos` (`tests.rs:358`).
- `clonefile_copies_data` (`tests.rs:368`).
- `clonefile_fails_when_dst_exists` (`tests.rs:394`).
- `clonefile_fails_on_missing_source` (`tests.rs:407`).
- `default_platform_copy_*` for small / empty / large / binary / nonexistent /
  overwrite cases (`tests.rs:72-172`).
- `parity_default_vs_std_fs_copy` (`tests.rs:232`).

Gaps in the existing coverage:

- The Windows `is_refs_filesystem == false` path that skips
  `FSCTL_DUPLICATE_EXTENTS_TO_FILE` entirely is exercised by the
  platform-conditional cfg, but there is no test that injects a "ReFS says yes
  but `DeviceIoControl` returns `ERROR_INVALID_DEVICE_REQUEST`" failure and
  verifies the fall-through to `CopyFileExW`. The cfg-guard plus the lack of
  a mockable seam makes this hard to fuzz on CI runners.
- The Windows `CopyFileExW` failure -> `std::fs::copy` retry
  (`dispatch.rs:114-117`) is only covered by the platform-portable path
  tests; there is no targeted negative test for `CopyFileExW` returning 0.
- No test asserts that `std::fs::remove_file(dst)` actually runs on each
  fall-through edge. A handler returning `Err` while leaving a partial
  destination behind would silently break the next handler that expects
  "destination must not exist" (notably `clonefile` on macOS).

These gaps are real and should be filled, but they exist independently of
whether dispatch is a cfg-cascade or a CoR chain.

## 3. Chain-of-Responsibility variant

The trait-and-chain refactor would introduce one trait with two methods:

```rust
// hypothetical: crates/fast_io/src/platform_copy/handler.rs

pub(crate) trait ZeroCopyHandler: Send + Sync + std::fmt::Debug {
    /// Returns:
    /// - `Ok(Some(bytes))` if this handler successfully copied the file,
    /// - `Ok(None)` if the handler is not applicable to this triple (e.g.
    ///   ReFS handler on NTFS, FICLONE under a tunable threshold),
    /// - `Err(_)` if the handler attempted the copy and the kernel refused.
    ///
    /// On `Err`, the chain runner cleans up any partial destination before
    /// invoking the next handler.
    fn try_copy(&self, ctx: &CopyCtx<'_>) -> io::Result<Option<u64>>;

    /// Stable identifier used for the `CopyMethod` tag and for telemetry.
    fn method(&self) -> CopyMethod;
}

pub(crate) struct CopyCtx<'a> {
    pub src: &'a Path,
    pub dst: &'a Path,
    pub size_hint: u64,
}
```

A per-platform `&'static [&'static dyn ZeroCopyHandler]` is walked in order:

```rust
fn run_chain(ctx: &CopyCtx<'_>) -> io::Result<CopyResult> {
    for handler in chain_for_platform() {
        match handler.try_copy(ctx) {
            Ok(Some(bytes)) => return Ok(CopyResult::new(bytes, handler.method())),
            Ok(None) => continue,                          // not applicable
            Err(_) => {
                let _ = std::fs::remove_file(ctx.dst);     // cleanup
                continue;
            }
        }
    }
    unreachable!("chain ends in StdCopyHandler which never returns None or Err")
}
```

Chain composition (still cfg-gated, just on slice construction instead of
function body):

```rust
#[cfg(target_os = "linux")]
fn chain_for_platform() -> &'static [&'static dyn ZeroCopyHandler] {
    &[&FicloneHandler, &CopyFileRangeHandler, &StdCopyHandler]
}

#[cfg(target_os = "macos")]
fn chain_for_platform() -> &'static [&'static dyn ZeroCopyHandler] {
    &[&ClonefileHandler, &FcopyfileHandler, &StdCopyHandler]
}

#[cfg(target_os = "windows")]
fn chain_for_platform() -> &'static [&'static dyn ZeroCopyHandler] {
    &[&RefsReflinkHandler, &CopyFileExHandler, &StdCopyHandler]
}
```

In this variant, threshold constants (`64 KiB`, `256 KiB`, `4 MiB`) move
onto the handler that uses them; each handler is independently re-tunable
without editing dispatch.

## 4. Pros of adopting Chain-of-Responsibility

- **Testability**: each handler is a stand-alone unit that can be tested
  against a tempdir without going through the full `PlatformCopy` trait.
  Mock handlers can be inserted into the chain to validate ordering and
  fall-through edges (covering the gaps listed in section 2).
- **Per-call telemetry hook**: the chain runner is a single function, so
  emitting "handler X attempted, returned Y" trace events is one
  modification rather than one per cfg-gated body.
- **Runtime ordering tweaks**: a hypothetical `--prefer=copy_file_range`
  flag or `OC_RSYNC_DISABLE_HANDLER=ficlone` env var becomes a slice filter
  rather than a new `cfg!` branch.
- **Reduces duplicate cleanup**: the four `let _ = std::fs::remove_file(dst);`
  call sites collapse into one inside `run_chain`.
- **Encodes the "skip vs failed" semantic**: today the cfg-cascade conflates
  "FICLONE returned `EXDEV` because the destination is on a different
  mount" with "FICLONE returned `EOPNOTSUPP` because the FS does not support
  reflinks" - both come back as `Err`. The `Ok(None)` vs `Err` split lets
  handlers express "I do not apply, do not bother cleaning up" cleanly.
- **`NoCowPlatformCopy` simplifies**: instead of duplicating the whole
  `PlatformCopy` impl (`platform_copy/mod.rs:106-129`), it becomes a chain
  of `&[&StdCopyHandler]`. Future filtered strategies (e.g. `FicloneOnly`
  for synthetic benchmarks) get the same treatment.

## 5. Cons of adopting Chain-of-Responsibility

- **The dispatch is already short and clear**. The three platform branches
  in `dispatch.rs:17-119` total about 100 LoC including comments. They read
  top-to-bottom as "try fast path, on failure clean up and try next". A
  trait + chain runner + eight handler structs is more code, not less.
- **Per-platform handlers still need `#[cfg]` guards** because they call
  into platform-specific FFI (`libc::clonefile`, `rustix::fs::ioctl_ficlone`,
  `windows_sys::Win32::Storage::FileSystem::CopyFileExW`). The cfg surface
  moves from `dispatch.rs` into eight handler modules; it does not vanish.
  The non-Linux stub for `FicloneHandler` is exactly as ugly as the
  non-Linux stub for `try_ficlone_impl` (`dispatch.rs:707-713`).
- **Dynamic dispatch overhead is real but negligible for whole-file copy**
  - one vtable call per file. Not a correctness concern, listed for
  completeness.
- **Abstraction tax for a closed set**. The handler population is fixed by
  what kernels expose. New primitives appear roughly once per Windows /
  Linux / macOS release cycle (years, not months). The "open for extension"
  benefit of the pattern is overstated for this domain.
- **`PlatformCopy` is already a Strategy**. The trait surface
  (`types.rs:122`) is the swappable interface. Pushing another swappable
  interface one level deeper risks the
  "Strategy-of-Strategies-of-Strategies" anti-pattern where every step is a
  trait and the call graph requires three indirections to read.
- **The size-tier chain in `copy_file_range.rs:86` is genuinely different**.
  It is invoked on already-open `File` handles, not paths; merging it into
  the path-based handler chain would require either opening files twice
  or smuggling open handles through `CopyCtx`. The two chains are easier to
  reason about as two chains.
- **Memoisation does not need a refactor**. The "remember the last winning
  handler" optimisation - which is the only behavioural change that would
  meaningfully improve hot paths - can be added to `DefaultPlatformCopy` as
  an `AtomicUsize` field plus a `match` inside the existing
  `platform_copy_impl`. No trait required.

## 6. Recommendation: do not refactor

Keep the current cfg-cascade. The Chain-of-Responsibility pattern is a
sound fit for chains of policy-style decisions whose population grows over
time and whose handlers compose against a single value type. `FilterChain`
in the filters crate is the textbook case - thousands of rules, all sharing
the same `Path` input. Whole-file copy is the opposite: a fixed, small set
of primitives, each tied to a specific platform with platform-specific FFI,
each with its own error taxonomy. The pattern provides no leverage here
that the existing Strategy-based `PlatformCopy` trait does not already
provide at the right altitude.

The two real shortcomings of the current code - duplicate cleanup at every
fall-through site, no memoisation of the winning handler - are both fixable
without the pattern.

### Targeted improvements (instead of the refactor)

1. **Extract a `cleanup_partial_destination` helper** in
   `platform_copy/dispatch.rs`. Replace the four
   `let _ = std::fs::remove_file(dst);` calls (`dispatch.rs:25, 40, 66, 78,
   104, 114`) with one helper call each. Cost: ~10 LoC saved, zero
   abstraction added.
2. **Add an `AtomicUsize` "last winner" cache to `DefaultPlatformCopy`**.
   On entry, branch to the previously-winning method first; on success keep
   the index; on failure fall through to the linear cascade and update the
   index from the new winner. Cost: ~15 LoC inside the existing
   `platform_copy_impl` functions, no trait surface change. Benefit: on
   100k-file recursive transfers where FICLONE consistently fails (ext4
   destination), files 2..N skip the failed-syscall probe.
3. **Add the negative tests called out in section 2** - inject a
   `CopyFileExW` failure (by passing an unwritable destination) and assert
   the cascade reaches `std::fs::copy`; assert
   `std::fs::remove_file(dst)` actually runs on each fall-through edge.

These three changes capture every meaningful benefit attributed to the CoR
refactor at a fraction of the diff size and without introducing dynamic
dispatch in the I/O hot path.

### When to revisit

Reopen the CoR question if any one of the following becomes true:

- The handler count on a single platform crosses 5 (today: 3 on Linux, 3 on
  macOS, 3 on Windows after counting ReFS, `CopyFileExW`, and the stdlib
  fallback).
- A runtime-tunable preference order becomes a product requirement
  (`--prefer=copy_file_range`, env-var disablement of named handlers, a
  daemon config knob).
- The size-tier chain in `copy_file_range.rs` and the path-based chain in
  `dispatch.rs` start sharing primitives in both directions, making the
  current two-chain split untenable.

Until one of those triggers fires, the cfg-cascade is the right design.

## 7. Cross-references

The following open tasks would each add a candidate handler if CoR were
adopted. Under the do-not-refactor recommendation, they each instead extend
the existing `dispatch.rs` per-platform `platform_copy_impl` function or
add a new file-to-socket primitive in `sendfile.rs` / `splice.rs`:

- **#1389 Windows ReFS reflink** - already landed as
  `try_refs_reflink_impl` (`dispatch.rs:283`) and integrated into the
  Windows cascade. See `docs/design/windows-refs-reflink.md`.
- **#1414 Windows `CopyFileEx`** - already landed as `try_copy_file_ex`
  (`dispatch.rs:217`) and integrated into the Windows cascade. See
  `docs/design/windows-copyfileex-impl.md` and
  `docs/design/windows-copyfileex-platform-copy.md`.
- **#1749 Windows `CopyFileEx` parity** - ongoing parity hardening against
  upstream behaviour; modifies the existing
  `try_copy_file_ex` flag selection (`dispatch.rs:235-239`). No new handler.
- **#1932 IOCP wiring** - lives one altitude above the file-to-file
  dispatch. IOCP factories produce reader/writer pairs that the chain is
  invoked against per file; IOCP is not itself a member of the chain.
- **#2131 Windows `FSCTL_SET_ZERO_DATA` hole punching** - a write-side
  sparse-region primitive (see `docs/design/windows-fsctl-set-zero-data.md`),
  not a file copy primitive. It would attach to the receiver's sparse-write
  path, not to `platform_copy_impl`.
- **#2130 Windows `TransmitFile`** - a file-to-socket zero-copy primitive
  (see `docs/design/windows-transmitfile.md`). It would extend
  `sendfile.rs`'s send-side dispatch, not the file-to-file cascade.

## 8. References

- Current dispatch: `crates/fast_io/src/platform_copy/dispatch.rs`
- Strategy trait: `crates/fast_io/src/platform_copy/types.rs` (line 122)
- `PlatformCopy` impls: `crates/fast_io/src/platform_copy/mod.rs` (lines
  71-129), `crates/fast_io/src/platform_copy/no_zero_copy.rs`
- Size-tier inner chain: `crates/fast_io/src/copy_file_range.rs` (lines
  86-124)
- ReFS filesystem probe: `crates/fast_io/src/refs_detect.rs`
- Fall-through tests: `crates/fast_io/src/platform_copy/tests.rs`
- Pattern catalogue entry: `docs/architecture/design-patterns-catalog.md`
  (Strategy and Chain of Responsibility sections)
