# IUS-4 - SEND_ZC opt-in vs default-on decision

Date: 2026-05-22
Scope: apply the IUS-4 decision rule against the IUS-3 evidence base and
record the IUS-5 / IUS-6 actions taken in this cycle.
Status: **DECISION RECORDED - keep opt-in (data-missing branch)**
Decision rule: `docs/design/ius-4-decision-framing-2026-05-21.md` section 3
Predecessors:
- IUS-1 (PR #4661, shipped): README + man-page note on the `--zero-copy`
  SEND_ZC build-time dependency.
- IUS-2 (PR #4664, shipped): kernel compatibility audit at
  `docs/audits/ius-2-send-zc-kernel-compat-matrix.md` - SEND_ZC needs
  Linux 6.0+ for a stable, complete dispatch.
- IUS-3 (PR #4680, shipped): bench harness scaffold at
  `crates/fast_io/benches/ius_3_send_zc_vs_send.rs` + design doc at
  `docs/design/ius-3-send-zc-bench-design-2026-05-21.md`. **Numbers
  capture deferred to offline multi-kernel hardware run that has not
  occurred.**
Successors:
- IUS-5: documentation-only this cycle - `crates/fast_io/Cargo.toml`
  feature stays opt-in. The expanded feature comment captures the IUS-4
  reasoning so a future agent rediscovers the gate without reading this
  doc.
- IUS-6: CLI help text + man-page entry tightened to honestly describe
  the opt-in posture for `--zero-copy` SEND_ZC dispatch.

## 1. Decision

**Keep `iouring-send-zc` opt-in.** Default builds continue to dispatch
plain `IORING_OP_SEND` on the io_uring socket-send path; `--zero-copy`
continues to rely on the other zero-copy primitives (`sendfile`,
`splice`, `copy_file_range`) on default builds.

This decision is taken under the **data-missing** branch of the IUS-4
rule, not the throughput / regression branches. No IUS-3 numbers exist
to evaluate; only the bench harness has shipped.

## 2. Inputs evaluated

The IUS-4 framing doc section 4 enumerates four required inputs. Status
at decision time:

| Input | Required | Status | Notes |
|-------|----------|--------|-------|
| IUS-3 captured numbers | Yes | **MISSING** | Bench scaffold ships at `crates/fast_io/benches/ius_3_send_zc_vs_send.rs` and is gated behind `OC_RSYNC_BENCH_IUS_3=1` + `--features iouring-send-zc`. No multi-kernel hardware run has been captured. The IUS-3 design doc section 10 calls this out explicitly as "Numbers capture on real hardware ... Gated on hardware availability; not blocking IUS-4." That statement deliberately keeps IUS-4 unblocked **for a decision**, not unblocked for a default-flip. With zero data points the default-flip branch is impossible to justify under the framing rule. |
| Supported-kernel matrix sign-off | Yes | **Not granted** | Raising the io_uring floor from 5.6 (current) to 6.0 (SEND_ZC) is a release-policy call. With no throughput evidence to motivate the platform-policy cost, sign-off has not been requested and is not pre-approved. |
| Opt-in maintenance cost | Yes | Known + acceptable | Two code paths in `fast_io`: the default `IORING_OP_SEND` writer at `crates/fast_io/src/io_uring/socket_writer.rs` and the gated `ZeroCopySender` at `crates/fast_io/src/io_uring/send_zc.rs`. The cargo feature flag (`iouring-send-zc = ["io_uring"]`) is one entry in `crates/fast_io/Cargo.toml`; build matrix surface is one optional Linux-only feature, comparable to `vmsplice`, `iouring-data-reads`, and `iouring-data-writes`. Cost is steady-state, not growing. |
| Runtime probe safety sign-off | Yes | Held (probe is correct) but inert this cycle | `send_zc::is_supported` at `crates/fast_io/src/io_uring/send_zc.rs:77` returns false on kernels < 6.0 via `IORING_REGISTER_PROBE`. Behaviour is correct per the IUS-2 audit section 2.4. The probe is unused in the dispatch path on default builds because the feature is opt-in; promoting to default-on would activate it. With opt-in retained the probe remains a passive guard with no live exercise this cycle. |

Two of the four required inputs are missing or absent. The rule in
section 3 of the framing doc is "if any of the three [throughput,
no-regression, kernel sign-off] conditions fails, **keep opt-in**." With
zero throughput data the throughput condition cannot evaluate as a pass;
the no-regression condition cannot evaluate either; the kernel sign-off
condition is not granted. All three are non-pass. Keep opt-in.

## 3. Why this is the correct call even though IUS-3 design doc
section 10 says "not blocking IUS-4"

The IUS-3 design doc allows IUS-4 to proceed without numbers so the
decision *body* (this doc) can be filed and the dependent IUS-5 / IUS-6
work can land. It does **not** waive the IUS-4 rule's requirement that a
default-flip needs a measured >= 10% throughput win on >= 2 of 4
workloads with no > 2% regression on any workload. Without numbers, the
only IUS-4 branch the rule permits is keep-opt-in.

This matches the IUS-4 framing doc section 5.2 ("If decision is keep
opt-in") which lists documentation actions only and explicitly does not
edit `crates/fast_io/Cargo.toml` to add `iouring-send-zc` to `default`.

## 4. Outputs this cycle (IUS-5 + IUS-6 actions taken)

Per IUS-4 framing doc section 5.2:

### 4.1 IUS-5 (Cargo + release-notes)

- **Cargo.toml**: no functional change. The `iouring-send-zc` feature
  stanza in `crates/fast_io/Cargo.toml` stays as
  `iouring-send-zc = ["io_uring"]` and stays out of the `default`
  feature set. The comment block above the stanza is updated to
  document the IUS-4 keep-opt-in decision so the gate's rationale
  travels with the code.
- **Release notes**: no release-notes section is added in this cycle
  because the public-facing behaviour does not change. Beta release
  notes already cover the opt-in posture via the IUS-1 README + man-page
  text. A release-notes-grade entry will be added once IUS-3 hardware
  numbers land and IUS-4 is reopened.

### 4.2 IUS-6 (CLI help + man-page alignment)

The CLI help string for `--zero-copy` advertises SEND_ZC without the
opt-in caveat, which is the exact gap captured in project memory at
`project_iouring_send_zc_optin_only.md`. IUS-6 closes that gap on
default builds:

- `crates/cli/src/frontend/help.rs:151` (`--zero-copy` long-help line)
  now states that the io_uring SEND_ZC primitive is a build-time opt-in
  via the `iouring-send-zc` cargo feature.
- `crates/cli/src/frontend/command_builder/sections/transfer_behavior_options.rs:226-231`
  (the Clap `Arg::new("zero-copy")` help string) carries the same
  qualifier.
- `docs/oc-rsync.1.md` already carries the opt-in note from IUS-1; the
  text is left as-is (verified accurate against the post-IUS-4 reality).
- `README.md` section `--zero-copy and io_uring SEND_ZC` already carries
  the opt-in note from IUS-1; the trailing sentence about "the path to
  flipping this default on" is left as-is because IUS-4 has not closed
  that path - it has documented why the path is still gated on missing
  evidence.

### 4.3 Project memory

`project_iouring_send_zc_optin_only.md` stays current. The evidence
pointer (this doc) and the keep-opt-in rationale documented above
satisfy the IUS-4 framing doc section 5.2 third bullet. Maintaining the
memory file is out of scope for this PR; the next memory pass will
backfill the IUS-4 reference.

## 5. What would change this call

The keep-opt-in decision is **not** "SEND_ZC is rejected". It is "we
have no evidence to promote and the framing rule forbids a no-evidence
promotion." A future IUS-4 reopen needs **all four** of:

1. IUS-3 numbers captured on at least kernel 6.0 + 6.6 LTS, ideally
   also 6.12 (per IUS-3 design section 3). The bench scaffold is ready;
   run it with `OC_RSYNC_BENCH_IUS_3=1 cargo bench -p fast_io --features
   iouring-send-zc --bench ius_3_send_zc_vs_send` on the multi-kernel
   hardware fleet.
2. Numbers meet the IUS-4 throughput gate: >= 10% improvement on at
   least 2 of 4 IUS-3 workloads (`small_chunks`, `medium_chunks`,
   `large_chunks`, `mixed`).
3. No IUS-3 workload regresses by > 2% versus the plain-SEND baseline.
4. Release-policy sign-off to raise the io_uring socket-send floor from
   5.6 to 6.0.

When all four hold, reopen IUS-4 with a new decision doc citing the
captured numbers and the sign-off; IUS-5 then flips
`iouring-send-zc` into the `default` feature set on Linux targets and
IUS-6 strikes the build-time-opt-in qualifier from the CLI help and
README.

## 6. Files touched this cycle

- `docs/design/ius-4-decision-2026-05-22.md` (new, this doc)
- `crates/fast_io/Cargo.toml` (comment-only update on the
  `iouring-send-zc` stanza; no feature-set change)
- `crates/cli/src/frontend/help.rs` (`--zero-copy` long-help line)
- `crates/cli/src/frontend/command_builder/sections/transfer_behavior_options.rs`
  (Clap `Arg::new("zero-copy")` help string)

The IUS-1 README and man-page text already carries the build-time
qualifier and is left unchanged.

## 7. References

- Rule: `docs/design/ius-4-decision-framing-2026-05-21.md`
- Bench: `docs/design/ius-3-send-zc-bench-design-2026-05-21.md`,
  `crates/fast_io/benches/ius_3_send_zc_vs_send.rs`
- Audit: `docs/audits/ius-2-send-zc-kernel-compat-matrix.md`
- IUS-1 docs: README.md `--zero-copy and io_uring SEND_ZC` section;
  `docs/oc-rsync.1.md` `--zero-copy` long entry
- Probe: `crates/fast_io/src/io_uring/send_zc.rs:77` (`is_supported`)
- Dispatch primitive: `crates/fast_io/src/io_uring/send_zc.rs:130`
  (`try_send_zc`)
- Production caller: `crates/fast_io/src/io_uring/socket_writer.rs:91-104`
- Policy resolution: `crates/fast_io/src/io_uring_common.rs:183`
  (`allow_send_zc`)
- Project memory: `project_iouring_send_zc_optin_only.md`
