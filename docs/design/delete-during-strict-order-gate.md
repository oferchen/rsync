# `--delete-strict-order` opt-in gate (#1940) - SUPERSEDED

> **Status: SUPERSEDED.** This design is retained for historical
> context only. The opt-in `--delete-strict-order` flag was replaced
> by the always-on two-phase parallel-deterministic delete model
> specified in
> [`docs/design/parallel-deterministic-delete.md`](parallel-deterministic-delete.md)
> (tasks #2251 - #2285). The flag is removed from the CLI surface;
> upstream per-directory ordering is the default and only behaviour.
> See
> [`docs/architecture/delete-during.md`](../architecture/delete-during.md)
> for the current architecture.

Status: Design (task #1940; cites audit #1893, follow-up #1894) - superseded
Audience: transfer, cli, filters maintainers
Scope (historical): introduce an opt-in `--delete-strict-order` flag
that forces oc-rsync's `--delete-during` path onto upstream's
per-directory interleaved order, without changing the default batched
behaviour.

## 1. Current `--delete-during` behaviour (#1893 audit)

The audit captured in `docs/architecture/delete-during.md` documents
oc-rsync's deletion model and its divergence from upstream rsync 3.4.1:

- **Phase ordering is batched.** `crates/transfer/src/receiver/transfer.rs`
  in `run_pipelined` dispatches a single
  `delete_extraneous_files` sweep between the metadata pre-pass and
  `build_files_to_transfer`. There is no per-directory interleave with
  the transfer phase.
- **Parallel and non-deterministic.** Above
  `DEFAULT_DELETION_THRESHOLD = 64`
  (`crates/transfer/src/parallel_io.rs:33`) the sweep uses
  `parallel_io::map_blocking` and `tokio::spawn_blocking`. Below the
  threshold the loop is sequential. The order of `*deleting` itemize
  lines depends on worker scheduling above the cutoff.
- **Single filter snapshot.** Workers share an `Arc<FilterChain>`
  cloned at sweep start (`receiver/directory/deletion.rs:93`). Per-dir
  `.rsync-filter` merge files loaded later in the transfer are not
  re-evaluated for deletion.
- **Error semantics differ.** `delete_extraneous_files` returns
  `io::Result`; orchestration errors propagate via `?` and may abort
  the transfer where upstream would log and continue.
- **Final state matches upstream** for any successful transfer; the
  divergence is observable only on failure modes, itemize ordering, and
  per-dir merge-file filter rules.

Upstream's interleave is `Delete(dir_A) -> Transfer(dir_A) ->
Delete(dir_B) -> Transfer(dir_B)`, dispatched serially through
`generator.c::recv_generator()` -> `delete_in_dir()`.

## 2. Proposal: `--delete-strict-order` opt-in

Introduce a new long-only CLI flag `--delete-strict-order` that, when
present, forces oc-rsync's deletion to mirror upstream's batched-vs-
interleaved model byte-for-byte. The flag is opt-in: omitting it leaves
today's batched sweep unchanged.

### 2.1 Surface

- **CLI**: long-only `--delete-strict-order`, no short alias. Lives
  next to `--delete-during` in
  `crates/cli/src/frontend/command_builder/sections/transfer_behavior_options.rs`.
  Help text: "force per-directory interleaved deletion order matching
  upstream rsync 3.4.1; disables parallel and batched delete sweep".
- **Config**: extend `TransferConfigBuilder` and `CoreConfig` deletion
  state in `crates/core/src/client/config/builder/deletion.rs` with a
  `strict_order: bool` field, defaulted to `false`.
- **Wire**: no protocol or remote-arg change. The flag is a local
  receiver-side scheduling switch; remote args constructed in
  `crates/core/src/client/remote/invocation/builder.rs` are unaffected.

### 2.2 Receiver dispatch

When `strict_order` is set, `run_pipelined` MUST:

1. Skip the standalone `delete_extraneous_files` call between the
   metadata pre-pass and `build_files_to_transfer`.
2. Drive deletion per-directory inside the transfer loop. Each
   directory entry, before signature generation or file dispatch, runs
   the equivalent of `delete_in_dir()` against the pre-loaded file
   list and the `FilterChain` snapshot in effect for that directory
   (after any `.rsync-filter` merge files for that subtree have been
   applied).
3. Force sequential deletion: bypass `parallel_io::map_blocking` and
   the `DEFAULT_DELETION_THRESHOLD` cutoff. Use the existing
   `delete_extraneous_files` primitives in
   `crates/transfer/src/receiver/directory/deletion.rs`, but invoke
   them one directory at a time from the generator's serial loop.
4. Adopt upstream's "log and continue" error policy on per-entry
   `delete_item()` failures, mirroring `rsync.c::do_delete()`.

### 2.3 Itemize and stats

- `*deleting` itemize lines emit in stable, reverse-iteration order
  per directory, matching upstream's
  `delete_in_dir()` walk.
- `DeleteStats` accounting (file/dir/symlink/device/special) and
  `--max-delete` enforcement remain shared with the batched path; only
  the dispatch order changes.

### 2.4 Mutual exclusion

- `--delete-strict-order` requires `--delete-during` (or `--del`).
  Combination with `--delete-before`, `--delete-after`, or
  `--delete-delay` is rejected at config build time, alongside the
  existing inplace/append/partial-dir/delay-updates checks in
  `crates/core/src/client/config/builder/deletion.rs`.
- Combination with `--remove-source-files` and `--max-delete` is
  permitted; both flow through the same shared primitives.

## 3. Backward compatibility: default unchanged

Omitting `--delete-strict-order` MUST leave receiver behaviour
identical to today's batched sweep. The audit's three risk areas
(error propagation, observable delete order, per-dir merge files)
remain present by default; the flag exists solely so users who need
upstream parity on those axes can opt in. No man-page, CHANGELOG, or
default-config bump beyond documenting the new flag is required.

A regression test under
`crates/transfer/tests/delete_during_strict_order.rs` asserts:

- Without the flag: today's batched dispatch and existing
  `delete_during_*` fixtures continue to pass unchanged.
- With the flag: per-directory interleave, sequential dispatch, stable
  itemize order, and per-dir merge-file filter re-evaluation match an
  upstream rsync 3.4.1 run captured by the interop harness in
  `tools/ci/run_interop.sh`.

## 4. Cross-references

- Audit: #1893 -- `docs/architecture/delete-during.md` and
  `docs/design/reorderbuffer-metrics-and-bypass.md` (Invariant 3).
- Documentation follow-up: #1894 -- man-page and CHANGELOG note that
  the default model is batched.
- This task: #1940.
