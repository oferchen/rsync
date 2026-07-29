# Receiver-side INC_RECURSE conversion plan (RS-1)

Status: design note gating RS-2..RS-4. No code changes in this stage.
Sibling: `docs/design/lazy-sender-inc-recurse.md` (LS-1) designs the SENDER
half of the same win; read it first - this note reuses its structure and the
two share one wire contract (Section 6).

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
