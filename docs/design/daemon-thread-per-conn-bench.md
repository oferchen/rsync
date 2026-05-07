# Daemon Thread-per-Connection Concurrency Bench

Tracking: oc-rsync task #1933.

> Slim, runnable plan that complements the broader scope captured in
> `docs/design/daemon-tpc-benchmark-plan.md`. Where the long plan covers
> three platforms, two arrival shapes, soft-limit triggers, and lock
> contention sampling, this document specifies the minimum harness
> needed to land a first measured number on Linux loopback at
> 100 / 1 000 / 10 000 concurrent rsync:// pulls.

## 1. Goal

Quantify the scalability cliff of the current synchronous
thread-per-connection daemon (`crates/daemon/src/daemon/sections/server_runtime/connection.rs`)
under concurrent rsync:// pull load. Cross-references the static
analysis in `docs/audits/daemon-thread-per-connection-scalability.md`
(task #1673), which fixes paper upper bounds on stack reservation,
descriptor cost, and admission semantics. This bench turns those upper
bounds into measured ttfb, completion p99, peak RSS, and peak thread
count on a single Linux host.

The output feeds the sync-vs-async daemon comparison tracked under
#1934 (RFC) and #1935 (implementation). Decision thresholds for the
async migration live in `docs/design/daemon-tpc-benchmark-plan.md`
Section 9; this bench supplies the W100 / W1k / W10k inputs to that
decision.

## 2. Bench harness

A stress-test client (`crates/daemon/benches/concurrency_driver/main.rs`,
new) opens `N` concurrent `rsync://127.0.0.1:PORT/test/...` pulls in
parallel via `std::thread::spawn` (one OS thread per connection so the
client side imposes no extra scheduling layer). Each pull transfers
exactly one 1 MiB file with deterministic content from a tmpfs-backed
module. Per connection the driver records:

- **time-to-first-byte** - clock between `connect()` returning and the
  first non-greeting byte read from the daemon socket.
- **completion latency** - clock between `connect()` and the
  `MSG_DONE` frame from the daemon. Aggregated as p50 / p99 / max.
- **bytes received** - asserted equal to 1 MiB per connection so
  partial completions count as failures, not fast successes.

A sidecar sampler thread polls `/proc/$PID/status` for `VmRSS`,
`VmSize`, and `Threads` at 100 ms cadence during the active phase.
Output: one CSV per run plus a Markdown summary table written under
`target/bench/daemon-tpc/$RUNID/`.

The driver and the daemon under test are pre-built; the harness does
not invoke `cargo` during measurement.

## 3. Concurrency levels

- **W100** - 100 concurrent pulls. Baseline; expected to pass cleanly
  with no resource pressure.
- **W1k** - 1 000 concurrent pulls. Operating point and the pass / fail
  gate (Section 5).
- **W10k** - 10 000 concurrent pulls. Exploratory ceiling test; not a
  pass / fail gate, but records where the sync path actually breaks
  under raised ulimits.

All three waypoints use the all-at-once arrival shape: the driver fires
`N` `connect()` calls within a 10 ms window so the bench stresses the
listen backlog, the per-accept `thread::spawn`, and the
`SharedLogSink` mutex. Steady-state and 24 h soak runs are deferred to
the broader plan.

## 4. Setup

- **Host** - Linux container `localhost/oc-rsync-bench:latest` (Arch
  Linux, kernel 6.x, see project rules) running on a dedicated benchmark
  runner. Bench tier slow.
- **Tunables** - `ulimit -u 65536`, `ulimit -n 65536` set in the
  container entrypoint before the daemon starts. These cover even the
  W10k waypoint with margin against thread, fd, and `vm.max_map_count`
  defaults documented in
  `docs/audits/daemon-thread-per-connection-scalability.md` Section 5.
- **Daemon** - `oc-rsync --daemon --no-detach --port=PORT --address=127.0.0.1`
  with a single read-only module backed by a tmpfs directory containing
  the 1 MiB test file. No `lock_file`, no `max_connections`, default
  verbosity (one log line per accept).
- **Network** - loopback only. Removes NIC and switch jitter from the
  ttfb measurement; isolates the daemon's accept and per-connection
  thread cost.
- **Driver host** - same container for W100 and W1k (no network
  jitter); separate container sharing the same pod network for W10k so
  driver-side fd budget does not steal from the daemon's.

## 5. Pass criteria

- **W100** - 100% session success, p99 completion <= 2 s, peak RSS
  <= 256 MiB, zero accept errors. Hard pass.
- **W1k** - 100% session success within a 30 s wall-clock budget from
  first `connect()` to last `MSG_DONE`. Peak RSS <= 1.5 GiB,
  zero accept errors, zero panic-on-spawn-failure traces (audit
  Section 6.3). Hard pass.
- **W10k** - exploratory. Records peak concurrent threads achieved,
  the failure mode if any (`EAGAIN` on `pthread_create`, `EMFILE` on
  `accept`, RSS pressure, drain timeout), and ttfb p99 at saturation.
  No pass / fail; the result feeds the #1935 decision.

A W1k regression vs the most recent recorded number (delta beyond
twice the run-to-run standard deviation across three warm runs)
blocks the merge that introduced it.

## 6. Outputs and downstream

The harness writes a JSON bundle plus a Markdown summary per waypoint.
The W1k and W10k numbers feed the sync-vs-async comparison the async
listener work needs:

- **#1934 (RFC).** Section 5.2 of the RFC names the sync baseline as
  the row this bench fills in. Without measured numbers the RFC's
  "async listener buys X at W1k" claim cannot be quantified.
- **#1935 (implementation).** Gates whether the async path lands behind
  the `async-daemon` Cargo feature on by default or stays opt-in.
  Decision thresholds in `docs/design/daemon-tpc-benchmark-plan.md`
  Section 9 are evaluated against this bench's W10k row.

Raw outputs and the summary table land in
`target/bench/daemon-tpc/$RUNID/`. The headline numbers are appended
to this document as an addendum after the first hardware run; no
README or changelog update is in scope here.
