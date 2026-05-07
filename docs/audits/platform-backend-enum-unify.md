# Platform Backend / Strategy Enum Unification Audit

Tracking issue: oc-rsync task #2117. No code changes - audit only.

Related: `docs/audits/init-time-backend-selection.md` (#2116) covers per-call
dispatch cost for the same enums; this audit covers the orthogonal question of
whether the enum *types themselves* should be unified.

## 1. Scope

The codebase has acquired a family of enums that name "the backend / strategy
that performs an I/O or copy operation". The task asks whether these can be
collapsed into a single canonical `PlatformBackend` type. This audit:

1. Enumerates every enum whose name ends in `Backend` or `Strategy` (plus the
   four `*Policy` enums in `fast_io` that fill the same role).
2. Records the variant set, definition site, and top consumer sites for each.
3. Marks duplicates / overlaps and the apparent intent of the existing split.
4. Proposes a canonical owner (or rejects unification) per cluster.
5. Quantifies blast radius if unification proceeds.

Source files inspected (all paths repository-relative):

- `crates/fast_io/src/platform_copy/types.rs` (`CopyMethod`, `PlatformCopy`).
- `crates/fast_io/src/platform_copy/{mod,no_zero_copy}.rs`
  (`DefaultPlatformCopy`, `NoCowPlatformCopy`, `NoZeroCopyPlatformCopy`).
- `crates/fast_io/src/lib.rs:477,514,542,587`
  (`IoUringPolicy`, `CowPolicy`, `IocpPolicy`, `ZeroCopyPolicy`).
- `crates/fast_io/src/temp_file_strategy.rs` (`TempFileKind`, `TempFileStrategy`).
- `crates/fast_io/src/o_tmpfile/types.rs` (`OTmpfileSupport`, `TempFileResult`).
- `crates/fast_io/src/iocp/file_factory.rs` (`IocpOrStdReader`,
  `IocpOrStdWriter`).
- `crates/fast_io/src/io_uring/file_factory.rs` (`IoUringOrStdReader`,
  `IoUringOrStdWriter`).
- `crates/fast_io/src/io_uring/socket_factory.rs`
  (`IoUringOrStdSocketReader`, `IoUringOrStdSocketWriter`).
- `crates/fast_io/src/mmap_reader.rs` (`AdaptiveReader`).
- `crates/transfer/src/map_file/adaptive.rs` (`AdaptiveMapStrategy`).
- `crates/engine/src/local_copy/deferred_sync.rs` (`SyncStrategy`).
- `crates/engine/src/local_copy/executor/file/copy/transfer/write_strategy.rs`
  (`WriteStrategy`).
- `crates/engine/src/local_copy/executor/file/sparse/mod.rs`
  (`SparseDetectStrategy`).
- `crates/engine/src/local_copy/executor/file/guard.rs` (`GuardStrategy`).
- `crates/checksums/src/simd_batch/md5_dispatcher.rs` (`Backend`).
- `crates/checksums/src/strong/{md4,md5}.rs` (`Md4Backend`, `Md5Backend`).

## 2. TL;DR

There is **no single `PlatformBackend` enum** to collapse to. The candidates
the issue mentioned (`IoBackend`, `PlatformCopy`, `IoStrategy`, `WriteStrategy`)
do not exist as a literal name set; they refer to a *family* of types with
overlapping responsibilities but disjoint variant axes. After enumerating the
candidates, the family decomposes into **four orthogonal concern clusters**,
each of which is internally consistent and externally non-substitutable:

| Cluster | Concern | Canonical type today | Variant axis |
|---------|---------|----------------------|--------------|
| **A** Platform copy | which kernel API performs a whole-file copy | `fast_io::CopyMethod` (output-only) + `PlatformCopy` trait (interface) | per-syscall identity |
| **B** Subsystem policy | user opt-in/opt-out for each kernel feature | `fast_io::{IoUringPolicy,CowPolicy,IocpPolicy,ZeroCopyPolicy}` | `Auto / Enabled / Disabled` |
| **C** Reader/writer wrapper | runtime-selected I/O dispatch enum | `fast_io::{IoUringOrStd*,IocpOrStd*}`, `mmap_reader::AdaptiveReader`, `transfer::map_file::AdaptiveMapStrategy` | `<fast-path-impl> / <std-fallback>` |
| **D** Local-copy commit strategy | how the engine decides where bytes go on disk | `engine::WriteStrategy`, `engine::SyncStrategy`, `engine::SparseDetectStrategy`, `engine::GuardStrategy`, `fast_io::TempFileKind` | engine-state machine, not platform-driven |

The checksum `*Backend` enums (`md5_dispatcher::Backend`, `Md4Backend`,
`Md5Backend`) are a fifth cluster that is unrelated to platform I/O - they
identify a SIMD lane width or a hash crate selection and must not be folded
into a platform-backend type.

**Recommendation:** Reject a single `PlatformBackend` super-enum. The clusters
satisfy single-responsibility today; collapsing them would couple unrelated
axes (`Disabled` does not make sense for a `WriteStrategy` variant; `Append`
does not make sense for an `IoUringPolicy`). Instead, perform two narrow
clean-ups under the same task:

1. **Cluster B normalisation** - factor the four subsystem policy enums into a
   single generic `BackendPolicy` (`Auto / Enabled / Disabled`) and an
   associated subsystem tag, so that CLI plumbing and `Display` impls share
   code. This is the only cluster with literal duplication.
2. **Cluster C consolidation** - the eight reader/writer wrappers
   (`IoUringOrStdReader`, `IoUringOrStdWriter`, `IocpOrStdReader`,
   `IocpOrStdWriter`, `IoUringOrStdSocketReader`, `IoUringOrStdSocketWriter`,
   `mmap_reader::AdaptiveReader`, `transfer::map_file::AdaptiveMapStrategy`)
   share the same shape (`Fast(impl) | Std(impl)`). `init-time-backend-selection`
   already recommends replacing them with trait objects stored once at
   construction; that audit is the right place for this work.

Clusters A and D should remain as-is.

## 3. Cluster A - Platform copy (single concern, single owner)

### 3.1 Definition

- `crates/fast_io/src/platform_copy/types.rs:12` - `pub enum CopyMethod`.
  Variants: `Ficlone`, `CopyFileRange`, `Clonefile`, `Copyfile`, `ReFsReflink`,
  `CopyFileEx`, `StandardCopy`. Used as the `CopyResult.method` tag returned
  *by* a copy call, never as an input.
- `crates/fast_io/src/platform_copy/types.rs:122` - `pub trait PlatformCopy`
  (interface, not enum).
- `crates/fast_io/src/platform_copy/mod.rs:71` - `pub struct DefaultPlatformCopy`
  auto-selects the platform default.
- `crates/fast_io/src/platform_copy/mod.rs:106` - `pub struct NoCowPlatformCopy`
  forces portable buffered copy.
- `crates/fast_io/src/platform_copy/no_zero_copy.rs:29` - `pub struct
  NoZeroCopyPlatformCopy` strips kernel zero-copy syscalls.

### 3.2 Top consumers

- `crates/engine/src/local_copy/options/types.rs:231` - the executor stores
  `platform_copy: Arc<dyn PlatformCopy>`.
- `crates/engine/src/local_copy/clonefile.rs:67,79` - `clone_or_copy_with`
  takes `&dyn PlatformCopy`.
- `crates/engine/src/local_copy/win_copy.rs:120,132` - `copy_file_optimized_with`
  same shape.
- `crates/core/src/client/run/mod.rs:411,424,435` - swaps in
  `NoZeroCopyPlatformCopy` / `NoCowPlatformCopy` based on
  `ZeroCopyPolicy::Disabled` / `CowPolicy::Disabled`.

### 3.3 Verdict

Single-owner already. `CopyMethod` is *output telemetry* and `PlatformCopy` is
the *behavioural interface*; they cannot be collapsed because they live on
opposite ends of the call. Do not touch.

## 4. Cluster B - Subsystem policy (literal duplication)

### 4.1 Definition (shape duplicated four times)

- `crates/fast_io/src/lib.rs:477` - `pub enum IoUringPolicy { Auto, Enabled, Disabled }`.
- `crates/fast_io/src/lib.rs:514` - `pub enum CowPolicy { Auto, Disabled }`
  (no `Enabled` because reflink is best-effort).
- `crates/fast_io/src/lib.rs:542` - `pub enum IocpPolicy { Auto, Enabled, Disabled }`.
- `crates/fast_io/src/lib.rs:587` - `pub enum ZeroCopyPolicy { Auto, Enabled, Disabled }`.

The three three-variant enums are byte-for-byte identical. `CowPolicy` is the
only outlier (no `Enabled` arm).

### 4.2 Top consumers

- `crates/core/src/client/config/builder/mod.rs:190,192,193` - all four held
  side by side on `ClientConfigBuilder`.
- `crates/core/src/client/config/client/mod.rs:155-159,316-319` -
  `ClientConfig` mirrors them and constructs the `Auto` defaults.
- `crates/core/src/client/run/mod.rs:423,434` - dispatches off
  `ZeroCopyPolicy::Disabled` and `CowPolicy::Disabled`.
- `crates/transfer/src/disk_commit/config.rs:80,96` -
  `io_uring_policy` and `iocp_policy` both stored on `DiskCommitConfig`.
- `crates/transfer/src/disk_commit/thread.rs:75,83-85,100-103,114-151` - per-policy
  `match` ladders for io_uring and IOCP. The two ladders are structurally
  identical (Disabled / Auto / Enabled), differing only in the called helper.

### 4.3 Verdict

Genuine duplication. Four definitions, four matching `Default`/`Display`
impls, identical CLI flag plumbing in `cli` -> `core` -> `transfer`. The
canonical fix is one generic enum:

```rust
// in fast_io
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BackendPolicy {
    #[default]
    Auto,
    Enabled,
    Disabled,
}
```

Each subsystem keeps a *type alias* (`pub type IoUringPolicy = BackendPolicy;`
etc.) for backward source compatibility, then deprecates the aliases in a
follow-up. `CowPolicy` retains a separate two-variant enum or a
`BackendPolicy::Enabled` semantics that the cow path treats as `Auto`.

**Migration path** (if pursued):

1. Add `pub enum BackendPolicy` to `fast_io::lib`.
2. Replace each of the four enum definitions with `pub type X = BackendPolicy;`.
3. Audit each consumer's `match` arms - they remain unchanged because the
   variants are named identically (`Auto`, `Enabled`, `Disabled`).
4. The `CowPolicy::Disabled` consumer site is the only divergent path; gate
   it behind a documented "Auto means best-effort, no Enabled forcing".

**Files touched (estimate):** 4 enum definitions + ~12 consumer match sites
(`crates/core/src/client/{config,run}/...` and
`crates/transfer/src/disk_commit/{config,thread}.rs`).

This is the only cluster that justifies the issue's framing.

## 5. Cluster C - Reader/writer dispatch wrappers (`Fast | Std` shape)

### 5.1 Definition

All eight types have the same two-arm shape: a fast-path implementation plus a
`Std` fallback the factory falls back to when the fast path is unavailable at
runtime.

- `crates/fast_io/src/io_uring/file_factory.rs:57` - `pub enum
  IoUringOrStdReader { IoUring(IoUringReader), Std(StdFileReader) }`.
- `crates/fast_io/src/io_uring/file_factory.rs:172` - `IoUringOrStdWriter`.
- `crates/fast_io/src/io_uring/socket_factory.rs:12,30` -
  `IoUringOrStdSocketReader`, `IoUringOrStdSocketWriter`.
- `crates/fast_io/src/iocp/file_factory.rs:24` - `pub enum IocpOrStdReader
  { Iocp(IocpReader), Std(StdFileReader) }`.
- `crates/fast_io/src/iocp/file_factory.rs:80` - `IocpOrStdWriter`.
- `crates/fast_io/src/mmap_reader.rs:215` - `pub enum AdaptiveReader { Mmap, Buffered }`.
- `crates/transfer/src/map_file/adaptive.rs:21` - `pub enum AdaptiveMapStrategy
  { Buffered(BufferedMap), Mmap(MmapStrategy) }`.

Every variant trampoline `impl Read for X { fn read(&mut self, buf) =
{ Self::Fast(r) => r.read(buf), Self::Std(r) => r.read(buf) } }` is
boilerplate.

### 5.2 Top consumers

- `crates/transfer/src/disk_commit/thread.rs` opens an `IoUringOrStdWriter`
  per file.
- `crates/transfer/src/delta_apply/applicator.rs:17,26,112-144` - holds an
  `AdaptiveMapStrategy` for the basis file.
- `crates/transfer/src/map_file/wrapper.rs:13,62,91` - `MapFile`
  parameterised over `AdaptiveMapStrategy`.

### 5.3 Verdict

Defer to `init-time-backend-selection.md` (#2116) class B / row 4-6, which
already recommends storing a `Box<dyn FileReader>` / `Box<dyn FileWriter>`
once at construction instead of an enum matched on every call. That work is
the right venue. Do not collapse into a single super-enum; the trait already
exists (`FileReader`, `FileWriter`, `MapStrategy`) and the eight wrappers
should become `Box<dyn Trait>` rather than become one huge enum that knows
about io_uring + IOCP + mmap + std all at once.

## 6. Cluster D - Local-copy commit strategy (engine state, not platform)

These describe **what the engine decides to do**, not **what the platform
provides**. They cannot be unified with cluster A/B/C.

- `crates/engine/src/local_copy/executor/file/copy/transfer/write_strategy.rs:24`
  - `pub(in crate::local_copy) enum WriteStrategy { Append, Inplace, Direct,
  TempFileRename, AnonymousTempFile }`. Selected by
  `select_write_strategy(append_offset, inplace, partial, delay, existing,
  temp_dir, dest)`. Mirrors upstream `receiver.c` selection.
  - Consumers: `crates/engine/src/local_copy/executor/file/copy/transfer/execute.rs`,
    `crates/engine/src/local_copy/tests/execute_direct_write.rs`.
- `crates/engine/src/local_copy/deferred_sync.rs:36` - `pub enum SyncStrategy
  { Immediate, Batched(usize), DirectoryLevel, Deferred, None }`. Defaults
  to `Deferred`. Consumers: `local_copy/{mod,context,context_impl/state}.rs`.
- `crates/engine/src/local_copy/executor/file/sparse/mod.rs:46` - `pub enum
  SparseDetectStrategy { Auto, Seek, Map, None }`. CLI-driven via
  `--sparse-detect`. Consumers: `cli::frontend::arguments::parser`,
  `core::client::config::{builder,client}`, `engine::local_copy::options`.
- `crates/engine/src/local_copy/executor/file/guard.rs:61` - `enum
  GuardStrategy { NamedTempFile{..}, Anonymous{..} }`. Internal to the
  destination write guard.
- `crates/fast_io/src/temp_file_strategy.rs:40` - `pub enum TempFileKind {
  Anonymous{..}, Named{..} }` (mirrors `GuardStrategy` from a different
  level of abstraction).

### 6.1 Verdict

These are five distinct decisions with disjoint variant axes:

- *Where in the file does the writer start?* (`WriteStrategy`).
- *When do we fsync?* (`SyncStrategy`).
- *How do we detect holes in the source?* (`SparseDetectStrategy`).
- *How does the temp guard finalise?* (`GuardStrategy`, `TempFileKind`).

A super-enum that tried to express all four would be a Cartesian product. The
only legitimate consolidation is between `GuardStrategy` (engine-internal)
and `TempFileKind` (fast_io public) - they describe the same finalisation
choice from two layers, and `GuardStrategy` could be replaced with
`fast_io::TempFileKind` directly (the engine's `Anonymous` variant is already
`#[cfg(target_os = "linux")]`-gated to match `TempFileKind`'s gating). That
is a one-file refactor with no API surface change; track it separately if
desired.

## 7. Cluster E - Checksum backend (out of scope)

For completeness:

- `crates/checksums/src/simd_batch/md5_dispatcher.rs:28` - `pub enum Backend
  { Avx512, Avx2, Sse41, Ssse3, Sse2, Neon, Wasm, Scalar }`. SIMD lane width.
- `crates/checksums/src/strong/md5.rs:219` - `enum Md5Backend { OpenSsl(..),
  Rust(..) }`. Hash crate selection.
- `crates/checksums/src/strong/md4.rs:49` - `enum Md4Backend { OpenSsl(..),
  Rust(..) }`. Same shape as `Md5Backend`.

These are not platform-I/O backends; they are crypto-library or SIMD-tier
selectors. Unifying with the platform clusters would be a category error.
The `Md4Backend` / `Md5Backend` pair is structurally identical and could be
made generic over the hasher type if the duplication ever grows beyond two
variants, but that is an unrelated refactor.

## 8. Risk and blast radius

If the recommendation in section 4 (Cluster B normalisation) is taken:

- **Files modified:** 1 (`fast_io/src/lib.rs` adds `BackendPolicy`, replaces
  four enum decls with `type` aliases).
- **Files re-compiled:** every crate that names any of the four policies -
  `core`, `cli`, `transfer`, `engine`, plus their tests. Approximately 50
  files transitively, but no source edits in any of them because the variant
  names (`Auto`, `Enabled`, `Disabled`) are unchanged.
- **Wire / behavioural risk:** zero. The enums never cross the wire and the
  variants are not user-visible strings (CLI flags are `--io-uring`,
  `--no-io-uring` etc., not `=Auto`).
- **Reverse compatibility:** `pub use IoUringPolicy = BackendPolicy;` keeps
  every external name resolvable.
- **Breaking change cost:** none if `pub type` aliases are kept indefinitely;
  one minor-version deprecation cycle if the aliases are eventually removed.
- **Documentation:** four rustdoc comment blocks collapse to one canonical
  block + per-subsystem one-liners.

If the recommendation in section 5 (Cluster C trait-object replacement) is
taken (covered by #2116):

- **Files modified:** 8 enum definitions + every transfer/engine consumer
  that pattern-matches on `IoUring(_) | Std(_)` (estimated 15-20 sites).
- **Wire / behavioural risk:** zero.
- **Performance risk:** virtual call overhead per `read`/`write`. The
  dispatcher already pays an enum-tag branch per call; a `vtable` indirect
  call replaces that with one indirect branch and is typically equivalent on
  modern OoO CPUs. Benchmark before merging.

If the broad super-enum (rejected) were taken:

- **Files modified:** every enum site (15+ definitions) plus every consumer
  (~80 files).
- **Wire / behavioural risk:** zero.
- **Maintenance risk:** high - a single enum with `Append | Inplace | Auto |
  Enabled | Disabled | Ficlone | Mmap | ...` violates SRP, defeats exhaustive
  matching as a correctness tool, and re-introduces the very ambiguity the
  current decomposition prevents.

## 9. Recommendation

1. **Reject** unification into a single `PlatformBackend` enum. The clusters
   are intentionally separate.
2. **Adopt** Cluster B normalisation (`BackendPolicy` generic + four type
   aliases) as a small follow-up to this audit.
3. **Defer** Cluster C consolidation to the existing
   `init-time-backend-selection.md` (#2116) workstream.
4. **Defer** the `GuardStrategy` / `TempFileKind` merge as a separate engine
   cleanup; it is a one-file refactor and not blocked by anything else.
5. **Leave** Clusters A, D, and E alone.
