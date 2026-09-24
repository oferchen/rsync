# Receiver-side INC_RECURSE conversion plan (RS-1)

Status: design note gating RS-2..RS-4. No code changes in this stage.
Sibling: `docs/design/lazy-sender-inc-recurse.md` (LS-1) designs the SENDER
half of the same win; read it first - this note reuses its structure and the
two share one wire contract (Section 6).

> ⚠ REVISED 2026-09-24 (RS-1r) - see Sections 8-10. The original RS-3 framing
> ("route the pipelined drivers off `ensure_all_segments_loaded`, move the
> per-segment `NDX_DONE`/reclaim into the per-file loop") is SUPERSEDED as a
> standalone step: it is unsound without a prerequisite the note missed. oc's
> transfer RESPONSE read is FIFO-positional and NOT marker-aware, so it desyncs
> on the sub-list segment markers that necessarily interleave with file
> responses once the receiver requests files while the sender is still streaming
> segments (any source tree larger than `MAX_FILECNT_LOOKAHEAD` = 10000 files).
> Section 8 adds the CORRECTNESS FOUNDATION (a marker-aware / NDX-addressed
> transfer read) the relocation sits on top of; Section 9 designs the
> SEGMENT-LENGTH performance layer (the broadened end goal) and settles the
> computed-vs-wire-propagated length decision; Section 10 gives the revised task
> decomposition that replaces Section 6's staging.

## 1. Problem

Under INC_RECURSE upstream's RECEIVING side (generator) never holds the whole
file list resident. It receives the initial list, then pulls one sub-list
segment at a time as its per-file cursor reaches the end of the current one,
and frees each segment the moment the receiver is done with it. Resident
memory is O(in-flight segments) plus the O(#dirs) `dir_flist`, not O(N).

oc's receiver does the opposite in the two code paths that matter:

1. On a PUSH into oc (oc is the server-receiver: daemon or ssh), oc as the
   server writes CF_INC_RECURSE = 0 unconditionally
   (`crates/transfer/src/lib.rs:428-429`), so INC_RECURSE is never negotiated
   and the receiver builds and holds the entire flat list.
2. On a PULL by oc (oc is the client-receiver, remote advertises `i`),
   INC_RECURSE is negotiated and the segment machinery is exercised, but the
   flat `file_list` Vec (`crates/transfer/src/receiver/context.rs:48`) is only
   heap-trimmed per segment (`reclaim_oldest_segment`, `context.rs:893-915`),
   never shortened, and two of the three drivers drain every segment up front
   (`ensure_all_segments_loaded`, `pipelined.rs:77`,
   `pipelined_incremental.rs:74`). Peak stays O(N).

This is the receiving-side twin of RSS problem #198 (measured 25.9x upstream
at 1M files). RS-2/RS-3/RS-4 close it; #102 (enable receiver INC_RECURSE) sits
on top of this design.

## 2. Upstream model (rsync 3.4.4, protocol 32)

### 2.1 Data structures (shared with the sender)

- Three globals `cur_flist`, `first_flist`, `dir_flist`
  (`// upstream: flist.c:101`, extern in `generator.c:101`). Received
  transfer lists form a linked ring `first_flist -> ... -> cur_flist`;
  `dir_flist` is a separate FLIST_TEMP list holding only directory entries,
  alive for the whole run - the accepted O(#dirs) floor.
- Segment NDX chaining: `flist->ndx_start = prev->ndx_start + prev->used + 1`
  (`// upstream: flist.c:2966`) - the +1 NDX gap between segments, identical
  to the sender's numbering so both sides address entries by the same NDX.

### 2.2 Initial list - `recv_file_list(f, -1)`

`recv_file_list` (`// upstream: flist.c:2596`) is called once with
`dir_ndx = -1` from the receiving entry points
(`// upstream: main.c:1201` server-recv, `main.c:1379` client-pull).

- Under INC_RECURSE it allocates the FLIST_TEMP `dir_flist` when
  `flist->ndx_start == 1` (`// upstream: flist.c:2638-2645`) and remembers
  `dstart = dir_flist->used`.
- Each received directory entry is appended to `dir_flist` as well as the
  transfer list (`// upstream: flist.c:2699-2705`, the `S_ISDIR` branch) -
  this is what populates the per-run directory pool that later sub-list
  headers index by `dir_ndx`.
- The list is sorted (`fsort` on the `sorted` view, `// upstream:
  flist.c:2739-2758`), `flist_done_allocating(flist)` records the pool
  boundary (`// upstream: flist.c:2759`), and the clean/dedup pass runs -
  `flist_sort_and_clean` with the receiver-active branch
  `if (!am_sender || inc_recurse)` (`// upstream: flist.c:3031`) that
  tombstones duplicate names via `clear_file` without renumbering.
- `flist_eof` is NOT set under INC_RECURSE (contrast the non-inc branch at
  `// upstream: flist.c:2762-2766`); it stays clear until the terminator
  arrives in the sub-list stream.
- A 1-entry initial list triggers one eager extra list
  (`recv_additional_file_list`, `// upstream: main.c:1207,1381`) to detect a
  1-file transfer, mirroring the sender's eager first extra
  (`// upstream: flist.c:2585`).

### 2.3 On-demand segment pull - `recv_file_list(f, dir_ndx)`

The generator's per-segment loop drives reception. `generate_files`
(`// upstream: generator.c:2299`) is a `do { ... } while ((cur_flist =
cur_flist->next) != NULL)` over segments:

- For each segment it walks `cur_flist->low..=cur_flist->high` calling
  `recv_generator` per entry (`// upstream: generator.c:2329-2356`), then at
  the bottom, if `!flist_eof && !cur_flist->next`, calls `wait_for_receiver`
  (`// upstream: generator.c:2360-2368`).
- `wait_for_receiver` (`// upstream: io.c:1750-1786`) reads one NDX. A
  `NDX_FLIST_OFFSET`-framed value dispatches to `recv_file_list(f, ndx)`
  which appends a new segment and sets `flist->parent_ndx = ndx`; a
  `NDX_FLIST_EOF` sets `flist_eof`; a `NDX_DONE` bumps `msgdone_cnt`.
- Inside `recv_file_list(f, dir_ndx>=0)` the header's `dir_ndx` is range- and
  duplicate-checked against `dir_flist` before any entry is trusted:
  `dir_ndx >= dir_flist->used` aborts RERR_PROTOCOL
  (`// upstream: flist.c:2622-2626`); a second sub-list for the same dir
  (FLAG_GOT_DIR_FLIST) aborts RERR_PROTOCOL
  (`// upstream: flist.c:2627-2632`); every entry's dirname must match
  `f_name(dir_flist->files[dir_ndx])` or it aborts RERR_UNSUPPORTED
  ("ABORTING due to invalid path from sender", `// upstream:
  flist.c:2719-2730`).
- Hardlink look-ahead: with `preserve_hard_links && inc_recurse` the loop
  pre-reads segments while `file_total < MIN_FILECNT_LOOKAHEAD/2`
  (`// upstream: generator.c:2300-2305`) so a follower's leader in a later
  segment resolves before hardlinking.

### 2.4 Freeing - `flist_free(first_flist)` (the RSS mechanism)

`check_for_finished_files` (`// upstream: generator.c:2219-2239`) is the
receiver's free point. When `cur_flist != first_flist` and the oldest list has
no work outstanding (`!first_flist->in_progress && !first_flist->to_redo`), it
writes `NDX_DONE` for that list, touches up its parent dir, and calls
`flist_free(first_flist)` (`// upstream: generator.c:2239`,
`flist.c:2980`), which unlinks the oldest ring entry and releases its pool
extent. `dir_flist` is never freed mid-run. Resident received-list memory is
therefore O(in-flight segments), never O(total).

### 2.5 The FOUR receiver conditions for INC_RECURSE

`set_allow_inc_recurse` (`// upstream: compat.c:162-180`) is the sole gate for
`allow_inc_recurse`. On the RECEIVING side all four must hold for it to stay 1
(pinned from the C source, not guessed):

1. **protocol_version >= 30.** `inc_recurse` is only ever assigned inside the
   `else if (protocol_version >= 30)` arm of the compat-flag exchange
   (`// upstream: compat.c:711,746`); below 30 it is 0 and no CF_INC_RECURSE
   bit exists.
2. **recurse && !use_qsort.** `if (!recurse || use_qsort) allow_inc_recurse
   = 0` (`// upstream: compat.c:172-173`). (`-r`/`--recursive` implies
   `xfer_dirs`; the gate itself tests `recurse`.)
3. **No receiver-disabling flags.** On the receiving side (`!am_sender`),
   `delete_before || delete_after || delay_updates || prune_empty_dirs`
   clears it: `else if (!am_sender && (delete_before || delete_after ||
   delay_updates || prune_empty_dirs)) allow_inc_recurse = 0`
   (`// upstream: compat.c:174-176`). Note `delete_during` is NOT in the set -
   inc_recurse forces delete-before to delete-during precisely so it stays
   compatible.
4. **Peer advertised `i` (server side).** `else if (am_server &&
   strchr(client_info, 'i') == NULL) allow_inc_recurse = 0`
   (`// upstream: compat.c:178-179`). `client_info` is the peer's `-e`
   capability string (`// upstream: compat.c:163-169`); the receiving server
   only enables inc-recurse if the pushing client advertised the `i`
   capability.

The server then folds `allow_inc_recurse` into the wire:
`compat_flags = allow_inc_recurse ? CF_INC_RECURSE : 0`
(`// upstream: compat.c:713`, bit at `rsync.h:118`), and BOTH sides set
`inc_recurse = compat_flags & CF_INC_RECURSE ? 1 : 0`
(`// upstream: compat.c:746`).

## 3. CF_INC_RECURSE negotiation - where upstream sets it, where oc does not

### 3.1 Upstream

- `set_allow_inc_recurse()` is called only when `am_server`
  (`// upstream: compat.c:597-598`), so the SERVER is the sole author of the
  bit regardless of transfer direction.
- Server writes it: `write_varint(f_out, compat_flags)`
  (`// upstream: compat.c:739`), with the pre-release `V` peer using
  `write_byte` (`// upstream: compat.c:737`).
- Client reads it: `compat_flags = read_varint(f_in)`
  (`// upstream: compat.c:741`).
- Both derive `inc_recurse` (`// upstream: compat.c:746`); a batch that
  demands inc_recurse the local side can't allow aborts
  (`// upstream: compat.c:769-774`).

So for a PUSH the receiving server sets the bit (all four conditions checked
with `am_server` true); for a PULL the sending server sets it and the
receiving client just reads it.

### 3.2 oc today

oc's decision is `compute_allow_inc_recurse`
(`crates/transfer/src/lib.rs:428-429`):

```rust
recursive && !qsort && role == ServerRole::Generator
```

The `role == ServerRole::Generator` clause is the divergence: the Receiver
role (`crates/transfer/src/role.rs:15`) never returns true, so a server-
receiver always advertises CF_INC_RECURSE = 0. The value flows through
`allow_inc_recurse` into `ProtocolSetupConfig` (`lib.rs:643-644,675`) and out
via `exchange_compat_flags_direct` / `build_compat_flags_from_client_info` /
`write_compat_flags` (`crates/transfer/src/setup/compat.rs:73-95`). The
CF_INC_RECURSE bit itself already exists
(`crates/protocol/src/compatibility/flags.rs:34`) and the client-read path is
correct; only the server's own advertisement is suppressed.

The existing comment on `compute_allow_inc_recurse` names the reason: the
receiver historically "drains the entire sub-list stream upfront, which
deadlocks against upstream's MIN_FILECNT_LOOKAHEAD-throttled
send_extra_file_list on source trees larger than the lookahead window." RS-3
removes that reason by making the receiver pull-and-free per segment; RS-2
then lifts the role restriction.

### 3.3 What RS-2 (#205) must change

- Replace the `role == ServerRole::Generator` clause with upstream's exact
  receiver predicate: allow INC_RECURSE for the Receiver role too when
  conditions 1-4 hold. Concretely: `protocol >= 30 && recursive && !qsort`,
  AND for the Receiver role additionally `!(delete_before || delete_after ||
  delay_updates || prune_empty_dirs)` (map to
  `config.late_delete`/`delete_after`/`delay_updates`/`flags.prune_empty_dirs`
  and the delete-before mode), AND (server side) the peer advertised `i` -
  which oc already parses into `client_info` inside
  `build_compat_flags_from_client_info`.
- The Generator (sending-server) branch stays exactly as today; the Sender
  side is governed by the LS chain and the same predicate.
- Wire-observable effect: a PUSH into an oc daemon/ssh server now negotiates
  CF_INC_RECURSE = 1 whenever an upstream server would, byte-identical in the
  compat_flags varint. This is the ONLY wire change in the whole RS chain and
  it is exactly upstream parity - #207 verifies it against real upstream
  bytes.

Guard: keep the batch-consistency abort (upstream `compat.c:769-774`) - if a
batch header implies inc_recurse but the four conditions fail locally, error
out rather than silently downgrade.

## 4. Per-segment recv + apply + free (the RSS win, RS-3 / #206)

Most of the machinery already exists on the receiver; RS-3 finishes wiring it
and makes the free real.

### 4.1 What already exists

- Segment table `ndx_segments: Vec<(flat_start, ndx_start)>`
  (`context.rs:60-67`), grown per sub-list (`receive.rs:382`), with the +1 NDX
  chaining mirrored from `// upstream: flist.c:2966`.
- The `dir_flist` analog: `dir_flist_used` (`context.rs:94`),
  `served_dir_flists` (`context.rs:105`), `dir_flist_names`
  (`context.rs:118`) - the O(#dirs) floor, carrying upstream's range /
  duplicate / path-belongs guards (`// upstream: flist.c:2622-2632,2719-2730`)
  with matching fail-closed tests in `on_demand.rs`.
- Cross-segment reader state: `flist_reader_cache` (`context.rs:125`) keeps
  the entry-decode continuation (prev_name/mode/uid/gid) across sub-lists,
  matching upstream's `static` vars in `recv_file_entry`.
- On-demand pull primitives (`on_demand.rs`): `read_next_frame` (:67-86),
  `ensure_flat_idx` (:104-128, the lazy per-index pull mirroring
  `// upstream: generator.c:2299-2368`), `prefetch_for_hardlinks` (:162-177,
  mirroring `// upstream: generator.c:2300-2305`), plus the NDX<->flat maps
  `wire_to_flat_ndx` / `flat_to_wire_ndx` (`context.rs:541-596`) that already
  resolve through the segment table, not a flat scan.
- The synchronous driver `sync.rs:130-140` already walks by `ensure_flat_idx`,
  so it pulls segments on demand.
- `first_segment_idx` (`context.rs:78`) and the per-segment `NDX_DONE` +
  `reclaim_oldest_segment` loop (`phases.rs:49-59`) mirror upstream's
  `first_flist` advance and `flist_free` call
  (`// upstream: generator.c:2226,2239`).

### 4.2 What holds the list resident and must change

1. **`reclaim_oldest_segment` trims, never frees.** It calls
   `entry.reclaim_heap_data()` over the retired range
   (`context.rs:911-913`) but leaves every fixed-size `FileEntry` struct and
   the `file_list` Vec capacity in place - residual stays O(N). Upstream's
   `flist_free` releases the whole pool extent
   (`// upstream: flist.c:2980`). RS-3: make the retired segment's storage
   actually reclaimable. Because NDX resolution already goes through
   `ndx_segments` + `first_segment_idx` (not raw `file_list` indexing), the
   retired prefix can be dropped and the live window compacted, or the entries
   replaced by a zero-size tombstone that carries no heap and no extras -
   whichever keeps `flat_to_wire_ndx` / `wire_to_flat_ndx` exact for the LIVE
   window. The invariant to preserve: an index below
   `ndx_segments[first_segment_idx].0` is never dereferenced.
2. **Reclaim fires too late.** It runs only in `exchange_phase_done`
   (`phases.rs:49-59`), after the whole per-file loop. Upstream frees inside
   the transfer loop as each list completes
   (`// upstream: generator.c:2219-2239`, called per entry via
   `check_for_finished_files` at `generator.c:2346`). RS-3: move the free to
   fire as the per-file cursor crosses a segment boundary in `sync.rs`, so the
   resident window is bounded DURING the transfer, not just trimmed at the end.
3. **Two drivers still drain everything.** `pipelined.rs:77` and
   `pipelined_incremental.rs:74` call `ensure_all_segments_loaded`, which
   pulls every segment before transferring - O(N) by construction and the
   original deadlock risk. RS-3: route these through the same on-demand
   `ensure_flat_idx` cursor as `sync.rs`, or bound their prefetch to the
   lookahead window. `ensure_all_segments_loaded` should survive only as the
   explicit non-INC_RECURSE / list-only fallback.

### 4.3 Retention target

After RS-3 the resident received-list memory is O(in-flight segments) +
O(#dirs) `dir_flist_names`, bounded by the same MIN_FILECNT_LOOKAHEAD window
upstream uses (`rsync.h:151-152`), not O(N). This is the pull-side twin of the
sender's O(segment) target in LS-1 Section 7.

### 4.4 Pacing / backpressure

Reuse the existing on-demand cursor as pure backpressure: a segment is pulled
only when `ensure_flat_idx` needs an index the current window does not cover,
and freed as the cursor leaves it. No controller, no new tuning knob - the
per-file loop rate is the flow-control signal, exactly as upstream's
`generate_files` loop paces `wait_for_receiver`. Upstream's idle-time
MIN/2 hardlink prefetch is already mirrored (`prefetch_for_hardlinks`); the
MAX-lookahead idle fill (`// upstream: io.c:753-758`) is a pre-existing gap
shared with LS-1 and stays out of scope.

## 5. Wire-identity risk (what must stay byte-identical)

RS-3 changes only WHEN oc reads a segment and WHEN it frees one - never the
bytes. What must stay identical, verified by #207:

- **Segment framing.** Each sub-list header is `NDX_FLIST_OFFSET - dir_ndx`
  (`// upstream: flist.c:2152`); the receiver only ever READS these (via
  `read_next_frame`), so the change is read-timing only. The one exception is
  RS-2's compat_flags bit (Section 3.3).
- **NDX signaling.** The +1 gaps between segments (`ndx_start = prev + used +
  1`) and the flat<->wire NDX maps must stay exact for the live window; the
  per-file reply NDX the receiver writes back is unchanged.
- **Per-segment done / goodbye sequence.** The per-segment `NDX_DONE`
  (`phases.rs:56`, `// upstream: generator.c:2226`), the phase `NDX_DONE`s,
  and the proto-31 `NDX_DEL_STATS` goodbye must keep their count and order.
  Freeing a segment earlier must not change how many `NDX_DONE`s cross the
  wire - one per received segment, then the phase markers.
- **Terminator.** `NDX_FLIST_EOF` placement and `flist_eof` semantics
  (`context.rs:264`) unchanged.
- **io_error propagation and flist stats** (`stats.flist_size`, file/dir
  counts) unchanged.

### How RS-4 (#207) validates

- Byte capture: oc-before vs oc-after vs upstream 3.4.4, both directions
  (PUSH into oc server, PULL by oc client) across local, daemon, and ssh.
  Segment content/order, sub-list headers, +1 gaps, per-segment `NDX_DONE`s,
  and `NDX_FLIST_EOF` must match. The `on_demand.rs` real-upstream-frame test
  (the captured `UPSTREAM_INC_RECURSE_FRAME`) is the seed; extend to a tree
  whose segment count exceeds the lookahead window so freeing actually fires
  mid-transfer.
- RSS: 1M-file tree (containerized, non-bind-mounted data dir per the repo
  container safety rule), both directions, receiver peak RSS O(segment window),
  parity-class with upstream's flat profile.
- Behavior: full nextest + upstream-testsuite + interop green; hardlinks,
  `--iconv` (the `iconv_reorder_suppressed` unsorted path,
  `incremental.rs:55`), `--delete-during`, `--itemize-changes`, and daemon
  module paths exercised under INC_RECURSE.

## 6. Staging, ownership, and blast radius

| Task | Owns |
|------|------|
| RS-2 (#205) | CF_INC_RECURSE negotiation both directions: extend `compute_allow_inc_recurse` (`lib.rs:428`) to the Receiver role with the four upstream conditions; keep the batch-consistency abort. One wire change (the compat_flags bit for PUSH), exactly upstream parity. |
| RS-3 (#206) | Per-segment recv+apply+free: make `reclaim_oldest_segment` a real free, move it into the `sync.rs` per-file loop at segment boundaries, and route `pipelined.rs` / `pipelined_incremental.rs` off `ensure_all_segments_loaded`. Wire-neutral. |
| RS-4 (#207) | Validation: wire-identity capture + 1M-file RSS both directions + full test matrix. |

### Interaction with the lazy SENDER chain (LS #200-202)

The two halves are symmetric but do NOT share code: the sender machinery lives
in `crates/transfer/src/generator/` (`DirQueue`, `SegmentScheduler`,
`PendingSegment`); the receiver machinery lives in
`crates/transfer/src/receiver/` (`ndx_segments`, `dir_flist_*`, `on_demand`).
They share exactly ONE thing: the wire contract in Section 5 (segment framing,
+1 NDX gaps, `NDX_FLIST_EOF`, per-segment `NDX_DONE`). Because a LOCAL transfer
runs a sender and a receiver in one process, a combined LS-3 + RS-3 local run
is the true end-to-end RSS test - each side must independently show O(segment).
RS and LS can land independently; neither blocks the other, and both must keep
the shared wire bytes fixed.

### Cross-crate blast radius

- `transfer` crate: the receiver subtree (RS-3) and the negotiation predicate
  in `lib.rs` (RS-2). Bulk of the change.
- `protocol` crate: none - CF_INC_RECURSE, the NDX codec, and the flist
  reader already exist.
- `core` crate: none beyond passing `ServerRole` through, which it already
  does.
- `daemon` crate: no code change, but RS-2 flips the observable behavior of a
  PUSH into an oc daemon (now inc-recursive). This is the path #102 depends on
  and the one #207 must cover with real upstream bytes.

## 7. Invariants each stage preserves

- Wire fidelity first: identical segment content and order, per-segment and
  phase `NDX_DONE` counts, `NDX_FLIST_EOF` placement, io_error propagation,
  and flist stats. The single intended wire delta is RS-2's CF_INC_RECURSE
  bit, which is upstream parity.
- The non-INC_RECURSE receiver path is unchanged in every stage
  (`ensure_flat_idx` / `reclaim_oldest_segment` are already no-ops once
  `flist_eof` is set on entry).
- The INC_RECURSE gate stays exactly upstream's predicate
  (`compat.c:162-180`); RS-2 adds the receiver arm upstream already has, it
  invents no new capability or advertisement.
- The fail-closed sub-list guards (range, duplicate, path-belongs;
  `// upstream: flist.c:2622-2632,2719-2730`) are preserved verbatim - RS-3
  changes retention, not validation.
- Existing observable divergences are not silently changed by an RS stage;
  any fix there is a separate, wire-verified task.

## 8. Correctness foundation: the marker-aware / NDX-addressed transfer read (RS-3a)

### 8.1 The defect the original RS-3 framing missed

RS-3 (Section 4) assumed the pipelined drivers could simply pull segments on
demand through `ensure_flat_idx` and free them per segment. That is necessary
but not sufficient. The blocker is in the TRANSFER read, not the flist read:

- The receiver's transfer response loop reads each file reply positionally.
  The decoupled pipeline pops the oldest in-flight request and reads its reply
  via `process_file_response_streaming`
  (`crates/transfer/src/receiver/transfer/pipeline.rs:875`, FIFO-positional
  window in `InFlightRequests`, `pipeline.rs:88`); the synchronous driver reads
  one reply per request via `SenderAttrs::read_with_codec_xattr`
  (`crates/transfer/src/receiver/transfer/sync.rs:262`). NEITHER dispatches an
  `NDX_FLIST_OFFSET` sub-list marker or a spontaneous `NDX_DONE` echo that
  arrives between file replies - both are read as a file-reply NDX and desync
  the stream ("multiplexed frame truncated", exit 12).
- The interleave is UNAVOIDABLE for a tree larger than the lookahead window.
  The sender pushes sub-list segments proactively at the top of every send
  iteration (`crates/transfer/src/generator/transfer/transfer_loop.rs:612`
  `send_extra_file_lists` -> refill to `MIN_FILECNT_LOOKAHEAD`; `:646`
  `grow_lookahead_while_idle` -> `MAX_FILECNT_LOOKAHEAD` when idle), matching
  upstream `io.c:753-774`. Once the receiver starts issuing NDX requests while
  the sender is still emitting segments, segment markers land BETWEEN file
  replies on the one multiplexed stream.
- The current batch receiver avoids the interleave only by reading the ENTIRE
  flist first (segments-only phase, the eager
  `ensure_all_segments_loaded`/`ensure_flat_idx`-to-`flist_eof` drain at
  `pipelined_incremental.rs:84-88` and `pipelined.rs:88`) and THEN transferring
  (replies-only phase). That is exactly why it works at
  <= `MAX_FILECNT_LOOKAHEAD` and DEADLOCKS beyond it: the eager drain blocks on
  the next segment the parked sender will not send without an `NDX_DONE` the
  eager path never emits mid-walk. `MIN_FILECNT_LOOKAHEAD = 1000`,
  `MAX_FILECNT_LOOKAHEAD = 10000` (`rsync.h:151-152`, mirrored in
  `crates/transfer/src/generator/segments.rs:28,46`); the sender's throttle is
  advanced only by inbound `NDX_DONE` -> `retire_current_flist`
  (`transfer_loop.rs:694-701`, `segments.rs:235`; upstream io.c:753-754 gates on
  `file_total - file_old_total < MAX_FILECNT_LOOKAHEAD`).

### 8.2 Upstream does it with ONE marker-aware read

Upstream has no separate "flist read" and "reply read". `read_ndx_and_attrs`
(`// upstream: rsync.c:322-433`) is a single dispatch loop:

- `read_loop` (rsync.c:330-381): read one NDX. `ndx >= 0` -> break out and
  handle a file reply. `NDX_DONE` -> return it. `NDX_DEL_STATS` -> drain and
  continue. `NDX_FLIST_EOF` -> set `flist_eof`, forward, continue.
  `NDX_FLIST_OFFSET`-framed -> `recv_file_list(f_in, ndx)` appends a new segment
  and continues. So segment markers, del-stats, EOF, and `NDX_DONE` are ALL
  consumed inline, interleaved with file replies, on the one stream.
- After the loop the file NDX is resolved NDX-ADDRESSED, not positionally:
  `flist = flist_for_ndx(ndx, ...)` (rsync.c:393) walks the segment ring to the
  owning list and advances `cur_flist` (rsync.c:394-401). The reply is matched
  to its entry by index, so an out-of-order or cross-segment reply is correct by
  construction.
- The generator's free point `check_for_finished_files`
  (`// upstream: generator.c:2219-2239`) and the generate loop
  (`generator.c:2299-2368`) call into the same read via `wait_for_receiver`
  (`// upstream: io.c:1750-1786`), so freeing a segment and pulling the next are
  the same interleaved read.

### 8.3 oc already has the primitive - it is just not on the transfer path

The marker-aware dispatch EXISTS on the receiver and mirrors upstream:

- `read_ndx_step` (`crates/transfer/src/receiver/ndx_stream.rs:364`) returns
  `NdxStep::{File, Done, DelStats, FlistEof, Segment}` and appends a segment
  through its sink - the direct analog of `read_ndx_and_attrs`'s `read_loop`.
- `read_marker_aware_ndx` (`ndx_stream.rs:428`) and `read_ndx_and_attrs`
  (`ndx_stream.rs:463`) already wrap it; `phases.rs` and the goodbye path use
  them.
- `on_demand.rs:read_next_frame` (`:55`) already drives `read_ndx_step` to
  append INC_RECURSE segments during `ensure_flat_idx`.
- NDX-addressed resolution already exists: `wire_to_flat_ndx` /
  `flat_to_wire_ndx` (`context.rs:823,856`) resolve through the `ndx_segments`
  table, and `InFlightRequests::retire_by_ndx` (used on the `MSG_NO_SEND`
  decline path, `pipeline.rs`) already matches an in-flight entry by wire NDX.

So RS-3a is a ROUTING change, not a new subsystem: make the transfer response
read dispatch through `read_ndx_step` (so an interleaved `Segment`/`FlistEof`/
`DelStats`/`Done` is handled inline, growing `file_list` via the same sink) and
resolve a `File(ndx)` reply to its in-flight request via `wire_to_flat_ndx` +
`retire_by_ndx` instead of a blind FIFO `pop`. This is the primitive the
relocation in Section 4 was silently assuming.

### 8.4 Isolation and byte-neutrality

On a non-INC_RECURSE transfer `flist_eof` is set on entry, no segment markers
ever appear in the reply stream, and every `read_ndx_step` result is `File` -
so the marker-aware read is behaviourally identical to the positional read on
the live path today. The change touches the SHARED transfer read path, so
byte-identity is a hard gate (Section 10, verified against #7953 both fixtures),
but the marker branches are inert whenever `flist_eof` holds at entry.

### 8.5 Scope: pipelined drivers only

RS-3a converts the two PRODUCTION drivers' reply reads: `run_pipelined`
(`process_file_response_streaming`, `pipeline.rs:875`) and, through it,
`run_pipelined_incremental`. The `run()` dispatcher only ever reaches these two
(`transfer.rs:50-68`: `run_pipelined_incremental` with the `incremental-flist`
feature, else `run_pipelined`). The synchronous `run_sync` (`sync.rs:262`) has
NO production call site - it is exercised only by tests - so it is left as-is
(surgical: do not touch dead code). If `run_sync` is ever revived for
production, it converts to the same marker-aware reply read; that is a note, not
a task in this chain.

## 9. Segment-length performance layer (the broadened end goal, RS-3c)

The end goal is not only deadlock-free correctness but USING each sub-list
segment's length for performance. This section designs that and settles where
the length comes from.

### 9.1 Where the length comes from - computed vs wire-propagated

Upstream does NOT put a segment length on the wire. `recv_file_list` reads
entries until a zero-flag terminator (`// upstream: flist.c:2653`
`if ((flags = read_varint(f)) == 0) break;`, written by `write_end_of_flist`,
`// upstream: flist.c:2112`); the segment's length is `flist->used`, KNOWN ONLY
once the terminator is read. The NDX chaining `flist->ndx_start = prev->ndx_start
+ prev->used + 1` (`// upstream: flist.c:2966`) likewise uses the computed
`used`. oc mirrors this: each segment boundary is recorded in `ndx_segments`
(`context.rs:60-67`) as `receive_one_extra_segment` reads to the terminator
(`receive.rs:297`).

- **Computed length (RECOMMENDED, byte-neutral, works vs upstream):** the
  receiver derives each segment's length as `end - start` from `ndx_segments`
  the instant the terminator is read. Available BEFORE that segment's transfer
  phase, which is where the perf wins live. Wire-identical; correct against a
  real upstream sender. This is what Section 9.2 uses.
- **Wire-propagated length (a BUILT opt-in oc capability - user decision
  2026-09-24):** the sender already knows each segment's size a priori
  (`PendingSegment.count` / `SegmentScheduler.lookahead_total`,
  `crates/transfer/src/generator/segments.rs:149,222`), so it emits the count in
  the sub-list header ONLY when the capability is negotiated, letting the
  receiver pre-size the `file_list` extent before decoding entries and
  cost-weight prefetch before pulling. It has NO upstream counterpart, so per
  the mirror-upstream policy it is default-off, negotiated, and byte-identical
  when off. Designed exactly like the existing `CONSECUTIVE_MATCH` extension
  (Section 9.1.1); this is part of RS-3c, on top of the computed baseline
  (which remains the always-on behaviour whenever the capability is off or the
  peer is upstream).

### 9.1.1 Wire-propagated segment length - capability design (mirrors CONSECUTIVE_MATCH)

The precedent is `CONSECUTIVE_MATCH` (design note
`docs/design/zsync-inspired-matching.md`; env `docs/oc-extension-env-reference.md`);
mirror it exactly:

- **Private `-e` capability letter.** Advertised by the client in the `-e.<...>`
  string, like `CONSECUTIVE_MATCH_CHAR = 'Z'` (`setup/capability.rs:129`).
  Proposed letter `'N'` (segment couNt/leNgth) - it MUST avoid every upstream
  letter (`i L s f x C I v u`, `capability.rs:41-112`) and oc's `'Z'`; the exact
  glyph is finalized at implementation from the free set. Upstream ignores
  unknown `-e` letters (`compat.c` only `strchr`s its own), so the advert is
  inert against upstream.
- **Private compat bit EXCLUDED from `KNOWN_MASK`.** A new
  `CompatibilityFlags::SEGMENT_LENGTH` at the next free private bit
  (`0x0400_0000`, alongside `CONSECUTIVE_MATCH = 0x0200_0000`), deliberately kept
  out of `KNOWN_MASK` (`crates/protocol/src/compatibility/flags.rs:56-73`) so it
  is never advertised unconditionally or accepted from an upstream peer.
- **Default-off env, both-peers opt-in.** An `OC_RSYNC_SEGMENT_LEN=1` env gate
  (mirroring `consecutive_match_opt_in()` / `OC_CONSECUTIVE_MATCH`,
  `capability.rs:141`; folds under the planned `OC_RSYNC_PEER` umbrella with
  `OC_CONSECUTIVE_MATCH`). The server sets the bit ONLY when it sees the peer's
  letter AND is itself opted in (`capability.rs:385-392` pattern), so the
  extension engages only oc-to-oc with both ends opted in.
- **Wire format when negotiated.** After the sub-list header
  `write_ndx(NDX_FLIST_OFFSET - dir_ndx)` and before the first entry, the sender
  writes the segment entry count as a varint; the receiver reads it (only when
  the bit is set) to `reserve` the `file_list` extent and to size prefetch.
  When the bit is off, NOTHING extra is written or read - byte-identical to
  today and to upstream. The trailing zero-flag terminator (`flist.c:2112/2653`)
  is unchanged and remains the authority on where the segment ends; the
  propagated count is a pre-sizing hint validated against the terminator (a
  mismatch is a protocol violation, fail-closed).

### 9.2 What computed segment length buys (all byte-neutral)

1. **Per-segment work-unit pre-sizing.** With the segment's `used` known at
   terminator time, `build_files_to_transfer` (`candidates.rs:185`) run over the
   segment's flat range pre-sizes its candidate `Vec` and the parallel-stat
   batch to the segment, instead of the whole-list `len/4` guess
   (`candidates.rs:367`).
2. **Per-segment batch signature parallelism.** A full segment's file set is
   known at terminator, so the decoupled pipeline's rayon signature map
   (`pipeline.rs:700`) runs over a whole segment at once - the natural batch
   boundary - rather than an arbitrary window slice.
3. **RSS-flat retention (the Section 4.3 target).** Reclaim the oldest segment
   at each boundary crossing (`reclaim_oldest_segment`, `context.rs:1252`) so
   resident received-list memory is O(in-flight segments) + O(#dirs)
   `dir_flist`, bounded by `MIN_FILECNT_LOOKAHEAD`, not O(N).
4. **Prefetch / backpressure sizing.** The on-demand cursor pulls the next
   segment only when the transfer cursor needs an index the live window does not
   cover (RS-1 Section 4.4), and the pump after each mid-walk `NDX_DONE` is
   gated on backlog `< MIN_FILECNT_LOOKAHEAD/2` (`// upstream: generator.c:2231`)
   - both expressed in the computed segment counts.

### 9.3 Fidelity note

None of Section 9.2 changes a wire byte: computed length is a read-time
derivation of data the receiver already parses, and is the always-on baseline.
The one segment-length item that touches the wire is 9.1.1's wire-propagated
capability, which is default-off, negotiated, and byte-identical when off or
against upstream by construction (no letter advertised unless
`OC_RSYNC_SEGMENT_LEN` is set; no bit set unless both peers opt in; no varint
written unless the bit is set). The capability-off byte-identity is a hard gate
in RS-3c.

## 10. Revised decomposition (replaces Section 6 staging)

Ordered so each stage is independently landable, gated, and (except the final
negotiation flip) wire-neutral. The isolated-relocation tasks in the tracker
(A5a-3 #25-30, A5a-4 #31-34) are re-expressed here; #25-30 must not ship as the
standalone relocation.

| Stage | Owns | Gate |
|-------|------|------|
| **RS-3a - marker-aware transfer read** (correctness foundation, Section 8) | Route the transfer response read through `read_ndx_step`; dispatch interleaved `Segment`/`FlistEof`/`DelStats`/`Done`; resolve `File(ndx)` via `wire_to_flat_ndx` + `retire_by_ndx`. PIPELINED DRIVERS ONLY (`pipeline.rs:875`, reached by both `run_pipelined` and `run_pipelined_incremental`); leave `run_sync` (dead code, no production caller) untouched, per Section 8.5. No relocation yet - the eager drain stays, so this alone is wire-neutral. | #7953 PULL byte-identity base-vs-after (both fixtures) + full `transfer` nextest green + a new test feeding an interleaved segment-marker+reply stream that desyncs the positional read and passes the marker-aware read. |
| **RS-3b - lazy consumption + mid-walk NDX_DONE/reclaim** (Section 4 relocation, now sound on 3a) | Reroute `run_pipelined_incremental`/`run_pipelined` off the eager drain onto the on-demand cursor; emit `NDX_DONE` mid-walk gated on R13 (`first_flist` no `in_progress`/`to_redo`, honor R17: redo pins the segment); `reclaim_oldest_segment` + advance `first_segment_idx` mid-walk; pump `< MIN/2`; make `exchange_phase_done` loop `ndx_segments.len() - first_segment_idx` (byte-identical when `first_segment_idx==0`). Gate on `inc_recurse && !flist_eof`; exclude `--delete` (kept for RS-4). | strace NO deadlock forced-INC_RECURSE (compat-flag override, oc daemon-sender + oc client-receiver, no `--delete`) at >1024 AND >10000 files; #7953 byte-identity; `transfer` nextest. |
| **RS-3c - segment-length perf** (Section 9, computed baseline + wire capability) | (a) Computed baseline (always-on): per-segment candidate/stat pre-sizing from computed `used`, per-segment batch signature parallelism, RSS-flat retention. (b) Wire-propagated `SEGMENT_LENGTH` capability (Section 9.1.1): new `-e` letter + private compat bit outside `KNOWN_MASK` + `OC_RSYNC_SEGMENT_LEN` env, both-peers opt-in, sender writes the per-segment count varint after the sub-list header when negotiated, receiver pre-sizes `file_list` extent + prefetch; add the env to `docs/oc-extension-env-reference.md`. | Capability-OFF byte-identical: #7953 both fixtures + interop vs real upstream auto-off byte-identical. Capability-ON oc-to-oc: forced/negotiated test proves the count is read, used, and validated against the terminator. 1M-file tree (containerized, non-bind-mounted data dir), both directions, receiver peak RSS O(segment window); no throughput regression on the live single-segment path. |
| **RS-4 = A5a-4 - per-dir delete** (#31-34) | Per-dir `delete_in_dir` when a directory's sub-list is complete; make `delete_pass_flist_complete` (`transfer.rs:404`) per-dir; break the `first_segment_idx==0` coupling and drop RS-3b's `--delete` exclusion. | Bait-file delete test with opposed controls (partial-list); `--delete-during` under forced-INC_RECURSE; the `first_segment_idx==0` debug-assert tripwire never fires. |
| **RS-2 = A5b/c/d - negotiation flip** (#35-39, LAST) | Extend `compute_allow_inc_recurse` (`lib.rs:428`) to the Receiver role with upstream's four conditions (Section 2.5) + client `i` advertise on pull; keep the batch-consistency abort. The ONE intended wire change. | oc-receiver vs REAL upstream-sender INC_RECURSE at >10000 files (the hard interop gate); #207-style byte capture both directions; each disabling condition suppresses the `i` letter / `CF_INC_RECURSE` bit. |
Dependency: RS-3a -> RS-3b -> {RS-3c, RS-4} -> RS-2. RS-3c and RS-4 are
independent of each other. RS-2 (the negotiation flip, A5b/c/d) requires an
EXPLICIT USER GO before its interop gate is touched. The lazy SENDER chain
(LS #200-202) is symmetric and shares only the wire contract (Section 5); it
does not block any RS stage.
