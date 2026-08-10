# DIS-4.e: first-block send latency

Focused audit of the sender path from "flist build complete" through "first
data byte on wire". Owns DIS-3 phase-table rows **20, 21, 22** - the gap
between the receiver-side handshake completing and the sender's first
MSG_DATA frame carrying file payload.

DIS-4.d (PR #4851) covers the flist build itself; DIS-5 (planned) will
diff wire bytes against upstream 3.4.1. Both are out of scope here. This
audit is docs-only; no `.rs` files are modified.

Sources cited (paths relative to worktree root):

- `crates/transfer/src/generator/protocol_io.rs` (`send_file_list`,
  `send_id_lists`, `send_io_error_flag`, `FirstByteWriter`)
- `crates/transfer/src/generator/transfer/transfer_loop.rs`
  (`run_transfer_loop` - NDX read, `receive_sums`, first-file dispatch)
- `crates/transfer/src/generator/delta.rs`
  (`stream_whole_file_transfer`, `compute_file_checksum`,
  `generate_delta_from_signature`)
- `crates/transfer/src/generator/open_source.rs`,
  `crates/transfer/src/generator/context.rs:402` (`open_source_reader`)
- `crates/transfer/src/adaptive_buffer.rs:80` (`adaptive_buffer_size`)
- `crates/transfer/src/generator/diagnostics.rs:64` (`flush_with_count`)
- `crates/protocol/src/multiplex/writer.rs`, `multiplex/io/send.rs`
- `crates/protocol/src/flist/write/mod.rs:375` (`write_entry`)
- `crates/matching/src/generator.rs` (rolling-checksum bootstrap)
- upstream: `target/interop/upstream-src/rsync-3.4.1/sender.c:199-462`
  (`send_files`), `sender.c:71-127` (`receive_sums`),
  `match.c:362-437` (`match_sums`), `flist.c:2192-2545`
  (`send_file_list`).

## 1. Phase 20 - file-list send (DIS-3 row 20)

`send_file_list` (`protocol_io.rs:327-412`) wire walk:

| # | Operation | Where | Cost tag |
|---|-----------|-------|----------|
| 1 | `PhaseTimer::new` + `Instant::now` for `flist_xfer_start` | `protocol_io.rs:328-331` | 1 A, 2 S (`clock_gettime`) |
| 2 | `build_flist_writer()` (iconv + ACL/xattr caches + prefix state) | `protocol_io.rs:333` | ~6-10 A |
| 3 | `FirstByteWriter` wrap (stack-only adapter) | `protocol_io.rs:348` | 0 A |
| 4 | Per-entry `prepare_pending_acl` + `flist_writer.write_entry` | `protocol_io.rs:356-360`, `flist/write/mod.rs:375` | per-entry: 2 S (`Instant::now` ACL timer), 4-12 small buffered writes |
| 5 | `flist_writer.write_end` (0-byte terminator + optional `io_error`) | `protocol_io.rs:368` | 1-2 small buffered writes |
| 6 | `probed.flush()` -> `MplexWriter::flush_buffer` -> `send_msg` -> `writev(2)` | `protocol_io.rs:369`, `multiplex/writer.rs:179-195` | 1+ S |
| 7 | Cache writer for INC_RECURSE reuse | `protocol_io.rs:375` | 0 A |
| 8 | `flist_xfer_end` + diagnostic `info_log!`/`debug_log!` | `protocol_io.rs:377-401` | 1 S, 0-3 A |

For the DIS-1 small-files corpus (N=500, 1 KiB each, vanilla `-a`), each
`write_entry` produces ~30-60 B of wire payload. 500 entries * ~45 B =
~22.5 KiB - fits in one 32 KiB `MplexWriter` buffer. The final
`probed.flush()` emits **one `writev(2)`** with one 4 B header + one
~22.5 KiB payload `IoSlice`. Upstream `send_file_list` follows the same
batching pattern via `iobuf.out`; both emit exactly one frame for this
corpus. Wire-byte parity is asserted by golden tests in
`crates/protocol/tests/golden/`.

### Where oc-rsync still pays more than upstream on row 20

| Source | Per-call overhead | Per-corpus (500) |
|--------|------------------:|------------------:|
| `PhaseTimer::new` allocation | ~30 ns | ~30 ns |
| `Instant::now` pair inside `prepare_pending_acl` on no-op ACL path | ~50 ns/entry | ~25 us |
| `FirstByteWriter` extra branch per write/flush | < 1 ns/call | < 1 us |
| iconv `apply_encoding_conversion` branch (Cow::Borrowed when off) | ~ns/entry | < 5 us |

The DIS-3 row 20 estimate of 3-10 ms is dominated by the **per-entry
encoding work** (`calculate_name_prefix_len`, varint encoding,
`write_metadata`) - byte-equivalent to upstream but with Rust's
per-`write_all` `Result` dispatch overhead. Structural, not a row-20
fix candidate.

## 2. Phase 21 - id lists + io_error flag (DIS-3 row 21)

`send_id_lists` (`protocol_io.rs:42-65`) and `send_io_error_flag`
(`protocol_io.rs:75-87`) are skipped or trivial on the small-files
corpus (INC_RECURSE off, proto 32 so no proto-<30 `io_error` LE write).
The trailing `send_id_lists` flush is 1 `writev` symmetric with
upstream's `flist.c:2513`. DIS-3 row 21 estimate of 0 ms gap is
confirmed - no fix opportunity.

## 3. Phase 22 - first NDX + first delta header (DIS-3 row 22)

### 3.1 Generator stall reading the first NDX

After `send_file_list` returns, `run_transfer_loop`
(`transfer/transfer_loop.rs:104-140`):

1. `flush_with_count(writer)` (`transfer_loop.rs:131`) - forces a
   multiplex flush before blocking on read. Upstream `io.c`
   `perform_io()` flushes the output buffer inside `select(2)` whenever
   the socket is writable, so upstream's sender never owes an explicit
   flush before reading. oc-rsync's split `Read`/`Write` traits force
   the explicit `flush()` here. Cost: 1 extra `writev` syscall **only
   if** something is pending in the buffer.
2. `ndx_read_codec.read_ndx` reads the first NDX. Wire RTT dominated by
   kernel scheduling on loopback.
3. Receiver replies with `(NDX, iflags, sum_head, sig_blocks)`. For 1
   KiB files on a cold dest, `sum_head.count = 0` so
   `read_signature_blocks` returns immediately with an empty `Vec`.

### 3.2 First-file write_ndx_and_attrs + sum_head

`write_ndx_and_attrs` (`protocol_io.rs:174-186`) writes, in order:
NDX varint (1-5 B), iflags 2 B (proto >= 29), optional xattr response
(skipped on vanilla), `sum_head.write` 16 B. All four small writes
coalesce into the freshly-flushed 32 KiB `MplexWriter` buffer. **No
syscall per write**; the flush is deferred. Byte-equivalent to upstream
`sender.c:180-197`.

### 3.3 Open source + first read + first MSG_DATA

`open_source_reader` (`context.rs:402-436`) routes by file size: files
**< 1 MiB** (the entire DIS-1 corpus) take the standard path -
`File::open` + `BufReader::with_capacity(4 KiB, f)`
(`adaptive_buffer.rs:80-90` returns 4 KiB for files < 64 KiB). Files
**>= 1 MiB** with io_uring enabled and `open_noatime` off take
`fast_io::reader_from_path_with_depth` (`context.rs:411`
`IO_URING_READ_THRESHOLD = 1 MiB`).

Whole-file path through `stream_whole_file_transfer`
(`delta.rs:199-274`) for a 1 KiB file:

1. `ChecksumVerifier::for_algorithm` - 1 A for the digest state.
2. `read_size = file_size.clamp(1, 256 KiB) = 1024`.
3. `buf.resize(4 + 1024, 0)` - no malloc (capacity already 32 KiB from
   `Vec::with_capacity(32 * 1024)` at `transfer_loop.rs:67`).
4. `source.read_exact(&mut buf[4..4+1024])` - 1 `read(2)`.
5. `verifier.update` - hash 1 KiB.
6. Encode 4 B length prefix into `buf[0..4]`; `writer.write_all(&buf[..4+1024])`
   into the `MplexWriter` buffer - coalesces, **no syscall**.
7. `write_token_end` - 4 B `0x00000000` end marker into buffer.
8. `verifier.finalize_into` -> 16-64 B into stack buffer; `write_all`
   appends to `MplexWriter`.

**The first MSG_DATA frame** appears on the wire when either the 32 KiB
buffer fills or the next iteration's `flush_with_count` runs. Each 1 KiB
file contributes ~1052 B (4 + 1024 + 4 + 20 for MD5), so after ~30
files the buffer fills and triggers a flush. **The first MSG_DATA on
the wire carries multiple files batched together**, exactly matching
upstream `match.c:matched()` -> `iobuf.out` -> mplex flush on fill.

### 3.4 Where the row-22 0.5-2 ms gap comes from

| Operation | oc-rsync | Upstream | Diff |
|-----------|---------:|---------:|-----:|
| `flush_with_count` before each NDX read | 1 S/iter (no-op if empty) | 0 (`perform_io` flushes inside `select`) | +1 S/iter |
| `Box<dyn Read>` virtual dispatch | 1 vtable hop/read | direct `read(2)` via `map_ptr` | ~ns/call |
| `ChecksumVerifier::for_algorithm` heap alloc | 1 A/file | static `sum_init` | ~50-100 ns/file |
| `Vec::with_capacity(32 KiB)` for `stream_buf` | 1 A (once) | static `iobuf.in` reuse | ~1 us, one-shot |
| `Instant::now` pair for `prepare_pending_acl` / `encode_and_send_segment` on no-op path | 2 S/file | 0 | ~25 us total |
| `source_path.display().to_string()` per file (only consumed in cold error path) | 1 A/file | 0 (stack `fname[]`) | ~50 ns/file |
| `debug_log!(Send, 1, ...)` level check | 1 branch | symmetric | 0 |

The dominant nameable contributor is the **path-display allocation**
(`transfer_loop.rs:319`): `source_path.display().to_string()` runs
unconditionally on every file, but only feeds `record_open_failure`,
which fires < 0.1% of the time. The `flush_with_count` extra syscall is
correctness-required; reworking it would require sharing an `io_buf`
between reader and writer (exactly what ASY-3 / PR #4838 decided to
defer).

## 4. Per-file send-prefix cost summary

From the first NDX read to the first byte of the first file's payload
reaching the kernel, on the DIS-1 corpus (500 x 1 KiB, proto 32, no
xattrs/ACLs, MD5, no compression):

| Bucket | oc-rsync per file | Upstream per file | Diff |
|--------|-------------------|-------------------|------|
| **File open + stat** | 1 `open(2)` + lazy stat via `BufReader::read` deferral | 1 `do_open_checklinks` + explicit `do_fstat` | equal syscall count |
| **Initial block read** | 1 `read(2)` (BufReader pulls up to 4 KiB) | 1 `map_ptr` page-fault on `mmap` | equal on cold cache; upstream wins only when basis is re-read (delta path) |
| **Rolling-checksum bootstrap** | `RollingChecksum::new` + `RingBuffer::with_capacity(block_len)` + `pending_literals` + `MatchedBlocks` + read buffer **only when `has_basis`** (skipped on cold dest) | `build_hash_table` + `sum_init` (basis only) | whole-file path skips entirely; delta path: ~3-5 small allocs/file |
| **First MSG_DATA construction** | 4 B length prefix into `buf[0..4]`, one `write_all(&buf[..4+chunk])`; frame header emitted lazily on buffer fill | `write_buf` into `iobuf.out`; frame header on fill | byte-equal; both batch ~30 small files per frame |
| **Per-file timing wrappers** | 2 `Instant::now`/file | 0 | +~100 ns/file |
| **Per-file debug path String** | 1 A | 0 | +~50 ns/file |
| **Per-file checksum verifier alloc** | 1 A | 0 (static) | +~50-100 ns/file |

**Total per-file overhead vs upstream on row 22: ~200-300 ns/file fixed
+ 1 small `String` heap op + 1 verifier state alloc.** Across 500 files
~0.5-1 ms wall-clock plus allocator pressure that DIS-4.d already names
as a tail amplifier. Matches the DIS-3 row 22 estimate of 0.5-2 ms.

## 5. Cold-start vs steady-state

The first file pays setup costs the second does not:

1. **`flist_writer` build** (`protocol_io.rs:333`) - once per transfer,
   reused via `incremental.flist_writer_cache` for INC_RECURSE
   sub-segments.
2. **`MplexWriter` buffer** - `Vec::with_capacity(32 KiB)` at session
   start; first write triggers no resize.
3. **`stream_buf`** - `Vec::with_capacity(32 KiB)` at
   `transfer_loop.rs:67`; first file's `resize(4 + read_size, 0)` does
   not malloc.
4. **`token_encoder`** (compression context) - built once and reused
   across files (`transfer_loop.rs:73`), mirroring upstream `token.c`'s
   single-CCtx-per-session pattern.
5. **NDX codecs** (`ndx_read_codec`, `ndx_write_codec`,
   `flist_ndx_codec`) - 3 allocs on entry, paid once.
6. **Adaptive read buffer** in `open_source_reader` -
   `BufReader::with_capacity(4 KiB)` allocates **per file**, not
   reused. Comparable to upstream's `mmap` per file.
7. **`ChecksumVerifier::for_algorithm`** - 1 A/file, steady-state floor.

On file #2 oc-rsync skips items 1-5 (~50 us one-shot) but still pays
6-7 (~200 ns/file). The cold-start premium attributable to file #1
alone is bounded at **~50 us out of the 1.35 s total** - the DIS-1 gap
is amortised across all 500 files via items 6-7 plus the per-file
`String` and timing-wrapper costs in section 4, not concentrated at
file #1.

## 6. Cross-references

### 6.1 Boundary with DIS-4.d (flist build)

- DIS-4.d owns everything before `send_file_list()` is called.
- DIS-4.e owns everything from `send_file_list()` entry onwards.
- The hand-off via `incremental.flist_writer_cache`
  (`protocol_io.rs:375` writer, `transfer_loop.rs:87-91` reader) costs
  nothing. No double-count.

### 6.2 What is out-of-scope for DIS-5 wire-byte diff

DIS-5 will diff wire bytes against an upstream 3.4.1 capture. The byte
stream on rows 20-22 is already covered by `crates/protocol/tests/golden/`
and the interop matrix. DIS-5 should focus on:

- The exact framing boundary where the first MSG_DATA appears (which
  file boundary triggers the buffer flush).
- The `flush_with_count` extra `writev` calls (section 3.1) versus
  upstream's opportunistic `perform_io` batching - upstream sends
  MSG_DATA less often per same-size payload.

**Not** in DIS-5 scope: byte-for-byte content of MSG_DATA payloads
(delta tokens); already covered by interop tests.

### 6.3 Concurrent-delta entry point (out of scope)

`crates/engine/src/concurrent_delta/` is **not** on the row-22 path.
The production transfer loop uses sequential
`generate_delta_from_signature` (`transfer_loop.rs:354`). The parallel
applier is feature-gated and disabled by default; see project memory
"parallel-receive-delta is phase 1 only". A first-block latency audit
of the parallel path belongs to a future PIP-N task, not DIS-4.e.

## 7. Recommendation - ranked fix sketches for DIS-6

Ranked by expected payoff per engineering hour:

### Recommendation 1 - drop the per-file path display materialization

`transfer_loop.rs:319`: `let source_path_display =
source_path.display().to_string();` is consumed only inside
`record_open_failure` (< 0.1% of files). Wrap in a closure or pass
`&Path` and defer `to_string()` to the error path.

- Cost: ~10 lines. Risk: low.
- Win: ~50 ns/file + heap pressure removed. 500 files: ~25 us + tcache
  hit-rate improvement.

### Recommendation 2 - gate per-file `Instant::now` timing wrappers

`prepare_pending_acl` (`protocol_io.rs:518-527`) and
`encode_and_send_segment` (`protocol_io.rs:431-442`) wrap empty
no-op bodies in `Instant::now` pairs. Skip the timer when the
inner work is a no-op, or gate the diagnostic counters behind
`cfg(feature = "diagnostic-counters")`.

- Cost: ~30 lines. Risk: low (counters are diagnostic-only).
- Win: ~50-100 ns/file on no-op paths. 500 files: ~25-50 us.

### Recommendation 3 - reuse `ChecksumVerifier` across files

`stream_whole_file_transfer` and `compute_file_checksum` each build a
fresh verifier per file (`delta.rs:217`, `delta.rs:298`). Move
construction into `run_transfer_loop` once per session (mirroring
upstream's static `sum_init` pattern) and add a `reset()` method.

- Cost: ~40 lines. Risk: medium (must match each digest crate's initial
  state exactly).
- Win: ~50-100 ns/file allocator savings. 500 files: ~25-50 us.

### Recommendation 4 - skip `BufReader` for files known to fit one read

For files < 4 KiB (likely the entire DIS-1 corpus),
`BufReader::with_capacity(4 KiB)` allocates a buffer it never needs to
refill. Use the `stream_buf` directly via a `Cursor` adapter or lower
`SMALL_BUFFER_SIZE` to ~1 KiB.

- Cost: ~30 lines. Risk: medium (changes read-pattern shape; needs
  interop confirmation that wire output is unchanged).
- Win: ~100-200 ns/file. 500 files: ~50-100 us.

### Recommendation 5 - extend io_uring read path to small files (IUD-5/6)

Currently io_uring read is only used for files >= 1 MiB
(`context.rs:411`). IUD-5/6 already shipped the data-path machinery;
extending it via a shared session reader with registered files would
let the sender issue all 500 reads as one batched submission. **This is
speculative**: on 500 x 1 KiB cold-cache reads, the syscall floor is
one `read(2)` per file regardless of API; io_uring wins only ~1-2 us of
context-switch cost per file (~0.5-1 ms across 500 files).

- Cost: ~150 lines + a SqPoll-aware path through
  `fast_io::reader_from_path_with_depth` for < 1 MiB files via fixed
  buffers. Risk: high.
- Win: ~0.5-1 ms speculative. **Gated** on the shared io_uring ring
  work first - per-file rings would regress this path. Cross-reference
  project memory `io_uring shared_ring bottleneck` (the current
  `Arc<Mutex>` shared ring is itself a bottleneck; per-thread rings
  must land before any small-file extension is worthwhile).

### Combined estimate

Recommendations 1+2+3 together: ~75-175 us saved on the 500-file
cold-start path with zero wire-format risk and ~80 lines of change.
That is ~1% of the 1.35 s DIS-1 gap - well below DIS-4.a's 200-500 ms
signal-poll fix and DIS-4.d's 20-50 ms allocator-arena win, but it is
**fix-by-touching-only-this-row** and unblocks no other work.
Recommendation 4 adds another ~50-100 us at slightly higher risk.
Recommendation 5 is **deferred** behind the shared io_uring ring work.

## 8. What DIS-6 should re-measure under `perf`

Before sequencing the above fixes:

- Per-file `malloc` count on row 22 should come out to **~3-5** (path
  display String, BufReader buffer, ChecksumVerifier state, optional
  delta-pipeline allocs). If higher, Recommendation 3 jumps to the top.
- Confirm `flush_with_count` (`transfer_loop.rs:131`) actually fires
  per iteration. If kernel TCP_CORK / Nagle coalesces, the extra
  syscall is cheap and Recommendation 5's io_uring batching is moot.
- Confirm the first MSG_DATA on the wire batches ~30 files (the
  buffer-fill point in section 3.3). If it batches fewer, the
  `flush_with_count` audit needs to widen its scope.

`tcpdump -X -i lo port 873` on rsync-profile confirms the actual
MSG_DATA framing pattern. `perf record -F 999 -g --call-graph fp`
quantifies the per-file alloc count.

## 9. File index

Direct evidence files cited above (paths relative to worktree root):

- `crates/transfer/src/generator/protocol_io.rs`
- `crates/transfer/src/generator/transfer/transfer_loop.rs`
- `crates/transfer/src/generator/delta.rs`
- `crates/transfer/src/generator/open_source.rs`
- `crates/transfer/src/generator/context.rs`
- `crates/transfer/src/generator/diagnostics.rs`
- `crates/transfer/src/adaptive_buffer.rs`
- `crates/protocol/src/multiplex/writer.rs`
- `crates/protocol/src/multiplex/io/send.rs`
- `crates/protocol/src/flist/write/mod.rs`
- `crates/matching/src/generator.rs`
- `docs/audits/dis-3-cold-start-phase-decomposition.md` (parent task)
- `docs/audits/dis-4a-rsyncd-greeting-overhead.md` (cross-reference)
- `docs/audits/dis-4d-flist-build-cold-start.md` (boundary audit)
- `target/interop/upstream-src/rsync-3.4.1/sender.c` (upstream
  `send_files`, `receive_sums`, `write_ndx_and_attrs`)
- `target/interop/upstream-src/rsync-3.4.1/match.c` (upstream
  `match_sums`, `hash_search`)
- `target/interop/upstream-src/rsync-3.4.1/flist.c` (upstream
  `send_file_list`)
