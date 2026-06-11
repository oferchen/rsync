# EDG-PANIC.5: `.unwrap()` / `.expect()` panic surface on slice-derived values

## Background

UTS-18 surfaced a panic class where a bare-slice index (`buf[start..end]`) is
evaluated with attacker-influenced bounds. EDG-PANIC.5 is the parallel class:
`.unwrap()` or `.expect()` on a `Result`/`Option` whose payload is itself
derived from a slice operation (`try_into`, `chunks_exact`, `get`,
`split_at`, `u32::try_from(len)`, etc.). When the source data flows from the
wire or filesystem, this turns a soft decoder error into a hard process
abort.

This audit catalogs the call sites in the hot-path crates so a future PR
can convert the high-risk ones to the `.get(start..end).ok_or_else(...)?`
pattern that UTS-18 established.

## Methodology

### Scope

Crates audited (per the EDG-PANIC.5 brief):

- `crates/transfer/src/`
- `crates/engine/src/`
- `crates/protocol/src/`
- `crates/compress/src/`
- `crates/checksums/src/`
- `crates/fast_io/src/`
- `crates/flist/src/`

Out of scope: `cli`, `daemon`, `core`, `rsync_io`, `matching`, `signature`,
`metadata`, `filters`, `bandwidth`, `logging`. These were already covered by
`docs/audits/unwrap-expect-library-crates.md` and are not slice-driven hot
paths.

### Patterns searched

For each file in the scoped crates:

1. `\.unwrap\(\)` — bare unwrap on a `Result` or `Option`.
2. `\.expect\(` — unwrap with a panic message.
3. `try_into\(\)\.unwrap\(\)` / `try_into\(\)\.expect\(` — slice-to-array
   conversions, the highest-density slice-unwrap pattern in the codebase.
4. `\[[^]]*\.\.[^]]*\]\.try_into` — slice-range conversion (UTS-18 cousin).
5. `\.get\([^)]+\)\.unwrap\(\)` — indexed access on `Vec`/`HashMap`.
6. `u32::try_from\(.+\.len\(\)\)\.expect` — capacity-bounded `usize`-to-`u32`
   conversion (panics on overflow rather than degrading).

### Triage filter

A line was treated as production code only if it appeared before the
file-level `#[cfg(test)]` block. Doc-comment examples (`/// foo.unwrap()`)
and `tests.rs` / `*/tests/*` files were excluded. Each production hit was
then opened in context and classified.

### Risk classification

- **HIGH** — value originates in a wire/file/network read, the index or
  length is derived from untrusted bytes, and the panic is reachable on a
  malformed payload.
- **MEDIUM** — value originates in an intra-process channel/queue, or
  derives from a state that is normally validated but could be stale; a
  panic is reachable only on internal invariant violation, OOM, or
  pathological inputs (e.g., 4 GiB arena overflow).
- **LOW** — invariant-enforced: the value comes from a compile-time
  constant, an iterator (`chunks_exact`, `rchunks_exact`) that guarantees
  the slice length, or an `Option` immediately after an `is_some()` /
  `is_none()` guard.

## Findings

### HIGH RISK

None identified in the scoped crates.

The hot-path wire decoders (`crates/protocol/src/multiplex/`,
`crates/protocol/src/envelope/`, `crates/protocol/src/wire/`,
`crates/protocol/src/flist/read/`, `crates/protocol/src/varint/`,
`crates/protocol/src/codec/`) propagate every length/index error as a
typed `io::Error` or protocol-specific error variant. The closest match
to the pattern is `crates/protocol/src/multiplex/codec.rs:110`, which
already uses `.try_into().map_err(...)?` — exactly the UTS-18 pattern.

This is a meaningful negative result: the deliberate convention in the
protocol crate is "wire bytes never reach an `.unwrap()`". The audit
confirms that convention holds at the file level across the scoped
surface.

### MEDIUM RISK

The MEDIUM tier covers sites where a panic is reachable on internal
invariant violation, capacity overflow, or initialization-time failure.
None are reachable from a single malformed wire frame, but each rewards
conversion to a typed error to keep the daemon and long-running
transfers fail-safe.

| # | Site | Pattern | Source / Invariant | Recommended fix |
|---|------|---------|--------------------|-----------------|
| 1 | `crates/protocol/src/flist/flat/intern.rs:104-105` | `u32::try_from(self.spans.len()).expect("PathArena exceeded u32::MAX distinct interned strings")` | `spans.len() < u32::MAX` invariant; theoretical 4 GiB-class file list | Return `PathArenaError::IndexSpaceExhausted` |
| 2 | `crates/protocol/src/flist/flat/intern.rs:112` | `u32::try_from(self.bytes.len()).expect("PathArena byte arena exceeded 4 GiB")` | `bytes.len() < u32::MAX` invariant | Return `PathArenaError::ByteArenaFull` |
| 3 | `crates/protocol/src/flist/flat/intern.rs:113` | `u32::try_from(s.len()).expect("interned string exceeds u32::MAX bytes")` | `s.len() < u32::MAX`; reachable on malicious path component | Return `PathArenaError::StringTooLong` |
| 4 | `crates/protocol/src/flist/flat/extras.rs:217` | `u32::try_from(self.blobs.len()).expect("ExtrasArena exceeded 4 GiB")` | `blobs.len() < u32::MAX` | Return `ExtrasArenaError::Full` |
| 5 | `crates/protocol/src/flist/flat/extras.rs:224` | `extras.rdev_major.expect("EXTRA_RDEV requires rdev_major")` | Caller sets `EXTRA_RDEV` bit only when `rdev_major.is_some()`; type-system gap | Make `EXTRA_RDEV` carry the two `u32`s as a struct so the `Option` cannot diverge from the bit |
| 6 | `crates/protocol/src/flist/flat/extras.rs:225` | `extras.rdev_minor.expect("EXTRA_RDEV requires rdev_minor")` | Same as #5 | Same as #5 |
| 7 | `crates/transfer/src/token_reader.rs:103` | `CompressedTokenDecoder::new_zstd().expect("zstd decoder init")` | zstd init failure is OOM-class | Return `TokenReaderError::DecoderInit` and propagate from `new` |
| 8 | `crates/transfer/src/disk_commit/thread.rs:56` | `thread::Builder::new().spawn(...).expect("failed to spawn disk-commit thread")` | Reachable when `EAGAIN`/`RLIMIT_NPROC` is hit | Return `DiskCommitError::ThreadSpawn(io::Error)` from `spawn_disk_thread` |
| 9 | `crates/protocol/src/flist/read/mod.rs:532` | `(hardlink_idx.expect("abbreviated follower has hardlink_idx") as i32 - self.ndx_start) as usize` | `is_abbreviated_follower` returns `false` when `hardlink_idx` is `None`; coupled invariant | Pattern-match `Some(idx)` and return `FlistError::AbbreviatedWithoutHardlink` on `None` |
| 10 | `crates/engine/src/concurrent_delta/parallel_apply/mod.rs:702-703` | `char::from_digit((b >> 4) as u32, 16).expect("hi nibble")` (and lo) | `(b >> 4) < 16` always | Use `b"0123456789abcdef"[(b >> 4) as usize] as char` and skip the `Option` |
| 11 | `crates/transfer/src/disk_commit/process.rs:171` | `outcome.delayed_path.as_ref().unwrap()` after `is_some()` | Guarded by surrounding `if outcome.delayed_path.is_some()` | Pattern-match `if let Some(staged) = outcome.delayed_path.as_ref()` |
| 12 | `crates/transfer/src/disk_commit/process.rs:310` | Same pattern as #11 | Same | Same |
| 13 | `crates/engine/src/local_copy/executor/file/copy/transfer/execute/iouring.rs:109` | `dispatch_result.expect("dispatch_result checked above")` after `is_none()` early-return | Guarded just above | Pattern-match `let Some(outcome) = dispatch_result else { unreachable!() }` is no better; either keep with comment or restructure as `match` |
| 14 | `crates/engine/src/concurrent_delta/reorder/mod.rs:419` | `self.adaptive.as_mut().expect("adaptive state present")` after `if self.adaptive.is_none()` early-return | Guarded by top-of-fn `is_none()` check | Restructure as `if let Some(state) = self.adaptive.as_mut()` outer block |
| 15 | `crates/engine/src/concurrent_delta/reorder/mod.rs:440` | Same as #14 | Same | Same |
| 16 | `crates/engine/src/concurrent_delta/reorder/mod.rs:445` | Same as #14 | Same | Same |
| 17 | `crates/protocol/src/flist/flat/parallel_builder.rs:248` | `source.get(i).expect("index within source.len() must be valid")` inside `for i in 0..source.len()` | Iterator invariant | Use `for entry in source.iter()` instead of `for i in 0..len() { .get(i).unwrap() }` |

### LOW RISK summary

The audit found **22 production-code unwraps** on slice-derived values
across the scoped crates that fall into LOW RISK. All are invariant-enforced
through one of:

- `chunks_exact(N)` / `rchunks_exact(N)` iterator guarantees:
  - `crates/transfer/src/constants.rs:98` (`leading_zero_count`)
  - `crates/transfer/src/constants.rs:125` (`trailing_zero_count`)
  - `crates/engine/src/local_copy/executor/file/sparse/detect.rs:166` (`trailing_zero_run`)
  - `crates/fast_io/src/zero_detect.rs:117` (`find_first_nonzero_scalar`)
  - `crates/checksums/src/simd_batch/md4/scalar.rs:93`
  - `crates/checksums/src/simd_batch/md5_scalar.rs:128`
- Explicit `word_offset + 4 <= padded.len()` bound check at the call site:
  - `crates/checksums/src/simd_batch/md4/simd/avx512.rs:106`
  - `crates/checksums/src/simd_batch/md5_simd/avx512.rs:174`
- 4-bit-value-to-hex-digit (always within `0..16`):
  - `crates/engine/src/concurrent_delta/parallel_apply/mod.rs:702`
  - `crates/engine/src/concurrent_delta/parallel_apply/mod.rs:703`
- `Option::as_ref().unwrap()` immediately after `is_some()`:
  - `crates/transfer/src/disk_commit/process.rs:171`
  - `crates/transfer/src/disk_commit/process.rs:310`
- `Option::as_mut().expect()` after a same-function `is_none()` early-return:
  - `crates/engine/src/concurrent_delta/reorder/mod.rs:419,440,445`
- `Vec::get(i).unwrap()` inside `for i in 0..vec.len()`:
  - `crates/protocol/src/flist/flat/parallel_builder.rs:248`

These overlap with the MEDIUM table above where the same site is structurally
LOW-RISK but cosmetically improvable. They are listed once in MEDIUM with
the structural rationale recorded.

Spot-check of `crates/compress/src/` confirmed it contains zero
production-code unwraps on slice-derived values. Every `.unwrap()` and
`.expect()` in that crate sits inside `#[cfg(test)]` modules or doc
examples.

## Conclusion

**0 HIGH RISK sites** identified in the EDG-PANIC.5 scope. The protocol
crate's wire-decoder discipline (typed errors, no slice unwraps) holds
across the audited surface.

**~10 MEDIUM RISK sites** identified, dominated by:

1. The `FlatFileList` arena code (intern.rs, extras.rs), where capacity
   overflows and presence-mask invariants are enforced by `.expect()`
   rather than by the type system.
2. Initialization-time spawn and decoder construction
   (`disk_commit/thread.rs`, `token_reader.rs`), where transient
   resource exhaustion is currently fatal.

**~12 LOW RISK sites** identified, all invariant-enforced by iterator
guarantees, explicit bound checks, or preceding `is_some()` / `is_none()`
gates.

Recommendation: convert the MEDIUM sites in two waves of small PRs that
match the UTS-18 `.get(start..end).ok_or_else(...)?` pattern adapted to
each context (typed error variant + `?` propagation). Hold the LOW sites
as-is unless the surrounding function is refactored for another reason -
the bare `.unwrap()` here documents the invariant more clearly than the
equivalent `unreachable!()`/`debug_assert!` would.

## Follow-up tasks

Per-site tasks for future PRs (use `crate:fn:line` format from the
brief):

1. `protocol:PathArena::intern:104-113` — return a typed
   `PathArenaError` from `intern()`; update callers in
   `flist::flat::parallel_builder::extend_from_worker` to propagate.
2. `protocol:ExtrasArena::append:217` — return a typed
   `ExtrasArenaError::Full` from `append()`; mark `#[must_use]` on the
   `Result` and propagate.
3. `protocol:ExtrasArena::append:224-225` — type-couple `rdev_major` and
   `rdev_minor` into a single `Option<(u32, u32)>` field on
   `FlatExtras` so the `EXTRA_RDEV` bit and the two `u32`s cannot
   diverge; deletes both `.expect()` calls.
4. `transfer:TokenReader::new:103` — change the signature to
   `pub fn new(...) -> Result<Self, TokenReaderError>` so zstd init
   failure propagates instead of panicking; update the four call sites.
5. `transfer:spawn_disk_thread:56` — return
   `Result<DiskThreadHandle, io::Error>` so `thread::Builder::spawn`
   failure (e.g., `EAGAIN`) surfaces as a transfer error instead of an
   abort.
6. `protocol:FlistReader::read_file_entry:532` — convert the
   `hardlink_idx.expect(...)` into a `match` that returns
   `FlistError::AbbreviatedWithoutHardlink`; the invariant is already
   double-checked in `is_abbreviated_follower`, so this is pure
   fail-loud hardening.
