# IoStrategy trait abstraction in `fast_io` (#1765)

This note evaluates the proposal to introduce an `IoStrategy` trait in the
`fast_io` crate with implementations for Linux (io_uring), Windows (IOCP), and
a Std fallback - intended to replace the current `cfg`-gated dispatch with a
polymorphic, easier-to-test surface. It audits the dispatch shape that exists
on `master` today, the scope of the `IoBackend` trait that landed with #2244,
the residual gap between what #2244 already provides and what #1765 originally
asked for, and a recommendation with a five-step implementation plan in case
the gap is closed.

## 1. Current state

`fast_io` has two interleaved abstraction layers for per-call I/O dispatch.

### 1.1 Trait layer: `FileReader` / `FileWriter`

`crates/fast_io/src/traits.rs:12-49` defines two object-safe traits:

- `FileReader: Read` with `size`, `position`, `seek_to`, `remaining`, and
  `read_all`.
- `FileWriter: Write` with `bytes_written`, `sync`, and `preallocate`.

Factories (`FileReaderFactory`, `FileWriterFactory` at
`crates/fast_io/src/traits.rs:51-70`) are generic over the produced reader /
writer; they are *not* dispatched dynamically. The `Std*` reference
implementations live alongside, and io_uring / IOCP supply their own factory
types that return wrapper enums.

### 1.2 Enum layer: `IoUringOrStd*` and `IocpOrStd*`

Each platform backend exposes a two-variant enum that picks between the fast
backend and the buffered fallback at runtime:

- `IoUringOrStdReader` / `IoUringOrStdWriter` in
  `crates/fast_io/src/io_uring_stub.rs:988-1147` (mirrored in the live Linux
  backend).
- `IocpOrStdReader` / `IocpOrStdWriter` in
  `crates/fast_io/src/iocp_stub.rs:387-560` (mirrored in
  `crates/fast_io/src/iocp/file_factory.rs`).

Both enums implement `Read` / `Write` / `Seek` and the `FileReader` /
`FileWriter` traits by hand-written `match` arms - one arm per variant -
producing the per-call dispatch overhead of a tagged-union jump rather than a
vtable indirection. The factory entry points (`IoUringReaderFactory::open`,
`IocpWriterFactory::create`, ...) decide which variant to construct using the
runtime probe (`is_io_uring_available`, `is_iocp_available`) and the
caller-supplied `BackendPolicy`.

Higher-level dispatch in callers follows the same shape. The disk-commit
thread carries a four-variant `Writer<'a>` enum
(`crates/transfer/src/disk_commit/writer.rs:144-156`) gated by `#[cfg]`:

```rust
pub(super) enum Writer<'a> {
    Buffered(ReusableBufWriter<'a>),
    #[cfg(all(target_os = "linux", feature = "io_uring"))]
    IoUring { batch: &'a mut fast_io::IoUringDiskBatch },
    #[cfg(all(target_os = "windows", feature = "iocp"))]
    Iocp { batch: &'a mut fast_io::IocpDiskBatch },
    #[cfg(target_os = "macos")]
    Macos(fast_io::MacosWriter),
}
```

Every method on `Writer` matches on the variant, with `cfg`-gated arms for the
platform branches. This is the dispatch shape the #1765 proposal would
replace.

### 1.3 Selection layer: `BackendPolicy`

`crates/fast_io/src/policy.rs:28-46` defines the canonical `BackendPolicy`
enum (`Auto`, `Enabled`, `Disabled`) and reuses it via type aliases for
`IoUringPolicy`, `IocpPolicy`, and `ZeroCopyPolicy`. Selection is deferred
until factory construction: `try_create_disk_batch`
(`crates/transfer/src/disk_commit/thread.rs:74-92`) and `try_create_iocp_batch`
(same file, lines 100-107) consult the policy once per disk-commit thread,
not per write call. This is the init-time backend selection point.

## 2. What `IoBackend` already provides (#2244)

`crates/fast_io/src/io_uring_common.rs:470-484` defines the trait:

```rust
pub trait IoBackend {
    fn is_available() -> bool;
    fn availability_reason() -> String;
    fn sqpoll_fell_back() -> bool { false }
}
```

It has two implementations:

- `LinuxIoUringBackend` in `crates/fast_io/src/io_uring/mod.rs:155-170` -
  delegates to the real probes (`is_io_uring_available`,
  `io_uring_availability_reason`, `sqpoll_fell_back`).
- `StubIoUringBackend` in `crates/fast_io/src/io_uring_stub.rs:38-49` -
  reports `false` and a canned reason string.

The trait is deliberately narrow. It captures *availability metadata* - the
runtime queries that diagnostic surfaces (`--version`, `iocp_status_detail`,
`io_uring_status_detail` at `crates/fast_io/src/status.rs:504-560`) need
without committing callers to a particular reader / writer type. The
associated commit message (`9b9f854f`) names the design goal explicitly:

> "The trait has no associated types so it can be used in generic contexts
> without monomorphisation explosions. New methods should be plain queries
> with default implementations."

#2244's scope was stub-deduplication: lift the cross-platform plain-data
types and constants out of `io_uring_stub.rs` so the file shrank from 73 KB
of mechanical duplication to the opaque-handle shell that exists today. The
trait is a side-effect of that refactor, not the primary deliverable, and it
is intentionally typed for *information*, not *operations*.

There is currently **no IOCP implementation of `IoBackend`**: neither
`iocp::mod.rs` nor `iocp_stub.rs` references the trait. The cross-platform
diagnostic helpers in `status.rs` still go through free functions
(`iocp_availability_reason`, `is_iocp_available`) rather than the trait.

## 3. The gap between `IoBackend` and the proposed `IoStrategy`

The #1765 ask was framed around *per-call polymorphic dispatch* with three
implementations (Linux, Windows, Std). Mapping that to what exists today:

### 3.1 Backend selection at init time

`BackendPolicy` already supplies init-time selection at the call sites that
matter (`try_create_disk_batch`, `try_create_iocp_batch`). The policy is
consulted once per disk-commit thread and the resulting batch handle is held
for the thread's lifetime. There is no per-call selection cost; introducing
an `IoStrategy` trait would not change this.

Where the existing surface is still messy is the *enum branches in the
hot-path Writer* (`disk_commit/writer.rs:144-156`). Even though selection is
done once at thread start, every `write_chunk` call still pays for a
tagged-union dispatch with `cfg`-gated arms. A trait object would collapse
the four arms into a single vtable call.

### 3.2 Per-call dispatch overhead

The enum match in `Writer::write_chunk` is two-way at run time on any given
platform (the buffered fallback plus the platform-specific fast path). LLVM
lowers two-arm matches to a single branch and the inner calls are typically
inlined, so the runtime cost vs a `Box<dyn FileWriter>` vtable call is
roughly:

| Form | Branch + call | Cache behaviour | Notes |
|------|---------------|-----------------|-------|
| Enum match (today) | 1 compare + indirect | Excellent (hot in I-cache) | Variant tag is in the same line as the payload |
| `Box<dyn FileWriter>` | 1 vtable indirection | One extra cache miss on cold-call | No tag compare, no inlining |
| Generic `impl FileWriter` | Inlined per monomorphisation | Best (full inlining) | Code-size cost across factories |

On the hot per-chunk write path, the enum match is the cheapest form and
already in place. The vtable form would be measurably *worse* by one indirect
branch and likely a few percent on workloads dominated by small writes. The
generic form is what the `FileReaderFactory` / `FileWriterFactory` traits
already use, and it is what the per-platform code that *can* monomorphise
already takes advantage of.

### 3.3 Test ergonomics

This is the only area where the trait-based approach has an unambiguous
advantage. Mocking the current enum requires either:

1. Adding a `Mock(MockWriter)` variant to `Writer<'a>` (intrusive, requires
   touching the production enum), or
2. Hand-rolling a parallel test-only enum and writing duplicate plumbing.

A `dyn FileWriter` slot would let tests inject any `FileWriter`
implementation without touching the production enum. However, `FileWriter`
already exists and *can* serve this role. The blocker for swap-in mocks
today is that the disk-commit thread holds a *concrete* `Writer<'a>` enum,
not a `Box<dyn FileWriter>`. That is a localised refactor inside
`disk_commit/writer.rs` rather than a new fast_io abstraction.

### 3.4 Cross-cutting parity

There is one real `IoBackend` gap worth closing regardless of the #1765
disposition: **IOCP does not implement `IoBackend`**. Adding an
`IocpBackend` (live) and `StubIocpBackend` would let the diagnostic surface
in `status.rs` route both subsystems through a single trait instead of two
parallel sets of free functions. This is a one-file change that mirrors the
existing Linux pair and does not require introducing per-call polymorphism.

## 4. Decision matrix

| Option | Selection cost | Per-call cost | Test ergonomics | Code-size cost | New surface to maintain |
|--------|----------------|---------------|-----------------|----------------|-------------------------|
| Keep enum dispatch (today) | None (init-time) | 1 tag compare | Intrusive mock variant | Small | None |
| `IoStrategy` trait + `Box<dyn>` | None (init-time) | 1 vtable indirection | Direct mock injection | Small | New trait, three impls, IocpBackend |
| Hybrid: enum-of-`Box<dyn>` | None (init-time) | 1 tag compare + 1 vtable | Direct mock injection | Small | Trait + enum + impls |
| Keep enum, add `IocpBackend` impl of existing `IoBackend` | None (init-time) | 1 tag compare | Intrusive mock variant | Negligible | One new marker type |

The hybrid option pays *both* dispatch costs while gaining only the trait's
test ergonomics. The pure-trait option trades a measurable per-call branch
for mockability that can already be achieved through the existing
`FileWriter` trait by tightening one caller (`disk_commit::Writer`).

## 5. Recommendation

**Defer #1765.** The original task statement is largely covered by #2244 plus
existing infrastructure, with one small follow-up:

1. `IoBackend` already supplies the cross-backend availability surface.
2. `BackendPolicy` already supplies init-time selection.
3. `FileReader` / `FileWriter` already supply the object-safe contract that
   per-call polymorphism would need; they are not used dynamically only
   because no caller has requested swap-in mocks.
4. The remaining `cfg`-gated dispatch in `disk_commit/writer.rs` is the
   shape callers actually want for the hot path (one-tag-compare, fully
   inlined, no vtable miss).

The only concrete gap is that **IOCP has no `IoBackend` implementation**.
Closing that gap brings cross-backend parity without adding per-call vtable
cost.

### Recommendation summary

| Action | Status |
|--------|--------|
| Introduce a per-call `IoStrategy` trait with Linux / Windows / Std impls | **Reject** - costs a vtable indirection on the hot write path; no measured win |
| Add `IocpBackend` + `StubIocpBackend` impls of the existing `IoBackend` | **Implement** - one-file parity follow-up, no perf impact |
| Refactor `disk_commit::Writer` to hold `Box<dyn FileWriter>` for mockability | **Optional** - only if a future test requires it |

## 6. Five-step plan if the IocpBackend follow-up is taken

If the IOCP parity follow-up (the only concrete gap above) is greenlit, the
implementation is small and contained:

1. **Define the marker types.** Add `LiveIocpBackend` in
   `crates/fast_io/src/iocp/mod.rs` and `StubIocpBackend` in
   `crates/fast_io/src/iocp_stub.rs`. Both are unit structs that derive
   `Debug`, `Clone`, `Copy`, `Default` - mirroring the io_uring pair.
2. **Implement `IoBackend` for each.** `LiveIocpBackend::is_available`
   delegates to `is_iocp_available`; `availability_reason` delegates to
   `iocp_availability_reason`; `sqpoll_fell_back` keeps the default
   `false`. The stub returns `false` and a canned reason string ("IOCP:
   disabled (not built for this target)").
3. **Re-export from `lib.rs`.** Add `pub use iocp::LiveIocpBackend` (and
   `StubIocpBackend` from the stub when compiled) next to the existing
   `pub use io_uring_common::IoBackend` line at
   `crates/fast_io/src/lib.rs:210`.
4. **Route `status.rs` through the trait.** Replace the direct calls in
   `iocp_status_detail` with `<LiveIocpBackend as IoBackend>::...` so both
   backends share a single diagnostic codepath. Keep the free-function
   shims for source compatibility.
5. **Add parity tests.** Mirror the io_uring availability tests in
   `crates/fast_io/src/io_uring_common.rs:486-549` for the IOCP pair so
   the stub backend's `is_available` returns `false` on every non-Windows
   target and the live backend's reason string is non-empty on Windows.

No callers change. The vtable is never on the hot path because `IoBackend`
methods are status queries called at init / diagnostics time, not per
chunk. This is the right shape for the abstraction that #2244 already
shipped, extended to the second platform backend.
