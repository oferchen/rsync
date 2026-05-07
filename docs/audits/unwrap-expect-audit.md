# `unwrap` / `expect` call-site audit (library crates)

Tracking issue: oc-rsync task #2122.

## Summary

The project conventions forbid `unwrap` and `expect` on fallible paths in
production code. This audit enumerates every `.unwrap()` and `.expect(...)`
site that survives in library code under `crates/*/src/` after excluding
test files (`tests/` directories, `tests.rs`, `*_tests.rs`,
`test_support.rs`, `comprehensive_tests.rs`, `simd_parity_tests.rs`),
`#[cfg(...test...)]` modules and items, and rustdoc/comment lines. It
classifies each surviving site into one of three buckets - safe-by-construction,
crash-acceptable, or fixable - and lists the fixable ones in
priority order so they can be funnelled into follow-up work items.

The headline figure is **76 `unwrap` and 90 `expect` call sites** spread
across 17 crates. The vast majority are safe-by-construction or use a
panic on `Mutex`/`Condvar` poisoning, which the project tolerates as a
crash-acceptable invariant. A small set (described in section 4) should
be migrated to `Result`-returning APIs.

This audit is read-only. No source file is modified.

## Methodology

1. Walk every `crates/*/src/` tree and collect `*.rs` files.
2. Exclude any path containing a `tests/` segment, plus files whose name
   matches the well-known test patterns above.
3. For every remaining file, strip text spans guarded by
   `#[cfg(test)]`, `#[cfg(all(test, ...))]`, or
   `#[cfg(any(..., test, ...))]` (brace-balanced, including the
   attribute itself). This removes inline `mod tests { ... }` blocks
   that live next to production code.
4. Skip any line whose first non-whitespace characters are `//`,
   `///`, or `//!`. This filters rustdoc examples and ordinary
   comments.
5. Count `\.unwrap\(\)` and `\.expect\(` occurrences in what remains and
   record each site (crate, file, line, kind, source line).
6. Manually inspect every site to assign a category.

`apple-fs/src/lib.rs` and similar files contain `#[cfg(test)] mod tests`
blocks that surface a few `.unwrap()` matches in a naive grep; these are
correctly stripped here. The same applies to the substantial test
modules embedded inside `engine/src/local_copy/hard_links.rs`,
`engine/src/local_copy/buffer_pool/*`, `cli/src/frontend/...`, and so
on.

## Per-crate counts

| Crate | `unwrap` | `expect` |
|---|---:|---:|
| `apple-fs` | 0 | 0 |
| `bandwidth` | 0 | 2 |
| `batch` | 2 | 2 |
| `branding` | 0 | 4 |
| `checksums` | 14 | 6 |
| `cli` | 21 | 5 |
| `compress` | 0 | 1 |
| `core` | 1 | 10 |
| `daemon` | 2 | 3 |
| `embedding` | 0 | 0 |
| `engine` | 16 | 19 |
| `fast_io` | 1 | 8 |
| `filters` | 0 | 0 |
| `flist` | 8 | 0 |
| `logging` | 0 | 0 |
| `logging-sink` | 0 | 3 |
| `match` | 0 | 0 |
| `metadata` | 1 | 0 |
| `platform` | 0 | 0 |
| `protocol` | 1 | 9 |
| `rsync_io` | 0 | 3 |
| `signature` | 1 | 4 |
| `transfer` | 8 | 11 |
| `windows-gnu-eh` | 0 | 0 |
| **Total** | **76** | **90** |

Crates with **zero** `unwrap`/`expect` in production code: `apple-fs`,
`embedding`, `filters`, `logging`, `match`, `platform`,
`windows-gnu-eh`.

## Categorisation

Every surviving site falls into one of three buckets.

### (a) Safe-by-construction

The compiler cannot prove the invariant, but the local context does.
Examples:

- `NonZeroU8::new(<positive const>).unwrap()` and similar uses where
  the input is a literal or a value that has already been bounds-checked.
- `parts.next().expect("splitn on non-empty string yields at least one element")`
  and similar - `splitn` always yields one element.
- `last_error.expect("no addresses available for daemon connection")`
  inside a loop body that only exits via `break` after setting
  `last_error = Some(...)`.
- `RAII` guards (`BufferGuard::deref`,
  `LineModeGuard`) where the `Option` is `Some` for the entire lifetime
  of the guard and only `take()`-en in `Drop`.
- Type-state pattern where the typestate guarantees the field
  (`NegotiationPipeline<AlgorithmSelected>::selected_algorithm_name`).
- Index-driven iteration over a `Vec<Option<T>>` where each index is
  visited exactly once (incremental file-list assembly in
  `transfer/src/generator/file_list/inc_recurse.rs`).
- `chunks_exact(N).try_into().unwrap()` and `from_le_bytes` of a slice
  whose length has just been verified.
- Writing into a `String` with `writeln!`/`write!` - `fmt::Write` for
  `String` cannot fail.
- `ProtocolVersion` conversions: every supported protocol value (28-32)
  fits in `i8` and is non-zero, the `expect` text documents the
  invariant.
- `splitn`, `chars().next()` after `len()` checks.

This bucket covers roughly 90% of the surviving sites. The `expect`
text in each case names the local invariant.

### (b) Crash-acceptable

Panics that mirror the standard "lock poisoning is a bug we cannot
recover from" stance. The project policy explicitly tolerates this
pattern.

- `Mutex::lock().unwrap()` / `.expect("... poisoned")` throughout
  `engine/src/local_copy/buffer_pool/memory_cap.rs`,
  `engine/src/local_copy/context_impl/options.rs` and `state.rs`,
  `engine/src/local_copy/executor/directory/recursive/batch.rs`,
  `engine/src/concurrent_delta/work_queue/drain.rs`,
  `flist/src/batched_stat/cache.rs` (8 sites),
  `signature/src/async_gen.rs`,
  `fast_io/src/iocp/pump.rs` (4 sites).
- `Condvar::wait().expect("... poisoned")` in
  `engine/src/local_copy/buffer_pool/memory_cap.rs`.
- `JoinHandle::join().expect("... worker panicked")` in
  `fast_io/src/iocp/pump.rs` (2 sites). Joining a panicked thread
  legitimately re-raises the panic on the joiner.

These match upstream rsync's "if a worker died, propagate the failure"
semantics and the broader Rust convention for poison handling.

### (c) Fixable - should return `Result`

A small number of sites cause a hard process abort on operating-system
or runtime conditions that are not strictly programmer errors. These
should be migrated to `Result`-returning surfaces and bubbled up.
Section 4 lists them in priority order.

## Fixable sites, prioritised

Severity is judged by (i) how easily the failure mode can be triggered
in a deployed binary, (ii) how much state is lost when the process
aborts, and (iii) whether the abort happens after substantial transfer
work has already been performed.

### P1 - reachable runtime failure during a transfer

1. `crates/transfer/src/temp_guard.rs:171`
   `getrandom::fill(&mut random_bytes).expect("getrandom failed")`.
   `fill_random_suffix` is called once per temporary file. A
   `getrandom` failure (entropy pool unavailable, sandbox restriction,
   etc.) is rare but observable, and aborts mid-transfer. Migrate
   `fill_random_suffix` to `io::Result<String>` and propagate through
   `temp_guard`.

2. `crates/transfer/src/token_reader.rs:103`
   `CompressedTokenDecoder::new_zstd().expect("zstd decoder init")`.
   Aborts the receiver if zstd context creation fails. The function is
   already only called after compression has been negotiated; convert
   the constructor to `Result` and surface the error to the caller as
   a protocol-error exit code.

3. `crates/fast_io/src/parallel.rs:162` and `:220`
   `rayon::ThreadPoolBuilder::new().build().expect("failed to build rayon thread pool")`.
   Triggered if the OS refuses thread creation (RLIMIT_NPROC, container
   PID limit). Both call sites already return a typed `ParallelResult`;
   plumb a `Result` for the pool-construction path so callers can fall
   back to single-threaded execution rather than aborting.

### P2 - reachable runtime failure during startup or auxiliary I/O

4. `crates/transfer/src/disk_commit/thread.rs:56`
   `thread::Builder::new().spawn(...).expect("failed to spawn disk-commit thread")`.
   Same family as above; thread spawn failures should propagate as
   `io::Error` to the receiver bootstrap.

5. `crates/engine/src/concurrent_delta/consumer.rs:143` and `:188`
   Two thread-spawn `.expect(...)` for the delta-drain and
   delta-reorder threads. Same fix shape as P1#3.

6. `crates/rsync_io/src/ssh/aux_channel.rs:111` and `:166`
   `thread::Builder::new().spawn(...).expect("failed to spawn ssh stderr ... thread")`.
   Two SSH stderr-pump thread spawns. Same fix shape; the SSH
   transport already returns `io::Result` from its constructors.

7. `crates/rsync_io/src/ssh/connection.rs:315`
   `thread::Builder::new().spawn(...).expect("failed to spawn ssh connect watchdog thread")`.
   Same fix shape; the connect watchdog can be omitted (treated as a
   degraded-mode warning) if its thread cannot be spawned.

8. `crates/daemon/src/daemon/sections/privilege.rs:87`
   `tempfile::tempfile().expect("open temporary file for privilege log sink")`.
   Fallback path when `/dev/null` (or `NUL` on Windows) cannot be
   opened. Both failures simultaneously imply a severely broken host;
   still, the function should return `io::Result<SharedLogSink>` so
   the daemon can fall back to a no-op sink instead of aborting.

### P3 - cosmetic / rare

9. `crates/protocol/src/legacy/lines.rs:303` and
   `crates/protocol/src/legacy/greeting/format.rs:54`
   Two `write_legacy_*(&mut String, ...).expect("writing to a String cannot fail")`
   sites. Strictly safe-by-construction (writing into `String` cannot
   fail), but the pattern returns `fmt::Result`; the redundant `expect`
   can be replaced by `let _ =` to make intent explicit and stop these
   showing up in `.expect` greps.

## Sites covered by buckets (a) and (b)

For completeness, the safe-by-construction and crash-acceptable buckets
include but are not limited to:

- `crates/cli/src/frontend/stats_format.rs` lines 144-260: 16
  `writeln!`/`write!` into a local `String`. **Safe-by-construction.**
- `crates/cli/src/frontend/filter_rules/arguments.rs` lines 73, 76, 81,
  85: `pop_front().unwrap()` after `front()` matched `Some`.
  **Safe-by-construction.**
- `crates/cli/src/frontend/filter_rules/parsing/mod.rs:262`:
  `parts.next()` on `splitn`. **Safe-by-construction.**
- `crates/cli/src/frontend/execution/{compression.rs,options/numeric.rs,options/size.rs}`:
  `NonZeroU{8,64}::new(...)` of pre-validated values, `chars().next()`
  after non-empty check. **Safe-by-construction.**
- `crates/cli/src/frontend/execution/drive/options.rs:487` and
  `workflow/operands.rs:29`: parsing of compile-time-constant strings
  and exit-code lookups for fixed codes. **Safe-by-construction.**
- `crates/checksums/src/strong/{md4.rs,md5.rs}` (5 sites): the OpenSSL
  bindings declare `Result` but cannot fail for `update`/`finalize` on
  non-streaming inputs of bounded size; the `expect` text records this.
  **Safe-by-construction.** Could be tightened by replacing with
  `let _ =` plus a comment but is not a correctness defect.
- `crates/checksums/src/simd_batch/...` (12 sites): `chunks_exact`
  followed by `try_into()` to a fixed-length array.
  **Safe-by-construction.**
- `crates/core/src/version/metadata.rs:150`,
  `core/src/version/report/renderer.rs:237`,
  `core/src/message/numbers.rs:16,34`,
  `core/src/message/segments/io.rs:180`,
  `core/src/message/scratch.rs:121,130`,
  `core/src/client/config/enums/checksum.rs:101`,
  `core/src/client/module_list/connect/proxy.rs:188`,
  `core/src/client/module_list/connect/direct.rs:43`,
  `core/src/client/remote/embedded_ssh_transfer.rs:223`:
  all **safe-by-construction** (writing into `String`,
  ASCII decimal slices, `splitn`, sentinel-loop invariants,
  `from_utf8` on whitelisted byte ranges).
- `crates/protocol/src/version/protocol_version/conversions.rs` (6
  sites): `i8::try_from` of values in 28..=32 and `NonZero*::new` of
  positive values from `as_u8()`. **Safe-by-construction.**
- `crates/protocol/src/flist/dir_tree.rs:100` and
  `protocol/src/flist/read/mod.rs:531`: invariants enforced by the
  surrounding `if` and the abbreviated-follower flag respectively.
  **Safe-by-construction.**
- `crates/transfer/src/{constants.rs,receiver/mod.rs,receiver/file_list.rs,receiver/transfer/pipeline.rs}`:
  `chunks_exact` casts, `NonZeroU8::new` of positive constants,
  `last()`/`pop()` after a freshly checked depth invariant, and the
  initial-segment-always-present invariant in the file-list manager.
  **Safe-by-construction.**
- `crates/transfer/src/generator/{delta.rs,file_list/inc_recurse.rs,file_list/walk.rs,protocol_io.rs}`:
  `Some(...)` set immediately before `unwrap`, `take()` of indices
  visited exactly once, initial-segment invariant.
  **Safe-by-construction.**
- `crates/engine/src/concurrent_delta/reorder.rs:225,246,251`: function
  early-returns when `adaptive` is `None`; subsequent `expect("adaptive
  state present")` is a **safe-by-construction** invariant.
- `crates/engine/src/local_copy/dir_merge/parse/line.rs:227`:
  `chars().next()` inside `if keyword.len() == 1`.
  **Safe-by-construction.**
- `crates/engine/src/local_copy/executor/file/comparison.rs:61`,
  `executor/file/sparse/detect.rs:166`,
  `executor/directory/recursive/batch.rs:152,157,158`,
  `local_copy/context_impl/options.rs` (12 sites - mostly mutex
  poisoning, plus a few `Some(_)` => `as_mut().unwrap()` inside a
  `match` arm), `context_impl/state.rs:53,60`,
  `context_impl/delta_transfer.rs:145`: all
  **safe-by-construction** for the `Some` arms or **crash-acceptable**
  for the mutex sites.
- `crates/engine/src/local_copy/buffer_pool/{guard.rs,memory_cap.rs}`:
  RAII guards (safe-by-construction) plus mutex/condvar poisoning
  (crash-acceptable).
- `crates/flist/src/batched_stat/cache.rs` (8 sites): mutex
  poisoning. **Crash-acceptable.**
- `crates/flist/src/batched_stat/statx_support.rs:31`:
  `CString::new(".")` of a literal. **Safe-by-construction.**
- `crates/fast_io/src/iocp/pump.rs` (6 sites): mutex poisoning and
  `JoinHandle::join` of a worker. **Crash-acceptable.**
- `crates/fast_io/src/zero_detect.rs:117`: `chunks_exact(16)` cast.
  **Safe-by-construction.**
- `crates/metadata/src/mapping/name_mapping.rs:85`: closure-local
  `cached_name = Some(...)` then immediate `as_ref().unwrap()`.
  **Safe-by-construction.**
- `crates/signature/src/{layout.rs,generation.rs,parallel.rs}` (4
  sites) and `signature/src/async_gen.rs:340`: `NonZeroU{8,32}::new`
  of clamped values plus one mutex poison.
  **Safe-by-construction** / **crash-acceptable** respectively.
- `crates/logging-sink/src/sink/guard.rs` (3 sites): RAII line-mode
  guard invariants. **Safe-by-construction.**
- `crates/branding/src/{branding,workspace}/json.rs` (4 sites):
  `serde_json::to_string(...)` of a struct that the type system
  guarantees serialisable. **Safe-by-construction.** A documented
  invariant; could be replaced with `serde_json::to_value` then
  `to_string` of a `serde_json::Value` if we want to remove the
  panic pattern entirely.
- `crates/branding/src/branding/profile.rs`,
  `crates/branding/src/workspace/version.rs`,
  `crates/branding/src/branding/brand.rs`,
  `crates/branding/src/validation.rs`: all surfaced unwrap/expect
  matches are inside `#[cfg(test)]` blocks (already excluded by the
  audit script) or doc-comment examples (excluded by line-prefix
  filter). The remaining counts in the per-crate table reflect this.
- `crates/bandwidth/src/async_limiter.rs` lines 58, 123:
  `NonZeroU64::new(bytes_per_second).expect("bytes_per_second must be greater than zero")`.
  Public APIs that already document the precondition; both are reached
  only after the caller has clamped to a positive value.
  **Safe-by-construction.**
- `crates/batch/src/replay.rs:560,760,772` and
  `crates/batch/src/reader/flist.rs:83`: invariants visible from
  immediately preceding code (`is_empty` check, `Some(_)` arm of a
  match, header presence verified above). **Safe-by-construction.**
- `crates/daemon/src/daemon/sections/name_converter.rs:28,29`:
  `child.stdin.take().expect("stdin piped")` immediately after
  `Stdio::piped()`. **Safe-by-construction.**
- `crates/daemon/src/daemon/runtime_options/parsing.rs:60`:
  `NonZeroUsize::new(1).unwrap()` of the literal `1`.
  **Safe-by-construction.**
- `crates/daemon/src/daemon/async_session/listener.rs:63`:
  `"0.0.0.0:873".parse().unwrap()` of a constant. **Safe-by-construction.**
- `crates/compress/src/strategy/type_state.rs:237`: type-state
  invariant. **Safe-by-construction.**

## Recommendations

1. Open follow-up tasks for the nine P1/P2/P3 fixable sites in
   section 4. Each is a focused refactor that adds no new public API
   beyond returning `Result` instead of panicking.
2. Add a short comment header above each safe-by-construction site
   that does not already explain the invariant in its `expect` text,
   so future readers do not need to reconstruct it from context. The
   sites in `crates/cli/src/frontend/stats_format.rs` are the largest
   single cluster.
3. Consider a clippy lint allowlist (rather than file-level
   `#[allow(clippy::unwrap_used, clippy::expect_used)]`) that names
   each accepted site, so accidental reintroductions show up in CI.
4. Re-run this audit after each batch of P1/P2 fixes lands, by
   re-executing the script described in the Methodology section.
