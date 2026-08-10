# NET-RIO.4: bench harness design - RIO vs IOCP `WSASend`/`WSARecv`

Status: DESIGN. Feeds NET-RIO.4.a (impl), .b (baseline run), .c (RIO run),
.d (synthesis), and gates NET-RIO.5 (default-on decision).

This document specifies the bench harness that will produce the evidence
NET-RIO.5 needs to decide whether to flip `OC_RSYNC_WINDOWS_RIO` from `Off`
to `Auto` by default on Windows. It is design-only; the script + criterion
cells land in NET-RIO.4.a.

Prior work cited inline: the audit at `docs/design/net-rio-windows-audit.md`
(call-site inventory, hybrid migration recommendation, env-gate contract)
and the API surface at `docs/audits/net-rio-1-api-surface.md` (PR #5821
scaffolding, `RioBufferPool` sizing, fallback semantics).

## 1. Bench objective

Quantify the steady-state, burst, and high-IOPS behaviour of the daemon's
socket I/O stack on Windows under three controlled configurations so the
NET-RIO.5 gate can compare apples to apples:

| Dimension | Variants |
|-----------|----------|
| Socket I/O backend | `iocp` (`OC_RSYNC_WINDOWS_RIO=off`, existing `WSASend`/`WSARecv` overlapped path), `rio` (`OC_RSYNC_WINDOWS_RIO=on`, `RIOSend`/`RIOReceive` via the `RioBufferPool` at `crates/fast_io/src/iocp/rio.rs:377`) |
| Concurrent connections | 1, 16, 64, 256, 1024 (powers of 4; covers single-tenant -> small fleet -> stress) |
| Workload shape | steady-state (1 GiB to N clients), burst (100 clients x 10 MiB), high-IOPS (10K clients x 4 KiB) |
| Notification mode (RIO only) | poll-only (`RioCompletionQueue::new` null notification, baseline RIO), IOCP-wired (`RIO_IOCP_COMPLETION`, hybrid per the audit's recommendation in section 4) |

Each `(backend, connections, workload, notify-mode)` tuple is one cell.
Two backends x five concurrencies x three workloads x (1 + 2 RIO modes) =
45 IOCP cells + 30 RIO cells = 75 measured cells, plus an `iperf3` link-
verification preamble per host. Cells where RIO is unavailable
(`try_init_rio` -> `Ok(None)`) are skipped with a clear log line so the
matrix degrades gracefully on hosts without the extension.

## 2. Workload shapes

### 2.1 Steady-state: 1 GiB single file, N concurrent clients

Server hosts a 1 GiB file generated once into a tempdir. N client
processes each invoke `oc-rsync.exe rsync://server/mod/payload .` against
the same file. Measures the dominant production shape: large-file transfer
under fan-out. RIO's per-call pinning win shows up here only when N is
high enough that page-pinning overhead exceeds disk read latency.

Per cell: 1 warmup run + 3 measured runs (matching the existing
`scripts/windows_throughput_bench.sh` `BENCH_RUNS=3` convention). Per-run
wall clock = aggregate transfer time across all N clients (measured by
the harness from first `accept` to last `close`).

### 2.2 Burst: 100 clients each pulling 10 MiB

Server hosts 100 distinct 10 MiB files. 100 client processes each pull
one file simultaneously. Burst arrival pattern: all clients launched
within a 100 ms window via `Start-Process` in a tight loop. Measures
admission cost + warm-pool reuse. RIO's win is the registration-amortisation
effect across many short-lived envelopes.

### 2.3 High-IOPS: 10K clients each pulling 4 KiB

Server hosts 10000 distinct 4 KiB files. Clients use a connection-reuse
shim (`oc-rsync` daemon already keeps the socket open across the multiplex
session; the test exercises new accepts to maximize accept + first-byte
cost). 10K clients overprovisions the daemon's default `--max-connections`,
so the bench raises the cap to `12000` and pre-allocates the RIO pool with
`OC_RSYNC_WINDOWS_RIO_POOL_BYTES=16777216` (16 MiB, 512 slots at default
32 KiB) so slot exhaustion does not confound the result.

This is the worst case for per-call IOCP page-pinning - tiny frames, huge
fan-out - and the cell where RIO is most likely to dominate.

## 3. Metrics

Each measured cell records:

| Metric | Source | Notes |
|--------|--------|-------|
| Throughput per connection (MiB/s) | `oc-rsync --stats` JSON per client | mean across N clients, also p50 / p99 |
| Aggregate throughput (GiB/s) | sum of per-connection throughput | derived; used for acceptance gating |
| Latency p50 / p99 (ms) | per-client wall clock from first byte to close | percentile across N samples per run |
| CPU user (%) | `Get-Counter '\Process(oc-rsync)\% User Time'` sampled at 1 Hz | server side only; mean over run window |
| CPU kernel (%) | `Get-Counter '\Process(oc-rsync)\% Privileged Time'` | server side only |
| Pinned working set (MiB) | `Get-Counter '\Process(oc-rsync)\Working Set - Private'` | RIO cells only; IOCP cells record for parity |
| Non-paged pool delta (MiB) | `Get-Counter '\Memory\Pool Nonpaged Bytes'` before/after | RIO pins into non-paged pool; key risk surface from the audit's section 5 |
| RIO slot exhaustion events | `RioBufferPool::available_slots()` sampled at 1 Hz | RIO cells only; 0 reading for > 1 s flags a sizing miss |
| Fallback events | log scrape for `rio.*fallback` | non-zero on any RIO cell invalidates the cell and is reported separately |

CPU is captured both as percentage and as `kernel/user` ratio. The audit
claims RIO's primary win is reduced kernel time on the page-pinning hot
path; the ratio is the most direct evidence.

Throughput numbers are normalised by the `iperf3` link-verification
preamble (single TCP stream, default window, 30 s) so any cell that ends
up bottlenecked on the NIC is flagged rather than scored.

## 4. Acceptance criteria

A backend cell is **accepted** as a RIO win when, in the matching
configuration:

- **Aggregate throughput >= +15 %** vs the IOCP baseline at the same
  `(connections, workload)`, OR
- **Server-side CPU% reduction >= 30 %** at >= 256 concurrent connections

at a confidence level of three runs per cell, with the worst run dropped.
The `>= 256` floor reflects the audit's finding (section 3) that RIO's
per-call dispatch savings are dwarfed by disk and link cost at low
concurrency.

A backend cell is **rejected** when:

- Aggregate throughput regresses by > 5 % vs IOCP baseline at any
  concurrency, OR
- Non-paged pool delta exceeds 64 MiB at any cell (the audit's documented
  upper cap), OR
- Any RIO cell reports a fallback event during steady-state

NET-RIO.5 flips the default to `Auto` only when the matrix shows wins on
the high-IOPS workload at 256+ connections AND no regressions on steady-
state at any concurrency. A win on the steady-state cell alone is
insufficient because RIO's per-call dispatch advantage is exactly where
upstream's `TransmitFile` path already shines (audit section 2 keeps RIO
out of `TransmitFile` precisely so the bench can attribute the win).

## 5. Environment

| Item | Required | Notes |
|------|----------|-------|
| OS | Windows Server 2022 or Windows 11 Pro 22H2+ | Audit section 5 documents Windows 8 floor; bench runs above the floor |
| CPU | >= 4 physical cores | Per-connection threads contend on the IOCP pump at N=1024; fewer cores would mask RIO's win as scheduler noise |
| RAM | >= 16 GiB | Steady-state cell allocates 1 GiB + N * working set |
| NIC | 10 Gbps preferred, 1 Gbps minimum | iperf3 preamble verifies link; cells where iperf3 < 9 Gbps on a 10 GbE host are flagged because link saturation hides backend differences |
| Build | `oc-rsync.exe` built `--release` with `--features daemon-rio,iocp` | `daemon-rio` from the audit's NET-RIO.3 spec; `iocp` is the existing fast_io path |
| Env gate | `OC_RSYNC_WINDOWS_RIO=on` for RIO cells, `off` for baseline | Matches `RIO_ENV_VAR` constant at `crates/fast_io/src/iocp/rio.rs:111` |
| Pool sizing | `OC_RSYNC_WINDOWS_RIO_POOL_BYTES=16777216` for high-IOPS, defaults elsewhere | Required to clear 10K-client cell |
| Diagnostics | `OC_RSYNC_WINDOWS_RIO_LOG=1` (proposed) | Surface fallback events for the rejection criteria |
| MSYS2 shell | Required for harness wrapper script | Reuses `scripts/windows_throughput_bench.sh` conventions |
| `hyperfine` | Required | Existing convention for wall-clock measurements |
| `iperf3` | Required | Link-verification preamble |
| PowerShell 7+ | Required for `Get-Counter` | Counter sampling in 1 Hz loop |
| Admin shell | Required | Non-paged pool counters and `\Process(*)\% Privileged Time` need elevation |

## 6. Failure modes

| Mode | Detection | Harness behaviour |
|------|-----------|-------------------|
| Kernel < Windows 8 (no RIO) | `try_init_rio()` -> `Ok(None)` at startup | Skip all RIO cells with `SKIP rio: kernel pre-Win8` log line; report IOCP cells only |
| RIO available but probe socket fails | `try_init_rio()` -> `Err` | Hard fail; this signals broken Winsock and the host should not run the bench |
| RIO pool registration fails (`RIORegisterBuffer` returns `RIO_INVALID_BUFFERID`) | `RioBufferPool::with_capacity` returns `Err` | Skip RIO cell at that sizing; record diagnostic, continue with next cell |
| Slot exhaustion mid-run (`available_slots() == 0` for > 1 s) | 1 Hz pool sampling | Invalidate cell, log `SIZING fail rio: pool too small`, do not score |
| Mid-run fallback to IOCP | log scrape for `rio.*fallback` | Invalidate cell; do not blend partial RIO + IOCP throughput |
| CPU counter unavailable (non-admin shell) | PowerShell `Get-Counter` returns access-denied | Hard fail at preamble; bench refuses to run without CPU evidence |
| iperf3 link saturation < 9 Gbps on 10 GbE | preamble check | Continue but flag report header `LINK suspect: iperf3 %.1f Gbps` |
| Hyperfine not installed | preamble check | Hard fail with link to install instructions |

## 7. Tooling

| Tool | Use |
|------|-----|
| MSYS2 bash | Harness driver, mirrors `scripts/windows_throughput_bench.sh` style for consistency with existing Windows benches |
| PowerShell 7 + `Get-Counter` | Per-cell 1 Hz sampling of `\Process(oc-rsync)\% User Time`, `% Privileged Time`, `Working Set - Private`, and `\Memory\Pool Nonpaged Bytes` |
| `hyperfine` | Per-cell wall-clock with `--warmup 1 --runs 3` (matches existing convention) |
| `iperf3` | Per-host link-verification preamble (`-c <server> -t 30 -J`) |
| `oc-rsync` (server) | `oc-rsync --daemon --no-detach --port 8730 --config <tmp>/rsyncd.conf` with one module rooted at the fixture; one daemon per cell |
| `oc-rsync` (client) | `oc-rsync.exe rsync://127.0.0.1:8730/mod/<file> <dst> --stats --no-checksum-output` per connection; `--stats` JSON parsed for throughput |
| `oc-rsync --windows-rio=status` | Diagnostic preamble printout (proposed in audit risk list; landed by NET-RIO.4.a if not already present) |
| Reporting | JSON per cell into `bench-out/net-rio-4/<workload>/<backend>/<connections>.json`; final synthesis Markdown table generated by NET-RIO.4.d |

The harness lives at `scripts/net_rio_throughput_bench.sh` in NET-RIO.4.a;
no rust criterion cell is required because the system under test is a
running daemon (criterion cells under `crates/fast_io/benches/` measure
the in-process I/O primitives, which is a separate evidence layer that
NET-RIO.4 does not subsume).

## 8. Follow-up tasks

| Task | Scope | Gating |
|------|-------|--------|
| **NET-RIO.4.a** | Implement the harness at `scripts/net_rio_throughput_bench.sh` per this spec. PowerShell counter helpers under `scripts/lib/`. Skip-on-missing tool semantics per `windows_throughput_bench.sh` precedent. | Needed before any data run |
| **NET-RIO.4.b** | Run the baseline (IOCP) matrix on a reference Windows 11 Pro 22H2 host and a Windows Server 2022 VM. Three runs per cell. Commit raw JSON under `target/bench-out/net-rio-4-baseline/`. | Blocks .c (apples-to-apples) |
| **NET-RIO.4.c** | Run the RIO matrix on the same hosts with `OC_RSYNC_WINDOWS_RIO=on`. Three runs per cell. Commit raw JSON under `target/bench-out/net-rio-4-rio/`. | Blocks .d |
| **NET-RIO.4.d** | Synthesise .b + .c into a results markdown at `docs/benchmarks/net-rio-4-results.md` per the acceptance criteria in section 4. Includes per-cell win/loss tables and the final NET-RIO.5 recommendation (`flip`, `keep off`, or `re-run with sizing change`). | Feeds NET-RIO.5 |
| **NET-RIO.5** | Default-on decision based on .d. If accepted, change `RioMode::default()` from `Off` to `Auto`, update audit + user-facing docs, ship release note. If rejected, document the rejection and close the parent task. | Gated on .d evidence |

## 9. Open questions

- **Hyperfine vs harness-internal timing for the high-IOPS cell.** Hyperfine
  overhead per invocation (~20 ms) dominates a 4 KiB transfer. NET-RIO.4.a
  may need to drop hyperfine for the 10K-client cell and measure via
  harness-internal wall clock + JSON aggregation. Decided at .a impl time.
- **Whether to enable `RIO_MSG_DEFER` batching in the RIO cells.** The
  audit's section 1 lists `RIO_MSG_DEFER` as supported but the current
  `rio_send` wrapper at `crates/fast_io/src/iocp/rio.rs:786` posts one
  buffer at a time. Defer batching is a NET-RIO.6 optimisation, not part
  of the .4 matrix; the bench should record whether the un-batched path
  meets criteria before any further code lands.
- **Per-connection RIO pool vs shared pool.** The audit recommends a
  single process-wide pool. The bench keeps that, but cell-level slot
  exhaustion may motivate per-connection pools as a NET-RIO.7 follow-up.
  Out of scope for .4 unless slot exhaustion blocks the high-IOPS cell.
