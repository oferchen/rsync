# DIS-2: daemon cold-start performance profile

Tracking: DIS-2. Parent: DIS-1 (PR #4813). Related: DIS-3 through
DIS-8.b.

Memory note: `[[project_daemon_initial_sync_3x_slow]]`.

## 1. Executive summary

The oc-rsync daemon cold-start path - from TCP accept to first data
byte - runs ~1.35 s versus upstream rsync 3.4.1's ~0.36 s on the
reference 500-file / 1 KiB-each `small_files` corpus, a 3.7x gap.
This document synthesises the DIS-3 through DIS-6 audit results into a
single critical-path trace, maps each phase to its measured or
estimated wall-clock cost, identifies which contributors have been
fixed by DIS-6, which remain open, and what the expected gap is after
all open fixes land.

DIS-8.a (PR #4905) added a CI bench cell that monitors the cold-start
ratio. DIS-8.b specifies the bake-window criteria for promoting the
cell to a required check once DIS-7 closes the gap to <= 1.1x.

## 2. Reference measurement

Methodology: `hyperfine --warmup 0 --runs 20 --prepare 'rm -rf
<dest>'` driving an upstream rsync CLIENT against (a) the oc-rsync
daemon and (b) the upstream rsync 3.4.1 daemon, both serving the same
500-file corpus from identical module configs on the rsync-profile
podman container (Debian, glibc allocator).

| Binary | Mean | Median | p5 | p95 | Stddev |
|--------|------|--------|----|-----|--------|
| oc-rsync daemon | ~1.35 s | ~1.30 s | ~0.85 s | ~1.85 s | ~0.25 s |
| upstream rsync 3.4.1 | ~0.36 s | ~0.35 s | ~0.33 s | ~0.40 s | ~0.02 s |

Gap: ~0.99 s / ~3.7x. High variance on oc-rsync (stddev 0.25 s vs
upstream 0.02 s) is diagnostic - the dominant contributor is a binary
latency tail from the accept-loop signal-poll sleep.

## 3. Critical path trace

The daemon cold-start critical path has six phases, each owned by a
DIS-4.x sub-task. Phases are serial - the client cannot advance to the
next phase until the daemon completes the current one.

```
TCP accept ──> Greeting ──> Module-select ──> Auth/Compat ──> Flist build ──> First-block send
  Phase A        Phase B       Phase C          Phase D         Phase E          Phase F
 DIS-4.a        DIS-4.a       DIS-4.b          DIS-4.c         DIS-4.d          DIS-4.e
```

### Phase A: TCP accept and worker dispatch

**Source:** `crates/daemon/src/daemon/sections/server_runtime/connection.rs`
lines 330-397 (`run_single_listener_loop`),
`listener.rs:45` (`SIGNAL_CHECK_INTERVAL`).

**Protocol:** None (pre-protocol).

**What happens:**
1. Listener runs a non-blocking `accept()` in a poll loop.
2. On `WouldBlock`, sleeps for `SIGNAL_CHECK_INTERVAL = 500 ms`
   before retrying.
3. On success, calls `set_nonblocking(false)` on the accepted socket.
4. Applies socket options, checks `max_connections` cap.
5. Spawns a per-connection worker thread via `thread::spawn`.

**Cost:**

| Operation | oc-rsync | upstream 3.4.1 | Gap |
|-----------|----------|----------------|-----|
| Accept-loop wake primitive | `thread::sleep(500ms)` after `WouldBlock` | `select(NULL)` - truly blocking, event-driven | **0-500 ms** latency tail |
| Worker dispatch | `thread::spawn` (1 `clone3`, 1 alloc for `JoinHandle`) | `fork()` (1 `clone`, 0 heap alloc in parent) | +1 alloc |
| Socket config | `set_nonblocking(false)` + `set_read_timeout` + `set_write_timeout` (3 syscalls) | `set_socket_options` + `set_nonblocking` (2 syscalls) | +1 syscall |

**Estimated wall-clock:** 0-500 ms (dominated by the signal-poll
sleep; when the client arrives mid-tick the full residual applies).

**Status:** UNFIXED. This is the single largest contributor. The
500 ms tick means the cold-start measurement has a bimodal distribution:
~50% of runs catch the tick window and pay ~250 ms median penalty,
explaining both the 3.7x gap and the high stddev.

**Dual-stack variant:** The dual-stack path
(`run_dual_stack_loop`, `connection.rs:404-500`) uses per-listener
acceptor threads with `thread::sleep(50ms)` and `recv_timeout(100ms)`
on the MPSC channel. This caps the worst case at ~100 ms instead of
500 ms, but is not the default path in the DIS-1 harness.

### Phase B: `@RSYNCD:` greeting exchange

**Source:** `crates/daemon/src/daemon/sections/session_runtime.rs`
lines 207-321 (`handle_legacy_session`),
`greeting.rs` (`cached_legacy_daemon_greeting`, `read_trimmed_line`),
`legacy_messages.rs` (`LegacyMessageCache::shared`).

**Protocol:** Server sends `@RSYNCD: 32.0 sha512 sha256 sha1 md5
md4\n`. Client responds with `@RSYNCD: 32.0\n` then the module name
(or `#list`).

**What happens:**
1. Write cached greeting bytes (41 B) via `write_limited`.
2. Read client version line via `read_trimmed_line`.
3. Read module-request line via `read_trimmed_line`.

**Cost (after DIS-6 fixes):**

| Operation | oc-rsync (current) | upstream | Gap |
|-----------|-------------------|----------|-----|
| Greeting build | `OnceLock<Box<[u8]>>` cached; zero alloc per accept | `io_printf` from stack buffer | 0 |
| `LegacyMessageCache` | `OnceLock` shared; zero alloc per accept | Inline `io_printf` | 0 |
| `BufReader` for greeting reads | Fresh 8 KiB heap alloc per accept | Stack `line[1024]` | +2 allocs |
| Per-line `String` for `read_trimmed_line` | Fresh `String::new()` per line, 2 lines | Stack buffer reuse | +2 allocs |
| No-op `flush()` after greeting | Removed by DIS-6 | n/a | 0 |

**Wire bytes:** 41 B greeting (byte-identical). 0 extra segments.

**Estimated wall-clock:** 0.1-0.5 ms. After DIS-6, the
per-accept allocation count dropped from ~9-11 to ~4-5 (BufReader +
two line Strings + one limiter struct). The remaining cost is
sub-millisecond on glibc.

**Status:** PARTIALLY FIXED by DIS-6 (PR #4890). Greeting cache
and message cache are hoisted to `OnceLock`. Still open: BufReader
pool (DIS-4.a R4) and line-buffer reuse (DIS-4.a R5).

### Phase C: module-select and arg parsing

**Source:** `crates/daemon/src/daemon/sections/module_access/request.rs`,
`client_args.rs`, `transfer.rs`,
`crates/daemon/src/daemon/sections/module_parsing.rs`.

**Protocol:** Client sends null-terminated argv (~15-20 args). Server
resolves module, applies host allow/deny, reads client args, builds
`ServerConfig`, validates module path, applies Landlock sandbox,
prepares stream halves.

**What happens:**
1. Linear scan over `&[ModuleRuntime]` for the module name.
2. Optional reverse-DNS lookup (`getnameinfo`, default enabled).
3. Optional `max_connections` lock-file roundtrip (default disabled).
4. `module.definition.clone()` - deep-copies all config fields.
5. Read ~15-20 null-terminated args, each into a fresh `Vec<u8>` +
   `String::from_utf8_lossy(...).into_owned()`.
6. `ParsedServerFlags::parse` + `apply_long_form_args`.
7. `validate_module_path` (1 `stat`) + `canonicalize` (1-5 syscalls).
8. `set_nodelay(true)` + 2x `try_clone()` for stream splitting.

**Cost:**

| Operation | oc-rsync | upstream | Gap |
|-----------|----------|----------|-----|
| Per-arg `Vec` + `String` (15-20 args) | ~30-40 small allocs, ~15-20 `read` syscalls | ~15-20 `strdup`s into single `argv`, stack read buffer | **+15-20 allocs** |
| `module.definition.clone()` | ~10-30 allocs (deep clone) | Global mutation, 0 alloc | **+10-30 allocs** |
| Module path validation | 2-6 syscalls (`stat` + `canonicalize`) | 0 syscalls (string-only `normalize_path`) | **+2-6 syscalls** |
| Stream prep (`set_nodelay` + 2x `dup`) | 3 syscalls | 0 (raw fd reuse) | **+3 syscalls** |
| Landlock sandbox (Linux only) | 3-5 syscalls on supporting kernels | No equivalent | **+3-5 syscalls** |

**Wire bytes:** ~48 B client args (byte-identical). 0 extra segments.

**Estimated wall-clock:** 1-5 ms total. The per-arg allocation chain
(~30-40 small allocs) is the median-cost dominant contributor.

**Status:** UNFIXED. Open recommendations from DIS-4.b:
- R1: Gate `definition.clone()` on `!options.is_empty()`.
- R2: Reuse per-arg read buffer.
- R3: Drop eager `validate_module_path` stat.
- R4: Switch lock file to per-slot `fcntl(F_SETLK)` ranges.

### Phase D: compat exchange, multiplex activation, filter list

**Source:** `crates/transfer/src/setup/mod.rs` (`setup_protocol`),
`crates/transfer/src/lib.rs` lines 411-532
(`run_server_with_handshake`).

**Protocol:** Compat byte + capability marker + digest name list +
checksum seed (42 B total). Client responds with peer-ok ack +
capability reply. Multiplex I/O rings allocated. Empty filter list
received (4 B).

**What happens:**
1. Auth bypass: `requires_authentication()` short-circuits; writes
   cached `@RSYNCD: OK\n` (12 B, 1 `write`).
2. `setup_protocol` with `&dyn ProtocolNegotiator`: compat flags
   exchange, capability negotiation, checksum seed. 2-6 small `Vec<u8>`
   allocations for algorithm tables.
3. Allocate 64 KiB `BufReader` for multiplex input.
4. Allocate 64 KiB `ServerWriter` for multiplex output +
   `activate_multiplex()`.
5. Receive empty filter list (1 `read` of 4 B zero-length terminator).

**Cost:**

| Operation | oc-rsync | upstream | Gap |
|-----------|----------|----------|-----|
| `setup_protocol` allocs | 2-6 `Vec<u8>` for algo tables | Module-static storage, 0 alloc | **+2-6 allocs** |
| Multiplex input ring | 64 KiB heap alloc per connection | Process-static `iobuf_in` reused via fork | **+64 KiB alloc** |
| Multiplex output ring | 64 KiB heap alloc per connection | Process-static `iobuf_out` reused via fork | **+64 KiB alloc** |
| `#` capability marker | Separate `write_all(b"#")` | Coalesced with name list | **+1 segment** |

**Wire bytes:** 42 B compat (byte-identical content); +1 segment from
split capability marker write (R-WIRE-1 in DIS-5).

**Estimated wall-clock:** 0.5-1 ms. The two 64 KiB allocations
dominate; on the first connection post-start, glibc issues `mmap` for
the second 64 KiB chunk (~50-200 us cold, ~5-15 us warm).

**Status:** UNFIXED. Open recommendations from DIS-4.c:
- R1: Pool multiplex ring buffers.
- R2: Cache negotiated algorithm tables in `OnceLock`.
- R6: Monomorphise `setup_protocol` to avoid dyn-dispatch allocs.

### Phase E: file-list build (sender side)

**Source:** `crates/transfer/src/generator/file_list/mod.rs`
(`build_file_list`), `walk.rs`, `entry.rs`, `batch_stat.rs`,
`inc_recurse.rs`.

**Protocol:** None (on-sender filesystem walk). Wire bytes emitted
only after the build completes.

**What happens:**
1. `readdir` + collect 500 `OsString` names.
2. `batch_stat_dir_entries`: 500 `lstat` calls (parallel via rayon
   above threshold 64).
3. 500 x `create_entry` + `FileEntry::new_file`: per entry,
   ~5 heap ops (relative `PathBuf`, dirname `Arc<Path>`, full-path
   `PathBuf`, walker transients).
4. Sort 500 entries via indirect permutation + cycle-following swaps.
5. INC_RECURSE partition (if negotiated; off on the default push path).

**Cost:**

| Sub-phase | oc-rsync | upstream | Gap |
|-----------|----------|----------|-----|
| Stat dispatch | ~20 ms (rayon parallel) | ~10 ms (sequential `lstat`) | ~10 ms |
| 500 x `create_entry` | ~12 ms (5 heap ops/entry) | ~3 ms (1 pool bump/entry) | **~9 ms** |
| Sort 500 entries | ~2 ms (indirect permutation) | ~0.5 ms (pointer sort) | ~1.5 ms |
| INC_RECURSE partition | ~0.4 ms (if active) | ~0.4 ms | 0 |

**Wire bytes:** 0 (all on-sender).

**Estimated wall-clock:** ~35-40 ms total. Per-entry heap-op
ratio is ~5x upstream (~5 allocs vs ~1 pool bump per entry).

**Status:** UNFIXED. DIS-4.d recommends waiting for RSS-7+8+12
(arena allocator for `FileEntry` backing storage). The flist build gap
is fully addressable by the open RSS work:
- RSS-7: Replace `PathBuf` with `Box<Path>` for entry name.
- RSS-8: Per-flist arena for basenames.
- RSS-12: Sender-side `PathInterner` for dirnames.

Combined expected saving: ~10-15 ms on the 500-entry build.

### Phase F: file-list send and first data byte

**Source:** `crates/transfer/src/generator/protocol_io.rs`
(`send_file_list`, `send_id_lists`, `send_io_error_flag`),
`crates/transfer/src/generator/transfer/transfer_loop.rs`
(`run_transfer_loop`), `delta.rs` (`stream_whole_file_transfer`).

**Protocol:** File list encoded as MSG_DATA mplex frame (~22.5 KiB for
500 entries). Id lists + io_error trailer. Then per-file: NDX read,
delta header, file open, read, MSG_DATA token, checksum trailer.

**What happens:**
1. 500 x `write_entry` into 32 KiB mplex buffer. Single flush at end.
2. `send_id_lists` + `send_io_error_flag` - separate mplex frame.
3. `flush_with_count` before each NDX read (1 extra `writev`/iter).
4. Per-file: `File::open` + `BufReader(4 KiB)` + read + checksum.
5. Per-file: `source_path.display().to_string()` (unconditional alloc).
6. Per-file: `ChecksumVerifier::for_algorithm` (1 alloc/file).
7. Per-file: 2 x `Instant::now` timing wrappers on no-op paths.
8. Per-file: MSG_INFO itemize frame emitted as separate segment.

**Cost:**

| Operation | oc-rsync | upstream | Gap |
|-----------|----------|----------|-----|
| Flist end marker | Separate mplex frame (4 B header + 2 B body) | Appended to flist frame body | **+1 segment, +6 B** |
| Per-file MSG_INFO itemize | Separate segment per file | No sender-side itemize; receiver derives from iflags | **~+500 segments on 500-file corpus** |
| Goodbye stats | 5 small frames (39 B) | 2 frames (26 B) | **+3 segments, +13 B** |
| Per-file path-display String | 1 alloc/file | 0 (stack `fname[]`) | +500 allocs |
| Per-file `ChecksumVerifier` | 1 alloc/file | Static `sum_init` | +500 allocs |
| `flush_with_count` per NDX | 1 extra `writev`/iter | `perform_io` flushes inside `select` | +1 syscall/iter |

**Wire bytes:** +157 B total (+2.7%), but +14 extra segments on a
5-file corpus, extrapolating to ~+1400 segments on the 500-file
corpus. The per-segment syscall cost (~1-2 us each) totals ~1-2 ms.

**Estimated wall-clock:** 5-15 ms for the flist send, plus
~0.5-2 ms for the first-file delta. Per-file overhead ~200-300 ns/file
above upstream.

**Status:** UNFIXED. Open recommendations from DIS-4.e and DIS-5:
- DIS-6.W1: Drop per-file MSG_INFO frames (~1-2 ms on 500 files).
- DIS-6.W2: Coalesce capability marker (+1 segment/connection).
- DIS-6.W3: Inline flist end marker (+1 segment/transfer).
- DIS-6.W4: Flush goodbye stats once (+3 segments/transfer).
- DIS-4.e R1: Defer path-display to error path.
- DIS-4.e R2: Gate no-op `Instant::now` timing wrappers.
- DIS-4.e R3: Reuse `ChecksumVerifier` across files.

## 4. Gap attribution summary

| Phase | Gap (ms) | % of total gap | Status | Tracking |
|-------|----------|----------------|--------|----------|
| A: Accept-loop signal-poll sleep | 0-500 (median ~250) | ~25-50% | **UNFIXED** | DIS-4.a R1 |
| B: Greeting allocs (post-DIS-6) | 0.1-0.5 | < 0.1% | Partially fixed | DIS-4.a R4, R5 |
| C: Module-select arg parsing | 1-5 | ~0.3% | **UNFIXED** | DIS-4.b R1-R4 |
| D: Compat exchange + multiplex rings | 0.5-1 | ~0.1% | **UNFIXED** | DIS-4.c R1, R2 |
| E: Flist build (sender) | 20-40 | ~3-4% | **UNFIXED** (deferred to RSS) | DIS-4.d / RSS-7,8,12 |
| F: Flist send + first delta | 5-15 | ~1-2% | **UNFIXED** | DIS-4.e / DIS-5 W1-W4 |
| **Attributed total** | **27-562** | | | |
| **Measured total gap** | **~990** | | | |

The attribution range of 27-562 ms covers ~60% of the measured 990 ms
gap. The unattributed residual (~430-963 ms depending on whether the
signal-poll sleep fires) likely lives in:

1. **Per-connection `getnameinfo` latency** (5-50 ms per call;
   confirmed symmetric with upstream but possibly called twice on
   oc-rsync - once at session level and once at module level).
2. **Scheduler latency on `thread::spawn`** (the worker thread may not
   be scheduled immediately after `clone3`; under load, the gap
   between accept and worker entry can be 0.5-2 ms).
3. **Transfer-loop per-file steady-state overhead** (200-300 ns/file x
   500 files = 100-150 us) accumulated across the transfer.
4. **Allocator cold-start penalty** (glibc `tcache` is empty on the
   first connection; the ~50-90 allocs in phase C and ~128 KiB mmap
   in phase D hit the slow arena path).

## 5. DIS-6 fixes landed (PR #4890)

DIS-6 implemented three of the DIS-4.a recommendations:

| Recommendation | What it fixed | Allocs removed/connection |
|----------------|---------------|--------------------------|
| DIS-4.a R2 | Greeting bytes cached in `OnceLock<Box<[u8]>>` | 1-3 |
| DIS-4.a R3 | `LegacyMessageCache` hoisted to `OnceLock` | 2 |
| DIS-4.a R6 | No-op `TcpStream::flush` dropped | 0 (dispatch only) |

Combined effect: ~3-5 allocs and ~0 syscalls removed from the greeting
critical path. Estimated wall-clock saving: < 0.1 ms per connection
(the fixes remove noise, not the dominant contributor).

These fixes are verified by the
`cached_legacy_daemon_greeting_matches_per_call_bytes` test which
asserts the cached bytes are byte-identical to the per-call builder.

## 6. Remaining bottleneck ranking

Ordered by expected wall-clock payoff. Each entry references the
originating audit and recommendation ID.

### Tier 1: removes the dominant tail (200-500 ms off p99)

**6.1 Replace `SIGNAL_CHECK_INTERVAL = 500 ms` with event-driven
accept** (DIS-4.a R1).

The single-listener path at `connection.rs:374-377` calls
`thread::sleep(Duration::from_millis(500))` after every `WouldBlock`.
Upstream uses `select(NULL)` which wakes the instant a connection
arrives.

Fix: wrap the listener fd in `mio::Poll` (or `libc::poll` / `epoll` /
`kqueue`) with the signal self-pipe as a second poll target. Or, as a
simpler first step, drop the interval to 5-10 ms. Either removes the
entire 0-500 ms latency tail at zero protocol cost.

Expected saving: **200-500 ms off p99**, ~50-250 ms off median.
This alone halves the gap.

### Tier 2: reduces the steady-state gap (20-50 ms off median)

**6.2 Arena allocator for `FileEntry` (flist build)** (DIS-4.d /
RSS-7+8+12).

Per-entry allocation count is ~5x upstream (~5 heap ops vs ~1 pool
bump). The fix is a per-flist bump arena that collapses `PathBuf`,
`Arc<Path>`, and `full_paths` into a single allocation per entry.
This is the RSS work already tracked in the project.

Expected saving: **10-15 ms on 500-entry build**, scaling linearly
with file count.

### Tier 3: removes per-connection allocation overhead (1-5 ms)

**6.3 Reuse per-arg read buffer** (DIS-4.b R2).

`client_args.rs` allocates a fresh `Vec<u8>` per argument (~15-20 per
connection) and converts each via `String::from_utf8_lossy().into_owned()`.
Hoist the buffer before the loop; clear between iterations.

Expected saving: **~30-40 allocs per connection**, ~0.5-1 ms.

**6.4 Gate `module.definition.clone()` on `--dparam` presence**
(DIS-4.b R1).

The deep clone fires unconditionally even though `--dparam` is never
sent on the default cold-start path. Gate on `!options.is_empty()`.

Expected saving: **~10-30 allocs per connection**, ~0.3-0.5 ms.

**6.5 Pool multiplex ring buffers** (DIS-4.c R1).

Two 64 KiB allocations per connection (input + output rings). Pool
behind `crossbeam_queue::ArrayQueue` sized to `max_connections`.

Expected saving: **~128 KiB resident per connection**, ~50-200 us
cold-start.

### Tier 4: removes per-transfer segment overhead (1-2 ms)

**6.6 Drop per-file MSG_INFO frames** (DIS-5 R-WIRE-4 / DIS-6.W1).

The sender emits a separate MSG_INFO segment per file for itemize
output. Upstream defers itemize to the receiver. On a 500-file
corpus this is ~500 extra segments, each costing ~1-2 us of syscall
overhead.

Expected saving: **~1-2 ms on 500-file corpus**.

**6.7 Coalesce small wire writes** (DIS-5 R-WIRE-1,3,5 / DIS-6.W2-W4).

Three minor framing divergences add ~5 extra segments per transfer.
Fix by merging the `#` capability marker, inlining the flist end
marker, and flushing goodbye stats once.

Expected saving: **~15-30 us per connection**.

### Tier 5: micro-optimizations (< 1 ms combined)

**6.8** BufReader pool for greeting reads (DIS-4.a R4).
**6.9** Line-buffer reuse across greeting reads (DIS-4.a R5).
**6.10** Drop eager `validate_module_path` stat (DIS-4.b R3).
**6.11** Defer path-display `to_string()` to error path (DIS-4.e R1).
**6.12** Gate no-op `Instant::now` timing wrappers (DIS-4.e R2).
**6.13** Reuse `ChecksumVerifier` across files (DIS-4.e R3).
**6.14** Cache algorithm tables in `OnceLock` (DIS-4.c R2).

Combined expected saving: **< 1 ms** per connection on the 500-file
corpus.

## 7. Projected gap after fixes

| Scenario | Estimated oc-rsync | Upstream | Ratio |
|----------|-------------------|----------|-------|
| Current (pre-DIS-7) | ~1.35 s | ~0.36 s | ~3.7x |
| After 6.1 only (signal-poll fix) | ~0.85-1.1 s | ~0.36 s | ~2.4-3.1x |
| After 6.1 + 6.2 (signal-poll + arena) | ~0.80-0.95 s | ~0.36 s | ~2.2-2.6x |
| After 6.1-6.7 (all tier 1-4) | ~0.75-0.90 s | ~0.36 s | ~2.1-2.5x |
| After all fixes (6.1-6.14) | ~0.73-0.88 s | ~0.36 s | ~2.0-2.4x |

**Note:** The projected ratios remain above 1.1x even with all
identified fixes. The residual gap of ~0.37-0.52 s (1.0-1.4x) is
attributable to structural factors that do not have single-PR fixes:

- **Rust binary startup overhead** (~30-60 ms vs upstream's ~20-40 ms;
  larger binary, lazier init).
- **Per-entry `FileEntry` size** (88 B vs upstream's ~40-50 B
  `file_struct`). Even with an arena, the larger entry means more
  cache pressure during sort and transfer.
- **Thread-per-connection model** vs upstream's `fork()`. Thread
  creation is comparable in syscall cost, but the forked child inherits
  the parent's pre-allocated `iobuf` static arrays (128 KiB) whereas
  each thread must allocate its own.
- **Per-connection allocator profile**: even after pooling the large
  buffers, oc-rsync still issues ~20-30 small allocations per
  connection that upstream avoids by using stack buffers and global
  state.

Closing to <= 1.1x requires either a fundamentally different allocator
strategy (process-wide arena with per-thread reset, matching upstream's
pool model) or a paradigm shift in the daemon's memory model.

## 8. Comparison with upstream rsync startup path

Upstream's cold-start path (from `socket.c:start_accept_loop` through
`clientserver.c:rsync_module` to `sender.c:send_files`) differs from
oc-rsync in three structural ways:

### 8.1 Accept primitive

Upstream uses a blocking `select()` over the listening sockets with a
`NULL` timeval. The accept returns the instant the kernel marks the
listener readable. There is no polling interval.

oc-rsync uses a non-blocking `accept()` with a 500 ms sleep on
`WouldBlock`. The async listener skeleton (gated behind the
`async-daemon` feature) uses tokio's `TcpListener::accept` which is
event-driven, but this path is not the default.

### 8.2 Process model

Upstream `fork()`s per connection. The child inherits all parent state
including pre-allocated `iobuf_in` / `iobuf_out` (each a `static`
buffer). Zero heap allocations in the forked child until `strdup()` in
`read_args()`.

oc-rsync spawns a thread per connection. The thread shares the parent
address space but must allocate its own `BufReader`, `BufWriter`,
multiplex rings, and `ServerConfig`. The `OnceLock` pattern (DIS-6)
shares the greeting and message cache, but per-connection buffers
remain per-thread.

### 8.3 Allocator profile

Upstream uses stack-allocated `char line[BIGPATHBUFLEN]` for all line
reads and a single pool allocator for the file list. The per-connection
heap-touch count is ~0-2 for the greeting, ~15-25 for arg parsing
(mostly `strdup`), and ~1 per file entry (pool bump).

oc-rsync uses heap-allocated `String`, `Vec<u8>`, `PathBuf`,
`Arc<Path>`, and `BufReader` throughout. The per-connection heap-touch
count is ~4-5 for the greeting (post-DIS-6), ~50-90 for module-select
+ arg parsing, ~5 per file entry, and +2 large (64 KiB each) for
multiplex rings.

## 9. Wire-byte profile

DIS-5 captured a byte-level diff on a 5-file corpus. Key findings:

| Metric | oc-rsync | upstream | Diff |
|--------|----------|----------|------|
| Client -> server bytes | 253 | 253 | 0 |
| Server -> client bytes | 5,748 | 5,591 | **+157 (+2.8%)** |
| Server -> client segments | 24 | 10 | **+14 (+140%)** |

The byte gap is small (2.8%), but the segment gap is large (140%).
Extra segments come from:

- Per-file MSG_INFO frames (~1 per file): dominant contributor.
- Separate flist end marker frame (+1).
- Fragmented goodbye stats (+3).
- Split capability marker (+1).

Each extra segment costs one `writev` syscall plus TCP header overhead.
On the 500-file DIS-1 corpus, the ~500 extra MSG_INFO segments alone
add ~1-2 ms of pure syscall time.

## 10. CI regression detection

DIS-8.a (PR #4905) added `.github/workflows/bench-daemon-coldstart.yml`:

- **Triggers:** `workflow_dispatch`, nightly `cron`, and `pull_request`
  on daemon-relevant paths.
- **Methodology:** `hyperfine --warmup 1 --runs 10` on a 10-file
  fixture against oc-rsync and upstream rsync daemons on free localhost
  ports.
- **Pass criterion:** oc-rsync mean <= 1.5x upstream mean (placeholder
  bound).
- **Status:** Advisory (`continue-on-error: true`). DIS-8.b specifies
  the bake-window for promotion to a required check after DIS-7 closes
  the gap to <= 1.1x.

The 1.5x ceiling is intentionally loose: the measured gap is ~3.7x.
The cell exists to catch regressions beyond the current baseline while
the gap is being closed.

## 11. DIS-7 sequencing recommendation

DIS-7 should implement fixes in this order, based on the tier ranking
in section 6:

1. **6.1 (signal-poll fix)** - highest payoff, one-PR fix. Clears the
   latency tail and drops the median from ~1.35 s to ~0.85-1.1 s.
   Unblocks meaningful measurement of the remaining contributors.
2. **6.6 (drop per-file MSG_INFO)** - second-highest payoff among
   fixes that do not depend on the RSS arena work. Removes ~500 extra
   segments on the 500-file corpus.
3. **6.3 + 6.4 (arg-buffer reuse + definition clone gate)** -
   combined, removes ~40-70 allocs per connection.
4. **6.5 (multiplex ring pool)** - removes the largest per-connection
   resident allocation.
5. **6.7 (wire-write coalescing)** - minor, but easy.
6. **6.2 (arena allocator)** - largest steady-state win, but depends
   on RSS-7+8+12 landing first. Defer to the RSS sprint.
7. **6.8-6.14 (micro-optimizations)** - lowest priority; implement
   opportunistically alongside other changes.

After steps 1-5 land, DIS-7.a should re-bench and document the new
ratio. If the ratio is <= 1.1x, DIS-8.b promotion criteria are met.
If not, step 6 (arena) becomes the critical path.

## 12. What DIS-2 profiling should confirm

Before DIS-7 schedules the fixes above, a `perf record -F 999 -g
--call-graph fp` run inside the rsync-profile container against the
oc-rsync daemon path should confirm:

1. **Is the 500 ms signal-poll sleep actually firing on measured
   runs?** If hyperfine timing dodges the tick window, the gap is
   dominated by phase E (flist build) and the signal-poll fix is a
   latent hazard rather than the active cause.
2. **Is `getnameinfo` (phase C) returning quickly on loopback?** A DNS
   resolver stall would add tens of ms on both sides; if oc-rsync
   calls it twice (session-level + module-level), the gap shifts.
3. **Is the per-accept malloc cluster visible in the flame graph?** If
   yes, the DIS-4.a/4.b alloc-reduction recommendations stack cleanly.
   If not, the allocator fast path is amortizing the per-accept cost
   and the fixes are hygiene rather than critical-path.
4. **Is the per-file `MSG_INFO` segment visible as a `writev` cluster
   per file?** If yes, DIS-6.W1 alone saves ~1-2 ms.
5. **Is `thread::spawn` scheduler latency measurable?** If the gap
   between `clone3` and worker entry exceeds ~1 ms, the thread model
   itself is a contributor and the async listener becomes relevant.

## 13. File index

Source files on the cold-start critical path (paths relative to
worktree root):

### Daemon crate
- `crates/daemon/src/daemon/sections/server_runtime/connection.rs` -
  accept loops (single + dual-stack)
- `crates/daemon/src/daemon/sections/server_runtime/listener.rs` -
  `SIGNAL_CHECK_INTERVAL`, `bind_with_backlog`, `configure_stream`
- `crates/daemon/src/daemon/sections/session_runtime.rs` -
  `handle_session`, `handle_legacy_session`
- `crates/daemon/src/daemon/sections/greeting.rs` -
  `cached_legacy_daemon_greeting`, `read_trimmed_line`
- `crates/daemon/src/daemon/sections/legacy_messages.rs` -
  `LegacyMessageCache::shared`
- `crates/daemon/src/daemon/sections/module_access/request.rs` -
  module lookup, auth dispatch
- `crates/daemon/src/daemon/sections/module_access/client_args.rs` -
  arg parsing
- `crates/daemon/src/daemon/sections/module_access/transfer.rs` -
  module validation, stream prep
- `crates/daemon/src/async_listener.rs` - async listener skeleton

### Transfer crate
- `crates/transfer/src/setup/mod.rs` - `setup_protocol`
- `crates/transfer/src/setup/negotiator.rs` - `ProtocolNegotiator`
- `crates/transfer/src/lib.rs` - `run_server_with_handshake`,
  multiplex activation
- `crates/transfer/src/generator/file_list/mod.rs` - `build_file_list`
- `crates/transfer/src/generator/file_list/entry.rs` - per-entry
  construction
- `crates/transfer/src/generator/file_list/walk.rs` - filesystem walk
- `crates/transfer/src/generator/file_list/batch_stat.rs` - parallel
  stat dispatch
- `crates/transfer/src/generator/protocol_io.rs` - `send_file_list`,
  `send_id_lists`
- `crates/transfer/src/generator/transfer/transfer_loop.rs` -
  `run_transfer_loop`
- `crates/transfer/src/generator/delta.rs` -
  `stream_whole_file_transfer`

### CI
- `.github/workflows/bench-daemon-coldstart.yml` - DIS-8.a regression
  bench

### Prior audits (all in `docs/audits/`)
- `dis-3-cold-start-phase-decomposition.md` - 23-row phase table
- `dis-4a-rsyncd-greeting-overhead.md` - greeting-phase inventory
- `dis-4b-module-select-roundtrip.md` - module-select inventory
- `dis-4c-auth-handshake-roundtrip.md` - auth/compat inventory
- `dis-4d-flist-build-cold-start.md` - flist build analysis
- `dis-4e-first-block-send-latency.md` - first-block send analysis
- `dis-5-cold-start-wire-byte-diff.md` - wire-byte diff
- `daemon-handshake-overhead.md` - earlier handshake overhead survey

### Design docs
- `docs/design/dis-8-b-required-check-wiring.md` - bench promotion
  criteria
