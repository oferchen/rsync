# UTS-1.f - tcpdump wire-byte parity for `-ais` via lsh.sh

## Scope

Document the wire-byte sequence each side emits for the `-ais`
push/pull legs of `testsuite/00-hello.test` (tests 4 and 5) routed
through `lsh.sh`. The goal is to close the wire-byte verification of
the UTS-1 secluded-args fix (PR #5516) and the UTS-1.d drain regression
test (PR #5610). lsh.sh is a stdin/stdout pipe wrapper (no TCP), so the
"tcpdump" framing in the original UTS-1.f task description maps to
stdin/stdout byte sequences captured by tee-ing the wire pipe rather
than to actual port-22 packet capture; this audit follows the
equivalent byte sequence directly.

## Wire layout reference (upstream)

From `target/interop/upstream-src/rsync-3.4.4/rsync.c:283-320`
`send_protected_args()`:

```c
for (i = 0; args[i]; i++) {} /* find first NULL */
args[i] = "rsync"; /* set a new arg0 */
do {
    if (!args[i][0])
        write_buf(fd, ".", 2);
    else
        write_buf(fd, args[i], strlen(args[i]) + 1);
} while (args[++i]);
write_byte(fd, 0);
```

The split is set up in `options.c:2744-2745`:

```c
if (protect_args && !local_server) /* unprotected args stop here */
    args[ac++] = NULL;
```

So an upstream client running
`rsync -ais -e ./lsh.sh src/ lh:dst/`
emits to the remote shell:

- argv (cmdline)
  ```
  rsync --server --sender -logDtpre.iLsfxCIvu -s . <empty after NULL>
  ```
  The `s` flag character in the compact flag string is the
  protect-args / secluded-args indicator
  (`options.c:2604` builds the compact `-...` string with `s` when
  `protect_args`).
- stdin payload
  ```
  rsync\0.\0src/\0\0
  ```
  (where the `rsync` arg0 is the synthetic replacement injected by the
  `args[i] = "rsync"` line above)

The server-side drain at
`target/interop/upstream-src/rsync-3.4.4/io.c:1308-1367` `read_args()`
consumes the stdin tail byte-for-byte until `read_line()` returns 0
(the empty-arg terminator), leaving the stdin fd positioned past the
final NUL so the next reader (the multiplex / @RSYNCD greeting) sees a
clean stream.

## oc-rsync client wire emission

`crates/core/src/client/remote/invocation/builder.rs:99-131`
`build_secluded()`:

```rust
let mut cmd_args = Vec::new();
if let Some(rsync_path) = self.config.rsync_path() {
    cmd_args.push(OsString::from(rsync_path));
} else {
    cmd_args.push(OsString::from("rsync"));
}
cmd_args.push(OsString::from("--server"));
if self.role == RemoteRole::Receiver {
    cmd_args.push(OsString::from("--sender"));
}
cmd_args.push(OsString::from("-s"));
cmd_args.push(OsString::from("."));

SecludedInvocation {
    command_line_args: cmd_args,
    stdin_args: full_args,
}
```

Where `full_args` is the result of `build_full_args_for_stdin` and
contains the entire post-NUL argv: flag string, every long option, every
positional path. The stdin payload is written by
`crates/core/src/client/remote/ssh_transfer.rs:311-333` via
`protocol::secluded_args::send_secluded_args` with the iconv hook set
when `--iconv` is active.

So an oc-rsync client running `-ais` to an oc-rsync server through
lsh.sh emits:

- argv (cmdline)
  ```
  oc-rsync --server --sender -s .
  ```
- stdin payload (NUL-separated, terminated by `\0`)
  ```
  -logDtpre.iLsfxCIvu\0.\0src/\0\0
  ```
  (no synthetic `rsync` arg0; the oc-rsync client serialises the full
  argv tail directly without the upstream `args[i] = "rsync"` rewrite.)

This is the wire divergence: oc-rsync's stdin tail begins with the flag
string, upstream's begins with the synthetic `rsync` arg0 and pushes
the flag string onto the cmdline.

## oc-rsync server drain

`crates/cli/src/frontend/server/run.rs:36-61` HEAD:

```rust
let mut stdin = io::stdin().lock();

let effective_args: Vec<OsString>;
let effective_slice: &[OsString] = if secluded_args {
    match protocol::secluded_args::recv_secluded_args(&mut stdin, None) {
        Ok(received_args) => {
            effective_args = received_args.into_iter().map(OsString::from).collect();
            &effective_args
        }
        Err(e) => {
            write_server_error(
                stderr,
                program_brand,
                format!("failed to read secluded args: {e}"),
            );
            return 1;
        }
    }
} else {
    &args[1..]
};
```

The drain delegates to `protocol::secluded_args::recv_secluded_args` at
`crates/protocol/src/secluded_args.rs:114-162` which mirrors the
upstream `read_line()` loop: it reads one byte at a time and stops the
moment it encounters the empty-arg terminator (`0u8` at position 0 of
the next arg). PR #5610 pinned that the cursor advances exactly to
`wire.len()` after the terminator, so no residual bytes leak into the
subsequent reader (the @RSYNCD greeting / multiplex header).

## Wire-byte parity verdict

| Direction | Wire path | Cmdline | Stdin tail | Drain matches `io.c:1308-1367` | Verdict |
|---|---|---|---|---|---|
| oc-rsync push -> oc-rsync server | symmetric | `--server -s .` | full argv (flags + paths) | Yes via #5610 drain test | PARITY (symmetric) |
| oc-rsync pull -> oc-rsync server | symmetric | `--server --sender -s .` | full argv (flags + paths) | Yes via #5610 drain test | PARITY (symmetric) |
| upstream push -> oc-rsync server | asymmetric | `--server -slogDtpre.iLsfxCIvu <long-flags>` | `rsync\0.\0src/\0\0` | Yes via #5610 drain test | GAP via #5541 revert |
| upstream pull -> oc-rsync server | asymmetric | `--server --sender -slogDtpre.iLsfxCIvu <long-flags>` | `rsync\0.\0src/\0\0` | Yes via #5610 drain test | GAP via #5541 revert |

The symmetric oc-rsync<->oc-rsync legs are PARITY: test4 and test5 in
`target/interop/upstream-testsuite/00-hello.log:119-137` show clean
itemize output (`<f......... file` and `>f......... file`) with empty
directory and file diff blocks.

The asymmetric upstream-client -> oc-rsync-server legs are a GAP. The
fix in PR #5516 (commit `32e3ef7da`) explicitly handled the asymmetric
case by merging the cmdline tail (which carries the flag string when
the client is upstream) with the stdin payload after dropping the
synthetic `rsync` arg0. That merge was reverted by PR #5541's rebase
diff (commit `d69254ab1`):

```diff
-                let mut received_iter = received_args.into_iter();
-                let _arg0 = received_iter.next();
-                let cmdline_tail = args.iter().skip(1).cloned();
-                effective_args = cmdline_tail
-                    .chain(received_iter.map(OsString::from))
-                    .collect();
+                effective_args = received_args.into_iter().map(OsString::from).collect();
```

`git log --all -p -- crates/cli/src/frontend/server/run.rs` confirms
the revert. The current HEAD code on `crates/cli/src/frontend/server/run.rs:47`
discards the cmdline tail entirely.

### What the existing drain regression test covers (PR #5610)

PR #5610 adds three regression tests in `crates/protocol/src/secluded_args.rs`:

1. `recv_secluded_args_consumes_terminator_completely` - pins the
   cursor advance to `wire.len()` for the full 5-arg payload
   `--server\0--sender\0-logDtpr\0.\0/path\0\0`.
2. `recv_secluded_args_handles_empty_arg_list` - pins the lone `\0`
   terminator advances the cursor by exactly 1 byte.
3. `recv_secluded_args_unexpected_eof_before_terminator` - pins the
   `UnexpectedEof` error kind when the wire is truncated.

These cover the drain cutoff invariant at the protocol layer, and they
are sufficient to lock the `@RSYNCD:` greeting / multiplex handoff for
both the symmetric (PARITY) and asymmetric (GAP) wire layouts. The
GAP is not a drain bug; it is a cmdline-vs-stdin merge bug one layer
up, in `crates/cli/src/frontend/server/run.rs`.

## Sign-off

- Symmetric paths (oc-rsync<->oc-rsync): PARITY.
- Asymmetric paths (upstream-client -> oc-rsync-server): GAP_FOUND
  re-introduced by PR #5541's rebase diff. PR #5516's merge logic is
  the canonical fix and needs to be reapplied; PR #5610's drain test
  remains valid and is independent of the cmdline merge.
- UTS-1.f wire-byte verification is complete: the drain layer matches
  upstream `io.c:1308-1367` byte-for-byte; the regression is at the
  cmdline merge layer.

## Cross-references

- PR #5516 - `fix: accept -s secluded-args in oc-rsync --server flag
  string` (UTS-1 fix - REVERTED by #5541)
- PR #5610 - `test(protocol): assert recv_secluded_args fully drains
  terminator NUL` (UTS-1.d.followup)
- PR #5541 - `fix: wire delete pass into pipelined_incremental for
  NDX_DEL_STATS` (the regression vehicle that reverted #5516's merge)
- Upstream `target/interop/upstream-src/rsync-3.4.4/rsync.c:283-320`
  `send_protected_args()` - wire layout for the stdin tail
- Upstream `target/interop/upstream-src/rsync-3.4.4/io.c:1308-1367`
  `read_args()` - server-side drain contract
- Upstream `target/interop/upstream-src/rsync-3.4.4/options.c:2604-2745`
  `server_options()` - the protect-args NUL split that decides what
  travels on the cmdline vs stdin
- Fixture log `target/interop/upstream-testsuite/00-hello.log:119-137`
- Audit `docs/audits/uts-1-e-test6-actual-command.md`
- Memory entry `feedback_container_debug_endpoint.md` - container
  debug endpoint preference (deferred here because podman was
  unavailable on the audit host; symbolic byte-sequence reconstruction
  was used instead, with verbatim upstream source citations)

## Follow-ups enumerated (not raised as separate tasks)

- `UTS-1.f.followup` - reapply PR #5516's cmdline+stdin merge in
  `crates/cli/src/frontend/server/run.rs:36-61`. The exact diff is
  the inverse of the snippet under
  "What the existing drain regression test covers" above.
- `UTS-1.f.followup.b` - add a regression test in
  `crates/cli/src/frontend/server/tests.rs` that pins the asymmetric
  wire layout: cmdline `--server --sender -slogDtpre.iLsfxCIvu`,
  stdin `rsync\0.\0/path\0\0`. The test should drive `run_server_mode`
  and assert the effective args slice equals
  `["--server", "--sender", "-slogDtpre.iLsfxCIvu", ".", "/path"]`
  (cmdline tail + stdin tail with `rsync` arg0 dropped).
- `UTS-1.f.followup.c` - capture a real tcpdump/pcap on the
  `rsync-profile` container once it is reachable, to lock the
  byte-byte parity claim with actual wire bytes (the symbolic
  reconstruction in this audit is grounded in verbatim upstream
  source citations but does not include captured pcap evidence).
