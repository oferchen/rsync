# UTS-15: batch-mode daemon argument defense



Status: shipped
Tracks: UTS-15.b (#3729), UTS-15.c (#3730), UTS-15.g (#3743)

## Symptom

Upstream's `batch-mode` interop suite repeatedly drove the oc-rsync daemon
to a silent connection close at protocol byte ~2241725 (inside the file-list
framing region). The triggering corpus was a client invocation that named
`--write-batch=PATH` or `--read-batch=PATH` against an oc-rsync daemon
module. The defect surfaced as a `Connection reset by peer` without any
`@ERROR` reply on the wire, so the client received no actionable diagnostic.

## Root causes

Three independent gaps combined to produce the silent close:

### 1. Client-side argv leakage (UTS-15.b)

`crates/core/src/client/remote/daemon_transfer/orchestration/arguments.rs`
builds the argv sent to the daemon. Today no code path emits the batch
flag family, but the builder had no explicit guard. A future refactor that
fans `remote_options` or any other user-supplied list into the daemon path
would silently reintroduce the leak.

Upstream `options.c:server_options()` deliberately omits
`--write-batch` / `--read-batch` from server argv. The sole related token
upstream emits is the literal `--only-write-batch=X` placeholder at
`options.c:2832-2833`, and only when `write_batch < 0` (dry-run write).

### 2. Goodbye flush not contract-enforced (UTS-15.c)

`crates/transfer/src/generator/transfer/goodbye.rs::handle_goodbye()`
already flushes after `write_ndx_done()`, but the orchestrator that drives
the run did not guarantee a final flush at end-of-`run()`. Any diagnostic
frame queued AFTER the goodbye exchange (debug logs, stats summaries) could
race the transport FIN and end up in a torn wire capture.

Upstream `main.c:912 client_run()` and `main.c:968 do_server_sender()`
both call `io_flush(FULL_FLUSH)` immediately before returning, so the
kernel ships the final NDX_DONE (plus any trailing multiplexed MSG_INFO
frames) before the FIN.

### 3. Silent unknown-arg drop in daemon (UTS-15.g)

`crates/daemon/src/daemon/sections/module_access/client_args.rs::apply_long_form_args()`
walked the client argv with a fall-through `_` arm that silently ignored
anything it didn't recognise. When a batch flag reached this layer (because
the client-side sanitiser failed), the daemon never sent `@ERROR`. The wire
just stalled.

Upstream `options.c:1444-1449` is loud-by-default: an unknown option in
daemon mode emits `rsync: <BAD>: <err> (in daemon mode)` and jumps to
`daemon_error:` at `options.c:1464-1466`, which exits `RERR_SYNTAX`.

## Fixes

### UTS-15.b: defensive strip

Added `strip_client_only_batch_flags()` as the final step of
`build_full_daemon_args()`. The helper removes `--write-batch`,
`--only-write-batch`, and `--read-batch` in both bare-flag and key=value
forms, and consumes a trailing positional value for the two-arg form so it
does not become an orphan path argument.

Test coverage:
- `build_full_args_strips_write_batch_flag`
- `build_full_args_strips_read_batch_flag_and_orphan_value`
- `build_full_args_strips_only_write_batch_flag`
- `build_full_args_default_path_emits_no_batch_flags`
- Unit tests on `strip_client_only_batch_flags` directly for both forms.

### UTS-15.c: explicit pre-return flush

Added an explicit `writer.flush()` call at the end of
`GeneratorContext::run()` in `transfer/orchestrator.rs`, after the goodbye
exchange completes. Failure is propagated unless the peer already closed,
following the existing `is_early_close_error` tolerance pattern.

Test coverage:
- `handle_goodbye_proto31_flushes_ndx_done_before_close` uses a
  `FlushTrackingWriter` to record every `write` and `flush` call. The test
  asserts (a) the wire buffer ends with the NDX_DONE marker bytes, (b) at
  least one flush occurred, and (c) the final operation before return was
  a flush (not a partial write).

### UTS-15.g: fail-loud unknown args

Refactored `apply_long_form_args()` to return `Option<String>` carrying the
first client-only batch flag it encounters. The caller (`build_server_config`)
checks the return value and, when set, logs an `rsync_warning!`, writes an
`@ERROR: <flag>: unrecognized option (in daemon mode)` to the client, and
closes the connection with `RERR_SYNTAX` semantics.

The fail-loud surface is scoped to client-only batch flags rather than
every unrecognised long form: many wire-only markers (`--server`,
`--sender`, `--checksum-choice`, etc.) intentionally bypass this parser,
and rejecting them would regress every existing daemon transfer. A future
extension can broaden the validation set once each wire-only marker has an
explicit allow-list entry.

Test coverage:
- `apply_long_form_args_reports_write_batch_kv_as_unknown`
- `apply_long_form_args_reports_read_batch_kv_as_unknown`
- `apply_long_form_args_reports_only_write_batch_kv_as_unknown`
- `apply_long_form_args_recognised_args_do_not_report_unknown`
- `apply_long_form_args_positional_paths_are_not_classified`

## Invariants

- **Argv invariant**: `--write-batch`, `--only-write-batch`, and
  `--read-batch` never reach the daemon argv. The strip happens at the
  builder boundary so a future caller refactor cannot accidentally bypass
  it.
- **Flush invariant**: every successful `GeneratorContext::run()` ends with
  a `writer.flush()` call. NDX_DONE is the last byte on the wire before
  the transport FIN.
- **Rule 12 fail-loud invariant**: an unrecognised client-only batch flag
  produces an explicit `@ERROR` frame and a non-zero exit. Silent close
  is a defect.

## Upstream references

- `options.c:784-786` - `read-batch` / `write-batch` / `only-write-batch`
  popt entries (client-only)
- `options.c:1444-1449` - daemon-mode unknown option error path
- `options.c:1464-1466` - `daemon_error:` exits `RERR_SYNTAX`
- `options.c:2832-2833` - the sole `--only-write-batch=X` placeholder
- `main.c:912` `client_run()` - `io_flush(FULL_FLUSH)` before return
- `main.c:968` `do_server_sender()` - `io_flush(FULL_FLUSH)` before return
- `main.c:875-906` `read_final_goodbye()` - goodbye exchange contract
