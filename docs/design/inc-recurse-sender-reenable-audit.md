# INC_RECURSE sender re-enable audit (#2089)

Tracker: #2089. Companion to #2088 (regression investigation, completed)
and #2196-#2200 (instrumentation, completed). No code changes - this is
the audit and re-enable decision record.

## 1. Current state

### 1.1 The gate

The sender advertises the `'i'` (INC_RECURSE) capability bit only when
the client config carries `inc_recursive_send = true`. The default is
`false`.

Single source of truth for the bit:

- `crates/transfer/src/setup/capability.rs:138` -
  `pub fn build_capability_string(allow_inc_recurse: bool) -> String`.
- `crates/transfer/src/setup/capability.rs:144` -
  `if mapping.requires_inc_recurse && !allow_inc_recurse { continue; }`
  strips the `'i'` row from `CAPABILITY_MAPPINGS` when the gate is
  closed.
- `crates/transfer/src/setup/capability.rs:40-48` - the `'i' ->
  CF_INC_RECURSE` row with `requires_inc_recurse: true`.

Call sites that drive the gate:

- SSH push: `crates/core/src/client/remote/invocation/builder.rs:184-186`
  - `args.push(OsString::from(build_capability_string(self.config.inc_recursive_send())))`.
- Daemon push:
  `crates/core/src/client/remote/daemon_transfer/orchestration/arguments.rs:167`
  - `args.push(build_capability_string(config.inc_recursive_send()))`
  inside the `protocol.as_u8() >= 30` branch.

Default wiring:

- Storage: `crates/core/src/client/config/client/mod.rs:154` (struct
  field) and `crates/core/src/client/config/client/mod.rs:319` (Default
  impl, `inc_recursive_send: false`).
- Builder default:
  `crates/core/src/client/config/builder/mod.rs:437` -
  `inc_recursive_send: self.inc_recursive_send.unwrap_or(false)`.
- Setter: `crates/core/src/client/config/builder/performance.rs:234`.
- Getter:
  `crates/core/src/client/config/client/performance.rs:208` (returns
  the field; doc comment incorrectly says "Default `true`" - stale
  relative to the actual `false` default and worth a cleanup in the
  flip PR).

### 1.2 What the signal flips

When `inc_recursive_send = true`:

- `build_capability_string(true)` includes `'i'` in the `-e.` token of
  the remote argv (SSH) or daemon command list (daemon).
- The peer's `compat.c:720 set_allow_inc_recurse()` sees `'i'` in
  `client_info`, leaves `allow_inc_recurse` at the default of `1`, and
  the negotiated `CompatibilityFlags::INC_RECURSE` bit becomes part of
  the compat-flags exchange.
- Our sender then takes the incremental path in
  `crates/transfer/src/generator/file_list/inc_recurse.rs` and
  `crates/transfer/src/generator/protocol_io.rs:214-386`
  (`send_file_list`, `encode_and_send_segment`, `send_flist_eof`).

When `false`:

- The `'i'` row is suppressed; upstream's `set_allow_inc_recurse()`
  hits `compat.c:177-178` (daemon) or the equivalent SSH branch, clears
  `allow_inc_recurse`, and `CF_INC_RECURSE` stays off.
- Our sender sends a single monolithic file list and the receiver
  buffers it whole, matching pre-protocol-30 behaviour.

The pull direction is unaffected: when we are the receiver, the remote
sender controls the advertisement and the flag is negotiated normally.

### 1.3 Upstream reference

- `target/interop/upstream-src/rsync-3.4.1/compat.c:161-179` -
  `set_allow_inc_recurse()`. The capability gate.
- `target/interop/upstream-src/rsync-3.4.1/compat.c:712-734` -
  capability bit parsing.
- `target/interop/upstream-src/rsync-3.4.1/options.c:3003-3050` -
  `maybe_add_e_option()`. Builds the `-e.` capability token.
- Upstream defaults `allow_inc_recurse = 1`; the regression that
  forced the disable was that *we* did not handle the `'i'` advert
  efficiently, not that upstream rejects the bit.

## 2. Why it is gated

### 2.1 Commit chain

1. `854aa753a feat(transfer): enable INC_RECURSE sender by default
   (#1862)` - sender default flipped from `false` to `true`.
2. `39d47722b feat(transfer): enable INC_RECURSE sender by default
   (#1862) (#3557)` - squash on master, 2026-05-02.
3. `d51c95c6a chore: release v0.6.1` - shipped with the default on.
4. `bd12b6ac5 fix(core): default inc_recursive_send to false to fix
   push regression (#1862)` - revert, 2026-05-06.
5. `b3a264061 fix(core): restore inc_recursive_send=false default to
   fix v0.6.1 push regression (#3744)` - PR-shaped commit that landed
   the revert on master.

### 2.2 Regression statement

From `bd12b6ac5` / `b3a264061`:

> PR #3557 (commit 39d47722b) flipped the sender-side INC_RECURSE
> default to true, advertising the `'i'` capability bit on push
> transfers. This caused severe performance regressions in v0.6.1 -
> push paths went 95-201x slower over both SSH and daemon transports.

Cross-reference:
`docs/audits/incremental-flist-memory-bench.md:267-275` characterises
the regression as "a syscall-rate pathology in the sender's
per-segment dispatch loop", not a memory pathology. The sender's
walk-and-buffer pattern is unchanged by enabling INC_RECURSE, so peak
RSS is similar with or without it; the wall-clock cliff comes from
how the per-segment dispatch interacts with the wire writer.

The existing investigation
`docs/investigations/inc-recurse-sender-regression.md` documents the
full disable mechanism, lists four candidate causes (flush cadence,
NDX request stall, per-segment writer cache churn, `DirectoryTree`
traversal cost) and defined the five instrumentation points that
became tasks #2196-#2200.

## 3. Instrumentation that now exists

Tasks #2196 through #2200 landed five process-global counters on the
generator hot path. All five emit at end-of-transfer via the existing
`GeneratorContext::run` finalize block and the receiver mirror in
`receiver/transfer/phases.rs::finalize_transfer`.

| ID  | Task  | PR    | What it measures                                            | Surface                                                                  |
|-----|-------|-------|-------------------------------------------------------------|--------------------------------------------------------------------------|
| I1  | #2196 | #4103 | First-byte latency of `send_file_list` (build-then-send)    | `GeneratorStats.flist_first_byte_latency`, `info_log!(Flist, 1, ...)`, `tracing::info!(target = "rsync::flist", ...)` |
| I2  | #2197 | #4143 | `encode_and_send_segment` call count + cumulative ns        | `debug_log!(Genr, 1, "generator encode_and_send_segment totals: calls=N elapsed_ns=M")` |
| I3  | #2198 | #4121 | `writer.flush()` invocations on the generator transfer loop | Single `flush_with_count()` helper, end-of-transfer debug log            |
| I4  | #2199 | #4120 | `wire_to_flat_ndx` / `flat_to_wire_ndx` call count + worst-case partition_point comparison depth | `debug_log!(Genr, 1, ...)` per side, `tracing::debug!` at `rsync::generator::ndx_convert` / `rsync::receiver::ndx_convert` |
| I5  | #2200 | #4142 | `prepare_pending_acl` call count + cumulative ns            | `debug_log!(Genr, 1, "generator prepare_pending_acl totals: ...")`       |

Where the numbers come out:

- All five counters surface through `--debug=GENR` (or `info` / `flist`
  facets) on the generator side, plus `tracing` spans when the
  optional `tracing` feature is enabled.
- I1 is also exposed on the public `GeneratorStats` struct so callers
  (the bench harness in particular) can read it programmatically.
- Counters are process-global `AtomicU64` pairs; the sampling cost is
  one fetch-add per call. The five PRs were explicitly designed to be
  low overhead so they can stay compiled in unconditionally.

What is *not* yet captured:

- No instrumentation has been wired into the bench harness
  (`scripts/benchmark_flist_memory.sh`) to consume the I1-I5 totals
  alongside `strace -c` syscall counts. The counters exist but the
  Mode C row in the audit's bench plan has not been re-run with them
  emitting.
- No regression in the test suite asserts an I1-I5 budget. They are
  observational only.

## 4. Recent perf verification (#2209)

`docs/audits/ssh-daemon-perf-verification.md` records the result of
PR #4154 (SSH socketpair stderr drain + russh `~/.ssh/config`) on the
benchmark workflow. Data source: `benchmark.yml` run `25964839057` on
tag `v0.6.2`, SHA `c99bbbc6d`.

| Mode               | Upstream mean | oc-rsync mean | Ratio          | Target             |
|--------------------|---------------|---------------|----------------|--------------------|
| SSH push initial   | 0.596 s       | 0.769 s       | slower 1.29x   | within 5%          |
| SSH push no-change | 0.346 s       | 0.528 s       | slower 1.53x   | within 5%          |
| Daemon push init   | 0.326 s       | 0.435 s       | slower 1.33x   | 2x or faster       |
| Daemon push noch.  | 0.137 s       | 0.256 s       | slower 1.87x   | 2x or faster       |

Verdict from #2209: "pass-with-caveat". Both modes recovered from the
prior 120 s / 30 s harness timeouts on v0.6.1, but neither meets the
absolute "SSH on par" or "daemon 2x faster" project target. The gap is
pre-existing and unrelated to PR #4154.

Critically, #2209's benchmark workload is 148.3 MB / 10000 files. This
is **not** the 1M-file shallow workload where the INC_RECURSE
regression manifested. The benchmark.yml workload is small enough that
INC_RECURSE provides no measurable wire-format benefit, and large
enough that the disable does not introduce a memory cliff. The "1.29x
to 1.87x" gap in #2209 is therefore a baseline transport-and-pipeline
gap that exists *with INC_RECURSE off*; it is not evidence that
turning INC_RECURSE on would close it.

## 5. Gap analysis

What we know now that we did not know at #3744:

- The five most likely per-segment dispatch hotspots are instrumented.
  Operators can run a 1M-file Mode C push with `--inc-recursive-send`
  and read off I1-I5 totals at end-of-transfer.
- PR #4154 has unblocked the SSH push and daemon push paths from the
  unrelated goodbye-phase deadlock, so a re-run of the 1M Mode C
  benchmark is no longer dominated by the harness timeout. A profile
  run is now reproducible.

What we do not yet know:

- The actual I1-I5 totals for a 1M-file push with INC_RECURSE on, vs
  the same workload with INC_RECURSE off (the audit's Mode C vs Mode B
  comparison). The instrumentation exists; no run has published the
  comparison.
- Which of the four candidate causes documented in
  `docs/investigations/inc-recurse-sender-regression.md` section 3.3
  is dominant. The 95-201x factor points at a per-segment system
  effect, but the instrumentation has not yet been used to localise
  it.
- Whether the Mode C wall-clock targets in
  `docs/investigations/inc-recurse-sender-regression.md` section 5.1
  (within 5-15% of Mode B depending on workload shape) can be met by
  tuning alone or require a structural change to the per-segment
  dispatch loop.
- Whether the syscall budget targets in section 5.3 (`sendto` /
  `write` / total syscalls within 1.2-1.3x of Mode B) are achievable
  without changing the buffered-writer boundary handling in
  `encode_and_send_segment` and `run_transfer_loop`.

In short: we have the disable, the diagnosis hypotheses, and the
counters to test them; we do not yet have a measurement that says the
underlying cause is fixed or tuned.

## 6. Recommendation

**Defer the flip.** Do not change the default in this PR or in any
near-term follow-up until the instrumentation that landed under
#2196-#2200 has been run against a 1M-file Mode C push and the
re-enable criteria in
`docs/investigations/inc-recurse-sender-regression.md` section 5 have
been met on the same nightly run.

Specifically, the flip from
`inc_recursive_send: self.inc_recursive_send.unwrap_or(false)` to
`unwrap_or(true)` in
`crates/core/src/client/config/builder/mod.rs:437` (and the
corresponding storage default at
`crates/core/src/client/config/client/mod.rs:319` and Default impl)
may proceed **only when all of the following are true on the same
nightly benchmark run**:

1. **Wall-clock parity vs Mode B (INC_RECURSE off)** at the workload
   shapes from
   `docs/investigations/inc-recurse-sender-regression.md` section 5.1.
   The 1M shallow cell is the strictest: Mode C must be within 10% of
   Mode B. Any cell where Mode C is slower than Mode A by any margin
   is a hard fail.
2. **Syscall budget** from `strace -c` over a steady-state window of
   the 1M shallow push: `sendto` and `write` per second within 1.2x of
   Mode B, total syscalls within 1.3x of Mode B (section 5.3).
3. **I1-I5 totals published** alongside the benchmark output, with
   - I3 flush rate (`flush_with_count` totals) per second within 1.5x
     of Mode B at the 1M shallow workload, and
   - I2 `elapsed_ns / call` per-segment dispatch within 2x of the Mode
     B equivalent on the same scheduler boundary.
   These two thresholds are the strongest predictors of the
   per-segment dispatch pathology and should be the gating signal.
4. **Interop validation** per section 5.4: `tools/ci/run_interop.sh`
   push test passes with `--inc-recursive-send` against upstream
   3.0.9, 3.1.3, and 3.4.1, with no `flist.c:2652-2659` "ABORTING due
   to invalid path from sender" on any version. The
   `test(interop): INC_RECURSE sender fuzz against upstream rsync
   (#1864)` work in `94b7648a7` provides the substrate for this gate.
5. **Two consecutive nightly runs** showing 1-4 green before the flip
   PR opens, to filter run-to-run noise.

Flip when:

- 1M-file shallow Mode C push is within 10% of Mode B on a local
  `file://` transport in the `rsync-profile` container, **and**
- the I3 flush rate ratio Mode C / Mode B is below 1.5 on the same
  workload, **and**
- `run_interop.sh` push is green against all three upstream pinned
  versions with `--inc-recursive-send`, **and**
- two consecutive nightly runs of the above all pass.

Defer until:

- the bench harness has been extended to emit I1-I5 alongside
  hyperfine wall-clock for a Mode C push, **and**
- a profile run has localised which of the four candidate causes in
  `docs/investigations/inc-recurse-sender-regression.md` section 3.3
  is the dominant cost driver, **and**
- the dominant cost driver has been addressed in a separate PR (a
  flush-cadence change, an interior-flush avoidance in
  `encode_and_send_segment`, or whatever the I1-I5 numbers point at).

Reject only if:

- the I1-I5 measurements show the regression is structural to our
  walk-then-partition design (i.e., the sender cannot meet upstream
  wire syscall budgets without interleaving the walk with the segment
  dispatch). In that case the flip is shelved until the streaming-walk
  rework (#1050) lands.

The 1M-file workload is not a niche: it is the regime in which
INC_RECURSE matters (sub-list bytes saved vs initial-segment latency
won). Smaller workloads gain nothing from flipping the default; larger
workloads gain everything and are exactly where the regression bit.

## 7. Five-step plan when the flip is justified

The flip itself is a single-line change in
`crates/core/src/client/config/builder/mod.rs:437` plus the matching
default in `crates/core/src/client/config/client/mod.rs:319` and a
test update in
`crates/core/src/client/config/client/performance.rs:322` and
`crates/core/src/client/config/builder/tests.rs`. The work around
that change is the verification:

1. **Wire the harness** (no behaviour change): extend
   `scripts/benchmark_flist_memory.sh` Mode C to capture I1-I5
   counters from the generator stats output, alongside the existing
   wall-clock and peak-RSS captures. Output a side-by-side Mode B vs
   Mode C table in the bench summary. Commit prefix `chore(bench):`.
2. **Profile run on 1M shallow + 1M deep + 100K shallow**: trigger
   `benchmark.yml` (or the local
   `scripts/benchmark_flist_memory_daemon.sh`) against the three
   upstream versions inside `rsync-profile`. Publish the I1-I5
   totals, syscall budgets, and wall-clock side by side in a new
   `docs/audits/inc-recurse-sender-mode-c-profile.md`. Commit prefix
   `docs(audits):`.
3. **Address the dominant cost driver**: based on the profile,
   land one of (a) interior-flush avoidance in
   `encode_and_send_segment`, (b) flush-cadence tuning in
   `run_transfer_loop`, (c) `ndx_segments` partition_point fast path
   for small tables, or (d) `prepare_pending_acl` short-circuit when
   `--acls` is off. Commit prefix `perf(transfer):`.
4. **Re-run** the harness from step 1 on two consecutive nightly runs;
   confirm Mode C meets all four gating criteria from section 6.
   Update `docs/audits/inc-recurse-sender-mode-c-profile.md` with the
   green run IDs. Commit prefix `docs(audits):`.
5. **Flip the default**: change `unwrap_or(false)` to
   `unwrap_or(true)` in
   `crates/core/src/client/config/builder/mod.rs:437`, the matching
   storage default in
   `crates/core/src/client/config/client/mod.rs:319`, and the
   `inc_recursive_send_default_is_false` tests in
   `crates/core/src/client/config/client/performance.rs:322` and
   `crates/core/src/client/config/builder/tests.rs:1592`. Update the
   doc comment in
   `crates/core/src/client/config/client/performance.rs:189-210` so
   it matches reality (it already claims "Default `true`"; the flip
   PR finally makes that true). Commit prefix `perf(transfer):` and
   reference both #1862 and #2089 in the PR body.

## 8. Out of scope

- Streaming-walk redesign (interleave filesystem walk with sub-list
  dispatch). That removes the sender-side memory ceiling and would
  shift the I1 measurement substantially, but is multi-PR work
  tracked under #1050 and not a prerequisite for re-enabling the
  capability bit.
- Daemon-specific syscall profiles. Mode C should be measured on
  local `file://` first to isolate the per-segment dispatch path
  from transport variance.
- The doc-comment drift in
  `crates/core/src/client/config/client/performance.rs:189-210` (the
  comment says "default `true`" while the code returns `false`).
  That fixes itself when the flip PR lands; it is not worth a
  drive-by patch.

## 9. Cross-references

- Investigation: `docs/investigations/inc-recurse-sender-regression.md`
  (#2088, completed).
- Perf verification: `docs/audits/ssh-daemon-perf-verification.md`
  (#2209, completed).
- Memory bench plan:
  `docs/audits/incremental-flist-memory-bench.md`.
- Disable commits: `bd12b6ac5`, `b3a264061`.
- Enable-then-revert commit: `39d47722b` / `854aa753a`.
- Instrumentation commits: `10650a354` (I1), `c4d619b02` (I2),
  `28c4c172e` (I3), `1ec7dec10` (I4), `56f041473` (I5).
- Release tag of the regression: `d51c95c6a` (v0.6.1).
- Interop fuzz substrate: `94b7648a7` (#1864).
- Upstream reference:
  `target/interop/upstream-src/rsync-3.4.1/compat.c:161-179`
  (`set_allow_inc_recurse`),
  `target/interop/upstream-src/rsync-3.4.1/options.c:3003-3050`
  (`maybe_add_e_option`).
- Related trackers: #966 (RSS gap), #971 (1M scaling), #1050
  (`Vec<FileEntry>` pool), #1862 (sender state machine), #2088
  (regression investigation), #2089 (this audit), #2196-#2200
  (instrumentation), #2209 (perf verification).
