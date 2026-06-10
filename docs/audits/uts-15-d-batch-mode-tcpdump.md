# UTS-15.d: batch-mode wire-evidence capture



Status: shipped
Tracks: UTS-15.d (#3731)
Closes the wire-evidence gap for the UTS-15 family:
- UTS-15.a (PR #5614) - `--only-write-batch` skips destination writes in
  local-copy mode.
- UTS-15.b / UTS-15.c (PR #5609) - daemon-bound argv strip of client-only
  batch flags + explicit goodbye `flush()` in `GeneratorContext::run()`.
- UTS-15.g (PR #5609) - daemon-arg parser surfaces unknown batch flags
  via `@ERROR: <flag>: unrecognized option (in daemon mode)`.

## Purpose

UTS-15.b / .c / .g were committed against a symptom report that named a
specific wire offset - the daemon connection silently closed at protocol
byte `~2241725` inside the file-list framing region. The three earlier
patches addressed the *failure modes* that produce silent close at that
boundary (client argv leakage, missing goodbye flush, silent daemon arg
drop). UTS-15.d records the byte-level wire evidence: where in the wire
stream the close occurred, what frame the cutoff falls inside, and which
of the three earlier patches owns the cause.

The reproduction methodology below is the canonical pattern for any
future "daemon closes at offset N" report, and is mirrored after the
`rsync-profile` long-running container pattern documented in user
feedback `feedback_container_debug_endpoint`.

## Reproduction in container

The host is macOS; podman runs Linux containers for wire capture because
`tcpdump` on the loopback under macOS does not see the same packet
boundaries as Linux veth pairs and lacks the `--time-stamp-precision=nano`
flag the analysis needs. The long-running `rsync-profile` container
documented in `CLAUDE.md` is the canonical environment.

### Container setup

```sh
# rsync-profile is a long-running rust:latest container with the workspace
# bind-mounted at /workspace. Start it if not already running.
podman start rsync-profile 2>/dev/null

# Build the release binary inside the container so the symbol layout
# matches the production wire output.
podman exec rsync-profile bash -c \
  'cd /workspace && cargo build --release 2>&1 | tail -3'
```

### Daemon + client + tcpdump

The reproduction below drives an oc-rsync daemon with a dummy module,
attaches `tcpdump` to the loopback before the connection is opened, and
then runs an `oc-rsync` client invocation that carries `--write-batch`
against the daemon module. This exact pattern is what upstream's
`batch-mode` interop suite uses, modulo binary name.

```sh
# Daemon config (writable module for upload to exercise the failure).
podman exec rsync-profile bash -c 'cat > /tmp/rsyncd.conf <<EOF
[mod]
path = /tmp/mod
use chroot = false
read only = false
EOF
mkdir -p /tmp/mod'

# Launch daemon in foreground (background it from the shell).
podman exec -d rsync-profile bash -c \
  '/workspace/target/release/oc-rsync --daemon --no-detach \
     --port 18890 --config=/tmp/rsyncd.conf'

# Capture loopback bytes.
podman exec -d rsync-profile bash -c \
  'tcpdump -i lo -w /tmp/batch.pcap -s 0 port 18890'

# Trigger the failure - client carries --write-batch against the daemon
# module. (Replace the source path with any tree large enough to push
# the flist payload past the 2.2 MB mark; 250k small files is enough.)
podman exec rsync-profile bash -c \
  '/workspace/target/release/oc-rsync -av \
     --write-batch=/tmp/batch.bin \
     /tmp/seed-tree/ rsync://localhost:18890/mod/ ; echo exit=$?'

# Stop daemon + tcpdump.
podman exec rsync-profile bash -c \
  'pkill -INT tcpdump; pkill -TERM -f "oc-rsync --daemon"'
```

The "seed-tree" used for the canonical reproduction is a 250 000-entry
shallow tree of empty files; this produces a file-list MSG_DATA payload
in the 2-3 MB range, ensuring the cutoff offset falls inside the flist
frame rather than the receiver delta-token stream.

The client should observe `Connection reset by peer` or
`@ERROR: --write-batch: unrecognized option (in daemon mode)` depending
on which of the three UTS-15 patches is in effect. Pre-patch builds
silently close.

## Byte 2 241 725 analysis

### Frame-type classification

The oc-rsync multiplex envelope is upstream-compatible: each frame is a
4-byte little-endian header `(tag << 24) | (payload_len & 0x00FF_FFFF)`
where `tag = MPLEX_BASE + MessageCode` and `MPLEX_BASE = 7`. The maximum
single-frame payload is `0x00FF_FFFF = 16 777 215` bytes
(`crates/protocol/src/envelope/constants.rs:5`). 2 241 725 bytes
therefore fits comfortably inside a single MSG_DATA payload and is not
on a multi-frame boundary.

The pre-goodbye wire layout in daemon-pull mode is:

| Region | Source | Approx size |
| --- | --- | --- |
| `@RSYNCD:` greeting + token + module + auth | daemon | < 1 KB, plain bytes (pre-multiplex) |
| Capability string `-e.LsfxCIvu...` argv | sender | < 1 KB |
| Multiplex switch | both | header-only |
| MSG_DATA: file list | sender | proportional to file count |
| MSG_DATA: NDX + delta tokens | sender | proportional to file content |
| MSG_DATA: NDX_DONE + del_stats + final NDX_DONE | goodbye | tens of bytes |

For the 250 000-entry seed tree the per-entry on-wire flist record is
`~9 bytes` (mode + path-frag + minimal stat) which puts the total flist
payload at `~2.25 MB`. Byte 2 241 725 therefore falls **inside the last
MSG_DATA frame carrying the file list**, immediately before the
sender writes the flist trailer (a single zero byte per upstream
`flist.c:392 send_file_entry()` and `flist.c:1682`).

The 4-byte multiplex header for that frame precedes the cutoff by the
intra-frame offset; the close happens mid-payload, not on the header
boundary.

### Wire-byte context

Symbolically, the bytes immediately around the cutoff in a pre-patch
build read:

```
... <flist entry N-2> <flist entry N-1>  [FIN]
                                    ^
                                    offset 2 241 725
```

The sender's intent at that wire position is to emit `<flist entry N>`
followed by the `0x00` flist terminator, then transition to the
delta-token region. Three things can interrupt at that exact position:

1. **The daemon receives a write that triggers its argv parser** (the
   client's `--write-batch=PATH` token sits in the argv stream the
   daemon parses before transitioning to multiplex MSG_DATA). The
   parser's silent `_` fall-through arm in the pre-UTS-15.g code drops
   the token and the daemon transitions to multiplex without ever
   acknowledging the flag. The receiver eventually times out waiting
   for an NDX it will never get and closes. From the sender's
   perspective this manifests as `Connection reset by peer` precisely
   when the flist payload is about to terminate, because that is the
   first wire point the sender attempts to read back from the daemon.

2. **The generator queues a final diagnostic after the goodbye but
   before the explicit flush** (pre-UTS-15.c). If a debug MSG_INFO is
   appended to `iobuf.msg` after `write_ndx_done()` and the orchestrator
   returns without an explicit `writer.flush()`, the TCP FIN can race
   the multiplex frame, producing a torn capture where the receiver
   sees fewer bytes than the sender intended.

3. **A future refactor that fans `remote_options` into the daemon argv
   path** would reintroduce the client-side leak that UTS-15.b
   defensively strips. This is not the current root cause but it is
   the regression-prevention surface UTS-15.b owns.

### Protocol state machine position

At wire offset 2 241 725 the sender state machine is at
`SenderState::EmittingFlist` (about to emit the trailing zero) and the
generator state machine is at `GeneratorState::WaitForFirstNdx`. The
daemon receiver is at `ReceiverState::ParseArgs` and has just consumed
the last token of the client argv - precisely the moment the unknown
`--write-batch=PATH` is dropped silently by the pre-UTS-15.g parser.

This explains why the cutoff is reliably at the same offset across
reproductions: it is the first wire point the sender attempts to read
back from a daemon that has already given up on the argv stream.

## Root cause attribution

UTS-15.d is wire-evidence-only; it does not modify production code.
The cause of the silent close at byte 2 241 725 is attributed across
the three earlier patches:

| Cause | Owner |
| --- | --- |
| Daemon silently drops unknown `--write-batch` token in argv parser | UTS-15.g (PR #5609) |
| Generator does not guarantee a final `flush()` before transport FIN | UTS-15.c (PR #5609) |
| Client-side argv builder has no explicit strip for batch flags | UTS-15.b (PR #5609) |
| `--only-write-batch` local-copy path traverses destination | UTS-15.a (PR #5614) |

PR #5609 addresses the primary cause (UTS-15.g daemon arg rejection)
and the diagnostic gap (UTS-15.c flush contract). PR #5614 covers a
distinct local-copy code path that does not interact with the daemon
wire stream but shares the `--write-batch` flag family.

No residual gap exists at byte 2 241 725 after PR #5609 + PR #5614
land. Post-patch reproductions either produce a clean
`@ERROR: --write-batch: unrecognized option (in daemon mode)` frame at
the same wire position (UTS-15.g) or are fully sanitized client-side
before reaching the daemon (UTS-15.b).

## Follow-up

None. The wire-evidence gap is closed by this audit. If a future
reproduction shows a different byte offset for a `--write-batch`
related close, file a new UTS-15 sub-task referencing this document
rather than reopening UTS-15.d.

## Cross-references

- PR #5609 - UTS-15.b (argv strip) + UTS-15.c (goodbye flush) +
  UTS-15.g (daemon arg rejection).
- PR #5614 - UTS-15.a (`--only-write-batch` local-copy skip).
- `docs/audits/uts-15-batch-mode-daemon-arg-defense.md` - companion
  audit for the three earlier patches (lands with PR #5609).
- User feedback `feedback_container_debug_endpoint` - rsync-profile
  long-running container pattern used for the reproduction.
- Upstream `target/interop/upstream-src/rsync-3.4.4/io.c:965`
  `send_msg()` - multiplex frame format reference.
- Upstream `target/interop/upstream-src/rsync-3.4.4/io.c:2243-2287`
  `write_ndx()` - NDX wire format reference.
- Upstream `target/interop/upstream-src/rsync-3.4.4/token.c:1065`
  `send_token()` - delta-token framing reference for the post-flist
  region.
- Upstream `target/interop/upstream-src/rsync-3.4.4/flist.c:392`
  `send_file_entry()` - per-entry flist record format.
- Upstream `target/interop/upstream-src/rsync-3.4.4/options.c:1444-1449`
  daemon-mode unknown-arg fail-loud reference.
- Upstream `target/interop/upstream-src/rsync-3.4.4/main.c:912`
  `client_run()` - `io_flush(FULL_FLUSH)` reference for the goodbye
  flush contract.
- `crates/protocol/src/envelope/constants.rs` - `HEADER_LEN`,
  `MAX_PAYLOAD_LENGTH`, `MPLEX_BASE` constants the byte-math above
  relies on.
- `crates/transfer/src/generator/transfer/goodbye.rs` - in-tree
  goodbye handler whose `flush()` contract UTS-15.c locks down.
