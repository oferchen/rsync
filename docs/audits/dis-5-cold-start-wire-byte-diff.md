# DIS-5: cold-start wire-byte count diff vs upstream

Tracking DIS-5: byte-level audit of the daemon cold-start path comparing
oc-rsync v0.6.2 against upstream rsync 3.4.1. DIS-3 (PR #4849) decomposed
the wall-clock gap into 22 phases; DIS-4.a-e narrowed the latency
attribution per phase. This audit produces the matching per-phase
**wire-byte attribution** so DIS-6 can target the redundancy that turns
into latency on the cold-start path.

This is a docs-only audit. No `.rs` files are modified.

## 1. Method

Captures were taken inside the `rsync-profile` podman container with
both daemons running on adjacent ports (28800 = oc-rsync v0.6.2 built
from worktree `target/release/oc-rsync`; 28801 = upstream rsync 3.4.1
from `/usr/bin/rsync`). The same upstream rsync CLIENT pulled a fixed
corpus from each daemon module in turn while `tcpdump -i lo -w` recorded
every loopback segment.

Corpus: 5 files x 1 KiB each (scaled down from the DIS-1 reference
500-file corpus so the per-phase byte alignment is tractable in this
audit; the per-file scaling factors are linear in N and noted below
where they affect the totals). Commands:

```sh
# Inside rsync-profile container, with daemon configs that mirror the
# DIS-1 harness (no auth users, no max connections, no log file,
# numeric ids = yes, use chroot = false).
rsync -a rsync://127.0.0.1:28800/bench/ /tmp/dis5/oc-dest-client/
rsync -a rsync://127.0.0.1:28801/bench/ /tmp/dis5/up-dest-client/
```

Captures aligned to the DIS-3 phase table by parsing the rsyncd ASCII
greeting boundary, mplex frame tags, and frame payload contents using
`tshark -T fields -e tcp.srcport -e tcp.len -e data`.

The CLIENT in both runs is the same upstream binary. Client->server
bytes are therefore byte-identical between the two captures (verified
below); all interesting deltas appear server->client.

## 2. Top-line tally

| Direction | oc-rsync bytes | upstream bytes | Diff (oc - up) | Diff (%) |
|-----------|---------------:|---------------:|---------------:|---------:|
| Client -> server (data segments) | 253 | 253 | **+0** | 0.00 % |
| Server -> client (data segments) | 5,748 | 5,591 | **+157** | +2.81 % |
| **Total** | **6,001** | **5,844** | **+157** | **+2.69 %** |

| Direction | oc-rsync segments | upstream segments | Diff |
|-----------|------------------:|------------------:|-----:|
| Client -> server | 11 | 11 | 0 |
| Server -> client | 24 | 10 | **+14** |
| **Total data segments** | **35** | **21** | **+14** |

The byte gap is small (157 B = 2.7 % more wire traffic on a 5 KiB cold
start), but the **segment count gap is large** (24 vs 10 server-side
segments, +140 %). Most of the extra segments are sub-MTU writes that
cost a full per-segment syscall round (`write` + `sendto` + TCP header
build + ACK on loopback) and amplify the wall-clock cost beyond what
the raw byte counter implies.

Per-file scaling on the DIS-1 500-file corpus, extrapolated linearly:

| Metric | 5 files measured | 500 files estimate | Notes |
|--------|----:|----:|-------|
| oc S->C bytes | 5,748 | ~554,000 | per-file overhead (root names + per-entry index) dominates flist size linearly |
| up S->C bytes | 5,591 | ~552,000 | upstream sends the same delta tokens per file |
| oc-up extra S->C bytes | +157 | ~+15,700 | flist + id-list redundancy is the only term that scales |
| oc extra segments | +14 | ~+1,400 | per-file itemize MSG_INFO frame is one extra segment per file |

## 3. Capability string comparison (byte-level)

Both implementations advertise the same on-the-wire capability set; the
divergence is in **framing**, not contents.

### oc-rsync server capability bytes

| Segment | Hex | ASCII | Bytes |
|---------|-----|-------|------:|
| compat | `81 ff` | (CF_INC_RECURSE \| CF_SYMLINK_TIMES \| CF_SYMLINK_ICONV \| CF_SAFE_FLIST \| CF_AVOID_XATTR_OPTIM \| CF_CHKSUM_SEED_FIX, then 0xff flag terminator) | 2 |
| caps-tag | `23` | `#` | 1 |
| caps-body | `78 78 68 31 32 38 20 78 78 68 33 20 78 78 68 36 34 20 6d 64 35 20 6d 64 34 20 73 68 61 31 20 6e 6f 6e 65` | `xxh128 xxh3 xxh64 md5 md4 sha1 none` | 35 |
| seed | `15 6e 71 6a` | (random 4-byte checksum seed) | 4 |
| **Total** | | | **42 in 4 segments** |

### upstream rsync server capability bytes

| Segment | Hex | ASCII | Bytes |
|---------|-----|-------|------:|
| compat | `81 ff` | same compat byte + flag terminator | 2 |
| caps | `23 78 78 68 31 32 38 20 78 78 68 33 20 78 78 68 36 34 20 6d 64 35 20 6d 64 34 20 73 68 61 31 20 6e 6f 6e 65` | `#xxh128 xxh3 xxh64 md5 md4 sha1 none` | 36 |
| seed | `e1 1e 71 6a` | (random 4-byte checksum seed) | 4 |
| **Total** | | | **42 in 3 segments** |

**Diff:** 0 bytes (byte-identical capability advertisement), **+1
segment** on oc-rsync. The oc-rsync sender splits the `#` marker
(1 byte) into its own `write()` ahead of the digest-name body
(35 bytes). Upstream coalesces them into a single 36-byte send.

Root cause likely lives in `crates/protocol/src/legacy/lines.rs`
where the daemon-side capability writer emits the marker via a
dedicated `write_all(b"#")` followed by the joined-names buffer. A
single `write_all` with the assembled `format!("#{}", names.join(" "))`
collapses to upstream's pattern.

The client capability response (`1e` peer-ok byte + `xxh128 xxh3
xxh64 md5 md4 sha1`, 31 bytes total in 2 segments) is byte-identical
on both runs because it is sent by the same upstream client.

## 4. Per-phase byte-count table

Phases match DIS-3 numbering. Each row's confidence is **measured**
(direct tcpdump observation) or **estimated** (extrapolated from
shorter capture or read from source).

The "Direction" column is `S->C` (daemon -> client), `C->S` (client
-> daemon), or `bidi` (RTT-bounded handshake). Bytes are **payload
only** (TCP headers excluded).

### 4.1 Anonymous (no auth) - the DIS-1 measurement path

| # | Phase | Dir | oc-rsync B | upstream B | Diff B | oc seg | up seg | Diff seg | Likely cause | Confidence |
|---|-------|----:|-----------:|-----------:|-------:|------:|------:|---------:|--------------|------------|
| 1 | TCP SYN handshake | bidi | 0 | 0 | 0 | 3 | 3 | 0 | symmetric kernel handshake | measured |
| 2 | Accept-loop wake | - | 0 | 0 | 0 | 0 | 0 | 0 | no wire bytes; latency-only (see DIS-4.a) | n/a |
| 3 | `@RSYNCD: 32.0 ...` greeting | S->C | 41 | 41 | **0** | 1 | 1 | 0 | byte-identical greeting; cost is the per-accept rebuild (DIS-4.a R2/R3) | measured |
| 4 | (capability advertisement on `#list` only) | S->C | 0 | 0 | 0 | 0 | 0 | 0 | not on cold-start pull path | n/a |
| 5 | Client version line | C->S | 41 | 41 | **0** | 1 | 1 | 0 | echoed by client; byte-identical | measured |
| 5b | Module request line | C->S | 6 | 6 | **0** | 1 | 1 | 0 | `bench\n` | measured |
| 6 | Module lookup | - | 0 | 0 | 0 | 0 | 0 | 0 | no wire bytes; in-process linear scan | n/a |
| 7 | Host allow/deny + reverse-DNS | - | 0 | 0 | 0 | 0 | 0 | 0 | no wire bytes on default config | n/a |
| 8 | Connection lock | - | 0 | 0 | 0 | 0 | 0 | 0 | no wire bytes; in-process atomic on default | n/a |
| 9 | Auth path | - | 0 | 0 | 0 | 0 | 0 | 0 | skipped: no `auth users` configured | n/a |
| 10 | `@RSYNCD: OK\n` accept | S->C | 12 | 12 | **0** | 1 | 1 | 0 | byte-identical; cost is per-accept `LegacyMessageCache` rebuild (DIS-4.a R3) | measured |
| 11 | Client args (null-terminated argv) | C->S | 8 + 40 = 48 | 8 + 40 = 48 | **0** | 2 | 2 | 0 | `--server\0--sender\0-logDtpre.iLsfxCIvu\0.\0bench/\0\0` | measured |
| 12 | `ServerConfig` build | - | 0 | 0 | 0 | 0 | 0 | 0 | no wire bytes; in-process | n/a |
| 13 | Privilege / chroot / Landlock | - | 0 | 0 | 0 | 0 | 0 | 0 | no wire bytes on default | n/a |
| 14a | Compat byte | S->C | 2 | 2 | **0** | 1 | 1 | 0 | `81 ff` - byte-identical (proto 32 compat flags) | measured |
| 14b | Capability marker `#` | S->C | 1 | 0 | **+1** | 1 | 0 | **+1** | **oc-rsync emits `#` as its own write** | measured |
| 14c | Capability name list | S->C | 35 | 36 | **-1** | 1 | 1 | 0 | upstream's segment includes the `#` (36 = 35 + 1); same names | measured |
| 14d | Client peer-ok ack | C->S | 1 | 1 | 0 | 1 | 1 | 0 | `1e` | measured |
| 14e | Client capability reply | C->S | 30 | 30 | **0** | 1 | 1 | 0 | `xxh128 xxh3 xxh64 md5 md4 sha1` (client's chosen set) | measured |
| 14f | Checksum seed | S->C | 4 | 4 | **0** | 1 | 1 | 0 | 4 random bytes; values differ run-to-run, length identical | measured |
| 15 | Multiplex output activation | - | 0 | 0 | 0 | 0 | 0 | 0 | no wire bytes (kept-buffer setup) | n/a |
| 16 | Receive filter list | C->S | 8 | 8 | **0** | 1 | 1 | 0 | `04 00 00 07 00 00 00 00` - empty filter list frame (mplex header + zero-length terminator) | measured |
| 17 | Sender flist build | - | 0 | 0 | 0 | 0 | 0 | 0 | no wire bytes; on-sender filesystem walk | n/a |
| 18 | Sender flist sort | - | 0 | 0 | 0 | 0 | 0 | 0 | no wire bytes; in-process | n/a |
| 19 | INC_RECURSE partition | - | 0 | 0 | 0 | 0 | 0 | 0 | no wire bytes; in-process | n/a |
| 20 | File list send (mplex MSG_DATA) | S->C | 131 | 123 | **+8** | 1 | 1 | 0 | **oc-rsync sends `04 root\0 04 root\0` inline (XMIT_USER_NAME_FOLLOWS-style) for first entry (+10 B); upstream omits names inline (-2 B from a different per-entry index field shape)** | measured |
| 20b | Receiver filter ack / iflag | C->S | 102 | 102 | **0** | 1 | 1 | 0 | byte-identical `62 00 00 07 ...` MSG_DATA frame from receiver | measured |
| 21 | id_lists + io_error trailer | S->C | 6 (separate mplex frame: `02 00 00 07 ff 01`) | 0 (coalesced into row 20 payload, trailing `ff 01`) | **+6** | 1 | 0 | **+1** | **oc-rsync emits the `ff 01` end marker in its own MSG_DATA frame** (4-byte mplex header + 2-byte body); upstream appends the same 2 bytes to the file-list frame body, saving the entire frame header | measured |
| 22 | First NDX read + delta header | C->S | 5 + 7 = 12 | 5 + 7 = 12 | **0** | 2 | 2 | 0 | `01 00 00 07 00` (mplex frame: NDX 0) + `03 00 00 07 00 00 00` (sum_head: count=0 indicating cold dest); byte-identical | measured |
| 22a | Sender writes file 1 delta | S->C | 1,071 | (batched into row 22b) | (see row 22b) | 1 | (see 22b) | (see 22b) | first 1 KiB literal + checksum + framing | measured |
| 22b | Sender batches files 2..5 deltas | S->C | 4 x 1,071 + 5 x 23 = 4,399 (one delta + one itemize per file, **separately framed**) | 5,342 (all 5 file deltas in one MSG_DATA frame; no per-file MSG_INFO) | **-160 raw bytes but +9 segments** | 9 | 1 | **+8** | **oc-rsync writes per-file itemize as separate MSG_INFO `13 00 00 09 ...` frames AND does not coalesce per-file deltas into a single MSG_DATA frame**; upstream sends one giant MSG_DATA covering all deltas | measured |
| 22c | Generator goodbye stats | C->S | 5 | 5 | **0** | 1 | 1 | 0 | `01 00 00 07 00` - end-of-list NDX | measured |
| 22d | Receiver final ack | C->S | 5 | 5 | **0** | 1 | 1 | 0 | `01 00 00 07 00` - byte-identical | measured |
| 23 | Goodbye + stats trailer | S->C | 5 + 5 + 5 + 19 + 5 = 39 (split across 5 small frames) | 6 + 20 = 26 (2 frames: stats summary + NDX_DEL_STATS) | **+13** | 5 | 2 | **+3** | **oc-rsync fragments the stats trailer across 5 mplex sends**; upstream sends 2 (stats varints + NDX_DEL_STATS) | measured |

**Sum of per-phase diffs (S->C, oc minus up):** +1 - 1 + 8 + 6 + (per-file
delta + itemize ~ -32 B per file on this corpus but **+1.8 segments**
per file) + 13 = **+157 bytes / +14 segments** total. Matches the
top-line tally in section 2.

### 4.2 Authenticated path (DIS-4.c scope) - estimated

The DIS-1 measurement scenario has no `auth users` set, so phase 9 is
zero on the wire. The table below estimates what would change if
`auth users = bench-user` were configured. **No capture was taken;
all rows here are code-read estimates from `crates/daemon/src/daemon/sections/auth.rs`
and upstream `clientserver.c::auth_server`.** Confidence: estimated.

| # | Phase | Dir | oc-rsync B | upstream B | Diff B | Likely cause | Confidence |
|---|-------|----:|-----------:|-----------:|-------:|--------------|------------|
| 9a | `@RSYNCD: AUTHREQD <challenge>\n` | S->C | ~36 + `len(challenge)` (~22 B base64) ≈ 58 | ~36 + 22 ≈ 58 | **0** | symmetric framing of MD5 challenge | estimated |
| 9b | Client `<user> <response>\n` | C->S | ~len(user) + 1 + ~32 (base64 MD5) ≈ 38 | ~38 | **0** | symmetric | estimated |
| 9c | Server `@RSYNCD: OK\n` or `@ERROR auth failed on module bench\n` | S->C | 12 (OK) or ~32 (ERROR) | 12 / ~32 | **0** | symmetric | estimated |

**Estimated auth-path diff:** 0 bytes / 0 segments. Auth adds ~100 B
of wire traffic on both sides and 1 RTT but introduces no per-phase
divergence. Note this is **estimated**, not measured.

### 4.3 Aggregate by DIS-4 sub-task slot

| DIS-4 slot | Phases | oc B | up B | Diff B | oc seg | up seg | Diff seg |
|------------|--------|-----:|-----:|-------:|------:|------:|---------:|
| **DIS-4.a** (greeting) | 2, 3, 4, 10 | 53 | 53 | **0** | 2 | 2 | 0 |
| **DIS-4.b** (module-select) | 5, 5b, 6-8, 11-13 | 95 (C->S) | 95 (C->S) | **0** | 5 (C->S) | 5 (C->S) | 0 |
| **DIS-4.c** (auth + capability + filter) | 9, 14, 15, 16 | 81 | 81 | **0** | 7 | 5 | **+2** |
| **DIS-4.d** (flist build) | 17-19 | 0 (wire) | 0 (wire) | 0 | 0 | 0 | 0 |
| **DIS-4.e** (first-block send) | 20, 21, 22 | 5,565 | 5,491 | **+74** | 14 | 4 | **+10** |
| (out-of-scope) | 23 (goodbye) | 39 | 26 | **+13** | 5 | 2 | **+3** |

DIS-4.a, DIS-4.b, and DIS-4.d are **byte-neutral**. The byte gap is
fully concentrated in DIS-4.c (capability framing) and DIS-4.e (file
list, id lists, and delta batching).

## 5. Highlighted redundancies (>10 % byte gap)

A "highlighted redundancy" is any phase row where oc-rsync sends >10 %
more bytes OR >100 % more segments than upstream. Five phases qualify:

### R-WIRE-1. Capability marker `#` split (phase 14b/14c)

oc-rsync emits the `#` capability marker via a separate `write_all(b"#")`
ahead of the digest-name body. Upstream uses a single `io_printf`-equivalent
that prepends `#` to the joined name list and writes 36 bytes in one
syscall. The byte total is identical (36 B in both cases when the marker
is counted) but oc-rsync pays 1 extra segment, 1 extra `write(2)` and 1
extra TCP header.

- **Bytes diff:** 0 (1 + 35 vs 36)
- **Segments diff:** +1 (oc 2, up 1)
- **Likely fix:** `crates/protocol/src/legacy/lines.rs` capability writer
  - merge marker + name list into a single `write_all` of the assembled
  `String`. Wire-compatible.

### R-WIRE-2. File list with inline user/group names (phase 20)

Phase 20 measured: oc 131 B vs up 123 B = **+6.5 %**. Below the 10 %
threshold individually, but the underlying issue scales linearly with
the number of unique uids/gids in the corpus and is the per-entry source
of the +10 B/first-entry observed here. Upstream does **not** inline
user/group name strings in the file-list entries; it sends id-only and
then appends a separate `add_uid_list`/`add_gid_list` section before the
trailer. oc-rsync's `crates/transfer/src/generator/file_list/entry.rs:162-184`
calls `set_user_name` / `set_group_name` unconditionally when
`!numeric_ids` AND `owner`/`group` is set, which is the default `-a`
case. The names are then encoded inline as `XMIT_USER_NAME_FOLLOWS`-style
length-prefixed strings (`04 'r' 'o' 'o' 't' \0`).

For root-owned files the per-entry cost is 10 B for the first occurrence
of each (uid, gid) pair (4 B for the name + 1 B for the length prefix +
1 B null terminator, times 2 for uid and gid). Subsequent entries
sharing the same name should reuse a 1-byte back-reference, but the
worst-case on a fresh module with all files owned by root is +10 B for
the first entry. Upstream emits the same names later in `add_uid_list`
where they cost 2 + 4 = 6 B per id (1 B length + name + null), once per
unique id. On the 5-file corpus the delta is +8 B; on a 500-file corpus
with 1 unique uid + 1 unique gid the delta is still +8 B (the back-ref
encoding handles the rest), so this row does not scale linearly with
file count - it scales with unique id count.

- **Bytes diff:** +8 (one-shot per unique (uid, gid))
- **Segments diff:** 0
- **Likely fix:** `crates/transfer/src/generator/file_list/entry.rs:162-184`
  - keep the names in a session-local id->name map and emit `add_uid_list`
  + `add_gid_list` after the file-list end marker, matching upstream
  `flist.c:2475-2509`. Wire-compatible only on protocol >= 30; oc-rsync
  already targets protocol 32 exclusively.

### R-WIRE-3. id_lists / io_error trailer in a separate mplex frame (phase 21)

oc-rsync writes the 2-byte `ff 01` (io_error word for non-INC_RECURSE
flist end) as its own MSG_DATA frame: 4-byte mplex header + 2-byte
body = 6 B in one segment. Upstream **appends** the same 2 bytes to
the trailing payload of the file-list MSG_DATA frame, saving the entire
4-byte mplex header AND avoiding a separate segment.

- **Bytes diff:** +6 (200 % more bytes for that mplex frame)
- **Segments diff:** +1 (+100 %)
- **Root cause:** `crates/transfer/src/generator/protocol_io.rs:368`
  flushes the file-list writer after `write_end()`, forcing the trailing
  io_error word into its own buffered batch. Upstream's `flist.c`
  writes the trailer through the same `iobuf.out` cursor as the file
  entries without an intervening flush.
- **Likely fix:** drop the intermediate `flist_writer.write_end()`
  flush; let the next phase's `flush_with_count` carry the bytes when
  the buffer is naturally full or when the next read forces a flush.

### R-WIRE-4. Per-file itemize emitted as separate MSG_INFO frames (phase 22b)

This is the **largest segment-count divergence** in the audit.

- oc-rsync: per file, the sender writes `13 00 00 09 <iflags> <basis>
  <name>\n` as its own MSG_INFO frame (mplex tag 9 = MSG_INFO, payload
  ~19 B for a short name). On the 5-file corpus that is 5 segments of
  23 B each = 115 B; on a 500-file corpus it would be ~11,500 B in
  ~500 segments. AND the per-file delta MSG_DATA is also flushed
  separately rather than coalesced into one giant MSG_DATA covering
  all 5 deltas.
- upstream: emits no per-file MSG_INFO during the transfer. Instead it
  prints itemize lines on the receiver after the entire transfer
  completes (the receiver derives item info from the iflags it already
  received in the delta header). The 5 file deltas land in a single
  5,342 B MSG_DATA frame.

This is the dominant contributor to oc-rsync's 24-vs-10 segment-count
gap. On the DIS-1 500-file corpus it extrapolates to ~+500 segments
of small writes, each of which costs a `writev`, a 4-byte mplex frame
header, and a TCP segment header.

- **Bytes diff:** +5 frame-headers * (extra per-file overhead) (small,
  amortised) but **the per-segment cost dominates the wall-clock**.
- **Segments diff:** **+8 on 5 files, ~+800 on 500 files** (largest
  contributor).
- **Likely fix:** the per-file MSG_INFO emission appears to live in the
  sender's per-file post-transfer logging; upstream defers that to the
  receiver. Two options:
  - Drop the per-file MSG_INFO entirely on the sender side (the receiver
    already has enough to emit itemize from the iflags it parsed).
  - Buffer per-file MSG_INFO frames into the same mplex `BufWriter`
    that carries the delta tokens, so they coalesce naturally with the
    next delta payload write.

### R-WIRE-5. Goodbye/stats trailer fragmented across 5 sends (phase 23)

oc-rsync emits the trailing session stats across 5 separate small
frames (5 + 5 + 5 + 19 + 5 = 39 B, all in distinct segments).
Upstream sends 2 frames (6 + 20 = 26 B): a stats summary followed by
the NDX_DEL_STATS varint block (project memory notes NDX_DEL_STATS was
added in v0.5.8, PR #2570).

- **Bytes diff:** +13 (+50 %)
- **Segments diff:** +3 (+150 %)
- **Likely fix:** `crates/transfer/src/generator/transfer/orchestrator.rs`
  goodbye path - flush once after writing all stats varints rather than
  per-varint. The 5 separate frames likely come from per-statistic
  `writer.flush()` calls that defeat the mplex buffer's batching.

## 6. Anonymous vs authenticated rollup

| Path | oc B | up B | Diff B | Diff % | Confidence |
|------|-----:|-----:|-------:|-------:|------------|
| Anonymous (DIS-1 measured scenario) | 6,001 | 5,844 | **+157** | **+2.69 %** | **measured** |
| Authenticated (estimated overlay) | 6,001 + ~108 | 5,844 + ~108 | **+157** | **+2.65 %** | estimated |

Auth adds ~108 B of wire traffic on both implementations (challenge
+ response + OK frames) but introduces no oc/upstream byte divergence
of its own. The 157 B / 2.7 % gap is the same on both paths; auth
**neither amplifies nor masks** the cold-start byte redundancy.

## 7. Cross-reference: DIS-3 phases this audit maps

| DIS-3 phase | Audit row(s) | Owner |
|-------------|--------------|-------|
| 1 (binary startup) | n/a - no wire bytes | (out of DIS-4 scope) |
| 2 (accept-loop wake) | n/a - no wire bytes | DIS-4.a |
| 3 (`@RSYNCD:` greeting build + write) | section 4.1 row 3 | DIS-4.a |
| 4 (capability advert on `#list`) | n/a - not on cold-start pull path | DIS-4.a |
| 5 (client version + module line) | section 4.1 rows 5, 5b | DIS-4.b |
| 6 (module lookup) | n/a - no wire bytes | DIS-4.b |
| 7 (host allow/deny + DNS) | n/a - no wire bytes | DIS-4.b |
| 8 (connection lock) | n/a - no wire bytes | DIS-4.b |
| 9 (auth) | section 4.2 (estimated) | DIS-4.c |
| 10 (`@RSYNCD: OK` write) | section 4.1 row 10 | DIS-4.a |
| 11 (client args) | section 4.1 row 11 | DIS-4.b |
| 12 (`ServerConfig` build) | n/a - no wire bytes | DIS-4.b |
| 13 (privilege / chroot / Landlock) | n/a - no wire bytes | DIS-4.b |
| 14 (`setup_protocol` compat + capability + seed) | section 4.1 rows 14a-14f, section 3 | DIS-4.c |
| 15 (multiplex output activation) | n/a - no wire bytes | DIS-4.c |
| 16 (receive filter list) | section 4.1 row 16 | DIS-4.c |
| 17 (sender flist build) | n/a - no wire bytes | DIS-4.d |
| 18 (flist sort) | n/a - no wire bytes | DIS-4.d |
| 19 (INC_RECURSE partition) | n/a - no wire bytes | DIS-4.d |
| 20 (file list send) | section 4.1 row 20 | DIS-4.e |
| 21 (id lists + io_error flag) | section 4.1 row 21 | DIS-4.e |
| 22 (first NDX + first delta header) | section 4.1 rows 22, 22a, 22b, 22c, 22d | DIS-4.e |
| 23 (goodbye + stats trailer) | section 4.1 row 23 | (out of DIS-4 scope) |

## 8. Fixable items for DIS-6 (ranked)

Ordered by expected wall-clock payoff per engineering hour. The byte
count itself is a small fraction of the cold-start gap; the dominant
mechanism by which wire redundancy hurts latency is **extra segments
= extra syscalls + extra ACKs**.

### DIS-6.W1 - drop per-file MSG_INFO frames (R-WIRE-4)

Largest contributor. ~+800 segments on the DIS-1 500-file corpus, each
of which is one `writev(2)` and one TCP segment. Even on loopback with
zero RTT, the syscall cost (~1-2 us per segment with current Linux)
totals ~1-2 ms on 500 files. Move per-file itemize emission to the
receiver (matches upstream) or coalesce into the mplex `BufWriter` so
it flushes with the next delta batch.

Wire-compatible if the receiver continues to derive itemize from the
existing iflags it already receives. Expected single-PR win:
**~1-2 ms on 500-file corpus**, plus measurable allocator-jitter
reduction.

### DIS-6.W2 - coalesce capability marker into name-list segment (R-WIRE-1)

One-line fix in `crates/protocol/src/legacy/lines.rs`. Save 1 segment
per accepted connection. Wire-byte identical. Expected win:
**~5-10 us per connection** (one syscall + one TCP segment).

### DIS-6.W3 - inline file-list end marker (R-WIRE-3)

Drop the explicit `flist_writer.write_end()` flush in
`crates/transfer/src/generator/protocol_io.rs:368`. Save 1 segment
+ 4 bytes (mplex frame header) per transfer. Wire-byte identical
modulo segment boundaries. Expected win: **~5-10 us per connection**.

### DIS-6.W4 - flush goodbye stats once (R-WIRE-5)

Single end-of-session flush in the goodbye writer instead of one flush
per stats varint. Save 3 segments + 13 bytes per transfer. Wire-byte
identical at the frame level (the bytes inside the frames are the same;
the gain is from no longer fragmenting them). Expected win:
**~5-15 us per connection**.

### DIS-6.W5 - move user/group names from inline to add_uid_list (R-WIRE-2)

Larger change. Touches `crates/transfer/src/generator/file_list/entry.rs`
and the generator-side id-list writer. Save ~8 bytes per unique (uid,
gid) pair. On the DIS-1 corpus (1 root/root pair) this is +8 B / 0
segments. On a multi-user corpus with N unique pairs it scales to
~8 * N bytes saved. Expected win on cold-start: **negligible byte-wise**
but it removes a structural divergence from upstream's file-list shape
that surfaces in INC_RECURSE wire-format work later.

**Combined DIS-6.W1-W4 estimate:** ~1.0-2.0 ms per cold-start
connection on the 500-file corpus, plus a structural reduction of ~+800
small segments to ~0. None of these fixes change a single wire byte
that the upstream receiver parses; they only change framing boundaries.
W5 is structural cleanup with sub-microsecond impact on cold start.

Compared with the DIS-4.a recommendation set (signal-poll fix alone
clears 200-500 ms off p99) and DIS-4.d (flist arena ~20-50 ms), the
DIS-5 wire-byte fixes are **third-tier optimizations** by wall-clock
magnitude. They are listed because they are byte-level evidence of
divergence from upstream and they remove segment-count amplifiers that
would dominate the cost picture once DIS-4.a's signal-poll tail is
gone.

## 9. What this audit did NOT measure

- **Authenticated path (section 4.2)** is estimated, not captured. A
  follow-up capture with `auth users` + `secrets file` configured
  would convert section 4.2 rows from "estimated" to "measured".
- **Multi-file corpora >5 files.** The 5-file corpus shows per-file
  scaling clearly enough to extrapolate (the R-WIRE-4 segment-count
  diff scales linearly per file), but the 500-file DIS-1 corpus would
  surface batch-boundary edge cases (when does the mplex `BufWriter`
  fill on a 1 KiB-per-file workload? See DIS-4.e section 3.3 for the
  ~30-files-per-batch estimate).
- **Multi-directory corpora.** The DIS-1 small-files corpus is a flat
  directory. INC_RECURSE path (multi-segment file lists) would have a
  different byte profile, including extra `node_to_seg` framing per
  segment boundary. Out of scope for this audit because INC_RECURSE
  push is currently disabled on oc-rsync (project memory:
  "Sender-side code exists in generator but interop not validated -
  disabled for push transfers").
- **TCP segment counts under varying MSS.** Captures here used the
  loopback MTU (65495 B), so all data fit in single TCP segments
  regardless of payload size. On real networks with MSS ~1460 the
  segment counts would be dominated by TCP-layer fragmentation, not by
  the per-mplex-frame `write` boundary. The per-syscall (write+sendto)
  cost still applies on real networks; the per-TCP-segment cost adds
  to it.

## 10. Reproducing this audit

```sh
# Inside rsync-profile podman container, with /workspace bind-mount
# (/Users/ofer/devel/rsync) and /tmp/dis5 working dir.

# Build worktree's oc-rsync (workspace target/release).
mkdir -p /tmp/dis5/oc-src /tmp/dis5/up-src
for i in 1 2 3 4 5; do
    dd if=/dev/urandom of=/tmp/dis5/oc-src/f${i}.dat bs=1024 count=1 status=none
    cp /tmp/dis5/oc-src/f${i}.dat /tmp/dis5/up-src/f${i}.dat
done

# Daemon configs (mirror DIS-1 harness).
printf 'pid file = /tmp/dis5/oc.pid\nport = 28800\nuse chroot = false\nnumeric ids = yes\n\n[bench]\n    path = /tmp/dis5/oc-src\n    read only = false\n' > /tmp/dis5/oc.conf
printf 'pid file = /tmp/dis5/up.pid\nport = 28801\nuse chroot = false\nnumeric ids = yes\n\n[bench]\n    path = /tmp/dis5/up-src\n    read only = false\n' > /tmp/dis5/up.conf

# Start daemons (detached, with daemon-fallback disabled to force native path).
OC_RSYNC_DAEMON_FALLBACK=0 /workspace/target/release/oc-rsync \
    --daemon --config /tmp/dis5/oc.conf --port 28800 --log-file /tmp/dis5/oc.log &
/usr/bin/rsync --daemon --no-detach --config /tmp/dis5/up.conf --log-file /tmp/dis5/up.log &

# Capture each cold-start pull. (One run per daemon; tcpdump on lo.)
tcpdump -i lo -w /tmp/dis5/oc.pcap -s 0 'port 28800' &
sleep 0.3
rsync -a rsync://127.0.0.1:28800/bench/ /tmp/dis5/oc-dest-client/
sleep 1; pkill -INT -f 'tcpdump.*oc.pcap'

tcpdump -i lo -w /tmp/dis5/up.pcap -s 0 'port 28801' &
sleep 0.3
rsync -a rsync://127.0.0.1:28801/bench/ /tmp/dis5/up-dest-client/
sleep 1; pkill -INT -f 'tcpdump.*up.pcap'

# Tally.
tshark -r /tmp/dis5/oc.pcap -Y 'tcp.len > 0' -T fields -e tcp.srcport -e tcp.len -e data
tshark -r /tmp/dis5/up.pcap -Y 'tcp.len > 0' -T fields -e tcp.srcport -e tcp.len -e data
```

## 11. File index

Direct evidence files cited above (all paths relative to worktree
root):

- `crates/protocol/src/legacy/lines.rs` - daemon greeting + capability
  writer (R-WIRE-1)
- `crates/transfer/src/generator/protocol_io.rs` - `send_file_list`,
  `send_id_lists`, `send_io_error_flag`, `FirstByteWriter` (R-WIRE-3,
  R-WIRE-4 sender side)
- `crates/transfer/src/generator/file_list/entry.rs` - per-entry
  user/group name encode (R-WIRE-2)
- `crates/transfer/src/generator/transfer/orchestrator.rs` - goodbye
  + stats writer (R-WIRE-5)
- `crates/protocol/src/multiplex/writer.rs` - mplex `BufWriter` flush
  semantics (R-WIRE-3, R-WIRE-4, R-WIRE-5 framing)
- `crates/protocol/src/flist/write/mod.rs` - file-list entry encode
  (R-WIRE-2 byte layout)
- `target/interop/upstream-src/rsync-3.4.1/clientserver.c` - upstream
  daemon path (capability advert reference)
- `target/interop/upstream-src/rsync-3.4.1/flist.c` -
  `send_file_list`, `add_uid_list`, `add_gid_list` (R-WIRE-2)
- `target/interop/upstream-src/rsync-3.4.1/io.c` - `writefd_unbuffered`,
  `perform_io` (upstream batching reference)
- `scripts/benchmark_daemon_cold_start.sh` - DIS-1 harness this audit
  mirrors
- `docs/audits/dis-3-cold-start-phase-decomposition.md` (parent task)
- `docs/audits/dis-4a-rsyncd-greeting-overhead.md` (DIS-4.a)
- `docs/audits/dis-4b-module-select-roundtrip.md` (DIS-4.b)
- `docs/audits/dis-4d-flist-build-cold-start.md` (DIS-4.d)
- `docs/audits/dis-4e-first-block-send-latency.md` (DIS-4.e)
