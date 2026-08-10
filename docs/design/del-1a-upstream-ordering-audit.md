# Upstream delete.c NDX_DEL_STATS ordering audit (DEL-1.a)

Status: Audit (task DEL-1.a; foundation for DEL-1.b reorder-buffer design and
DEL-1.c cohort batching strategy)
Audience: receiver and engine maintainers planning a parallel `DeleteEmitter`
consumer behind a feature flag.
Scope: every wire side effect that the receiver-side delete pass produces or
consumes, with emphasis on the ordering relationship between `MSG_DELETED`
per-file notifications and the cohort-level `NDX_DEL_STATS` frame.

Out of scope: the destination-side filesystem syscall ordering (covered by
`docs/design/parallel-deterministic-delete.md` and
`docs/design/delete-during-strict-order-gate.md`), `--remove-source-files`,
and the sender-side delete path under `--delete-excluded` (the sender never
emits `NDX_DEL_STATS`; only the generator does).

Upstream reference base: `target/interop/upstream-src/rsync-3.4.1/`. Upstream
calls the file `delete.c` (the task brief used `del.c`; there is no `del.c`
in the tree). Citations below use `delete.c` for the source of `delete_item`
/ `delete_dir_contents`, and `generator.c` for `delete_in_dir`,
`do_delete_pass`, `do_delayed_deletions`, and the goodbye-phase calls to
`write_del_stats`.

## 1. Where deletions originate

The generator is the only role that decides a destination path should be
removed. Three orchestration entry points exist, all in `generator.c`, and
they correspond directly to the upstream `--delete-{before,during,after}`
modes plus the `--delete-delay` variant which is a sibling of `delete_during`:

| Mode | Variable | Entry point | Wire effect |
|------|----------|-------------|-------------|
| `--delete-before` | `delete_before` | `do_delete_pass` (`generator.c:351-387`) called from `generate_files` before the first content directory is processed (`generator.c:2263-2264`). | One `MSG_DELETED` per deleted entry, emitted in destination-directory traversal order, **before** any file-transfer NDX or any `NDX_DEL_STATS`. |
| `--delete-during` | `delete_during == 1` | `delete_in_dir` (`generator.c:272-347`) called inline as the generator descends each content directory (`generator.c:1520-1523`, `generator.c:2298-2307`). | `MSG_DELETED` emitted interleaved with the per-file NDX traffic for that directory's content. |
| `--delete-delay` | `delete_during == 2` | `delete_in_dir` queues entries via `remember_delete` (`generator.c:161-182`) into a `deldelay` temp file; `do_delayed_deletions` (`generator.c:252-265`) replays them at end-of-flist, calling `delete_item` directly. | `MSG_DELETED` emitted **after** the file-transfer phase finishes for that flist segment. |
| `--delete-after` | `delete_after` | `do_delete_pass` called from `generate_files` after the last content directory (`generator.c:2410-2411`). | `MSG_DELETED` emitted at end-of-transfer, after the redo phase. |

The actual deletion syscall and the per-file wire notification both happen
inside `delete_item` (`delete.c:130-225`). On success, `delete_item` calls
`log_delete(fbuf, mode)` (`delete.c:180`) which - when running on the server
side at protocol >= 29 - calls `send_msg(MSG_DELETED, fname, len,
am_generator)` (`log.c:863`). For directories the trailing NUL is preserved
(`log.c:861-862`) so the receiver can tell directory deletes from file
deletes (`io.c:1594-1600`). Recursive directory contents are peeled by
`delete_dir_contents` (`delete.c:48-122`), which loops over the destination
listing in reverse and calls `delete_item` per entry, so a single user-visible
directory delete can fan out into many `MSG_DELETED` lines, in
reverse-directory order, before the parent `rmdir` succeeds.

Also note `delete_item`'s `DEL_MAKE_ROOM` path (`delete.c:156-159, 179`):
when the generator deletes a destination file because a same-named source
file of a different kind is about to overwrite it, the deletion is **not**
counted in `stats.deleted_*` and **no `MSG_DELETED`** is emitted (the
`!(flags & DEL_MAKE_ROOM)` guard around both `log_delete` and the counters
short-circuits everything). That side path is invisible on the wire; the
parallel consumer never has to think about it.

## 2. When NDX_DEL_STATS is sent

`NDX_DEL_STATS` is defined as the negative ndx `-3` (`rsync.h:287`). It is
written by `write_del_stats` (`main.c:225-238`) and read by `read_del_stats`
(`main.c:240-247`). Both functions live in `main.c` rather than `delete.c`
because they live on the wire-codec layer, not the delete-policy layer.

Wire layout produced by `write_del_stats`:

1. `write_ndx(f, NDX_DEL_STATS)` - the ndx-codec frame, the same channel
   that carries `NDX_DONE`, file indices, and `NDX_FLIST_OFFSET`. Note the
   read-batch path uses raw `write_int` (`main.c:227-228`) so an offline
   batch file is byte-identical to a varint-encoded frame minus the ndx
   compression.
2. `write_varint(f, files - dirs - symlinks - devices - specials)` -
   regular-file count, derived by subtraction so the wire stays compact
   even when most deletes are regular files (`main.c:231-233`).
3. `write_varint(f, dirs)` (`main.c:234`).
4. `write_varint(f, symlinks)` (`main.c:235`).
5. `write_varint(f, devices)` (`main.c:236`).
6. `write_varint(f, specials)` (`main.c:237`).

`read_del_stats` (`main.c:240-247`) reverses the encoding and accumulates
into `stats.deleted_files` by adding every subsequent varint, so the field
"deleted_files" on the reader side ends up as the total across all five
kinds while the per-kind buckets remain isolated.

Triggers, all inside `generate_files` in `generator.c`:

- **Early path** (`generator.c:2376-2381`): fires when `protocol_version >=
  31 && EARLY_DELETE_DONE_MSG()`. The macro
  `EARLY_DELETE_DONE_MSG()` is `!(delete_during == 2 || delete_after)`
  (`generator.c:124`), so early means "deletes have already happened" - the
  `--delete-before` and inline `--delete-during` modes. The send is gated
  by `(INFO_GTE(STATS, 2) && (delete_mode || force_delete)) || read_batch`.
  The first conjunct is `--stats`; the second guards against sending stats
  for a no-delete run; the `read_batch` path always sends so the batch file
  is self-describing.
- **Late path** (`generator.c:2420-2425`): fires when `protocol_version >=
  31 && !EARLY_DELETE_DONE_MSG()`, i.e. when `delete_during == 2`
  (`--delete-delay`) or `delete_after`. Gated by `INFO_GTE(STATS, 2) ||
  read_batch`. Emitted **after** `do_delayed_deletions` and the
  `do_delete_pass` for `--delete-after` (`generator.c:2408-2411`), so the
  stats reflect the deletions that fired in the late pass.

In both cases the frame is followed by `write_ndx(f_out, NDX_DONE)`
(`generator.c:2380, 2424`) to advance the goodbye state machine.

The receiver-side decoder is `read_ndx_and_attrs` (`rsync.c:322-353`), which
loops over `NDX_DEL_STATS`, calling `read_del_stats(f_in)` and - if `am_sender
&& am_server` - re-emitting it on `f_out` for the sender child to pick up:

```c
if (ndx == NDX_DEL_STATS) {
    read_del_stats(f_in);
    if (am_sender && am_server)
        write_del_stats(f_out);
    continue;
}
```

So `NDX_DEL_STATS` travels the same direction the rest of the ndx stream
travels - generator -> receiver during the goodbye exchange - and a daemon
server with sibling sender/receiver forwards it laterally.

The batch-replay path (`main.c:888-895`) drains everything up to
`NDX_DEL_STATS` from the batch-generator fd before forwarding the final
`NDX_DONE`, which is the only place upstream synchronously waits on
`NDX_DEL_STATS`. Elsewhere it is fire-and-forget.

## 3. Channel structure: why MSG_DELETED and NDX_DEL_STATS are loosely
coupled on the wire

This is the central observation for a parallel consumer.

- **`MSG_DELETED` rides the multiplex side-channel.** `send_msg(MSG_DELETED,
  fname, len, am_generator)` (`log.c:863`) writes into `iobuf.msg`
  (`io.c:965-1058`), which is multiplexed against `MSG_DATA` envelopes
  carrying the main ndx stream (`io.c:680-708`). The wire reader splits the
  channels in `perform_io`; `MSG_DELETED` is dispatched at `io.c:1549-1601`
  and turns into a `log_delete` callback on the receiving end. There is no
  ndx in the `MSG_DELETED` payload at all - just the path and an implicit
  file kind (the trailing NUL trick).
- **`NDX_DEL_STATS` rides the main ndx stream.** `write_ndx(f, NDX_DEL_STATS)`
  pushes the negative ndx onto `iobuf.out`, which is wrapped in `MSG_DATA`
  multiplex frames before hitting the socket. Receivers consume it through
  `read_ndx`/`read_ndx_and_attrs` (`rsync.c:331, 337`), the same path that
  reads positive file ndx values.

Because these are two different channels, the wire ordering between a given
`MSG_DELETED` and the closing `NDX_DEL_STATS` frame for the same cohort is
**not** strict. The only thing that gives them apparent order on a normal
TCP stream is the buffering policy:

- `send_msg` buffers into `iobuf.msg` (`io.c:992-1058`) and flushes on
  pressure or on explicit `io_flush` calls.
- `write_ndx` buffers into `iobuf.out` and flushes through `perform_io`.

The generator emits all per-cohort `MSG_DELETED` calls (via `log_delete`
called from `delete_item`) **before** the generator's main thread reaches
the `write_del_stats` call in `generate_files`. So the **causal** order is
strict on the producer side: every `MSG_DELETED` for a cohort precedes the
`NDX_DEL_STATS` for that cohort. The **observed** wire order can interleave
because the two channels share the socket only via multiplex framing and
the receiver's read loop drains them independently.

The receiver does not enforce any ordering between the two channels either:
`log_delete` from `MSG_DELETED` updates only the user-visible itemize log,
and `read_del_stats` updates only `stats.deleted_*` counters. Neither side
asserts the other ran first.

## 4. Cohort definition

There is **one cohort per `write_del_stats` call**, and at most two calls
ever fire per transfer (early xor late, never both). The cohort boundary is
the call site, not the per-directory boundary, not the per-flist-segment
boundary.

Practically:

- For `--delete-before` and inline `--delete-during`: a single cohort covers
  every deletion the generator made during the whole transfer. Stats are
  shipped during the early goodbye phase.
- For `--delete-delay` and `--delete-after`: a single cohort covers every
  deletion made by the late pass (`do_delayed_deletions` or
  `do_delete_pass`). The early-path `MSG_DELETED` calls from any inline
  decisions would still have been emitted earlier on the wire (this case
  does not happen for `--delete-after`, but it can for INC_RECURSE +
  `--delete-during == 2` where `delete_in_dir` already emitted `MSG_DELETED`
  via `log_delete` calls before deferred-replay - confirm: actually for
  `delete_during == 2` the inline `delete_in_dir` call routes through
  `remember_delete` not `delete_item`, so no `MSG_DELETED` fires until
  `do_delayed_deletions`. See `generator.c:338-342`.)

INC_RECURSE does **not** subdivide the cohort. Although `generate_files`
iterates flist segments and calls `delete_in_dir` inside the per-segment
loop (`generator.c:1520-1523`), the `write_del_stats` calls happen once,
after the whole flist loop completes. The stats accumulator
(`stats.deleted_files`, `stats.deleted_dirs`, etc.) is a single global
struct (`rsync.h:1045`) mutated in place by `delete_item` (`delete.c:181-193`),
so the late `write_del_stats` reads the totals across every per-directory
batch the generator processed.

The exception is the read-batch / write-batch path. In write-batch mode the
batch file accumulates a single `NDX_DEL_STATS` frame; in read-batch mode
the receiver drains up to that frame as part of the goodbye sequence
(`main.c:888-895`). Still one cohort.

## 5. Invariants a parallel consumer can rely on vs must preserve

### 5.1 What a parallel consumer can reorder safely

- **Per-file `MSG_DELETED` deliveries within a cohort.** The receiver-side
  handler (`io.c:1549-1601`, `log_delete` at `log.c:839-876`) treats each
  message independently. It logs the entry through the itemize formatter
  and updates no shared counter. The relative order of two `MSG_DELETED`
  notifications affects only the order lines appear in `--verbose` /
  `--itemize-changes` output, not protocol correctness.
- **Per-file syscall dispatch.** `delete_item` (`delete.c:130-225`) and
  `delete_in_dir` (`generator.c:272-347`) take no inter-entry locks; the
  only shared state across entries is `stats.deleted_*`, which is summed
  globally. A parallel consumer that pre-aggregates kind counters and
  atomically merges them before `write_del_stats` runs is wire-equivalent.
- **`MSG_DELETED` vs `NDX_DEL_STATS` interleaving across channels.** As
  established in section 3, upstream does not promise a strict wire order
  here. A parallel consumer that emits `MSG_DELETED` later than the
  current single-emitter does is still upstream-compatible as long as
  every `MSG_DELETED` for a cohort is on the wire **before** the closing
  `NDX_DONE` that ends the cohort's goodbye phase. The closing `NDX_DONE`
  (`generator.c:2380, 2424`) is the only hard fence.

### 5.2 What a parallel consumer must serialise

- **One `NDX_DEL_STATS` per cohort.** Upstream sends exactly one frame per
  call site. Emitting two would either trip `read_ndx_and_attrs`'s loop
  twice (harmless to the wire but doubles the totals on the receiver since
  `read_del_stats` accumulates additively at `main.c:243-246`) or, if both
  frames raced into the goodbye-phase `NDX_DONE`, the receiver would
  consume the first frame as stats and then mis-parse the second's varints
  as the next ndx, crashing with `Invalid file index` (`rsync.c:349-352`).
- **`NDX_DEL_STATS` must precede the next `NDX_DONE` on the ndx channel.**
  Both upstream call sites pair the frame with an immediate `write_ndx(f_out,
  NDX_DONE)` (`generator.c:2380, 2424`). The receiver's goodbye reader
  loops over `NDX_DEL_STATS` and then expects `NDX_DONE`
  (`receiver/transfer/phases.rs:144-162` mirrors this). Emitting `NDX_DONE`
  before `NDX_DEL_STATS` would advance the receiver past the goodbye
  state and any later `NDX_DEL_STATS` would arrive in the
  read_final_goodbye loop, which only tolerates `NDX_DONE` after the first
  iteration consumed `NDX_DEL_STATS` (`main.c:886-897`). The mismatch
  surfaces as `RERR_PROTOCOL` (`rsync.c:352`).
- **Stats accumulation must be complete before the frame is written.** The
  five counters in the frame are derived by subtraction (regular files =
  total - dirs - symlinks - devices - specials in `main.c:231-233`), so a
  parallel consumer must finish every cohort dispatch and merge every
  worker's counters into a single `DeleteStats` before serialising. A
  consumer that races the merge will under-count regular files (and the
  receiver, which accumulates additively, will sum a wrong total).
- **No per-cohort `NDX_DEL_STATS` if `do_stats` is false in the non-batch
  case.** Upstream skips the frame entirely if `--stats` was not requested
  (`generator.c:2377, 2422`). A parallel consumer must keep this gate
  intact; otherwise an old peer that does not expect the frame at this
  point will mis-parse the ndx stream.
- **`DEL_MAKE_ROOM` deletes are silent.** The `!(flags & DEL_MAKE_ROOM)`
  guard at `delete.c:179` skips both the `MSG_DELETED` send and the stat
  increment. A parallel consumer that classifies destination-clearing
  deletes as "real" deletes would over-count and inject spurious
  `MSG_DELETED` lines.
- **Cohort identity must survive the ENOTEMPTY recursive fallback.** When
  a directory delete falls through to `delete_dir_contents`
  (`delete.c:144-154`), upstream emits one `MSG_DELETED` per contained
  entry **before** the eventual parent `MSG_DELETED`, and counts every
  one of them in the same global `stats.deleted_*`. A parallel consumer
  that hands the nested plan to a different worker thread must still
  fold every increment into the cohort that owns the parent dispatch -
  upstream has exactly one cohort spanning the whole sweep, and the
  receiver's `read_del_stats` cannot tell otherwise.

### 5.3 Suggested invariants for the DEL-1.b reorder buffer

- The reorder buffer's commit boundary is the closing `NDX_DONE` of the
  cohort's goodbye phase, **not** the per-directory boundary.
- The buffer must reject any attempt to enqueue an `NDX_DEL_STATS` frame
  before all per-cohort `MSG_DELETED` enqueues have been observed (commit
  ordering on the producer side - even though the wire ordering is loose,
  enforcing it on the producer side keeps the regression-vs-baseline
  delta to zero).
- The buffer must enforce a hard "stats first, then NDX_DONE" sequence on
  the ndx channel.
- The buffer should treat `MSG_DELETED` enqueues as commutative within a
  cohort (any permutation is wire-equivalent) so the parallel consumer can
  batch them per worker without an explicit sort.

## 6. Failure modes

Catalogued from the upstream decoder's perspective, so DEL-1.b knows what
divergence costs at each failure point.

### 6.1 NDX_DEL_STATS arrives early (before per-file MSG_DELETED)

Upstream never emits this pattern: `write_del_stats` is called only after
all `delete_item` calls for the cohort have returned. A parallel consumer
that races stats emission ahead of `log_delete` does **not** crash the
receiver - `MSG_DELETED` updates a separate side channel that ignores stats
state - but it produces user-visible weirdness:

- `--stats` output appears before some `deleting foo` lines.
- `--itemize-changes` lines arrive after the stats summary, which is
  harmless to scripts that parse line-by-line but breaks
  cohort-by-cohort log scraping.

No exit-code change. Classified as stat-divergence only.

### 6.2 NDX_DEL_STATS arrives late (after the closing NDX_DONE)

Upstream's goodbye reader pattern is `loop { read NDX; if NDX_DEL_STATS,
read stats and continue; if NDX_DONE, break }` (`main.c:886-897`,
`rsync.c:329-342`, mirrored in `receiver/transfer/phases.rs:144-162`). A
stats frame that arrives after the first `NDX_DONE` of the goodbye:

- In `read_final_goodbye` (`main.c:886-905`): the first `NDX_DONE` triggers
  the echo path. The second `read_ndx_and_attrs` call (`main.c:897`) then
  reads the stale `NDX_DEL_STATS`, accumulates its varints into
  `stats.deleted_*` a **second time**, and continues looking for the final
  `NDX_DONE`. Net effect: the user sees doubled deleted-count totals in
  `--stats` output. The next ndx must still be `NDX_DONE` or the receiver
  exits with `RERR_PROTOCOL` (`main.c:901-904`).
- In `read_ndx_and_attrs` mid-transfer: the loop happily consumes
  `NDX_DEL_STATS`, double-counts, and resumes (`rsync.c:337-342`). No
  protocol error, only stat divergence.

So "late" is mostly survivable as stat divergence, with the corner case
that any non-`NDX_DONE` ndx after the goodbye echo crashes the receiver.

### 6.3 NDX_DEL_STATS with wrong counter counts

`read_del_stats` reads exactly five varints (`main.c:242-246`). The wire
field is a varint with no length prefix, so the decoder will:

- Under-read (fewer than five varints): the next four reads consume bytes
  intended for a subsequent ndx or the next multiplex envelope header.
  The next `read_ndx` call decodes garbage and hits one of the index-range
  guards in `rsync.c:343-373`, exiting with `RERR_PROTOCOL`.
- Over-read (more than five varints): the extra varints are not consumed;
  the next ndx read decodes the first leftover varint as a new ndx, which
  is statistically a positive (file) ndx in the wrong range or a negative
  ndx outside `{-1, -2, -3, -101..}`. Either way `rsync.c:349-352` exits
  with `RERR_PROTOCOL`.
- Right count but wrong values (e.g. negative-looking varints because of
  signed overflow): `read_varint` returns an `i32` cast to `u32` in the
  oc-rsync decoder (`protocol/src/stats/delete.rs:94-99`); upstream's
  `read_del_stats` casts via `int` arithmetic and adds to `int` counters
  with no overflow check. Stat divergence only; not a crash.

Verdict: count errors are **fatal**. A parallel consumer must produce
exactly five varints in the documented order, every time.

### 6.4 Duplicate NDX_DEL_STATS within the same cohort

Equivalent to "wrong count" via the doubling path (section 6.2). The first
frame is consumed correctly, the second is consumed as stats too (because
`read_ndx_and_attrs` loops), totals double, and the closing `NDX_DONE` is
read normally. No crash, stat-divergence only.

### 6.5 NDX_DEL_STATS for a cohort that produced no deletions

Upstream sends the frame with all five varints set to zero when
`--stats` is on and the cohort dispatched zero deletes (`generator.c:2422`
late path - the `INFO_GTE(STATS, 2)` gate is the only check). The receiver
accumulates `+0` into `stats.deleted_*` and proceeds. A parallel consumer
that suppresses the frame on an empty cohort is upstream-compatible only
on the early path (`generator.c:2376-2377` gates on `delete_mode ||
force_delete`); on the late path the frame must be sent unconditionally
once `do_stats` is on, or interop diverges in the `--stats` output
formatting (`main.c:424` `output_itemized_counts("Number of deleted files",
&stats.deleted_files)`).

### 6.6 MSG_DELETED with mismatched directory vs file kind

The trailing-NUL convention (`log.c:861-862`, `io.c:1594-1600`) is the only
in-band kind discriminator. A parallel consumer that emits a directory path
without the trailing NUL would have it logged as `del.` regular-file rather
than `del.` directory in `--itemize-changes`. The `S_IFDIR` vs `S_IFREG`
distinction also affects log_formatted's `%n` rendering. No crash, only
log-format divergence.

## 7. Summary of strictest invariant

**Per cohort, exactly one `NDX_DEL_STATS` frame, carrying exactly five
varints (regular-files-by-subtraction, dirs, symlinks, devices, specials)
must appear on the ndx channel between the last `MSG_DELETED` for the
cohort and the closing `NDX_DONE` of the goodbye phase.** Anything before
or after that window is recoverable as stat divergence; violating either
the count or the position breaks the goodbye state machine and exits the
receiver with `RERR_PROTOCOL`.

This bounds DEL-1.b's reorder-buffer design: the buffer needs cohort
membership tracking (which the current single-emitter gets for free by
running sequentially), a barrier at the `NDX_DEL_STATS` enqueue point that
flushes every pending `MSG_DELETED`, and a strict ordering between the
stats frame and the goodbye `NDX_DONE` on the ndx side.

## 8. Cross-references

- Current single-emitter consumer:
  `crates/engine/src/delete/emitter/mod.rs:245-310` (`emit_all`, `drain_plan`,
  `run_entry`).
- Current generator-side `NDX_DEL_STATS` emission:
  `crates/transfer/src/generator/transfer/goodbye.rs:79-110`.
- Current receiver-side `NDX_DEL_STATS` consumption:
  `crates/transfer/src/receiver/transfer/phases.rs:141-166`.
- Wire codec for the five-varint frame:
  `crates/protocol/src/stats/delete.rs:74-107`.
- DDP design: `docs/design/parallel-deterministic-delete.md`.
- Strict-order gate (existing constraint already in tree):
  `docs/design/delete-during-strict-order-gate.md`.
- Upstream entry points:
  `target/interop/upstream-src/rsync-3.4.1/delete.c:130-225` (`delete_item`),
  `delete.c:48-122` (`delete_dir_contents`),
  `generator.c:252-265` (`do_delayed_deletions`),
  `generator.c:272-347` (`delete_in_dir`),
  `generator.c:351-387` (`do_delete_pass`),
  `generator.c:2263-2425` (delete-pass scheduling and `write_del_stats`
  call sites),
  `main.c:225-247` (`write_del_stats` / `read_del_stats`),
  `rsync.c:322-353` (`read_ndx_and_attrs` loop over `NDX_DEL_STATS`),
  `log.c:839-876` (`log_delete` -> `send_msg(MSG_DELETED, ...)`),
  `io.c:1549-1601` (receiver-side `MSG_DELETED` dispatch).
