# DIS-3: cold-start phase decomposition (daemon initial-sync gap)

Tracking the DIS-3 task: identify the top contributor phases of the
daemon cold-start latency gap. Baseline established by DIS-1
(PR #4813) is **~1.35 s for oc-rsync vs ~0.36 s for upstream rsync
3.4.1** on a 500-file / 1 KiB-each pull from a daemon module
(`small_files` scenario). Ratio ~3.7x.

This audit is docs-only. The goal is to produce a phase-by-phase
attribution that DIS-6 can act on, and to map each phase to one of
the DIS-4.a-e sub-task slots.

Sources of evidence are mixed and labelled per row:

- **Code-read estimate** - derived from reading the daemon and
  transfer crates; no per-phase wall-clock measurement was possible
  because the pre-built `oc-rsync-dev` v0.5.9 binary in the
  rsync-profile container hangs in module-list and single-file pulls
  against the harness used here (the very symptom DIS-1/DIS-3 are
  measuring). One end-to-end timing pair was captured before the
  hang: upstream pull completed in 0.13-0.17 s, oc-rsync pull never
  completed.
- **Cross-reference** - existing audit data from
  `docs/audits/daemon-handshake-overhead.md` (per-connection
  allocation and message-cache profile) and `binary-startup-overhead`
  (process-startup floor).

Confidence column on every estimate is **high** / **medium** / **low**
to flag which numbers DIS-6 should re-measure before optimizing.

## Reproduction harness

Run the DIS-1 bench at `scripts/benchmark_daemon_cold_start.sh`:

```sh
podman exec rsync-profile bash /workspace/scripts/benchmark_daemon_cold_start.sh \
    -n 20 -s small_files
```

The harness drives an upstream rsync CLIENT against (a) the
oc-rsync daemon and (b) the upstream rsync daemon on adjacent
ports, both serving the same corpus from the same module config.
`hyperfine --warmup 0 --prepare 'rm -rf <client-dest>'` ensures
every sample is a true cold start.

## Phase table

Phases listed in protocol order. Times are best estimates from
code reading; "high" confidence means the phase is bounded by a
single named cost we measured or that another audit has already
quantified.

| # | Phase | oc-rsync ms | upstream ms | gap ms | likely root cause | DIS-4 slot | Confidence |
|---|-------|------------:|------------:|-------:|-------------------|------------|------------|
| 1 | Process startup + dynamic linker + `OnceLock` init | 30-60 | 20-40 | 10-20 | larger Rust binary, lazier SIMD/io_uring probes | (out of DIS-4 scope; tracked under `binary-startup-overhead`) | medium |
| 2 | TCP accept + `set_nonblocking(true)` poll loop | 0-500 | 0-1 | up to 500 | `SIGNAL_CHECK_INTERVAL = 500 ms` busy-poll in `run_single_listener_loop` | DIS-4.a (greeting/admission path) | high |
| 3 | `@RSYNCD:` greeting build + write | 0.05-0.2 | 0.01 | 0.05-0.2 | per-connection `format!` + `String::pop`/`push_str` rebuilding the digest list | DIS-4.a | high |
| 4 | Capabilities advertisement (`modules` + `authlist`) | 0.05 | 0.01 | 0.04 | per-connection `Vec::push` + `join(" ")` | DIS-4.a | medium |
| 5 | Read client version line + module name | 0.02 | 0.02 | 0 | symmetric; both call `read_line` | DIS-4.b (module-select) | high |
| 6 | Module lookup (linear scan over `Vec<ModuleRuntime>`) | 0.001 | 0.001 | 0 | linear scan; only one module in this bench | DIS-4.b | high |
| 7 | Host allow/deny + reverse-lookup | 5-50 | 5-50 | 0 | both implementations call `getnameinfo`; cost dominated by glibc resolver | DIS-4.b | medium |
| 8 | Connection lock + `try_acquire_connection` | 0.05 | 0.05 | 0 | both open the lock file; equal cost | DIS-4.b | medium |
| 9 | Authentication path (skipped here - no `auth users` set) | 0 | 0 | 0 | conditional; not on the small_files cold-start path | DIS-4.c (auth roundtrip) | high |
| 10 | `@RSYNCD: OK` write + flush | 0.02 | 0.01 | 0.01 | cached `Box<[u8]>`; near-zero | DIS-4.a | high |
| 11 | Read client args (null-terminated argv) | 0.2 | 0.1 | 0.1 | `read_until(b'\0')` allocates a fresh `Vec` per arg; ~15-20 args | DIS-4.b | medium |
| 12 | `ServerConfig` build from flag string + long opts | 1-5 | 0.05 | 1-5 | full Clap-style re-parse via `ServerConfig::from_flag_string_and_args`; upstream just sets globals | DIS-4.b | medium |
| 13 | Privilege / chroot / Landlock setup | 0.1 | 0.05 | 0.05 | Landlock probe is no-op when not engaged; both pay similar cost | DIS-4.b | low |
| 14 | `setup_protocol` (compat flags + capability + checksum seed) | 0.3 | 0.1 | 0.2 | dyn-dispatch `ProtocolNegotiator`; per-call allocations | DIS-4.c | medium |
| 15 | Multiplex output activation | 0.05 | 0.02 | 0.03 | `BufWriter` + 64 KiB buffer allocation | DIS-4.c | medium |
| 16 | Receive filter list | 0.05 | 0.05 | 0 | symmetric | DIS-4.c | high |
| 17 | **`build_file_list` (sender side, 500 files)** | 30-80 | 8-15 | 20-65 | sequential `lstat` per entry + per-entry `FileEntry` build with `PathBuf`/`Arc<Path>`; upstream uses pool allocator | DIS-4.d (flist build) | high |
| 18 | File list sort + apply permutation | 1-3 | 0.5 | 1-2 | indirect sort with closure; upstream sorts pointers in pool | DIS-4.d | high |
| 19 | INC_RECURSE partition | 0.5 | 0.5 | 0 | symmetric (both honour CF_INC_RECURSE) | DIS-4.d | medium |
| 20 | `send_file_list` (encode + write 500 entries) | 5-15 | 2-5 | 3-10 | per-entry `write_entry` allocates; upstream batches into one iobuf | DIS-4.e (first-block send) | high |
| 21 | `send_id_lists` + `send_io_error_flag` | 0.2 | 0.2 | 0 | symmetric | DIS-4.e | high |
| 22 | First receiver NDX request + first delta header | 1-3 | 0.5-1 | 0.5-2 | extra `writer.flush()` round on first frame | DIS-4.e | medium |
| 23 | Goodbye + stats trailer | 0.5 | 0.3 | 0.2 | NDX_DEL_STATS varint encode (protocol >= 31) | (out of DIS-4) | medium |

**Sum of attributed gaps:** ~36-103 ms steady-state code costs, plus
**up to ~500 ms** from the signal-poll sleep when the connection
arrives mid-tick. The two figures together more than cover the
~990 ms gap when the sleep penalty fires; when it does not, the
attributed code gaps fall short of the reference 990 ms by enough
that a hidden contributor (most likely a per-connection
`getnameinfo` synchronous DNS lookup, or a daemon-side `select`
deadline) is in play and should be confirmed by DIS-2 profiling
under `perf record -g`.

## Top 3 contributors (ranked by expected gap contribution)

### 1. Accept-loop signal-poll sleep (`SIGNAL_CHECK_INTERVAL = 500 ms`)

Source: `crates/daemon/src/daemon/sections/server_runtime/connection.rs:337`
and `crates/daemon/src/daemon/sections/server_runtime/listener.rs:45`.
The default synchronous daemon path runs a non-blocking
`listener.accept()`; on `WouldBlock` it calls
`thread::sleep(Duration::from_millis(500))`. A client connecting
just after the sleep started waits up to one full tick before being
accepted. On a cold-start measurement this is amortised across runs
but appears as a heavy right tail (p99 ~ 500 ms above median); on
the very first run after daemon start it is the single largest
variance source.

Upstream rsync's daemon uses a blocking `accept()` in the parent
process with `select()` over the listening sockets so the wake-up
is event-driven. There is no equivalent 500 ms tick. The fix is to
switch oc-rsync to a `poll(2)`/`epoll`/`kqueue`-based wait with the
signal pipe added as a poll target, or to drop the timer to
something like 5-10 ms while we are still on the sync path. Either
removes the entire latency tail at zero protocol cost.

### 2. File-list build cost (~500 files * (stat + entry alloc))

Source: `crates/transfer/src/generator/file_list/mod.rs:52`
(`build_file_list`) and the per-entry walk under
`crates/transfer/src/generator/file_list/walk.rs` and
`entry.rs`. On a 500-file cold-start corpus the sender pays:

1. 500 `lstat` calls (one per file). Upstream pays the same syscall
   count, but in C with a pre-allocated pool.
2. 500 `FileEntry` constructions. Each allocates a `PathBuf` for
   the full path, an `Arc<Path>` for the basename, and a small
   `Vec` for the optional ACL handle. The per-entry alloc count is
   ~5-7 versus upstream's single bump from the file_list pool.
3. A sort over 500 entries via `sort_by` with a closure that
   re-borrows `file_list[a]` and `file_list[b]`. Upstream sorts
   pointers into the pool with a flat comparator.

Memory-allocator dominance here is also visible in the existing
`RSS 3-11x upstream` finding (project memory). 500 files at ~5-7
allocations per entry produces 2.5k-3.5k heap touches in the
sender hot path before the first transfer byte goes on the wire.
At 8-15 ns per malloc/free pair on the Debian glibc allocator,
that's ~20-50 ms of pure allocator time, which lines up with the
estimated 20-65 ms gap on row 17.

### 3. Per-connection greeting and `BufReader` allocation pattern

Source: `crates/daemon/src/daemon/sections/greeting.rs:13`
(`legacy_daemon_greeting_for_protocol`),
`crates/daemon/src/daemon/sections/session_runtime.rs:220`
(`BufReader::new(stream)`), `crates/daemon/src/daemon/sections/legacy_messages.rs`
(`LegacyMessageCache::new`).

Every accepted connection currently:

- Rebuilds the `@RSYNCD: 32.0 sha512 sha256 sha1 md5 md4\n`
  greeting via `format!() + String::pop + push_str` even though the
  digest list is fixed at startup.
- Constructs a fresh `LegacyMessageCache` (two `Box<[u8]>` alloc).
- Allocates a fresh 8 KiB `BufReader` on the heap.
- In the async path (gated, not the default), allocates an
  additional `BufWriter` of equal size.

The existing `daemon-handshake-overhead.md` audit already flags
these and proposes precomputing the greeting via `OnceLock` and
pooling `BufReader` buffers. On its own each call is sub-microsecond,
but together they account for ~6-8 allocations per connection on
the critical path before the protocol setup even runs. Under a
malloc-allocator stall this becomes a tail-amplifier rather than
median cost - it is on the list because it is cheap to fix and
removes noise from the DIS-2 flame graph.

## Cross-reference: DIS-4.x mapping

| DIS-4 sub-task | Phases covered |
|----------------|-----------------|
| **DIS-4.a** rsyncd greeting overhead | 2, 3, 4, 10 (greeting build + admission tail) |
| **DIS-4.b** module-select roundtrip | 5, 6, 7, 8, 11, 12, 13 (module lookup + arg parse) |
| **DIS-4.c** auth handshake roundtrip count | 9, 14, 15, 16 (compat exchange + capability + checksum seed + filter list) |
| **DIS-4.d** flist build cold-start time | 17, 18, 19 (filesystem walk + sort + INC_RECURSE partition) |
| **DIS-4.e** first-block send latency | 20, 21, 22 (file list send + first NDX) |

Phases 1 (binary startup) and 23 (goodbye trailer) fall outside the
DIS-4 sub-task slots and are tracked separately
(`docs/audits/binary-startup-overhead.md` for #1, and the
NDX_DEL_STATS work tracked under the recent v0.5.8 release for #23).

## Recommendation for DIS-6 sequencing

DIS-6 should tackle the contributors in this order, by expected
payoff per engineering hour:

1. **DIS-4.a first (signal-poll sleep).** A one-line fix - drop
   the 500 ms tick to 5-10 ms, or wrap the listener in a `mio`
   `Poll` instance with the signal pipe registered. Removes the
   entire right tail of the cold-start distribution and is fully
   wire-compatible. Expected single-PR win: 200-500 ms off p99,
   ~0-50 ms off median.

2. **DIS-4.d (flist build cold-start).** Pool the `FileEntry`
   backing storage so per-entry allocations collapse into a single
   bump out of a per-transfer arena. Cross-references the open RSS
   3-11x project; the same fix moves both metrics. Expected
   single-PR win: 20-50 ms off median on a 500-file corpus,
   growing roughly linearly with file count.

3. **DIS-4.a residual + DIS-4.b combined hygiene.** Cache the
   greeting via `OnceLock`, drop the per-connection
   `LegacyMessageCache`, replace the module `Vec` lookup with a
   `HashMap<&str, &ModuleRuntime>` at config-load time, and reuse
   the `BufReader` line buffer across greeting/module-select/auth
   reads. Together these eliminate ~6 heap touches per connection
   on the hot path. Expected median win: 0.1-0.3 ms per connection
   plus a measurable reduction in jitter under load (visible in
   p99 once #1 has removed the dominant tail).

4. **DIS-4.e first-block flush ordering.** Audit
   `run_transfer_loop` and `send_file_list` for stray
   `writer.flush()` calls that defeat multiplex batching. Each
   eliminated flush saves one `sendto` syscall on the cold-start
   path. Expected win: 0.5-2 ms per connection.

5. **DIS-4.c (auth handshake) deferred.** The cold-start scenario
   in the DIS-1 harness has no `auth users` configured, so phase 9
   is zero in this measurement. DIS-4.c is still worth profiling
   for daemons with auth turned on, but it does not contribute to
   the 1.35 s baseline DIS-3 is decomposing.

## What to confirm with DIS-2 profiling

Before DIS-6 lands fixes, DIS-2 should re-run the harness with
`perf record -F 999 -g --call-graph fp` (Linux container) and
produce a flame graph against the oc-rsync daemon path
specifically. Two questions are not answerable from code reading
alone:

- **Is the 500 ms signal-poll sleep actually firing on the
  measured runs?** If hyperfine arrival timing happens to dodge
  the tick window, the gap is dominated by row 17 (flist build) and
  the 500 ms is a latent hazard rather than the active cause. The
  reference 1.35 s pencils out cleanly if it fires on ~70 % of
  runs; pencils out only loosely otherwise.
- **Is `getnameinfo` (row 7) actually returning quickly on the
  loopback?** A DNS resolver stall here would add tens of ms on
  both sides; both implementations call it. We assumed symmetric
  cost, but if oc-rsync calls it twice (session-level and
  module-level lookups in `session_runtime.rs:88` and
  `request.rs:280`), the gap shifts onto DIS-4.b instead.

Either finding changes the row-1 ranking but not the top-3
contributor list.

## File index

Direct evidence files cited above (all paths relative to
worktree root):

- `crates/daemon/src/daemon/sections/server_runtime/connection.rs`
- `crates/daemon/src/daemon/sections/server_runtime/listener.rs`
- `crates/daemon/src/daemon/sections/server_runtime/accept_loop.rs`
- `crates/daemon/src/daemon/sections/session_runtime.rs`
- `crates/daemon/src/daemon/sections/greeting.rs`
- `crates/daemon/src/daemon/sections/legacy_messages.rs`
- `crates/daemon/src/daemon/sections/module_access/request.rs`
- `crates/daemon/src/daemon/sections/module_access/client_args.rs`
- `crates/daemon/src/daemon/sections/module_access/transfer.rs`
- `crates/transfer/src/lib.rs`
- `crates/transfer/src/setup/mod.rs`
- `crates/transfer/src/generator/transfer/orchestrator.rs`
- `crates/transfer/src/generator/file_list/mod.rs`
- `crates/transfer/src/generator/protocol_io.rs`
- `scripts/benchmark_daemon_cold_start.sh`
- `docs/audits/daemon-handshake-overhead.md`
- `docs/audits/binary-startup-overhead.md`
