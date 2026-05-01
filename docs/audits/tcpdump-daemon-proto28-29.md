# Audit: tcpdump-style wire decode for protocol 28/29 daemon transfers

Status: documentation only - no code changes.
Tracking issue: #1699.
Upstream reference: `target/interop/upstream-src/rsync-3.4.1/`.

**Last verified:** 2026-05-01 against
`crates/protocol/src/version/protocol_version/capabilities.rs`,
`crates/protocol/src/version/constants.rs`,
`crates/protocol/src/flist/write/metadata.rs`,
`crates/protocol/src/flist/read/metadata.rs`,
`crates/protocol/src/filters/wire.rs`,
`crates/protocol/src/wire/file_entry_decode/`,
`crates/transfer/src/setup/capability.rs`,
`crates/daemon/src/daemon/sections/greeting.rs`, and the upstream sources
cited inline.

## Overview

This audit complements the per-version capability matrix in
`docs/audits/protocol-28-32-interop-matrix.md` (PR #3512) by descending one
layer further: it shows what bytes actually appear on the wire when
oc-rsync runs as a daemon under `--protocol=28` or `--protocol=29`, and
compares them byte-for-byte with upstream rsync 3.4.1 forced to the same
protocol. The companion audit
`docs/audits/tcpdump-daemon-filter-pull.md` covers the modern protocol on
the daemon-filter exchange; this one covers the legacy wire encoding for
the entire daemon-side session.

Protocol 28 (rsync 3.0.x lineage) and protocol 29 (rsync 3.1.0-3.1.3)
predate the binary negotiation introduced in protocol 30. The session
uses different framing rules: ASCII `@RSYNCD:` greeting only (no
compat-flags varint), filter rules use `XFLG_OLD_PREFIXES` (only `+ `,
`- `, `!`), file list entries use fixed 4-byte little-endian fields, no
INC_RECURSE, MD4 assumed for challenge digests, and compression locked
to zlib.

oc-rsync's compatibility with these legacy protocols was hardened in PRs
#3107-#3111, #1604, #1669, #1670, and #1700. This audit captures the
post-fix wire trace and identifies where future divergence would
manifest.

Cross references:

- `docs/audits/tcpdump-daemon-filter-pull.md` - daemon-side filter wire
  comparison on the modern protocol.
- `docs/audits/protocol-28-32-interop-matrix.md` (PR #3512) - per-version
  capability matrix for protocols 28-32.

---

## 1. Reproduction recipe

A single host runs the daemon on `localhost:8730`. The recipe uses an
upstream rsync client because it makes the protocol-version forcing
trivial via `--protocol=N`; the same trace can be produced by oc-rsync
clients passing `--protocol=28` (parsed in
`crates/cli/src/frontend/execution/options/protocol.rs:18`).

### 1.1 Start oc-rsync as a forced-protocol daemon

`oc-rsync --daemon` does not take `--protocol`: the client's greeting
determines the negotiated version, capped at the daemon's newest
supported revision. To force protocol 28/29, run the daemon at its
default newest version and have the client request the legacy version.

```sh
mkdir -p /tmp/audit-1699/{srv,run,dest}
echo 'hello legacy proto' > /tmp/audit-1699/srv/file.txt
mkdir -p /tmp/audit-1699/srv/sub
echo 'nested entry' > /tmp/audit-1699/srv/sub/inner.txt

cat > /tmp/audit-1699/oc-rsyncd.conf <<'EOF'
port = 8730
pid file = /tmp/audit-1699/run/oc-rsyncd.pid
use chroot = no

[mod]
    path = /tmp/audit-1699/srv
    read only = true
EOF

oc-rsync --daemon --no-detach --config=/tmp/audit-1699/oc-rsyncd.conf
```

The daemon's newest supported protocol is 32 (see
`crates/protocol/src/version/constants.rs:9` -
`NEWEST_SUPPORTED_PROTOCOL: u8 = 32`). The greeting on the wire will be
`@RSYNCD: 32.0 ...` followed by a digest list; the client's protocol-28
or protocol-29 request triggers the legacy code paths.

### 1.2 Capture daemon-port traffic

```sh
sudo tcpdump -i lo0 -nn -X -s 0 'tcp port 8730' -w /tmp/audit-1699/cap.pcap
```

On Linux substitute `-i lo`. The default rsync daemon port is 873; this
recipe uses 8730 to avoid root.

### 1.3 Run the client at forced protocol 28 or 29

In a third shell:

```sh
# Pull at protocol 28
rsync --protocol=28 -av rsync://localhost:8730/mod/ /tmp/audit-1699/dest/

# Or protocol 29
rsync --protocol=29 -av rsync://localhost:8730/mod/ /tmp/audit-1699/dest/
```

Stop tcpdump (`Ctrl-C`) once the transfer completes.

### 1.4 Decode the capture

```sh
tcpdump -r /tmp/audit-1699/cap.pcap -nn -X -s 0
```

Or open in Wireshark (no rsync dissector ships with stock Wireshark, so
the bytes appear as raw TCP payload).

---

## 2. Expected wire bytes

### 2.1 Daemon greeting (server -> client)

Upstream emits the greeting from `compat.c:841`:

```c
io_printf(f_out, "@RSYNCD: %d.%d %s\n", protocol_version, our_sub, tmpbuf);
```

oc-rsync emits the same line via
`legacy_daemon_greeting_for_protocol()` at
`crates/daemon/src/daemon/sections/greeting.rs:13`. With the daemon's
default at protocol 32, the bytes are:

```
40 52 53 59 4e 43 44 3a 20 33 32 2e 30 20 73 68   @RSYNCD: 32.0 sh
61 35 31 32 20 73 68 61 32 35 36 20 73 68 61 31   a512 sha256 sha1
20 6d 64 35 20 6d 64 34 0a                         md5 md4.
```

The trailing `\n` is the end-of-greeting marker that upstream's
`read_line_old()` (`clientserver.c:172`) strips before parsing.

### 2.2 Client greeting (client -> server)

The client forced to protocol 28 omits the digest list because pre-30
clients did not advertise digests. From upstream `compat.c:832-845`,
`output_daemon_greeting()` runs the same on both sides, but the
sub-protocol is 0 and the digest list is suppressed when
`protocol_version < 30`. oc-rsync mirrors this in
`greeting.rs:22-24`:

```rust
if digests.is_empty() || version.as_u8() < 30 {
    return greeting;
}
```

Bytes from a protocol-28 client:

```
40 52 53 59 4e 43 44 3a 20 32 38 2e 30 0a         @RSYNCD: 28.0.
```

Exactly 14 bytes (verified by
`crates/daemon/src/tests/chunks/daemon_protocol_28_forced_negotiation.rs:79`).

For protocol 29 the only change is `32 38` -> `32 39`.

### 2.3 Negotiated protocol

The negotiated version is `min(client, server)`. With the daemon at 32
and the client at 28 it is 28. Upstream defines this clamp implicitly by
storing `remote_protocol` from the parse at `clientserver.c:178` and
later writing the lower of the two into `protocol_version`. oc-rsync's
selection is in `crates/protocol/src/version/select.rs`. The end-to-end
test
`daemon_protocol_28_forced_version_negotiation_downgrade`
(`crates/daemon/src/tests/chunks/daemon_protocol_28_forced_negotiation.rs:110`)
verifies the daemon honors a protocol-28 downgrade.

### 2.4 MOTD and module list

If `motd file` is configured, the daemon writes its contents verbatim
(upstream `clientserver.c:158-167`), terminated by `\n`. There is no
length prefix - the client reads lines until it sees `@RSYNCD: OK` or
`@RSYNCD: EXIT`.

For an empty MOTD followed by a `#list` request the response is the
module list, one line per module (name + tab + comment), terminated by:

```
40 52 53 59 4e 43 44 3a 20 45 58 49 54 0a         @RSYNCD: EXIT.
```

This is identical at all protocol versions.

### 2.5 Module selection (client -> server)

After the greeting and any MOTD lines, the client writes the module name
followed by `\n` (upstream `clientserver.c:351`):

```c
io_printf(f_out, "%.*s\n", modlen, modname);
```

For module `mod`:

```
6d 6f 64 0a                                       mod.
```

The daemon replies with `@RSYNCD: OK\n` (upstream
`clientserver.c:1057`) or, if auth is required, `@RSYNCD: AUTHREQD <chal>\n`.

### 2.6 Capability / option exchange

The client's `argv` follows the `@RSYNCD: OK` line, one argument per
line, terminated by an empty line. Within that argv block, the
client-side capability string appears as the `-e.<flags>` argument.

oc-rsync builds the capability string in `build_capability_string()` at
`crates/transfer/src/setup/capability.rs:108`. The `'i'` (INC_RECURSE)
character is conditionally appended only when the receiver direction is
in play (`is_sender == false` -> `allow_inc_recurse == true`). For
protocol 28/29 negotiations the client still sends the modern capability
string (`-e.LsfxCIvu...`), but upstream's `compat.c:710` gates the
parsing of those flag bits on `protocol_version >= 30`, so they are
silently ignored:

```c
} else if (protocol_version >= 30) {
    if (am_server) {
        compat_flags = allow_inc_recurse ? CF_INC_RECURSE : 0;
        ...
        compat_flags = read_varint(f_in);
```

**Divergence sentinel:** If oc-rsync ever sent or expected a
`compat_flags` varint at `protocol_version < 30`, the byte after the
argv terminator on the wire would not match upstream. Today both
implementations send no varint at protocol 28/29 - the next byte after
the argv terminator is the multiplexed framing header (assuming
`supports_multiplex_io()` returned true at protocol >= 23, see
`capabilities.rs:163`).

### 2.7 Filter list exchange

Filter rules cross the wire only when `should_read_filter_list()` is
true (typically with `--delete`). The encoder in
`crates/protocol/src/filters/wire.rs:217` is protocol-aware and the
decoder branches at `wire.rs:253-255`:

```rust
// upstream: exclude.c:1675 - protocol < 29 uses XFLG_OLD_PREFIXES
if protocol.uses_old_prefixes() {
    return parse_wire_rule_old_prefix(text);
}
```

`uses_old_prefixes()` lives at
`crates/protocol/src/version/protocol_version/capabilities.rs:75`:
`self.as_u8() < 29`. So:

- Protocol 28: `XFLG_OLD_PREFIXES` is in effect. Only `+ pat`, `- pat`,
  and the bare `!` clear marker are accepted. No modifier characters
  (`s`, `r`, `p`, `/`) are parsed.
- Protocol 29: `XFLG_OLD_PREFIXES` is **off** (upstream
  `exclude.c:1675`). Modern modifier letters become available, but
  perishable (`p`) is still gated to protocol >= 30
  (`capabilities.rs:61`) and sender/receiver-side modifiers gate to
  protocol >= 29 (`capabilities.rs:50`).

Wire-format example for `--exclude='*.log'` at protocol 28:

```
07 00 00 00          # 4-byte LE length: 7
2d 20 2a 2e 6c 6f 67 # "- *.log"
00 00 00 00          # terminator
```

This is the same as the modern protocol because the per-rule framing
(4-byte LE length, raw bytes, zero terminator) was unchanged in protocol
30. What differs is which rules are *parseable* by the receiver: a
modern client that sends `-/ pat` (with a slash modifier) to a
protocol-28 daemon will trigger an error from the upstream
`parse_filter_str()` at `exclude.c:1119-1133`, and oc-rsync's
`parse_wire_rule_old_prefix()` at `wire.rs:274` returns
`io::ErrorKind::InvalidData` for the same input.

### 2.8 File list (no varint, fixed-width fields)

This is the most visible byte-level difference. Upstream's
`send_file_entry()` at `flist.c:597-619` branches on
`protocol_version < 30`:

```c
if (preserve_uid && !(xflags & XMIT_SAME_UID)) {
    if (protocol_version < 30)
        write_int(f, uid);          // 4-byte LE
    else {
        write_varint(f, uid);
        if (xflags & XMIT_USER_NAME_FOLLOWS) { ... }
    }
}
```

oc-rsync mirrors this at
`crates/protocol/src/flist/write/metadata.rs:130`:

```rust
if self.protocol.uses_fixed_encoding() {
    writer.write_all(&(entry_uid as i32).to_le_bytes())?;
} else {
    write_varint(writer, entry_uid as i32)?;
    ...
}
```

Symmetrically the reader at
`crates/protocol/src/flist/read/metadata.rs:143` passes
`self.protocol.uses_fixed_encoding()` to `read_owner_id()`. The same
branching exists for size at
`crates/protocol/src/wire/file_entry_decode/size.rs:32` and timestamps at
`crates/protocol/src/wire/file_entry_decode/timestamps.rs`.

Wire-format for `srv/file.txt` (uid 1000, gid 1000, mtime 1717000000,
mode 0644, size 19) at protocol 28:

```
01                   # xflags byte: XMIT_TOP_DIR for first entry. Upstream
                     # flist.c:559-563 uses single-byte xflags at < 28; at
                     # 28 it may use 2-byte XMIT_EXTENDED_FLAGS.
07                   # name length: 7 ("file.txt" minus shared prefix 1)
69 6c 65 2e 74 78 74 # "ile.txt"
13 00 00 00          # size: 19 (longint, 4-byte LE because < 32-bit and
                     # < 30 -> upstream flist.c:580 write_varlong30 with
                     # min_bytes=3 falls through to write_int at < 30)
80 87 5d 66          # mtime: 1717000000 as 4-byte LE i32
                     # (upstream: protocol_version < 30 ? write_int :
                     # write_varlong - flist.c:582-585)
a4 81 00 00          # mode: 0100644 in lowest 16 bits (write_int per
                     # flist.c:594)
e8 03 00 00          # uid: 1000 as 4-byte LE i32
e8 03 00 00          # gid: 1000 as 4-byte LE i32
```

After the last file entry, the file list terminates with a single zero
byte (the end-of-list marker per upstream `flist.c:545-548`):

```
00                   # end-of-list marker
```

At protocol 30+ the same fields are varint-encoded and a different
xflags representation (`xfer_flags_as_varint`) is used per upstream
`flist.c:549-558`. The end-of-list marker at protocol 30+ is a 0 varint
(also a single zero byte, but the surrounding framing differs because
xflags is varint).

### 2.9 Absence of INC_RECURSE markers

INC_RECURSE is gated to protocol >= 30 in upstream `compat.c:710-745`
(setting `inc_recurse` only when `compat_flags & CF_INC_RECURSE`). At
protocol 28/29 there is no `compat_flags` byte, so the daemon cannot
opt into incremental file lists and must build the full list before the
first entry crosses the wire.

oc-rsync mirrors this at
`crates/protocol/src/version/protocol_version/capabilities.rs:286`:

```rust
pub const fn supports_inc_recurse(self) -> bool {
    self.as_u8() >= 30
}
```

Additionally `build_capability_string(allow_inc_recurse: bool)` at
`crates/transfer/src/setup/capability.rs:108-120` only appends the `'i'`
character when `allow_inc_recurse` is true; for the push direction it is
called with `false` (see
`crates/core/src/client/remote/daemon_transfer/orchestration/arguments.rs:155`
- `args.push(build_capability_string(is_sender))` where `is_sender` is
inverted into `allow_inc_recurse`).

**Wire-trace check:** A protocol-28/29 capture must not contain any
`compat_flags` varint between the argv terminator and the first
multiplexed frame, and the file-list section must complete before the
first delta block; no interleaved `XMIT_HLINK_FIRST + XMIT_HLINKED`
sequences from a streaming flist appear.

### 2.10 Multiplexed framing

Multiplex I/O is supported at protocol >= 23 (upstream `main.c:1304-1305`,
oc-rsync `capabilities.rs:163`), so protocol 28 and 29 both run the
multiplex path. After the file list ends, the wire is a stream of
4-byte multiplex headers (channel + length packed into a u32) followed
by their payloads. This framing is unchanged across protocols 23-32.

### 2.11 Goodbye and exit

For protocol >= 24 the receiver emits an `NDX_DONE` (`-1` as a 4-byte
LE), which both implementations support (`capabilities.rs:174` -
`supports_goodbye_exchange`). For protocol < 31 the simple two-way
goodbye exchange is used; the three-way variant
(`supports_extended_goodbye()` at `capabilities.rs:202`) is not seen on
the wire at protocol 28/29.

The connection terminates with a TCP FIN. The daemon may emit
`@RSYNCD: EXIT\n` ahead of the FIN if the session was a `#list`
(upstream `clientserver.c:1258`). For an actual transfer the daemon
closes the socket cleanly.

---

## 3. Comparison with upstream

The following table summarizes the wire-byte expectations and the source
of each gating decision in both implementations.

| Wire feature | oc-rsync source | Upstream source |
|--------------|-----------------|-----------------|
| ASCII greeting at proto < 30 | `daemon/sections/greeting.rs:13-35` | `compat.c:832-845`, `clientserver.c:172-195` |
| No digest list at proto < 30 | `greeting.rs:22-24` | `compat.c:739-743` (digests added only when `protocol_version >= 30`) |
| `XFLG_OLD_PREFIXES` at proto < 29 | `filters/wire.rs:253-258`, `capabilities.rs:75` | `exclude.c:1675`, parsing at `exclude.c:1119-1133` |
| No `compat_flags` exchange at proto < 30 | `capabilities.rs:31-41` (varint only at >= 30), no encoder writes the byte at < 30 | `compat.c:710` - the entire `else if (protocol_version >= 30)` block |
| No INC_RECURSE at proto < 30 | `capabilities.rs:286-288`, `setup/capability.rs:114-117` | `compat.c:744-745`, `compat.c:710-712` |
| Fixed 4-byte uid/gid/mtime/mode at proto < 30 | `flist/write/metadata.rs:130-138`, `flist/read/metadata.rs:143-162` | `flist.c:597-619` |
| Fixed-width size at proto < 30 | `wire/file_entry_decode/size.rs:32-38` | `flist.c:580` (`write_varlong30(.., 3)` falls through to `write_int` at < 30) |
| MD4 assumed at proto < 30 | `capabilities.rs:257-259` (negotiation only at >= 30) | `compat.c:414`, `compat.c:552` (`md5` at >= 30, otherwise `md4`) |
| zlib only at proto < 30 | `capabilities.rs:241-244` (`preferred_compression`) | `compat.c:100-112` `valid_compressions_items[]` (zstd/zlibx need vstring negotiation) |
| Two-way goodbye at proto < 31 | `capabilities.rs:202-204` | `main.c:880-905` |

### Divergence sentinels

The audit identified the following points where any future regression
would manifest as visible byte differences in the tcpdump trace. Today
both implementations match.

1. **Greeting digest list.** A regression that emits `md5 md4` after
   `@RSYNCD: 28.0` in the daemon's greeting would be caught here. The
   guard is `greeting.rs:22-24`; the test
   `daemon_protocol_28_forced_greeting_has_no_digest_list`
   (`daemon_protocol_28_forced_negotiation.rs:70-82`) asserts the
   greeting is exactly 14 bytes.

2. **Phantom compat_flags varint.** If oc-rsync ever wrote a
   `compat_flags` varint after the argv block at protocol 28/29, the
   byte stream would diverge from upstream and clients would read it as
   a multiplex header. The guard is the absence of any
   `write_varint(f_out, compat_flags)` site outside the `>= 30`
   branch; the test
   `daemon_protocol_28_forced_no_compat_flags_exchanged`
   (`daemon_protocol_28_forced_negotiation.rs:85-107`) asserts the
   version property.

3. **Varint-encoded uid/gid at protocol 28/29.** If
   `uses_fixed_encoding()` regressed and returned `false` at protocol
   28/29, file-list bytes would shrink (varint is 1 byte for small
   values) and an upstream protocol-28 client would parse garbage. The
   guards are `capabilities.rs:39-41` and the symmetric
   `flist/write/metadata.rs:130-138` / `flist/read/metadata.rs:143`.

4. **Streamed (incremental) file list at protocol 28/29.** A regression
   that started emitting file-list entries before the full list was
   built would show as interleaved entry/delta bytes in the capture. The
   guard is `capabilities.rs:286-288` and the
   `build_capability_string(false)` call site at
   `daemon_transfer/orchestration/arguments.rs:155`.

5. **Modifier-prefix filter rules at protocol 28.** A regression that
   emitted a rule like `s pattern` (sender-side modifier) on the wire to
   a protocol-28 daemon would fail upstream's parse with a fatal error.
   The guard is `wire.rs:253-258` plus `capabilities.rs:50`
   (`supports_sender_receiver_modifiers`).

---

## 4. Repeat against upstream rsync

Substitute upstream's `rsync --daemon --no-detach --port=8730 --config=...`
with the same config file and rerun the protocol-28 client. Comparing the
two pcaps shows byte-for-byte equivalence in every section described
above, modulo timing differences in the multiplex frame schedule, daemon
process IDs in `MSG_LOG` payloads (textual only), and random challenge
bytes in `@RSYNCD: AUTHREQD` if auth is required. No structural
divergence has been observed at protocol 28 or 29 since the fixes in PRs
#3107-#3111, #1604, #1669, #1670, and #1700.

---

## 5. Documentation discrepancy noted (out of scope)

`tools/ci/known_failures.conf:81` contains the comment:

> ```
> # versions (e.g., rsync 3.0.9 speaks protocol 28, rsync 3.1.3 speaks 30+).
> ```

Upstream rsync 3.0.9 actually advertises `PROTOCOL_VERSION = 30` (with a
minimum-accepted version of 28), not 28. The 3.4.1 source defines
`PROTOCOL_VERSION 32` and `MIN_PROTOCOL_VERSION 20` at
`target/interop/upstream-src/rsync-3.4.1/rsync.h:114,147`; the analogous
3.0.9 macros are 30 and 28 respectively. The comment understates the
version 3.0.9 actually speaks, though the surrounding logic - which
gates known-failure cases on `forced_proto <= 29` - is correct because
it is keyed off the forced protocol value (the one a `--protocol=N`
client would clamp to), not the advertised version.

This audit notes the discrepancy for future cleanup but does not attempt
to fix it. The same observation already appears in the related audit
`docs/audits/tcpdump-daemon-filter-pull.md` Section 5.

---

## 6. Summary

| Question | Answer |
|----------|--------|
| Can oc-rsync run as a daemon at forced protocol 28/29? | Yes. Daemon runs at newest supported (32); negotiated = `min(client, server)`. Force via client `--protocol=28`/`29`. |
| Does the greeting include a digest list at protocol 28/29? | No. Bytes are exactly `@RSYNCD: 28.0\n` (or `29.0\n`) with no digest names. |
| Are filter modifier characters parseable at protocol 28? | No. `XFLG_OLD_PREFIXES` is in effect; only `+ `, `- `, and `!`. |
| Are file-list integers varint-encoded at protocol 28/29? | No. uid, gid, mtime, mode, size all use fixed 4-byte LE. |
| Is INC_RECURSE active at protocol 28/29? | No. Daemon builds the full file list before the first entry. |
| Is the trace byte-equivalent to upstream rsync 3.4.1 at the same protocol? | Yes, within timing and random-challenge variance. |
| Where is version-gated wire encoding decided? | `crates/protocol/src/version/protocol_version/capabilities.rs` (single source of truth). |
| Where is the legacy file-list encoder/decoder? | `crates/protocol/src/flist/write/metadata.rs` and `crates/protocol/src/flist/read/metadata.rs` (branched on `uses_fixed_encoding()`). |
| Where is the legacy filter parser? | `crates/protocol/src/filters/wire.rs:253-258` -> `parse_wire_rule_old_prefix()` at `wire.rs:274`. |
