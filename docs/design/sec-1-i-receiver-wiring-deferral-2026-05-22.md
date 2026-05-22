# SEC-1.i - Receiver wiring follow-up (deferred)

Date: 2026-05-22
Scope: closure note for wiring `fast_io::fchmodat_via_sandbox_or_fallback`,
`fchownat_via_sandbox_or_fallback`, and `utimensat_via_sandbox_or_fallback`
through the receiver-side metadata application pipeline.
Status: deferred pending the SEC-1.b carrier refactor that lifts `DirSandbox`
out of `fast_io` into a dependency the `metadata` crate can consume without
forming a cycle.
Predecessor: SEC-1.i (PR #4690, merged) shipped the three helpers and the
single-component-leaf detector. Receiver wiring was explicitly listed as
"deferred to follow-up" in that PR's summary.
Tracker: SEC-1 umbrella (see `docs/security/sec-1-status.md`); the wiring
sub-task stays open with the "deferred pending carrier" label.

## 1. SEC-1.i recap

PR #4690 added three sandbox-aware helpers in
`crates/fast_io/src/dir_sandbox/at_syscalls_metadata.rs`:

| Helper | Sandbox path | Fallback path |
|--------|--------------|---------------|
| `fchmodat_via_sandbox_or_fallback` | `fchmodat(dirfd, leaf, mode, flags)` | `std::fs::set_permissions` |
| `fchownat_via_sandbox_or_fallback` | `fchownat(dirfd, leaf, uid, gid, flags)` | `libc::chown` / `libc::lchown` (preserves `AT_SYMLINK_NOFOLLOW` semantics) |
| `utimensat_via_sandbox_or_fallback` | `utimensat(dirfd, leaf, atime, mtime, flags)` | `filetime::set_file_times` / `set_symlink_file_times` |

The sandbox path is taken when:
1. A `DirSandbox` is passed in (`Some(sandbox)`), and
2. `single_component_leaf(dest_dir, relative_path, link_path)` returns `Some`,
   i.e. `link_path == dest_dir.join(relative_path)` and `relative_path` is a
   single normal component.

Both conditions match the prior-art pattern used by SEC-1.h's `mkdirat` /
`symlinkat` / `unlinkat` / `linkat` wiring at
`crates/transfer/src/receiver/directory/links.rs` and
`crates/transfer/src/receiver/directory/creation.rs`.

PR #4690 wired zero receiver call sites and explicitly stated that wiring
would follow once the carrier blocker (below) was resolved. This document
discharges that follow-up by formalising the deferral with the carrier
plumbing required for each candidate site.

## 2. Candidate sites and per-site blockers

The receiver-side metadata application surface funnels through
`metadata::apply_metadata_from_file_entry` (and its variants
`apply_metadata_with_cached_stat`, `apply_metadata_with_attrs_flags`,
`apply_file_metadata_with_options`, `apply_file_metadata_with_fd_if_changed`)
defined at `crates/metadata/src/apply/mod.rs:218-289`. Every receiver call
site goes through this funnel:

| Call site | File:line | Sandbox reachable in scope? | Blocker |
|-----------|-----------|-----------------------------|---------|
| Reference-dest link path | `crates/transfer/src/receiver/quick_check.rs:180` | No (`try_reference_dest` does not take sandbox) | Carrier (signature change up the call chain plus metadata-crate funnel) |
| Reference-dest copy path | `crates/transfer/src/receiver/quick_check.rs:203` | No (same scope) | Same |
| Up-to-date quick-check apply | `crates/transfer/src/receiver/transfer/candidates.rs:175` | Indirect (`map_blocking` closure; sandbox not currently captured) | Carrier (capture + metadata-crate funnel) |
| Post-rename metadata apply | `crates/transfer/src/receiver/transfer/sync.rs:367` | Yes (`sandbox.as_deref()` reachable just above at line 349) | Metadata-crate funnel |
| Batch directory metadata apply | `crates/transfer/src/receiver/directory/creation.rs:191` | Yes (`sandbox: Option<&DirSandbox>` arg + `map_blocking` closure) | Metadata-crate funnel + closure capture lifetime |
| Incremental directory metadata apply | `crates/transfer/src/receiver/directory/creation.rs:388` | Yes (`sandbox: Option<&DirSandbox>` arg) | Metadata-crate funnel |
| Post-commit disk thread | `crates/transfer/src/disk_commit/process.rs:423` | No (runs on `spawn_disk_thread`'s worker; `DiskCommitConfig` has no sandbox field) | Carrier + `Send + Sync` arc threading + metadata-crate funnel |

In every case, the path from the receiver call site to the actual `chmod` /
`chown` / `utimensat` syscall goes through one of the public `metadata::apply_*`
functions, which in turn dispatch to the per-concern modules at
`crates/metadata/src/apply/permissions.rs`, `:ownership.rs`, and
`:timestamps.rs`. Those modules currently call `std::fs::set_permissions`,
`rustix::fs::chownat(CWD, ...)`, and `filetime::set_file_times` directly with
no carrier parameter and no fast-path branch.

## 3. The carrier blocker

`metadata` has no dependency on `fast_io` today (verified against
`crates/metadata/Cargo.toml`). The deps are limited to `filetime`,
`protocol`, `rustix`, `libc`, `thiserror`, plus the optional `xattr` and
`exacl` features and the Windows/Apple ACL crates. `fast_io` is the sole
home of `DirSandbox` and the three SEC-1.i helpers, and it transitively
ships `dashmap`, `memmap2`, `io-uring`, and platform-specific glue that
the metadata crate has historically been kept clean of so it can stay
portable and minimally-deps-heavy.

Wiring the SEC-1.i helpers into the metadata crate requires one of:

1. **Add `fast_io` as a `[target.'cfg(unix)'.dependencies]` dep to
   `metadata`.** Simplest mechanically; pulls `dashmap` + `memmap2` +
   the io_uring stack into a crate that is currently held to a small dep
   surface. Risks circularity if `fast_io` ever needs metadata types
   (none today; not future-proof).
2. **Extract a `DirSandboxLike` trait** into a new leaf crate (or into
   `metadata` itself) that exposes only the methods SEC-1.i helpers need
   (`current_dirfd() -> BorrowedFd<'_>`). `fast_io::DirSandbox` would
   implement it. Keeps the dep graph acyclic but adds a new crate or a
   public trait surface in `metadata`. Aligns with the long-term
   direction of consolidating unsafe code into `fast_io` and exposing
   safe public APIs from it.
3. **Refactor `metadata::apply_*` to accept a callback** of the shape
   `Option<&dyn Fn(&Path, &Path, Mode, ...) -> io::Result<()>>` that
   transfer constructs around `fast_io::*_via_sandbox_or_fallback`.
   No dep change; surfaces the three syscall families as injectable
   strategies. Most invasive at the metadata-crate call-site level (every
   public `apply_*` function gains an optional carrier argument plus an
   `Option<&Path> dest_dir`, `&Path relative_path`) and bleeds the
   sandbox concept into a crate whose contract is "apply this metadata
   to this destination path".

All three are cross-crate API changes that touch the `metadata` public
surface and the four receiver-side call paths in the table above. None is
a drop-in change.

## 4. Why we defer the wiring (not just the implementation)

A wiring PR that lands one site at a time without first agreeing on the
carrier shape would either:

- Pick option 1 and pollute the metadata crate's dep surface for a single
  site's gain, or
- Pick option 2/3 and ship the trait/callback abstraction without the
  consumer that justifies its existence, then immediately revisit the
  shape on the next site.

The cost asymmetry is the same one that gated SEC-1.b (carrier-first) and
SEC-1.h (mknodat deferred): the carrier-design step needs to land as one
unit, after which the wiring is mechanical for every remaining `*at`
syscall family. Doing it the other way around bakes in shape decisions
that fight subsequent wiring.

SEC-1.j's receiver-wiring agent took the same call when it discovered the
`renameat` carrier touched a similar slice of cross-crate plumbing; it
wired 1 of 3 sites and explicitly deferred the other 2 on the same
"carrier plumbing is the dominant blocker" rationale. SEC-1.i wires zero
sites for the same reason: the only site where the sandbox is already in
scope (`crates/transfer/src/receiver/transfer/sync.rs:367`) still funnels
through `metadata::apply_metadata_from_file_entry`, which is the exact
chokepoint the carrier refactor must address.

## 5. What would change the call

The carrier-design task (call it SEC-1.b-2, follow-up to the existing
SEC-1.b doc at `docs/design/sec-1-b-dirfd-carrier.md`) picks one of the
three options in section 3 and ships:

1. The chosen abstraction (dep addition, trait, or callback).
2. A small carrier PR that converts the `metadata::apply_*` family to
   accept the carrier optionally, with the existing path-based fallback
   preserved for `None` and for multi-component relative paths.
3. A second wiring PR that threads the carrier through the four
   receiver-side sites where `DirSandbox` is already reachable
   (`creation.rs:191`, `creation.rs:388`, `sync.rs:367`, plus the
   `candidates.rs:175` closure capture).
4. A third wiring PR that extends the `DiskCommitConfig` to carry an
   `Arc<DirSandbox>` so the post-commit metadata apply at
   `disk_commit/process.rs:423` can route through the carrier. This
   touches the disk-thread spawn surface and is the largest of the
   wiring PRs.
5. A fourth wiring PR that plumbs the carrier through `try_reference_dest`
   so the reference-dest link/copy sites at `quick_check.rs:180` and
   `:203` pick up the fast path.

The mechanical work in steps 2-5 is small once the carrier shape is
settled. The decision gate is "which of options 1/2/3 in section 3 does
the carrier track pick?"; until that gate is met, individual wiring PRs
risk locking the abstraction shape via consumer-driven design.

## 6. Closure shape for the tracker

- SEC-1.i wiring sub-task: stays open. Label `deferred pending carrier
  refactor`. No assignee. Linked back to this doc, to the SEC-1.b carrier
  doc at `docs/design/sec-1-b-dirfd-carrier.md`, and to PR #4690 (the
  helpers that landed).
- The seven candidate sites in section 2 are catalogued here so the
  follow-up PRs do not need to re-discover them. Each row carries the
  exact file:line, the in-scope sandbox availability, and the per-site
  blocker beyond the shared carrier blocker.
- Project memory page tracking the SEC-1 path-syscall surface
  (`docs/audits/sec-1-a-path-syscall-surface-2026-05-20.md`) keeps the
  three metadata-family rows it already has and gains a reference to
  this closure doc.

## 7. Re-open trigger

Re-open SEC-1.i wiring when any one of the following is true:

- A carrier-design PR lands that picks one of options 1/2/3 in section 3
  and converts `metadata::apply_*` to optionally accept the carrier.
- A separate security finding raises the priority of converting one of
  the seven sites in section 2 to a sandbox-anchored syscall ahead of
  the others; in that case the wiring PR ships only that site under a
  surgical workaround (e.g. inline the three sandbox syscalls into the
  transfer site before delegating to the metadata crate for any
  remaining metadata work, accepting the syscall duplication for the
  affected site).
- A non-metadata receiver call site appears that calls `set_permissions`,
  `chown`, or `set_file_times` directly without going through the
  metadata crate; that site could be wired without touching the
  carrier blocker.

## 8. References

- PR #4690 - SEC-1.i helpers (`fchmodat` / `fchownat` / `utimensat`
  via-sandbox-or-fallback). Receiver callers wired: none.
- `crates/fast_io/src/dir_sandbox/at_syscalls_metadata.rs:230-336` -
  the three helpers, their signatures, and the
  `single_component_leaf` detector this doc references.
- `docs/design/sec-1-b-dirfd-carrier.md` - the carrier design doc that
  catalogues the broader 107-syscall plumbing problem and picks the
  hybrid stack + side cache shape SEC-1.c-j build on.
- `docs/audits/sec-1-a-path-syscall-surface-2026-05-20.md` - the
  audit that enumerates every path-based syscall site, including the
  ones SEC-1.i targets.
- `crates/metadata/src/apply/mod.rs:218-289` - the
  `apply_metadata_from_file_entry` funnel and its three siblings; the
  carrier refactor must address this surface.
- `crates/metadata/src/apply/permissions.rs`,
  `crates/metadata/src/apply/ownership.rs`,
  `crates/metadata/src/apply/timestamps.rs` - the per-concern modules
  that issue the syscalls SEC-1.i would route through the sandbox.
- `crates/transfer/src/receiver/transfer/sync.rs:367` - the only
  candidate site where `DirSandbox` is already in scope. Still requires
  the metadata-crate funnel rework before wiring.
- `crates/transfer/src/receiver/directory/links.rs:138`,
  `crates/transfer/src/receiver/directory/creation.rs:124` - the
  SEC-1.h prior-art that SEC-1.i wiring will mirror once the carrier
  funnel exists.
- SEC-1.j receiver-wiring PR (#4693) - the prior closure precedent that
  partial wiring + explicit per-site deferral is preferred over a
  sprawling cross-crate refactor.
