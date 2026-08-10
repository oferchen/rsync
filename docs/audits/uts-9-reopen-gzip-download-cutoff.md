# UTS-9.REOPEN: daemon-gzip-download 612425-byte cutoff audit

Status: investigation / audit only (no production code change in this PR)
Tracks: UTS-9.REOPEN (#3890), sub-tasks #3958 / #3959 / #3960
Companion fix already on master: PR #5609 (UTS-15.c) - explicit `writer.flush()`
after `handle_goodbye` in `GeneratorContext::run()`
Companion regression test already on master: commit `dbaf81fec`
(`crates/core/tests/uts_9_daemon_gzip_download_goodbye.rs`)
Feed-forward: UTS-9.REOPEN.4 (additional implementation hardening) and .5
(regression test extension) - both out of scope for this audit PR.

## 1. Reported symptom

When the upstream `daemon-gzip-download` testsuite test runs against an
oc-rsync daemon, the oc-rsync client (acting as receiver) closes mid-stream
at protocol byte ~612425. Concretely:

- Client cmdline shape: `oc-rsync -aizz --timeout=30 rsync://localhost:N/testmodule/file.bin dst/`
- Client stderr: `connection unexpectedly closed (612425 bytes received so far) [receiver]`
- Client exit code: 12 (`STREAMIO`)
- No `@ERROR` frame reaches the wire. The diagnostic surface is empty:
  the byte count is the only signal.

The cutoff byte is not a hard upstream-rsync constant. It is the byte at
which the receiver's buffered read sees a transport FIN with no orderly
`NDX_DONE` having landed first. The numeric value reflects the negotiated
multiplex window plus the test fixture size and is sensitive to negotiated
buffer sizes (SO_RCVBUF, daemon write batching, codec frame boundaries).

## 2. Daemon-sender goodbye path trace (REOPEN.2)

The daemon-pull `-zz` codepath flows through the server-sender role:

| Step | File / line | Role |
|------|-------------|------|
| 1 | `crates/daemon/src/daemon/sections/module_access/transfer.rs` | Daemon worker dispatches transfer after auth + module-select |
| 2 | `crates/transfer/src/lib.rs:308` `run_server_with_handshake` | Role dispatch on `config.role` |
| 3 | `crates/transfer/src/generator/transfer/mod.rs` `GeneratorContext::run()` | Sender pipeline driver |
| 4 | `crates/transfer/src/generator/transfer/orchestrator.rs:107` `run_transfer_loop` | Phase = DeltaTransfer; emits data blocks |
| 5 | `crates/transfer/src/generator/transfer/orchestrator.rs:113` `advance_to(Finalization)` | FSM transition. Equivalent of upstream phase = 3 (`receiver.c::recv_files` exit) |
| 6 | `crates/transfer/src/generator/transfer/orchestrator.rs:123` `send_stats` (server-sender only) | Mirrors `main.c:960-962 do_server_sender()` `handle_stats` |
| 7 | `crates/transfer/src/generator/transfer/orchestrator.rs:128` `handle_goodbye` | Reads receiver NDX_DONE, sends NDX_DEL_STATS + NDX_DONE, reads final NDX_DONE |
| 8 | `crates/transfer/src/generator/transfer/goodbye.rs:102` inline `writer.flush()` | Flush after the writer's own NDX_DONE |
| 9 | `crates/transfer/src/generator/transfer/orchestrator.rs:142` post-`handle_goodbye` `writer.flush()` | UTS-15.c defense-in-depth flush mirroring `main.c:968 io_flush(FULL_FLUSH)` |
| 10 | `crates/transfer/src/generator/transfer/orchestrator.rs:158-216` diagnostic counters | INC_RECURSE I3/I4/I5 logs (no wire output) |

Annotated excerpt of the critical exit sequence (orchestrator.rs:118-146):

```rust
if !self.config.connection.client_mode {
    let flist_buildtime = calculate_duration_ms(self.timing.flist_build_start, self.timing.flist_build_end);
    let flist_xfertime = calculate_duration_ms(self.timing.flist_xfer_start, self.timing.flist_xfer_end);
    self.send_stats(writer, &transfer_result, flist_buildtime, flist_xfertime)?;
}

let mut ndx_read_codec = transfer_result.ndx_read_codec;
let mut ndx_write_codec = transfer_result.ndx_write_codec;
self.handle_goodbye(reader, writer, &mut ndx_read_codec, &mut ndx_write_codec)?;

// upstream: main.c:968 do_server_sender() and main.c:912 client_run()
// both call io_flush(FULL_FLUSH) immediately before returning so the
// kernel ships the final NDX_DONE (and any trailing multiplexed
// MSG_INFO frames) before the transport FIN. We mirror that contract
// here as defense-in-depth ...
if let Err(e) = writer.flush() {
    if !super::super::is_early_close_error(&e) {
        return Err(e);
    }
}
```

Without step 9 (added by PR #5609), any post-goodbye buffered byte - a
delayed multiplex MSG_INFO log frame, a diagnostic counter print that
inadvertently routes through the multiplexed writer, or the codec's own
trailing block - would be left in the kernel send buffer when the
generator thread returned and the daemon worker shut the connection.
The receiver kernel saw the FIN before the trailing frames, producing
the silent close at byte 612425.

The path from `handle_goodbye` outward is identical for `-z` (zlib),
`-zz` (zlibx), and `-zz` over zstd. Compression is orthogonal to the
goodbye contract: the gzip frame layout does not change the byte at
which a torn close becomes observable; it only changes the window at
which buffered output crystallises. That is why the symptom byte is
not a fixed offset across capture runs.

## 3. UTS-15.c coverage analysis (REOPEN.3)

PR #5609 added the explicit post-goodbye `writer.flush()` to fix the
upstream batch-mode interop suite, which surfaced at a different
protocol offset (~2241725 bytes). Both lineages share a single root
cause: `GeneratorContext::run()` did not enforce upstream's
`io_flush(FULL_FLUSH)` contract before returning.

File-by-file coverage table:

| Site | UTS-15.c batch-mode | UTS-9 daemon `-zz` pull | Shared? |
|------|---------------------|--------------------------|---------|
| `crates/transfer/src/lib.rs::run_server_with_handshake` | yes | yes | shared entry |
| `crates/transfer/src/generator/transfer/mod.rs::GeneratorContext::run` | yes | yes | shared driver |
| `crates/transfer/src/generator/transfer/goodbye.rs::handle_goodbye` | yes | yes | shared finalizer |
| `crates/transfer/src/generator/transfer/orchestrator.rs:142` post-goodbye flush | yes (UTS-15.c shipped it) | yes (UTS-9 inherits it) | shared fix |
| `crates/transfer/src/generator/transfer/orchestrator.rs:123` `send_stats` | guarded by `!client_mode`; UTS-15.c daemon batch path takes it | guarded by `!client_mode`; UTS-9 daemon `-zz` pull takes it | shared |
| `crates/daemon/.../module_access/transfer.rs` worker dispatch | yes | yes | shared |
| `crates/daemon/.../module_access/client_args.rs::apply_long_form_args` batch-flag rejection | UTS-15.c-distinct (`--write-batch` / `--read-batch`) | not applicable to `-zz` | UTS-15.c-only |
| `crates/transfer/src/.../arguments.rs::build_full_daemon_args` `--write-batch` strip | UTS-15.c-distinct | not applicable | UTS-15.c-only |

Conclusion: the **finalisation flush** introduced for UTS-15.c is the
same flush that closes the UTS-9 lineage. The two regressions were
discovered against different upstream tests but the structural defect
is identical. The UTS-15.b argv strip and UTS-15.g argv rejection paths
are UTS-15.c-distinct; they are unrelated to UTS-9 and do not need to
fire on the `-zz` path.

A regression test for the UTS-9 path is already on master in
`crates/core/tests/uts_9_daemon_gzip_download_goodbye.rs` (commit
`dbaf81fec`). It spawns an oc-rsync daemon serving a deterministic
~700 KB fixture (sized to clear the 612425-byte cutoff), pulls with
`oc-rsync -azz`, and asserts:

- exit code 0,
- stderr does not contain `connection unexpectedly closed`,
- destination matches source byte-for-byte.

The `EDG-GOODBYE` series (commit `37230ee01`,
`crates/protocol/tests/goodbye_contract.rs`) adds wire-byte goldens,
stress, and proptest coverage that the trailing wire frame is
NDX_DONE across protocols 28-32 with no trailing bytes, independent
of compression.

## 4. Byte 612425 hypothesis

Given the goodbye structure and the gzip frame layout, the cutoff at
~612425 is best explained as a buffered-frame retention boundary, not
a protocol-defined offset:

1. The transfer body for the fixture occupies the bulk of the bytes
   leading up to ~612 KB. The receiver's per-multiplex-frame buffered
   reader has consumed the body without difficulty.
2. After the body, the generator emits in order:
   `MSG_STATS` (server-sender path) -> echo of receiver `NDX_DONE` ->
   optional `NDX_DEL_STATS` -> generator-side `NDX_DONE` -> any
   trailing `MSG_INFO` diagnostic frame.
3. The codec batches its output under the multiplexed writer. For
   `-zz` the codec emits the next compressed block only when it has
   enough input to fill a frame OR when it is forced to flush.
4. `handle_goodbye::goodbye.rs:102` already flushes after writing
   NDX_DONE. But any byte queued **after** `handle_goodbye` returns -
   in particular the post-`run_transfer_loop` diagnostic INC_RECURSE
   counters routed through the multiplexed writer, or the codec's
   own trailing-byte sync - has no flush gate between it and the
   transport shutdown.
5. The daemon worker thread returns from `run()`, the per-connection
   socket is dropped, and Linux delivers FIN to the receiver. The
   receiver, mid-frame in its buffered reader, observes a short read
   on byte 612425 and reports `connection unexpectedly closed`.

The 612425 number itself is the file size plus the cumulative protocol
overhead up to the point the codec releases its last compressed block.
On a different host, with different TCP window sizes or a different
fixture, the cutoff would be a different integer. The constant in
this case happened to be reliable enough to pattern-match in tickets.

This hypothesis predicts that the fix at orchestrator.rs:142 -
forcing a `writer.flush()` immediately after `handle_goodbye` -
closes both the codec-trailing-byte case and the post-goodbye
diagnostic-frame case in one stroke. The regression test in
`uts_9_daemon_gzip_download_goodbye.rs` confirms this prediction:
the test passes on master with the flush in place.

## 5. tcpdump reproduction (REOPEN.1)

The standing CLAUDE.md / `feedback_container_debug` guidance is to use
the persistent `rsync-profile` podman container for byte-level wire
captures. The exact command set to reproduce the 612425 cutoff against
a daemon running pre-PR #5609:

```sh
# host (one window)
podman exec -it rsync-profile bash

# inside rsync-profile (window A) - serve a deterministic 700 KB file
mkdir -p /tmp/uts9-src /tmp/uts9-dst
head -c 700000 /dev/urandom > /tmp/uts9-src/uts9.bin
cat > /tmp/uts9-rsyncd.conf <<'EOF'
use chroot = no
port = 8730
log file = /tmp/uts9-rsyncd.log
[testmodule]
    path = /tmp/uts9-src
    read only = yes
EOF
oc-rsync --daemon --no-detach --config=/tmp/uts9-rsyncd.conf &
DAEMON_PID=$!

# inside rsync-profile (window B) - capture and reproduce
mkdir -p /tmp/uts9-cap
sudo tcpdump -i lo -nn -X -s 0 'tcp port 8730' \
    -w /tmp/uts9-cap/uts9.pcap &
TCPDUMP_PID=$!
sleep 1
oc-rsync -aizz --timeout=30 \
    rsync://localhost:8730/testmodule/uts9.bin \
    /tmp/uts9-dst/ 2>&1 | tee /tmp/uts9-cap/client.log
echo "client exit=$?"
sudo kill "$TCPDUMP_PID"
kill "$DAEMON_PID"

# decode (capture summary)
tcpdump -r /tmp/uts9-cap/uts9.pcap -nn -X | tail -200 > /tmp/uts9-cap/tail.txt
wc -c /tmp/uts9-dst/uts9.bin   # short read on pre-fix builds
grep -c 'connection unexpectedly closed' /tmp/uts9-cap/client.log
```

Expected pre-fix observation: client exit code 12, stderr contains
`connection unexpectedly closed (612425 bytes received so far)`, and
`tail.txt` shows the daemon-side FIN immediately following a
multiplexed frame header without the expected NDX_DONE payload bytes.
Expected post-fix (current master) observation: client exit 0,
destination file byte-identical to source, NDX_DONE is the last
multiplexed payload before the orderly FIN.

The capture is documented as the command set rather than committed as
a pcap because (a) cutoff byte varies by host RNG / TCP buffer sizes,
(b) the fix already shipped (PR #5609), and (c) the in-process
regression test in `uts_9_daemon_gzip_download_goodbye.rs` exercises
the same code path deterministically without requiring the container.

## 6. Feed-forward for UTS-9.REOPEN.4 and .5

**.4 (implementation):** No additional production-code fix is needed
beyond what PR #5609 already shipped at
`crates/transfer/src/generator/transfer/orchestrator.rs:142`. Mark
UTS-9.REOPEN.4 as covered by PR #5609. If future work expands the
post-goodbye block (for example a hypothetical `MSG_STATS_2` extension
or additional diagnostic frames routed through the multiplexed
writer), the same flush must remain the last operation before
`GeneratorContext::run()` returns. The `EDG-GOODBYE` proptest at
`crates/protocol/tests/goodbye_contract.rs` will fail loudly if a
future change emits any wire payload after NDX_DONE.

**.5 (regression test):** The shipped test
`crates/core/tests/uts_9_daemon_gzip_download_goodbye.rs` covers the
oc-rsync-vs-oc-rsync `-zz` round-trip. A natural extension is a second
test cell exercising `-z` (legacy zlib) on the same fixture, on the
hypothesis that the goodbye flush is codec-orthogonal. The shipped
EDG-GOODBYE.1 / .3 contract tests already lock the protocol-32-and-32
NDX_DONE invariant independent of codec, so this is belt-and-braces.

## 7. Cross-references

- `docs/audits/uts-9-daemon-gzip-download-goodbye.md` - prior
  audit (codepath analysis + UTS-15.c cross-check + verification).
- `docs/audits/uts-15-batch-mode-daemon-arg-defense.md` - UTS-15.b/.c/.g
  three-defense write-up; the .c flush is the shared mechanism with UTS-9.
- `docs/audits/edg-goodbye-contract.md` - machine-checked NDX_DONE
  trailing-byte invariant across protocols 28-32.
- `crates/transfer/src/generator/transfer/orchestrator.rs:128-146` -
  current production fix site.
- `crates/transfer/src/generator/transfer/goodbye.rs:46-136` -
  goodbye exchange implementation.
- `crates/core/tests/uts_9_daemon_gzip_download_goodbye.rs` -
  shipped regression test.
- `crates/protocol/tests/goodbye_contract.rs` - shipped wire-byte
  contract suite (EDG-GOODBYE.1/.2/.3).

## 8. Upstream references

- `rsync-3.4.1/main.c:960-962` `do_server_sender()` - `io_flush` then
  `handle_stats` before `read_final_goodbye`.
- `rsync-3.4.1/main.c:968` `do_server_sender()` - `io_flush(FULL_FLUSH)`
  before return.
- `rsync-3.4.1/main.c:912` `client_run()` - matching
  `io_flush(FULL_FLUSH)` before return on the client side.
- `rsync-3.4.1/main.c:875-906` `read_final_goodbye()` - goodbye
  exchange contract; explains the round-trip NDX_DONE semantics.
- `rsync-3.4.1/options.c:2011-2012` - `-zz` to `compress_choice = "zlibx"`
  mapping; confirms the negotiated codec on the failing transfer.
