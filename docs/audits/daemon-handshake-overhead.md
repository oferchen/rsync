# Daemon Handshake Overhead Profile

Tracking: oc-rsync task #1038.

This audit catalogues the per-connection cost of the rsync daemon
handshake, flags suspected hot spots, and proposes a profile plan plus
mitigation candidates. Scope is the pre-transfer walk only - TCP
accept through `@RSYNCD: OK` and server arg dispatch. Bulk transfer
cost is out of scope.

Last verified against
`crates/daemon/src/daemon/sections/greeting.rs`,
`crates/daemon/src/daemon/sections/legacy_messages.rs`,
`crates/daemon/src/daemon/sections/module_access/request.rs`,
`crates/daemon/src/daemon/sections/session_runtime.rs`,
`crates/daemon/src/daemon/async_session/session.rs`, and
`crates/daemon/src/auth.rs`.

## 1. Handshake phases

The daemon walks each connection through a fixed sequence; file and
line references are anchors for instrumentation:

1. **TCP accept** -
   `crates/daemon/src/daemon/sections/server_runtime/accept_loop.rs`
   spawns a thread per connection. The async path in
   `crates/daemon/src/daemon/async_session/listener.rs` dispatches via
   `tokio::task::spawn`.
2. **`@RSYNCD:` greeting** - `legacy_daemon_greeting_for_protocol`
   (`greeting.rs:13`) builds the version line and appends the
   protocol-aware digest list. The async path uses an inline
   `format!("@RSYNCD: {}.0\n", 32)` at `async_session/session.rs:138`.
3. **Module select** - `read_trimmed_line` (`greeting.rs:49`) parses
   the client request, then `request.rs:246` resolves it via
   `modules.iter().find(|module| module.name == request)`.
4. **Auth** - `crates/daemon/src/auth.rs:148`
   (`generate_authentication_challenge`) emits a 22-byte base64 nonce;
   the response is verified via the negotiated digest.
5. **`@RSYNCD: OK`** - served via `LegacyMessageCache::write_ok`
   (`legacy_messages.rs:52`); the cache holds a pre-rendered
   `Box<[u8]>`.
6. **Server arg dispatch** - `module_access/client_args.rs` reads the
   newline-delimited argv until a blank line.
7. **Protocol setup** - `session_runtime.rs:220` constructs a
   per-connection `BufReader` over the `TcpStream` and hands control
   to the transfer engine.

## 2. Suspected costs

Inspection (no measurement yet) flags four candidate hot spots:

- **Per-connection `BufReader::new`** - the sync path
  (`session_runtime.rs:220`, `proxy_protocol.rs:216`) allocates a
  fresh 8 KiB heap buffer on every accept. The async path repeats the
  pattern with `BufReader::new(reader)` and `BufWriter::new(writer)`
  at `async_session/session.rs:132-133`.
- **`format!()` allocations in greeting** - the sync greeting builder
  (`greeting.rs:13`) allocates a `String`, calls `.pop()` and
  `.push_str()`, and rebuilds the digest list per connection even
  though `(version, digests)` is fixed at startup. The async path
  reformats `@RSYNCD: 32.0\n` per call (`async_session/session.rs:138`).
- **Module config scan** - lookup is O(n) over `Vec<ModuleRuntime>`
  (`request.rs:246`); host-allow/deny rules iterate
  `Vec<HostPattern>` per connection
  (`module_state/definition.rs:162-186`).
- **Auth construction** - while no `Regex::new` exists today
  (`rg "Regex::new"` returns zero matches under `crates/daemon`), the
  challenge path still re-derives digest selection and base64-encodes
  18 random bytes per session; if any future host-pattern logic
  adopts regex it must be `OnceLock`-cached from the start.

## 3. Profile plan

- **Criterion microbench (`crates/daemon/benches/handshake.rs`,
  new):** one `criterion_group` per phase above, each driving a
  loopback `TcpStream` pair. Phases 1, 2, 5 measure raw allocation
  and write latency; phase 3 sweeps `n_modules in {1, 16, 256, 4096}`;
  phase 7 measures `BufReader` setup in isolation. Report median and
  p99 per phase.
- **End-to-end cold-start trace:** drive 1000 sequential connections
  through `oc-rsyncd --no-detach` with `perf record -F 999 -g` on
  Linux and `dtrace -n 'profile-997 { @[ustack()] = count(); }'` on
  macOS. Aggregate flame graphs with `inferno-flamegraph`.
- **Allocation accounting:** wrap a single handshake in `dhat-rs`
  (heap profiler) to enumerate allocations per phase; budget target
  is < 8 allocations per connection.
- **Wall-clock distribution:** `hyperfine --warmup 50 --runs 1000
  'oc-rsync rsync://127.0.0.1:1873/'` to capture client-observed
  handshake latency.

## 4. Comparison target

Upstream `rsyncd` 3.4.1 on the same loopback fixture is the baseline.
Empirical numbers will be filled in by the profile run; the public
goal is `oc-rsyncd` p99 within 10% of upstream and median <= upstream.
The `scripts/rsync-interop-server.sh` harness already starts upstream
3.0.9 / 3.1.3 / 3.4.1 in the `rsync-profile` container, so the same
hyperfine command can target each port for a fair head-to-head.

## 5. Mitigation candidates

Ordered by expected impact, all wire-compatible with upstream:

1. **Precompute greeting bytes via `OnceLock`** - cache
   `legacy_daemon_greeting_for_protocol(NEWEST)` once at startup; the
   sync path returns `&'static [u8]`, eliminating per-connection
   `String`/`format!()` work. Apply the same to the async greeting at
   `async_session/session.rs:138`.
2. **Slimmer greeting writer** - issue one `write_all` of the cached
   bytes directly to the `TcpStream`, removing the intermediate
   `LegacyMessage::Owned(format_legacy_daemon_message(...))` branch
   in `LegacyMessageCache::render`.
3. **Pooled `BufReader`** - hand out reusable 8 KiB buffers from a
   `crossbeam-queue::ArrayQueue<Box<[u8; 8192]>>` shared across
   accepts, reclaiming on session drop. Saves one heap alloc and one
   free per connection.
4. **`OnceLock`-backed regex/pattern caches** - if host-allow/deny
   gains glob-or-CIDR compilation, store the compiled form on
   `ModuleRuntime` at config-load time so per-connection matching is
   pure pointer-walks.
5. **Module index** - for daemons with dozens of modules, replace the
   `Vec` linear scan with a `HashMap<&str, &ModuleRuntime>` built once
   in `ServerRuntime`; module names are static after config parse.
6. **Reuse `String` line buffers** - `read_trimmed_line` allocates a
   fresh `String` per call; pass `&mut String` from the session frame
   so the buffer survives across greeting, module select, and auth
   reads.

Each mitigation is independently mergeable. Run the profile plan
before and after each change to confirm the win.
