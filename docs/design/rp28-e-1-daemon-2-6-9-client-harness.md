# RP28.e.1 - Daemon-Mode rsync 2.6.9 Client Interop Harness

Spec-only document. No code or workflow changes ship with this task.

Task: RP28.e.1 (#2963). Parent: RP28.e (#2730). Grandparent: RP28 (#2725).

Memory note: `[[project_protocol_compat]]`.

## 1. Scope

RP28.e.1 specs the harness for the daemon-mode rsync 2.6.9 interop test. The topology
under spec is:

- oc-rsync runs as `--daemon --no-detach`, listening on a `rsync://` URL.
- rsync 2.6.9, built via `scripts/build_rsync_2_6_9.sh` (RP28.b.1, PR #4903), is the
  client. It issues both pull and push operations against the oc-rsync daemon.

RP28.e.1 is the design alone; the follow-up tasks own implementation and reporting:

- RP28.e.2 (#2964) wires the harness into the Interop Validation workflow.
- RP28.e.3 documents the run outcome and any deviations from this spec.

The pre-existing RP28.c push fixture (PR #4923) and RP28.d pull fixture (in CI at
`.github/workflows/_interop.yml:198-281`) both run rsync 2.6.9 as the *daemon*. RP28.e
inverts that: oc-rsync is the daemon, rsync 2.6.9 is the client.

## 2. Why Daemon-Mode Matters

rsync 2.6.x clients in real deployments commonly point at modern rsync daemons.
Backup-server appliances frozen on rsync 2.6.x may continue to be deployed alongside
modern Linux distros that have moved on to rsync 3.4.x. If oc-rsync stands in for the
modern daemon, it MUST handle the following pre-30 code paths correctly:

- Protocol 28 capability negotiation. The 2.6.9 client never sends the `'i'`
  (INC_RECURSE) capability bit and never expects the daemon to advertise it.
- The proto 28 flist encoding, exercised end-to-end by the wire-byte regression
  test at `crates/protocol/tests/flist_wire_flags_rp28g.rs` (RP28.g) and the golden
  flist at `crates/protocol/tests/golden_protocol_v28_flist.rs`.
- The proto < 31 zlib codec without-cursor-advance behaviour, covered by the
  wire-byte regression at `crates/protocol/tests/zlib_codec_proto_lt_31.rs` (RP28.i)
  and the golden bytes at `crates/protocol/tests/zlib_golden_bytes.rs`.
- The pre-INC_RECURSE sender path. With no `'i'` capability flag in the negotiated
  set, the daemon must drive the legacy non-incremental file-list walk.
- The proto < 31 absence of NDX_DEL_STATS. The generator must not emit the
  `NDX_DEL_STATS` sentinel + five varints during goodbye when the negotiated
  protocol is below 31; the 2.6.9 client would treat the trailing bytes as a
  protocol error.

The flist sort-key parity for proto 28 is independently asserted at
`crates/protocol/tests/flist_sort_keys_rp28h.rs` (RP28.h).

## 3. Test Topology

Concrete setup the harness scripts assemble at runtime:

- oc-rsync daemon invocation:
  ```
  target/release/oc-rsync --daemon --no-detach --port $port --config $cfg
  ```
- Daemon config written to `/tmp/oc-rsyncd-rp28-e.conf`:
  ```
  use chroot = no
  pid file = /tmp/oc-rsyncd-rp28-e.pid
  [test]
      path = /tmp/rp28-e/daemon-share
      read only = false
      list = yes
  ```
- Client binary path: `/usr/local/bin/rsync-2.6.9`, installed by
  `scripts/build_rsync_2_6_9.sh` with `PREFIX=/usr/local`. In CI the same binary is
  also available at `target/interop/upstream-install/2.6.9/bin/rsync` per the
  RP28.c/RP28.d cells; the harness resolves whichever exists first.
- Client invocations:
  - Pull: `rsync-2.6.9 -av rsync://localhost:$port/test/ /tmp/rp28-e/client-dest/`
  - Push: `rsync-2.6.9 -av /tmp/rp28-e/client-src/ rsync://localhost:$port/test/`

The daemon binds 127.0.0.1 on an ephemeral port (see section 8). The `[test]` module
allows both read and write (`read only = false`) because the matrix exercises both
directions through the same module.

## 4. Fixture Matrix

Ten fixtures cover the protocol surface the 2.6.9 client touches against a modern
daemon. Each row names the fixture, the directions it runs in, and what wire-level
behaviour it exercises.

| Fixture | Direction | What it exercises |
|---------|-----------|-------------------|
| D1: empty dir | pull + push | smoke test of greeting, module list, empty flist |
| D2: 100 small files | pull + push | flist encoding at proto 28 (RP28.g surface) |
| D3: file with `-z` | pull | zlib codec at proto 28 (RP28.i surface) |
| D4: file with `--checksum` | pull | checksum negotiation absence at proto 28 |
| D5: file with `--delete` on dest | pull + push | NDX_DEL_STATS absence at proto < 31 |
| D6: directory tree (3 levels) | push | non-INC_RECURSE legacy flist path |
| D7: file with extended chars in name | pull + push | name encoding at proto 28 |
| D8: hardlink group (2 files) | pull + push | hardlink wire encoding at proto 28 |
| D9: incremental update | pull (twice) | quick-check + delta wire format at proto 28 |
| D10: 1 MiB file with delta | pull (modify daemon-side) | rolling+strong checksum at proto 28 |

D9 runs the same pull twice without modification so the second invocation drives the
quick-check skip path. D10 modifies the daemon-side copy between two pulls so the
client receives a non-trivial delta script.

## 5. Pass / Fail Criteria

Each fixture passes only when all three of the following hold:

- Client exit code is 0.
- The destination tree is byte-identical to the source. Verified with
  `diff -r --no-dereference $src $dst`. For D8 the verifier also asserts the
  destination preserves the hardlink relationship via `stat -c '%i'` comparison.
- oc-rsync daemon stderr contains no panics, no `error:` log lines, and no
  `WARNING` lines other than expected protocol-downgrade messages emitted on
  negotiation to protocol 28. The runner script greps `^WARNING` and allowlists
  the downgrade message; any other warning fails the fixture.

The harness collects per-fixture exit codes and emits a summary line at the end.
Any non-zero fixture exits the runner with status 1 so the CI job fails.

## 6. Harness Structure

RP28.e.2 implements the following concrete files:

- `scripts/rp28_e_1_setup.sh` - deterministic fixture generator. Builds the
  daemon-side share at `/tmp/rp28-e/daemon-share`, the client-side source at
  `/tmp/rp28-e/client-src`, and writes the daemon config to
  `/tmp/oc-rsyncd-rp28-e.conf`. Uses fixed seeds for any random data so re-runs
  produce identical bytes.
- `scripts/rp28_e_1_run.sh` - orchestrates daemon start, runs each fixture in
  sequence, captures exit codes, kills the daemon on completion or failure.

Both scripts open with `set -euo pipefail` and register an `EXIT` trap that:

1. Sends `SIGTERM` to the daemon PID (read from the pid file).
2. Waits up to 5 seconds for graceful exit, then `SIGKILL`.
3. Removes the `/tmp/rp28-e/` tree and the daemon pid + log files.

The daemon listens on an ephemeral port. The runner picks a free port via the same
helper used in the RP28.c cell:

```
python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1]); s.close()'
```

The chosen port is written to `/tmp/rp28-e/port.sentinel`; both the daemon launch
command and every client invocation read the value back from that file. The runner
then polls the TCP port (`bash` `/dev/tcp` probe) until bind succeeds or a 10-second
budget elapses, mirroring the RP28.c/RP28.d cells.

## 7. CI Workflow Integration

For RP28.e.2 to wire in:

- Extend `.github/workflows/_interop.yml` with a new job step
  `Run rp28_e_daemon_2_6_9_interop`, placed after the RP28.d pull cell at
  `.github/workflows/_interop.yml:198-281` so the workflow groups all
  pre-30 cells.
- The step depends on the rsync 2.6.9 build artifact already produced by
  `tools/ci/run_interop.sh build-only` and cached at
  `target/interop/upstream-install/2.6.9/bin/rsync` (RP28.b.3 surface).
- oc-rsync is already built via standard cargo earlier in the workflow; no
  additional build step is required.
- The step invokes `scripts/rp28_e_1_run.sh` and asserts exit code 0.
- The step is initially advisory: `continue-on-error: true`, matching the
  RP28.c/RP28.d cells. Promotion to a required check is a future RP28 task,
  tracked alongside RP28.k.

## 8. Known Fragilities

- rsync 2.6.9 may issue `--info=stats` or other info-class keywords the modern
  oc-rsync parser does not recognise. The daemon code path must tolerate unknown
  info keywords without aborting; if it does not, RP28.e.2 surfaces it.
- Daemon listen socket binding: use `127.0.0.1` explicitly, never `0.0.0.0`, to
  avoid CI firewall and IPv6 dual-stack issues. Mirrors the RP28.c/RP28.d cells.
- Ephemeral port allocation: never hardcode. Use the `python3` socket probe shown
  in section 6; hardcoded ports collide with other CI jobs on the same runner.
- rsync 2.6.9 may print deprecation or compatibility warnings on stderr. The
  harness MUST NOT treat client stderr warnings as failure - only the exit code
  and the byte-identical diff are authoritative for the client side. Daemon-side
  warnings are evaluated by the allowlist in section 5.
- The 2.6.9 build script disables ACL/xattr/iconv support
  (`scripts/build_rsync_2_6_9.sh:67-71`). Fixtures D7 and D8 therefore avoid
  ACL/xattr metadata; extended-char names exercise only filename bytes, not
  filesystem-extended attributes.

## 9. Cross-References

- RP28.b.1 build script: `scripts/build_rsync_2_6_9.sh` (PR #4903).
- RP28.c push-direction fixture spec: PR #4923; CI cell at
  `.github/workflows/_interop.yml:104-187`.
- RP28.d pull interop cell: `.github/workflows/_interop.yml:198-281` (already in CI).
- RP28.g flist wire-byte regression:
  `crates/protocol/tests/flist_wire_flags_rp28g.rs`.
- RP28.h flist sort-key parity: `crates/protocol/tests/flist_sort_keys_rp28h.rs`.
- RP28.i zlib codec proto < 31 regression:
  `crates/protocol/tests/zlib_codec_proto_lt_31.rs`.
- RP28.a inventory: `docs/design/rp28-a-pre30-code-paths-inventory.md`.
- Memory note: `[[project_protocol_compat]]`.
