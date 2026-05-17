# Module LoC Enforcement Audit

> **Status:** Wired into CI as informational on 2026-05-18; flip to required
> once over-limit count reaches zero.

`tools/enforce_limits.sh` invokes `cargo xtask enforce-limits`, which walks every
`.rs` file outside `target/` and `.git/` and fails when a file exceeds either an
explicit override in `tools/line_limits.toml` or the workspace default of
**650 lines** (warn at 400). This audit cross-references the script's output with
`git log --name-only -100` so the table only lists files that both:

1. exceed their effective LoC cap today, and
2. were touched in the most recent 100 commits on `origin/master`.

The full unfiltered run reports 324 files above their cap; the 19 entries below
are the subset that recent work has churned and should be addressed first.

## Effective failures touched in the last 100 commits

| File | Lines | Cap | Overshoot |
| --- | ---: | ---: | ---: |
| `crates/transfer/src/receiver/tests.rs` | 4049 | 650 | 523% |
| `crates/engine/src/local_copy/buffer_pool/tests.rs` | 2761 | 650 | 325% |
| `crates/fast_io/src/io_uring_stub.rs` | 2118 | 650 | 226% |
| `crates/fast_io/src/io_uring/registered_buffers.rs` | 1607 | 650 | 147% |
| `crates/engine/src/delete/emitter.rs` | 1466 | 650 | 126% |
| `crates/transfer/src/receiver/file_list.rs` | 1195 | 650 | 84% |
| `crates/fast_io/src/iocp/disk_batch.rs` | 1061 | 650 | 63% |
| `crates/cli/src/frontend/arguments/parser/mod.rs` | 915 | 650 | 41% |
| `crates/cli/src/frontend/execution/drive/workflow/run.rs` | 882 | 650 | 36% |
| `crates/core/src/client/remote/ssh_transfer.rs` | 855 | 650 | 32% |
| `crates/cli/src/frontend/command_builder/sections/transfer_behavior_options.rs` | 846 | 650 | 30% |
| `crates/fast_io/src/io_uring/config.rs` | 815 | 650 | 25% |
| `crates/rsync_io/src/ssh/builder.rs` | 811 | 650 | 25% |
| `crates/core/src/client/run/mod.rs` | 809 | 650 | 24% |
| `crates/fast_io/src/io_uring/session_pool.rs` | 758 | 650 | 17% |
| `crates/transfer/src/receiver/mod.rs` | 741 | 650 | 14% |
| `crates/engine/src/delete/context.rs` | 739 | 650 | 14% |
| `crates/cli/src/frontend/arguments/parser/tests.rs` | 707 | 650 | 9% |
| `crates/core/src/client/remote/async_ssh_transport.rs` | 651 | 650 | 0% |

Effort key: S = under half a day, M = 1-2 days, L = 2-5 days.

## Decomposition plans

### `crates/transfer/src/receiver/tests.rs` (4049, +523%) - L

1. Promote the in-line `mod tests` into a sibling `receiver/tests/` directory and
   re-export through `receiver/tests/mod.rs`.
2. Split per feature area: `tests/file_list.rs`, `tests/delta_apply.rs`,
   `tests/hard_links.rs`, `tests/symlinks_and_devices.rs`,
   `tests/partial_resume.rs`, `tests/errors_and_timeouts.rs`.
3. Move shared fixtures (`make_entry`, `setup_dirs`, `dummy_token_stream`) into
   `tests/support.rs` and `pub(super) use` from each leaf module.
4. Re-target each leaf under the 650 cap; expect 5-7 modules averaging 600-700
   lines.
5. Land in two PRs: (a) extract `support.rs` and split fixtures only, (b) split
   tests group-by-group with `git mv` so blame stays useful.

### `crates/engine/src/local_copy/buffer_pool/tests.rs` (2761, +325%) - M

1. Group by exercised surface: `tests/checkout.rs`, `tests/refill.rs`,
   `tests/metrics.rs`, `tests/pressure.rs`.
2. Reuse the existing `tests::support` pattern other engine modules already
   employ; export `make_pool(...)`, `force_drain(...)` helpers there.
3. Property tests get their own `tests/properties.rs` so the bounded-input
   helpers stay isolated.
4. Update `buffer_pool/mod.rs` to include the new submodule tree.

### `crates/fast_io/src/io_uring_stub.rs` (2118, +226%) - L

1. The stub is the non-Linux mirror of `io_uring/mod.rs`. Re-create the same
   directory structure (`io_uring_stub/mod.rs`, `io_uring_stub/probe.rs`,
   `io_uring_stub/ring.rs`, `io_uring_stub/buffer_ring.rs`,
   `io_uring_stub/registered_buffers.rs`, `io_uring_stub/sqe.rs`).
2. Pair each Linux file with a stub of the same name so cross-platform diffs
   stay readable.
3. Co-locate the `#[cfg(not(target_os = "linux"))]` gate on the new `mod`
   declarations; do not gate inside each leaf.
4. Strip dead constants kept only to satisfy the original mirror once the
   matching Linux file has been trimmed.
5. Long-term: hide the stubs behind a single `IoUringBackend` trait so callers
   bind to the trait and the platform variant module loads conditionally.

### `crates/fast_io/src/io_uring/registered_buffers.rs` (1607, +147%) - M

1. Split into `registered_buffers/mod.rs` (public types `RegisteredBufferGroup`,
   `RegisteredBufferSlot`, `RegisteredBufferStats`).
2. `registered_buffers/group.rs` for `new`, `try_new`, `unregister`, `Drop`.
3. `registered_buffers/slot.rs` for `RegisteredBufferSlot` and the unsafe
   slice accessors with their safety comments.
4. `registered_buffers/io.rs` for `submit_read_fixed_batch` and
   `submit_write_fixed_batch` and the private `page_size` helper.
5. Keep the `unsafe impl Send/Sync` blocks adjacent to the type they cover.

### `crates/engine/src/delete/emitter.rs` (1466, +126%) - M

1. Extract the `DeleteFs` abstraction into `delete/emitter/fs.rs` (trait,
   `RealDeleteFs`, `RecordingDeleteFs`, `DeleteEvent`).
2. Move `EmitterErrorPolicy` and policy helpers into
   `delete/emitter/policy.rs`.
3. Move `CohortDeleteRecord` and cohort indexing into
   `delete/emitter/cohort.rs`.
4. Keep `DeleteEmitter` itself in `delete/emitter/mod.rs`, importing the four
   leaf modules.
5. Move the in-file `mod tests` into `delete/emitter/tests/` and split by
   trait surface (`fs.rs`, `policy.rs`, `cohort.rs`, `emitter.rs`).

### `crates/transfer/src/receiver/file_list.rs` (1195, +84%) - M

1. The file mixes three concerns: `ReceiverContext` glue, the streaming
   `IncrementalFileListReceiver`, and post-processing helpers
   (`match_hard_links`, `normalize_pre30_hardlinks`).
2. Move `IncrementalFileListReceiver` into `receiver/file_list/incremental.rs`.
3. Move `match_hard_links` and `normalize_pre30_hardlinks` into
   `receiver/file_list/hardlinks.rs`.
4. Leave the `ReceiverContext` extensions in `receiver/file_list/mod.rs`.
5. Tests follow the same split into `file_list/tests/{incremental,hardlinks}.rs`.

### `crates/fast_io/src/iocp/disk_batch.rs` (1061, +63%) - M

1. Split into `iocp/disk_batch/mod.rs` re-exporting `IocpDiskBatch`.
2. `iocp/disk_batch/api.rs` for the public methods (`begin_file`,
   `write_data`, `flush`, `commit_file`, `Write` impl, `Drop`).
3. `iocp/disk_batch/submit.rs` for `submit_write_batch`, `submit_one_write`,
   `drain_completions`, `pinned_overlapped_addr`, `read_offset`.
4. `iocp/disk_batch/handles.rs` for `reopen_overlapped`,
   `close_overlapped_handle`, `ntstatus_to_dos_error`, `zeroed_entry`.
5. Test-injection helpers (`inject_next_write_error_for_test`,
   `clear_injected_write_error_for_test`, `take_injected_write_error`) live in
   `iocp/disk_batch/test_hooks.rs` behind `#[cfg(test)]`.

### `crates/cli/src/frontend/arguments/parser/mod.rs` (915, +41%) - S

1. The file is dominated by a single 850-line `parse_args` function. Extract
   per-clap-group blocks (filters, transfer behaviour, output, daemon flags) as
   private helpers `apply_filter_args`, `apply_transfer_args`, etc.
2. Move helpers into `parser/groups/` (one file per group).
3. Keep `parse_args` as a thin orchestrator that allocates `ParsedArgs` and
   defers to each group helper in sequence.
4. `parse_thread_count` moves to `parser/thread_count.rs` alongside its tests.

### `crates/cli/src/frontend/execution/drive/workflow/run.rs` (882, +36%) - M

1. The file is `execute()` plus `open_log_file()`. Split `execute` into a
   builder-style pipeline by extracting the configuration phases:
   `workflow/run/preflight.rs`, `workflow/run/configure.rs`,
   `workflow/run/dispatch.rs`, `workflow/run/finalize.rs`.
2. Re-implement `execute` in `workflow/run/mod.rs` as a sequencer that calls
   each phase and threads the accumulated state.
3. Move `open_log_file` into `workflow/run/logging.rs`.
4. Add focused unit tests per phase rather than the current monolithic happy
   path.

### `crates/core/src/client/remote/ssh_transfer.rs` (855, +32%) - S

1. Move `ServerProgressAdapter` into `ssh_transfer/progress.rs`.
2. Move `parse_single_remote`, `parse_remote_operands` and
   `build_ssh_connection` into `ssh_transfer/connection.rs`.
3. Move the three transfer drivers (`run_pull_transfer`,
   `run_push_transfer`, `run_proxy_transfer`,
   `run_server_over_ssh_connection`) into `ssh_transfer/drivers.rs`.
4. Move `map_child_exit_status`, `format_stderr_context`,
   `convert_server_stats_to_summary` and the in-file `mod tests` into
   `ssh_transfer/status.rs` and `ssh_transfer/tests/`.
5. `ssh_transfer/mod.rs` becomes the public surface (`run_ssh_transfer`,
   `build_server_config_for_receiver`,
   `build_server_config_for_generator`).

### `crates/cli/src/frontend/command_builder/sections/transfer_behavior_options.rs` (846, +30%) - S

1. The file is a single `add_transfer_behavior_options` function chaining
   ~80 `clap::Arg` definitions.
2. Group args by upstream rsync man-page section: `archive_and_recursion.rs`,
   `compression.rs`, `delete_and_backup.rs`, `partial_and_inplace.rs`,
   `iconv_and_charset.rs`.
3. Each helper takes and returns `ClapCommand`. The top-level function chains
   the helpers in order.
4. No behaviour change; pure mechanical extract.

### `crates/fast_io/src/io_uring/config.rs` (815, +25%) - S

1. Move the public probe surface (`is_io_uring_available`, `sqpoll_fell_back`,
   `IoUringProbeResult`, `check_io_uring_reason`) into `config/probe.rs`.
2. Move the `config_detail` introspection module into `config/kernel.rs`.
3. Move `IoUringConfig::build_ring` and helpers into `config/build.rs`.
4. Move the `mod tests` block into `config/tests.rs`.
5. `config/mod.rs` re-exports the existing surface.

### `crates/rsync_io/src/ssh/builder.rs` (811, +25%) - S

1. Keep `SshCommand` and its `set_*`/`push_*` setters in
   `ssh/builder/mod.rs`.
2. Move `spawn`, `command_parts`, `target_argument`, and the AES-GCM/keepalive
   helpers into `ssh/builder/spawn.rs`.
3. Move `HostKind`, `BuildError`, `parse_host_for_ssh`, `validate_zone_id`,
   and `host_str_for_validation` into `ssh/builder/host.rs`.
4. Move `has_hardware_aes` and `arg_enables_ssh_compression` into
   `ssh/builder/util.rs`.

### `crates/core/src/client/run/mod.rs` (809, +24%) - S

1. Move `LocalCopyOptionsBuilder` and its `apply_*` methods into
   `run/local_copy_options.rs`.
2. Move `apply_max_alloc` and other top-level config helpers into
   `run/config.rs`.
3. Move the two in-file test modules into `run/tests/iconv_wiring.rs` and
   `run/tests/cow_policy_wiring.rs`.
4. `run/mod.rs` keeps `run_client`, `run_client_with_observer`,
   `run_client_internal`, and `build_local_copy_options` only.

### `crates/fast_io/src/io_uring/session_pool.rs` (758, +17%) - S

1. Split into `session_pool/config.rs` for `SessionPoolConfig`,
   `session_pool/pool.rs` for `SessionRingPool` and `RingLease`,
   `session_pool/thread_local.rs` for `ThreadLocalRingPool` and its lease.
2. Move `build_ring` to `session_pool/ring.rs`.
3. Keep `session_pool/mod.rs` as a thin re-export.

### `crates/transfer/src/receiver/mod.rs` (741, +14%) - S

This is borderline. Extract the largest impl block (likely
`ReceiverContext` finalize helpers) into `receiver/finalize.rs` and the
`ReceiverError` plumbing into `receiver/errors.rs`. Aim for a 600-line core
`mod.rs`.

### `crates/engine/src/delete/context.rs` (739, +14%) - S

1. Move `EmitterTiming` and the `From` conversions to
   `delete/context/timing.rs`.
2. Move `DrainOutcome` to `delete/context/drain.rs`.
3. Move the in-file `mod tests` to `delete/context/tests/`.
4. `delete/context/mod.rs` keeps `DeleteContext` plus its builder methods.

### `crates/cli/src/frontend/arguments/parser/tests.rs` (707, +9%) - S

Group tests by clap group, mirroring the proposed `parser/groups/` split:
`tests/filters.rs`, `tests/transfer.rs`, `tests/output.rs`,
`tests/daemon.rs`. Shared fixtures move to `tests/support.rs`.

### `crates/core/src/client/remote/async_ssh_transport.rs` (651, +0%) - S

One line over. Either inline a short helper to reclaim space or add a focused
override entry to `tools/line_limits.toml` once a follow-up is filed. Preferred
fix: move the `mod tests` (if present) into `async_ssh_transport/tests.rs`.

## Operational notes

- All counts are physical line counts (`wc -l`) and match the script's
  `count_file_lines` behaviour.
- 305 additional files (not touched in the last 100 commits) currently fail the
  check. They are out of scope for this audit but should be tracked separately
  before `enforce-limits` is promoted to a required CI check.
- `enforce-limits` runs in CI as an informational (non-blocking) job
  (`continue-on-error: true`) on every push and pull request. Promote to a
  required check on branch protection once the over-limit count reaches zero.
- Until decomposition lands, contributors can also invoke `enforce-limits`
  manually before publishing a release branch so the warn-threshold output is
  visible locally.
