# QUIC transport concurrency model: sans-IO thread vs confined tokio runtime

Status: decided - the sans-IO driver thread ships; the confined-runtime
implementation was deleted.

## Candidates

Both candidates expose the same public surface in `rsync_io::quic`
(`QuicAcceptor`, `QuicConnector`, `QuicStream` as blocking `Read`/`Write`,
ALPN `rsync`, self-signed certificate pinning) behind the off-by-default
`quic` feature.

1. **Confined runtime (spike, deleted).** `quinn` on a current-thread tokio
   runtime per endpoint; every blocking operation is a `block_on`. Small
   (185 code lines) because quinn hides the driver, but the endpoint only
   makes progress while some facade call is blocking. The spike already hit
   the consequence: the final transport ACK starved until idle timeout, and
   fixing teardown took a `finish()`+`stopped()` barrier plus explicit
   `close()`+`wait_idle()` - roughly a third of the glue was teardown
   correctness, and the discipline extends to the peer ("keep issuing
   blocking calls until my finish resolves").
2. **Sans-IO (shipped).** `quinn-proto` driven from one dedicated I/O thread
   that owns the UDP socket: datagrams in via `Endpoint::handle`, timers via
   `poll_timeout`/`handle_timeout`, packets out via `poll_transmit`. The
   facade exchanges bytes with the driver through a condvar-guarded buffer
   pair and wakes it with a loopback datagram (UDP self-pipe). No async
   runtime; `quinn` and tokio dropped from the `quic` feature.

## Method

Primary platform: x86_64 Linux (Arch, i7-7700, 8 threads); macOS
(aarch64) used only for sanity runs, which agreed directionally. Both
implementations were built into one release binary
(`examples/quic_bench.rs`, temporary, deleted with the loser) that ran the
server and client as separate processes over loopback, so syscall counts and
thread counts are per-side. Correctness gates
(`crates/rsync_io/tests/quic_loopback.rs`: round-trip+EOF, half-close,
prompt teardown < 1 s) passed for both candidates before deletion.

## Measurements

Loopback throughput, 1 GiB, 64 KiB chunks, client-side MB/s, 3 runs:

| Direction | Confined runtime | Sans-IO |
|-----------|------------------|---------|
| client sends | 758.9 / 783.3 / 805.9 | 232.0 / 237.7 / 232.6 |
| server sends | 758.4 / 769.8 / 685.0 | 227.1 / 231.9 / 228.4 |

Client syscalls (`strace -c -f`), 64 MiB in 4 KiB read/write cycles
(16384 cycles):

| Direction | Confined runtime | Sans-IO |
|-----------|------------------|---------|
| client sends | 5538 total (sendmsg 4933, recvmmsg 473) = 0.34/cycle | 83158 total (sendto 57516, recvfrom 25149) = 5.1/cycle |
| server sends | 2210 total (recvmmsg 1192, sendmsg 746) = 0.13/cycle | 60702 total (recvfrom 58142, sendto 680) = 3.7/cycle |

Threads alive during a 1+ GiB transfer (`/proc/<pid>/task`): confined
runtime 1 per endpoint process; sans-IO 2 (application + `quic-io` driver).

Teardown latency (last byte written to both sides closed), bulk runs: both
~79-135 ms, dominated by QUIC's 3xPTO drain - equivalent.

Duty-cycle run (the deciding measurement): server streams 128 MiB; the
client reads and pauses 50 ms after every MiB without touching the stream,
simulating rsync's checksum/delta phases. Ideal time is ~6.4 s of pauses
plus the transfer.

| Metric | Confined runtime | Sans-IO |
|--------|------------------|---------|
| client elapsed (3 runs) | 7241 / 7279 / 7216 ms | 6503 / 6508 / 6508 ms |
| client teardown | 198 / 283 / 275 ms | 0.13 / 0.15 / 0.15 ms |

Glue size (non-blank, non-comment lines): confined runtime 185; sans-IO 815
(`quic/mod.rs` 364 + `quic/driver.rs` 451).

## Decision

Criteria order: correctness-under-load robustness > maintainability >
syscall overhead > raw throughput.

The sans-IO driver wins on the top criterion and that settles it. With the
confined runtime, timers, ACKs, retransmissions, and flow-control updates
freeze whenever the application is between blocking calls - exactly rsync's
steady state, which interleaves I/O with computation. The duty-cycle run
shows the cost: ~11% wall-clock overhead on top of the compute time plus
200+ ms teardown tails, versus the sans-IO driver running at the structural
optimum with sub-millisecond teardown because everything was already
acknowledged in the background. The teardown hazard the spike had to patch
around is absent by construction, and no cross-peer call discipline leaks
into transport users. On maintainability the extra ~630 lines buy plain
`std` threading (thread + mutex + condvar) with no async runtime in the
transport path, consistent with the codebase's threaded-only I/O model. The
confined runtime's genuine wins - 0.13-0.34 vs 3.7-5.1 syscalls per 4 KiB
cycle and ~3.3x loopback throughput - come from quinn-udp's GSO/GRO
batching, which `std::net::UdpSocket` cannot express; they sit at the bottom
of the criteria order, 230 MB/s (~1.9 Gbit/s) already exceeds the WAN links
QUIC targets, and the gap is recoverable later by exposing
`sendmsg`+`UDP_SEGMENT`/`UDP_GRO` batching through `fast_io` behind a safe
API without changing the concurrency model.

## Integration caveat

A QUIC bidirectional stream is invisible to the acceptor until the opener
sends on it. Handshake layering above this transport must ensure the
connecting side transmits first (rsync's client-speaks-first negotiation
already does).
