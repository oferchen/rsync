# NET-TFO.2 - Daemon listener TCP Fast Open design

Status: DESIGN. Pins the existing server-side TFO call site and the
contract every future TFO change must respect. Follows NET-TFO.1
(`docs/design/net-tfo-availability-audit.md`). Feeds NET-TFO.3
(client connect), NET-TFO.4 (CLI flag surface), NET-TFO.5 (WAN bench).

## Scope

The audit (NET-TFO.1) catalogues what is supported per platform and
records that server-side TFO already ships at
`crates/daemon/src/daemon/sections/server_runtime/listener.rs:204-226`.
This design doc does not re-justify TFO. It pins:

- The exact insertion point on the listener.
- The platform abstraction boundary (`fast_io::socket_options`).
- The failure handling contract.
- The CLI/runtime gate contract that NET-TFO.4 must honour.
- The deployment preconditions (sysctl, qlen).

Anything outside that envelope is explicitly deferred.

## Insertion point

The daemon listener is built in
`crates/daemon/src/daemon/sections/server_runtime/listener.rs` via
`build_tcp_listener_with_fastopen(addr, backlog, tcp_fastopen)`.
The sequence is fixed:

1. `socket2::Socket::new(domain, STREAM, TCP)` (line 180).
2. `set_reuse_address(true)` (line 192).
3. If IPv6, `set_only_v6(true)` (line 197).
4. `bind(&addr.into())` (line 201).
5. **`enable_tcp_fastopen_raw(fd, DEFAULT_TCP_FASTOPEN_QLEN)`**
   (lines 209-226), gated on `TcpFastOpenMode::is_enabled()` and
   `fast_io::tcp_fastopen_listener_supported()`.
6. `listen(backlog)` (line 228).

The TFO call MUST sit between `bind` and `listen`. Linux accepts a
later `setsockopt(TCP_FASTOPEN)`, but the SYN cookie cache is most
effective when installed before the first SYN is processed; placing
the call after `listen` opens a brief window in which inbound SYNs
miss the cookie path. FreeBSD's semantics are the same.

Re-ordering steps 4 and 5 is a regression. Step 6 must remain the
last step. Test coverage for this ordering lives at
`crates/fast_io/src/socket_options.rs:397`
(`enable_tcp_fastopen_listener_reports_supported_platforms`).

## Platform abstraction

All platform divergence is funnelled through one safe API in
`fast_io::socket_options`:

```
pub fn enable_tcp_fastopen_raw(fd_or_socket, qlen) -> io::Result<bool>
pub fn enable_tcp_fastopen_listener(&TcpListener, qlen) -> io::Result<bool>
pub fn tcp_fastopen_listener_supported() -> bool
pub const DEFAULT_TCP_FASTOPEN_QLEN: i32 = 128
```

The daemon listener calls the `_raw` variant because it still holds a
`socket2::Socket` (not yet a `TcpListener`) at step 5. The contract is
that the returned `Ok(true)` means the kernel accepted the option,
`Ok(false)` means the platform stubs the call (Darwin without
entitlement, Windows, illumos), and `Err(e)` means the call site
attempted the option and the kernel rejected it (e.g. sysctl off).

| Platform        | Option                                          | qlen control  | Return value             |
|-----------------|-------------------------------------------------|---------------|--------------------------|
| Linux           | `setsockopt(IPPROTO_TCP, TCP_FASTOPEN, qlen)`   | qlen = backlog hint, capped at `net.ipv4.tcp_fastopen_blackhole_timeout_sec`-driven cookie limits | `Ok(true)` on success, `Err` on EPERM/EOPNOTSUPP/EINVAL |
| FreeBSD         | `setsockopt(IPPROTO_TCP, TCP_FASTOPEN, qlen)`   | qlen ignored on stable; treat as Linux-equivalent | `Ok(true)` on success, `Err` if `net.inet.tcp.fastopen.server_enable=0` |
| macOS / iOS     | n/a (entitlement-gated)                         | n/a           | `Ok(false)` permanent stub |
| Windows         | n/a in listener form; `WSAIoctl` is connect-side | n/a          | `Ok(false)` permanent stub |
| illumos         | n/a                                             | n/a           | `Ok(false)` permanent stub |

The cross-platform divergence is intentional: callers MUST NOT branch
on `cfg(target_os = ...)` themselves. All branching lives inside
`enable_tcp_fastopen_raw`. New platforms are added by extending the
`cfg` cascade at
`crates/fast_io/src/socket_options.rs:97-126`.

### Why not abstract `TCP_FASTOPEN` itself

Linux and FreeBSD agree on the option number (`TCP_FASTOPEN` in
`libc`) but disagree on the meaning of `qlen` (Linux uses the value
as a queue depth; FreeBSD ignores most non-zero values and treats it
as a boolean enable). The abstraction is at the function level, not
the option level, so future BSDs whose `TCP_FASTOPEN` semantics drift
do not pollute the daemon listener call site.

## Failure handling

`setsockopt(TCP_FASTOPEN)` can fail for benign reasons:

- `EPERM` - the sysctl `net.ipv4.tcp_fastopen` does not enable bit 1
  (server). The kernel refuses the option without root.
- `EOPNOTSUPP` - kernel below 3.7 or a build without TFO support.
- `EINVAL` - qlen out of range (negative or above the sysctl cap).

The daemon MUST NOT abort on any of these. Current behaviour
(`let _ = ...` at listener.rs:213) swallows the result; this design
upgrades that to a debug-level log carrying the kernel errno and the
attempted qlen. The listener proceeds to `listen(backlog)` regardless.
Rationale: TFO is an optimisation. A daemon that refuses to accept
connections because TFO is unavailable would regress every container
deployment without sysctl access (rootless Podman, most K8s pods - see
NET-TFO.1 audit section "Containers and Kubernetes").

The audit's recommendation to "switch to `if let Err(e) = ...` and
log at debug level" is binding on the NET-TFO.2 implementation
follow-up. The log message format is:

```
TCP_FASTOPEN setsockopt failed (errno {}, qlen {}); daemon will accept
connections without TFO
```

No INFO or WARN level. Operators who care can grep debug logs;
operators who do not care should not see the line.

## CLI / runtime gate (NET-TFO.4 contract)

The `--tcp-fastopen` flag parses into
`core::client::config::enums::tcp_fastopen::TcpFastOpenMode`
(see audit table at NET-TFO.1, row 12: `tcp_fastopen.rs:15-104`).
The daemon listener honours the mode as follows:

| Mode  | Listener behaviour                                                                                                    |
|-------|-----------------------------------------------------------------------------------------------------------------------|
| `Auto` (default) | Probe `tcp_fastopen_listener_supported()`. If `true`, set `TCP_FASTOPEN`. If `false`, skip silently. No warning. |
| `On`  | Always attempt to set `TCP_FASTOPEN`. If `tcp_fastopen_listener_supported()` returns `false`, emit the one-shot warning at `listener.rs:241-267`. |
| `Off` | Never call `enable_tcp_fastopen_raw`. The listener path skips step 5 entirely.                                       |

The default is `Off`, NOT `Auto`. Rationale per NET-TFO.1 audit
("Risk summary"): middlebox SYN-with-data loss is the dominant
operational risk on the public internet, and the daemon listener is
the side that exposes the surface. Defaulting to `Off` keeps every
existing deployment byte-identical to upstream rsync on the wire
until an operator explicitly opts in. NET-TFO.4 may revisit this
once WAN bench numbers (NET-TFO.5) demonstrate consistent wins
above middlebox-loss noise.

## qlen sizing

`DEFAULT_TCP_FASTOPEN_QLEN = 128` matches the kernel's historical
default for `net.core.somaxconn`. The implementation follow-up MUST
expose qlen as a function of the daemon's `max_connections` directive:
when `max_connections` is set, `qlen = min(max_connections, 1024)`.
The 1024 cap is the practical ceiling on Linux before the cookie
cache starts thrashing under SYN flood. When `max_connections` is
unset (no admission cap), qlen falls back to `DEFAULT_TCP_FASTOPEN_QLEN`.
The implementation MUST NOT exceed 1024 even if `max_connections`
does; oversized qlen is a denial-of-service amplifier.

## Sysctl preconditions (deployment)

Server-side TFO requires `net.ipv4.tcp_fastopen` bit 1 set
(`echo 2 > /proc/sys/net/ipv4/tcp_fastopen` for server-only,
`echo 3` for client+server). This is a host-wide knob; the daemon
cannot set it from user space without `CAP_NET_ADMIN`. The
deployment documentation (NET-TFO.4 follow-up) MUST surface:

- The sysctl bit and how to set it persistently
  (`/etc/sysctl.d/99-rsync-tfo.conf`).
- Container guidance: rootless Podman inherits host; K8s pods need
  `securityContext.sysctls: [{name: net.ipv4.tcp_fastopen, value: "3"}]`
  plus a kubelet `--allowed-unsafe-sysctls=net.ipv4.tcp_fastopen` entry.
- FreeBSD equivalent: `sysctl net.inet.tcp.fastopen.server_enable=1`.

Without the sysctl, the listener attempts the setsockopt and gets
`EPERM`. The debug log fires, the daemon continues. This is the
correct silent-degrade behaviour for an optimisation.

## Windows deferral

Windows exposes TFO via `WSAIoctl(SIO_TCP_INITIAL_RTO)` on the
connect side and a separate `TCP_FASTOPEN` socket option that
requires registered I/O on the listener side (Win 10 1607+ /
Server 2016+). The control surface differs enough from POSIX that a
listener path needs a separate code branch with its own test matrix.
NET-TFO.2 ships the Windows path as a permanent `Ok(false)` stub
(`crates/fast_io/src/socket_options.rs:134-139`). A future task
NET-TFO.W will revisit if a Windows daemon deployment materialises;
until then the gap is documented in NET-TFO.1's per-platform table.

## Test contract

The implementation follow-up MUST land:

- `enable_tcp_fastopen_listener_reports_supported_platforms` (already
  in tree at `fast_io/src/socket_options.rs:397`) extended to assert
  the debug log fires on EPERM and EOPNOTSUPP.
- A daemon integration test that boots the listener with
  `tcp_fastopen=Off` and asserts no `setsockopt(TCP_FASTOPEN)` syscall
  is issued (strace assertion, gated to Linux).
- A daemon integration test that boots with `tcp_fastopen=On` on a
  Linux host with sysctl=0 and asserts the daemon still accepts a
  connection.

No new wire protocol tests are required: TFO only affects the SYN
exchange, not the rsync protocol that follows. The existing
upstream-interop matrix already covers wire-compatible behaviour.

## Follow-up tasks

- NET-TFO.3 (#4254) - client-side connect path
  (`TCP_FASTOPEN_CONNECT` on Linux, `connectx` on macOS,
  `Ok(false)` stub on Windows; insertion point at
  `crates/core/src/client/module_list/tcp_perf.rs:26-33`, where
  the `mode` argument is already reserved).
- NET-TFO.4 (#4255) - CLI flag wiring and documentation
  (`--tcp-fastopen=auto|on|off`, default `Off`).
- NET-TFO.5 (#4256) - WAN-latency bench
  (single-RTT measurement vs upstream rsync on a 100 ms loopback).
- NET-TFO.W (future) - Windows listener path via registered I/O.
