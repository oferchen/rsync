# Zero-copy file copy: Chain-of-Responsibility refactor (#2133)

This note proposes refactoring the platform-specific zero-copy dispatch in
`crates/fast_io/` from linear `cfg`-gated branches into an explicit
Chain-of-Responsibility composed of trait objects, with a memoised "winning
handler" cache so successful probes are not repeated per file. The change is
internal to `fast_io`; no public API surface changes, no wire-format changes,
no upstream-compatibility changes.

## 1. Inventory of zero-copy paths in `fast_io`

The crate currently implements seven distinct kernel zero-copy / CoW primitives
plus a portable read/write fallback. The list below is exhaustive against
`crates/fast_io/src/`:

| Primitive | File | Direction | Platform | Threshold | Method tag |
|-----------|------|-----------|----------|-----------|------------|
| `FICLONE` ioctl | `platform_copy/dispatch.rs::try_ficlone_impl` | file -> file (CoW) | Linux 4.5+, Btrfs/XFS/bcachefs | none (O(1)) | `CopyMethod::Ficlone` |
| `copy_file_range(2)` | `copy_file_range.rs::try_copy_file_range` | file -> file | Linux 4.5+ same-fs / 5.3+ cross-fs | `>= 64 KiB` | `CopyMethod::CopyFileRange` |
| `sendfile(2)` | `sendfile.rs::send_file_to_fd` | file -> socket | Linux | `>= 64 KiB` (`SENDFILE_THRESHOLD`) | n/a (returns bytes) |
| `splice(2)` (via pipe) | `splice.rs::try_splice_to_file` / `recv_fd_to_file` | socket -> file | Linux 2.6.17+ | `>= 64 KiB` | n/a |
| `clonefile(2)` | `platform_copy/dispatch.rs::clonefile_impl` | file -> file (CoW) | macOS, APFS only | none (O(1)) | `CopyMethod::Clonefile` |
| `fcopyfile(3)` (`COPYFILE_DATA`) | `platform_copy/dispatch.rs::fcopyfile_impl` | file -> file | macOS, all FS | none | `CopyMethod::Copyfile` |
| `CopyFileExW` | `platform_copy/dispatch.rs::try_copy_file_ex` | file -> file | Windows | `> 4 MiB` enables `COPY_FILE_NO_BUFFERING` | `CopyMethod::CopyFileEx` |
| `FSCTL_DUPLICATE_EXTENTS_TO_FILE` | `platform_copy/dispatch.rs::try_refs_reflink_impl` | file -> file (CoW) | Windows + ReFS | none (O(1), cluster-aligned) | `CopyMethod::ReFsReflink` |
| io_uring batched read/write | `copy_file_range.rs::try_io_uring_copy` | file -> file | Linux 5.6+ + `io_uring` feature | `>= 256 KiB` (`IO_URING_COPY_THRESHOLD`) | n/a |
| `std::fs::copy` | `platform_copy/dispatch.rs` (every fall-through) | file -> file | all | always | `CopyMethod::StandardCopy` |

Sendfile and splice live on a different axis (socket endpoint) and are out of
scope for the file-to-file chain proposed below; they retain their own
single-tier-with-fallback structure inside `sendfile.rs` and `splice.rs`.

## 2. Current dispatch logic (linear `cfg` branches)

The file-to-file dispatch is currently three monolithic, platform-gated
functions inside `crates/fast_io/src/platform_copy/dispatch.rs`:

- `#[cfg(target_os = "linux")] platform_copy_impl` - tries `FICLONE`, then
  (above 64 KiB) `copy_file_range`, then `std::fs::copy`. Each branch hard-codes
  cleanup of the destination on failure (`std::fs::remove_file(dst)`).
- `#[cfg(target_os = "macos")] platform_copy_impl` - tries `clonefile`, then
  `fcopyfile`, then `std::fs::copy`.
- `#[cfg(target_os = "windows")] platform_copy_impl` - probes ReFS via
  `is_refs_filesystem`, conditionally tries `FSCTL_DUPLICATE_EXTENTS_TO_FILE`,
  then `CopyFileExW`, then `std::fs::copy`. Above 4 MiB it sets
  `COPY_FILE_NO_BUFFERING` on the `CopyFileExW` call.
- `#[cfg(not(any(...)))] platform_copy_impl` - always `std::fs::copy`.

Inside `crates/fast_io/src/copy_file_range.rs::copy_file_contents` there is a
second linear chain ordered by size threshold: io_uring (>= 256 KiB) ->
`copy_file_range` (>= 64 KiB) -> `copy_file_contents_readwrite`. The two
chains are independent: the platform copy chain calls the size-tier chain only
on Linux when the size threshold is met.

### Problems with the current shape

1. Adding a new primitive (e.g. `splice` for file-to-file via two `splice`
   calls, or BSD `copy_file_range`) requires editing every platform branch and
   threading another threshold constant through `copy_file_contents`.
2. The threshold constants (`64 KiB`, `256 KiB`, `4 MiB`) are hard-coded inside
   the dispatch fns, not on the primitive. Tuning requires editing both files.
3. The "remove the partial destination on failure" cleanup is duplicated at
   every fall-through site.
4. There is no memoisation. If `clonefile` returns `EXDEV` (cross-device) on
   the first file of a 100k-file recursive transfer, every subsequent file
   pays the failed-syscall cost just to learn the same answer.
5. The `NoCowPlatformCopy` variant
   (`crates/fast_io/src/platform_copy/mod.rs:106`) and any future filtered
   strategy (e.g. `FicloneOnly` for synthetic benchmarks) must duplicate the
   whole impl rather than skipping a single handler.
6. Tests currently reach into `dispatch.rs` private helpers (`try_ficlone_impl`,
   `clonefile_impl`, ...) to exercise individual paths. A trait-based design
   lets each handler be tested in isolation against a `MockHandler`.

## 3. Proposal: `CopyHandler` trait + chain

Introduce one trait with a single fallible method:

```rust
// crates/fast_io/src/platform_copy/handler.rs

pub(crate) trait CopyHandler: Send + Sync + std::fmt::Debug {
    /// Returns `Ok(Some(bytes))` if this handler successfully copied the file,
    /// `Ok(None)` if the handler is not applicable to this (src, dst, size)
    /// triple (e.g. ReFS handler on NTFS, FICLONE under threshold), and
    /// `Err(_)` if the handler attempted the copy and the kernel refused.
    ///
    /// On `Err`, the caller is responsible for cleaning up any partial
    /// destination before invoking the next handler.
    fn try_copy(&self, ctx: &CopyCtx<'_>) -> io::Result<Option<u64>>;

    /// Stable identifier used for memoisation and for the `CopyMethod` tag.
    fn method(&self) -> CopyMethod;
}

pub(crate) struct CopyCtx<'a> {
    pub src: &'a Path,
    pub dst: &'a Path,
    pub size_hint: u64,
}
```

The chain is a `&'static [&'static dyn CopyHandler]` selected per-platform at
compile time, walked in order:

```rust
pub(super) fn run_chain(ctx: &CopyCtx<'_>) -> io::Result<CopyResult> {
    for handler in chain_for_platform() {
        match handler.try_copy(ctx) {
            Ok(Some(bytes)) => return Ok(CopyResult::new(bytes, handler.method())),
            Ok(None) => continue,                        // not applicable
            Err(_) => {
                let _ = std::fs::remove_file(ctx.dst);    // cleanup, then fall through
                continue;
            }
        }
    }
    unreachable!("chain must end with StdCopyHandler which never returns None or Err for valid paths")
}
```

The semantic split between `Ok(None)` ("skip me, I do not apply") and `Err`
("I tried and failed, clean up") is the key piece - it lets the chain encode
threshold gating and platform gating (`ReFsHandler` returns `Ok(None)` when
the destination is on NTFS) without conflating them with kernel errors. It
also fixes a subtle issue in the current code: today, a `FICLONE` failure on
ext4 produces a partial empty destination; the handler returns `Err`, the
chain cleans up, and the caller proceeds. The current code already does this
manually but inconsistently (`platform_copy_impl` does, `copy_file_contents`
does not).

### Per-platform chain composition

```rust
#[cfg(target_os = "linux")]
fn chain_for_platform() -> &'static [&'static dyn CopyHandler] {
    &[&FicloneHandler, &IoUringHandler, &CopyFileRangeHandler, &StdCopyHandler]
}

#[cfg(target_os = "macos")]
fn chain_for_platform() -> &'static [&'static dyn CopyHandler] {
    &[&ClonefileHandler, &FcopyfileHandler, &StdCopyHandler]
}

#[cfg(target_os = "windows")]
fn chain_for_platform() -> &'static [&'static dyn CopyHandler] {
    &[&RefsReflinkHandler, &CopyFileExHandler, &StdCopyHandler]
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn chain_for_platform() -> &'static [&'static dyn CopyHandler] {
    &[&StdCopyHandler]
}
```

`NoCowPlatformCopy` becomes a chain consisting of `&[&StdCopyHandler]`. A
`--bench-only-ficlone` flag (hypothetical) becomes `&[&FicloneHandler,
&StdCopyHandler]`. No code duplication.

### Concrete chain (the canonical reference)

Per the task brief, the canonical chain is

```text
FicloneHandler -> CopyFileRangeHandler -> SendfileHandler -> StdCopyHandler
```

This is the Linux-shaped exemplar used in the trait doc-string and in the
property test that exercises chain ordering. macOS and Windows diverge in
which handlers are present and in what order, but the contract (try-CoW-first,
try-zero-copy-second, fall-back-to-portable-last) is identical. `Sendfile`
appears in the canonical chain only as the file-to-pipe primitive that the
disk-commit path may eventually wire up; for whole-file copy it is currently
not in the live chain because the destination is a regular file, not a socket.
The doc string makes the distinction explicit.

### Threshold gating moves onto the handler

```rust
struct CopyFileRangeHandler;

impl CopyHandler for CopyFileRangeHandler {
    fn try_copy(&self, ctx: &CopyCtx<'_>) -> io::Result<Option<u64>> {
        if ctx.size_hint < 64 * 1024 { return Ok(None); }
        let src = File::open(ctx.src)?;
        let dst = File::create(ctx.dst)?;
        match crate::copy_file_range::raw_copy_file_range(&src, &dst, ctx.size_hint) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(e) => Err(e),
        }
    }

    fn method(&self) -> CopyMethod { CopyMethod::CopyFileRange }
}
```

The 64 KiB / 256 KiB / 4 MiB constants live next to the handler that uses
them; each handler is independently re-tunable. The `copy_file_contents`
function in `copy_file_range.rs` becomes a thin wrapper that constructs
`CopyCtx` and walks the chain - the duplicate inner chain inside that file is
removed.

## 4. Memoisation strategy for the successful handler

A single bulk transfer (recursive directory copy, daemon module pull, etc.)
typically involves N files on the same source filesystem and the same
destination filesystem. The platform-handler outcome for the first file is
overwhelmingly the outcome for files 2..N. We exploit this with a three-tier
cache.

### Tier 1: per-call cache (the cheap one)

`run_chain` does not memoise on its own; the wrapper `PlatformCopy` impl does:

```rust
pub struct DefaultPlatformCopy {
    last_winner: AtomicUsize,    // index into chain_for_platform(), or usize::MAX
}
```

Before iterating the chain, `copy_file` looks at `last_winner`. If it is a
valid index, that handler is tried first; on `Ok(Some(_))` we keep the index;
on `Ok(None)` or `Err` we fall back to the linear chain walk and update
`last_winner` with the first handler that returns `Ok(Some(_))`.

Two writes are tolerated under contention - the cache is advisory, never
required for correctness. `AtomicUsize` with `Relaxed` ordering is sufficient
because the only invariant is "this index is in-bounds for the static chain
slice", and the slice is `'static`.

For the FICLONE-then-fail-on-ext4 cold path described in section 2, the second
file pays one atomic load (free), one in-bounds branch, calls
`StdCopyHandler::try_copy` directly, returns `Ok(Some(_))`, and stores the
index. Files 3..N pay only the atomic load + branch.

### Tier 2: filesystem-keyed sticky cache (optional, gated)

For workloads that interleave multiple destination filesystems (a daemon
serving modules from `/mnt/btrfs/...` and `/mnt/ext4/...` to the same
process), a single per-process winner is wrong for half the files. The
upgrade is keyed by the destination's `(st_dev, st_ino_of_parent)`:

```rust
struct StickyCache {
    map: parking_lot::Mutex<lru::LruCache<(u64, u64), usize>>,
}
```

Capacity 32 covers any realistic mount tree; `LruCache` evicts cold entries.
The `(dev, parent_ino)` key is cheap (`statx` on the destination's parent
directory) and stable across renames within the same directory. This tier is
gated behind a feature or runtime flag and is not in the initial change.

### Tier 3: handler self-memoisation (already present, unchanged)

`refs_detect::is_refs_filesystem` already caches its answer per-volume via
`OnceLock` inside the `refs_detect` module. The kernel-version probe used by
`io_uring` is similarly cached. These remain. The chain refactor does not
change handler-internal caches.

### What we deliberately do not memoise

- The `size_hint` threshold check. It is one integer compare; caching it is
  net-negative.
- Handler applicability by platform. Already done at compile time via the
  `chain_for_platform()` const slice.
- The `CopyMethod` returned from `try_copy`. Always derived from the handler
  type - no dynamic dispatch on success.

### Cache invalidation

The Tier 1 cache is invalidated implicitly: any handler that returns
`Ok(None)` or `Err` while cached as the winner causes the wrapper to fall
through to the linear chain walk and re-elect a winner. This handles the
"destination filesystem changed mid-process" case (rare, but legal: a new
mount on top of an existing path).

## 5. Migration plan

1. Introduce `crates/fast_io/src/platform_copy/handler.rs` with the trait and
   the eight concrete handlers. Each handler is a unit struct that calls into
   the existing private `*_impl` functions in `dispatch.rs`. No behaviour
   change yet.
2. Replace the body of `dispatch::platform_copy_impl` with `run_chain`. Delete
   the per-platform monolithic functions.
3. Add the Tier 1 `AtomicUsize` cache in `DefaultPlatformCopy`. Behaviour
   change: the second file in a recursive copy skips the failed-FICLONE probe.
4. Move threshold constants from `dispatch.rs` and `copy_file_range.rs` onto
   the handlers. Delete the duplicate inner chain in
   `copy_file_contents`; have it call `run_chain` directly.
5. Add unit tests per handler against a fixture filesystem (tempfile dir +
   small file + large file). Add a chain-ordering property test that asserts
   each handler is invoked in the documented order until one returns
   `Ok(Some(_))`.
6. Re-run the existing `crates/fast_io/src/platform_copy/tests.rs` suite
   unchanged - the public `PlatformCopy` API is unchanged, so the tests pass
   without edits.

The change is invisible to consumers of `fast_io` (the `engine` and
`transfer` crates). No public API moves; no `Cargo.toml` changes; no feature
flag changes.

## 6. Out of scope

- Sendfile and splice retain their existing single-tier-plus-fallback shape.
  Their direction (file <-> socket) makes them a different state machine; the
  Chain-of-Responsibility refactor is for the file -> file dispatch only. A
  later note can apply the same trait to the file <-> socket pipeline if the
  number of primitives there grows past two.
- The `iocp` and `io_uring` submission-mode logic inside their respective
  modules. Those are batched-async pipelines, not single-call zero-copy
  primitives, and the trait shape does not match.
- Wire-protocol behaviour. None of the handlers participate in the rsync wire
  protocol; they are below the `engine` crate's delta executor.

## 7. References

- Existing dispatch: `crates/fast_io/src/platform_copy/dispatch.rs`
- Existing types: `crates/fast_io/src/platform_copy/types.rs::PlatformCopy`
- Inner size-tier chain: `crates/fast_io/src/copy_file_range.rs::copy_file_contents`
- ReFS filesystem probe: `crates/fast_io/src/refs_detect.rs`
- `NoCowPlatformCopy` example of an alternate chain:
  `crates/fast_io/src/platform_copy/mod.rs`
