# NET-NSL.3 - TCP_NOTSENT_LOWAT WAN-Latency Bench: Harness Design

Status: Design (NET-NSL.3). Audit lives in
[`net-nsl-audit.md`](net-nsl-audit.md). Implementation design lives in
[`net-nsl-2-implementation-design.md`](net-nsl-2-implementation-design.md)
(PR #5996). This doc pins the bench shape so a follow-up impl task can
land the harness. No bench is run in this PR.

NET-NSL.2 picked **256 KiB** as the bumped `DEFAULT_TCP_NOTSENT_LOWAT`
(up from 64 KiB). NET-NSL.3 must answer empirically: does that bump
deliver measurable wall-clock improvement on realistic WAN-latency
paths, and does it regress on any RTT or workload cell? Bench results
gate the bake window and decide whether the audit follow-ups
(`--tcp-notsent-lowat=NBYTES` CLI flag, LAN-detection heuristic,
`SO_SNDBUF`-aware downgrade) land.

## 1. Objective

Measure how `TCP_NOTSENT_LOWAT` at four watermark values affects three
observable signals on WAN-latency paths:

- **Wall-clock throughput.** Time to transfer each workload under each
  simulated RTT. Primary acceptance metric.
- **Bandwidth-delay-product saturation.** Kernel send-queue depth
  versus link BDP. Diagnoses whether the watermark is clamping
  throughput on fat-pipe / long-RTT paths.
- **Reaction time to throughput drops.** Time from a simulated cwnd
  reduction event to the producer noticing and refilling the queue.
  Validates the latency-bounding rationale that motivated the
  watermark.

Watermark cells:

| Cell | Watermark | Source of value |
| --- | --- | --- |
| W-64K | 65 536 | Current production default (pre-NET-NSL.2). |
| W-256K | 262 144 | NET-NSL.2 chosen default. |
| W-1M | 1 048 576 | Upper-bound sanity ceiling; one OoM above default. |
| W-OFF | unset / skip `setsockopt` | Control. Matches upstream rsync behaviour. |

The W-OFF cell isolates "did the option do anything?" from "did the
default value matter?". Both questions must answer yes for the bump to
ship long-term.

## 2. Tooling

All tooling already runs inside the `rsync-profile` podman container
(per the project's Containers section); the harness must not require
host installs.

- **`tc qdisc add ... netem delay <Nms>`** for RTT simulation. Applied
  on a network-namespace veth pair so host networking is unaffected.
  Use `netem delay 50ms 5ms distribution normal` for jitter cells once
  the constant-delay baseline lands.
- **`iperf3 -t 30 -c <peer>`** for path-baseline throughput. Confirms
  the netem-shaped link delivers the expected BDP before running
  oc-rsync against it. Recorded once per (latency) cell; not part of
  the main result table.
- **`hyperfine --warmup 1 --runs 5`** for wall-clock timing. Per-cell
  invocation wraps `oc-rsync` push or pull. `--export-csv` produces
  the per-cell CSV slice the harness concatenates.
- **`ss -tinm` snapshot mid-transfer** for kernel send-queue depth.
  `ss -tinm dst :873` polled at 100 ms cadence during the transfer;
  the harness records mean and p95 `notsent_bytes` per cell.
- **`oc-rsync` push and pull** (both directions; the watermark applies
  to the connected stream regardless of role). Built with the
  NET-NSL.2 helper landed; for the W-OFF cell the harness exports
  `OC_RSYNC_TCP_NOTSENT_LOWAT=off` (the audit follow-up env-var; the
  harness PR lands the env-var read alongside the bench so the
  control cell exists without rebuilding).
- **Upstream `rsync 3.4.1`** as a sanity control on every cell to
  catch netem misconfiguration (upstream never calls
  `TCP_NOTSENT_LOWAT`, so its wall-clock is independent of the
  watermark and must stay constant across cells; a drift signals
  netem / cache contamination).

## 3. Workloads

Three workloads, chosen to exercise the latency-sensitive and
throughput-sensitive halves of the watermark hypothesis:

| Workload | Size | File count | Hypothesis |
| --- | --- | --- | --- |
| **WL-100M** | 100 MiB | 1 | Saturated-pipe scenario. Watermark should affect time-to-first-window-recovery on long RTTs; the bulk transfer fills the BDP regardless of watermark. |
| **WL-1G** | 1 GiB | 1 | Long-running. Multiple cwnd ramp / cut cycles amortise per-cycle effects; watermark differences show as steady-state throughput delta. |
| **WL-10K** | 4 KiB x 10 000 = ~40 MiB | 10 000 | Small-payload scenario. Multiplex control frames dominate; watermark expected to either show **strong** latency improvement (control frames don't wait behind 256 KiB of queued `MSG_DATA`) or **no** effect (each file fits in a single segment). |

Each workload uses a deterministic seeded payload (`urandom` piped
through `head -c <size> | dd seek=...` so reruns hit byte-identical
data) so the rolling checksum hot path is stable across cells. The
generator script seeds once at harness setup; the corpus lives in
`/build/oc-rsync/target/bench/net-nsl-3/<workload>/` inside the
container.

WL-1G is the headline cell for the acceptance gate; WL-100M is the
sanity ceiling for short-running runs; WL-10K is the regression watch
(small-file paths are where a too-small watermark could starve the
queue).

## 4. Latency matrix

Simulated RTTs cover the audit's framing
(`net-nsl-audit.md` Recommended-value table) plus a 0 ms control:

| Cell | RTT | Path archetype |
| --- | --- | --- |
| L-0 | 0 ms | LAN / loopback control. Detects whether the watermark regresses LAN throughput (the W-64K BDP-clamp risk the audit flagged). |
| L-10 | 10 ms | Same-region cloud / metro WAN. |
| L-50 | 50 ms | Cross-country WAN. NET-NSL.2 chose 256 KiB primarily for this RTT class; this is the headline cell. |
| L-100 | 100 ms | Transcontinental WAN. |

Each (workload x watermark x RTT) cell runs `hyperfine --runs 5
--warmup 1` for 5 timed transfers. Total cells: 3 workloads x 4
watermark values x 4 RTTs = **48 cells**. Plus the upstream-rsync
sanity control on the same 4 RTTs x 3 workloads = **12 control
cells**. Plus the `iperf3 -t 30` path baseline per RTT = **4
baseline cells**. Total bench wall-clock at ~30 s/run, 6 runs/cell:
~3 hours including warmup and netem setup overhead.

## 5. Network topology and isolation

Run inside the existing `rsync-profile` podman container documented
in the project's Containers section. The container shares the host
network namespace by default; the harness must not `tc qdisc` on the shared interface (it
would shape host traffic). Two options, both Linux-only:

- **Network namespace pair (recommended).** Inside the container,
  `ip netns add nsl-bench-{client,server}`, veth pair between them,
  apply `tc qdisc add dev veth-server root netem delay <Nms>` on the
  server side. Both oc-rsync processes run inside their respective
  netns via `ip netns exec nsl-bench-client oc-rsync ...`. Host
  networking unaffected.
- **`unshare --net` fallback.** If the container's CAP_NET_ADMIN is
  unavailable (rootless podman default), use `unshare --net --map-root`
  inside the container. Same veth + netem; user-namespaced root is
  sufficient for `tc qdisc`.

Either path keeps the bench reproducible and isolated. The harness
records `uname -r`, `tc -V`, `ip --version`, and `podman --version`
at run start so cell results stay traceable to the test rig.

## 6. Output format

Per (workload x latency x watermark) cell, one CSV row:

```
workload,rtt_ms,watermark_bytes,run_index,wall_clock_s,mean_notsent_bytes,p95_notsent_bytes,upstream_baseline_s,iperf3_bps
```

Headers fixed; downstream summarisation joins on
`(workload, rtt_ms, watermark_bytes)`. `run_index` is 0-4 for the
5 hyperfine runs. `upstream_baseline_s` and `iperf3_bps` are
broadcast per-cell from the sanity-control rows so the joined CSV is
self-contained for the synthesis step. Chart generation (PNG / SVG)
is deferred to a follow-up; the harness emits CSV only.

CSV file path inside the container:
`/build/oc-rsync/target/bench/net-nsl-3/results-<git-sha>-<utc>.csv`.

## 7. Acceptance criteria

The bench passes (NET-NSL.2's 256 KiB default ships permanently) if
**all** of the following hold:

1. **Headline gate.** WL-100M x L-50 x W-256K shows >=5% wall-clock
   improvement vs WL-100M x L-50 x W-64K (median of 5 hyperfine runs,
   one-sided). 5% is the audit-stated minimum that justifies the
   default bump; smaller deltas are noise on netem-shaped paths.
2. **No regression.** No (workload x RTT) cell shows >2% wall-clock
   regression at W-256K vs W-64K. The W-OFF cell is the floor; W-256K
   must never regress past unset.
3. **LAN safety.** L-0 cells stay within 2% of upstream rsync 3.4.1
   wall-clock across all watermark values. LAN throughput regression
   was the audit's primary risk and the gating constraint on the bump.
4. **WL-10K stability.** WL-10K x L-50 x W-256K shows no regression
   vs W-64K. If small-file throughput drops, the watermark is too
   small for control-frame interleaving and the default must shrink.
5. **BDP sanity.** Mean `notsent_bytes` mid-transfer stays below the
   watermark across all cells. If it doesn't, the harness or the
   helper API is broken and results are invalid.

## 8. Rollback criterion

If the bench shows >2% regression at any (workload x RTT) cell
versus W-64K (or vs W-OFF, whichever is lower), the bump does not
ship as an unconditional default. Instead, file a follow-up task to:

- Make the watermark configurable via the `--tcp-notsent-lowat=NBYTES`
  CLI flag and `tcp notsent lowat = N` daemon config directive (both
  pre-specified in the NET-NSL.1 audit as deferred follow-ups).
- Default the constant to whichever cell wins on the largest fraction
  of the matrix.
- Document the per-RTT / per-workload recommendation table inside
  the bench-synthesis follow-up so operators can pick a non-default
  watermark for their workload class.

A regression on **only** the W-1M ceiling is acceptable and expected
(the audit framed 1 MiB as the upper-bound sanity check, not a
viable default); the rollback gate fires only on W-256K cells.

## 9. Follow-up sub-tasks

| Task | Deliverable |
| --- | --- |
| **NET-NSL.3.a** | Implement the bench harness as `scripts/benchmark_net_nsl_3.sh` (matching the project's existing `scripts/benchmark_*.sh` naming pattern). Includes corpus generation, netns + netem setup, hyperfine invocation per cell, CSV emit, container-side environment capture. No measurement; just the harness. |
| **NET-NSL.3.b** | Run the baseline pass: W-OFF cell only, all workloads x all RTTs. 12 cells. Validates the harness end-to-end before running the full 48-cell matrix. Produces the CSV row floor against which all other cells are compared. |
| **NET-NSL.3.c** | Run the measurement pass: W-64K, W-256K, W-1M cells, all workloads x all RTTs. 36 cells plus the 12 control cells (upstream rsync) + 4 iperf3 path baselines. Produces the full results CSV. |
| **NET-NSL.3.d** | Synthesize: load the CSV, evaluate the acceptance criteria (Section 7), decide ship or rollback (Section 8). Write a results doc at `docs/design/net-nsl-3-bench-results.md` summarising the matrix, the headline cell delta, and the ship / rollback verdict. Generate the chart PNG as a separate deliverable inside that doc's PR. |

NET-NSL.3.a is the only sub-task that writes code; .b and .c are
container runs; .d is analysis + a results design doc.

## 10. Out of scope

- TLS-wrapped transfers (`stunnel` / external TLS proxy). The watermark
  applies to the underlying TCP socket regardless of TLS framing;
  TLS adds ~13 KiB per record overhead that's orthogonal to the
  lowat hypothesis. A TLS variant is a follow-up if NET-NSL.3.d
  finds the plain-TCP cells gate cleanly.
- SSH transport. SSH stdio passthrough does not touch
  `TCP_NOTSENT_LOWAT`; the option fires only on direct
  `rsync://` daemon TCP connections.
- macOS / FreeBSD bench. The harness is Linux-only by design
  (`netem` is Linux-only); macOS bench would need `dummynet` and a
  separate harness PR. The audit's macOS coverage is the existing
  Safari production reference.
- Windows bench. No `TCP_NOTSENT_LOWAT` equivalent on Windows; the
  audit defers the parity story to NET-RIO.
- Chart generation. CSV is the artifact; chart PNG / SVG is owned by
  NET-NSL.3.d as a downstream deliverable on the results doc.
- `SO_SNDBUF` interaction matrix. The audit flagged this as a
  follow-up; benching the lowat-vs-sndbuf cross product requires
  another 4-cell axis and roughly doubles bench wall-clock. Deferred
  unless NET-NSL.3.d finds an unexplained anomaly.

## 11. Cross-references

- Audit: [`docs/design/net-nsl-audit.md`](net-nsl-audit.md).
- Implementation design: [`docs/design/net-nsl-2-implementation-design.md`](net-nsl-2-implementation-design.md), PR #5996.
- Existing bench harness pattern: `scripts/benchmark_hyperfine.sh`,
  `scripts/benchmark_daemon_concurrency.sh`.
- Container baseline: project Containers (Podman) section,
  `rsync-profile` image.
- Helper API the bench exercises:
  `crates/fast_io/src/socket_options.rs::set_tcp_notsent_lowat`,
  `enable_notsent_lowat`, `DEFAULT_TCP_NOTSENT_LOWAT`.
