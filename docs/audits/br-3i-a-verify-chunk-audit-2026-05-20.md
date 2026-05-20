# BR-3i.a - verify_chunk callers and checksum negotiation surface audit

Date: 2026-05-20
Scope: read-only research for BR-3i.b
Tracked under: #2497

## Goal

Trace the path the negotiated strong-checksum identifier takes from handshake
to `ParallelDeltaApplier::verify_chunk` so the follow-up BR-3i.b can plumb the
real algorithm through and replace the `chunk.data.len()` stub at
`crates/engine/src/concurrent_delta/parallel_apply.rs:482-488`.

## 1. Resolved checksum path from handshake to applier

### 1.1 Wire negotiation produces the algorithm

The `negotiate_capabilities_with_override` exchange in
`crates/protocol/src/negotiation/capabilities/negotiate.rs:163-361` is the
single point that resolves the algorithm both peers agreed on. The function
returns a `NegotiationResult` carrying a `ChecksumAlgorithm` enum value:

- struct definition: `crates/protocol/src/negotiation/capabilities/negotiate.rs:56-63`
  - field `pub checksum: ChecksumAlgorithm`
- enum definition: `crates/protocol/src/negotiation/capabilities/algorithms.rs:49-66`
  - variants `None | MD4 | MD5 | SHA1 | XXH64 | XXH3 | XXH128`
  - derives `Debug, Clone, Copy, PartialEq, Eq`
- public re-export: `crates/protocol/src/lib.rs:192-193`

The default fallbacks live in the same negotiator:

- protocol < 30 -> `ChecksumAlgorithm::MD4` (`negotiate.rs:181`)
- `do_negotiation == false` (peer lacks `CF_VARINT_FLIST_FLAGS`) -> `ChecksumAlgorithm::MD5` (`negotiate.rs:211`)
- successful negotiation -> mutual-first match via `choose_checksum_algorithm` (`negotiate.rs:316`, `negotiate.rs:377-411`)

### 1.2 Handshake stores the result

The transfer handshake captures the `NegotiationResult` and propagates it
through the per-role context structs:

- `crates/transfer/src/handshake.rs:18` (import) and `:39`
  - field `pub negotiated_algorithms: Option<NegotiationResult>` on the handshake outcome.
- `crates/transfer/src/setup/types.rs:5,12`
  - field `pub negotiated_algorithms: Option<NegotiationResult>` on the setup outcome.
- `crates/transfer/src/setup/mod.rs:166` constructs the value when the
  handshake completes without a wire exchange.
- `crates/transfer/src/setup/negotiator.rs:90,180-181`
  - the `Negotiator` trait method `negotiate(..) -> io::Result<NegotiationResult>`
    and its production impl that calls `protocol::negotiate_capabilities_with_override`.

### 1.3 Receiver context owns the result

The receiver stores the negotiated value as state for the duration of the
transfer:

- `crates/transfer/src/receiver/mod.rs:53` (import) and `:150`
  - field `negotiated_algorithms: Option<NegotiationResult>` on `ReceiverContext`.
- `crates/transfer/src/generator/context.rs:16,64`
  - mirror field on the generator side (`pub(crate) negotiated_algorithms`).

### 1.4 Apply path materialises the algorithm

When the receiver builds its per-file delta-apply state, it borrows
`negotiated_algorithms` from `ReceiverContext` and feeds it to the
algorithm-aware constructors:

- `crates/transfer/src/receiver/transfer/setup.rs:117-123`

  ```text
  let checksum_factory = ChecksumFactory::from_negotiation(
      self.negotiated_algorithms.as_ref(),
      self.protocol,
      self.checksum_seed,
      self.compat_flags.as_ref(),
  );
  let checksum_algorithm = checksum_factory.signature_algorithm();
  ```

- `crates/transfer/src/receiver/transfer/pipeline.rs:77` and `:110-115`
  pass both the `NegotiationResult` and `checksum_algorithm` into the
  request loop and into the whole-file `ChecksumVerifier`.
- `crates/transfer/src/shared/checksum.rs:31,76-98` - `ChecksumFactory`
  resolves the algorithm enum once (`algorithm: ChecksumAlgorithm`, line 53)
  and exposes it via `algorithm()` (line 114).
- `crates/transfer/src/delta_apply/checksum.rs:6,42-95` - `ChecksumVerifier`
  consumes the same enum to build a per-file whole-file digest.

### 1.5 Where the trail stops today

`ParallelDeltaApplier` is constructed without a `NegotiationResult` and the
per-chunk verify step is a length-only stub:

- `crates/engine/src/concurrent_delta/parallel_apply.rs:248-258` - the struct
  has no field for the negotiated algorithm.
- `crates/engine/src/concurrent_delta/parallel_apply.rs:271-287` - `new`
  takes only `concurrency: usize`.
- `crates/engine/src/concurrent_delta/parallel_apply.rs:478-488` - `verify_chunk`
  returns `digest_len = chunk.data.len()` and does not invoke any strong
  checksum primitive.

The applier itself is feature-gated behind `parallel-receive-delta`:

- `crates/engine/src/concurrent_delta/mod.rs:177-178` and `:188-189`.

## 2. Every `ParallelDeltaApplier::new` construction site

All call sites in the main tree (worktree copies excluded). Production
receiver code does **not** construct the applier today - it is only
exercised by tests and the bench:

| Path | Line | In scope at call site? |
|------|------|------------------------|
| `crates/engine/src/concurrent_delta/parallel_apply.rs` | 550, 567, 588, 622, 631, 641, 657, 703, 717, 732 | unit-test module - no `NegotiationResult`, none in scope |
| `crates/engine/tests/arc_drain_panic_recovery.rs` | 163 | integration test - no `NegotiationResult` |
| `crates/engine/benches/parallel_receive_delta_perf.rs` | 256 | criterion bench - no `NegotiationResult` |

The applier is re-exported via `crates/engine/src/concurrent_delta/mod.rs:189`
but no production caller in `crates/transfer/` references it. The closest
production caller that already owns a `NegotiationResult` is the receiver
pipeline at `crates/transfer/src/receiver/transfer/pipeline.rs:77,110-115` -
that is the natural future wiring point for BR-3i.b once the applier accepts
the algorithm.

In short: every call site needs plumbing because no caller currently has the
algorithm in scope. Test and bench call sites can pass a fixed default
(MD5) without changing behaviour; production-wiring is a separate step
beyond BR-3i.b.

## 3. Checksum API the applier should call

The `checksums` crate exposes two layers; either can back `verify_chunk`,
with the Strategy layer being the most aligned with runtime dispatch:

### 3.1 Strategy layer (preferred for runtime dispatch)

`crates/checksums/src/strong/strategy/mod.rs:76-94` is the public surface:

- `ChecksumStrategy` trait (`trait_def.rs:24-50`) - `Send + Sync`, methods
  `compute(&self, data: &[u8]) -> ChecksumDigest`, `compute_into(&self, data,
  out)`, `digest_len(&self) -> usize`, `algorithm_kind(&self) ->
  ChecksumAlgorithmKind`.
- `ChecksumStrategySelector::for_algorithm(kind, seed) -> Box<dyn ChecksumStrategy>`
  (`selector.rs:86-98`) - one-shot factory.
- `ChecksumAlgorithmKind` (`kind.rs:13-37`) - `Clone, Copy, Debug, Eq,
  PartialEq, Hash` plain enum (MD4, MD5, SHA1/256/512, XXH64, XXH3, XXH3_128).
- `ChecksumDigest` and `MAX_DIGEST_LEN` (`digest.rs`, re-exported at
  `strategy/mod.rs:86`).

Simplest call shape inside `verify_chunk`:

```text
let digest = self.strategy.compute(&chunk.data);
let digest_len = digest.len();
```

The selector also caches per-algorithm singletons via `for_algorithm`, but
each call boxes. Building one `Box<dyn ChecksumStrategy>` once at the applier
boundary (or wrapping it in `Arc`) keeps the rayon worker hot path
allocation-free.

### 3.2 Direct concrete hashers

`crates/checksums/src/strong/mod.rs:73-179` exposes the concrete hashers
(`Md4`, `Md5`, `Sha1`, `Sha256`, `Sha512`, `Xxh64`, `Xxh3`, `Xxh3_128`) and
the `StrongDigest` trait (`mod.rs:139-179`) with one-shot helpers
`digest(data)` and `digest_with_seed(seed, data)`. The existing whole-file
`ChecksumVerifier` (`crates/transfer/src/delta_apply/checksum.rs:12-159`) is
an enum dispatch over these hashers; the applier could replicate that style
to avoid the trait object, but that brings the algorithm enum into the
engine crate which today only depends on `checksums::strong`. The Strategy
layer is the cleaner abstraction across crate boundaries.

## 4. Recommendation for BR-3i.b

### 4.1 Add one field to `ParallelDeltaApplier`

```text
strategy: Arc<dyn checksums::strong::strategy::ChecksumStrategy>,
```

- `Arc` (rather than `Box`) so the same strategy can be cloned cheaply onto
  each rayon worker without re-boxing.
- The trait is already `Send + Sync` (`trait_def.rs:24`), satisfying the
  applier's struct-level Send/Sync requirement.

### 4.2 Constructor surface

Add a second constructor that takes the algorithm; keep `new(concurrency)`
for the existing tests and bench by defaulting it to MD5 (the protocol >= 30
default that matches what `ChecksumFactory::from_negotiation` resolves when
no `NegotiationResult` is present):

```text
pub fn new(concurrency: usize) -> Self  // keeps existing tests valid, default MD5
pub fn with_strategy(
    concurrency: usize,
    strategy: Arc<dyn ChecksumStrategy>,
) -> Self
```

The default in `new` is `ChecksumStrategySelector::for_algorithm(ChecksumAlgorithmKind::Md5, 0)`.

### 4.3 Update `verify_chunk`

Switch the signature from `fn verify_chunk(chunk: DeltaChunk) -> VerifiedChunk`
to a method `fn verify_chunk(&self, chunk: DeltaChunk) -> VerifiedChunk` (or
take `&Arc<dyn ChecksumStrategy>` as a parameter) so the rayon closure can
clone the `Arc` cheaply. Body becomes:

```text
let digest = self.strategy.compute(&chunk.data);
let digest_len = digest.len();
VerifiedChunk { chunk, digest_len }
```

Both call sites (`apply_chunk_parallel` at `parallel_apply.rs:361` and
`apply_batch_parallel` at `parallel_apply.rs:391-395`) need the closure to
capture `self.strategy.clone()` so the rayon worker is not borrowing
`&self`.

### 4.4 Sites that need updating

- `crates/engine/src/concurrent_delta/parallel_apply.rs:482-488` - the
  verify body itself.
- `crates/engine/src/concurrent_delta/parallel_apply.rs:361` and
  `:391-395` - hand the captured `Arc<dyn ChecksumStrategy>` into the rayon
  closure rather than calling `Self::verify_chunk`.
- `crates/engine/tests/arc_drain_panic_recovery.rs:163` - no change
  needed if `new(concurrency)` keeps the MD5 default.
- `crates/engine/benches/parallel_receive_delta_perf.rs:256` - same; can
  optionally switch to `with_strategy` to bench against XXH3 once wired.
- Production wiring into the receiver pipeline at
  `crates/transfer/src/receiver/transfer/pipeline.rs:77-115` is out of scope
  for BR-3i.b; that step belongs with the activation of the
  `parallel-receive-delta` feature when the parity-gap (#4205 G2) closes.

## 5. Surprises / sharp edges

- `ChecksumStrategy` is `Send + Sync` by trait bound
  (`crates/checksums/src/strong/strategy/trait_def.rs:24`), so wrapping in
  `Arc` keeps `ParallelDeltaApplier: Send + Sync` (the applier already
  documents this at `parallel_apply.rs:240-241`).
- `ChecksumAlgorithm` (protocol) and `ChecksumAlgorithmKind` (checksums)
  are **two different enums** with different variant naming
  (`MD5` vs `Md5`). `protocol::ChecksumAlgorithm` is derived
  `Debug, Clone, Copy, PartialEq, Eq`
  (`crates/protocol/src/negotiation/capabilities/algorithms.rs:49`);
  `checksums::strong::strategy::ChecksumAlgorithmKind` is derived
  `Clone, Copy, Debug, Eq, PartialEq, Hash`
  (`crates/checksums/src/strong/strategy/kind.rs:13`). BR-3i.b will need a
  mapping helper, or it can pass through to the engine layer using the
  `checksums` enum directly to keep `engine` independent of `protocol`.
  Today there is no `From<ChecksumAlgorithm> for ChecksumAlgorithmKind` in
  the codebase - that conversion will need adding (or routing via
  `ChecksumFactory` which already owns the mapping at
  `crates/transfer/src/shared/checksum.rs:60-98`).
- `ChecksumFactory::from_negotiation`
  (`crates/transfer/src/shared/checksum.rs:75-98`) already centralises the
  "negotiated or default (MD5 / MD4 by protocol)" logic; BR-3i.b can reuse
  it to drive the future production constructor call.
- The applier is **feature-gated** behind `parallel-receive-delta`
  (`crates/engine/src/concurrent_delta/mod.rs:177-178,188-189`), so no
  default-build path observes the change. The Strategy boxing cost is
  paid once per applier (constructor), not per chunk.
- `parallel_apply.rs:366` already has a comment `let _ = verified.digest_len;
  // reserved for future stats wiring` - the digest length will become
  meaningful as soon as the strategy is wired in.
- The selector also returns `Box<dyn ChecksumStrategy>`
  (`selector.rs:87`); converting to `Arc` requires a one-line
  `Arc::from(box_value)` at the constructor boundary, no API change in
  `checksums`.

## One-line answer for BR-3i.b

Add `strategy: Arc<dyn checksums::strong::strategy::ChecksumStrategy>` to
`ParallelDeltaApplier`, defaulting to
`ChecksumStrategySelector::for_algorithm(ChecksumAlgorithmKind::Md5, 0)` in
`new(concurrency)` and exposing `with_strategy(concurrency, strategy)` for
the negotiated path.
