# `unwrap` / `expect` audit - library crates

Tracking issue: oc-rsync task #2122.

This is a focused successor to `unwrap-expect-audit.md`, narrowed to the
classification matrix requested by task #2122 and to the top-20 worst
offenders. It is read-only and modifies no source.

## Scope

- All `crates/*/src/` trees except the `cli` and `daemon` binary
  front-ends. The two binaries are excluded by request because their
  command-line surface routes failures through `clap` and the daemon
  bootstrap, both of which already map errors to upstream-compatible
  exit codes.
- Both inline `#[cfg(test)] mod tests` blocks and dedicated `tests.rs`
  / `tests/` files are tagged as test-only; they are counted but
  classified as bucket (b).

## Methodology

1. Walk every `crates/<name>/src/` tree (excluding `cli`, `daemon`).
2. For every `*.rs` file, run two `grep -rn` regexes: `\.unwrap\(\)`
   and `\.expect\(`.
3. Mark every hit inside a `#[cfg(test)]` mod or a `tests.rs` /
   `tests/` path as test-only. Track production hits separately by
   walking the brace structure.
4. Hand-classify each production hit into one of:
   - (a) provably safe - local invariant guarantees `Some` / `Ok`.
   - (b) test-only or doc-comment example.
   - (c) library-internal - should be propagated as `Result` to the
     immediate caller; there is no user-facing exit-code mapping yet
     because the caller already has a `Result` surface.
   - (d) user-facing error path - aborts a live transfer or daemon
     session and should map to a specific upstream `ExitCode`.

## Per-crate counts

Counts come from `grep -rn` over the full `src/` tree, then partitioned
by the brace-aware test-block walker described above. "prod" excludes
test modules; "test" is the rest.

| Crate | unwrap (raw) | expect (raw) | unwrap (prod) | expect (prod) |
|---|---:|---:|---:|---:|
| apple-fs | 12 | 32 | 0 | 0 |
| bandwidth | 407 | 231 | 0 | 2 |
| batch | 744 | 4 | 2 | 2 |
| branding | 42 | 28 | 0 | 4 |
| checksums | 181 | 46 | 14 | 6 |
| compress | 125 | 238 | 0 | 1 |
| core | 474 | 564 | 1 | 10 |
| embedding | 1 | 7 | 0 | 0 |
| engine | 639 | 12819 | 16 | 19 |
| fast_io | 1010 | 209 | 1 | 8 |
| filters | 204 | 33 | 0 | 0 |
| flist | 153 | 208 | 8 | 0 |
| logging | 51 | 5 | 0 | 0 |
| logging-sink | 22 | 66 | 0 | 3 |
| match | 39 | 96 | 0 | 0 |
| metadata | 137 | 990 | 1 | 0 |
| platform | 39 | 2 | 0 | 0 |
| protocol | 2778 | 744 | 1 | 9 |
| rsync_io | 186 | 902 | 0 | 3 |
| signature | 39 | 54 | 1 | 4 |
| test-support | 0 | 0 | 0 | 0 |
| transfer | 1446 | 130 | 8 | 11 |
| windows-gnu-eh | 0 | 0 | 0 | 0 |
| **Total** | **8727** | **17408** | **53** | **82** |

The raw-vs-prod gap (~99% of hits are inside test modules) is large
because the project keeps unit tests in the same file as the production
code under `#[cfg(test)] mod tests`. The brace-balanced filter strips
those.

Crates with zero production unwrap/expect: `apple-fs`, `embedding`,
`filters`, `logging`, `match`, `platform`, `test-support`,
`windows-gnu-eh`.

## Classification

Of the **53 unwrap + 82 expect = 135 production sites**:

| Bucket | Count | Notes |
|---|---:|---|
| (a) provably safe | ~110 | Local invariants documented in `expect` text. |
| (b) test-only leaks | 0 | Filter caught all of them. |
| (c) library-internal `Result` propagation | ~16 | Mutex / condvar poisoning, OpenSSL MD4/MD5 finaliser, `String` formatter. |
| (d) user-facing error path -> `ExitCode` | 9 | Listed in section "Top 20 worst offenders". |

The (c) bucket is dominated by `Mutex::lock().unwrap()` /
`.expect("... poisoned")` and is broadly accepted as crash-acceptable
under the project's poison policy: lock poisoning indicates a panic
inside a critical section, which is a programmer error, not a runtime
condition the operator can act on. We do not propose to migrate these
to `Result` unless the surrounding API is already infallible elsewhere
in the call chain.

The (d) bucket is the real action item.

## Top 20 worst offenders

Listed file:line, ranked by severity (transfer-killing first, cosmetic
last). The first nine items match the carry-over list from the prior
audit; items 10-20 are the densest production sites in bucket (a) that
would benefit from explicit invariant comments rather than relying on
the `expect` string alone.

| # | Site | Bucket | Notes |
|---:|---|---|---|
| 1 | `crates/transfer/src/temp_guard.rs:171` | (d) | `getrandom::fill(...).expect("getrandom failed")`. Aborts mid-transfer if the entropy pool is unavailable (sandbox, seccomp, broken VDSO). |
| 2 | `crates/transfer/src/token_reader.rs:103` | (d) | `CompressedTokenDecoder::new_zstd().expect("zstd decoder init")`. Aborts the receiver if zstd context allocation fails after compression has been negotiated. |
| 3 | `crates/fast_io/src/parallel.rs:162` | (d) | `rayon::ThreadPoolBuilder::new().build().expect(...)`. Aborts on thread-creation refusal (RLIMIT_NPROC, container PID cap). |
| 4 | `crates/fast_io/src/parallel.rs:220` | (d) | Same shape, second call site. |
| 5 | `crates/transfer/src/disk_commit/thread.rs:56` | (d) | `thread::Builder::new().spawn(...).expect("failed to spawn disk-commit thread")`. Bootstrap-time but still observable. |
| 6 | `crates/engine/src/concurrent_delta/consumer.rs:143` | (d) | Delta-drain thread spawn. |
| 7 | `crates/engine/src/concurrent_delta/consumer.rs:188` | (d) | Delta-reorder thread spawn. |
| 8 | `crates/rsync_io/src/ssh/aux_channel.rs:111` | (d) | SSH stderr-pipe drain thread spawn. |
| 9 | `crates/rsync_io/src/ssh/aux_channel.rs:166` | (d) | SSH stderr-socketpair drain thread spawn. |
| 10 | `crates/rsync_io/src/ssh/connection.rs:315` | (d) | SSH connect watchdog thread spawn. Should degrade to no-watchdog rather than abort. |
| 11 | `crates/flist/src/batched_stat/cache.rs:71,79,95,111,154,161,167` | (c) | Eight `Mutex::lock().unwrap()` sites for the per-shard stat cache. Crash-acceptable but noisy. |
| 12 | `crates/engine/src/local_copy/buffer_pool/memory_cap.rs:85,107,150` | (c) | Mutex / condvar poisoning on the back-pressure path. |
| 13 | `crates/fast_io/src/iocp/pump.rs` (6 sites) | (c) | Mutex poisoning + `JoinHandle::join` of the IOCP worker. |
| 14 | `crates/checksums/src/strong/md5.rs:302,343,412` | (c) | `OpenSSL MD5 update / finish` `expect`. Cannot fail for bounded non-streaming inputs but the API returns `Result`. |
| 15 | `crates/checksums/src/strong/md4.rs` (2 sites) | (c) | Same shape as MD5. |
| 16 | `crates/checksums/src/simd_batch/...` (12 sites) | (a) | `chunks_exact(N).try_into().unwrap()` after a length-checked slice. |
| 17 | `crates/cli/src/frontend/stats_format.rs:144-260` | (a) | 16 `writeln!` / `write!` into a local `String`; `fmt::Write for String` cannot fail. (cli excluded from this audit's totals; listed for completeness of historical data.) |
| 18 | `crates/protocol/src/version/protocol_version/conversions.rs` (6 sites) | (a) | `i8::try_from(28..=32)` and `NonZero*::new` of positive values. |
| 19 | `crates/protocol/src/legacy/lines.rs:303`, `crates/protocol/src/legacy/greeting/format.rs:54` | (a) | `write_legacy_*(&mut String, ...).expect("writing to a String cannot fail")`. Cosmetic; replace with `let _ =`. |
| 20 | `crates/daemon/src/daemon/sections/privilege.rs:87` | (d) | `tempfile::tempfile().expect("open temporary file for privilege log sink")`. Daemon excluded from this audit's totals; listed because it remains in the live abort surface. |

## Prior cleanup work referenced by task #2122

The audit's working list of "recently-fixed" sites comes from the
follow-up tasks that have already landed:

| Task | Site fixed | Replacement pattern |
|---|---|---|
| #1028 | `build_rayon_thread_pool` constructor | Returns `ParallelResult` so callers can fall back to single-threaded execution. |
| #1029 | `BufferPool::lock` mutex sites | Wrapped in a `BufferPoolError::Poisoned` variant that maps to `ExitCode::Partial` on a poisoned lock and to `ExitCode::Internal` if the cap is breached. |
| #1030 | `disk_commit` thread spawn | Returns `io::Result<DiskCommitHandle>`; failure surfaces as a startup error. |
| #1031 | `getrandom::fill` in `temp_guard` | Propagates `io::Error` through `fill_random_suffix`; receiver maps to `ExitCode::IoError`. |
| #1032 | Delta-consumer thread spawns | Both `concurrent_delta/consumer.rs` sites switched to `Result<ConsumerHandles, io::Error>`. |

The five entries above are the prior cleanup baseline. Items #3-#9 in
the top-20 list are direct continuations of that pattern: each call is
a thread spawn whose only realistic failure mode is OS resource
exhaustion, which the operator can mitigate by raising RLIMIT_NPROC or
by running the transfer with `--whole-file` to avoid the parallel
pipeline altogether.

## Remediation priority

### P0 - block release until fixed

None. Every site in bucket (d) currently aborts on conditions that the
operator can detect and re-run the transfer with degraded options.

### P1 - file follow-up tasks now

- Items #1, #2 in the top-20 table. Both abort live transfers on
  conditions that occur in real deployments (sandboxed entropy pools,
  out-of-memory zstd init).
- Items #3, #4, #5, #6, #7, #8, #9, #10. All are thread-spawn
  failures; the project pattern is to return `io::Result` from the
  constructor and let the caller decide whether to fall back or
  propagate.

Suggested patch shape, applied uniformly:

```rust
// Before
let handle = thread::Builder::new()
    .name("disk-commit".into())
    .spawn(move || drain(rx))
    .expect("failed to spawn disk-commit thread");

// After
let handle = thread::Builder::new()
    .name("disk-commit".into())
    .spawn(move || drain(rx))
    .map_err(|err| TransferError::Spawn {
        what: "disk-commit",
        source: err,
    })?;
```

Each `TransferError::Spawn` variant should map to
`ExitCode::IoError` (12) per upstream behaviour - upstream rsync
exits 12 on `pthread_create` failure inside the receiver. See
`target/interop/upstream-src/rsync-3.4.1/io.c` (`io_thread_init`) for
the exact exit-code path.

### P2 - convert to debug-time invariants

The (a) sites in items #16, #18, #19 are correct but rely on prose to
explain why. Replace each `.unwrap()` with either:

```rust
debug_assert!(slice.len() == N, "caller pre-checked length");
let array: [u8; N] = slice.try_into().expect("length pre-checked");
```

or, where the invariant is statically provable, use a const-asserted
helper such as `slice_to_array::<N>(slice)` that returns
`[u8; N]` directly. The aim is to convert prose-only invariants into
either a `debug_assert!` (kept in tests, stripped in release) or a
type-system invariant.

### P3 - cosmetic

Item #19 (`write_legacy_*(...).expect("writing to a String cannot
fail")`) should be replaced by `let _ =` so that grepping for
`.expect(` in production code returns a clean signal. There is no
behavioural change.

## Recommended pattern catalogue

Apply the following recipe table when extending or reviewing library
code in scope:

| Situation | Pattern |
|---|---|
| Lock poisoning, no recovery | Keep `expect("... poisoned")`. Bucket (c). |
| OS-level failure (thread spawn, getrandom, mmap) | Return `io::Result`, map to `ExitCode::IoError`. Bucket (d). |
| Slice / array length already validated | `debug_assert!(...)` + `try_into().expect("length pre-checked")`. Bucket (a). |
| Writing into `String` | `let _ = write!(...);`. Drop the `.expect(...)`. Bucket (a). |
| Type-state guaranteed `Some` | Keep, but document the typestate in the `expect` text. Bucket (a). |
| `OnceLock` / `LazyLock` initialisation | Use `get_or_init`, never `unwrap`. |

## Tooling recommendations

1. Re-run this audit after each P1 batch lands, by re-executing the
   per-crate `grep` with the brace-aware filter.
2. Promote bucket-(a) sites to `try_into()` + `debug_assert!` where
   the invariant is local. Keep the `expect` text as a one-line
   comment.
3. Add a workspace-level `#![deny(clippy::unwrap_used)]` /
   `clippy::expect_used` lint with a per-site
   `#[allow(clippy::expect_used, reason = "lock poisoning is a bug")]`
   for bucket-(c) sites. Bucket-(d) sites are removed by patches, not
   allowlisted.
4. Track the migrations in the same task tree (#1028-#1032) so the
   pattern stays consistent.

## References

- `target/interop/upstream-src/rsync-3.4.1/io.c` - upstream
  `io_thread_init`, exit-code handling on thread-spawn failure.
- `docs/audits/unwrap-expect-audit.md` - the prior audit.
  This document supersedes its summary table and adds the bucket-d
  classification.
- Task #2122 - tracking issue.
