# Parallel delta scan: the stripe-invariance contract

Prerequisite note for the stripe-invariance gate on `--parallel-delta-scan`
(PDS-0a and PDS-0b). It states what "striped delta == sequential delta" has
to mean before any invariance test is written, and it enumerates every
source of stripe-dependence in the matcher with a classification and the
fixture shape that would expose it.

Scope. This is a docs-only note. It defines a contract and an inventory;
it changes no code, moves no wire byte, and decides nothing about whether
`--parallel-delta-scan` should default on. The default-flip decision is
gated on this contract's tests going green.

Non-goal. This note does not re-describe the window-split scheme. That
lives in `docs/design/delta-intra-file-parallel.md` (boundary overlap,
result ordering, threshold) and is assumed here.

## 1. What is striped today, and where

The striped scan has a live production caller. Reading from the top:

- `crates/transfer/src/generator/transfer/transfer_loop.rs:1063-1066`
  computes `want_parallel = self.config.flags.parallel_delta_scan &&
  should_parallel_delta(file_size, block_length, cores)` and memory-maps
  the source only when it holds.
- `crates/transfer/src/generator/transfer/transfer_loop.rs:1126-1140`
  dispatches to `generate_delta_from_signature_chunked(mmap.as_slice(),
  config, cores.min(PARALLEL_DELTA_MAX_CHUNKS))` on the mmap arm, and to
  the sequential `generate_delta_from_signature` otherwise.
- `crates/transfer/src/generator/delta.rs:391-421` is that entry point.
  It reconstructs the signature index through the shared
  `build_signature_index`, then either falls back to the sequential
  `DeltaGenerator::generate` (duplicate-content basis) or calls
  `DeltaGenerator::generate_chunked`.
- `crates/matching/src/generator.rs:1101-1229` is `generate_chunked`
  itself: four bail-outs to sequential, then the overlapping spatial
  split, the per-stripe scan, and `merge_copy_runs`.

So the striped path is reachable from a real `oc-rsync` invocation on the
network sender whenever `--parallel-delta-scan` is passed and the file
clears `should_parallel_delta`. It is opt-in, not dead. See section 7 for
what this means for the task rows that call it unwired.

The local-copy path never reaches this code. It has its own matcher
(`crates/engine/src/local_copy/context_impl/delta_transfer.rs:84`), which
is single-threaded, so the contract below is a sender-side contract only.

## 2. The contract

Three claims of increasing strength. Only the first two are asserted; the
third is where the current tree is self-contradictory (section 6).

### Level 0 - reconstruction identity (MUST HOLD, the floor)

For every source, every basis and every stripe count, applying the
emitted token stream to the basis MUST reproduce the source file
byte-for-byte, and the reconstruction MUST be identical to the one the
sequential scan produces.

This is the floor because it is the only property a peer can observe as
correctness rather than as encoding. It is what task 942 asserts from the
other side: a real upstream 3.5.0 receiver has to reconstruct the file
byte-identically from a striped oc sender, and the receiver applies
`Copy` and `Literal` tokens without ever knowing how the sender chose
them.

Upstream requires this and nothing weaker. Upstream has no striping at
all, so there is no upstream rule about which of several valid token
streams a sender must emit; `match.c` documents the opposite freedom in
its own comment at `match.c:330-331`, where the adjacency hint is
justified as making "the RLL coder happy", i.e. as an encoding
preference, not a correctness rule. A sender that emits more literals and
fewer copies is slower and fatter on the wire but not wrong.

Level 0 is total: it admits no eligibility carve-out. Every bail-out in
section 4 and every classification in section 5 has to preserve it.

### Level 1 - token-sequence identity (MUST HOLD for eligible inputs)

For every input that reaches the striped scan (that is, that survives all
four bail-outs of section 4), the emitted `DeltaToken` sequence MUST
equal the sequential scan's, element for element: the same `Copy` tokens
with the same basis indices in the same order, and the same `Literal`
byte runs at the same offsets.

Upstream does not require this - see Level 0 - but oc does, for two
reasons that are oc's own:

1. Wire framing is derived from the token stream. `script_to_wire_delta`
   plus `write_delta_with_inline_checksum`
   (`crates/transfer/src/generator/transfer/transfer_loop.rs:1178`)
   turn tokens into wire ops; a different token stream is a different
   byte stream even when it reconstructs the same file. Wire-byte
   divergence between an opt-in flag being on and off is exactly the
   class of change the golden-byte and interop gates exist to prevent.
2. The consecutive-match extension halves `s2length` on the wire when
   `CAP_CONSECUTIVE_MATCH` is negotiated. Any path where the sender's
   trust model differs from the receiver's assumption is a correctness
   question, not an encoding one. oc resolves this by routing the gated
   scan to sequential entirely (section 4, bail-out 1) rather than by
   reasoning about it per stripe.

Level 1 is what PDS-2b tests and what PDS-2c pins at the degenerate end
(stripe count 1 must equal the sequential path exactly).

### Level 2 - wire-byte identity (ASSERTED BY ONE SITE, HEDGED BY ANOTHER)

The strongest form: the bytes written to the socket are identical with
the flag on and off.

Level 2 follows from Level 1 only if literal framing is also identical,
because the sequential scan splits a long literal run into
`literal_flush_cadence(block_len)`-sized tokens as it goes, while the
merge emits one `Literal` per gap. `resegment_literals`
(`crates/matching/src/generator.rs:113-138`) exists precisely to restore
that framing, and its own doc claims the framing "matches" and that both
paths frame literals "identically by construction rather than by
coincidence" (`crates/matching/src/generator.rs:91-95`).

The caller's doc disagrees. `crates/transfer/src/generator/delta.rs:371-377`
says the wire bytes match "in the common case", and that "a rare
literal-run segmentation seam at a range boundary can still shift the
literal-token length framing by a few bytes (never the counts or the
reconstructed data)".

Both statements are in the tree today and they cannot both be right.
Settling this is the job of PDS-2b plus the boundary fixture of PDS-2d.
Until it is settled, this note asserts Levels 0 and 1 and records Level 2
as OPEN. Do not write a test that assumes Level 2 holds and do not write
one that assumes it fails; write the fixture that decides it.

### What the contract is measured against

The reference arm is oc's own sequential scan (`DeltaGenerator::generate`
over the identical source and index), not upstream. Upstream enters at
two removes:

- as the floor's external check (task 942 / PDS-4a): a real 3.5.0
  receiver reconstructs from a striped oc sender;
- as the source of truth for what the sequential scan itself should do,
  which is a separate question this note does not reopen.

## 3. Upstream anchors for the sequential semantics

Cited from `target/interop/upstream-src/rsync-3.5.0/match.c`, the pinned
oracle.

- `match.c:197` - `end = len + 1 - s->sums[s->count-1].len;`. The scan
  bound is expressed in terms of the LAST block's length, which is the
  short final block. This is the upstream reason only an EOF-reaching
  scan may claim that block: at any earlier offset `l = MIN(blength,
  len-offset)` (`match.c:254`) is the full block length and the
  `l != s->sums[i].len` test at `match.c:255` rejects it.
- `match.c:215-228` - the hash lookup and `hash_hits++`. The hash table
  is built once by `build_hash_table` (`match.c:78-111`) and, absent
  `updating_basis_file`, is not mutated by the scan.
- `match.c:232-239` - under `updating_basis_file`, a bypassed chain entry
  is physically UNLINKED from the hash chain (`*prev = s->sums[i].chain`).
  This is a scan-order-dependent mutation of the shared index, performed
  by upstream itself, and it is decided against the running `offset`.
- `match.c:321-334` - `check_want_i`. After a hash match is found,
  upstream may REDIRECT the resolved index to `want_i` when `want_i`
  matches equally well (same `sum1`, same length, same `sum2`), and then
  sets `want_i = i + 1`. The redirect can only change the outcome when
  two basis blocks have identical content, which is the exact condition
  `has_duplicate_blocks()` names.
- `match.c:372` - `if (backup >= s->blength+CHUNK_SIZE && end-offset >
  CHUNK_SIZE)`, the early literal flush, with `CHUNK_SIZE` = 32 KiB
  (`rsync.h:158`).

## 4. The four routes back to sequential

`generate_chunked` is not always parallel. Every one of these is a
precondition of the contract, and each has a test row that must prove the
route is actually taken rather than assumed.

| # | Condition | Site | Reason | Test row |
|---|---|---|---|---|
| 1 | `consecutive_match_needed >= 2` and enough full blocks | `crates/matching/src/generator.rs:1117-1121` | The seq-match predicate needs cross-stripe block adjacency; routed to `generate_gated` | PDS-3b (task 940) |
| 2 | `updating_basis_file` | `crates/matching/src/generator.rs:1127-1130` | The `match.c:235-236` in-place guard compares a basis offset against the GLOBAL source cursor; a stripe knows only a stripe-local cursor | new row, see section 8 |
| 3 | `chunks <= 1` or `block_len == 0` | `crates/matching/src/generator.rs:1143-1146` | Too small to split; also the degenerate arm PDS-2c pins | PDS-2c |
| 4 | `index.has_duplicate_blocks()` | `crates/transfer/src/generator/delta.rs:408-413` | Duplicate content makes block resolution history-dependent (see section 5, row B) | PDS-2e (task 936) |

Bail-out 4 is decided by the CALLER, one level above `generate_chunked`.
A test that drives `generate_chunked` directly will not exercise it. That
is a live vacuity risk for PDS-2e: the test must enter through
`generate_delta_from_signature_chunked`, or it will pass while proving
nothing.

## 5. PDS-0b: sources of stripe-dependence

Classification vocabulary:

- INVARIANT - cannot differ across stripe counts, by construction. The
  reason is structural, not empirical.
- ORDER-SENSITIVE - the result depends on the order in which windows are
  visited, so striping can change it unless something else forbids that.
- BOUNDARY-SENSITIVE - the result depends on a window's position relative
  to a stripe edge or to EOF.

| Row | Source | Site | Class | Fixture that would expose it |
|---|---|---|---|---|
| A | BitHash negative prefilter | `crates/matching/src/index/bithash.rs:91` (insert), `:103` (contains); probe gate in `crates/matching/src/index/mod.rs` | INVARIANT | Any; see below for why no fixture can separate the arms |
| B | Matched-block prune / consumed bitset | `crates/matching/src/index/matched_blocks.rs`; shared bitset `crates/matching/src/index/mod.rs:162`, `:535` `is_consumed`, `:560` `mark_consumed`, `:576` `reset_consumed` | ORDER-SENSITIVE, neutralised | Basis with two identical blocks, source referencing both |
| C | Consecutive-match (`seq_matches=2`) | `crates/matching/src/generator.rs:867` `generate_gated`, threshold set at `:299` | ORDER-SENSITIVE, routed away | Source whose only match run straddles a stripe edge, with `CAP_CONSECUTIVE_MATCH` on |
| D | `want_i` adjacency hint | `crates/matching/src/generator.rs:460`, probed at `:551-566` | INVARIANT given bail-out 4 | Duplicate-content basis (already routed to sequential) |
| E | Short final block / tail probe | `crates/matching/src/generator.rs:740-812`, gated by `tail_match` = `e == n` at `:1176-1178` | BOUNDARY-SENSITIVE, handled | Source ending in the basis's short final block, stripe edge placed just before it |
| F | Stripe read-ahead and match straddle | `crates/matching/src/generator.rs:1154-1170` | BOUNDARY-SENSITIVE | Match starting `block_len - 1` bytes before a stripe edge |
| G | Copy-run merge and dedup | `crates/matching/src/generator.rs:189-237` `merge_copy_runs` | BOUNDARY-SENSITIVE | Two stripes both claiming the boundary block |
| H | Literal re-segmentation | `crates/matching/src/generator.rs:113-138` `resegment_literals`, cadence at `:98` | BOUNDARY-SENSITIVE, contested | Literal gap longer than `block_len + 32 KiB` spanning a stripe edge |
| I | In-place monotonicity guard | `crates/matching/src/generator.rs` `basis_offset_ok`, consulted at `:566-568` | ORDER-SENSITIVE, routed away | `--inplace` push with a backward-referencing match |
| J | mmap-failure fallback | `crates/transfer/src/generator/transfer/transfer_loop.rs:1063-1066` | INVARIANT by dispatch | Source on a filesystem where mmap fails (procfs, some FUSE) |
| K | Match counters (`matches`, `hash_hits`, `false_alarms`) | `crates/matching/src/generator.rs:447-449`, `:716`, `:723`, reported at `:826-832` | ORDER-SENSITIVE, unobservable on this path | None needed; see below |

### Row A - BitHash order-neutrality (task 941)

Structural answer: membership is order-neutral, and the proof is that the
structure is immutable during the scan.

The bit array is populated once at index-build time, from the set of
basis rolling sums, and `insert` is only reachable from the builder. The
probe path (`contains`) is a pure read. `generate_chunked` shares one
`&DeltaSignatureIndex` across all rayon workers
(`crates/matching/src/generator.rs:1176-1180`), and Rust's borrow rules
make a `&` shared reference to a non-interior-mutable field unwritable,
so no worker can insert. `bithash` holds no atomics and no `UnsafeCell`;
the only interior-mutable field on the index is `consumed`
(`crates/matching/src/index/mod.rs:162`), which is row B, not row A.

Set membership is therefore a pure function of the basis, computed before
any stripe exists. Two probes of the same rolling sum from two workers
return the same answer, and neither can change the answer for the other.

What a proof would need, if the structure ever gains a mutating probe
path: a test asserting that the bit array's contents are byte-identical
before and after a full striped scan, at two different stripe counts, and
a mutation that inserts on probe to prove the assertion is not vacuous.
Today such a test would be a tautology, which is why row A carries no
PDS-2 fixture. State that in the PDS-3c row rather than writing a test
that cannot fail.

### Row B - matched-block prune under striping (task 939)

The task asks how consumption order across stripes changes later probes.
The answer in the current design is that it cannot, because pruning is
DISABLED for the striped scan and the shared bitset is cleared before it:

- `crates/matching/src/generator.rs:1149-1152` calls
  `index.reset_consumed()` once, with the comment that a prior pruned
  `generate()` would otherwise leave every block consumed and defeat all
  matching.
- `crates/matching/src/generator.rs:1179` passes `false` as the
  `prune` argument to `generate_with_prune` for every stripe, so no
  worker calls `mark_consumed`.

So the bitset is written once (to all-zero) and then read-only, which
makes it invariant for the duration of the striped scan by the same
argument as row A.

That is a design decision, not an accident, and PDS-3a's job is to PIN
it, not to re-derive it. Two things the pin must cover:

1. The `reset_consumed()` call is load-bearing across CALLS, not just
   within one. `DeltaSignatureIndex` can outlive a `generate()` call, and
   a pruned sequential scan leaves bits set. A test that builds a fresh
   index per scan cannot see a regression here. The fixture must run a
   pruned sequential `generate()` and THEN a `generate_chunked()` on the
   same index, and assert the second one still matches.
2. Prune-off changes which basis block a duplicate resolves to, which is
   why bail-out 4 exists. The two are one decision and must be pinned
   together, or a future change that re-enables pruning per stripe will
   look safe.

### Row C - consecutive-match at a stripe boundary (task 940)

The predicate is inherently sequential: a match is trusted only when it
is preceded by a matching neighbour, so the decision for the first window
of stripe k depends on the last window of stripe k-1, which a different
worker scanned.

At a stripe boundary the predicate has no defined value under striping.
Concretely, a run of matching blocks that straddles the edge would be
seen by the boundary worker as starting fresh, so its first block has no
predecessor within that stripe and would be demoted to a literal, while
the sequential scan would emit it as a `Copy`. That is a Level-1
violation and, because the extension also halves `s2length` on the wire,
it is not a difference the receiver can absorb silently.

oc does not resolve this per stripe. It routes the whole file to the
sequential gated scan (bail-out 1). PDS-3b's decision is therefore
"keep the route, pin that it is taken", and the pin must assert on the
ROUTE (that a gated config produces the sequential scan's exact token
stream at every stripe count), not merely that the output is
reconstructible.

### Rows E, F, G - the boundary trio

These three are the reason PDS-2d exists and they interlock:

- E: only the stripe whose end equals `n` receives `tail_match = true`,
  so only it may claim the basis's short final block. The upstream reason
  is `match.c:197` plus the `l != s->sums[i].len` rejection at
  `match.c:255`: at any non-EOF offset that block's length test fails, so
  a mid-file stripe claiming it would be emitting a match upstream's own
  scan could never produce.
- F: each stripe scans `block_len` bytes past its end so a straddling
  match completes inside the owning worker rather than being lost to
  literals. The consequence is deliberate overlap: the same basis block
  can be claimed by two workers.
- G: `merge_copy_runs` resolves that overlap. It sorts by
  `source_start`, tie-breaking on longer-run-first
  (`crates/matching/src/generator.rs:194-198`), then walks a greedy
  cursor that skips any run starting before the cursor
  (`:206-208`). Fat seq-match runs are split to block granularity before
  the merge (`:1196-1218`) specifically so the greedy cursor can dedup
  the shared boundary block and tile the remainder instead of discarding
  a whole run.

The fixture PDS-2d needs is one where a match begins strictly inside the
read-ahead zone (within `block_len` bytes before a stripe edge) AND the
same region is reachable from the next stripe's own scan, so that both
workers emit a run for it and the merge has to choose. Scanning a source
whose matches happen to align to stripe edges will exercise none of this
and will pass vacuously.

### Row H - literal framing

The contested one, per section 2. `resegment_literals` coalesces adjacent
literals and re-splits at exact `literal_flush_cadence` boundaries from
the run start; the sequential scan flushes when its pending accumulator
crosses the same threshold. The fixture that decides it is a literal gap
strictly longer than `block_len + CHUNK_SIZE` that spans a stripe edge,
compared token-for-token including token lengths, not just concatenated
bytes.

Two observations that the PDS-2b author should carry, both unmeasured:

- Upstream's flush at `match.c:372` has a second conjunct,
  `end-offset > CHUNK_SIZE`, which suppresses the early flush near EOF.
  oc's `literal_flush_cadence` models only the first conjunct. This is a
  question about oc's sequential path versus upstream, not about
  striping, so it does not affect Levels 0 and 1 as defined here. It does
  bear on task 942's upstream-reconstruct cell, and it should be measured
  there rather than assumed either way.
- oc's citation for the cadence
  (`crates/matching/src/generator.rs:96`, "match.c:339-340") has drifted
  at the 3.5.0 pin. The construct is at `match.c:372`. Reported, not
  fixed: `crates/matching` is being edited concurrently and this note
  changes no code.

### Row K - counters

`matches`, `hash_hits` and `false_alarms` are locals in
`generate_with_prune` and are consumed only by the `Deltasum` debug line
at `crates/matching/src/generator.rs:827`. They are not returned:
`DeltaScript` carries tokens, total bytes and literal bytes only. Under
striping each worker keeps its own counters and reports its own debug
line, so the numbers differ from the sequential scan's by construction.

This is unobservable in the transferred data and in the wire bytes, and
the once-per-run `total:` line is rendered only for local transfers
(`crates/cli/src/frontend/execution/drive/summary.rs:205` gates
`emit_total` on `is_local_transfer`), which never stripe. So row K is
recorded as a known, deliberate non-invariant rather than a defect. If
`--debug=deltasum` is ever wired on the network sender, the striped arm
will emit N sets of counters instead of one, and that is the moment to
decide whether to aggregate them.

## 6. Open questions this note does not settle

1. Level 2 (wire-byte identity). Two in-tree docs contradict each other.
   Decided by PDS-2b plus the PDS-2d boundary fixture.
2. Whether the merge's greedy tie-break can ever select a DIFFERENT copy
   set than the sequential scan on a duplicate-free basis. The structural
   argument says no (unique content means one candidate per window), but
   the argument covers WHICH block, not WHICH RUN wins when two
   overlapping runs start at the same offset with different lengths. The
   tie-break at `crates/matching/src/generator.rs:194-198` exists because
   that situation arises. PDS-2b should assert on it directly.
3. Whether `should_parallel_delta`'s threshold interacts with the
   `min_chunk` floor inside `generate_chunked`
   (`crates/matching/src/generator.rs:1136-1142`) such that some
   file-size band passes the outer gate and then silently takes bail-out
   3. Not a correctness risk (bail-out 3 is the sequential path) but it
   is a coverage risk: a benchmark or test in that band measures the
   sequential scan while believing it measures the striped one.

## 7. Verdict on `generate_chunked`'s callers

Asked as part of PDS-0b, answered by reading, and it CONTRADICTS the way
tasks 131 and 272 are worded.

`generate_chunked` has exactly one non-test caller:
`generate_delta_from_signature_chunked`
(`crates/transfer/src/generator/delta.rs:413`), which itself has exactly
one non-test caller: the network sender loop at
`crates/transfer/src/generator/transfer/transfer_loop.rs:1127`. That call
is reached whenever `--parallel-delta-scan` is set, the file clears
`should_parallel_delta`, and the mmap succeeds.

So the striped scan is WIRED and reachable in production today, behind an
opt-in flag. Task 131 ("verify/wire sender-side striped delta") is
half-stale: the wiring exists; what does not exist is the invariance
gate, which is this epic. Task 272's concern is narrower and is NOT
refuted by this: it asks whether `generate_gated` carries a basis-index
adjacency check, which is a property of the gated sequential scan
(bail-out 1), not of the striped path. Nothing here settles it.

Recorded per the task instruction: observed, not fixed.

## 8. Findings that contradict the PDS task rows

- Task 131 wording. See section 7. The striped path is not unwired.
- Task 939 as filed asks how cross-stripe consumption order changes later
  probes. It cannot, because prune is off under striping and the bitset
  is reset first. The row's real content is a PIN with two specific
  requirements (section 5, row B), one of which - the cross-CALL reset -
  no natural fixture covers by accident.
- Task 941 as filed asks for a proof of BitHash order-neutrality. The
  honest deliverable is a structural argument plus a statement that no
  discriminating test exists today, not a test. Writing a test here would
  be a tautology dressed as evidence.
- The bail-out inventory is FOUR routes, not the three implied by the
  PDS-2c/2e/3b row set. Bail-out 2 (`updating_basis_file`, the
  `--inplace` monotonicity guard) has no row. It needs one: `--inplace`
  plus `--parallel-delta-scan` is a reachable flag combination, and the
  guard silently disables striping.
- PDS-2e has a vacuity hazard: bail-out 4 lives in the caller, so a test
  entering at `generate_chunked` cannot observe it (section 4).

## References

Upstream rsync 3.5.0 (`target/interop/upstream-src/rsync-3.5.0/`):

- `match.c:78-111` - `build_hash_table`, index built once
- `match.c:197` - scan bound expressed via the short final block
- `match.c:215-228` - hash lookup, `hash_hits++`
- `match.c:232-239` - `updating_basis_file` chain unlink
- `match.c:254-255` - block length test
- `match.c:321-334` - `check_want_i` adjacency redirect
- `match.c:372` - early literal flush; `rsync.h:158` - `CHUNK_SIZE`

oc-rsync sources:

- `crates/matching/src/generator.rs:113-138` - `resegment_literals`
- `crates/matching/src/generator.rs:189-237` - `merge_copy_runs`
- `crates/matching/src/generator.rs:433` - `generate_with_prune`
- `crates/matching/src/generator.rs:867` - `generate_gated`
- `crates/matching/src/generator.rs:1101-1229` - `generate_chunked`
- `crates/matching/src/index/mod.rs:162`, `:535`, `:560`, `:576` - the
  consumed bitset
- `crates/matching/src/index/mod.rs:357` - `has_duplicate_blocks`
- `crates/matching/src/index/bithash.rs:91`, `:103`
- `crates/transfer/src/generator/delta.rs:391-421` - the striped entry
- `crates/transfer/src/generator/transfer/transfer_loop.rs:1063-1140` -
  the production dispatch

Sibling design notes:

- `docs/design/delta-intra-file-parallel.md` - the window-split scheme
- `docs/design/zsync-inspired-matching.md` - parent of the three zsync
  borrows
- `docs/design/zsync-prune.md` - matched-block prune
- `docs/design/zsync-bithash.md` - bithash shape and sizing
- `docs/design/zsync-seq-match.md` - consecutive-match extension
