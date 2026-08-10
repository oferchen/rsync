# IUS-4 - SEND_ZC opt-in vs default-on decision framing

Date: 2026-05-21
Scope: pre-commit the decision rule and the artifacts the decision will be based on; no decision made in this doc
Status: **PENDING** - awaits IUS-3 bench numbers (currently DEFERRED to offline multi-kernel hardware capture)
Predecessors:
- IUS-1 (PR #4661, shipped): documented the `--zero-copy` SEND_ZC build-time dependency.
- IUS-2 (PR #4664, shipped): kernel compatibility audit at `docs/audits/ius-2-send-zc-kernel-compat-matrix.md` - `IORING_OP_SEND_ZC` requires Linux 6.0+ for a stable, complete dispatch.
- IUS-3 (PR #4680, in_progress): bench harness scaffold at `crates/fast_io/benches/ius_3_send_zc_vs_send.rs` + design doc at `docs/design/ius-3-send-zc-bench-design-2026-05-21.md`. Numbers capture deferred to offline multi-kernel hardware run.
Successors (blocked):
- IUS-5: implement the IUS-4 decision (Cargo.toml feature change + release notes).
- IUS-6: update CLI help text + man page to reflect the IUS-5 wiring.
Tracker: IUS-4 (#2585); IUS-5 (#2586); IUS-6 (#2587).
Reference: project memory `project_iouring_send_zc_optin_only.md` captures the current opt-in posture and why `--zero-copy` silently downgrades on default builds.

## 1. Purpose

This doc does not decide. It pre-commits the rule that will be applied
once IUS-3 numbers land, plus the exact downstream actions IUS-5 and
IUS-6 take in each branch of the decision. Pre-committing the criteria
removes "moving the goalposts" risk once numbers are visible and keeps
the decision objective.

## 2. Current state (what IUS-4 is deciding against)

`crates/fast_io/Cargo.toml` at the `iouring-send-zc` feature stanza:

```toml
iouring-send-zc = ["io_uring"]
```

The feature is not in the default feature set. Default builds use
`opcode::Send` (plain io_uring SEND); only `cargo build --features
iouring-send-zc` routes through `ZeroCopySender` and the
`IORING_OP_SEND_ZC` dispatch.

User-facing impact is captured in
`project_iouring_send_zc_optin_only.md`: the `--zero-copy` CLI flag
advertises SEND_ZC as a supported zero-copy primitive, but distro
default builds silently downgrade to plain SEND.

## 3. Decision rule (binding once IUS-3 numbers land)

**Promote `iouring-send-zc` to default-on if and only if:**

1. IUS-3 numbers show **>= 10% throughput improvement** on at least
   **2 of the 4 IUS-3 workloads** (`small_chunks`, `medium_chunks`,
   `large_chunks`, `mixed`), AND
2. **No IUS-3 workload regresses by > 2%** (throughput) versus the
   plain-SEND baseline on the same kernel, AND
3. The kernel 6.0 floor from IUS-2 is acceptable for the
   supported-kernel matrix we are willing to ship as default behaviour.

If any of the three conditions fails, **keep opt-in** via the
`iouring-send-zc` Cargo feature.

A separate IUS-2 corollary applies regardless of the throughput gate:
if the runtime probe at `crates/fast_io/src/io_uring/send_zc.rs::is_supported`
cannot reject 5.19 prerelease kernels safely, the feature stays opt-in
until the probe is hardened. The throughput gate does not override a
safety gate.

### 3.1 Why these thresholds

- **10% on 2/4 workloads** matches the IUS-3 design doc's bench-quality
  threshold for "the SEND_ZC primitive is materially better than plain
  SEND". A smaller win is inside bench noise on the `mixed` workload
  per IUS-3 section 4 and would not survive into real daemon-driven
  workloads.
- **No workload regresses by > 2%** protects users on the kernel-6.0
  edge: SEND_ZC's two-CQE accounting can lose on small chunks and we
  refuse to ship a regression to existing default-build users to chase
  a win on the bulk-transfer regime.
- **Kernel-floor sign-off** is the platform-policy gate, not a bench
  gate. Even if the numbers are perfect, raising the io_uring floor
  from 5.6 (current) to 6.0 (SEND_ZC) is a release-notes-grade change
  that needs explicit approval, not implicit promotion via a bench
  result.

### 3.2 What counts as a workload regression

Per IUS-3 section 4, the workloads are deterministic
(LCG-seeded `mixed`, fixed chunk counts for the others) so the
SEND_ZC vs plain-SEND delta is not buried in run-to-run jitter.
A regression is "the SEND_ZC group has lower median throughput than
the plain-SEND group on the same kernel + workload by more than 2%".
The 2% bar matches the IUS-3 bench's intra-run variance budget.

## 4. Required inputs (must exist before IUS-4 decides)

The decision is blocked until all four artifacts exist:

1. **IUS-3 captured numbers**: `criterion` output for the 4 workloads x
   {SEND, SEND_ZC} x kernel range from
   `docs/design/ius-3-send-zc-bench-design-2026-05-21.md` section 3
   (5.15, 5.19, 6.0, 6.6 LTS, 6.12 - or whichever subset the hardware
   provides, but **at minimum 6.0 and 6.6 LTS**). Numbers land in the
   IUS-3 bench artifacts directory once offline capture completes.
2. **Supported-kernel matrix decision**: explicit answer to "is raising
   the default-build io_uring floor from 5.6 to 6.0 acceptable for the
   distros we ship to?". IUS-2 section 1.3 is the input data; the
   decision is a release-policy call, not a bench call.
3. **Maintenance cost of the opt-in**: count of extra entries in the
   build matrix (Cargo features x platform x kernel) and surface area
   of the two code paths in `fast_io` (the plain-SEND writer plus
   `ZeroCopySender` plus their shared dispatch in `socket_writer.rs`).
   This input is already known and stable; documented here so it is on
   the IUS-4 scoresheet.
4. **Probe safety sign-off**: confirmation that
   `send_zc::is_supported` rejects the 5.19 prerelease kernels per
   IUS-3 section 3. If the probe cannot, default-on is unsafe and the
   decision is keep-opt-in regardless of throughput.

## 5. Outputs / downstream actions (post-decision)

The decision rule produces one of two branches. IUS-5 and IUS-6 execute
the matching branch; nothing else.

### 5.1 If decision is "promote to default-on"

- **IUS-5** edits `crates/fast_io/Cargo.toml`:
  - Adds `iouring-send-zc` to the `default` feature list on Linux
    targets (and leaves it off the cross-platform default).
  - Updates the feature comment to remove "Default off pending
    kernel/workload benchmarks" and replace with "Default on as of
    IUS-4; requires kernel 6.0+".
- **IUS-5** release notes call out:
  - Default builds now route the socket-send path through SEND_ZC on
    kernel 6.0+.
  - On kernel < 6.0, the runtime probe at `send_zc::is_supported`
    transparently falls back to plain SEND. No user action required.
  - To opt out: `--no-default-features --features
    io_uring,<other-features>` minus `iouring-send-zc`.
- **IUS-6** updates `--zero-copy` CLI help text in
  `cli/src/frontend/help.rs` and the man page entry to:
  - Remove the "(opt-in, requires `iouring-send-zc` Cargo feature)"
    qualifier.
  - Keep the "requires Linux 6.0+ for SEND_ZC; falls back to plain
    io_uring SEND on older kernels" qualifier (the runtime story is
    still real even after the build-time story changes).
- **Project memory**: `project_iouring_send_zc_optin_only.md` is
  superseded; replaced with a `project_iouring_send_zc_default_on.md`
  capturing the new posture.

### 5.2 If decision is "keep opt-in"

- **IUS-5** does not edit the Cargo.toml feature list. Instead:
  - Expands the feature comment to document the IUS-3 bench result
    that drove the keep-opt-in decision (e.g., "regression on
    `small_chunks` on kernel 6.0 of N%; promotion rejected per IUS-4
    section 3").
  - Adds a release notes section documenting how to enable
    (`--features iouring-send-zc`) and the kernel-6.0 floor.
- **IUS-6** keeps `--zero-copy` CLI help text as-is and adds explicit
  "requires `iouring-send-zc` Cargo feature (not in default builds)"
  to both the CLI help string and the man page entry, fixing the
  silent-downgrade gap captured in
  `project_iouring_send_zc_optin_only.md`.
- **Project memory**: `project_iouring_send_zc_optin_only.md` stays
  current; updated with the IUS-4 evidence pointer and the keep-opt-in
  rationale.

## 6. Non-goals

This doc does not:

- Capture or interpret IUS-3 numbers. That is the IUS-3 bench-run
  artifact; this doc only commits the rule applied to those numbers.
- Modify `crates/fast_io/Cargo.toml`. The feature stays opt-in until
  IUS-5 executes the IUS-4 decision.
- Decide the supported-kernel floor. That is a release-policy call
  feeding IUS-4 as an input, not a bench-derived output.
- Touch `--zero-copy` CLI help text or the man page. That is IUS-6,
  which is blocked on IUS-5 which is blocked on IUS-4.

## 7. Open items (resolved before IUS-4 closes)

1. Hardware target for the IUS-3 capture run (loopback TCP on multi-
   kernel host: bare metal vs nested-virt). The IUS-3 design doc
   section 3 prefers bare metal for the registered-buffer fast path on
   kernel 6.6+.
2. Whether to capture CPU% (`perf stat` task-clock) alongside
   throughput. IUS-3 section 5 reports throughput; CPU% is a soft
   secondary metric for the IUS-4 keep-opt-in branch ("SEND_ZC trades
   throughput for CPU"). Not a gate; informational.
3. Distro signal: are downstream packagers (Debian, Fedora, Arch,
   Alpine) shipping kernels >= 6.0 in their next stable cuts? IUS-2
   section 1.3 has the snapshot; the IUS-4 decision needs the
   trajectory.

## 8. Decision log (filled when IUS-4 closes)

Empty. The next edit to this section is the decision itself, with a
pointer to the IUS-3 numbers and the supported-kernel sign-off that
together justify the call.
