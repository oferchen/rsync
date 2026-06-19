# NET-NSL.2 - TCP_NOTSENT_LOWAT Wiring: Implementation Design

Status: Design (NET-NSL.2). Audit lives in
[`net-nsl-audit.md`](net-nsl-audit.md). Bench follow-up is NET-NSL.3.
Reference implementation pattern: NET-TFO.2 (PR #5993,
[`net-tfo-availability-audit.md`](net-tfo-availability-audit.md)).

`TCP_NOTSENT_LOWAT` caps the unsent bytes the kernel queues in a socket's
send buffer. The cap converts the kernel send buffer from "fill until
autotune ceiling" to "wake the producer when buffered-unsent drops below
N". This bounds queueing delay for latency-sensitive multiplex control
frames (`MSG_ERROR`, `MSG_NO_SEND`, `NDX_DEL_STATS`) without altering the
rsync wire protocol.

## Goals

- Bound buffer bloat in both directions of every TCP socket the daemon
  accepts and every TCP socket the client connects to a daemon.
- Stay wire-compatible with upstream rsync. Upstream never calls
  `TCP_NOTSENT_LOWAT`; the option is purely a kernel-side hint and the
  rsync byte stream is unchanged.
- Land best-effort: the option is an optimisation, never a correctness
  requirement. A failing `setsockopt` must never abort a transfer.
- Compose with existing socket tuning (`TCP_NODELAY`, `SO_KEEPALIVE`,
  `TCP_FASTOPEN`); do not contend with operator-supplied `SO_SNDBUF`
  overrides.

## Insertion points

Mirrors NET-TFO.2: server applies the option to each accepted client
stream after `accept(2)`, client applies the option after `connect(2)`
returns. Both surfaces already exist in the workspace from the
NET-NSL.1 audit pass; this design formalises the helper API and bumps
the default to 256 KiB.

### Daemon (server) side

`crates/daemon/src/daemon/sections/server_runtime/listener.rs:388-394`
already declares the apply helper:

```rust
fn apply_accepted_stream_tcp_notsent_lowat(stream: &TcpStream) {
    if fast_io::tcp_notsent_lowat_supported() {
        let _ = fast_io::set_tcp_notsent_lowat(stream, fast_io::DEFAULT_TCP_NOTSENT_LOWAT);
    }
}
```

It runs from two `accept`-side call sites:

- `crates/daemon/src/daemon/sections/server_runtime/connection.rs:361`
  (plain TCP path).
- `crates/daemon/src/daemon/sections/server_runtime/connection.rs:503`
  (TLS-wrapped path; the underlying TCP socket is configured before the
  TLS handshake to avoid layering-order surprises).

NET-NSL.2 changes the constant the helper passes from 64 KiB to 256 KiB
(see Default below). No new call sites; no new helper at this layer.

### Client side

`crates/core/src/client/module_list/tcp_perf.rs:26-33` already declares
`apply_client_tcp_perf_options(stream, mode)`. It runs from two
`connect`-side call sites:

- `crates/core/src/client/module_list/listing.rs:422` (module-list
  query path).
- `crates/core/src/client/remote/daemon_transfer/mod.rs:141` (rsync://
  transfer path).

NET-NSL.2 leaves these call sites unchanged; the constant bump flows
through the shared `fast_io::DEFAULT_TCP_NOTSENT_LOWAT`.

### Why not pre-`listen` on the daemon listener socket?

The kernel applies `TCP_NOTSENT_LOWAT` per accepted socket, not per
listener. Setting it on the `TcpListener` is a silent no-op. The audit
already cataloged this; the listener `bind` -> `listen` path stays the
TFO-only surface.

## Socket option

```c
setsockopt(fd, IPPROTO_TCP, TCP_NOTSENT_LOWAT, &val, sizeof(val));
```

`val` is a `c_int` byte count. Linux 3.12+; Darwin 10.13+; FreeBSD
11.0+; Windows has no direct equivalent. The audit documents the
per-platform constant values; `crates/fast_io/src/socket_options.rs`
already abstracts those behind `set_tcp_notsent_lowat`.

## Chosen default: 256 KiB

`DEFAULT_TCP_NOTSENT_LOWAT` moves from `64 * 1024` to `256 * 1024`.
Rationale:

- 256 KiB keeps the pipe full on 1 Gbps WAN at up to ~2 ms RTT and on
  100 Mbps WAN at up to ~20 ms RTT. The current 64 KiB ceiling clamps
  throughput on 10 GbE LAN paths (BDP ~125 KiB at 0.1 ms RTT) to
  roughly 50% line rate.
- 256 KiB stays small enough to react to throughput drops within a
  multiplex frame round-trip (8 KiB max frame; one full buffer drains
  in <1 ms at 1 Gbps).
- Matches the macOS Safari production default (`131072`-`262144`
  depending on workload class) and the Linux net mailing list
  recommendation for general WAN sync workloads in the NET-NSL audit.
- Stays an order of magnitude below the default `tcp_wmem` ceiling
  (4 MiB), so kernel autotuning of the send buffer is unaffected.

## Helper API

`crates/fast_io/src/socket_options.rs` already exposes
`set_tcp_notsent_lowat(stream, bytes) -> io::Result<bool>`. NET-NSL.2
adds a thin convenience wrapper that mirrors the NET-TFO.2 naming
pattern (`enable_tcp_fastopen_listener`) so call sites read uniformly:

```rust
/// Enable `TCP_NOTSENT_LOWAT` on a connected stream at the default
/// watermark. Returns `Ok(false)` on platforms without support.
/// Errors are best-effort: callers should log at debug and continue.
pub fn enable_notsent_lowat(stream: &TcpStream) -> io::Result<bool> {
    set_tcp_notsent_lowat(stream, DEFAULT_TCP_NOTSENT_LOWAT)
}
```

The existing `set_tcp_notsent_lowat(stream, bytes)` stays the
configurable entry point for tests and future operator-tunable knobs.
The new `enable_notsent_lowat` is the default-policy entry point that
daemon and client both call.

## Cross-platform behaviour

| Platform | Behaviour |
| --- | --- |
| Linux 3.12+ | `setsockopt(SOL_TCP, TCP_NOTSENT_LOWAT, 256 KiB)`; `Ok(true)`. |
| Linux <3.12 | `setsockopt` returns `ENOPROTOOPT`; logged at debug; not fatal. |
| macOS 10.13+ | Darwin hardcoded constant `0x201`; `Ok(true)`. |
| FreeBSD 11+ | Hardcoded constant `41`; `Ok(true)`. |
| Windows | No equivalent; `Ok(false)`. Auto-Tuning Level + future NET-RIO handle similar bloat shaping. |
| Other Unix | `Ok(false)` no-op. |

The platform table is enforced today by `tcp_notsent_lowat_supported()`
and the per-platform `#[cfg]` arms in `set_tcp_notsent_lowat`.

## Failure handling

- `setsockopt` returning `ENOPROTOOPT` (old kernels, unsupported
  variants): downgrade to `log::debug!` and continue. Documented in
  the helper rustdoc.
- `setsockopt` returning `EPERM` (sandbox / seccomp): debug-log and
  continue. The audit notes this is the rootless-container failure
  mode for related TCP knobs; same treatment applies here.
- `setsockopt` returning `EINVAL` with a watermark above the effective
  send buffer: debug-log and continue. The kernel silently clamps in
  most builds; this branch is defensive.
- Any other error: debug-log and continue. NET-NSL is an optimisation,
  never a correctness requirement.

The current call sites use `let _ = ...`; NET-NSL.2 keeps that pattern
but adds the debug log inside `enable_notsent_lowat` (Linux path only;
unsupported platforms return `Ok(false)` and never trip the log).

## Interaction with existing TCP tuning

- `TCP_NODELAY`: orthogonal. Nagle disables segment coalescing for
  small writes; `TCP_NOTSENT_LOWAT` bounds the unsent-queue depth.
  Both can be set on the same socket without conflict and are
  designed to be used together.
- `TCP_FASTOPEN` (NET-TFO.2): orthogonal. TFO operates on the SYN
  exchange before the lowat path exists.
- `SO_SNDBUF` (operator override via `--sockopts=SO_SNDBUF=N` or
  `socket options = SO_SNDBUF=N`): the audit documents this is the
  one option that can interact. The watermark must be strictly less
  than the effective send buffer or the option never fires. With
  kernel autotuning left on (the default), `tcp_wmem` grows the send
  buffer above 256 KiB and the watermark behaves as designed.
  NET-NSL.2 does not detect or override operator-supplied
  `SO_SNDBUF`; that escalation is a follow-up if bench shows
  measurable regressions on small-`SO_SNDBUF` setups.
- Bandwidth limiter (`crates/bandwidth/src/limiter/`): no interaction.
  The throttler sleeps before submitting more bytes, so the kernel
  buffer rarely fills above the watermark.
- `BufferPool` / multiplex 32 KiB user buffer: no interaction. The
  lowat path bounds kernel-side queueing only.

## CLI surface

No user-facing flag in NET-NSL.2. The default auto-enables on Linux,
macOS, and FreeBSD; Windows skips silently. The audit recommends
exposing `--tcp-notsent-lowat=NBYTES` once bench numbers from NET-NSL.3
inform a credible operator default; NET-NSL.2 stays scope-tight to the
constant bump and the helper rename so the bake window can collect
clean signal.

Operators who need to tune today can:

- Patch `DEFAULT_TCP_NOTSENT_LOWAT` in `fast_io/src/socket_options.rs`
  and rebuild.
- Set `SO_SNDBUF` below the watermark via `--sockopts=` /
  `socket options =` to neuter the cap (the kernel never queues more
  than the send-buffer ceiling, so the watermark becomes
  irrelevant).

A future env-var override (e.g. `OC_RSYNC_TCP_NOTSENT_LOWAT=NBYTES`,
power-of-two validation matching the existing `fast_io` chunk-size
env-var pattern) can land in a separate PR if user feedback warrants
it. NET-NSL.2 does not introduce this.

## Testing strategy

- Reuse the existing `crates/fast_io/tests/tcp_perf_socket_options.rs`
  round-trip test. Extend it to assert
  `enable_notsent_lowat(stream)` returns `Ok(true)` on supported
  platforms and `Ok(false)` on Windows / other Unix.
- Unit test in `socket_options.rs` (test module) confirming the new
  wrapper delegates to `set_tcp_notsent_lowat` with the default.
- No interop tests: the option is invisible on the wire and upstream
  rsync never emits it.
- No bench in this PR. NET-NSL.3 owns WAN-latency / RTT benchmarks.

## Out of scope

- `--tcp-notsent-lowat=NBYTES` CLI flag (audit follow-up).
- `tcp notsent lowat = N` daemon config directive (audit follow-up).
- LAN-detection heuristics to skip the watermark on loopback / RFC1918
  (audit follow-up; NET-NSL.3 should answer whether this is needed).
- `SO_SNDBUF`-aware downgrade logic (audit follow-up).
- Windows Auto-Tuning / Registered I/O parity (NET-RIO).
- Client-side TFO (NET-TFO.3).

## Follow-ups

- **NET-NSL.3**: bench the watermark on WAN-latency paths (5 ms /
  50 ms / 200 ms RTT, 100 Mbps / 1 Gbps), compare 64 KiB / 128 KiB /
  256 KiB / 512 KiB / unset; produce a per-link recommendation and
  decide whether to expose the CLI / config knob from the audit
  follow-ups.

## Cross-references

- Audit: [`docs/design/net-nsl-audit.md`](net-nsl-audit.md).
- NET-TFO.2 pattern reference:
  [`docs/design/net-tfo-availability-audit.md`](net-tfo-availability-audit.md)
  and PR #5993.
- Helper module: `crates/fast_io/src/socket_options.rs`
  (`set_tcp_notsent_lowat`, `tcp_notsent_lowat_supported`,
  `DEFAULT_TCP_NOTSENT_LOWAT`).
- Daemon insertion: `crates/daemon/src/daemon/sections/server_runtime/listener.rs:388-394`
  with call sites at
  `crates/daemon/src/daemon/sections/server_runtime/connection.rs:361,503`.
- Client insertion: `crates/core/src/client/module_list/tcp_perf.rs:26-33`
  with call sites at
  `crates/core/src/client/module_list/listing.rs:422` and
  `crates/core/src/client/remote/daemon_transfer/mod.rs:141`.
