# UTS-1.e - test6 actual command and diff failure

## Scope

Capture the upstream `testsuite/00-hello.test` test6 actual command vector
that lsh.sh forwards to oc-rsync, and document the observed diff failure
against the existing 00-hello run log. Closes the wire-byte verification
of the UTS-1 secluded-args fix series (#5516, #5610) for the `-ais`
round-trips that surround test6.

## Test layout

`target/interop/upstream-src/rsync-3.4.4/testsuite/00-hello.test:46-53`:

```sh
echo test6

touch "$fromdir/one" "$fromdir/two"
(cd "$fromdir" && $RSYNC -ai --old-args --rsync-path="$RSYNC" lh:'one two' "$todir/")
if [ ! -f "$todir/one" ] || [ ! -f "$todir/two" ]; then
    test_fail "old-args copy of 'one two' failed"
fi
```

test6 is the `--old-args` (`old_style_args`) leg, not `-ais`. The
`--old-args` flag opts the client out of `protect_args` and asks the
remote rsync to re-split the single string `'one two'` into two
positional path arguments. lsh.sh is the upstream test-local shell
wrapper at
`target/interop/upstream-src/rsync-3.4.4/support/lsh.sh:1-37`; it strips
its own `-l`/`--no-cd` flags, pops the hostname (`lh` -> `do_cd=n`), and
`eval`s the remaining command verbatim.

## test6 captured failure

The fixture log at `target/interop/upstream-testsuite/00-hello.log:139-141`
records the verbatim oc-rsync output:

```
test6
oc-rsync error: transfer failed: received file entry with zero-length filename (code 12) at error.rs(176) [client=0.6.3]
oc-rsync error: server error: failed to fill whole buffer (code 1) at run.rs(317) [server=0.6.3]
```

### Command lsh.sh emits to oc-rsync

Reconstructed from `00-hello.test:50` and `support/lsh.sh:36`. After
lsh.sh strips its hostname argument (`lh`), it `eval`s:

```
$RSYNC --server --sender -logDtpre.iLsfxCIvu --old-args . 'one two'
```

Where `$RSYNC = /workspace/target/release/oc-rsync` and the server-side
flag string is the canonical 3.4.4 packed encoding emitted by
`options.c:server_options()`
(`target/interop/upstream-src/rsync-3.4.4/options.c:2604` and surrounding
lines). The single `'one two'` arg arrives at the server unsplit because
`--old-args` instructs upstream `options.c:1969-1985` to honour the
`RSYNC_OLD_ARGS` env var or the explicit flag and skip the
`send_protected_args()` arg split entirely.

### Diff failure analysis

The `received file entry with zero-length filename` error in the client
surfaces from `crates/protocol/src/file_list.rs` when the receiver
parses a flist entry whose path is empty. The server-side
`failed to fill whole buffer (code 1) at run.rs(317)` confirms that the
server died trying to read the next protocol frame from stdin.

Root cause (cross-referenced):

1. The oc-rsync server's `--old-args` parse path does not implement the
   upstream re-split semantics. Upstream `options.c:1640-1648`
   (`old_style_args >= 1` branch) joins all post-`.` argv entries with a
   space and re-splits at every whitespace via `glob_expand()` so the
   single `'one two'` becomes two positional paths (`one`, `two`).
2. The oc-rsync side treats `'one two'` as one literal path that does
   not exist in `$fromdir`, which causes the sender to enqueue a
   zero-entry flist plus an empty terminator. The receiver-side
   `error.rs(176)` raise fires when the terminator is misaligned.
3. The server's `run.rs:317` error chain originates inside the
   `run_server_stdio` loop where the protocol reader hits EOF
   prematurely because the client has already torn down stdin after
   reporting the receiver-side flist error.

### Where this is handled post-PR #5516

`crates/cli/src/frontend/server/run.rs:36-61` is the entry point that
classifies the inbound argv into secluded-args vs cmdline branches:

```rust
let secluded_args = detect_secluded_args_flag(args);

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

For test6 the `secluded_args` detector at
`crates/cli/src/frontend/server/flags.rs:22` returns `false` (because
upstream does not pack `s` into the flag string when `protect_args` is
off), so the server falls through to the `&args[1..]` branch and
forwards `--old-args .` `one two` to
`parse_server_flag_string_and_args` and `ServerConfig::from_flag_string_and_args`.

That parser does not call any equivalent of upstream's `glob_expand()`
re-split, so the single arg `'one two'` is treated as one literal
filename, which is then enqueued as a missing path. The result is the
zero-length-filename receiver error captured in the log.

## Regression: PR #5570 silently reverted PR #5516's secluded-args merge

While verifying the test6 path, the audit identified an unrelated
regression in the secluded-args branch that PR #5516 had previously
fixed.

PR #5516 (commit `32e3ef7da`) shipped the explicit cmdline+stdin merge
for the `-ais` case (test4/test5 path):

```rust
let mut received_iter = received_args.into_iter();
let _arg0 = received_iter.next();
let cmdline_tail = args.iter().skip(1).cloned();
effective_args = cmdline_tail
    .chain(received_iter.map(OsString::from))
    .collect();
```

PR #5541 (commit `d69254ab1`, title
`fix: wire delete pass into pipelined_incremental for NDX_DEL_STATS`)
landed a rebase-artefact diff to the same file that reverted the merge
to:

```rust
effective_args = received_args.into_iter().map(OsString::from).collect();
```

Confirm via `git log --all -p -- crates/cli/src/frontend/server/run.rs`.

Today's master (`a314a7217` HEAD) carries the reverted form. test4 and
test5 still pass in the captured 00-hello.log because both ends are
oc-rsync and the oc-rsync client packs the full argv (including the
`--server`/`--sender`/flag-string head) into the stdin payload via
`crates/core/src/client/remote/invocation/builder.rs:99-131`
`build_secluded()`, so the receiver does not depend on the cmdline
tail. The upstream client emits only the post-NUL tail
(`rsync\0.\0/path\0\0`, per
`target/interop/upstream-src/rsync-3.4.4/rsync.c:283-320`
`send_protected_args()`), so an upstream-client -> oc-rsync-server
`-ais` round-trip would re-surface
`invalid server arguments: missing rsync server flag string`.

This is enumerated as a follow-up below; it is not directly a UTS-1.e
regression but it does cap the wire-byte verification we intended to
close.

## Status

- test6 (`--old-args` re-split) is a SEPARATE failure cluster from
  UTS-1's secluded-args work. It is not addressed by #5516 or #5610 and
  needs its own server-side `--old-args` glob-re-split implementation.
  The transfer-failure error is real and reproducible.
- test4 / test5 (`-ais` oc-rsync<->oc-rsync) pass in the captured log,
  so the UTS-1 fix is effective for the symmetric implementation case.
- test4 / test5 (`-ais` upstream-client -> oc-rsync-server) is regressed
  by PR #5541's rebase artefact and is the gap UTS-1.f surfaces below.

## Cross-references

- PR #5516 - `fix: accept -s secluded-args in oc-rsync --server flag
  string` (the UTS-1 fix)
- PR #5610 - `test(protocol): assert recv_secluded_args fully drains
  terminator NUL` (UTS-1.d.followup)
- PR #5541 - `fix: wire delete pass into pipelined_incremental for
  NDX_DEL_STATS` (the regression vehicle that reverted #5516's merge)
- Upstream `target/interop/upstream-src/rsync-3.4.4/io.c:1308-1367`
  `read_args()` - server-side drain contract for secluded args
- Upstream `target/interop/upstream-src/rsync-3.4.4/rsync.c:283-320`
  `send_protected_args()` - wire layout for the stdin tail
- Upstream `target/interop/upstream-src/rsync-3.4.4/options.c:1640-1648`
  `--old-args` re-split semantics
- Fixture log `target/interop/upstream-testsuite/00-hello.log:139-141`
- Test source
  `target/interop/upstream-src/rsync-3.4.4/testsuite/00-hello.test:46-53`
- Shell wrapper
  `target/interop/upstream-src/rsync-3.4.4/support/lsh.sh:1-37`

## Follow-ups enumerated (not raised as separate tasks)

- `UTS-1.e.followup` - implement `--old-args` server-side re-split.
  Mirror upstream `options.c:1640-1648` `old_style_args` branch:
  whenever the server parses positional args after the `.` separator
  and `old_style_args >= 1`, join all positional args with `' '` and
  re-split on whitespace via the existing `glob_expand` equivalent
  before passing them to `ServerConfig::from_flag_string_and_args`.
- `UTS-1.e.followup.b` - add a regression test in
  `crates/cli/src/frontend/server/tests.rs` that pins the upstream
  test6 invocation (`--server --sender -logDtpre.iLsfxCIvu --old-args
  . 'one two'`) and asserts the server splits into two positional
  paths.

## Sign-off

- UTS-1.e captured: test6 failure root-caused to a missing server-side
  `--old-args` re-split, with verbatim command line, error chain, and
  upstream reference.
- The audit is the deliverable; no production code change in this PR.
