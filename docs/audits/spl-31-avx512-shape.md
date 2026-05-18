# SPL-31 avx512 md5 SIMD shape audit (#2459)

Audits `crates/checksums/src/simd_batch/md5_simd/avx512.rs` (1292 lines)
against the workspace `default_max_lines = 650` cap and decides whether
the file is decomposable or warrants a `tools/line_limits.toml`
override.

## Tool

- Config: `tools/line_limits.toml`.
- Enforcer: `tools/enforce_limits.sh` -> `cargo run -p xtask --
  enforce-limits`.
- CI job: `enforce-limits (informational)` in
  `.github/workflows/ci.yml` - `continue-on-error: true`.

## Baseline

- Branch: `origin/master` (worktree checkout).
- File: `crates/checksums/src/simd_batch/md5_simd/avx512.rs`.
- Lines: **1292**.
- Effective cap: 650.
- Delta: **+642 lines over cap**.

Sibling SIMD backends for context (same module):

| File                                                              | Lines |
|-------------------------------------------------------------------|-------|
| `crates/checksums/src/simd_batch/md5_simd/avx512.rs`              | 1292  |
| `crates/checksums/src/simd_batch/md5_simd/sse2.rs`                |  480  |
| `crates/checksums/src/simd_batch/md5_simd/ssse3.rs`               |  439  |
| `crates/checksums/src/simd_batch/md5_simd/sse41.rs`               |  437  |
| `crates/checksums/src/simd_batch/md5_simd/avx2.rs`                |  421  |
| `crates/checksums/src/simd_batch/md5_simd/neon.rs`                |  413  |
| `crates/checksums/src/simd_batch/md5_simd/wasm.rs`                |  341  |
| `crates/checksums/src/simd_batch/md4/simd/avx512.rs`              |  562  |

The avx512 md5 file is roughly 3x the size of every other md5 backend
and 2.3x the size of its md4 avx512 sibling.

## Shape

The file contains exactly two production functions and one tests
module:

| Span        | Lines | Item                                                                |
|-------------|-------|---------------------------------------------------------------------|
| 1-119       |  119  | RFC 1321 constants (`INIT_A..D`, `K[64]`), `Aligned512` storage     |
| 120-199     |   80  | `digest_x16` (Rust harness: pad, transpose, dispatch, collect)      |
| 200-1102    |  903  | `process_block_avx512` (single `asm!` block, 64 unrolled rounds)    |
| 1103-1118   |   16  | non-x86_64 fallback `digest_x16` (scalar dispatch)                  |
| 1120-1292   |  173  | `#[cfg(test)] mod tests` (three vectors: scalar parity, RFC, mixed) |

The 903-line `process_block_avx512` body is dominated by a **single
`asm!(...)` invocation spanning lines 238-1101** (864 lines). That one
Rust statement contains 64 unrolled MD5 rounds (`F`, `G`, `H`, `I`
auxiliary functions), interleaved with per-round `vpternlogd`,
`vprold`, `vpaddd`, and `vpbroadcastd` instructions.

An inline source comment at lines 232-234 documents the constraint:

> All assembly is in a single asm! block to prevent the compiler from
> inserting code between rounds that could clobber ZMM registers. K
> constants are loaded from memory via pointer instead of per-round
> operands.

## Classification

Category **(a)**: one monolithic SIMD round-loop. The bulk of the
file (864 of 1292 lines, 67%) is a single Rust statement that must
remain atomic for correctness.

## Decomposition options considered

1. **Split the asm into per-round-group blocks (F/G/H/I)**. Rejected:
   each `asm!` block is a compiler-visible boundary across which ZMM
   register state is not preserved. Splitting would require spilling
   `a, b, c, d` plus all sixteen message words to memory between
   groups, then reloading. That defeats the entire point of the AVX-512
   backend (16-lane parallel state held entirely in `zmm0..zmm23`).
   Author has explicitly forbidden this pattern in the source comment.

2. **Macroize round emission**. A `macro_rules!` round! macro would
   collapse each 12-line round into one invocation, dropping
   `process_block_avx512` to ~250 lines. Net file size after macro
   definitions: ~400 lines. Trade-off: the asm becomes opaque to a
   reader chasing a bug across rounds (no inline round numbers, no
   per-round message-word/shift-amount visibility). MD5 round
   schedules are notoriously fiddly (the `m[i]` index permutation in
   rounds 16-31, 32-47, 48-63 is non-obvious); the current layout
   preserves the index in a comment on every round. Macroization is
   plausible but represents a readability regression that buys nothing
   functional.

3. **Move `K[64]` constants to a shared module**. The MD5 round
   constants are shared by every md5_simd backend (scalar, sse2,
   ssse3, sse41, avx2, avx512, neon, wasm). A `pub(super) const K:
   [u32; 64]` in `md5_simd/mod.rs` would remove ~65 lines from this
   file and from each sibling. Worthwhile cleanup but out of scope
   for SPL-31 (touches eight files, not just avx512.rs). Tracked
   separately if pursued.

4. **Extract `mod tests` to `avx512/tests.rs`**. Saves 173 lines.
   Mechanical, no behaviour change. Brings the file to 1119 lines -
   still 469 over cap. Does not change the classification, only
   defers it.

5. **Extract Rust harness `digest_x16` + `Aligned512` to
   `avx512/harness.rs`**. Saves ~199 lines. Combined with the test
   split: 920 lines remaining, still 270 over cap. Still does not
   resolve the asm monolith.

None of the available extractions bring the file under 650 lines
without either (a) macroizing the asm (readability regression) or (b)
splitting the asm (performance regression). The asm body itself is
incompressible without trade-offs.

## Recommendation

Add a per-file override to `tools/line_limits.toml`:

```toml
[[overrides]]
path = "crates/checksums/src/simd_batch/md5_simd/avx512.rs"
max_lines = 1400
warn_lines = 1300
```

Cap = 1400 (current + ~8%), warn = 1300. Reasoning:

- The dominant span is one inline `asm!` invocation that must stay
  atomic; the LoC count reflects 64 unrolled MD5 rounds, each
  spanning ~12 asm lines plus a round-number comment.
- The override mirrors the existing precedent for
  `crates/engine/src/local_copy/buffer_pool/pool.rs` (cap 1200) where
  splitting would regress a hot-path data structure.
- Headroom of ~108 lines (1400 - 1292) accommodates minor future
  changes (e.g., adding a per-CPU dispatch fast-path or extra
  documentation) without requiring another config edit.
- Setting `warn_lines = 1300` gives an early signal if the file
  grows materially, prompting a re-audit before approaching the cap.

If a future refactor consolidates the MD5 `K[64]` constants into a
shared module across all md5_simd backends, the override can be
reduced to ~1350 / 1250 at that point.

## Decision

**Audit-only.** No code restructuring; ship the
`tools/line_limits.toml` override and this audit document.
