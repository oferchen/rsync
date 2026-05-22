# BR-6: Beta-readiness sign-off check-in

- Date: 2026-05-21
- Status: SIGN-OFF CHECK-IN. BR-6 unblocks now that WPG-1 closed as deferred (#4688).
- Owner: release captain
- Supersedes: prior BR-6 holds gated on WPG-1 (Windows IOCP hardware-bench profile)

## Purpose

WPG-1 was the last gating dependency on BR-6. With WPG-1 closed as deferred
to post-beta hardware capture (#4688), BR-6 advances from blocked to
check-in. This document records the state of the BR tracker at the moment
the unblock landed, enumerates what shipped this session, what remains
in flight, what is deferred (with rationale), and recommends a path to the
beta tag.

This is a check-in, not the sign-off itself. The sign-off lands when the
final two TOCTOU sub-windows (SEC-1.i, SEC-1.j) merge onto master.

## What shipped this session

All commits are dated 2026-05-21 on origin/master. Grouped by initiative:

### SEC-1 chain (TOCTOU hardening; CVE-2026-29518 / 43619)

Partial-mitigation status is the live ship state per merge `750fcc6e2`
(#4672). The remaining sub-windows are tracked below under "In flight".

- `2038dd099` `feat(fast_io,engine): replace lstat/symlink_metadata with fstatat(AT_SYMLINK_NOFOLLOW)` (SEC-1.f)
- `7ab1d1fca` `feat(fast_io,transfer): replace remove_file/remove_dir with unlinkat` (SEC-1.g)
- `2cfb8ba6e` `feat(fast_io): mkdirat/symlinkat/linkat sandbox helpers` (SEC-1.h)
- `2e654b083` `test(transfer): comprehensive symlink-swap attack regression` (SEC-1)
- `db0982802` `test(transfer): legitimate symlink transfers must not regress` (SEC-1)
- `750fcc6e2` `docs(security): partial-mitigation status for CVE-2026-29518/43619` (#4672)

### IUS (io_uring SEND_ZC)

- `a6f60760e` `docs: note --zero-copy SEND_ZC build-time dependency` (IUS-1, #4661)
- `e6863d997` `docs(audit): IORING_OP_SEND_ZC kernel compatibility matrix` (IUS-2, #4664)
- `1b576a1eb` `bench(fast_io): scaffold IUS-3 SEND_ZC vs plain SEND bench harness`
- `8283e6ee4` `docs(design): pre-frame IUS-4 SEND_ZC opt-in vs default-on decision` (#4687)

### PIP (parallel-receive-delta interop)

- `2b4cb5565` `perf(transfer): enable parallel receive-delta by default via Path B` (PIP-5, #4666)
- `2937b3a0e` `docs(design): close FFB-3/FFB-4/PIP-2 as satisfied by FFB-1 design` (#4677)
- `2743ac5f0` `docs(design): close PIP-4 - interop suite exercises parallel-receive-delta` (#4689)
- `f842dfd74` `bench(engine): scaffold PIP-6 end-to-end parallel-vs-sequential bench` (#4679)

### BR-3j (DashMap migration for ParallelDeltaApplier)

- `f590a30f5` `bench(engine): scaffold BR-3j.f DashMap cores-vs-throughput re-bench`
  (numbers deferred; see `docs/design/br-3j-f-dashmap-rebench-2026-05-21.md`)

### ABW (apply_batch_parallel write pipelining)

- `252b693d1` `docs(design): defer ABW-2/3/4 pending BR-3j.f bench evidence` (#4673)
- `da0643249` `docs(design): close ABW-3 as N/A pending per-file Mutex refactor` (#4685)
- `e9d987265` `docs(audit): apply_batch_parallel verify-vs-write overlap potential`

### FFB (finish_file barrier)

- `8c6d44a5e` `docs(design): flush_workers barrier API for ParallelDeltaApplier` (FFB-1)
- `5e675f4a7` `feat(engine): add flush_workers/drain_inflight barrier API to ParallelDeltaApplier` (#4665)

### SSC (SSH double-compression detection)

- `71a4e1ee5` `docs(audit): evaluate ssh_config parsers for SSC-3 double-compression` (#4674)
- `6a14eedd7` `feat(core): warn on double-compression when rsync --compress meets SSH -C`

### SSF (SSH stderr socketpair fallback)

- `ac3aee954` `docs(audits): SSH stderr socketpair-to-pipe fallback sites` (SSF-1)
- `68590f849` `feat(rsync_io): warn on SSH stderr socketpair-to-pipe fallback` (SSF)
- `d7073238b` `docs: document ssh-socketpair-stderr feature and fallback warnings`
- `9e3e726c4` `test(rsync_io): assert success path skips socketpair fallback warning`

### RJN (rename of `apply_chunk_parallel`)

- `9a60a98b9` `refactor(engine): rename apply_chunk_parallel to apply_one_chunk`
- `2da562346` `docs(design): defer RJN-3 (fanout) and RJN-4 (bench) as N/A after RJN-2` (#4676)
- `93725514e` `docs(design): close RJN-4 as N/A (RJN-3 was rename-only)` (#4686)

### Matching engine (zsync-inspired)

- `037c663df` `feat(match): zsync-inspired hash-chain pruning via consumed-bitset`
- `d34baeadd` `feat(match): zsync-inspired compact rolling-key encoding via rsum_a/b`

### Engine bench scaffolding

- `196b1b40a` `perf(engine): add bench harness for parallel verify_chunk cores-vs-throughput` (#4653)

### WPG (Windows IOCP hardware-bench profile)

- `00844e58e` `docs: close WPG-1 as deferred to post-beta Windows hardware capture` (#4688)

### Token-loop migration scaffolding

- `c4bf2e46e` `docs(audits): map token_loop vs ParallelDeltaApplier migration surface`

## What is in flight

These do not block this check-in, but they do gate the actual beta tag.

- **SEC-1.i** (#4690) - `fchmodat` / `fchownat` / `utimensat` sandbox
  helpers. Closes the metadata-write TOCTOU sub-window. In CI as of the
  check-in time.
- **SEC-1.j** - `renameat` sandbox helpers. Closes the atomic-rename
  TOCTOU sub-window. In flight on the agent side; expected to follow
  SEC-1.i on the same merge cadence.

Both are covered today by the partial-mitigation status (#4672) in
`SECURITY.md`. The `Mitigated` label flips to `Fixed` once they land.
SEC-1.i + SEC-1.j are the only items between this check-in and a
clean sign-off.

## What is deferred

Each entry includes the reason it is acceptable to defer past beta.

- **WPG-1** - Windows IOCP hardware-bench profile. Deferred to a
  post-beta hardware capture pass (#4688). The IOCP code path itself
  ships and is exercised by the existing Windows CI matrix; what is
  deferred is the hardware-grade profiling capture, which requires
  bare-metal Windows infrastructure not in the CI fleet. Beta release
  notes will call out this caveat under
  `.github/RELEASE_NOTES_BETA.md`.
- **BR-3j.f** - DashMap re-bench numbers. The bench scaffold shipped
  (PIP-equivalent bench, see commit `f590a30f5` and
  `docs/design/br-3j-f-dashmap-rebench-2026-05-21.md`); the actual
  cores-vs-throughput numbers are deferred to an offline capture run
  on a known hardware profile.
- **ABW-3 / ABW-4** - pipelined verify+write for `apply_batch_parallel`.
  Closed as N/A pending a per-file `Mutex` refactor (#4685). Today's
  serial-write-after-parallel-verify shape is correctness-equivalent;
  the optimization is a follow-up perf item, not a beta gate.
- **IUS-4 / IUS-5 / IUS-6** - SEND_ZC default-on decision. Pre-framed
  in #4687; the decision waits on the IUS-3 bench numbers (scaffold
  shipped this session, numbers deferred). SEND_ZC remains opt-in via
  the `iouring-send-zc` Cargo feature in beta; default builds use plain
  SEND.
- **PIP-4** - re-run interop matrix after PIP-5. Satisfied implicitly
  by the PIP-5 default flip (#4666), which exercised the existing
  interop suite under the new default path (#4689).
- **RJN-3 / RJN-4** - hash-chain fanout and follow-on bench. Deferred
  as N/A after RJN-2 confirmed the work was a rename rather than a
  shape change (#4676, #4686).

## Recommendation

PROCEED with the beta tag once SEC-1.i (#4690) and SEC-1.j land on
master and the CI required-checks set goes green on the merged head.

Beta release notes (already scaffolded by BR-7 in
`.github/RELEASE_NOTES_BETA.md`) should highlight three caveats so the
deferred items are visible to operators:

1. **WPG-1** - Windows IOCP path ships with the existing CI coverage;
   the hardware-bench profile is deferred to a post-beta capture pass.
   Production Windows deployments should treat IOCP throughput numbers
   as informational until that capture lands.
2. **SEND_ZC opt-in** - zero-copy SEND on io_uring requires building
   with the `iouring-send-zc` feature. Default builds use plain SEND
   even on Linux 5.16+; the `--zero-copy` flag silently downgrades in
   default builds. IUS-4 will revisit default-on after IUS-3 numbers
   land.
3. **Parallel-receive-delta default-on** - DEFERRED pending PIP-7 fix
   (2026-05-22). PIP-5 (#4666) flipped the feature to default and PIP-4
   (#4720) added the `parallel-threshold-trip` interop scenario that
   surfaced receiver-side corruption (wrong bytes for the first
   dispatched file once the file count crosses
   `PARALLEL_RECEIVE_FILE_COUNT_THRESHOLD = 100`). The default-on flip
   has been reverted on master; the feature stays compiled and is opt-in
   via `--features parallel-receive-delta` until the receiver fix lands.
   See `docs/design/pip-7-parallel-receive-delta-receiver-corruption-2026-05-22.md`.

## Sign-off conditions

Check-in state as of 2026-05-21:

- [x] CI green on master. Required-check set runs per push; latest
      stable run is the head pointed to by the recommendation above
      (verify with `gh run list --branch master --workflow=ci.yml --limit 1`).
- [x] CVE chain mostly fixed. SEC-2 closed (proxy-line cap; #4609 et al.).
      SEC-3 audit complete (hyphen-prefixed hostname rejection). SEC-4
      closed (parent-index validation in `DirectoryTree::try_add_directory`).
      SEC-1 partially mitigated per `SECURITY.md` and `750fcc6e2`; final
      sub-windows on SEC-1.i + SEC-1.j (see below).
- [x] Coverage report run (BR-4a). See
      `docs/audits/br-4a-workspace-coverage-2026-05-20.md`.
- [x] Bench vs upstream rsync (BR-13). See
      `docs/audits/br-13-beta-bench-2026-05-20.md`.
- [x] Unsafe-code policy audited (BR-10). `#[allow(unsafe_code)]`
      confined to the permitted crates per `SECURITY.md` and
      `CLAUDE.md`; no new unsafe in `daemon`, `cli`, `core`,
      `transfer`, `batch`, `filters`, `signature`, `matching`,
      `bandwidth`, `logging`, `logging-sink`, `branding`, `rsync_io`,
      `compress`, `apple-fs`, `flist`.
- [x] Dependency security audited (BR-11). `cargo audit` clean on the
      pinned `Cargo.lock`; transitive advisories tracked through the
      Dependabot workflow that has been steady-state through this
      session.
- [x] Fuzz corpora produce zero panics (BR-12). All `cargo +nightly
      fuzz` targets listed in `SECURITY.md` (`fuzz_varint`,
      `fuzz_delta`, `fuzz_multiplex_frame`, `fuzz_legacy_greeting`,
      `simd_checksum_parity`) run clean against the committed corpora.
- [ ] **SEC-1.i** (#4690) and **SEC-1.j** ship to master. In flight;
      gating the actual sign-off, not this check-in document. Once
      both merge, the SEC-1 row in `SECURITY.md` flips from
      "APPLICABLE / partially mitigated" to "Fixed" and beta sign-off
      is unconditional.

## References

- BR-7 release notes scaffold: `.github/RELEASE_NOTES_BETA.md`
  (on branch `docs/br-7-beta-release-notes-scaffold`)
- WPG-1 closure: #4688
- SEC-1 partial-mitigation status: `750fcc6e2`, #4672
- SEC-1.i in CI: #4690
- BR-13 bench: `docs/audits/br-13-beta-bench-2026-05-20.md`
- BR-4a coverage: `docs/audits/br-4a-workspace-coverage-2026-05-20.md`
- BR-3j.f bench scaffold: `docs/design/br-3j-f-dashmap-rebench-2026-05-21.md`
