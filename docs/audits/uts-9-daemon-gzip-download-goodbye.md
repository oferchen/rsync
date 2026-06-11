# UTS-9: daemon-gzip download goodbye flush

Status: verification gap closed
Tracks: UTS-9.REOPEN (#3890), sub-tasks #3958-#3962
Companion fix: PR #5609 (UTS-15.c)

## Symptom

An oc-rsync client pulling a file from an oc-rsync daemon with `-zz`
(new-style compression) observed an early connection close at protocol
byte ~612425, mid-stream. The receiver reported
`connection unexpectedly closed (... bytes received so far)` and exited
with a non-zero status. No `@ERROR` frame reached the wire, so the
diagnostic surface was empty: the symptom was a torn capture between the
final NDX_DONE and the transport FIN.

The cutoff at ~612425 bytes is the size of the multiplexed window in
which the trailing NDX_DONE + any post-goodbye MSG_INFO frame became
visible to the client kernel. The exact byte depends on negotiated
buffer sizes; the symptom is the absence of an orderly goodbye, not a
specific byte offset.

## Codepath analysis

The daemon-pull `-zz` codepath exercises the server-sender role:

1. Client invokes `oc-rsync -azz rsync://daemon/module/file dst/`.
2. Daemon spawns a worker that runs `run_server_with_handshake`
   (`crates/transfer/src/lib.rs:308`).
3. `config.role == ServerRole::Generator` dispatches to
   `GeneratorContext::run()`
   (`crates/transfer/src/generator/transfer/orchestrator.rs:36`).
4. `run()` drives the transfer loop, sends server stats, then calls
   `handle_goodbye()`
   (`crates/transfer/src/generator/transfer/goodbye.rs:46`).
5. For protocol >= 29 (which all `-zz` transfers use since zstd/zlibx
   require a modern protocol), `handle_goodbye` performs its own
   `writer.flush()` after writing the final NDX_DONE
   (`goodbye.rs:102`).
6. Control returns to `run()`.

Before the fix, step 6 returned without an additional flush. Any
diagnostic frame queued after `handle_goodbye` (cumulative INC_RECURSE
totals, debug-log MSG_INFO frames, etc.) could remain in the buffered
writer when the underlying transport closed. The kernel then sent the
FIN before the trailing frames, producing the silent close symptom.

## Cross-check vs UTS-15.c batch-mode

PR #5609 (UTS-15.c) added the explicit post-`handle_goodbye` flush to
fix the upstream batch-mode interop suite, which surfaced the same
class of defect at a different protocol offset
(~2241725 bytes, inside the file-list framing region). The two
symptoms share a root cause: `GeneratorContext::run()` did not enforce
upstream's `io_flush(FULL_FLUSH)` contract before returning.

The fix location is the **same** for both lineages:

```
crates/transfer/src/generator/transfer/orchestrator.rs
  GeneratorContext::run()
    handle_goodbye(...)?;
    if let Err(e) = writer.flush() {
        if !is_early_close_error(&e) {
            return Err(e);
        }
    }
```

This mirrors upstream `main.c:983` (`do_server_sender()`) and
`main.c:1344` (`client_run()`), both of which call
`io_flush(FULL_FLUSH)` immediately before returning. Because every
daemon-sender path - batch-mode (UTS-15.c) and `-zz` daemon-pull
(UTS-9) - flows through the same `run()` exit point, a single flush
closes both gaps.

## Verification

The UTS-9 regression test lives in
`crates/core/tests/uts_9_daemon_gzip_download_goodbye.rs`:

- Spawns an oc-rsync daemon serving a deterministic ~700 KB file
  (sized to clear the 612425-byte cutoff).
- Pulls the file with `oc-rsync -azz --timeout=30
  rsync://localhost:N/testmodule/uts9.bin`.
- Asserts exit code 0.
- Asserts stderr does not contain the
  `connection unexpectedly closed` signature.
- Asserts the destination matches the source byte-for-byte.

The test is `#[ignore]` because it requires the oc-rsync binary and an
OS-assigned TCP port; it runs during interop validation rather than
default CI.

## Invariants

- **Flush invariant** (shared with UTS-15.c): every successful
  `GeneratorContext::run()` ends with a `writer.flush()` call.
  NDX_DONE is the last byte on the wire before the transport FIN.
- **Wire signature invariant**: `connection unexpectedly closed`
  appearing in client stderr during a daemon-pull `-zz` transfer is a
  defect, not a transient. The regression test guards the signature so
  a future regression cannot mask the symptom behind a successful exit
  code.

## Upstream references

- `main.c:983` `do_server_sender()` - `io_flush(FULL_FLUSH)` before
  return
- `main.c:1344` `client_run()` - `io_flush(FULL_FLUSH)` before return
- `main.c:875-906` `read_final_goodbye()` - goodbye exchange contract
- `options.c:2011-2012` `-zz` -> `compress_choice = "zlibx"` mapping
