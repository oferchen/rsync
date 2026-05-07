# PlatformBackend Enum Unification

Issue: #2117

## Current State

Platform/backend decomposition is currently spread across several enums and
traits, each shaped to a single concern:

- `PlatformCopy` trait + `CopyMethod` enum
  (`crates/fast_io/src/platform_copy/types.rs`) - covers copy-file fast paths
  (`Ficlone`, `CopyFileRange`, `Clonefile`, `Copyfile`, `ReFsReflink`,
  `CopyFileEx`, `StandardCopy`). Tracked under #1822 / #1009.
- `IoUringPolicy`, `CowPolicy`, `IocpPolicy`, `ZeroCopyPolicy`
  (`crates/fast_io/src/lib.rs`) - per-feature `Auto`/`Enabled`/`Disabled`
  toggles. Tracked under the original #1821 IoBackend rollout.
- `WriteStrategy`
  (`crates/engine/src/local_copy/executor/file/copy/transfer/write_strategy.rs`)
  - receiver-side write path selection (`Append`, `Inplace`, `Direct`,
  `TempFileRename`, `AnonymousTempFile`). Tracked under #1765 (alongside the
  proposed `IoStrategy`).

There is no single enum or trait that answers "for the current host, which
fast-path family is in use?"; callers stitch the answer together from
`DefaultPlatformCopy`, the policy enums, and per-call write-strategy logic.

## Risk

Each enum encodes a slightly different platform decomposition:

- `CopyMethod` is per-syscall (`Ficlone` vs `CopyFileRange` are both Linux).
- `WriteStrategy` is per-receiver-mode and ignores OS entirely.
- The four `*Policy` enums are per-feature toggles, with platform implied by
  the feature name (`IoUring` = Linux, `Iocp` = Windows).

Drift between these views makes "what fast path actually runs?" hard to reason
about, raises the cost of adding a new platform (e.g. FreeBSD `copy_file_range`
or illumos `reflink`), and forces every new caller to learn three vocabularies.

## Unification Plan

Introduce a single platform-rooted enum exposed from `fast_io`:

```rust
pub enum PlatformBackend {
    Linux(LinuxOps),
    Macos(MacosOps),
    Windows(WindowsOps),
    Std,
}
```

Each variant carries a per-OS struct that holds one trait object per operation
family, rather than one mega-enum of every (OS, op) pair:

- `trait CopyOps` - `copy_file`, `supports_reflink`, `preferred_method`
  (subsumes `PlatformCopy`).
- `trait WriteOps` - destination open/commit (subsumes `WriteStrategy`).
- `trait ZeroCopyOps` - `sendfile`/`splice`/`TransmitFile` selection.
- `trait BulkIoOps` - submission/completion model (`io_uring`, IOCP, kqueue,
  blocking std).

`PlatformBackend::current()` returns the runtime selection; the per-OS struct
is built once and cached. Policy enums (`IoUringPolicy` etc.) become inputs to
the constructor, not parallel runtime state.

## Migration

1. Land `PlatformBackend` and the four op traits inside `fast_io`, behind the
   existing feature gates. Wire `DefaultPlatformCopy` and the `*Policy` enums
   to delegate to the new types - no caller changes.
2. Re-export the trait objects from the same paths the current
   `PlatformCopy`/`CopyMethod` types use, so engine and core build unchanged.
3. Migrate engine call sites
   (`crates/engine/src/local_copy/{clonefile,win_copy,executor}`) and the
   builder/options layer to `PlatformBackend` op traits one family at a time
   (copy, write, zero-copy, bulk-io).
4. Mark `PlatformCopy`, `CopyMethod`, `WriteStrategy`, and the standalone
   policy enums `#[deprecated(note = "use PlatformBackend ops")]`.
5. Remove the deprecated items two minor releases after the migration commits
   land (so external embedders see one release with both APIs available).

## Risks

- **Feature gates.** `PlatformBackend` variants must compile on every host.
  Each per-OS struct ships behind its `#[cfg(target_os = ...)]` and a `Std`
  fallback always exists; cross-platform tests must exercise the `Std` arm.
- **Enum size growth.** Carrying four trait objects per variant inflates the
  enum. Mitigation: each `*Ops` struct holds `Arc<dyn Trait>` (already the
  shape of `PlatformCopy` consumers in the engine builder), keeping the
  variant pointer-sized.
- **Trait-object dispatch cost.** Hot paths (per-block copy, per-write commit)
  must not regress. Mitigation: keep `CopyMethod` (or its successor) as a
  `Copy` enum returned from `preferred_method`, so dispatch happens once per
  file, not per block. Benchmark the `local_copy` and `transfer` paths before
  removing the deprecated APIs.
