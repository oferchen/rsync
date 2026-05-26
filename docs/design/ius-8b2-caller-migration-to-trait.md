# IUS-8.b.2 - Migrate existing callers to `IoUringBackend` trait

Date: 2026-05-26
Scope: design specification for migrating all callers of concrete
io_uring types to use the `IoUringBackend` trait introduced in
IUS-8.a/IUS-8.b.1. This is the key migration step where callers become
generic over the backend trait, enabling IUS-8.c (stub tree deletion).
Status: **SPEC DRAFT** - no source changes in this PR.
Predecessor: IUS-8.a (trait surface), IUS-8.b.1 (Linux impl skeleton).
Depends on: IUS-8.b.1 merged (trait + `LinuxIoUringOpsBackend` exist).
Downstream: IUS-8.c (stub tree deletion), IUS-9 (free-function
deprecation).

---

## 0. Goal

Migrate every call site that directly uses concrete io_uring types
(`is_io_uring_available`, `IoUringConfig`, `IoUringDiskBatch`,
`IoUringOrStdWriter`, `IoUringReaderFactory`, `writer_from_file`, etc.)
to route through the `IoUringBackend` trait instead. After this
migration:

- Callers become generic over `<B: IoUringBackend>` or use trait-object
  storage.
- The stub tree (`io_uring_stub/`) becomes unreachable from external
  callers, unblocking its deletion in IUS-8.c.
- No behavioral change: every migrated path produces identical I/O on
  Linux and identical no-ops on non-Linux.

**Non-goals:** deleting the stub tree (IUS-8.c), deprecating the free
functions (IUS-9), adding new io_uring features.

## 1. Call-site inventory

### 1.1 Taxonomy

Call sites fall into three categories based on how they consume io_uring
types:

| Category | Description | Migration approach |
|----------|-------------|-------------------|
| **A - Policy/config** | Stores `IoUringPolicy`, `IoUringConfig`, or `IoUringDepthError` as config fields. These are plain-data types from `io_uring_common.rs`. | **No migration needed.** These types are already platform-free. |
| **B - Free-function dispatch** | Calls `fast_io::is_io_uring_available()`, `fast_io::writer_from_file()`, `fast_io::reader_from_path()`, etc. to obtain concrete `IoUringOrStd*` handles. | Migrate to `backend.open_reader()`, `backend.writer_from_file()`, etc. |
| **C - Direct opcode use** | Calls `fast_io::io_uring::build_statx_sqe()`, `fast_io::write_file_with_io_uring()`, etc. - either cfg-gated Linux-only code or convenience wrappers. | Migrate to `backend.submit_statx_blocking()`, `backend.build_disk_batch()`, etc. |

### 1.2 Complete inventory

Organized by crate, with each call site classified per section 1.1.

#### 1.2.1 `crates/cli/`

| File | Call site | Cat | Current usage |
|------|-----------|-----|---------------|
| `frontend/arguments/parser/mod.rs` | `fast_io::IoUringPolicy::{Enabled,Disabled,Auto}` | A | Parses `--io-uring` / `--no-io-uring` CLI flags into policy enum |
| `frontend/arguments/parsed_args/mod.rs` | `pub io_uring_policy: fast_io::IoUringPolicy` | A | Stores parsed policy in args struct |
| `frontend/server/flags.rs` | `fast_io::IoUringPolicy::{Enabled,Disabled,Auto}` | A | Server-side flag parsing |
| `frontend/execution/drive/config.rs` | `fast_io::IoUringPolicy` | A | Config field in drive execution |

**Migration:** None. All uses are category A (plain-data policy enum from
`io_uring_common.rs`).

#### 1.2.2 `crates/core/`

| File | Call site | Cat | Current usage |
|------|-----------|-----|---------------|
| `version/report/config.rs:140` | `fast_io::is_io_uring_available()` | B | Populates `supports_io_uring` bool in version report |
| `version/report/renderer.rs:219` | `fast_io::io_uring_status_detail()` | B | Writes io_uring status line to `--version` output |
| `client/config/builder/mod.rs:193` | `fast_io::IoUringPolicy` | A | Builder field type |
| `client/config/builder/partials.rs:96` | `fast_io::IoUringPolicy` | A | Builder method parameter |
| `client/config/client/mod.rs:170` | `fast_io::IoUringPolicy` | A | Client config field |
| `client/config/client/partials.rs:71,77` | `fast_io::IoUringPolicy`, `fast_io::IoUringConfig` | A | Accessor + doc reference |

**Migration:** Two category-B sites need migration. See section 3.2.

#### 1.2.3 `crates/transfer/`

| File | Call site | Cat | Current usage |
|------|-----------|-----|---------------|
| `config/mod.rs:63,67` | `fast_io::IoUringPolicy`, `fast_io::IoUringConfig` | A | Transfer config fields |
| `config/builder.rs:266` | `fast_io::IoUringPolicy` | A | Builder method |
| `transfer_ops/mod.rs:114,118` | `fast_io::IoUringPolicy`, `fast_io::IoUringConfig` | A | Transfer ops config |
| `transfer_ops/response.rs:119` | `fast_io::writer_from_file_with_depth(...)` | B | Creates `IoUringOrStdWriter` for file response writes |
| `disk_commit/config.rs:95,99` | `fast_io::IoUringPolicy`, `fast_io::IoUringConfig` | A | Disk commit config |
| `disk_commit/thread.rs:75-89` | `fast_io::IoUringPolicy`, `fast_io::IoUringConfig`, `fast_io::IoUringDiskBatch::try_new(...)` | B+C | Creates disk batch per policy |
| `disk_commit/thread.rs:114-136` | `fast_io::IoUringPolicy`, `fast_io::io_uring_availability_reason()` | B | Logs io_uring status |
| `disk_commit/process.rs:42,160,298` | `fast_io::IoUringDiskBatch` | C | Receives and uses disk batch for batched writes |
| `disk_commit/writer.rs:148` | `fast_io::IoUringDiskBatch` | C | Writer variant holding a disk batch reference |
| `generator/context.rs:417-428` | `fast_io::IoUringPolicy`, `fast_io::reader_from_path_with_depth(...)` | B | Opens io_uring reader for basis file |

**Migration:** Five category-B/C sites need migration. See section 3.3.

#### 1.2.4 `crates/engine/`

| File | Call site | Cat | Current usage |
|------|-----------|-----|---------------|
| `local_copy/executor/file/copy/transfer/execute/iouring.rs:188` | `fast_io::is_io_uring_available()` | B | Guards io_uring data-write dispatch |
| `local_copy/executor/file/copy/transfer/execute/iouring.rs:197` | `fast_io::write_file_with_io_uring(...)` | C | Writes file data through io_uring registered-buffer path |
| `concurrent_delta/strategy.rs:371` | `fast_io::read_file_with_io_uring(...)` | C | Slurps basis file through io_uring `READ_FIXED` path |

**Migration:** Three call sites need migration. All are feature-gated
(`iouring-data-writes`, `iouring-data-reads`). See section 3.4.

#### 1.2.5 `crates/fast_io/` (internal callers)

| File | Call site | Cat | Current usage |
|------|-----------|-----|---------------|
| `src/status.rs:7-10` | `crate::io_uring::{is_io_uring_available, config_detail}` | B | Status reporting for `--version` |
| `src/io_uring_ops.rs:10-15` | `crate::io_uring::{LinkAtArgs, RenameAt2Args, ...}` | C | try-then-fallback dispatch for rename/link/statx |
| `src/copy_file_range.rs:143-153` | `crate::io_uring::{IoUringConfig, is_io_uring_available}` | B+C | io_uring-accelerated copy fallback |
| `src/policy.rs` | `IoUringPolicy` (type alias) | A | Policy enum definition |
| `src/sqpoll_basis.rs` | doc reference to `crate::io_uring::config::build_ring` | A | Documentation only |

**Migration:** Three category-B/C sites need migration. See section 3.5.

### 1.3 Summary counts

| Category | Call sites | Migration needed |
|----------|-----------|-----------------|
| A - Policy/config | ~25 | 0 (platform-free types) |
| B - Free-function dispatch | ~12 | 10 |
| C - Direct opcode use | ~8 | 7 |
| **Total** | ~45 | **17** |

## 2. Migration strategy

### 2.1 Generic parameter vs trait object vs type alias

Three dispatch mechanisms are available. The choice per call site is
driven by the hot-path / cold-path classification from IUS-7.b:

| Mechanism | When to use | Codegen cost |
|-----------|-------------|-------------|
| **Generic `<B: IoUringBackend>`** | Hot paths (submission loops, probe checks). Monomorphized; zero vtable overhead. | One copy per backend type (2 copies total: Linux + stub). |
| **`&dyn IoUringBackend`** | Cold paths (version reporting, config construction, logging). Avoids monomorphization bloat for code called once per session. | One vtable indirect call per method invocation. |
| **Type alias** | When the concrete backend type is known at compile time via cfg. The simplest form; no generic parameter threading. | Zero overhead - direct call. |

**Decision: use the type-alias approach as the primary migration
strategy.** Rationale:

1. The `#[cfg]` gate in `lib.rs` already selects which `io_uring`
   module is compiled. The backend type can be selected at the same
   boundary.
2. Threading `<B: IoUringBackend>` through 5 crates (`cli -> core ->
   transfer -> engine -> fast_io`) to reach the 17 call sites would
   add a generic parameter to dozens of intermediate structs and
   functions, including `CoreConfig`, `TransferConfig`,
   `DiskCommitConfig`, `GeneratorContext`, etc. The parameter pollution
   is disproportionate to the benefit - there are exactly two backend
   types (Linux and stub), and which one is compiled is known at
   compile time.
3. The type alias retains the zero-cost property. `IoUringBackend`
   methods on a concrete type alias are direct calls, not vtable
   lookups. The compiler resolves the alias to the concrete type and
   inlines the forwarders.
4. The small number of call sites that need runtime backend selection
   (zero today, potentially IUR-2 per-thread storage in the future)
   can use `dyn DynIoUringBackend` locally without forcing generics on
   the rest of the tree.

### 2.2 Type-alias approach

Add to `crates/fast_io/src/lib.rs`:

```rust
/// The compiled io_uring backend for this platform.
///
/// On Linux with the `io_uring` feature, this is `LinuxIoUringOpsBackend`
/// with real kernel ring submission. On every other target, this is
/// `StubIoUringOpsBackend` where every operation returns `Unsupported`.
#[cfg(all(target_os = "linux", feature = "io_uring"))]
pub type PlatformIoUringBackend = io_uring::backend_impl::LinuxIoUringOpsBackend;

#[cfg(not(all(target_os = "linux", feature = "io_uring")))]
pub type PlatformIoUringBackend = io_uring::backend_stub::StubIoUringOpsBackend;
```

Callers use `fast_io::PlatformIoUringBackend` to obtain the backend, then
call trait methods. No generic parameter needed.

### 2.3 Backend instance lifecycle

A single process-wide backend instance is sufficient because:

- `LinuxIoUringOpsBackend` uses `OnceLock` for probe caching (IUS-8.b.1
  section 1.2). Multiple instances would each cache independently, wasting
  7 syscalls per instance on first probe. A shared instance amortizes this.
- The backend holds no ring state. Rings are created per call site via
  `backend.build_ring()` and owned by the caller.
- `StubIoUringOpsBackend` is stateless.

The instance lives in a process-wide `OnceLock`:

```rust
/// Process-wide io_uring backend instance.
///
/// Created on first access. The backend itself is cheap (two `OnceLock`
/// fields on Linux; zero fields on the stub). Ring creation is deferred
/// to individual call sites.
pub fn platform_io_uring_backend() -> &'static PlatformIoUringBackend {
    static BACKEND: OnceLock<PlatformIoUringBackend> = OnceLock::new();
    BACKEND.get_or_init(PlatformIoUringBackend::with_eager_probe)
}
```

Call sites that currently call `fast_io::is_io_uring_available()` become:

```rust
fast_io::platform_io_uring_backend().is_available()
```

### 2.4 Transition plan: two-phase migration

**Phase 1 (this spec):** Add the type alias, the `OnceLock` accessor,
and migrate the 17 call sites. Free functions remain as shims that
delegate to the backend. No external API removed.

**Phase 2 (IUS-9, future):** Deprecate the free functions with
`#[deprecated]`. After one release cycle, remove them.

This two-phase approach avoids breaking downstream code that may depend
on the free-function signatures (even though no external consumer exists
today).

## 3. Per-call-site migration plan

### 3.1 Notation

- **Before:** the current code as of IUS-8.b.1 merge.
- **After:** the migrated code.
- **Risk:** what can go wrong.

### 3.2 `crates/core/` (2 sites)

#### 3.2.1 `version/report/config.rs:140`

**Before:**
```rust
config.supports_io_uring = fast_io::is_io_uring_available();
```

**After:**
```rust
config.supports_io_uring = fast_io::platform_io_uring_backend().is_available();
```

**Risk:** None. Cold path, called once at startup.

#### 3.2.2 `version/report/renderer.rs:219`

**Before:**
```rust
let detail = fast_io::io_uring_status_detail();
```

**After:** No change. `io_uring_status_detail()` is a `fast_io`-internal
convenience that itself will delegate to the backend. The renderer does
not need to know about the backend.

### 3.3 `crates/transfer/` (5 sites)

#### 3.3.1 `transfer_ops/response.rs:119` - writer creation

**Before:**
```rust
let mut output = fast_io::writer_from_file_with_depth(
    file, writer_capacity, ctx.config.io_uring_policy, ctx.config.io_uring_depth,
)?;
```

**After:**
```rust
let backend = fast_io::platform_io_uring_backend();
let mut output = backend.writer_from_file(
    file, writer_capacity, ctx.config.io_uring_policy, ctx.config.io_uring_depth,
)?;
```

**Risk:** Low. The trait method `writer_from_file` forwards to the
same `mod.rs::writer_from_file_with_depth`. Return type changes from
`IoUringOrStdWriter` to the trait's associated writer type; callers use
it through `std::io::Write` which both types implement.

#### 3.3.2 `disk_commit/thread.rs:75-89` - disk batch creation

**Before:**
```rust
fn try_build_disk_batch(
    policy: fast_io::IoUringPolicy,
    depth: Option<u32>,
) -> Option<fast_io::IoUringDiskBatch> {
    let mut config = fast_io::IoUringConfig::default();
    if let Some(d) = depth { config.sq_entries = d; }
    match policy {
        fast_io::IoUringPolicy::Disabled => None,
        fast_io::IoUringPolicy::Auto => fast_io::IoUringDiskBatch::try_new(&config),
        fast_io::IoUringPolicy::Enabled => { /* ... */ fast_io::IoUringDiskBatch::try_new(&config) }
    }
}
```

**After:**
```rust
fn try_build_disk_batch(
    policy: fast_io::IoUringPolicy,
    depth: Option<u32>,
) -> Option<Box<dyn fast_io::io_uring::backend::DiskBatch>> {
    let mut config = fast_io::IoUringConfig::default();
    if let Some(d) = depth { config.sq_entries = d; }
    let backend = fast_io::platform_io_uring_backend();
    match policy {
        fast_io::IoUringPolicy::Disabled => None,
        fast_io::IoUringPolicy::Auto | fast_io::IoUringPolicy::Enabled => {
            backend.build_disk_batch(&config).ok()
        }
    }
}
```

**Risk:** Medium. The return type changes from the concrete
`IoUringDiskBatch` to `Box<dyn DiskBatch>`. This affects the callers
in `process.rs` and `writer.rs` that take `&mut IoUringDiskBatch`. They
must accept `&mut dyn DiskBatch` instead. This is a cold path (one
batch per disk-commit thread), so the `Box` allocation and vtable
overhead are acceptable.

**Alternative:** keep the concrete type by returning
`Option<fast_io::IoUringDiskBatch>` and calling the trait method
only for the construction:

```rust
backend.build_disk_batch(&config).ok().map(|b| *b.downcast::<IoUringDiskBatch>().unwrap())
```

Rejected: `downcast` requires `Any` bound and defeats the trait
abstraction. The `Box<dyn DiskBatch>` approach is cleaner.

#### 3.3.3 `disk_commit/thread.rs:114-136` - status logging

**Before:**
```rust
fast_io::io_uring_availability_reason()
```

**After:**
```rust
fast_io::platform_io_uring_backend().availability_reason()
```

**Risk:** None. Cold path, called once per session.

#### 3.3.4 `disk_commit/process.rs` + `writer.rs` - batch consumption

**Before:**
```rust
disk_batch: Option<&mut fast_io::IoUringDiskBatch>,
```

**After:**
```rust
disk_batch: Option<&mut dyn fast_io::io_uring::backend::DiskBatch>,
```

**Risk:** Low. The `DiskBatch` trait requires `io::Write` as a
supertrait, so callers that call `write_all` and `flush` on the batch
continue to work. The only `IoUringDiskBatch`-specific methods consumed
are `begin_file`, `commit_file`, and `bytes_written` - all of which are
on the `DiskBatch` trait.

#### 3.3.5 `generator/context.rs:417-428` - reader creation

**Before:**
```rust
match fast_io::reader_from_path_with_depth(
    path, self.config.write.io_uring_policy, self.config.write.io_uring_depth,
) {
    Ok(r) => return Ok(Box::new(r)),
    Err(_) => { /* fall through */ }
}
```

**After:**
```rust
let backend = fast_io::platform_io_uring_backend();
match backend.open_reader(path, &config) {
    Ok(r) => return Ok(r), // already Box<dyn FileReader>
    Err(_) => { /* fall through */ }
}
```

**Risk:** Low. The reader is already boxed as `Box<dyn Read>` at the
call site. The trait method returns `Box<dyn FileReader>`, which
implements `Read`.

### 3.4 `crates/engine/` (3 sites)

#### 3.4.1 `local_copy/.../iouring.rs:188` - availability check

**Before:**
```rust
if !fast_io::is_io_uring_available() {
    return Ok(None);
}
```

**After:**
```rust
if !fast_io::platform_io_uring_backend().is_available() {
    return Ok(None);
}
```

**Risk:** None. Cold path guarding the data-write dispatch.

#### 3.4.2 `local_copy/.../iouring.rs:197` - data write

**Before:**
```rust
match fast_io::write_file_with_io_uring(destination, &buf) { ... }
```

**After:** Two options:

**(a) Route through backend trait:**
```rust
let backend = fast_io::platform_io_uring_backend();
match backend.write_file(destination, &buf) { ... }
```

This requires adding a `write_file` convenience method to the
`IoUringBackend` trait (or using `build_disk_batch` + write + commit).

**(b) Keep the free function as a thin backend shim:**
The `fast_io::write_file_with_io_uring` function delegates internally to
the backend. The engine call site stays unchanged; the free function
becomes the migration boundary.

**Decision:** option (b) for this file. The function is feature-gated
behind `#[cfg(all(target_os = "linux", feature = "iouring-data-writes"))]`
and used in exactly one place. Adding a trait method for a single
feature-gated call site is over-engineering. The free function internally
delegates to backend methods when the backend migration is complete.

#### 3.4.3 `concurrent_delta/strategy.rs:371` - data read

**Before:**
```rust
if let Ok(bytes) = fast_io::read_file_with_io_uring(path) { ... }
```

**After:** Same reasoning as 3.4.2 - keep the free function as the
migration boundary. It is feature-gated behind
`#[cfg(all(target_os = "linux", feature = "iouring-data-reads"))]`.

### 3.5 `crates/fast_io/` internal (3 sites)

#### 3.5.1 `src/status.rs` - status reporting

**Before:**
```rust
use crate::io_uring;
fn io_uring_status_detail_impl() -> String {
    let info = io_uring::config_detail::io_uring_kernel_info();
    // ...
}
```

**After:**
```rust
fn io_uring_status_detail_impl() -> String {
    let backend = crate::platform_io_uring_backend();
    let info = backend.kernel_info();
    // ...
}
```

The cfg-branching in `status.rs` collapses: instead of 4 separate
cfg-gated function bodies for Linux/non-Linux x feature/no-feature,
a single body calls trait methods on the platform backend. The backend
itself handles the platform-specific behavior.

**Risk:** Low. This simplification reduces the 6 cfg-gated helper
functions in `status.rs` to 2-3 unconditional ones.

#### 3.5.2 `src/io_uring_ops.rs` - try-then-fallback dispatch

**Before:**
```rust
#[cfg(all(target_os = "linux", feature = "io_uring"))]
fn try_rename_via_io_uring_impl(...) -> Option<io::Result<()>> {
    if !renameat2_supported() { return None; }
    // ... build args, call renameat2_blocking ...
}
#[cfg(not(all(target_os = "linux", feature = "io_uring")))]
fn try_rename_via_io_uring_impl(...) -> Option<io::Result<()>> { None }
```

**After:**
```rust
fn try_rename_via_io_uring_impl(...) -> Option<io::Result<()>> {
    let backend = crate::platform_io_uring_backend();
    if !backend.renameat2_supported() { return None; }
    match backend.submit_renameat2_blocking(...) {
        Ok(result) if result < 0 => Some(Err(io::Error::from_raw_os_error(-result))),
        Ok(_) => Some(Ok(())),
        Err(e) if e.is_unsupported() => None,
        Err(e) => Some(Err(e.into())),
    }
}
```

The dual cfg-gated bodies collapse to one. The stub backend's
`renameat2_supported()` returns false, short-circuiting at the first
line. Same pattern for `try_hard_link_via_io_uring_impl` and
`try_statx_batch_via_io_uring_impl`.

**Risk:** Low. The behavior is identical: on non-Linux, every function
returns `None` at the probe check. On Linux, the existing wrapper code
runs through the trait forwarder.

#### 3.5.3 `src/copy_file_range.rs:143-153` - io_uring copy

**Before:**
```rust
#[cfg(all(target_os = "linux", feature = "io_uring"))]
fn try_io_uring_copy(...) -> io::Result<u64> {
    use crate::io_uring::{IoUringConfig, is_io_uring_available};
    if !is_io_uring_available() { return Err(...); }
    let config = IoUringConfig::default();
    let mut ring = config.build_ring()?;
    // ... submission loop ...
}
```

**After:**
```rust
fn try_io_uring_copy(...) -> io::Result<u64> {
    let backend = crate::platform_io_uring_backend();
    if !backend.is_available() {
        return Err(io::Error::new(io::ErrorKind::Unsupported, "io_uring not available"));
    }
    let config = IoUringConfig::default();
    let mut ring = backend.build_ring(&config)?;
    // ... submission loop using backend.submit_one(&mut ring, ...) ...
}
```

**Risk:** Medium. This function uses the raw `io-uring` crate's
`opcode::Read` / `opcode::Write` builders to construct SQEs and submits
them on a locally-constructed ring. After migration, SQEs are expressed
as `SubmissionEntry::Read { ... }` and `SubmissionEntry::Write { ... }`
variants and submitted through `backend.submit_one()`. The
`SubmissionEntry` enum must be expressive enough to carry the
offset/length/fd/buffer fields the current code passes to the raw
opcode builders.

## 4. Impact on public API surface of `fast_io`

### 4.1 New public items

| Item | Kind | Visibility |
|------|------|-----------|
| `PlatformIoUringBackend` | Type alias | `pub` in `lib.rs` |
| `platform_io_uring_backend()` | Function | `pub` in `lib.rs` |

### 4.2 Modified public items

None. All existing public functions, types, and re-exports remain.
They become thin shims over the backend trait methods internally.

### 4.3 Removed public items

None in this phase. Free-function removal is IUS-9 scope.

### 4.4 Downstream crate impact

| Crate | Change needed |
|-------|---------------|
| `cli` | None (category A only) |
| `core` | 1 line changed (version report) |
| `transfer` | ~15 lines changed (writer/reader/batch creation + types) |
| `engine` | 2 lines changed (availability checks) |
| `daemon` | None (uses `transfer` layer) |
| `protocol` | None |

The type change in `transfer::disk_commit` from
`Option<fast_io::IoUringDiskBatch>` to
`Option<Box<dyn fast_io::io_uring::backend::DiskBatch>>` is the only
signature change visible to callers of the `transfer` crate's internal
API. This is a crate-internal type; no external consumer is affected.

## 5. Backward compatibility

### 5.1 Feature flag compatibility

No new feature flags. Existing flags (`io_uring`, `iouring-data-reads`,
`iouring-data-writes`, `iouring-send-zc`) continue to gate the same
functionality. The `PlatformIoUringBackend` type alias selects the
correct backend per cfg:

| Configuration | `PlatformIoUringBackend` resolves to |
|---------------|--------------------------------------|
| Linux + `io_uring` feature | `LinuxIoUringOpsBackend` |
| Linux without `io_uring` feature | `StubIoUringOpsBackend` |
| macOS (any features) | `StubIoUringOpsBackend` |
| Windows (any features) | `StubIoUringOpsBackend` |

### 5.2 Cfg gate reduction

After migration, several files that currently have dual cfg-gated
function bodies (e.g., `status.rs`, `io_uring_ops.rs`) collapse to
single bodies that dispatch through the trait. This is a net reduction
in cfg complexity:

| File | Before (cfg branches) | After (cfg branches) |
|------|-----------------------|---------------------|
| `status.rs` | 6 cfg-gated functions | 2-3 unconditional functions |
| `io_uring_ops.rs` | 6 cfg-gated functions | 3 unconditional functions |
| `copy_file_range.rs` | 2 cfg-gated functions | 1 unconditional function |
| `lib.rs` (re-exports) | 2 cfg paths for `io_uring` module | 2 cfg paths (unchanged) + 2 for type alias |

### 5.3 Semver impact

No breaking changes. New items are purely additive. The type change
in `disk_commit` is crate-internal (`pub(crate)` visibility).

## 6. Testing strategy

### 6.1 Compile-test both backends

Every migrated call site must compile on both backends. The CI
cross-platform matrix (Linux, macOS, Windows) already verifies this.
After migration, the macOS and Windows builds exercise the stub backend
path; Linux exercises the real backend.

### 6.2 Existing test coverage

Migrated call sites are already covered by:

- `crates/fast_io/tests/iouring_probe_fallback_mock.rs` - probes the
  free-function dispatch with policy variants
- `crates/fast_io/tests/io_uring_probe_fallback.rs` - same, on Linux
- `crates/fast_io/tests/io_uring_byte_identical.rs` - writer byte
  fidelity
- `crates/transfer/src/disk_commit/tests.rs` - disk batch creation
  per policy
- `crates/fast_io/src/status.rs` (embedded tests) - status reporting
- `crates/fast_io/src/io_uring_ops.rs` (embedded tests) - dispatch

After migration, these tests exercise the backend trait path instead of
the free-function path. No new test files are needed; the existing tests
validate the same behavior through the new dispatch path.

### 6.3 New tests

| Test | Purpose |
|------|---------|
| `crates/fast_io/tests/platform_backend_smoke.rs` | Verifies `platform_io_uring_backend()` returns a valid backend on every platform. Checks `is_available()` consistency with `is_io_uring_available()`. |
| `crates/fast_io/tests/backend_free_fn_parity.rs` | For each migrated free function, asserts that the trait-method path and the free-function path produce identical results (same error kind, same success value, same bytes written). |

### 6.4 Interop test regression

The interop suite (`tools/ci/run_interop.sh`) exercises the full
transfer pipeline including the io_uring writer/reader paths on Linux.
A regression here would indicate the trait forwarding layer is not
byte-identical to the direct path.

### 6.5 Feature-gate matrix

The following cargo feature combinations must compile and pass tests:

| Combination | Coverage |
|-------------|----------|
| `--all-features` (Linux) | Real backend, all optional opcodes |
| `--no-default-features` (Linux) | Stub backend on Linux |
| `--all-features` (macOS) | Stub backend on macOS |
| `--all-features` (Windows) | Stub backend on Windows |
| `--features io_uring` (Linux) | Real backend, no optional opcodes |

## 7. Performance regression risk

### 7.1 Monomorphization vs vtable

The type-alias approach (section 2.2) produces identical codegen to
direct calls because the compiler resolves the alias to the concrete
backend type at compile time. There is no vtable, no indirect call, and
no `Box` allocation on any hot path.

The one exception is `disk_commit/process.rs`, which changes from
`&mut IoUringDiskBatch` to `&mut dyn DiskBatch`. The `DiskBatch` trait
methods (`begin_file`, `write_data`, `commit_file`) are called in the
disk-commit thread's drain loop - a warm path but not the innermost hot
loop. The vtable overhead is ~1-2 ns per call, bounded by the number of
files committed (not the number of bytes). For a 100 K-file transfer
this adds ~0.2 ms total.

### 7.2 `OnceLock` accessor cost

`platform_io_uring_backend()` reads a process-wide `OnceLock`. After
initialization, this is a single atomic `Acquire` load (~1 ns). Call
sites that invoke backend methods in a loop should hoist the accessor
outside the loop:

```rust
let backend = fast_io::platform_io_uring_backend();
for entry in entries {
    backend.submit_one(&mut ring, entry)?;
}
```

The current free-function call sites already pay an equivalent cost
(the `is_io_uring_available()` atomic load).

### 7.3 Expected regression bounds

| Metric | Before | After | Delta |
|--------|--------|-------|-------|
| Hot-path dispatch (submit/drain) | Direct call | Inlined trait method | 0 % (same codegen) |
| Disk batch writes | `&mut IoUringDiskBatch` | `&mut dyn DiskBatch` | +1-2 ns/call |
| Availability check | `is_io_uring_available()` (atomic) | `backend.is_available()` (OnceLock + atomic) | +1 ns first call |
| Writer/reader creation | Free function | Trait method via type alias | 0 % (same codegen) |

No metric exceeds the 2 % threshold from IUS-7.b.

## 8. Migration ordering and dependencies

### 8.1 Dependency graph

```
IUS-8.a (trait surface)
  |
  v
IUS-8.b.1 (Linux impl skeleton)
  |
  v
IUS-8.b.2 (THIS SPEC - caller migration)
  |
  v
IUS-8.c (stub tree deletion)
  |
  v
IUS-9 (free-function deprecation + removal)
```

### 8.2 Suggested PR sequence

The 17 call-site migrations can ship as a single PR or 3 stacked PRs:

| PR | Scope | Files touched |
|----|-------|---------------|
| PR 1 | `fast_io` internals: add type alias, OnceLock accessor, migrate `status.rs`, `io_uring_ops.rs`, `copy_file_range.rs` | 4 files in `fast_io` |
| PR 2 | External callers: `core`, `transfer`, `engine` | ~8 files across 3 crates |
| PR 3 | Test parity: `platform_backend_smoke.rs`, `backend_free_fn_parity.rs` | 2 test files |

Single-PR is preferred if the total diff is under 300 LoC. The
estimated diff is ~200 LoC of production code + ~100 LoC of tests.

### 8.3 Rollback plan

If any regression surfaces after merge:

1. The free functions still exist as shims. Callers can be reverted to
   call the free functions directly by reverting the migration PR.
2. The `PlatformIoUringBackend` type alias and `platform_io_uring_backend()`
   accessor are purely additive. They can remain even if individual call
   sites are reverted.

## 9. Open questions

### 9.1 `DiskBatch` trait object vs generic on disk-commit thread

The disk-commit thread uses `IoUringDiskBatch` as a concrete type
through `process.rs` and `writer.rs`. Changing to `dyn DiskBatch`
is the simplest migration, but it introduces a trait object on a warm
path. Alternative: make the disk-commit thread generic over
`<D: DiskBatch>` and monomorphize. This adds a generic parameter to
`DiskCommitContext`, `process_completion`, and `Writer::IoUring`.

**Recommendation:** start with `dyn DiskBatch`. If the vtable overhead
is measurable in the DIS-8.a daemon benchmark (>1 % wall-clock
regression), upgrade to the generic form.

### 9.2 `writer_from_file` return type

The trait's `writer_from_file` method returns a trait-level writer type.
The current free function returns `IoUringOrStdWriter` - a concrete enum.
Callers use this through `std::io::Write`. Two options:

**(a)** The trait method returns `Box<dyn Write + Seek>`.
**(b)** The trait method returns the concrete `IoUringOrStdWriter` (via
associated type).

**Recommendation:** option (b) during the coexistence window. The
concrete enum is the existing return type; callers already pattern-match
on it (e.g., to check if io_uring was selected). Erasing to `Box<dyn>`
would break those pattern matches. After IUS-8.c deletes the stub tree,
the enum can be reconsidered.

### 9.3 Feature-gated free functions

`write_file_with_io_uring` and `read_file_with_io_uring` are gated
behind optional cargo features. They are not on the `IoUringBackend`
trait (IUS-7.a section 9.5 - methods exist unconditionally, returning
`OpcodeUnsupported` when the feature is off). Should these functions
migrate to trait methods?

**Recommendation:** no. They are used in exactly one place each,
behind hard cfg gates. They become thin shims over backend internals in
phase 2 (IUS-9). Adding unconditional trait methods for single-use
feature-gated paths adds surface area without proportional benefit.

---

**Summary:** 17 call sites across 4 crates migrate from concrete
io_uring free functions to a cfg-selected type alias
(`PlatformIoUringBackend`) backed by the `IoUringBackend` trait. The
type-alias approach avoids generic-parameter pollution while preserving
zero-cost dispatch. The only signature change visible outside `fast_io`
is the `disk_commit` batch type (`IoUringDiskBatch` to
`dyn DiskBatch`), which is crate-internal. Estimated diff: ~300 LoC.
No behavioral change, no feature-flag change, no breaking API change.
