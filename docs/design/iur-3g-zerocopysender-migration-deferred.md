# IUR-3.g: defer ZeroCopySender per-thread migration

Tracking task: **IUR-3.g**. This is a deferral record. No `.rs` files or
`Cargo.toml` change. The companion design notes are:

- `docs/design/iur-2-per-thread-rings.md` sections 1.1, 1.3, and 7
  (row IUR-3.g) - the IUR-2 layout that marked `ZeroCopySender::ring`
  "deferred" and sequenced it as "blocked-on IUS-8".
- `docs/design/iur-3f-shared-rings-decision.md` section 5 row
  `ZeroCopySender::ring` - cross-references this record.
- IUR-3.a..e (PRs #4793, #4804, #4807, #4806, #4811) - per-thread
  migration of `file_writer`, `file_reader`, `socket_writer`, and the
  `BgidLease`.
- `docs/design/ius-4-decision-2026-05-22.md` - kept `iouring-send-zc`
  opt-in under the data-missing branch of the IUS-4 rule.
- `docs/design/ius-7a-trait-surface.md` section 14 (IUS-8.a..c) - the
  `IoUringBackend` / `IoUringSubmitter` trait abstraction.

## 1. Decision

**Defer the `ZeroCopySender` per-thread migration.** The path stays on
the existing `Arc<Mutex<RawIoUring>>` topology shipped at
`crates/fast_io/src/io_uring/send_zc.rs:282-292`. It is **explicitly
excluded** from the IUR-3.a..f scope and is not scheduled. The
decision holds regardless of whether `per-thread-rings` is on. Re-open
only when one of the section 5 trigger criteria fires.

## 2. What `ZeroCopySender` is today

`ZeroCopySender` (`crates/fast_io/src/io_uring/send_zc.rs:282-464`) is
the higher-level wrapper exposed when the `iouring-send-zc` cargo
feature is on (`crates/fast_io/Cargo.toml:141`). Layout:

- `ring: Arc<Mutex<RawIoUring>>` - owns its own ring via
  `IoUringConfig::default().build_ring()` in `Self::new`
  (`send_zc.rs:316`), or accepts a caller-supplied shared ring via
  `from_shared_ring` (`send_zc.rs:345`). The mutex is taken inside
  every submission (`send_zc.rs:419-422`, `:436-439`).
- `buffers: Option<RegisteredBufferGroup>` - a pinned
  `IORING_REGISTER_BUFFERS` pool, 8 slots of 256 KiB. Registration is
  best-effort; failure falls back to an unregistered SEND_ZC path
  still zero-copy at the socket layer.

The wrapper does **not** consult `per_thread_ring::with_ring` (the
IUR-3.a primitive) and does not lease from the IUR-3.e per-thread
`BgidLease`; its registered buffers are owned per-wrapper.

Note: the **default** socket-send path
(`crates/fast_io/src/io_uring/socket_writer.rs`) is already per-thread
via IUR-3.d. When `iouring-send-zc` is enabled, `IoUringSocketWriter`
routes payloads `>= send_zc::SEND_ZC_DISPATCH_MIN_BYTES` (4 KiB)
through the per-thread ring's SEND_ZC dispatch
(`socket_writer.rs:43-46`). Only the standalone `ZeroCopySender`
wrapper remains on its own ring.

## 3. Why deferred (rationale)

Three reasons, in order of weight:

1. **Opt-in code path. Low contention surface today.** The
   `iouring-send-zc` feature is default-off (IUS-4 keep-opt-in).
   Default builds dispatch plain `IORING_OP_SEND` through the
   per-thread ring and never construct a `ZeroCopySender`. There is
   no observed hot-path complaint to relieve.
2. **The mutex is uncontested in practice.** `from_shared_ring` has
   no production caller. At most one sender exists per session, so
   the `Arc<Mutex<RawIoUring>>` acquire is a fast-path uncontended
   `compare_exchange`. Per-thread migration would dissolve a
   contention that the bench harness cannot measure.
3. **A clean migration depends on IUS-8.** `ZeroCopySender` should
   not own its ring at all once the `IoUringBackend` /
   `IoUringSubmitter` trait surface (IUS-8.a..c) lands. The right
   shape is `ZeroCopySender<S: IoUringSubmitter>` where a per-thread
   ring is one valid `S` and a shared ring is another. Migrating to
   per-thread storage ahead of the trait freezes the wrapper into a
   layout it should outlive.

## 4. What a future migration would do

A migration scheduled by one of the section 5 triggers would:

- Replace `ring: Arc<Mutex<RawIoUring>>` with a generic
  `S: IoUringSubmitter` parameter sourced from
  `per_thread_ring::with_ring`, mirroring the layout IUR-3.d gave
  `IoUringSocketWriter`.
- Acquire registered-buffer slots through the per-thread `BgidLease`
  from IUR-3.e instead of the per-wrapper
  `RegisteredBufferGroup`. Slot churn amortises across every
  per-thread ring user, not just SEND_ZC.
- Retire `from_shared_ring` once no caller depends on it; the
  per-thread `with_ring` helper and `SessionRingPool` are the
  supported topologies after IUR-3.a..e.

Files to touch (rough sketch):

- `crates/fast_io/src/io_uring/send_zc.rs` - rewrite `ZeroCopySender`
  field set; route `send_zc` through `with_ring`; delete
  `from_shared_ring`.
- `crates/fast_io/src/io_uring/socket_writer.rs` - already per-thread;
  only standalone `ZeroCopySender` consumers (none today) need
  re-wiring.

Test coverage available to verify the migration:

- `crates/fast_io/tests/io_uring_send_zc.rs:48,146,170` - three
  `zero_copy_sender_*` integration tests (1 MiB socket-pair
  round-trip, empty-buffer rejection, fd accessor) re-point at the
  per-thread topology with no behavioural change.
- `crates/fast_io/src/io_uring/send_zc.rs:477-579` - in-crate unit
  tests: probe cache, CQE classification, empty-buffer rejection,
  64 KiB loopback `try_send_zc`.
- `crates/fast_io/benches/ius_3_send_zc_vs_send.rs` - IUS-3
  SEND_ZC-vs-SEND harness (gated on `OC_RSYNC_BENCH_IUS_3=1` +
  `--features iouring-send-zc`).

## 5. Trigger criteria

Elevate this from "deferred" to "scheduled" when **any one** of the
following is observed:

- **`iouring-send-zc` flips to default-on.** IUS-4 will revisit the
  opt-in posture once IUS-3 multi-kernel bench numbers exist
  (>= 10% throughput on >= 2 of 4 IUS-3 workloads, no workload
  regressing > 2%, release-policy sign-off for an io_uring socket-send
  floor raise from 5.6 to 6.0, per `crates/fast_io/Cargo.toml:128-141`).
  A default flip activates SEND_ZC dispatch on every supported build;
  the `Arc<Mutex<RawIoUring>>` becomes a real per-process hot path.
- **IUS-8.a..c land (IoUringBackend trait shipped).** Migration
  becomes a mechanical refactor once `ZeroCopySender` can take
  `S: IoUringSubmitter`.
- **A second `from_shared_ring` caller appears.** Today
  (`send_zc.rs:345`) has no production caller; a multi-sender fan-out
  would turn the mutex into a measurable contention point matching
  the IUR-1 section 3.4 model that drove IUR-3.b..d.
- **Per-thread bench shows SEND_ZC user contention.** A bench in the
  `OC_RSYNC_BENCH_IUR_*` family attributing > 5% of SEND_ZC submit
  CPU to the mutex acquire (or showing starvation of a parallel
  SEND_ZC fan-out) shifts the contention model.

The per-thread ring primitive (IUR-3.a) is a contention-relief tool
for high-frequency multi-producer factories. It is the right lever
for `ZeroCopySender` only once the wrapper is actually exercised by
multiple producers, which today it is not.

## 6. Cross-references

- IUR-3.a (PR #4793) - `per_thread_ring::with_ring` primitive.
- IUR-3.b..d (PRs #4804, #4807, #4806) - `file_writer`,
  `file_reader`, `socket_writer` per-thread migrations. IUR-3.d also
  covers the SEND_ZC fast-path on the per-thread ring.
- IUR-3.e (PR #4811) - per-thread `BgidLease` from the BGE-4 pool.
- IUR-3.f - keep one-shot probes + disk-commit ring shared.
- IUS-4 (`docs/design/ius-4-decision-2026-05-22.md`) - keep
  `iouring-send-zc` opt-in (data-missing branch).
- IUS-7 / IUS-8 (`docs/design/ius-7a-trait-surface.md` section 14) -
  `IoUringBackend` trait surface and Linux impl.
- `crates/fast_io/Cargo.toml:141` -
  `iouring-send-zc = ["io_uring"]`; the comment at lines 128-141
  documents the IUS-4 rationale for keeping it opt-in.
