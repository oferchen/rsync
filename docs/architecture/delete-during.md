# Receiver deletion model: parallel-deterministic two-phase pipeline

This document describes oc-rsync's deletion architecture and how it
matches upstream rsync 3.4.4's observable ordering byte-for-byte while
preserving internal parallelism. The design is specified in
[`docs/design/parallel-deterministic-delete.md`](../design/parallel-deterministic-delete.md);
this page is the architectural overview.

`--delete` (and its phase variants `--delete-before`, `--delete-during`,
`--delete-delay`, `--delete-after`) removes destination entries that no
longer exist on the sender. The implementation parallelises candidate
computation across rayon workers and serialises every observable side
effect through a single emitter thread that walks directories in
upstream depth-first order.

## Two-phase model

```
                  +-----------------+      +------------------+
   flist segment  | compute_extras  |---->-| DeletePlan(D)    |
   arrives (#N)   | (rayon worker)  |      +------------------+
                  +-----------------+               |
                                                    v
                  +-----------------+      +------------------+
   flist segment  | compute_extras  |---->-| DeletePlan(D')   |
   arrives (#N+1) | (rayon worker)  |      +------------------+
                  +-----------------+               |
                                                    v
                  +---------------------------------------+
                  | DeletePlanMap (keyed by dir relpath)  |
                  +---------------------------------------+
                                    |
                                    v
                  +---------------------------------------+
                  | DirTraversalCursor (upstream order)   |
                  +---------------------------------------+
                                    |
                                    v
                  +---------------------------------------+
                  | single emitter thread:                |
                  |   for each dir in upstream order      |
                  |     await DeletePlan(D)               |
                  |     for each entry in plan order      |
                  |       unlink, itemize, stat++         |
                  +---------------------------------------+
```

### Phase 1: parallel `compute_extras`

For every arriving file-list segment, rayon workers compute the set of
destination entries that are not present in the sender's listing for
each content directory inside the segment. Each worker:

1. Snapshots the destination directory's `read_dir` output.
2. Computes `extras(D) = readdir(D) - segment_entries(D)`, intersected
   with the `FilterChain::allows_deletion()` snapshot in effect for
   that directory (including any `.rsync-filter` merge files loaded by
   `enter_directory` for that subtree).
3. Sorts the result with `compare_file_entries` (our port of upstream
   `f_name_cmp`), then reverses the order to match upstream's
   `delete_in_dir()` decrementing iteration.
4. Publishes the result as a `DeletePlan` into `DeletePlanMap`, keyed
   by the directory's relative path.

Workers are pure: read-only `read_dir`/`stat`, immutable flist,
immutable filter chain snapshot. They never call `unlink`, never emit
itemize output, and never mutate shared state beyond the single
publish into `DeletePlanMap`.

### Phase 2: single emitter

A single drain task owns every observable side effect. It walks
directories in upstream depth-first traversal order via
`DirTraversalCursor`; for each directory `D` it blocks until
`DeletePlanMap[D]` is ready, then:

- Evaluates `--max-delete` and per-entry filter rules in upstream
  order.
- Calls `unlink`/`rmdir`/recursive removal.
- Emits the `*deleting` itemize line via `writer.send_msg_info`.
- Updates `DeleteStats` (files, dirs, symlinks, devices, specials) and
  `io_error` exactly where upstream sets them.

Because every observable effect happens on one thread in upstream
order, the wall-clock event sequence (unlink syscall order, itemize
emission order, `MSG_INFO` framing order) matches upstream
byte-for-byte.

## Upstream parity guarantees

| Aspect                 | Upstream 3.4.4                            | oc-rsync                                                 |
| ---------------------- | ----------------------------------------- | -------------------------------------------------------- |
| Phase ordering         | Interleaved per directory                 | Same: emitter walks directories in upstream order        |
| Determinism            | Reverse-iteration single-threaded         | Same: single emitter, plans reverse-sorted by `f_name_cmp` |
| Filter evaluation      | Re-evaluated per directory (merge files)  | Same: each `DeletePlan` is built against the per-dir snapshot |
| Error handling         | Logged, transfer continues                | Same: emitter logs per-entry `delete_item()` failures and continues |
| Itemize order          | Stable for a given input                  | Same: emitter is single-threaded                         |
| Final filesystem state | Deterministic                             | Same                                                     |

**Conformance: matches upstream.** No user-visible flag controls this
behaviour; parity is the default.

## Phase-mode handling

- `--delete-before`: emitter drains plans for the whole tree before
  the transfer loop begins.
- `--delete-during` (default for `--delete`): emitter interleaves with
  the transfer loop, draining plans for each directory just as
  upstream's generator visits it.
- `--delete-delay`: emitter buffers plans during the transfer and
  replays them in upstream order at finalisation, mirroring
  `do_delayed_deletions()`.
- `--delete-after`: emitter drains plans after the transfer loop
  completes.

In every mode the per-directory plan is computed once during phase 1
and the emitter is the only thread that mutates state.

## Upstream references

- `generator.c::recv_generator()` -- per-entry generator dispatch.
- `generator.c::delete_in_dir()` -- per-directory delete enumeration
  (reverse iteration).
- `generator.c::do_delete_pass()` -- full-tree sweep used by
  `--delete-before`.
- `generator.c::do_delayed_deletions()` -- replay path used by
  `--delete-delay`.
- `rsync.c::do_delete()` / `delete_item()` -- shared removal primitive.

## oc-rsync references

- Design specification:
  [`docs/design/parallel-deterministic-delete.md`](../design/parallel-deterministic-delete.md)
- Receiver hook into the segment-dispatch loop:
  `crates/transfer/src/receiver/file_list.rs` (`receive_extra_file_lists`).
- Plan publication and emitter coordination:
  `crates/transfer/src/receiver/directory/deletion.rs`.
- Upstream `f_name_cmp` port:
  `crates/protocol/src/flist/sort.rs::compare_file_entries`.

## History

This document supersedes the earlier audit (#1893) of the batched
pre-transfer sweep and the opt-in `--delete-strict-order` gate design
(#1940). Both approaches were replaced by the two-phase model
(#2251 - #2285); the strict-order flag is no longer part of the CLI
surface and the batched code path no longer exists.
