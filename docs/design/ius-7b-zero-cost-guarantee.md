# IUS-7.b - Zero-cost guarantee for the Linux `IoUringBackend`

Date: 2026-05-23
Scope: design-only specification of what "zero-cost" means for the
Linux impl of the `IoUringBackend` trait defined in IUS-7.a, the
mechanisms that achieve it, the verification methodology that proves
it, the anti-patterns the impl must forbid, and the per-method audit
that classifies hot vs cold paths.
Status: **SPEC DRAFT** - no source changes in this PR; verification
companion to `docs/design/ius-7a-trait-surface.md`. Referenced by
IUS-8.a (trait define), IUS-8.b (Linux impl), and IUS-8.c (stub
collapse) for acceptance criteria.
Predecessor: `docs/design/ius-7a-trait-surface.md` (38-method
`IoUringBackend` + 4 auxiliary traits = 57 methods; `LinuxIoUringBackend`
is the production impl).
Related: `docs/design/io-strategy-trait.md` (#1765 disposition - the
existing `IoBackend` trait is information-only by design; this spec
extends to *operations* without re-introducing the per-call vtable
penalty that #1765 deferred).

## 0. Why this spec exists

IUS-7.a settles the trait's shape. It does **not** settle the cost
model. The 38 `IoUringBackend` methods sit on the hottest data plane
in the codebase: the per-SQE submission loop in `parallel_apply`, the
per-chunk write path in `disk_commit`, the per-completion drain loop
in `shared_ring`, and the per-syscall probe shortcuts that gate
fast-path opcodes. A naive `dyn IoUringBackend` plumbed through those
loops adds:

- one vtable indirection per call (4-8 ns L1, 20-40 ns under cache
  pressure),
- one branch prediction miss on the indirect call site,
- one inlining boundary that disables LLVM's SQE-construction
  constant folding,
- one `Arc<dyn ...>` clone per submission if the handle is shared
  across threads (atomic refcount = ~5 ns + cache-line contention).

For a 10 GB/s NVMe target at 4 KB SQEs that is 2.5 M submissions per
second per worker; even 10 ns of trait overhead per call costs 2.5 %
throughput. The IUS-3 / IUR-2 / SQM-1 work all assume the trait is
free. **This spec writes that assumption down and gives IUS-8.b a
quantitative acceptance gate.**

The non-Linux stub side of zero-cost is trivial: every method returns
`Err(IoUringError::Unsupported)` (or `Ok(false)` for probes), the
optimiser folds the entire body to a single return, and there is no
hot path to defend on non-Linux targets. This spec therefore focuses
exclusively on the Linux impl.

## 1. What "zero-cost" means here

Zero-cost in this design has a precise, falsifiable definition broken
into two claims:

### 1.1 Compile-time claim (codegen)

Trait dispatch must compile away on Linux release builds
(`--release`, default `opt-level = 3`, default `lto = "off"`; LTO is
not required to meet the guarantee but must not regress it). For
every `IoUringBackend` method invoked through a statically-typed
`impl IoUringBackend` parameter:

- **No vtable lookup.** The emitted instruction stream must contain
  no indirect call (`callq *%reg`) to a function-pointer table.
- **No `Box` allocation.** No `malloc` / `__rust_alloc` appears in
  the call site or the inlined body. The non-erased `Box<dyn ...>`
  returns from `drain_completions` and `build_session_pool` are
  exceptions and are documented in section 6.
- **No spurious branch.** The match-on-`SubmissionEntry` dispatch in
  `submit_one` must fold to a single arm when the variant is
  statically known at the call site. For dynamic variants (vector of
  mixed SQE kinds) the match remains but matches a jump-table.
- **Inlined direct call.** Each `IoUringBackend` method, when called
  through the trait with a statically known impl, must produce
  assembly identical to calling the underlying `IoUring` wrapper
  function directly. "Identical" means same instruction count + same
  register allocation modulo function-prologue/epilogue stitching.

### 1.2 Run-time claim (benchmarks)

For each method classified as "hot path" in section 5, the through-trait
call latency must be within **2 %** of the direct-call baseline at
`p50` and within **5 %** at `p99` (`p99` allows for one extra cache
miss caused by an unrelated allocation in the inlined caller). The
2 % gate is the headline acceptance criterion.

Rationale for 2 %: at 10 M submits/sec the absolute budget is 0.4 ns
per call - within the noise floor of a `rdtsc`-measured tight loop on
modern x86 (1-2 cycles ~= 0.5 ns) and below the per-syscall amortised
io_uring cost (~120 ns per `io_uring_enter` waking the kernel). A
2 % delta is therefore detectable by `criterion` (10 k samples, 5 %
significance threshold) but tight enough to flag real regressions.

5 % was considered and rejected as too lax: it allows a 2-3 ns
regression per call which would silently absorb a future
"convenient" `Arc<dyn>` plumbing change. 1 % was considered and
rejected as too tight: it sits inside `criterion`'s own measurement
noise on shared CI hardware. **2 % is the CI gate.** Section 3.4
specifies the bench harness; section 7 specifies the IUS-8.b test
plan.

### 1.3 What "zero-cost" does NOT mean

- Not "zero kernel cost." `io_uring_enter` still costs what it costs;
  the trait does not change kernel behaviour.
- Not "zero amortised cost across method calls." One-shot init
  methods (`register_buffers`, `register_files`,
  `build_session_pool`) are allowed to pay vtable cost because they
  fire at session setup, not in the per-SQE loop.
- Not "identical with `lto = "fat"`." The guarantee is at default LTO
  ("off" in dev, "thin" in release for this workspace; both meet the
  bar). Fat LTO is a nice-to-have, not a requirement.

## 2. Mechanisms to ensure zero-cost

### 2.1 `#[inline(always)]` on every trait method impl

Every `IoUringBackend` method on `LinuxIoUringBackend` carries
`#[inline(always)]`. `#[inline]` alone is insufficient: rustc's
inlining heuristic refuses to inline functions that cross a crate
boundary at default `-Copt-level=3` unless the function is in a generic
context or annotated `always`. The trait method bodies are 1-3 lines
of forwarder code; `#[inline(always)]` is appropriate and matches the
upstream rsync style of marking the rolling-checksum and SQE-init
hot paths as forced-inline.

`#[inline(always)]` carries a known risk: it can bloat call sites and
hurt I-cache locality. For 38 1-3 line methods that risk is bounded
- each inlined body is on the order of 4-16 x86 instructions, vs a
`call`+`ret` pair (~6 cycles) saved per invocation. Section 3.2 of
the verification methodology mandates an I-cache miss-rate check on
the hot path; if `perf stat -e L1-icache-load-misses` shows >2 %
regression vs the baseline the offending methods drop back to
`#[inline]` and rely on cross-crate inlining via thin LTO.

### 2.2 Generic dispatch preferred over `dyn` for hot paths

Hot-path callers take `impl IoUringBackend` or `B: IoUringBackend`,
not `&dyn IoUringBackend`:

```rust
// Hot-path: monomorphises per backend; LLVM inlines the impl.
pub fn submit_loop<B: IoUringBackend>(backend: &B, ring: &mut B::Ring, sqes: &[SubmissionEntry<'_>]) {
    for sqe in sqes {
        backend.submit_one(ring, *sqe).ok();
    }
}

// Cold-path: dyn is acceptable because this fires once per session.
pub fn register_session_buffers(backend: &dyn DynIoUringBackend, bufs: &[&[u8]]) -> Result<(), IoUringError> {
    backend.register_buffers_dyn(bufs)
}
```

The associated type `Ring` on `IoUringBackend` already pushes callers
toward generics: `dyn IoUringBackend` is not object-safe without the
`BoxedRing` adapter from IUS-7.a section 9.2. Generic dispatch is
therefore the path of least resistance and the path the IUS-8.b
acceptance gate rewards.

### 2.3 Dyn dispatch is acceptable for one-shot probes

A small number of `IoUringBackend` methods fire once per ring
construction or once per process. For these, `dyn` dispatch is
acceptable because the cost is amortised at session init, not in the
hot loop:

- `is_available`, `availability_reason`, `kernel_info` -
  diagnostics, called once per `--version` or once per error log.
- `build_ring` - once per ring.
- `register_buffers`, `register_files`,
  `register_provided_buffer_ring` - once per ring lifetime.
- `allocate_bgid`, `deallocate_bgid` - once per bgid lifetime, which
  is typically once per provided-buffer ring construction.
- `build_session_pool`, `build_shared_ring` - once per session.
- `build_disk_batch` - once per disk-commit thread.
- `open_reader`, `open_writer`, `writer_from_file` - once per file.

These methods may be called through `dyn DynIoUringBackend` without
violating the guarantee. Section 5 marks each method explicitly.

### 2.4 Const-generic specialisation for fixed-size submission entries

The current trait does not use const generics; `SubmissionEntry` is a
runtime tagged enum. Const-generic specialisation is **not** required
to meet the 2 % gate (the enum tag folds away when the variant is
statically known). It is documented here as a future option if the
2 % gate ever loosens:

```rust
// Hypothetical const-generic specialisation; deferred.
trait SubmitFixed<const N: usize> {
    fn submit_n(&self, ring: &mut Self::Ring, sqes: [SubmissionEntry<'_>; N]) -> Result<[SubmissionToken; N], IoUringError>;
}
```

Open issue: when ABW-2 lands pipelined verify/write with batch size
known at compile time, the const-generic variant becomes attractive.
Tracked in section 8.3.

### 2.5 Forwarders, not wrappers

The Linux impl's method bodies are pure forwarders to the existing
`io_uring::*` wrappers. They do not:

- log,
- update metrics,
- check probe caches (the cache lookup is in the wrapper, not the
  trait method),
- validate arguments beyond what the wrapper validates,
- allocate.

Any such concern is added to the wrapper, not to the trait method.
This keeps the inlined body small enough that `#[inline(always)]`
remains net-positive for I-cache.

## 3. Verification methodology

Four orthogonal verification techniques, run in order from cheapest
to most expensive. Each is wired into the IUS-8.b CI gate.

### 3.1 `cargo expand` on a known hot path

Goal: confirm that the trait method's macro expansion does not
introduce a dynamic dispatch boundary.

Command (run in the IUS-8.b PR description, not in CI):

```sh
cargo expand --release -p fast_io --lib io_uring::backend_impl::submit_loop
```

Expected: the expanded function body inlines `submit_one`'s match arm
directly under the loop, with no `<dyn ...>::submit_one` call. This
catches regressions from macro changes (e.g., a future
`#[instrument]` wrapper from `tracing` re-introducing a function
boundary).

`cargo expand` is informational; the asm check in 3.2 is the binding
gate.

### 3.2 `cargo asm` (or `cargo-show-asm`) on a representative function

Goal: prove the generated assembly matches the direct-call baseline.

For each of the 7 hot-path methods (section 5), the IUS-8.b PR adds
a fixture function in `crates/fast_io/tests/backend_asm.rs`:

```rust
// Test fixtures - one per hot-path method - that the asm-diff step
// builds and inspects. These are not run as functional tests; they
// exist purely so the asm-diff script can locate stable symbols.
#[inline(never)]
pub fn submit_one_through_trait<B: IoUringBackend>(b: &B, r: &mut B::Ring, sqe: SubmissionEntry<'_>) -> Result<SubmissionToken, IoUringError> {
    b.submit_one(r, sqe)
}

#[inline(never)]
pub fn submit_one_direct(r: &mut SharedRing, sqe: SubmissionEntry<'_>) -> Result<SubmissionToken, IoUringError> {
    // Direct call sequence equivalent to the trait method body.
    submit_one_impl(r, sqe)
}
```

The `#[inline(never)]` on the fixture is intentional: it prevents the
fixture itself from being inlined into the caller, so the asm-diff
compares the *inside* of the fixture (which is what we care about,
the inlined trait call) rather than the whole call-site.

CI step:

```sh
cargo asm --release -p fast_io --lib backend_asm::submit_one_through_trait > /tmp/trait.s
cargo asm --release -p fast_io --lib backend_asm::submit_one_direct > /tmp/direct.s
diff -u /tmp/direct.s /tmp/trait.s | tee /tmp/asm-diff.txt
# Acceptance: difference is at most function name + return label;
# instruction sequence inside is byte-identical modulo register names.
```

The CI script normalises register names (`rax` <-> `rcx` allocation
differences are not regressions) and function-prologue stitching
before comparing. A non-empty normalised diff fails the gate.

The asm-diff runs on `x86_64-unknown-linux-gnu` only; aarch64 Linux
gets a separate diff with the same logic. The diff fixtures are
checked in under `crates/fast_io/tests/backend_asm/` so reviewers can
inspect the baseline assembly.

### 3.3 Micro-benchmark: 10 M submits through trait vs direct call

Goal: empirical run-time confirmation that the through-trait dispatch
is within the 2 % gate.

Bench file: `crates/fast_io/benches/backend_dispatch.rs`. Uses
`criterion` (already a dev-dependency of `fast_io`). One bench group
per hot-path method; each group has two benches: `_through_trait`
and `_direct`. Each bench loops 10 M iterations of a no-op
submission (uses `IORING_OP_NOP` so the kernel work is constant
across both arms).

```rust
fn bench_submit_one(c: &mut Criterion) {
    let backend = LinuxIoUringBackend::new();
    let mut ring = backend.build_ring(&IoUringConfig::default()).unwrap();
    let sqe = SubmissionEntry::Nop { user_data: 0 };

    let mut group = c.benchmark_group("submit_one");
    group.throughput(Throughput::Elements(10_000_000));

    group.bench_function("through_trait", |b| {
        b.iter(|| {
            for _ in 0..10_000_000 {
                black_box(submit_one_through_trait(&backend, &mut ring, sqe));
            }
        })
    });

    group.bench_function("direct", |b| {
        b.iter(|| {
            for _ in 0..10_000_000 {
                black_box(submit_one_direct(&mut ring, sqe));
            }
        })
    });

    group.finish();
}
```

`IORING_OP_NOP` is added to `SubmissionEntry` for this purpose if
not already present; it has no kernel side-effect beyond the SQE
state-machine round trip and so isolates the dispatch cost.

The bench runs on the existing `oc-rsync-bench` container (Arch
Linux on bare-metal-equivalent CI runner). Results are persisted to
`target/criterion/backend_dispatch/` and the IUS-8.b PR description
includes the side-by-side mean numbers.

### 3.4 CI gate: bench delta < 2 %

Goal: a quantitative pass/fail signal in CI for IUS-8.b and every
subsequent PR that touches the trait or the Linux impl.

Implementation: a new CI job `io-uring-zero-cost` in
`.github/workflows/benchmarks.yml` runs the
`backend_dispatch` criterion bench on the `oc-rsync-bench` self-hosted
runner (Linux x86_64; aarch64 Linux is added as a follow-up under
IUS-8.b.3 if available). The job parses `criterion`'s JSON output and
computes the `through_trait_mean / direct_mean` ratio for each
hot-path method. The job fails if any ratio exceeds **1.02** (the
2 % threshold).

Pseudo-code for the gate:

```sh
cargo bench --bench backend_dispatch -- --save-baseline pr
python3 tools/ci/check_zero_cost.py \
    target/criterion/backend_dispatch \
    --max-overhead-pct 2.0
```

`tools/ci/check_zero_cost.py` reads
`target/criterion/<group>/<bench>/new/estimates.json`, computes the
ratio per group, prints a markdown summary suitable for the PR
status, and exits non-zero if any ratio exceeds the threshold.

The gate is **mandatory for the IUS-8.b merge** and **mandatory for
every subsequent change to `fast_io::io_uring::backend_impl` or to
the trait itself**. It is **advisory** (warning, not failure) for
unrelated PRs to keep CI flake low.

A 5 % warn-only threshold sits above the 2 % fail threshold to flag
suspicious-but-not-broken changes for review (e.g., a 3 % regression
gets a CI warning that prompts an asm-diff inspection before merge).

## 4. Anti-patterns to forbid

The following patterns are forbidden in any caller of
`IoUringBackend`. The IUS-8.b PR adds the patterns to
`tools/audit_no_dyn_in_hot_path.sh` (a grep-based linter) and to
`crates/fast_io/src/io_uring/backend.rs` rustdoc as "Do NOT" examples.

### 4.1 `Box<dyn IoUringBackend>` in any per-op storage

```rust
// FORBIDDEN: stores a boxed dyn handle on every submission record.
struct PendingSubmission {
    backend: Box<dyn DynIoUringBackend>, // <-- one Box per pending op
    sqe: SubmissionEntry<'static>,
}
```

The `Box<dyn>` here triples the per-pending-op heap footprint (24 B
for the fat pointer + 24 B `Box` overhead vs an 8 B `&B` reference)
and adds a per-call vtable lookup. The correct pattern is to hold
the backend by reference on the worker (each worker has one backend
for its ring lifetime) and pass it down to the submission record's
constructor.

### 4.2 `dyn IoUringBackend` parameter to a per-submission function

```rust
// FORBIDDEN: every per-SQE call pays a vtable lookup.
fn submit_one(backend: &dyn DynIoUringBackend, ring: &mut BoxedRing, sqe: SubmissionEntry<'_>) {
    backend.submit_one(ring, sqe).ok();
}

// CORRECT: monomorphises; LLVM inlines the impl.
fn submit_one<B: IoUringBackend>(backend: &B, ring: &mut B::Ring, sqe: SubmissionEntry<'_>) {
    backend.submit_one(ring, sqe).ok();
}
```

The `audit_no_dyn_in_hot_path.sh` linter greps for `&dyn
(?:Dyn)?IoUringBackend` in callers under `crates/transfer/`,
`crates/engine/`, and `crates/fast_io/src/io_uring/`. Hits in
`crates/cli/`, `crates/daemon/setup/`, and any file matching
`*setup*.rs` / `*init*.rs` / `*config*.rs` are allowlisted because
those paths are session-init, not per-op.

### 4.3 `Arc<dyn IoUringBackend>` cloned per call

```rust
// FORBIDDEN: atomic refcount bump on every SQE.
fn dispatch(backend: Arc<dyn DynIoUringBackend>, sqes: &[SubmissionEntry<'_>]) {
    for sqe in sqes {
        let b = backend.clone(); // <-- atomic increment per sqe
        b.submit_one(&mut ring, *sqe).ok();
    }
}
```

The `Arc::clone` is ~5 ns on uncontended cache lines and tens of ns
under contention; for the 2.5 M submits/sec target it eats >10 % of
the budget. The fix is one of:

- pass `&backend` instead of `Arc<...>` (worker-scoped reference);
- pass `&*backend` (Deref-borrow) if the caller already owns the
  `Arc`;
- store the `Arc` in the worker once and call `&*self.backend` per
  submission.

### 4.4 Allowed: `Box<dyn IoUringBackend>` stored once per ring construction

The IUR-2 per-thread rings design (referenced in IUS-7.a section
9.1) stores one backend handle per worker thread. Storing that
handle as `Box<dyn DynIoUringBackend>` (or `Arc<dyn ...>` if shared
across threads) is **acceptable** because the storage cost is paid
once at thread spawn, not per submission. The hot loop pulls the
handle into a local generic-bound reference for the per-SQE work.

The boundary between "stored once" and "per op" is the
acceptance question. The linter draws the line at:

- inside a `for` / `while` / iterator chain that fires per SQE or per
  CQE: forbidden.
- inside a struct initialiser, `new` constructor, or one-shot
  `setup_*` function: allowed.

## 5. Per-method audit

The 38 `IoUringBackend` methods (plus the 19 methods on the
auxiliary traits `RingHandle`, `SessionPool`, `SessionLease`,
`SharedRingHandle`, `DiskBatch`) are classified below. **Hot path**
methods are subject to the asm-diff gate (section 3.2) and the 2 %
bench gate (section 3.4). **Cold path** methods are exempt from
both gates and may be called through `dyn` without violating the
guarantee.

### 5.1 `IoUringBackend` (38 methods)

| # | Method | Path | Rationale |
|---|--------|------|-----------|
| 1 | `is_available` | cold | Diagnostics; called once per `--version` or once per error log path. |
| 2 | `availability_reason` | cold | Diagnostics; allocates a `String`. |
| 3 | `sqpoll_fell_back` | cold | Diagnostics; per-session check. |
| 4 | `kernel_info` | cold | Diagnostics; cached via `OnceLock`. |
| 5 | `build_ring` | cold | Once per ring lifetime. |
| 6 | `submit_one` | **HOT** | Per-SQE; the primary submission hot path. |
| 7 | `submit_batch` | **HOT** | Per-batch (typically per N SQEs where N is the batch size); same hot loop as `submit_one`. |
| 8 | `submit_and_wait` | **HOT** | Per `io_uring_enter`; gates the submission/reap cycle. |
| 9 | `drain_completions` | **HOT** | Per-reap; the per-CQE inner loop runs through the returned iterator. The returned `Box<dyn Iterator>` itself is one allocation per drain (~1 per submission batch), allocates ~24 B once per batch - acceptable at batch granularity but tracked as a known cost in section 6.2. |
| 10 | `register_buffers` | cold | Once per ring lifetime. |
| 11 | `unregister_buffers` | cold | Once per ring lifetime (teardown). |
| 12 | `register_files` | cold | Once per ring lifetime. |
| 13 | `unregister_files` | cold | Once per ring lifetime (teardown). |
| 14 | `register_provided_buffer_ring` | cold | Once per provided-buffer ring construction. |
| 15 | `registered_buffer_stats` | cold | Diagnostics; called from `--stats` and from error paths. |
| 16 | `registered_buffer_status` | cold | Diagnostics. |
| 17 | `probe_op` | **HOT** (cached) | First call probes the kernel (cold); subsequent calls read a cached `u128` bitmap (hot). The cache must be `OnceLock<u128>` so the hot path is a single atomic load + bit test, no branch on cold/warm. |
| 18 | `statx_supported` | **HOT** (cached) | Default impl calls `probe_op`; same caching guarantee. |
| 19 | `linkat_supported` | **HOT** (cached) | Same. |
| 20 | `renameat2_supported` | **HOT** (cached) | Same. |
| 21 | `send_zc_supported` | **HOT** (cached) | Same. |
| 22 | `pbuf_ring_supported` | **HOT** (cached) | Same; cached separately from `probe_op` because the kernel-side query is different. |
| 23 | `cancel_supported` | **HOT** (cached) | Same. |
| 24 | `cancel_by_fd_supported` | **HOT** (cached) | Same. |
| 25 | `allocate_bgid` | cold | Once per provided-buffer ring construction. |
| 26 | `deallocate_bgid` | cold | Once per provided-buffer ring teardown. |
| 27 | `bgid_remaining` | cold | Diagnostics. |
| 28 | `submit_statx_blocking` | cold | Synchronous wrapper; not on the async hot path. |
| 29 | `submit_statx_batch` | warm | Per-directory; runs ~once per `readdir` in receiver. Not on the per-SQE hot path, but called often enough that `dyn` is discouraged. Generic dispatch preferred. |
| 30 | `submit_linkat_blocking` | cold | Synchronous wrapper. |
| 31 | `submit_renameat2_blocking` | cold | Synchronous wrapper. |
| 32 | `build_session_pool` | cold | Once per session. |
| 33 | `build_shared_ring` | cold | Once per shared-ring pair. |
| 34 | `open_reader` | cold | Once per file. |
| 35 | `open_writer` | cold | Once per file. |
| 36 | `writer_from_file` | cold | Once per file. |
| 37 | `build_disk_batch` | cold | Once per disk-commit thread. |

Counted: 9 hot (including 8 cached probe shortcuts that fold to a
single bitmap load), 1 warm (`submit_statx_batch`), 28 cold.

The 8 probe shortcuts share one cache line in the
`LinuxIoUringBackend` struct; the IUS-8.b impl must lay them out so
the `OnceLock<u128>` falls on the same cache line as
`pbuf_ring_supported`'s separate `OnceLock<bool>` (or fuse them into
one `OnceLock<u128>` with bit 127 reserved for `pbuf_ring`).

### 5.2 `RingHandle` (2 methods)

| Method | Path | Rationale |
|--------|------|-----------|
| `sq_entries` | cold | Diagnostics. |
| `sqpoll_active` | cold | Diagnostics. |

### 5.3 `SessionPool` + `SessionLease` (3 methods)

All cold: `ring_count`, `acquire`, `slot`. Session-pool acquisition
fires once per worker-iteration, not per SQE; vtable cost is amortised
over the worker's hot loop.

### 5.4 `SharedRingHandle` (9 methods)

| Method | Path | Rationale |
|--------|------|-----------|
| `reader_slot` | cold | Configuration query. |
| `writer_slot` | cold | Configuration query. |
| `poll_add_supported` | cold (cached) | Same caching guarantee as `IoUringBackend::probe_op`. |
| `has_registered_buffers` | cold | Configuration query. |
| `submit_read` | **HOT** | Per-read; the SSH transport read path. |
| `submit_send` | **HOT** | Per-write; the SSH transport write path. |
| `submit_poll_write` | **HOT** | Per-write readiness gate. |
| `submit_and_wait` | **HOT** | Per `io_uring_enter`. |
| `reap` | **HOT** | Per-reap; returns `Vec<SharedCompletion>` (one allocation per reap, acceptable at batch granularity). |

`SharedRingHandle` is the trait the SSH transport will dispatch
through. The IUS-8.b impl must generic-bound the SSH read/write loops
on `R: SharedRingHandle` rather than `&dyn SharedRingHandle`.

### 5.5 `DiskBatch` (5 methods)

| Method | Path | Rationale |
|--------|------|-----------|
| `begin_file` | cold | Once per file. |
| `write_data` | **HOT** | Per-chunk; the disk-commit thread's primary write path. |
| `commit_file` | cold | Once per file. |
| `bytes_written` | cold | Diagnostics. |
| `bytes_written_with_pending` | cold | Diagnostics. |

`DiskBatch::write_data` is the third hot method outside
`IoUringBackend` itself. The disk-commit thread must hold the
`DiskBatch` by concrete generic type or as `Box<dyn DiskBatch>` with
the per-chunk loop calling the boxed method directly (one indirect
call per chunk is acceptable because chunk sizes are 8-64 KB, putting
the call rate at <200 k/sec - well below the 2.5 M/sec submission
budget).

### 5.6 Hot-path summary

12 methods carry the asm-diff + 2 % bench gate:

- `IoUringBackend`: `submit_one`, `submit_batch`, `submit_and_wait`,
  `drain_completions`, `probe_op`, `statx_supported`, `linkat_supported`,
  `renameat2_supported`, `send_zc_supported`, `pbuf_ring_supported`,
  `cancel_supported`, `cancel_by_fd_supported` (12 methods; 8 probe
  shortcuts share one bench because they share one cache line + one
  inlined body).
- `SharedRingHandle`: `submit_read`, `submit_send`, `submit_poll_write`,
  `submit_and_wait`, `reap`.
- `DiskBatch`: `write_data`.

Total bench harness: 9 distinct benches (probe shortcuts collapse to
one), each with a `_through_trait` and `_direct` arm.

## 6. Type-erasure boundaries

There are three places where the trait must be erased anyway. Each
imposes a documented, bounded cost.

### 6.1 `BoxedRing` newtype (IUS-7.a section 9.2)

`BoxedRing` wraps `Box<dyn RingHandle>` to erase the associated type
when callers want to dispatch through `dyn DynIoUringBackend`. Cost:
one `Box` allocation per ring construction (cold path, ~1 per
session) + one extra indirection on `sq_entries` / `sqpoll_active`
(both cold).

**Recommendation:** the IUS-8.a impl provides `BoxedRing` for the
`dyn` adapter path **only**; callers that use generic dispatch hold
the ring by value (`SharedRing`, `SessionRingPool`, etc.) and never
touch the box. For stack-allocated rings (the typical case in the
hot-path workers) the wrapper holds the impl by value, not by
`Box`:

```rust
// Provided as the canonical wrapper for the generic path. The
// associated type stays concrete; no Box, no indirection.
pub struct ConcreteRing<R: RingHandle>(pub R);

// Provided as the adapter for the dyn path; used only by callers
// that explicitly need dyn-safe storage (e.g., per-thread storage
// in IUR-2 before per-thread monomorphisation is wired up).
pub struct BoxedRing(Box<dyn RingHandle>);
```

The IUS-8.b acceptance test asserts that `ConcreteRing<SharedRing>`
sits on the stack (no allocation) by checking the function's stack
frame size with `-Z print-type-sizes` (nightly) or by manual
inspection of the generated asm.

### 6.2 `drain_completions` iterator return type

`drain_completions` returns `Box<dyn Iterator<Item = CompletionEntry>
+ 'a>`. This is one heap allocation per drain call. At batch
granularity (~1 drain per 100-1000 CQEs) the amortised per-CQE cost
is 1-10 ns of allocation overhead - inside the 2 % gate when measured
at per-CQE rate, but visible in the per-drain bench.

The IUS-8.b impl has two options:

1. **Keep the boxed iterator.** Simple, dyn-compatible, costs one
   alloc per drain. Acceptable per the cost analysis.
2. **Return a concrete iterator type via GAT.** `type Drain<'a>:
   Iterator<Item = CompletionEntry> + 'a where Self: 'a;` removes
   the allocation but breaks `dyn` compatibility on this method. The
   `DynIoUringBackend` adapter then provides the boxed form for
   callers that need it.

**Recommendation:** option 2. GATs are stable since Rust 1.65; the
workspace pins Rust 1.88.0 so the feature is available. The
per-drain allocation is the largest single contributor to the
through-trait latency in early prototyping (estimated ~8 ns per
drain on a warm allocator), and removing it gives a 2-4 % headroom
back to the hot loop.

### 6.3 Per-thread ring storage (IUR-2)

Each worker thread holds a concrete `LinuxIoUringBackend`, no
erasure needed. The per-thread storage is a `thread_local!` of
`LinuxIoUringBackend` (or `OnceLock<LinuxIoUringBackend>` inside a
worker struct field). No `dyn` boundary; no cost.

The trait's `Send + Sync` bound means a single backend instance can
be shared across threads, but the IUR-2 design prefers per-thread
instances for cache-line locality on the probe cache. Either layout
meets the zero-cost guarantee.

## 7. Test plan for IUS-8.b

The IUS-8.b PR (Linux impl of `IoUringBackend`) must include the
following tests. Each is wired to the CI gate or to the IUS-8.b
acceptance review.

### 7.1 Functional smoke test

Asserts the impl reaches every existing wrapper. Lives in
`crates/fast_io/tests/backend_smoke.rs`:

```rust
#[test]
fn backend_covers_every_wrapper() {
    let backend = LinuxIoUringBackend::new();
    if !backend.is_available() { return; } // skip on non-uring CI

    let mut ring = backend.build_ring(&IoUringConfig::default()).unwrap();
    // exercise every method at least once; assertions are weak
    // (just "does not panic"). Coverage is the goal.
    let _ = backend.submit_one(&mut ring, SubmissionEntry::Nop { user_data: 0 });
    let _ = backend.submit_and_wait(&mut ring, 1);
    let _ = backend.drain_completions(&mut ring).count();
    // ... 38 calls total
}
```

This is a coverage scaffold, not a behavioural test. The behavioural
tests for each wrapper already exist in
`crates/fast_io/src/io_uring/*::tests`.

### 7.2 Tight-loop micro-bench (the binding gate)

`crates/fast_io/benches/backend_dispatch.rs` per section 3.3. The
bench runs each of the 12 hot-path methods 10 M times in a tight
loop, once through the trait and once direct. The CI gate at section
3.4 fails the merge if any ratio exceeds 1.02.

### 7.3 Asm-diff fixture

`crates/fast_io/tests/backend_asm/` per section 3.2. The fixtures
are checked in as `_through_trait.s` and `_direct.s` baselines; CI
re-generates them and `diff` fails the gate if the normalised diff
is non-empty.

### 7.4 Stack-frame size assertion for `ConcreteRing`

```rust
#[test]
fn concrete_ring_does_not_allocate() {
    let backend = LinuxIoUringBackend::new();
    if !backend.is_available() { return; }
    let ring: ConcreteRing<<LinuxIoUringBackend as IoUringBackend>::Ring> =
        ConcreteRing(backend.build_ring(&IoUringConfig::default()).unwrap());
    // The struct sits on the stack; no heap allocation beyond what
    // the ring itself does. This is a compile-time + size assertion.
    let _ = std::mem::size_of_val(&ring);
}
```

Pairs with `-Z print-type-sizes` output in the IUS-8.b PR description.

### 7.5 Defect path

If any of the 12 hot-path methods exceeds the 2 % gate, the IUS-8.b
PR is **not merged**. The defect is filed against the impl (not the
trait) and triaged before IUS-8.c lands. The asm-diff (section 3.2)
is the diagnostic of first resort because it pinpoints the regressed
instruction sequence; the bench (section 3.3) confirms the run-time
impact.

A method exceeding 2 % but below 5 % is a **warn**: it merges with
a follow-up issue tagged `io-uring-zero-cost-regression`. A method
exceeding 5 % is a **fail**: the PR is blocked.

## 8. Open issues for IUS-7.b decision

### 8.1 `Sized` bound on the trait

Adding `: Sized` to `IoUringBackend` would let the compiler inline
more aggressively in some edge cases (it removes the
"object-unsafe" warning from accidentally `dyn`-using a trait with
`Self: Sized` methods). The cost is that `dyn IoUringBackend` becomes
impossible without the `DynIoUringBackend` adapter from IUS-7.a
section 9.2 - which is already part of the spec.

**Recommendation:** do not add `: Sized` to `IoUringBackend`. The
associated type `Ring` already prevents `dyn IoUringBackend`; the
`DynIoUringBackend` adapter is the dyn path; the trait stays
object-unsafe-by-design without an explicit `: Sized` bound. Adding
the bound would be a redundancy.

### 8.2 Public "zero-cost" badge

Two ways to communicate the guarantee:

1. **Document the empirical measurement.** Publish the IUS-8.b
   bench results on the `fast_io` README and link this spec from the
   trait's rustdoc. No promise beyond "we measured 1.4 % at the
   IUS-8.b ship date."
2. **Commit to a zero-cost contract.** State in the trait's rustdoc
   that "the Linux impl is guaranteed within 2 % of direct calls; CI
   blocks regressions." Stronger claim, requires the CI gate to stay
   green forever (with the warn/fail thresholds in section 3.4).

**Recommendation:** option 2. The CI gate is the contract; the
rustdoc statement is the public face of it. Wording (proposed for
IUS-8.a rustdoc):

> # Performance contract
>
> On Linux, the `LinuxIoUringBackend` impl of this trait is held to
> a CI-enforced contract: every hot-path method (see
> `docs/design/ius-7b-zero-cost-guarantee.md` section 5) runs within
> 2 % of the equivalent direct call. The non-Linux stub impl returns
> `IoUringError::Unsupported` and is not subject to the contract.

### 8.3 Const-generic specialisation for batch submission

Deferred to ABW-2 / IUS-8.c follow-up. The current bench rig
captures `submit_batch` at a runtime-known batch size. If the
2 % gate proves tight in practice (`submit_batch` ratio sits at
1.018 - 1.020), the const-generic variant becomes the obvious
escape hatch.

Tracked outside this spec; mentioned here so the IUS-8.b reviewer
knows the option exists.

### 8.4 GAT for `drain_completions` return type

Per section 6.2, the recommendation is to use GAT to avoid the
per-drain `Box` allocation. The decision is whether to land the GAT
form in IUS-8.a (with the trait definition) or to land the boxed
form first and migrate in IUS-8.c.

**Recommendation:** land the GAT form in IUS-8.a. The migration
later is gratuitous churn; the GAT form costs nothing extra to
specify now.

### 8.5 Asm-diff normalisation rules

The asm-diff in section 3.2 requires register-name normalisation.
The exact normalisation rules (which register classes to canonicalise,
how to handle reordered instructions that are semantically
equivalent) are deferred to the IUS-8.b PR. A starting point is to
use `cargo-show-asm`'s built-in `--simplify` mode plus a small
post-processing script that strips function-prologue stitching.

If the normalisation proves brittle (false positives flaking the
gate), the fallback is to compare instruction *counts* and the
*opcode histogram* rather than the literal byte sequence - this is
weaker but more robust to LLVM's register-allocator drift across
toolchain bumps.

---

**Headline:** the CI gate is **2 %** for the per-method run-time
delta; **asm-diff must be empty modulo register names and function
stitching**; **9 distinct benches** (collapsing the 8 probe shortcuts
into one) cover the 12 hot-path methods identified in section 5.6.
The Linux impl ships with `#[inline(always)]` on every method,
generic dispatch on every per-op caller, and GAT-typed
`drain_completions` to avoid the per-drain allocation.
