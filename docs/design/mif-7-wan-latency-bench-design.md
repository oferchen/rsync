# MIF-7: WAN Latency Benchmark for MSG_INFO Frame Coalescing

## Goal

Measure the wall-clock transfer time improvement from MSG_INFO frame
coalescing (MIF-5) under simulated WAN conditions. MIF-6 confirmed
wire-byte parity; this benchmark quantifies the latency benefit of
fewer TCP segments on high-RTT links.

## Background

Before MIF-5, oc-rsync emitted one TCP segment per MSG_INFO frame -
roughly +140% more wire segments than upstream rsync for itemize-enabled
transfers. Frame coalescing defers MSG_INFO flushes, letting the write
buffer batch multiple frames into fewer syscalls and TCP segments.

The improvement is most pronounced on high-latency links because each
extra TCP segment incurs a round-trip penalty from Nagle/delayed-ACK
interactions. A 10,000-file transfer producing 10,000 extra segments at
100ms RTT adds roughly 500-1000ms of cumulative delay from small-packet
congestion and ACK wait states.

## Workload

| Parameter | Value |
|-----------|-------|
| File count | 10,000 |
| File size | 1 KB each (uniform random content) |
| Transfer mode | Daemon push (`rsync://`) with `--itemize-changes` |
| Simulated RTT | 0ms, 50ms, 100ms, 200ms (tc netem on loopback) |
| Packet loss | 1% at each non-zero RTT level |
| Runs per config | 7 (median reported) |

The workload maximizes per-file overhead relative to data volume.
Small files ensure the transfer is dominated by protocol framing and
round-trips rather than bulk data throughput.

## Methodology

1. **Environment.** Linux host or container with `tc` (iproute2) and
   `netem` kernel module. The `rsync-profile` container works if
   `NET_ADMIN` capability is granted.

2. **Daemon setup.** An upstream rsync daemon serves as the receiver.
   This isolates the measurement to the client-side framing change -
   both upstream rsync and oc-rsync push to the same daemon.

3. **Netem configuration.** `tc qdisc add dev lo root netem delay Xms`
   applies half-RTT delay to loopback. Combined with the return path
   this produces the target RTT. Packet loss is added via `loss 1%`.

4. **Measurement.** Wall-clock time from `date +%s%N` around each
   transfer. Seven runs per configuration; the median is reported
   to reduce outlier influence.

5. **Comparison.** Each RTT level runs both upstream rsync and oc-rsync
   as clients. The delta percentage shows the coalescing improvement.

6. **Cleanup.** Netem rules are removed after each RTT level. A trap
   handler ensures cleanup on script exit.

## Expected Results

| RTT | Expected delta (oc-rsync vs upstream) |
|-----|---------------------------------------|
| 0ms | Within noise (< 3%) - no latency penalty to remove |
| 50ms | 3-8% faster - fewer Nagle delays |
| 100ms | 5-15% faster - ACK coalescing benefit compounds |
| 200ms | 8-20% faster - high-RTT amplifies per-segment penalty |

These bounds assume the coalescing reduces client-to-server segments
by roughly 40-60% (from MIF-6 segment count measurements). The actual
latency improvement is sublinear in segment reduction because TCP
window scaling and Nagle interact non-trivially.

If oc-rsync is slower than upstream at any RTT level, the result
indicates a regression unrelated to MSG_INFO coalescing (e.g., higher
per-file CPU overhead) that warrants separate investigation.

## Reproduction

### Prerequisites

- Linux with `tc` and `netem` kernel module (`modprobe sch_netem`)
- `NET_ADMIN` capability if running inside a container
- Upstream rsync installed at `/usr/bin/rsync`
- oc-rsync release binary at `/workspace/target/release/oc-rsync`

### Commands

```sh
# Inside rsync-profile container (or any Linux host with tc/netem):
podman exec -it --cap-add NET_ADMIN rsync-profile bash

# Build oc-rsync release binary
cd /workspace && cargo build --release

# Create fixture (done automatically by the script)
bash tools/bench/wan_latency_coalescing.sh
```

The script outputs a CSV at `/tmp/mif7-coalescing-results.csv` and
prints a summary table with median times and delta percentages.

### Manual single-run example

```sh
# Set 100ms RTT with 1% loss
tc qdisc add dev lo root netem delay 50ms loss 1%

# Start upstream daemon
rsync --daemon --config=/tmp/mif7-rsyncd.conf --no-detach &

# Run oc-rsync transfer
time oc-rsync -a --itemize-changes /tmp/mif7-fixture/ \
    rsync://127.0.0.1:18895/bench/

# Cleanup
tc qdisc del dev lo root
```

## Limitations

- **Loopback only.** Netem on loopback does not perfectly model real WAN
  conditions (no bandwidth cap, no jitter, no reordering). Results are
  directional, not absolute predictions for real networks.

- **No SSH path.** This benchmark tests daemon transfers only. SSH adds
  its own buffering layer that may mask or amplify the coalescing effect.

- **macOS/Windows.** `tc netem` is Linux-only. The benchmark cannot run
  on macOS or Windows without a Linux VM or container.

## Bench Script

`tools/bench/wan_latency_coalescing.sh` - self-contained script that
sets up the fixture, daemon, netem rules, runs all configurations, and
produces the results CSV. See the script header for usage.

## Acceptance Criteria

MIF-7 is complete when:

1. The bench script runs successfully on a Linux host with netem
2. Results at 100ms RTT show oc-rsync within 5% of upstream (parity)
   or faster (improvement from coalescing)
3. No regression at 0ms RTT (within noise floor)
4. Results are recorded in this document or a companion results file
