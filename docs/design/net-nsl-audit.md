# NET-NSL Audit: TCP_NOTSENT_LOWAT Defaults and TCP Tuning Interaction

Status: Audit (NET-NSL.1). Implementation lives in NET-NSL.2 and benchmarking in NET-NSL.3.

`TCP_NOTSENT_LOWAT` caps the amount of unsent data the kernel buffers in a
socket's send buffer. Lowering the watermark reduces buffer bloat at the
expense of more frequent wake-ups of the producer. On bandwidth-fat,
latency-fat WAN links, a high watermark hides the true RTT from the application
and inflates time-to-first-byte for control frames the multiplex layer
interleaves with bulk file data.

## Per-Platform Availability and Defaults

| Platform | Kernel floor | Constant value | Default unsent cap | Notes |
| --- | --- | --- | --- | --- |
| Linux | 3.12+ | `libc::TCP_NOTSENT_LOWAT` (25) | `UINT_MAX` (effectively unlimited) | Read via `/proc/sys/net/ipv4/tcp_notsent_lowat`; per-socket override via `setsockopt(IPPROTO_TCP, TCP_NOTSENT_LOWAT, ...)`. |
| macOS / iOS | 10.13+ | Not exported by libc; hardcoded `0x201` (matches `/usr/include/netinet/tcp.h`) | Effectively unlimited until set | Used in production by Safari/HTTPS stack since 10.13. |
| FreeBSD | 11.0+ | Hardcoded `41` (IPPROTO_TCP option) | Effectively unlimited until set | Wire-compatible with Linux semantics. |
| Windows | Not supported | -- | -- | No direct equivalent. Closest knobs are `SIO_IDEAL_SEND_BACKLOG_*` and Registered I/O `RIO_BUF`; both pursued separately under NET-RIO. |
| Other | Not supported | -- | -- | `tcp_notsent_lowat_supported()` reports `false`; setter is an `Ok(false)` no-op. |

Recommended values for `setsockopt` argument (per the Linux net mailing list
guidance and existing macOS HTTPS deployments):

| Workload class | Watermark | Rationale |
| --- | --- | --- |
| LAN-only daemon | unset or 256 KiB | LAN throughput requires deeper buffering to keep the wire fed; small watermarks burn CPU on wakeups. |
| WAN sync (default) | 128 KiB-256 KiB | Caps queueing delay below ~5 ms at 100 Mbps; pacing window matches multiplex 32 KiB buffer + 8 KiB frame. |
| Conservative WAN | 64 KiB | Matches the Linux fast-path internal value; safe default with no measurable throughput loss in upstream benchmarks. |

## Existing TCP-Setsockopt Site Inventory

Production setsockopt sites in the workspace (4 distinct dispatch surfaces;
test-only helpers omitted):

- `crates/fast_io/src/socket_options.rs:53-79` -- `set_socket_int_option` /
  `set_listener_int_option` safe wrappers over `setsockopt(2)` / Winsock
  `setsockopt`.
- `crates/fast_io/src/socket_options.rs:98-126` -- `enable_tcp_fastopen_raw`
  (Linux/FreeBSD enabled, macOS/iOS/Windows reported unsupported).
- `crates/fast_io/src/socket_options.rs:172-214` -- `set_tcp_notsent_lowat`
  (Linux libc constant, Darwin/FreeBSD hardcoded constants, fallback
  `Ok(false)` no-op).
- `crates/daemon/src/daemon/sections/server_runtime/socket_options.rs:154-167`
  -- daemon `socket options =` directive applies `TCP_NODELAY`, `SO_SNDBUF`,
  `SO_RCVBUF`, `SO_KEEPALIVE`, `IP_TOS` via `socket2` typed setters and the
  fast_io int wrappers. Parsed at config load
  (`socket_options.rs:48-90`).
- `crates/daemon/src/daemon/sections/server_runtime/listener.rs:388-394` --
  `apply_accepted_stream_tcp_notsent_lowat` runs the default 64 KiB watermark
  per accepted client. Called from
  `connection.rs:361` and `connection.rs:503`.
- `crates/daemon/src/daemon/sections/module_access/transfer.rs:466` --
  `setup_transfer_streams` forces `TCP_NODELAY` once handshake completes and
  the transfer phase starts.
- `crates/daemon/src/daemon_stream.rs:113-122` -- `set_nodelay` adapter over
  `Plain` / `Tls` wrapped streams.
- `crates/core/src/client/module_list/tcp_perf.rs:26-33` --
  `apply_client_tcp_perf_options` invokes the same 64 KiB
  `DEFAULT_TCP_NOTSENT_LOWAT` watermark on the client side after the rsync://
  TCP connect. Reserved `TcpFastOpenMode` arg for the deferred client TFO
  follow-up.
- `crates/core/src/client/module_list/listing.rs:422` and
  `crates/core/src/client/remote/daemon_transfer/mod.rs:141` -- the two
  production call sites that drive `apply_client_tcp_perf_options`.
- `crates/core/src/client/module_list/connect/mod.rs:256` --
  `configure_transfer_options` toggles `TCP_NODELAY` and read/write timeouts
  on the connected client stream entering the transfer phase.
- `crates/core/src/client/module_list/socket_options/` -- `--sockopts=` user
  parser, lookup table, and apply path (`mod.rs`, `lookup.rs`, `types.rs`,
  `apply.rs`, `consts.rs`).

`SO_SNDBUF` is not auto-tuned by oc-rsync today. The kernel's autotuning
(`net.ipv4.tcp_wmem`) sizes the send buffer dynamically, and the explicit
`SO_SNDBUF=N` user knob disables Linux autotuning -- a footgun called out
in upstream `socket.c`.

## Interaction Analysis

`TCP_NOTSENT_LOWAT` is orthogonal to the rsync wire format -- it only
shapes when the kernel grants `epoll_wait`/`POLLOUT` to user space. The
relevant interactions:

- **Multiplex frame buffering** (`crates/protocol/src/multiplex/writer.rs`)
  uses a 32 KiB user-space buffer and 8 KiB max frame size. With the
  watermark at 64-256 KiB, the kernel will keep at least one full multiplex
  buffer in flight before unblocking the writer; throughput is unaffected
  but latency-sensitive control frames (`MSG_ERROR`, `MSG_NO_SEND`,
  `NDX_DEL_STATS`) reach the peer with bounded delay instead of riding
  behind hundreds of KiB of `MSG_DATA`.
- **User `SO_SNDBUF`** via `--sockopts=SO_SNDBUF=N` or `socket options =
  SO_SNDBUF=N` is the most likely conflicting setting. `TCP_NOTSENT_LOWAT`
  must be strictly less than the effective send buffer; otherwise it
  silently never triggers. With autotuning left on, the kernel grows the
  send buffer above any reasonable watermark and the lowat behaves as
  designed. NET-NSL.2 must skip the setsockopt when the operator has set
  `SO_SNDBUF` below the watermark.
- **`BufferPool`** (`crates/engine/src/local_copy/buffer_pool/`) governs
  user-space allocation reuse for the I/O hot path; it has no interaction
  with kernel socket buffering and does not need to change.
- **Bandwidth limiter** (`crates/bandwidth/src/limiter/`) throttles by
  sleeping the producer between batched writes. Lowering the unsent cap
  reduces the kernel's appetite, so a bwlimit-throttled run already keeps
  the lowat path inactive most of the time. No conflict, but no benefit
  either: NET-NSL.3 should bench bwlimited and unlimited transfers
  separately.
- **TCP_NODELAY** is currently forced on for daemon transfers and the
  client transfer phase. `TCP_NOTSENT_LOWAT` complements nodelay: nodelay
  stops Nagle from coalescing small writes, lowat stops the kernel from
  hoarding large writes. The two are designed to be used together.

## Recommended Insertion Point for NET-NSL.2

The plumbing already exists. NET-NSL.2 should:

1. Promote `apply_accepted_stream_tcp_notsent_lowat`
   (listener.rs:390) to a public helper exposed from
   `fast_io` so the daemon listener and the client tcp_perf path share one
   source of truth -- they do today, both calling
   `fast_io::set_tcp_notsent_lowat` with `DEFAULT_TCP_NOTSENT_LOWAT`.
2. Add a CLI / config knob so operators can override the watermark per
   transfer (`--tcp-notsent-lowat=NBYTES`, with `auto` matching today's
   64 KiB, `off` skipping the syscall, and an explicit integer for tuning
   experiments). Daemon-side: a `tcp notsent lowat = N` directive parsed
   alongside `socket options =`.
3. Detect operator-supplied `SO_SNDBUF=N` lower than the watermark and
   either downgrade the watermark to `min(N/2, watermark)` or skip the
   call with a `log::warn` -- not silently raise it.
4. Keep the apply path best-effort (`let _ = ...`) on all platforms.
   `TCP_NOTSENT_LOWAT` is a hint; a failing `setsockopt` must never abort
   a transfer.

Recommended initial production value: **128 KiB** (`128 * 1024`). Rationale:

- Doubles today's 64 KiB to give bulk throughput more headroom on
  100 Mbps+ links while keeping queueing delay below 12 ms.
- Stays an order of magnitude below typical Linux send-buffer high-water
  marks (default `tcp_wmem` max is 4 MiB), so autotuning is unaffected.
- Aligns with the macOS/Safari production default of 131072 used by Apple
  for HTTPS, which gives us a real-world bake reference.

## Risk

- **LAN throughput loss**: A watermark too small to cover one bandwidth-delay
  product caps throughput. At 10 Gbps LAN with 0.1 ms RTT, the BDP is ~125 KiB
  -- 64 KiB clamps to ~50% line rate, 128 KiB clamps to ~100%. The
  conservative posture is to detect LAN by remote-address heuristics
  (RFC1918 / loopback) and skip the watermark, or to expose the knob and
  default it to `auto` with a sensible distance-aware policy.
- **Operator surprise**: Production daemons with manually tuned `SO_SNDBUF`
  may regress if NET-NSL silently lowers the cap. Recommendation: only
  apply the watermark when no explicit `socket options = SO_SNDBUF=...`
  override is present, or when the operator opts in with `tcp notsent
  lowat = N`.
- **Windows gap**: No equivalent socket option. Document explicitly that
  Windows daemons rely on the kernel's `Auto-Tuning Level` heuristic and
  Registered I/O (NET-RIO follow-up) for similar buffer-bloat avoidance.
- **Best-effort errors**: The setter is intentionally lossy. NET-NSL.2
  should still log at `debug` when the kernel rejects the call so
  operators can correlate behaviour changes during the bake.

## Cross-References

- `crates/fast_io/src/socket_options.rs` -- safe setsockopt wrappers and
  the existing `set_tcp_notsent_lowat` helper.
- `crates/fast_io/tests/tcp_perf_socket_options.rs` -- existing round-trip
  test that NET-NSL.2 must extend with the operator-override path.
- Upstream `socket.c:set_socket_options()` -- the reference for how
  rsync wires `SO_SNDBUF` and friends; upstream does not call
  `TCP_NOTSENT_LOWAT`, so the feature is purely additive and wire-neutral.
