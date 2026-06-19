# NET-TFO.1 - TCP Fast Open availability and privilege model audit

Status: AUDIT. Feeds NET-TFO.2 (server impl) and NET-TFO.3 (client impl).

TCP Fast Open (RFC 7413) piggy-backs application data on the TCP SYN/SYN-ACK
exchange, saving one RTT per connection at the cost of a per-host cookie
cache and middlebox interaction risk. Server-side TFO already ships in
`fast_io::socket_options::enable_tcp_fastopen_raw`; client-side is deferred.
This audit catalogues platform support, privilege rules, and exact wiring
points so NET-TFO.2/.3 can land surgically.

## Per-platform availability and privilege

| Platform       | Server (listener)             | Client (connect)              | Privilege / control surface                                                                       | Kernel/OS floor                          |
|----------------|-------------------------------|-------------------------------|---------------------------------------------------------------------------------------------------|------------------------------------------|
| Linux          | `setsockopt(TCP_FASTOPEN, qlen)` pre-`listen` | `sendto(MSG_FASTOPEN, ...)` or `setsockopt(TCP_FASTOPEN_CONNECT, 1)` | `sysctl net.ipv4.tcp_fastopen` bitmask: bit0 client, bit1 server, bit2 client-without-cookie, bit3 server-without-cookie. No CAP needed to *use*. Writing the sysctl needs root (`CAP_NET_ADMIN` for namespaced sysctls). Cookie cache is per-source-IP in kernel. | 3.6 client, 3.7 server full       |
| macOS / iOS    | Effectively unavailable from user space | `connectx(2)` with `CONNECT_DATA_IDEMPOTENT` | Listener TFO requires per-process entitlement; treated as unsupported in oc-rsync (`enable_tcp_fastopen_raw` returns `Ok(false)` on Darwin). Client `connectx` is public but needs the data-on-SYN code path. | 10.11 (El Capitan) client; server gated by entitlement |
| Windows        | No first-class listener API   | `WSAIoctl(SIO_TCP_INITIAL_RTO)` plus `ConnectEx` with send buffer | TFO socket option exists (`TCP_FASTOPEN` since Win 10 1607 / Server 2016) but flow is registered-IO style; no public listener equivalent. oc-rsync's stub returns `Ok(false)`. | Win 10 1607 / Server 2016                |
| FreeBSD        | `setsockopt(TCP_FASTOPEN, qlen)` pre-`listen` | `setsockopt(TCP_FASTOPEN, 1)` then `sendto` with data on first call | `sysctl net.inet.tcp.fastopen.{server_enable,client_enable}` (off by default). No CAP needed once sysctl set. | 12.0                                    |
| illumos        | Not implemented (SmartOS / OmniOS) | Not implemented            | N/A                                                                                               | -                                       |

Containers and Kubernetes inherit the host's `tcp_fastopen` sysctl unless
the runtime configures a network namespace with its own value. Rootless
Podman and unprivileged K8s pods cannot rewrite the sysctl; they must
inherit. A `securityContext.sysctls` entry of `net.ipv4.tcp_fastopen=3` is
required to enable both directions in a pod, and the sysctl must be
whitelisted in the kubelet (`--allowed-unsafe-sysctls`). Setting the
listener `TCP_FASTOPEN` option itself does not require any capability;
only changing the kernel-wide knob does.

## Middlebox and operational risk

- Carrier-grade NATs, transparent proxies, and some firewalls drop SYNs
  carrying data, leading to a 1-RTT fallback that masquerades as connect
  latency. RFC 7413 section 6 documents the blackhole risk.
- Linux maintains a per-destination "TFO known-bad" cache and disables
  client TFO for that peer after repeated failures. The cache lives in
  the kernel and survives across rsync invocations on the same host.
- TFO cookies are 4-16 bytes; they leak no application data but do
  identify the source-IP+server pair to passive observers.

The combination argues for a default-off CLI flag with explicit opt-in.

## Existing TCP setsockopt sites in oc-rsync

| File                                                                                  | Line(s)      | Option(s)                                       | Role            |
|---------------------------------------------------------------------------------------|--------------|-------------------------------------------------|-----------------|
| `crates/fast_io/src/socket_options.rs`                                                | 53-79        | `set_socket_int_option` / `set_listener_int_option` safe wrappers | infrastructure |
| `crates/fast_io/src/socket_options.rs`                                                | 81-162       | `enable_tcp_fastopen_raw` / `enable_tcp_fastopen_listener` (Linux + FreeBSD live, macOS/Windows stub) | infrastructure |
| `crates/fast_io/src/socket_options.rs`                                                | 164-214      | `set_tcp_notsent_lowat` (Linux + macOS + FreeBSD) | infrastructure |
| `crates/daemon/src/daemon/sections/server_runtime/listener.rs`                        | 192          | `SO_REUSEADDR` via `socket2::Socket::set_reuse_address` | daemon listener |
| `crates/daemon/src/daemon/sections/server_runtime/listener.rs`                        | 197          | `IPV6_V6ONLY` via `set_only_v6` (dual-stack)    | daemon listener |
| `crates/daemon/src/daemon/sections/server_runtime/listener.rs`                        | 209-226      | server-side `TCP_FASTOPEN` (gated on `TcpFastOpenMode::is_enabled` + platform support) | daemon listener |
| `crates/daemon/src/daemon/sections/server_runtime/listener.rs`                        | 233-237      | `SO_RCVTIMEO` / `SO_SNDTIMEO` via `configure_stream` | accepted stream |
| `crates/daemon/src/daemon/sections/server_runtime/socket_options.rs`                  | 50-83        | per-connection `TCP_NODELAY` / `SO_KEEPALIVE` parsed from `socket options` directive | accepted stream |
| `crates/core/src/client/module_list/connect/direct.rs`                                | 134-154      | `socket2::Socket` connect path (TCP source-bind, optional `connect_timeout`) | client connect  |
| `crates/core/src/client/module_list/tcp_perf.rs`                                      | 26-33        | client `TCP_NOTSENT_LOWAT` apply (`TcpFastOpenMode` argument reserved, not used yet) | client connect  |
| `crates/core/src/client/module_list/socket_options/lookup.rs`                         | 18-93        | `SO_KEEPALIVE`, `SO_REUSEADDR`, `TCP_NODELAY` from `--sockopts`/daemon directive | client connect  |
| `crates/core/src/client/remote/daemon_transfer/mod.rs`                                | 160          | `TCP_NODELAY` on transfer stream                | client transfer |
| `crates/core/src/client/config/enums/tcp_fastopen.rs`                                 | 15-104       | `TcpFastOpenMode` (`Auto`/`On`/`Off`) + parser  | CLI surface     |

Twelve distinct TCP-tuning sites already touch socket options; the
listener TFO call slots in at `listener.rs:209-226` and the client-side
slot is `tcp_perf.rs:26-33` (the `mode` argument is reserved precisely
for NET-TFO.3).

## Recommended approach for NET-TFO.2 / NET-TFO.3

NET-TFO.2 (server) is largely shipped. Remaining work is verification:

- Confirm `fast_io::enable_tcp_fastopen_raw` returns `Ok(true)` on
  Linux + FreeBSD CI cells; the existing `tcp_perf_socket_options.rs`
  test already exercises this.
- Surface a debug log when the kernel returns `EPERM` / `EOPNOTSUPP`
  (sysctl bit unset). The current path swallows the result via
  `let _ = ...`; switch to `if let Err(e) = ...` and log at debug
  level so operators can diagnose "TFO not engaging" without losing
  the optimisation.

NET-TFO.3 (client) is the larger piece. The Linux client path cannot
use plain `connect` + `write`: TFO requires the first payload byte to
ride the SYN. Two options:

- `setsockopt(TCP_FASTOPEN_CONNECT, 1)` before `connect(2)` (Linux
  4.11+). The kernel buffers the first `write` and emits it with the
  SYN. This keeps the existing `connect`/`write` flow unchanged - the
  cleanest landing.
- `sendto(fd, buf, MSG_FASTOPEN, ...)` adapter. Required for older
  kernels but introduces a control-flow split at the first
  socket-write site. Reject for now; we already require modern kernels
  for io_uring features.

Recommend `TCP_FASTOPEN_CONNECT` on Linux with graceful fallback when
the setsockopt fails (kernel below 4.11). macOS gets `connectx` if we
choose to invest; Windows stays an `Ok(false)` no-op until a real API
materialises. The `TcpFastOpenMode::Auto` setting should leave the
option off until we accumulate cookie-cache hits across CI runs - cold
caches give zero benefit (the first connection still pays 1 RTT to
fetch a cookie).

For NET-TFO.4 (CLI surface), the tri-state already exists
(`TcpFastOpenMode::{Auto, On, Off}` in `tcp_fastopen.rs`). Defaults:

- `Auto`: probe support, enable server-side, defer client-side until
  a cookie is cached (Linux-only path).
- `On`: enable everywhere supported, warn once if the platform stubs
  to `Ok(false)`. The existing one-shot warning at
  `listener.rs:241-267` is the template.
- `Off`: skip every TFO setsockopt. This is the **recommended
  default-default** for new builds until WAN bench numbers
  (NET-TFO.5) show consistent wins above middlebox-loss noise.

## Risk summary

- Middlebox SYN-with-data loss is the dominant operational risk;
  default-off mitigates.
- Kernel cookie cache is per-source-IP and survives across processes;
  rsync's typical pattern (short-lived bursts to the same daemon) is
  the workload TFO targets.
- Privilege model is benign on Linux/FreeBSD: no capability needed at
  the application layer; the sysctl is an operator-side concern.
- Container deployments without sysctl access (rootless Podman, most
  K8s pods) silently skip the optimisation - no error, no warning.
- Windows and illumos are permanent no-ops; document in the
  per-platform availability table above so users do not file
  "TFO not engaging on Windows" bugs.
