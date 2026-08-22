# One logcode-to-stream funnel

Status: design, partially implemented (the routing half already ships).

## The defect

oc has **two output channels that are drained at different times**, so the
relative order of two messages depends on which channel each happened to use,
not on which was produced first.

| channel | produced by | buffered in | drained at |
|---|---|---|---|
| client events | `core`/`engine` transfer records | the client summary collection | `emit_transfer_summary`, inside `execute()` |
| diagnostic events | `info_log!` / `warn_log!` / `debug_log!` | `logging::thread_local::EVENTS` | `flush_diagnostics`, **after** `execute()` returns |

Because `flush_diagnostics` runs once after `execute()`, every deferred
diagnostic lands after every client event, by construction - regardless of
production order.

### It already forced one workaround

`crates/engine/src/local_copy/executor/sources/orchestration.rs` documents a
notice that had to be moved out of the deferred channel to get it into the
right place:

> the generator prints the delta-transmission status once at
> `DEBUG_GTE(FLIST, 1)` ... before the per-file generate loop. A local copy
> renders its name list post-hoc in the CLI, so that notice is emitted there
> (in `emit_transfer_summary`) to keep it ahead of the list rather than
> dead-last through the deferred diagnostic flush.

That is the cost this design removes: without a funnel, **every** notice whose
position matters needs its own hand-placed emission at the render site, and
each one is a separate opportunity to get the gate or the wording wrong.

## Upstream's model

Upstream has exactly one chooser. `rwrite()` is called in production order and
writes immediately, so ordering is not a concern it has to solve.

| step | upstream | anchor |
|---|---|---|
| default sink | `f = msgs2stderr == 1 ? stderr : stdout` | `log.c:272` |
| daemon redirection | `am_daemon > 0 && code != FCLIENT` becomes `FLOG` | `log.c:290-291` |
| sibling forwarding | `send_msgs_to_gen` forwards and returns | `log.c:292-301` |
| code normalisation | `FERROR_SOCKET`/`FERROR_UTF8` become `FERROR`; `FCLIENT` becomes `FINFO` | `log.c:303-311` |
| log destination | `am_daemon \|\| logfile_name` writes the log copy, returns for `FLOG` | `log.c:312-334` |
| stream switch | `FERROR_XFER`/`FERROR`/`FWARNING` to stderr; `FINFO` to stdout, suppressed under `quiet` | `log.c:336-355` |
| server forwarding | `am_server` sends the message to the client, with a proto<30 downgrade | `log.c:357-373` |

The normalisation happening **before** the switch is load-bearing: those three
codes never reach the switch under their own name.

## What oc already has

The routing half is built and live. Do not rebuild it.

- `crates/logging/src/log_code.rs` - `LogCode` mirrors upstream's `enum logcode`
  one variant per `F*` identifier, each documenting its routing rule.
- `crates/logging/src/stream.rs` - `message_stream(code, ctx)` implements the
  switch, in upstream's order, including the pre-switch normalisation. Returns
  `BadLogCode` rather than silently picking a stream.
- `StreamContext` carries the non-code inputs (`quiet`, `msgs2stderr`,
  `log_destination`); `Msgs2Stderr` is a tri-state because upstream's gate
  distinguishes all three values.
- Live consumers: `crates/cli/src/frontend/progress/diagnostic.rs` at two sites.

So the missing piece is **ordering**, not routing.

## Design

Both channels are already buffered (verified: `render.rs` takes a collection
and a writer; it does not stream during the transfer). Uniform deferral plus a
single ordering key therefore reproduces upstream's order exactly, without
needing a process-wide write-through sink.

1. **One sequence source.** A monotonic counter in `logging`, stamped at
   production time. `core` already depends on `logging`, so both producers can
   reach it. It must be process-wide (an `AtomicU64`), not thread-local - the
   emitter can run on a worker thread.
2. **Stamp both event kinds** with that sequence as they are produced.
3. **Merge at render.** Emit the two buffers as one sequence-ordered stream,
   routing each entry through the existing `message_stream(code, ctx)`.
4. **Remove the workaround.** The relocated delta-transmission notice returns to
   its production site once ordering is intrinsic.

### The merge seam

Two properties of the render path were measured before choosing where step 3
attaches; both rule out the obvious placements.

**`emit_transfer_summary` has no stderr.** Its signature ends
`writer: &mut dyn Write` (`render.rs:124`) - one stream. So it cannot route
diagnostics itself: `message_stream` returns `Stderr` for the error and warning
codes, and there is nowhere to put them. The stream split therefore has to
happen in the caller, which holds both `out` and `err`; only the stdout-bound
diagnostics travel into the summary renderer.

That is not a compromise, it is upstream's own rule: `rwrite()` picks a stream
per log code, so relative order is only observable *within* a stream. Ordering
stdout-bound diagnostics against the stdout listing is the whole requirement.

**The per-event emitters are generic over the writer, not over a sink.**
`emit_list_only` (:370), `emit_progress` (:462) and `emit_verbose` (:954) are
each `<W: Write + ?Sized>`. A plain `Write` cannot express "a new event is about
to be rendered, with this key", which is exactly the hook the merge needs - so
decorating the writer is not enough.

The seam is therefore a collaborator passed alongside the writer, not a wrapper
around it. A `Write` subtrait would need a blanket impl for plain writers to
keep existing callers compiling, and that blanket impl would also cover the
merging type - the two impls overlap and coherence rejects them. Passing the
pending diagnostics as their own parameter sidesteps that entirely: the three
emitters gain one argument and one call at the top of their per-entry loop, and
callers with nothing to interleave pass the empty value.

Client events carry `Option<Sequence>`: `None` on a remote transfer, which builds
its events from the wire and never populates the diagnostic buffer from the same
production run. `begin_event(None)` must therefore flush nothing rather than
flush everything - an unkeyed event has no position to order against, and
draining on it would hoist unrelated diagnostics to an arbitrary point.

### Why not write-through

Mirroring `rwrite()` literally - resolve and write at the emit site - would
need a sink reachable from deep inside `engine`/`transfer`, where no writer is
threaded. It would also collide with the server path, which must frame
diagnostics to the peer instead of writing them locally
(`drain_events_for_peer`). Deferring uniformly preserves the observable order
that upstream gets from writing immediately, which is the property under test.

## Known adjacent defect

`logging::set_quiet`, the `QUIET` cell and `finfo_suppressed` are **not on
master** - they are added by the `skipping directory` branch. There, they are
thread-local while the emitter may run on a worker thread, so the suppression
can be consulted on a thread that never had it set.

The sequence counter must not repeat that mistake, which is why it is an
`AtomicU64` rather than a thread-local. Fixing `QUIET` itself belongs to the
step that rebases that branch onto the funnel, not to the step that introduces
the counter - on master there is nothing to fix yet.

## Gates

- The relative order of a diagnostic and a client event must follow production
  order, proven by a test that fails when the sequence is dropped.
- No change to which stream a message lands on: `message_stream` is unchanged,
  and its existing tests stay green.
- Server-side framing (`drain_events_for_peer`) must keep working - the funnel
  changes ordering, not destination.
- Both 3.5.0 pipe legs stay at their current pass/fail counts, with any row that
  flips recorded in the same commit.
