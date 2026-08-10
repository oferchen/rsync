# UTS-V3.D files-from 4th-invocation hang root-cause audit

Audit-only design doc. No code changes ship with this PR.

## Scope

The upstream rsync 3.4.4 testsuite `testsuite/files-from.test` hangs the
4th `checkit` invocation for 300 s on oc-rsync, then `runtests.py` aborts.
This doc enumerates every rsync invocation in the test, pins the transport
of the 4th one, identifies code sites where the receiver can block
indefinitely, and specifies the additional fix needed beyond the
already-shipped UTS-DD-files-from.5 secluded-args drain (PR #5941).

## Upstream test enumeration

Upstream source (identical bytes in 3.4.1 and 3.4.4):
`target/interop/upstream-src/rsync-3.4.4/testsuite/files-from.test`.

| # | Test line | Form | Transport |
|---|-----------|------|-----------|
| 1 | 27 | `$RSYNC -a --exclude=...` (chkdir setup) | local |
| 2 | 29 | `$RSYNC -av --files-from=<local> <local> <local>` | local |
| 3 | 40 iter 1 | `filehost='' srchost=''` -> `desthost=localhost:` | **lsh.sh PUSH** |
| 4 | 40 iter 2 | `filehost='' srchost=localhost:` -> `desthost=''` | **lsh.sh PULL** |
| 5 | 40 iter 3 | `filehost=localhost: srchost=''` -> `desthost=localhost:` | **lsh.sh PUSH**, `--files-from=localhost:...` |
| 6 | 40 iter 4 | `filehost=localhost: srchost=localhost:` -> `desthost=''` | **lsh.sh PULL**, `--files-from=localhost:...` |

The shipped UTS-DD-files-from.5 fix (commit `b3c53530a`, PR #5768) targets
the lsh.sh PULL with `--files-from=localhost:...`, which is row 6 above.
Row 6 is "the 4th invocation" counted from the `checkit` line at 29
(row 2 is the first `checkit`, rows 3-6 are the loop's four `checkit`
calls; the loop's 4th `checkit` is row 6).

## Transport for the 4th invocation

Row 6: `lsh.sh PULL with filehost prefix`. Cited at upstream
`files-from.test:40` inside the `for filehost in '' 'localhost:'` x
`for srchost in '' 'localhost:'` nested loop. When `srchost='localhost:'`
the test sets `desthost=''` (line 36) -> the local oc-rsync is the
**client-receiver**, the remote rsync (spawned by `lsh.sh` -> `$RSYNC --server --sender`)
is the source. The filehost prefix on `--files-from` triggers upstream
`options.c:3112-3138 check_for_hostspec`, which strips the host and
forwards `--files-from <path>` to the remote server. The server then opens
the file locally and `start_filesfrom_forwarding` writes the bytes back
to the sender's `filesfrom_fd`.

## Receiver hang sites

### Site A: receiver still holds a working forward path (already shipped, NOT the hang)

`crates/transfer/src/receiver/transfer/setup.rs:485-533`
`forward_files_from_to_sender` fires for `!client_mode &&
files_from_path != "-"`. This is the SERVER-receiver case, not the
client-receiver case. Row 6 is `client_mode=true`, so this function
returns at line 489 (`if self.config.connection.client_mode { return
Ok(()); }`) without forwarding.

### Site B: client-receiver path emits but does not drain

`crates/transfer/src/lib.rs:565-575`:

```rust
// upstream: main.c:1354-1356 - after sending filter list, forward
// pre-read --files-from data to the remote daemon's generator so it
// can build the file list from the forwarded filenames.
// This applies only in client-mode pull (Receiver), where the daemon's
// generator reads filenames from the protocol stream.
if config.connection.client_mode && config.role == ServerRole::Receiver {
    if let Some(data) = config.connection.files_from_data.take() {
        writer.write_all(&data)?;
        writer.flush()?;
    }
}
```

The local client-receiver has the files-from bytes pre-staged in
`config.connection.files_from_data` and writes them out as a single
buffered burst. If the client never *populates* `files_from_data` when
the `--files-from` path uses a `host:path` form, the receiver dispatches
zero bytes, the remote sender blocks at `recv_filesfrom` for 300 s, and
the test times out. The trigger is path resolution at the client.

### Site C: client-side resolver may drop the host prefix on the wrong branch

`crates/cli/src/frontend/execution/file_list/parser.rs::resolve_files_from_source`
is the load-bearing parser for `--files-from`. Per UTS-DD-files-from.5
(`b3c53530a`), it handles `host:path` by skipping the local-file load and
deferring to the server. For *PULL* (client = receiver), the resolver
must still *open the local file when the host part is empty* and stage
the bytes into `files_from_data`. If the parser folds
`localhost:scratch/filelist` and `:scratch/filelist` into the same
"remote" branch and never opens any local file at the client, then on
PULL the remote server expects the client to forward the bytes -
nothing arrives, and `recv_filesfrom` blocks.

### Site D: secluded-args terminator drain (UTS-DD-files-from.5 covers row 5 PUSH)

`crates/protocol/src/secluded_args.rs:142-190 recv_secluded_args` reads
NUL-delimited args until an empty arg appears as the terminator. PR
#5941 (commit `0c83f93013`) encodes empty mid-list args as `.\0` so the
trailing arg-list NUL is unambiguous. This closes row 5 (PUSH) where the
sender emits the arg payload over stdin to `--server`. Row 6 is PULL;
the local client is not the secluded-args receiver, so SITE D does not
gate row 6 directly. The shipped fix is necessary but not sufficient
for the 4th invocation hang on master HEAD.

## Cross-check vs UTS-DD-files-from.5 scope

| Fix | Code path | Closes |
|-----|-----------|--------|
| UTS-DD-files-from.5 (#5941, `0c83f93013`) | `protocol::secluded_args::send_secluded_args` empty-arg encoding | PUSH-direction hangs (row 3, row 5) |
| UTS-DD-files-from.5 prior (#5768, `b3c53530a`) | `forward_files_from_to_sender` on SERVER-receiver + landlock allowlist | Row 5 push-with-filehost and row 6 pull-with-filehost when the local *server* side is oc-rsync |
| Pending (this audit) | client-side `resolve_files_from_source` populating `files_from_data` for PULL when host part is local | Row 6 when the local *client* is oc-rsync and the remote sender expects the receiver to push the list |

The hang reported under UTS-V3.D therefore lands on the local-client
arm of row 6, not on the server arm that #5768 fixed. The remote
upstream sender works fine; oc-rsync's local client-receiver fails to
ship the files-from payload because the parser treats `localhost:path`
as a remote-only resource and never stages bytes into
`config.connection.files_from_data`.

## Fix specification

**One-line:** Resolve `localhost:path` `--files-from` source in the
*client* arm as both remote (for argv forwarding) and local (open the
file, stage NUL-delimited bytes into `files_from_data`) so the PULL
client-receiver has a buffer to flush at `lib.rs:570`.

### Detail

1. In `crates/cli/src/frontend/execution/file_list/parser.rs::resolve_files_from_source`,
   add a branch: when the parsed hostspec resolves to `localhost` AND the
   path is openable on the client, eagerly read the file and emit
   `FilesFromSource::Hybrid { local_bytes, wire_arg }`. Wire-arg uses the
   stripped path form for server-side argv (matches upstream
   `options.c:3112-3138`).
2. In the orchestration layer (`crates/core/src/client/remote/*.rs`
   `server_config.connection.files_from_data = Some(data)` sites at
   `ssh_transfer.rs:369`, `embedded_ssh_transfer.rs:176`,
   `daemon_transfer/orchestration/transfer.rs:66`), populate
   `files_from_data` from the `Hybrid::local_bytes` payload on PULL when
   the resolver returned hybrid.
3. Defensive timeout: add a 30 s connect-stage deadline to the
   `recv_files_from` blocking read on the receiver path so a future
   resolver regression surfaces as `ETIMEDOUT (code 30)` instead of a
   300 s testsuite hang. Upstream `io.c:374-381 forward_filesfrom_data`
   has the same shape; oc-rsync's protocol-level read should match.

The wire bytes remain identical to upstream: the server arg list keeps
`--files-from <stripped-path>`, and the client emits the file payload as
a NUL-delimited stream before the flist exchange.

## Test plan (nextest)

`crates/transfer/tests/files_from_lsh_repeated.rs` already pins rows 3,
4 wire bytes. Extend with:

1. **`files_from_lsh_localhost_pull_4th`**: PULL via `lsh.sh` with
   `--files-from=localhost:<path>`. Local client = oc-rsync receiver,
   remote = upstream `rsync --server --sender`. Assert `files_from_data`
   is populated, the writer emits the payload before flist read, and
   completion within 10 s (300 s is the hang ceiling).
2. **`files_from_resolver_hybrid_localhost`**: unit test for
   `resolve_files_from_source("localhost:/tmp/foo")` returning the
   hybrid variant; assert `local_bytes` is the file contents and
   `wire_arg` is `/tmp/foo`.
3. **`files_from_recv_timeout`**: synthetic receiver with no incoming
   filesfrom payload. Assert `ETIMEDOUT` at 30 s instead of indefinite
   block.

CI: wire into the upstream-testsuite IFX-15.a cell so the full
`files-from.test` exercises all 4 loop iterations end-to-end.

## References

- upstream `target/interop/upstream-src/rsync-3.4.4/testsuite/files-from.test`
- upstream `main.c:1173-1180` `start_filesfrom_forwarding`
- upstream `options.c:3112-3138` `check_for_hostspec`
- upstream `io.c:370-381` `forward_filesfrom_data`
- oc-rsync `crates/transfer/src/receiver/transfer/setup.rs:485` server-side forwarder
- oc-rsync `crates/transfer/src/lib.rs:565-575` client-side emit
- oc-rsync `crates/cli/src/frontend/execution/file_list/parser.rs` resolver
- oc-rsync `crates/protocol/src/secluded_args.rs:142` recv loop
- PR #5768 (`b3c53530a`) UTS-DD-files-from.5 prior server-side fix
- PR #5941 (`0c83f93013`) UTS-DD-files-from.5 empty-arg drain fix
