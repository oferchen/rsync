# Daemon Handshake Overhead Audit

Tracking: oc-rsync task #1038.

> Static analysis. No code lands in this PR. Empirical timing is a
> follow-up.

## 1. Summary

The oc-rsync daemon walks every accepted connection through the legacy
`@RSYNCD:` text protocol before the transfer engine takes over. The
walk is wire-compatible with upstream 3.4.1, but several per-phase
costs are paid eagerly that upstream skips, defers, or amortises. The
biggest single offender is the synchronous reverse DNS lookup invoked
on the accept thread when `reverse lookup = yes` (default); secondary
costs sit in the `String`-based line reader and the per-connection
greeting allocation. Five prioritised improvements are listed in
section 6.

Last verified: 2026-05-07 against
`crates/daemon/src/daemon/sections/server_runtime/{accept_loop,connection,listener}.rs`,
`crates/daemon/src/daemon/sections/{session_runtime,greeting,legacy_messages,module_parsing}.rs`,
`crates/daemon/src/daemon/sections/module_access/{request,authentication,client_args,transfer}.rs`,
`crates/daemon/src/daemon/module_state/hostname.rs`, and
`target/interop/upstream-src/rsync-3.4.1/clientserver.c`.

## 2. Handshake State Machine

The daemon does not encode the handshake as an explicit `enum` state
machine. Phases are sequenced by control flow inside three functions
linked by a single `BufReader<TcpStream>`:

1. `serve_connections()` -
   `crates/daemon/src/daemon/sections/server_runtime/accept_loop.rs:11`.
   Sets up signal handlers, listeners, log sink, syslog, PID file,
   privilege drop, and enters the accept loop.
2. `handle_session()` -
   `crates/daemon/src/daemon/sections/session_runtime.rs:44`. Per-thread
   entry. Hard-codes `SessionStyle::Legacy`, applies socket timeouts via
   `configure_stream()`, optionally reads PROXY protocol, optionally
   resolves peer hostname, and dispatches to the legacy handler.
3. `handle_legacy_session()` -
   `crates/daemon/src/daemon/sections/session_runtime.rs:206`. Sends the
   `@RSYNCD:` greeting, drains client lines until a module name (or
   `#list`) is reached, then dispatches to
   `respond_with_module_request()` -
   `crates/daemon/src/daemon/sections/module_access/request.rs:232`.
4. `process_approved_module()` -
   `crates/daemon/src/daemon/sections/module_access/transfer.rs:204`.
   Acquires a connection slot, evaluates daemon params, applies module
   timeout, runs auth, runs early/pre-xfer exec, reads client args, sets
   up streams, then hands off to `run_server_with_handshake()`.

Implicit phases, in observable wire order:

| # | Phase | Owner |
|---|-------|-------|
| A | Accept + per-conn config | `accept_loop.rs:230`, `listener.rs:113` |
| B | PROXY header (optional) | `session_runtime.rs:69` |
| C | Reverse DNS (optional) | `session_runtime.rs:87` |
| D | Greeting send | `session_runtime.rs:225`, `greeting.rs:13` |
| E | Version + option line drain | `session_runtime.rs:233` |
| F | Module name / `#list` / early-input | `session_runtime.rs:265` |
| G | Module lookup + access check | `module_access/request.rs:246` |
| H | Connection slot claim | `module_access/transfer.rs:210` |
| I | Per-module timeout install | `module_access/transfer.rs:253` |
| J | Auth challenge + verify | `module_access/authentication.rs:33` |
| K | Early/pre-xfer exec | `module_access/transfer.rs:263, 456` |
| L | Client args (with secluded fallback) | `module_access/client_args.rs:116` |
| M | Server config build + filter rules | `module_access/client_args.rs:216` |
| N | Stream split, NODELAY | `module_access/transfer.rs:48` |
| O | Hand-off to transfer engine | `module_access/transfer.rs:132` |

Phase E doubles as the implicit "env vars" channel: refused options
arrive interleaved with the version line and accumulate into
`refused_options: Vec<String>`. There is no `RSYNC_*` env exchange on
the wire; upstream's `set_env_str("RSYNC_HOST_NAME", ...)` populates
the daemon process's env for downstream `pre-xfer exec`, and oc-rsync
mirrors that via `xfer_exec.rs::run_pre_xfer_exec()`.

## 3. Per-Phase Cost Reading

Costs identified by reading the code, in approximate descending order
of magnitude on a fast-LAN connection.

### 3.1 Phase C - Reverse DNS

`resolve_peer_hostname()` -
`crates/daemon/src/daemon/module_state/hostname.rs:42` - calls
`dns_lookup::lookup_addr()` synchronously on the worker. The call
blocks until the system resolver answers; on a misconfigured DNS host
or a peer with no PTR record this waits seconds (glibc defaults to
~5 s per server with two retries). The worker is scheduled before the
greeting is sent, so the client sees the resolver delay.

Hostname is cached for the connection (`hostname_cache` -
`request.rs:267`), but the cache is per-connection and rebuilt on
every accept. The session-level lookup at `session_runtime.rs:87`
fires regardless of whether any module consults it; the module-level
lookup at `request.rs:269` already matches upstream's deferred path.

### 3.2 Phase D - Greeting Allocation

`legacy_daemon_greeting()` builds a fresh `String` per connection
(`greeting.rs:13`). The protocol/digest portion is identical for every
client at `ProtocolVersion::NEWEST`; the work duplicates across every
accept. `write_limited()` (`session_runtime.rs:176`) collapses to a
single `write_all()` when `daemon_limit` is `None`.

### 3.3 Phase E/F - Line Reader Loop

`read_trimmed_line()` - `greeting.rs:49` - allocates a fresh `String`
per line via `read_line()`, then pops `\r`/`\n` one byte at a time.
Version exchange sends 1 line; option-refusal pre-roll several;
early-input adds one more. Each line is a separate heap allocation;
the `BufReader`'s internal buffer reuse is wasted because the result
is collected into a fresh `String`.

### 3.4 Phase A - Accept-Loop Wake Pattern

`run_single_listener_loop()` polls non-blocking with
`thread::sleep(SIGNAL_CHECK_INTERVAL)` (500 ms) -
`listener.rs:45`, `accept_loop.rs:251-254`. With no pending client
this introduces up to 500 ms wake latency on the first connection
after an idle period. Dual-stack uses `recv_timeout(100 ms)` -
`accept_loop.rs:342` - so dual-stack is strictly faster on idle.

Per-accept syscalls also include two `try_clone()` (`dup(2)`) calls
in `setup_transfer_streams()` -
`module_access/transfer.rs:54, 63` - plus `set_nodelay(true)` and the
read/write timeout pair. Upstream forks once and inherits the
descriptor unchanged.

### 3.5 Phase G/H - Module Lookup + Slot Claim

`modules.iter().find(|m| m.name == request)` -
`request.rs:246` - is a linear scan; negligible for small tables.
`try_acquire_connection()` takes the per-module mutex and (when
`lock file` is configured) opens, byte-locks, increments, and
unlocks - same cost upstream pays.

### 3.6 Phase I - Module Timeout Install

`apply_module_timeout()` - `module_parsing.rs:125` - calls
`set_read_timeout` and `set_write_timeout`, each one
`setsockopt(SO_RCVTIMEO/SO_SNDTIMEO)`. These overwrite the 10 s
`SOCKET_TIMEOUT` installed by `configure_stream()` -
`listener.rs:113`. The defensive timeout is redundant when a
module-level `timeout =` is set, costing two extra setsockopt calls
per session.

### 3.7 Phase J - Auth

`generate_auth_challenge()` (`authentication.rs:103`) hashes a 32-byte
input with MD5 (MD4 for protocol < 30); one hash per protected
connection. `verify_secret_response()` reads the secrets file every
auth (`fs::read_to_string(secrets_path)` -
`authentication.rs:171`) and scans line-by-line. Upstream's
`authenticate.c:check_secret()` does the same; secrets files rarely
change and could be cached with a stat-based freshness check.

### 3.8 Phase L - Client Args

`read_client_arguments()` (`client_args.rs:21`) reads NUL- or
LF-terminated args via `read_until` into a fresh `Vec<u8>` per arg.
`apply_long_form_args()` (`client_args.rs:316`) is a sequential
`match`; trivial. `has_secluded_args_flag()` (`client_args.rs:76`)
scans every arg even when none start with `-`; cost is negligible.

## 4. Comparison with Upstream

References point into `target/interop/upstream-src/rsync-3.4.1/clientserver.c`
unless noted.

| Phase | Upstream | oc-rsync |
|-------|----------|----------|
| Accept | `start_accept_loop()` -> `fork()` -> `start_daemon()` (line 1536). Child inherits stdin/stdout as the socket. | `thread::spawn` per accept; stream cloned twice. |
| Privilege drop | After bind, before accept loop, in parent (lines 1313-1339). | Identical, in `accept_loop.rs:233`. |
| PROXY protocol | `read_proxy_protocol_header(f_in)` (line 1298). | `parse_proxy_header()` - `session_runtime.rs:69`. |
| Reverse DNS | `client_name(addr)` only when `lp_reverse_lookup(-1)` (line 1342); module-level lookup is deferred to `rsync_module()` (line 721). | Session-level lookup at `session_runtime.rs:87` whenever `reverse_lookup=true`; module-level lookup deferred to `request.rs:269`. **Note**: oc-rsync triggers DNS earlier than necessary. |
| Greeting | `io_printf(f_out, "@RSYNCD: %d.%d %s\n", ...)` via `exchange_protocols()` (line 178+). | `legacy_daemon_greeting()` returns a fresh `String` (`greeting.rs:13`). |
| Version exchange | `read_line_old(f_in, line, sizeof line, 0)` reuses a stack buffer (line 1354). | `read_trimmed_line()` allocates a `String` per line (`greeting.rs:49`). |
| Module select | `read_line_old` then `lp_number(line)` lookup (line 1383). | Linear scan via `iter().find()` (`request.rs:246`). |
| Connection slot | `claim_connection(lp_lock_file(i), lp_max_connections(i))` (line 744). | `module.try_acquire_connection()` (`transfer.rs:210`). |
| Auth challenge | `gen_challenge()` builds 32-byte input, MD5/MD4 hashes, base64 (authenticate.c:61). | `generate_auth_challenge()` mirrors exactly (`authentication.rs:103`). |
| Auth verify | `check_secret()` reads + scans secrets file (authenticate.c:100). | `verify_secret_response()` reads + scans secrets file (`authentication.rs:155`). |
| Args | `read_args()` with `rl_nulls` flag (line 1059). Two-phase when `protect_args` (lines 1066-1071). | `read_client_arguments()` mirrors NUL/LF; `recv_secluded_args` for phase 2 (`client_args.rs:21, 138`). |
| Env vars | `set_env_str("RSYNC_*", ...)` populated in-process (lines 723, 765, 866). | Populated lazily inside exec contexts (`xfer_exec.rs`); no wire equivalent on either side. |
| Filter receive | Daemon module filters parsed from config (lines 874-892); client filters arrive over the multiplex stream after handshake. | `build_daemon_filter_rules(module)` (`transfer.rs:417`); same model. |

The main divergences:

1. Upstream defers the worker reverse-DNS to the point where it is
   actually consulted; oc-rsync runs it before the greeting unless
   `reverse lookup = no` is set globally.
2. Upstream uses a stack-allocated `char line[1024]` reused across the
   line reader; oc-rsync allocates a `String` per line.
3. Upstream's accept blocks in `select()` (`socket.c:start_accept_loop`)
   so connection latency on idle daemons is bounded by kernel notify;
   oc-rsync polls with a 500 ms sleep on the single-listener path.

## 5. Likely Hotspots

In a fast-LAN micro-benchmark the wall-clock floor for a single
`rsync rsync://host/mod` with no auth and `reverse lookup = no` is
dominated by the four packet round-trips (greeting, version+module
name, OK, args). All of section 3 is below that floor today. Hotspots
that show up under load (many short-lived connections), or under
adverse network conditions:

| Hotspot | Trigger | Magnitude |
|---------|---------|-----------|
| Reverse DNS blocking (3.1) | Default-on `reverse lookup`; slow resolver | Up to ~5 s per accept on bad DNS |
| Single-listener wake (3.4) | Idle daemon, single bind family | Up to 500 ms first-packet latency |
| Per-line `String` alloc (3.3) | High-rate connect storms | ~3 allocs per accept |
| Two `dup(2)` for stream split (3.4) | Every accept | 2 syscalls per accept |
| Redundant timeout setsockopt (3.6) | Module with explicit `timeout =` | 2 setsockopt per accept |
| Secrets file re-read (3.7) | Every authenticated connection | 1 disk read per accept |
| Greeting `String` rebuild (3.2) | Every accept | 1 alloc + format per accept |

## 6. Proposed Improvements

Five prioritised items. Costs and risk levels are static estimates;
ordering is by `impact / effort`.

### P1 - Defer reverse DNS to first consumer

**Change**: drop the unconditional `resolve_peer_hostname` call at
`session_runtime.rs:87`. Move the lookup behind a lazy
`OnceCell<Option<String>>` populated on first read by
`log_connection`, the module-level access check, or
`module_peer_hostname()`. Modules whose `hosts allow`/`hosts deny`
rules require DNS already trigger the lookup at `request.rs:269`.

**Impact**: removes the synchronous resolver call from the
greeting-send path entirely when no consumer needs it. Eliminates the
worst-case multi-second handshake stall on misconfigured DNS.

**Risk**: low. The current `peer_host` value is consumed only by log
sinks and module access; both already have optional handling.

**Upstream parity**: matches `clientserver.c:1342` semantics when
`reverse lookup = no` is set globally and the module needs DNS - which
is the expensive path upstream defers identically.

### P2 - Cache the greeting bytes in a `OnceLock`

**Change**: store
`static GREETING: OnceLock<Box<[u8]>>` populated on first call inside
`legacy_daemon_greeting()`. The greeting is constant for the daemon's
lifetime at `ProtocolVersion::NEWEST`. Existing per-protocol callers
(used by the binary path and tests) keep the dynamic builder.

**Impact**: removes the per-connection allocation and `format!` on
the greeting send. Single-digit microseconds, but it is on every
accept's critical path.

**Risk**: trivial. `LegacyMessageCache` already caches `OK` and `EXIT`
this way (`legacy_messages.rs:25`); extending the pattern is direct.

### P3 - Inline-buffer the line reader

**Change**: introduce a small `Vec<u8>` owned by the session and
reused across `read_until(b'\n', ...)` calls. Provide a
`read_trimmed_line_into(&mut buf, reader)` that returns a `&str`
slice for parsing, leaving allocation to the caller. The greeting
phase reads one or two lines per session; reusing the buffer collapses
those allocations to zero.

**Impact**: removes 3-5 small allocations per accept on the common
path, more for clients that send refused-options pre-roll. Exact
savings depend on the allocator; modest.

**Risk**: low. The `String` allocation is internal and not exposed
across the daemon boundary.

### P4 - Skip `try_clone` for stream split

**Change**: replace the two `try_clone()` calls in
`setup_transfer_streams()` (`module_access/transfer.rs:48`) with a
single `try_clone()` and pass `(&stream, &cloned)` into
`run_server_with_handshake`. The transfer engine reads on one and
writes on the other; one half can be the original. Saves one `dup(2)`
per session.

**Impact**: 1 syscall per accept. Matters under connection storms
(thousands of short-lived sessions per second).

**Risk**: low. The engine already accepts `&mut TcpStream` for both
ends; passing one as `&mut stream` and the other as
`&mut cloned` is a refactor, not a model change.

### P5 - Cache secrets file with mtime check

**Change**: introduce a process-wide
`Mutex<HashMap<PathBuf, (SystemTime, Vec<(String, String)>)>>` keyed
on the secrets-file path. On each auth, `stat` the file once; if mtime
matches the cached entry, reuse the parsed `(user, secret)` pairs;
otherwise re-read. Permission/ownership checks
(`platform::secrets::check_secrets_file_permissions`) still run on
every auth so `strict modes = yes` semantics are preserved.

**Impact**: eliminates one disk read per authenticated connection.
Significant when the secrets file lives on a slow filesystem (NFS,
SMB) or when many short-lived authenticated transfers run back-to-back
(e.g. CI fanout).

**Risk**: medium. Cache invalidation must be airtight; an admin who
edits the secrets file with `cp -f` may not bump mtime if the source
is on a filesystem with second-granularity timestamps. The
conservative fallback is to also invalidate on `inode` change, which
covers `mv`-replace edits.

## 7. Out of Scope

- Switching the legacy `@RSYNCD:` walk to an event-driven async model.
  That is the subject of the daemon-thread-per-connection audit
  (`docs/audits/daemon-thread-per-connection-scalability.md`) and the
  RFC under task #1934. The current task #1038 only touches the
  per-session cost map.
- Protocol additions. Section 6 is wire-compatible by construction; no
  changes alter the bytes the daemon emits.
- Empirical timing. The per-phase numbers above are read off the
  source. A follow-up benchmark can attach `tracing` spans (the
  daemon already has `instrument` annotations on `handle_session` and
  `handle_legacy_session`) and time each phase against upstream
  3.4.1 for the matrix `{auth, no-auth} x {reverse-DNS, no-DNS}`.
