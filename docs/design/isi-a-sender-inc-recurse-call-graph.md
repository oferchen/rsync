# ISI.a - Sender-side INC_RECURSE Call Graph Audit

Doc-only audit of the sender-side INC_RECURSE call graph. Six focused
questions with file:line citations, plus the minimal flip path for ISI.b
through ISI.h.

Related: `project_parallel_interop_parity_gap.md` memory note (sender-side
generator code exists but the capability bit is gated off). Prior write-up
`docs/design/inc-recurse-sender-reenable-audit.md` covers the broader shape;
this doc nails down the exact flip surface.

## 1. Definition of `inc_recursive_send`

Struct field and default:
- `crates/core/src/client/config/client/mod.rs:152` -
  `pub(super) inc_recursive_send: bool`.
- `crates/core/src/client/config/client/mod.rs:328` - default initializer
  sets `inc_recursive_send: false`.

Builder side (option storage, setter, build-time fallback):
- `crates/core/src/client/config/builder/mod.rs:219` -
  `inc_recursive_send: Option<bool>`.
- `crates/core/src/client/config/builder/mod.rs:437` -
  `inc_recursive_send: self.inc_recursive_send.unwrap_or(false)`.
- `crates/core/src/client/config/builder/performance.rs:234` -
  `pub const fn inc_recursive_send(mut self, value: bool) -> Self` setter.

Read accessor:
- `crates/core/src/client/config/client/performance.rs:208` -
  `pub const fn inc_recursive_send(&self) -> bool`.

CLI tri-state plumbing:
- `crates/cli/src/frontend/execution/drive/config.rs:108,268-269` - resolved
  tri-state forwarded into the builder.
- `crates/cli/src/frontend/execution/drive/workflow/run.rs:773` - workflow
  wiring of the resolved value.
- `crates/cli/src/frontend/command_builder/sections/build_base_command/transfer.rs:40,43-44` -
  Clap args for `--inc-recursive` / `--no-inc-recursive`.
- `crates/cli/src/frontend/arguments/parser/mod.rs:144` -
  `tri_state_flag_positive_first(&matches, "inc-recursive", "no-inc-recursive")`.

## 2. Runtime consumption sites

Two production read sites, both gating the `'i'` character in the
client-side capability string:

- `crates/core/src/client/remote/invocation/builder.rs:184-186` - SSH
  invocation:
  `args.push(OsString::from(build_capability_string(self.config.inc_recursive_send())));`.
  Adjacent comment (lines 178-183) is aspirational ("advertises INC_RECURSE
  in both directions by default") - the actual value reaches in as `false`
  because of the builder default in Q1.
- `crates/core/src/client/remote/daemon_transfer/orchestration/arguments.rs:166-168` -
  daemon transfer args for protocol >= 30:
  `args.push(build_capability_string(config.inc_recursive_send()));`.

Capability assembler (single source of truth):
- `crates/transfer/src/setup/capability.rs:138-153` -
  `pub fn build_capability_string(allow_inc_recurse: bool) -> String`. The
  gate is at line 144: `if mapping.requires_inc_recurse && !allow_inc_recurse
  { continue; }`. False -> `'i'` is stripped.

Generator-side runtime read of the negotiated flag (different path - reads
the post-handshake compat flag, not the local config flag):
- `crates/transfer/src/generator/context.rs:187-190` -
  `pub(crate) fn inc_recurse(&self) -> bool` checks
  `CompatibilityFlags::INC_RECURSE` on `compat_flags`. Returns `false` today
  because the peer never sees `'i'` and never agrees to INC_RECURSE.

## 3. Sender-side segment-boundary logic

The sender already emits multiple `NDX_FLIST` batches with full
segmentation. It is NOT monolithic. (Note: the brief asked about
`crates/transfer/src/sender/`, which does not exist; sender-side code lives
under `crates/transfer/src/generator/`.)

Partitioning:
- `crates/transfer/src/generator/file_list/inc_recurse.rs:32-228` -
  `partition_file_list_for_inc_recurse` reorders `file_list`/`full_paths`
  (initial top-level first, then per-directory in depth-first order) and
  populates `IncrementalState::{initial_segment_count, pending_segments}`.
  Early-returns when `!self.inc_recurse() || self.file_list.is_empty()`
  (line 39) - unreachable at runtime because `inc_recurse()` is false.

State and scheduling:
- `crates/transfer/src/generator/segments.rs:134-178` - `IncrementalState`
  (`pending_segments`, `flist_eof_sent`, `flist_writer_cache`,
  `initial_segment_count`, `ndx_segments`).
- `crates/transfer/src/generator/segments.rs:82-126` - `SegmentScheduler`
  with `MIN_FILECNT_LOOKAHEAD = 1000` throttling (line 23), matching
  upstream `flist.c:46`.

Initial batch:
- `crates/transfer/src/generator/protocol_io.rs:327-412` - `send_file_list`.
  Lines 352-355 cap the send at `initial_segment_count` when INC_RECURSE has
  populated it.

Sub-list dispatch and EOF:
- `crates/transfer/src/generator/protocol_io.rs:431-500` -
  `encode_and_send_segment` writes `NDX_FLIST_OFFSET - parent_dir_ndx`, the
  segment entries, and the 0 end marker. Updates `ndx_segments` per
  `flist.c:2931` (`ndx_start = prev->ndx_start + prev->used + 1`).
- `crates/transfer/src/generator/protocol_io.rs:563-579` - `send_flist_eof`
  emits `NDX_FLIST_EOF` once `SegmentScheduler::is_exhausted()`.

Loop integration:
- `crates/transfer/src/generator/transfer/transfer_loop.rs:84-125` -
  `let inc_recurse = self.inc_recurse();` then per-iteration
  `scheduler.next_if_needed(remaining)` -> `encode_and_send_segment`, and a
  one-shot `send_flist_eof` when exhausted. Mirrors `sender.c:227,261`.

## 4. Sender file-list construction entry point

Orchestrator (top-level build/partition/send sequence):
- `crates/transfer/src/generator/transfer/orchestrator.rs:75-87` -
  ```text
  if files_from_paths.is_empty() {
      self.build_file_list(paths)?;
  } else {
      self.build_file_list_with_base(&base_dir, &files_from_paths)?;
  }
  self.partition_file_list_for_inc_recurse();
  self.send_file_list(writer)?;
  ```

Builders (rust counterparts to upstream `send_file_list` flist build):
- `crates/transfer/src/generator/file_list/mod.rs:52-128` -
  `pub fn build_file_list(&mut self, base_paths: &[PathBuf]) -> io::Result<usize>`
  (cites `flist.c:2192 send_file_list()`).
- `crates/transfer/src/generator/file_list/mod.rs:142-244` -
  `build_file_list_with_base` for the `--files-from` variant.

Wire send entry:
- `crates/transfer/src/generator/protocol_io.rs:327` -
  `pub fn send_file_list` (Rust name overloads upstream's combined naming).

## 5. Dead-code / partial markers in the sender INC_RECURSE path

Across `file_list/inc_recurse.rs`, `segments.rs`, `protocol_io.rs`,
`transfer/transfer_loop.rs`, `transfer/orchestrator.rs`, and
`diagnostics.rs`:

- 0 `#[allow(dead_code)]` attributes.
- 0 `todo!()` / `unimplemented!()` calls.
- 0 `FIXME` / `XXX` comments.
- 0 `#[cfg(test)]` gates inside the production call graph.

Total dead/partial markers: **0**.

The only runtime gate is the early-return at
`crates/transfer/src/generator/file_list/inc_recurse.rs:39`. Everything past
it is unreachable today purely because `self.inc_recurse()` returns `false`
(the negotiated peer flag is never set because the capability bit is never
advertised - see Q2). Diagnostic counters in `diagnostics.rs` are wired up
but read zero in production for the same reason.

## 6. Single flip to advertise `'i'`

The narrowest change is the builder default at
`crates/core/src/client/config/builder/mod.rs:437`. Flipping
`unwrap_or(false)` to `unwrap_or(true)` causes both consumer sites in Q2
(SSH builder and daemon arguments builder) to call
`build_capability_string(true)`, which emits the `'i'` character per
`crates/transfer/src/setup/capability.rs:144`. The remote peer then
acknowledges `INC_RECURSE` in its compat flags, `GeneratorContext::inc_recurse()`
returns `true`, and the partition / segment-scheduler / EOF code paths
described in Q3 light up.

The CLI tri-state already routes `--inc-recursive` / `--no-inc-recursive`
through the same builder setter, so the override survives the flip:
`--no-inc-recursive` still sets the field to `false`, matching upstream
`set_allow_inc_recurse()` precedent.

## Verdict

PARTIAL-NEEDS-WORK. All sender-side machinery is implemented, upstream-cited, and wired through the orchestrator and transfer loop. The capability gate is the only on-wire blocker. Calling this "single flip" understates the cost: the `false` default exists precisely because sender-direction interop has not been validated against 3.0.9 / 3.1.3 / 3.4.1 / 3.4.2 (see `project_parallel_interop_parity_gap.md` and the re-enable audit). Flipping without staged interop validation would advertise the capability against unverified peers.

## Recommended flip path for ISI.b..ISI.h

- ISI.b: capability-bit interop matrix. Run interop with `inc_recursive_send(true)` against rsync 3.0.9 / 3.1.3 / 3.4.1 / 3.4.2 in push direction (sender = oc-rsync). Confirm wire bytes via `strace` / `tcpdump`. Record per-version pass/fail and byte diffs.
- ISI.c: add a sender-direction integration test asserting the `-e.xxx` capability contains `'i'` post-flip and that `GeneratorContext::inc_recurse()` returns `true` after compat exchange in push mode.
- ISI.d: invert the hard-coded `inc_recursive_send(off)` expectations at `client/remote/invocation/tests.rs:74,114` and `client/remote/daemon_transfer/orchestration/tests.rs:83-132` so the flip PR is mechanical.
- ISI.e: audit `MIN_FILECNT_LOOKAHEAD = 1000` (`transfer/generator/segments.rs:23`) vs upstream `flist.c:46`; confirm the throttle does not starve dispatch on tiny trees.
- ISI.f: verify `partition_file_list_for_inc_recurse` (`inc_recurse.rs:38`) preserves `parent_dir_ndx` alignment for every interop fixture - the `flist.c:2652-2659` "ABORTING due to invalid path from sender" risk flagged in the module header (lines 7-22).
- ISI.g: surface diagnostic counters in `transfer/generator/diagnostics.rs` at `--info=flist1` / `--info=stats3` for operator visibility during rollout.
- ISI.h: flip `client/config/builder/mod.rs:437` default; update doc comments at `client/config/client/performance.rs:217-225` and `client/config/builder/performance.rs:220-225` with the validated-version list from ISI.b.

## Upstream cross-references already in-tree

- `compat.c:720 set_allow_inc_recurse()` - `client/config/builder/performance.rs:229`, `client/config/client/performance.rs:201`.
- `options.c:3003-3050 maybe_add_e_option()` - `transfer/setup/capability.rs:137`.
- `flist.c:2192 send_file_list()` - `transfer/generator/file_list/mod.rs:17,48`, `transfer/generator/protocol_io.rs:325`.
- `flist.c:send_extra_file_list()` - `transfer/generator/protocol_io.rs:429`, `transfer/generator/segments.rs:32`.
- `flist.c:2534-2545` NDX_FLIST_EOF - `transfer/generator/protocol_io.rs:560-562`, `transfer/generator/segments.rs:132`.
- `sender.c:227,261` send-loop interleaving - `transfer/generator/segments.rs:13,80`, `transfer/generator/transfer/transfer_loop.rs:83`.
