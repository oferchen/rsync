# SPL-39 — Audit: `concurrent_delta/types.rs` Split-Points

**Status:** Audit complete. **Recommendation: NO SPLIT.**
**File:** `crates/engine/src/concurrent_delta/types.rs`
**Total LoC:** 896 (file as a whole)
**Production LoC:** ~440 (lines 1–552)
**Test LoC:** ~342 (lines 554–896, `#[cfg(test)] mod tests`)

---

## 1. Scope of the audit

SPL-39 requested a decomposition audit of `types.rs`, motivated by raw LoC
count. Per the standing policy in `feedback_loc_limits.md` (LoC enforcement
removed 2026-05-18, LoC alone is not a split trigger), the question is not
"can we split?" but "would splitting yield cohesion, ownership, or
maintainability gains that justify the import churn?"

The audit reads the file end-to-end, enumerates every public item and impl
block, classifies the seams, and weighs split vs. no-split.

---

## 2. Inventory of public items

| # | Item                                | Lines     | Kind          | Notes                                                           |
|---|-------------------------------------|-----------|---------------|-----------------------------------------------------------------|
| 1 | `FileNdx`                           | 23–51     | newtype + impls | `#[repr(transparent)] u32`. `Display`, `From<u32>`, `new`, `get`. |
| 2 | `DeltaWork`                         | 64–99     | struct         | 9 fields, all private.                                          |
| 3 | `DeltaWorkKind`                     | 102–108   | enum           | 2 variants (`WholeFile`, `Delta`).                              |
| 4 | `impl DeltaWork`                    | 110–295   | impl           | 3 constructors, 11 accessors, `into_parts`, sequence setters.   |
| 5 | `DeltaResult`                       | 307–325   | struct         | 6 fields, all private.                                          |
| 6 | `DeltaResultStatus`                 | 328–346   | enum           | 3 variants (`Success`, `NeedsRedo { reason }`, `Failed { reason }`). |
| 7 | `impl DeltaResult`                  | 348–446   | impl           | 3 constructors, accessors, status predicates.                   |
| 8 | `impl spill::SpillCodec for DeltaResult` | 457–552 | trait impl | Binary spill format (encode/decode/estimated_size).             |
| 9 | `mod tests`                         | 554–896   | tests         | 36 unit tests, ~342 LoC, co-located.                            |

---

## 3. Candidate seams considered

### Seam A — `FileNdx` newtype into its own file

- **Lines:** 13–51 (~39 LoC) + corresponding tests at 785–853 (~69 LoC).
- **Cohesion:** `FileNdx` is the *identity* of every `DeltaWork` and
  `DeltaResult` in the file. It is small (newtype + 4 trivial impls) and
  exists solely to type-safe the NDX value used by both other types.
- **Net effect of splitting:** Both `DeltaWork` and `DeltaResult` would have
  to `use super::file_ndx::FileNdx;` (or pub-re-export it) in every signature.
  No call site outside this module would gain anything — `FileNdx` is already
  reached via `concurrent_delta::types::FileNdx`.
- **Verdict:** Reject. The newtype is the spine of the module; co-location
  improves readability and removes an import.

### Seam B — `DeltaWork` family vs. `DeltaResult` family

- **Lines (Work):** 53–295 (~243 LoC production).
- **Lines (Result):** 297–446 (~150 LoC production).
- **Cohesion:** These are the two sides of one pipeline: producer dispatches
  `DeltaWork`, worker returns `DeltaResult`. They share `FileNdx`, share
  the `sequence: u64` reorder field, and share the producer/worker/consumer
  vocabulary documented in their module docs.
- **Co-tested:** Several tests cross both types
  (e.g. `result_sequence_preserved_on_clone` and
  `work_sequence_preserved_on_clone` deliberately mirror each other; splitting
  would either duplicate the helpers or scatter symmetric tests across files).
- **Net effect of splitting:** Two ~200-line files (`work.rs`, `result.rs`)
  plus a `mod.rs` re-export shim, with `FileNdx` either re-exported or
  duplicated as a third file. Two extra `use super::*;` paths in the tests.
- **Verdict:** Reject. The two types are designed as a pair and documented
  as a pair; the seam is a *symmetry*, not a *boundary*.

### Seam C — `SpillCodec for DeltaResult` impl into a sibling

- **Lines:** 447–552 (~106 LoC), cleanly delimited block.
- **Cohesion:** `SpillCodec` is defined in `super::spill`; this impl is the
  *only* concrete impl shipped by `types.rs` that depends on a sibling module.
- **Argument for splitting:** Moving this impl to `concurrent_delta/spill/`
  (e.g. `spill/codec_delta_result.rs`) would let `types.rs` stop depending
  on `super::spill::SpillCodec` entirely, making `types.rs` a pure data
  module.
- **Argument against splitting:** The impl needs access to *every* private
  field of `DeltaResult` (`ndx`, `sequence`, `bytes_written`,
  `literal_bytes`, `matched_bytes`, `status`) and the private constructor
  pattern via the struct literal. Moving it across a module boundary forces
  either (a) making all six fields `pub(super)`, exposing the internal
  representation, or (b) adding a `from_spill_parts` constructor purely to
  support the spill path. Both choices leak codec concerns into the type's
  public surface.
- **Verdict:** Reject. The codec lives where it does precisely because it
  is a structural operation on the private layout. Splitting trades 100 LoC
  of co-location for a permanent representation-leak risk.

### Seam D — `DeltaWorkKind` / `DeltaResultStatus` enums into a `status.rs`

- **Lines:** 102–108 + 328–346 (~26 LoC).
- **Cohesion:** Both enums are tightly coupled to their owning struct: each
  has exactly one method that constructs them (`DeltaWork::whole_file` /
  `::delta` set `kind`; `DeltaResult::success` / `::needs_redo` / `::failed`
  set `status`). They are never matched outside their owning impls inside
  this file.
- **Net effect of splitting:** A 26-line file plus two extra imports. The
  status enums also carry the upstream-rsync reference comment
  (`receiver.c:960-968`) that belongs next to its consuming struct.
- **Verdict:** Reject. Splitting an enum away from its single consumer is
  textbook over-decomposition.

### Seam E — Test module relocation

- **Lines:** 554–896 (~342 LoC).
- **Cohesion:** Tests are `#[cfg(test)] mod tests` with `use super::*;` —
  the standard idiom. They access only the public API.
- **Argument for splitting:** Mechanical reduction of file LoC.
- **Argument against splitting:** The tests are intentionally co-located to
  keep round-trip checks (e.g. `SpillCodec` encode→decode parity, default
  sequence values, builder chains) adjacent to the code they verify.
  Extracting to `tests/types.rs` integration tests would change visibility
  (private fields hidden), forcing accessor-only verification and weakening
  invariant coverage.
- **Verdict:** Reject. Co-located unit tests are the project norm and the
  ratio (342 test : 440 production ≈ 0.78) is healthy, not bloated.

---

## 4. Quantitative summary

| Bucket                                     | LoC | Share |
|--------------------------------------------|-----|-------|
| Module docs + `use`                        |   12 | 1.3%  |
| `FileNdx` (struct + 4 impls)               |   39 | 4.4%  |
| `DeltaWork` family (struct + enum + impl)  |  243 | 27.1% |
| `DeltaResult` family (struct + enum + impl)|  150 | 16.7% |
| `SpillCodec for DeltaResult`               |  106 | 11.8% |
| Tests                                      |  342 | 38.2% |
| Whitespace / closing braces / comments     |    4 | 0.5%  |
| **Total**                                  |  896 | 100%  |

Production code (rows 1–5): ~440 LoC across **one tightly coupled domain**
(work-item / result envelope for the concurrent delta pipeline).

---

## 5. Cohesion analysis (why this file is *one* module)

1. **Single responsibility — pipeline envelopes.** Every type in this file
   exists to move one file's delta computation request across thread
   boundaries and back. There is no second concern hiding here (no config,
   no policy, no error taxonomy, no channel plumbing).

2. **Shared identity type.** `FileNdx` is referenced by every public
   constructor, accessor, and codec method. Splitting any seam means
   threading `FileNdx` through extra imports.

3. **Symmetric API surface.** `DeltaWork::with_sequence` and
   `DeltaResult::with_sequence`, `::sequence()` accessors, and the
   `sequence` field doc-comments are written as mirrors. Reading them side
   by side is the point.

4. **Private-field codec.** `SpillCodec for DeltaResult` is a structural
   serializer that requires field-level access. Moving it outside `types.rs`
   forces either a representation leak or an artificial constructor.

5. **Co-located tests assert invariants, not API.** Tests reach into
   defaults, clone semantics, sequence preservation, and codec round-trips
   — exactly the kind of structural coverage that benefits from `use
   super::*;` visibility.

6. **Low maintenance burden.** This file is mostly trivial accessors and
   constructors; there are no algorithmic hot paths, no platform-conditional
   blocks, no `unsafe`, and no cross-crate trait bounds to manage. Read-only
   type-definition modules are the lowest-risk shape in the codebase.

---

## 6. Risks of splitting

- **Import churn.** Every call site that uses two of the three families
  (the common case in the pipeline) would gain a `use` line.
- **Representation leak.** Moving `SpillCodec` out of `types.rs` either
  promotes private fields or invents a back-door constructor.
- **Mirror-test fragmentation.** Symmetric `DeltaWork`/`DeltaResult` tests
  would split across files; reviewers would have to chase both.
- **Doc-link breakage.** Several rustdoc cross-links
  (`[\`DeltaTransferStrategy::process\`]`, `[\`ReorderBuffer\`]`,
  `[\`DeltaGenerator\`]`) currently resolve via `super::`. New module paths
  would force re-shimmed paths and risk silent link rot on re-exports
  (see the known pitfall in the project memory about rustdoc links on
  re-exports).
- **Zero benefit to call sites.** The file is consumed via
  `concurrent_delta::{DeltaWork, DeltaResult, FileNdx}` re-exports; the
  external API surface is unchanged by any internal split.

---

## 7. Recommendation

**Do not split `crates/engine/src/concurrent_delta/types.rs`.**

The file is a textbook cohesive types module: one responsibility (pipeline
work/result envelopes), a shared identity type that threads through every
public signature, a private-field codec that must stay co-located, and a
healthy co-located test suite. Production code is ~440 LoC — well within
the comfortable read-in-one-sitting band. The 896-LoC headline number is
dominated by tests (38%), which is the project norm.

Per `feedback_loc_limits.md`, LoC alone is not a split trigger, and none of
the candidate seams (A–E) clear the bar of *cohesion gain net of import
churn and representation risk*.

**No SPL-40+ follow-up tasks proposed.** Close SPL-39 as audit-complete /
no-action.

---

## 8. Future re-audit triggers

Re-open the question only if any of the following become true:

- A new concern is added that does not belong to the pipeline envelope
  (e.g. wire-format codec for a non-`DeltaResult` type, or a config struct).
- `DeltaWork` or `DeltaResult` grow a non-trivial algorithm (>50 LoC method
  body) rather than another accessor.
- `SpillCodec for DeltaResult` gains version negotiation or a second
  on-disk format, at which point a dedicated `spill_codec.rs` next to the
  other spill code is justified by the new responsibility, not by LoC.
- Tests grow past a 1.5× production-LoC ratio without commensurate coverage
  gains (i.e. drift toward redundant duplication).

Until then, the file is in its right shape.
