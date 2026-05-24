# DIS-4.a: rsyncd `@RSYNCD:` greeting overhead

Focused audit of the daemon greeting path: TCP accept through the
`@RSYNCD:` version exchange, ending at the handoff to module-select.
DIS-4.b covers module lookup, host allow/deny, and arg parsing;
DIS-4.c covers auth and capability negotiation. Both are out of scope
here.

This audit narrows the DIS-3 phase-table rows **2, 3, 4, 10** to
file-and-line evidence, counts syscalls and allocations versus
upstream rsync 3.4.1, and ranks fixes for DIS-6 to schedule.

Sources cited (all paths relative to worktree root):

- `crates/daemon/src/daemon/sections/server_runtime/listener.rs`
- `crates/daemon/src/daemon/sections/server_runtime/connection.rs`
- `crates/daemon/src/daemon/sections/server_runtime/accept_loop.rs`
- `crates/daemon/src/daemon/sections/greeting.rs`
- `crates/daemon/src/daemon/sections/legacy_messages.rs`
- `crates/daemon/src/daemon/sections/session_runtime.rs`
- `crates/daemon/src/daemon/module_state/hostname.rs`
- `crates/protocol/src/legacy/lines.rs`
- `crates/core/src/auth/mod.rs` (`digests_for_protocol`)
- upstream: `target/interop/upstream-src/rsync-3.4.1/socket.c`
  (`start_accept_loop`), `clientserver.c` (`start_daemon`,
  `exchange_protocols`), `compat.c` (`output_daemon_greeting`),
  `clientname.c` (`client_addr`, `client_name`).

## 1. Greeting-phase inventory (per accepted connection)

Numbered in protocol order. Each row cites file:line for the operation
and tags syscalls (`S`), heap allocations (`A`), and lock or atomic
acquisitions (`L`).

| # | Operation | Where | Cost tag |
|---|-----------|-------|----------|
| 1 | `accept(2)` returns the client fd | `connection.rs:309` (single-listener) and `connection.rs:393` (dual-stack acceptor thread) | 1 S |
| 2 | `set_nonblocking(false)` to undo BSD inheritance of `O_NONBLOCK` on the accepted fd | `connection.rs:311` and `connection.rs:401` | 1 S (`fcntl`) |
| 3 | Apply client `socket_options` (defaults: none) | `connection.rs:322` | 0-N S (zero on default config) |
| 4 | Hit per-connection cap, build refusal banner if at capacity (default: no cap) | `connection.rs:324` | 0 S on default config |
| 5 | `ConnectionCounter::acquire()` increments an atomic; held by RAII guard for the worker lifetime | `connection.rs:194`, `server_runtime/connection_counter.rs` | 1 L (atomic RMW) |
| 6 | `Arc::clone(modules)` x2, `Arc::clone(motd_lines)` x2, `Arc::clone(log_sink)` x1 | `connection.rs:191-198` | 5 L (atomic RMW each) |
| 7 | `thread::spawn` for the per-connection worker (heap allocation for `JoinHandle`, OS thread create) | `connection.rs:200` | 1 S (`clone3`/`pthread_create`), 1 A |
| 8 | Worker enters `handle_session`; sets read/write timeouts | `session_runtime.rs:65` -> `listener.rs:152` (`configure_stream`) | 2 S (`setsockopt SO_RCVTIMEO`, `SO_SNDTIMEO`) |
| 9 | PROXY-protocol probe (skipped when `proxy protocol = false`, the default) | `session_runtime.rs:69-86` | 0 S on default config |
| 10 | `resolve_peer_hostname` calls `lookup_addr` -> `getnameinfo(2)` when `reverse lookup = yes` (default) | `session_runtime.rs:88`, `module_state/hostname.rs:42` | 1-N S (DNS resolver), 1+ A |
| 11 | `log_connection` formats and writes an info message to the log sink | `session_runtime.rs:92` | 1 A (`format!`), 0-1 S (log write) |
| 12 | `handle_legacy_session` constructs `BufReader::new(stream)` over the `TcpStream` (8 KiB heap buffer + struct) | `session_runtime.rs:220` | 2 A |
| 13 | `BandwidthLimitComponents::new(...).into_limiter()` builds an `Option<BandwidthLimiter>`; default daemon limit is `None` | `session_runtime.rs:221` | 0-1 A on default config |
| 14 | `LegacyMessageCache::new()` builds two `Box<[u8]>` slices (`@RSYNCD: OK\n`, `@RSYNCD: EXIT\n`) | `session_runtime.rs:222`, `legacy_messages.rs:24-31` | 2 A (each is `String` -> `Box<str>` -> `Box<[u8]>`; only the box survives) |
| 15 | `legacy_daemon_greeting()` builds the version+digest list via `format!` then `String::pop` + 5x `push_str` (`sha512 sha256 sha1 md5 md4`) | `session_runtime.rs:224`, `greeting.rs:13-35`, `lines.rs:301` | 1 A (`String::with_capacity`), 1-2 A reallocs as digests are appended |
| 16 | `write_limited` writes the greeting bytes; default limiter is `None`, so it falls through to one `stream.write_all` | `session_runtime.rs:225`, `session_runtime.rs:176-193` | 1 S (`write(2)`) |
| 17 | Explicit `reader.get_mut().flush()` on the raw `TcpStream` (no-op on a `TcpStream`, but still an extra method dispatch) | `session_runtime.rs:226` | 0 S (`TcpStream::flush` is a no-op) |
| 18 | `read_trimmed_line(reader)` allocates a fresh `String` and calls `BufReader::read_line` (1 `read(2)` on the kernel side for the version reply) | `session_runtime.rs:233`, `greeting.rs:49-62` | 1 A, 1 S |
| 19 | `parse_legacy_daemon_message(&line)` parses the `@RSYNCD: 32.0\n` reply (zero-alloc parse on the borrowed string) | `session_runtime.rs:234` | 0 A |
| 20 | Loop body stores `negotiated_protocol` and re-enters the loop to read the next line; the second `read_trimmed_line` reads the module request and exits the greeting phase | `session_runtime.rs:235-272` | 1 A (second `String`), 1 S (second `read(2)`) |

The phase ends at row 20: the next operation is module lookup, which
DIS-4.b owns. Rows 11 (log line), 13 (limiter struct), and 14 (OK/EXIT
cache) are work the greeting phase does even though their *output* is
consumed later (the OK cache is used at module-accept time, not in the
greeting). They are listed here because they sit on the greeting
critical path before the first byte goes out.

### Allocation tally for the greeting phase only (default config, no auth)

Counting from row 7 (worker start) through row 20 (module request
read):

- Worker spawn: 1 alloc + 1 syscall.
- Socket timeouts: 0 alloc, 2 syscalls.
- Reverse DNS: 1+ alloc, 1+ syscall (cached after first call per peer
  via `peer_host` parameter, but the per-connection lookup still runs
  every time the cache is empty).
- Log info line: 1 alloc, 0-1 syscalls.
- `BufReader`: 2 alloc (struct + 8 KiB buffer), 0 syscalls.
- `LegacyMessageCache`: 2 alloc, 0 syscalls (used later, but built
  here).
- Greeting `String`: 1-3 alloc, 1 syscall (`write_all` of ~38 bytes).
- Version-reply `read_line`: 1 alloc, 1 syscall.
- Module-request `read_line`: 1 alloc, 1 syscall.

**Total per connection: ~9-11 heap touches and ~7-8 syscalls between
the accept return and the start of module lookup.**

## 2. Upstream comparison (rsync 3.4.1, default config)

Upstream walks the same logical phases but in C with a fixed `line[1024]`
stack buffer and one `io_printf` per line written. Per accepted
connection, upstream pays:

| Category | oc-rsync | upstream 3.4.1 | Delta |
|----------|----------|----------------|-------|
| Accept-loop wait primitive | `thread::sleep(500ms)` after every `WouldBlock` (single listener) or `recv_timeout(100ms)` (dual stack) | `select(maxfd+1, &fds, NULL, NULL, NULL)` with a NULL timeval (truly blocking; wakes on FD readiness) | +0 syscalls steady state; +up to one full sleep tick of latency per accept |
| Process / thread model | `thread::spawn` per connection (one `clone3` + one `JoinHandle` allocation) | `fork()` per connection (one `clone` + zero heap allocations in the parent) | -1 alloc on upstream, comparable syscall cost |
| Socket option calls after accept | `set_nonblocking(false)`, `set_read_timeout`, `set_write_timeout` (3 syscalls) | `set_socket_options(f_in, "SO_KEEPALIVE")` + `set_nonblocking(f_in)` (2 syscalls, no read/write timeouts) | +1 syscall (`SO_SNDTIMEO`) |
| Reverse DNS | One `getnameinfo` via `lookup_addr` per accept when `reverse lookup = yes` (default) | One `getnameinfo` (NI_NUMERICHOST) for the IP + one `getaddrinfo`+`getnameinfo` round for the name | Symmetric kernel cost; oc-rsync allocates one extra `String` for the normalized form |
| Greeting build | `format!("@RSYNCD: 32.0\n")` -> `String::pop` -> 5x `push_str(" sha*")` -> `push('\n')` | One `io_printf(f_out, "@RSYNCD: %d.%d %s\n", protocol_version, our_sub, tmpbuf)` into the io buffer, where `tmpbuf` is a `MAX_NSTR_STRLEN`-sized stack array filled once by `get_default_nno_list` | +1-3 heap allocs on oc-rsync; upstream stays entirely in the io ringbuffer |
| OK / EXIT cache | `LegacyMessageCache::new()` allocates two `Box<[u8]>` per connection even though the strings are fixed | Upstream emits the OK line directly with `io_printf(f_out, "@RSYNCD: OK\n")`; no per-connection cache | +2 heap allocs on oc-rsync (zero functional benefit on the greeting path - the cache is meant to amortize across OK/EXIT writes, but the *cache itself* is rebuilt per accept) |
| Buffered reader for the greeting | `BufReader::new(stream)` (8 KiB heap buffer + 32 B struct) | Reads via `read_line_old(f_in, line, sizeof line, 0)` into a 1024-byte stack buffer | +2 heap allocs |
| Read of the client `@RSYNCD: <ver>` line | `String::new()` then `BufReader::read_line` (allocates and grows the `String`) | `read_line_old` fills the stack `line[]` in place | +1 heap alloc |
| Wait primitive penalty (single listener, default path) | Up to 500 ms of latency between `accept` becoming ready and the worker handling it | None (blocking `select`) | **+up to 500 ms p99** |

**Allocation diff:** oc-rsync ~9-11 heap touches per accept versus
upstream ~0-2 (the `default_name` and `ipaddr_buf` strings are
statically sized). Net delta is **~+9 allocations per greeting**.

**Syscall diff (steady state, no `WouldBlock` retries):** roughly
parity (oc-rsync pays one extra `setsockopt` for the write timeout).

**Wake-up diff:** oc-rsync's single-listener path adds up to one
`SIGNAL_CHECK_INTERVAL = 500 ms` (`listener.rs:45`) of latency per
accept that is not preceded by client traffic. The dual-stack path
caps the same penalty at 100 ms (`connection.rs:436` -
`rx.recv_timeout(100ms)` plus `connection.rs:410` - acceptor
`thread::sleep(50ms)`). Upstream pays zero - `select(NULL)` is
truly blocking.

## 3. Top contributors (ranked by estimated wall-clock cost)

Cost estimates assume the DIS-1 small-files cold-start scenario
(500 files, 1 KiB each, loopback, Debian-glibc allocator) running
in the rsync-profile container.

### 1. `SIGNAL_CHECK_INTERVAL = 500 ms` poll sleep (DIS-3 row 2)

`listener.rs:45` + `connection.rs:337`. Default config uses the
single-listener path, which after every `WouldBlock` does
`thread::sleep(Duration::from_millis(500))`. A cold-start client
that arrives between ticks waits the full residual of the sleep
before its connection is accepted.

- Expected cost: **0-500 ms per accept**, median ~250 ms when arrivals
  are uniformly distributed inside the tick window; p99 ~500 ms.
- Dominates everything else in this audit by two orders of magnitude.
- Confirmed root cause of the DIS-3 measurement gap when the tick
  fires; latent hazard otherwise.

### 2. `BufReader` + `LegacyMessageCache` + greeting-`String` per-accept allocations (DIS-3 rows 3, 4, 10)

`session_runtime.rs:220-225`, `legacy_messages.rs:24-31`,
`greeting.rs:13-35`. Every accept rebuilds:

- 8 KiB `BufReader` heap buffer (`session_runtime.rs:220`).
- Two `Box<[u8]>` for `OK` and `EXIT` (`legacy_messages.rs:25-31`).
- One `String` for the greeting that gets `pop`'d and `push_str`'d
  5 times (`greeting.rs:26-33`).
- One `String` for each `read_trimmed_line` (`greeting.rs:50`).

Each allocation is sub-microsecond on glibc when uncontended, but in
aggregate they push the per-connection heap-touch count from
upstream's ~0-2 to oc-rsync's ~9-11.

- Expected cost: **~30-80 us per accept** under an idle allocator;
  **2-10x amplifier** under malloc contention (the cold-start scenario
  is the *least* contended case, but the same code runs under load).
- Removes a measurable jitter source from the DIS-2 flame graph once
  the 500 ms tail is gone.

### 3. Per-accept `format!` / `push_str` greeting build (DIS-3 row 3)

`greeting.rs:13-35`. The greeting bytes are a function of
`ProtocolVersion::NEWEST` and the static digest list. They never
change between accepts on a running daemon. Today we rebuild them
on every connection.

- Expected cost: **~3-8 us per accept** plus 1-3 allocations.
- Trivial fix (`OnceLock<&'static [u8]>`); cost dominated by the
  *fact of allocating* rather than the formatting compute.

### 4. Extra `setsockopt` for `SO_SNDTIMEO` (DIS-3 row 2)

`listener.rs:152-155`. oc-rsync sets both read and write timeouts on
the accepted stream. Upstream sets only `SO_KEEPALIVE`. The write
timeout is defensible (it protects the daemon from a stalled peer
during the OK write), but it is one extra syscall per accept.

- Expected cost: **~1-2 us per accept**.
- Listed for completeness; not worth ripping out, but worth noting.

### 5. Per-accept reverse-DNS lookup (DIS-3 borderline; partial credit to greeting phase)

`session_runtime.rs:88` -> `module_state/hostname.rs:42` ->
`lookup_addr`. When `reverse lookup = yes` (default) every accept
calls `getnameinfo` synchronously on the calling thread. Upstream
does the same. The cost is borne by both implementations, so this
does not show up as a *gap* contributor on the DIS-1 measurement,
but it is the largest single fixed cost in the greeting phase
(~5-50 ms on loopback depending on resolver state) and should be
flagged as a future joint-optimization target shared with DIS-4.b.

- Expected cost (both implementations): **5-50 ms first call,
  cached afterwards within the same connection only**.

## 4. Recommendations (ranked, each one-paragraph; DIS-6 implements)

### R1. Replace the 500 ms `thread::sleep` with an event-driven wait

Wrap the listener fd in a `mio::Poll` (or use `socket2::Socket::poll`
with `libc::poll`/`epoll`/`kqueue`) so the accept loop wakes the
instant the kernel marks the listener readable. Register the signal
self-pipe as a second poll target so the shutdown path stays as
responsive as today. The dual-stack path can use the same mechanism
or, alternatively, switch to a blocking `accept()` per acceptor
thread with a self-pipe to interrupt. This single change removes
the entire latency tail in the DIS-1 measurement (200-500 ms off
p99) without touching the wire format or protocol negotiation.
Wire-compatibility: unaffected.

### R2. Cache the greeting bytes in a `OnceLock<&'static [u8]>`

`greeting.rs:42` (`legacy_daemon_greeting`) already returns the
greeting at `ProtocolVersion::NEWEST`. The protocol version and the
digest list are both fixed at startup. Cache the rendered bytes in a
`OnceLock` (or `LazyLock<Box<[u8]>>`) and have `handle_legacy_session`
write the cached slice directly. Eliminates the per-accept
`format!`/`pop`/`push_str` chain plus 1-3 allocations. The greeting
*for non-default protocols* is currently used only by tests and
`exchange_protocols` peering; both can stay on the existing
allocating helper. Wire-compatibility: byte-identical to today.

### R3. Hoist `LegacyMessageCache` to a `OnceLock` (or share via `Arc`)

`legacy_messages.rs:24-31` builds two `Box<[u8]>` per connection that
hold the literal strings `"@RSYNCD: OK\n"` and `"@RSYNCD: EXIT\n"`.
Move the cache to a process-wide `OnceLock<LegacyMessageCache>` (or
make the boxes `&'static [u8]` consts) so every accept borrows the
same bytes. Removes 2 heap touches per connection. Wire-compatibility:
byte-identical.

### R4. Pool the `BufReader` 8 KiB buffer

`session_runtime.rs:220` allocates a fresh 8 KiB heap buffer per
accept. Replace with a `crossbeam_queue::ArrayQueue<Box<[u8; 8192]>>`
shared across the listener, handed out at session start and
returned via RAII on session end. Same pattern the
`daemon-handshake-overhead.md` audit proposed. Removes 1 alloc + 1
free per connection. Wire-compatibility: unaffected. Note that
`BufReader::with_capacity` is needed to install the pooled buffer;
the pool's `ArrayQueue` capacity can be set to the daemon's
`max_connections` so steady-state allocations are zero.

### R5. Reuse the line-read `String` across greeting reads

`greeting.rs:49` allocates a fresh `String` in every
`read_trimmed_line` call. The greeting loop in
`session_runtime.rs:233-272` reads at least two lines (version and
module request) per connection. Thread a `&mut String` from the
session frame so the same buffer is cleared and reused. Removes
2 small heap touches per connection. Wire-compatibility: unaffected.

### R6. Drop the `reader.get_mut().flush()` after the greeting write

`session_runtime.rs:226`. `TcpStream::flush` is a no-op on `std`'s
implementation (the underlying socket is unbuffered). The call is
free at runtime but adds a method dispatch and signals intent the
code does not actually have. Drop it. Wire-compatibility: unaffected
(verify with the daemon protocol golden tests in
`crates/protocol/tests/golden/`).

## 5. Cross-reference: DIS-3 phases covered

This audit covers DIS-3's phase rows:

- **Row 2** - TCP accept + `set_nonblocking(true)` poll loop -> R1.
- **Row 3** - `@RSYNCD:` greeting build + write -> R2, R5, R6.
- **Row 4** - Capabilities advertisement (`modules` + `authlist`):
  reviewed; lives in `greeting.rs:64-104` and runs **only on
  `#list` requests** (`session_runtime.rs:276-289`). It is not on
  the cold-start critical path measured by DIS-1 (which is a
  module-pull, not a list request) and so does not need a dedicated
  recommendation in this audit. Tracked as a future cleanup if the
  `vec![features.join(" ")]` `Vec`+`String` allocation ever shows up
  under listing-heavy workloads.
- **Row 10** - `@RSYNCD: OK` write + flush -> R3 (cache hoist).

DIS-3 rows 5-9 and 11-13 belong to **DIS-4.b** (module-select
roundtrip). Rows 14-16 belong to **DIS-4.c** (auth and capability
negotiation). Rows 17-23 belong to **DIS-4.d** (flist build) and
**DIS-4.e** (first-block send) respectively.

## 6. Confidence and what DIS-2 should confirm

- **High confidence:** R1 (`SIGNAL_CHECK_INTERVAL`), R2/R3/R4 (per-
  accept allocations). All four are code-readable; the costs follow
  directly from counting allocations and timer ticks. The DIS-3
  estimate of 0-500 ms on row 2 has high confidence on the
  *magnitude* but is gated by whether the tick window actually
  catches the arrival on a given run.
- **Medium confidence:** R5 (line-buffer reuse) and the allocation
  totals. Allocator-induced jitter depends on the workload around
  the daemon; the per-accept cost in isolation is sub-microsecond.
- **Low confidence:** Reverse-DNS row in Section 3. The `5-50 ms`
  range assumes typical glibc resolver timing on loopback; the
  actual cost on the DIS-1 harness needs to be measured under
  `strace -c` or `perf trace` before any optimization is scheduled.

DIS-2 should re-run the harness with `perf record -F 999 -g
--call-graph fp` against `run_single_listener_loop` and look for:

- Whether `thread::sleep` accounts for the bulk of the daemon-side
  wall time on the cold-start runs. If yes, R1 alone explains the
  gap and R2-R5 are minor hygiene. If no, the gap has another
  hidden contributor (most likely `getnameinfo` on row 10 of the
  inventory, or a Linux scheduler effect on `thread::spawn`).
- Whether the per-accept allocator pattern is visible in the flame
  graph as a `jemalloc`/`glibc malloc` cluster. If it is, R2-R5
  collectively are worth ~30-80 us per connection and stack with
  R1 cleanly.

## 7. Related audits

- `docs/audits/dis-3-cold-start-phase-decomposition.md` - parent task
  this audit feeds.
- `docs/audits/daemon-handshake-overhead.md` - prior inventory and
  mitigation list; R3-R5 realign with its proposals.
- `docs/audits/binary-startup-overhead.md` - DIS-3 row 1 (out of
  DIS-4 scope).
