# Evaluate Arc-wrapped WorkQueueSender for multi-generator fan-in

Tracking: oc-rsync task #1383. This note evaluates whether wrapping
`WorkQueueSender` in `Arc` is a useful primitive for multi-generator
fan-in, or whether the existing `Clone`-based design (#1611) already
covers the use cases.

## 1. Cross-references

- **#1382** - Multi-producer design scoping (done). Established that
  multi-producer fan-in is opt-in and that the SPMC ordering contract
  is mandatory for default builds.
- **#1404** - `Clone` impl on `WorkQueueSender` behind the
  `multi-producer` cargo feature (done). Delegates directly to
  `crossbeam_channel::Sender::clone`.
- **#1612** - Multi-producer integration tests (done). Cover ordering,
  drop-disconnect, atomic sequence coordination, and backpressure.

A longer design exploration lives at `docs/design/arc-workqueuesender.md`
(#1610). This note is the focused #1383 evaluation question only.

## 2. Question

Does `Arc<WorkQueueSender>` add value over the existing `Clone` route?

`crossbeam_channel::Sender` is already cheap to clone: internally it is
an `Arc` over the channel state, so each `clone()` is one atomic
refcount bump on the same backing structure. Wrapping `WorkQueueSender`
(itself a thin newtype around the crossbeam sender) in another `Arc`
introduces a second refcount layer over the first. The runtime cost
versus `Clone` is negligible; the question is whether the API ergonomics
justify the indirection.

## 3. Use cases

The motivating fan-in scenarios from #1382:

- **Multi-root local transfers** (`oc-rsync /a/ /b/ /c/ dst/`) - each
  source root is independent. The local copy path bypasses
  `WorkQueue` today and uses `rayon::par_iter` directly, so this
  scenario does not need fan-in until #1565 lands.
- **Parallel source enumeration** with `--files-from` (#1573 evaluated)
  - sender-side, not receiver-side. `WorkQueue` is receiver-side, so
  the primitive does not apply unless a sender-side delta pipeline
  emerges.
- **`--inc-recurse` generator decomposition** - ruled out by the wire
  ordering invariant.

In every case where fan-in is *plausibly* useful, the consumer side
already needs an external `AtomicU64` sequence coordinator (the
reorder buffer relies on monotonic sequence numbers). Whether
producers obtain the sender via `Arc::clone` or `WorkQueueSender::clone`
does not change that requirement.

## 4. Recommendation

**Keep the Clone-based design from #1611. Do not introduce
`Arc<WorkQueueSender>` as a separate primitive.**

Rationale:

1. The crossbeam sender is already an internal `Arc`. Wrapping it
   externally duplicates the indirection without adding behaviour.
2. `Clone` is gated by the `multi-producer` cargo feature, so the
   default build still gets the `!Clone` SPMC compile-time check from
   #1614.
3. Callers who legitimately need fan-in (4.x scenarios above) gain
   nothing from `Arc`: the sequence coordinator is the actual
   complexity, not the sender-cloning mechanism.
4. Test harnesses already use `Clone` (per #1612) without friction.

If a future call site genuinely needs shared ownership without
`Clone`, the caller can wrap the sender in `Arc` at the call site
(`Arc::new(tx)`) without library support. No type-system or feature-flag
change is required.

## 5. Risks of adopting Arc-wrapped sender

- **Extra indirection.** Two refcount layers (`Arc` over crossbeam's
  internal `Arc`) for no semantic gain. Each send pays one extra
  pointer chase.
- **Hides clone cost from caller.** `Arc::clone` and
  `crossbeam_channel::Sender::clone` are both cheap atomic ops, but
  `Arc::clone` looks free at the call site while implying shared
  mutability via interior atomic state on the underlying channel.
  Reviewers may miss that the underlying channel is already
  multi-producer-capable.
- **Two ways to do the same thing.** Ships both `Clone` (feature-gated)
  and `Arc` (always available) as fan-in routes. The audit in
  `multi_producer_audit.rs` becomes harder to keep authoritative.
- **No type-system enforcement.** `Arc<WorkQueueSender>` does not
  surface "this is shared" in the type. A reviewer cannot tell from
  the signature whether SPMC is intended.

## 6. Outcome

Close #1383 with no code change. #1611 covers the design space. Update
`multi_producer_audit.rs` if a future call site revisits the question.
