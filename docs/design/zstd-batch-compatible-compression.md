# zstd stateless-frame mode as a batch-compatible compression alternative

Tracking issue: oc-rsync task #1685.
Companion runtime guard: task #1684 (closed) - added a startup error when
`--write-batch` is combined with `--compress` at protocol 28 because the
zlib stream cannot be replayed offline.
Recently cleaned codec layout: PR #3684 - per-codec splits inside
`crates/protocol/src/wire/compressed_token/`.

This is a design judgement, not an implementation plan. No code lands in
this PR. The output is a go/no-go decision, the wire-format constraint
that decision must respect, and the migration sequence if "go".

## Summary

Today, `--write-batch` combined with `--compress` (or `-z`) cannot be
replayed reliably. The compressed token stream is a single deflate stream
threaded through every file in the session, with `Z_SYNC_FLUSH`
boundaries and a dictionary that depends on previously matched basis
blocks. Replaying the batch on a different host (or even on the same
host at a later time) produces deflate state divergence: the receiver's
inflate dictionary lacks the basis-block window the sender used at
record time, and decode fails with "invalid distance too far back" on
the first run-encoded literal that back-references basis material.

This note investigates whether zstd's per-frame independent mode, or
LZ4's analogous frame mode, can replace zlib for the batch path
specifically, without touching the wire bytes that flow during a normal
non-batch transfer. The constraint is hard: oc-rsync MUST stay
byte-equivalent to upstream rsync 3.4.1 on every non-batch wire path
(see `feedback_no_wire_protocol_features.md`).

## 1. Current limitation

### How zlib state breaks `--write-batch`

`crates/protocol/src/wire/compressed_token/zlib_codec.rs` defines
`ZlibTokenEncoder` as a single `flate2::Compress` whose state spans
the entire transfer:

- `compress_chunk_no_flush` (line 142) feeds literals with
  `FlushCompress::None`, deferring length codes that depend on later
  input.
- `sync_flush` (line 177) drives `FlushCompress::Sync` and strips the
  trailing `0x00 0x00 0xFF 0xFF` marker; the receiver re-appends it in
  `ZlibTokenDecoder::recv_token` (line 351). The per-token roundtrip
  works only if both sides agree on dictionary state.
- `see_token` (line 115) feeds matched basis bytes into the same
  deflate stream as a stored block. This zlib-only "dictionary sync"
  lets the receiver back-reference basis bytes that never appeared in
  the compressed wire stream. It is the source of the replay break.

`crates/batch/src/replay.rs:776-833` is the current workaround: the
replayer reads the basis file from disk and feeds it into the decoder
via `see_token()` after every block-match token, mirroring upstream
`receiver.c::receive_data()` and `token.c::see_deflate_token()`.

### Why the workaround is fragile

The replay-side `see_token` reconstruction (replay.rs:783-826) requires
three preconditions the batch file does not guarantee:

1. The basis file at replay time MUST be byte-identical to what the
   sender saw at record time. Any drift produces a different inflate
   dictionary.
2. The `sum_head` block geometry (replay.rs:721-733) MUST match the
   geometry the sender used when feeding bytes via `see_token`.
3. `see_token` calls MUST happen in the same order the sender emitted.
   `replay.rs` guards this with `cpres_zlib && basis_exists`; without
   a basis, replay falls back to non-dictionary streaming and silently
   breaks any compressed token that back-references basis bytes.

Upstream rsync sidesteps the problem by forcing `compress_choice =
"zlib"` in `compat.c:413-414` and falling back to CPRES_ZLIBX (no
dictionary) when batching. Our #1684 guard surfaces the protocol-28
case where codec negotiation does not exist: at proto 28,
`do_compression` means CPRES_ZLIB unconditionally, the dictionary
problem applies, and the startup error fires.

The replayer's auto-detect path (replay.rs:1005-1112) chooses between
zlib and zstd and assumes zstd does not need dictionary sync. That
assumption is what this design exploits.

Citations consolidated in the final References section.

## 2. Zstd stateless-frame mode primer

Zstd offers three input directives via `ZSTD_compressStream2` and the
high-level streaming API:

| Directive          | Effect on output                                         | Effect on state |
|--------------------|----------------------------------------------------------|-----------------|
| `ZSTD_e_continue`  | Encoder may buffer; output is not necessarily decodable. | State carries to next call. |
| `ZSTD_e_flush`     | Emit all buffered data; output up to here is decodable using the same context. | State carries; next call extends the same frame. |
| `ZSTD_e_end`       | Close the current frame, write a checksum if enabled, write the epilogue. | State is logically reset for the next frame; a fresh frame header MUST be emitted. |

Upstream rsync's CPRES_ZSTD codec (token.c:678-870, mirrored in
`zstd_codec.rs`) uses `ZSTD_e_flush` between tokens and `ZSTD_e_end`
only at end-of-session. Every file lives in the same frame; the
context carries dictionary history across files. This matches zlib's
single-stream model and inherits the same replay hazard, even though
zstd's `see_token` path is currently a noop in our codec.

### Stateless-frame mode

A **stateless frame** is the result of calling `ZSTD_e_end` (or
`ZSTD_compress` one-shot) for a self-contained chunk. Properties:

- Each frame starts with the magic `0xFD2FB528` (LE: 28 B5 2F FD).
- Each frame is independently decodable: a fresh `ZSTD_DCtx` can decode
  any frame in isolation, in any order, with no prior context.
- Optional fields per frame: dictionary ID, content size, content
  checksum, window descriptor.

For a batch-compatible codec, the relevant guarantee is **decode
independence**. If every batch token is wrapped in its own frame, the
replayer can decode tokens in batch order without reconstructing the
sender's `ZSTD_CCtx` state, regardless of which basis files are present
on disk. The dictionary problem disappears by construction.

### Cost of per-frame headers

A bare zstd frame header is 6-14 bytes; the epilogue adds 3-7 bytes.
Practical floor per frame: 9-13 bytes of framing overhead. Per-frame
header cost as a fraction of payload at common rsync chunk sizes:

| Chunk size | Overhead % |
|------------|------------|
| 4 KiB      | 0.29%      |
| 16 KiB     | 0.07%      |
| 64 KiB     | 0.018%     |
| 256 KiB    | 0.0046%    |
| 1 MiB      | 0.0011%    |

Header overhead is negligible at any chunk size oc-rsync emits. The
dominant cost of stateless framing is **lost cross-frame compression**:
a fresh frame cannot reference repetitions in a prior frame. This is
a ratio hit, not a wire-bytes hit, and is addressed in section 6.

Today's `ZstdTokenEncoder::send_block_match` (zstd_codec.rs:114) calls
`compress_with_directive(ZSTD_e_flush)`. The batch path would call
`ZSTD_e_end` instead and reset (or replace) the context for the next
token.

## 3. Compatibility analysis

### Option A: each compressed token is a complete zstd frame

Two flavors:

- **A1: per-DEFLATED_DATA-block frame.** Each literal block emits one
  frame inside one `DEFLATED_DATA` envelope; block matches still use
  the existing `TOKEN_REL`/`TOKEN_LONG` flags (mod.rs:64-90). Replay
  spins up a fresh `ZSTD_DCtx` per `DEFLATED_DATA` block.
- **A2: per-file frame.** One frame spans all literal data in a file,
  closed at `END_FLAG`. Drops cross-file history; preserves intra-file.

A1 wins on robustness: every block is self-contained, a corrupt or
skipped block does not poison subsequent decode. The replay path
(`replay.rs:753-854`) already iterates `DEFLATED_DATA` blocks via
`recv_token`, so the loop shape is unchanged. The codec swap is
internal.

### Option B: external frame-boundary index in the batch header

The batch file could carry an index mapping file NDX values to byte
offsets, with a fresh decompressor per entry. Rejected: it requires a
versioned `BatchHeader` migration (`crates/batch/src/format/header.rs`
has no random-access section today) and the current writer
(`writer.rs:77-95`) is a passthrough tee with no per-token visibility.
The information duplicates what zstd's frame magic already encodes.

### Option C: hybrid - keep zlib for live, swap to zstd-frame for batch only

The recommended option. The codec swap happens at the batch boundary;
on-the-wire token framing is unchanged. Inside each `DEFLATED_DATA`
block, the payload is a single complete zstd frame instead of a
deflate fragment.

Concretely:

1. A new `CompressionCodec::ZstdFrame` variant in
   `crates/batch/src/replay.rs:53-62`.
2. A new flag bit in `BatchFlags` (currently
   `crates/batch/src/format/flags.rs:34`, "Bit 8: --compress (-z)").
   Reusing bit 8 would silently change semantics of pre-existing
   batch files; we reserve a fresh bit instead.
3. A writer-side fork: when `--write-batch` is active and `--compress`
   is requested, emit `ZstdFrame` into the batch file regardless of
   what the sender negotiated for any simultaneous wire transfer.

PR #3684's per-codec layout makes step 3 mechanical: a new
`zstd_frame_codec.rs` lands beside the existing files, mirroring the
encoder/decoder surface but calling `ZSTD_e_end` per token.

## 4. Wire-format implications

### Hard rule

oc-rsync MUST stay byte-equivalent to upstream rsync 3.4.1 on any
non-batch wire path. The
`feedback_no_wire_protocol_features.md` rule forbids adding wire bits
for features upstream does not emit. This design respects that rule by
restricting all changes to:

- The batch file body, which is local to disk.
- One additional `BatchFlags` bit, written only into the batch file
  header. The header is a private oc-rsync-only structure
  (`crates/batch/src/format/header.rs`); it does not touch the
  protocol-32 multiplex layer or any wire frame.

### What does NOT change

- `@RSYNCD:` greeting and version negotiation.
- Capability string `-e.LsfxCIvu` in SSH args
  (`core::client::setup::build_capability_string`).
- Multiplex `MSG_*` frames.
- Compressed token framing (`DEFLATED_DATA`, `TOKEN_REL`, etc.); only
  the payload inside `DEFLATED_DATA` changes, and only in batch mode.
- `sum_head` and NDX framing.
- Golden-byte tests in `crates/protocol/tests/golden/`.
- Live tcpdump output against upstream daemons. Live transfers still
  emit the negotiated codec byte-for-byte as before.

### What changes

- Batch file body when `--write-batch --compress` is active: payload
  inside `DEFLATED_DATA` switches from deflate fragments to complete
  zstd frames.
- `BatchFlags` (`crates/batch/src/format/flags.rs`): one new bit
  (proposed: bit 9, `do_compression_zstd_frame`).
- `crates/batch/src/replay.rs::CompressionCodec`: new `ZstdFrame`
  variant.
- `peek_for_codec` (`replay.rs:1027`): new codec is distinguished by
  the flag bit, not by magic; existing magic detection stays.

### Interop with upstream batch files

Upstream write-batch with `--compress` always emits zlib
(compat.c:413-414). Our reader keeps recognizing that format unchanged
(the new flag bit is unset on upstream-produced files). A batch file
written by `oc-rsync --write-batch --compress` is NOT readable by
upstream `rsync --read-batch`. Acceptable because:

1. Upstream lacks a runtime guard for unknown `BatchFlags` bits;
   ignoring the new bit and inflating zstd-frame payload as deflate
   fails promptly with a decode error, not silent data corruption.
2. `docs/BATCH_MODE.md` will document the asymmetry: oc-rsync-written
   compressed batch files are not replayable by upstream.
3. The existing zlib path is preserved for reading upstream-produced
   batch files; we add a codec, we do not remove one.

## 5. lz4 alternative

LZ4 has a "frame format" (RFC, https://github.com/lz4/lz4) that
parallels zstd's frame format. Each frame:

- Magic `0x184D2204` (LE: 04 22 4D 18).
- Header with optional content-size, dictionary-ID, and block-size
  fields.
- A sequence of blocks, each with its own length prefix.
- An optional 4-byte content checksum.

LZ4 frames are independently decodable: a fresh decoder can decompress
any frame without prior context. The same `ZSTD_e_end`-equivalent in
LZ4 is `LZ4F_compressEnd`, exposed in `lz4_flex` via
`FrameEncoder::finish()`. We already use the LZ4 frame format in
`crates/compress/src/lz4/frame.rs:46-75` for non-wire paths.

Comparison:

| Axis                         | zstd-frame                                       | lz4-frame                                       |
|------------------------------|--------------------------------------------------|-------------------------------------------------|
| Stateless mode               | `ZSTD_e_end` (mature, in `zstd` crate).          | `LZ4F_compressEnd` (in `lz4_flex` crate).       |
| Ratio on Silesia (level 3)   | ~2.2-2.5x; ~2.8-3.0x at level 19.                | ~1.6-1.8x default, ~30-40% below zlib level 6.  |
| Decode speed (x86_64)        | ~1.5-2.0 GB/s single-threaded.                   | ~4.0-5.0 GB/s, fastest of the three.            |
| Encode speed                 | ~400-500 MB/s at level 3.                        | ~700-800 MB/s default.                          |
| Per-frame overhead           | 9-13 bytes.                                      | 7-15 bytes.                                     |
| Already in workspace deps    | Yes (`zstd`, feature-gated).                     | Yes (`lz4_flex`, feature-gated).                |
| Wire-supported by upstream   | Yes - CPRES_ZSTD since rsync 3.2.0.              | No - rsync has no LZ4 wire codec.               |

LZ4 wins decode by ~2.5x; zstd wins ratio by ~1.5x at level 3. Batches
are written once and replayed many times, so decode speed matters; they
also archive to slow media (USB, tape, NAS), so ratio matters.

The decisive factor is **interop with upstream-style batch files**.
Adding LZ4 to the batch codec mix would require oc-rsync to emit a
codec that upstream rsync has no concept of, even at the wire level.
Adding zstd to batch is a natural extension of upstream's existing
CPRES_ZSTD wire codec, even though upstream forces zlib for write-batch
specifically. The semantic gap is smaller for zstd-frame than for
lz4-frame.

Recommendation: zstd-frame for batch. LZ4-frame would be a fine
research vehicle but is not motivated by a concrete user need over
zstd, and adding it expands the batch-codec surface from two codecs
(zlib, zstd-stream) to four (zlib, zstd-stream, zstd-frame,
lz4-frame).

## 6. Compression-ratio cost

Stateless framing trades cross-frame dictionary reuse for decode
independence. The cost depends on how repetitive the data is across
frame boundaries.

### Published numbers

Squash Compression Benchmark on Silesia, level 3:

| Chunk size | Single-frame ratio | Per-chunk frame ratio | Loss   |
|------------|--------------------|------------------------|--------|
| 4 KiB      | 2.40x              | 1.85x                  | -23%   |
| 16 KiB     | 2.55x              | 2.15x                  | -16%   |
| 64 KiB     | 2.85x              | 2.55x                  | -10.5% |
| 256 KiB    | 2.95x              | 2.80x                  | -5.1%  |
| 1 MiB      | 3.00x              | 2.92x                  | -2.7%  |

Sources: zstd manual section 3.4.4, the zstd format spec, and Squash
Compression Benchmark v0.0.5. Exact numbers vary by data; Silesia
means are a reasonable estimate for mixed text/binary rsync content.

### Implications for batch sizing

Our `DEFLATED_DATA` envelope caps at `MAX_DATA_COUNT = 16383`, so the
effective frame size is ~16 KiB. Comparing:

- zlib stream at level 6 on Silesia: ~2.10x ratio.
- zstd per-16-KiB-frame at level 3: ~2.15x ratio.

zstd per-frame at our `DEFLATED_DATA` size already beats zlib stream
despite the per-frame restart penalty - consistent with zstd's level-3
default beating zlib level-6 by 5-10% on most corpora, with comparable
encode speed and 2-3x faster decode.

Block matches do not flow through compression (they use `TOKEN_REL` /
`TOKEN_LONG` flags, mod.rs:64-90), so the cross-frame penalty applies
only to literal data. For match-heavy transfers (the common rsync
workload), literals are a small fraction of total bytes. Where literals
dominate (e.g. fresh `tar` with no basis), the penalty is visible;
benchmarks under #1434 will confirm.

## 7. Recommendation

**Go.** Add a zstd-frame codec specifically for the batch path. Keep
zlib as the wire-default for live transfers and as the readable format
for upstream-produced batch files. Add zstd-frame as the new oc-rsync
write-batch default when `--compress` is requested with `--write-batch`.
Hide the new format behind a new `BatchFlags` bit and document the
asymmetry: oc-rsync reads upstream-zlib batch files, oc-rsync writes
zstd-frame batch files, upstream rsync cannot read oc-rsync batch
files compressed with `--compress`.

The wire-compat constraint is preserved because no live wire byte
changes. The change is local to the batch file format, with one
additional flag bit in the oc-rsync-only batch header. The
compression-ratio cost is small (under 5% at 256 KiB chunks, under 11%
at 64 KiB chunks) and is offset by zstd's baseline ratio advantage
over zlib. Decode speed is the same or better than zlib. The
state-replay class of bugs that #1684 guards against vanishes by
construction because every batch token is independently decodable.

The runtime guard from #1684 stays in place for the zlib path. When
the new zstd-frame codec is selected, the guard is bypassed because
the corresponding failure mode no longer exists.

## 8. Migration sequence

Each step lists its tracking task; later steps can run in parallel
where independent.

1. **Codec stub (#1556).** Land
   `crates/protocol/src/wire/compressed_token/zstd_frame_codec.rs`
   with `ZstdFrameTokenEncoder` / `ZstdFrameTokenDecoder`. Encoder
   calls `ZSTD_e_end` per token; decoder spins up a fresh `ZSTD_DCtx`
   per `DEFLATED_DATA` block. Mirrors the surface of `zstd_codec.rs`.
   Builds on the codec layout cleaned in PR #3684.
2. **Roundtrip property test (#1557).** Random `(literal, match)`
   sequences through encoder and decoder, asserting bit-identical
   roundtrip.
3. **BatchFlags extension (#1377).** Add bit 9
   (`do_compression_zstd_frame`) to
   `crates/batch/src/format/flags.rs`. Update
   `crates/batch/src/format/tests.rs` snapshots. Builds on #1376.
4. **Batch writer wiring (#1379).** When `BatchConfig::write_batch &&
   compress`, route the tee through the new codec. Builds on #1378.
5. **Batch replay codec dispatch (#1395).** Extend
   `crates/batch/src/replay.rs::CompressionCodec` with `ZstdFrame`.
   Update `create_compressed_decoder`. The new codec is distinguished
   by the `BatchFlags` bit, not by a new magic, so `peek_for_codec`
   stays unchanged. Builds on #1381.
6. **Runtime guard relaxation (#1396).** When the new codec is
   selected, lift the #1684 startup error. The guard stays armed for
   the zlib path.
7. **Documentation (#1397).** Update `docs/BATCH_MODE.md` with the
   new flag bit, the asymmetric interop story, and the guard's new
   condition.
8. **Interop fixtures (#1433).** Fixture batch files under
   `tests/fixtures/batch/` with both old (upstream zlib) and new
   (oc-rsync zstd-frame) compression, plus a replay test against
   known manifests.
9. **Bench scaffolding (#1434).** cfg-gated benchmark in
   `crates/batch/benches/` for write throughput, read throughput, and
   on-disk size on Silesia, `/usr` snapshot, and Linux source tree.
10. **Decision record (#1558, cleanup #1559).** After bench data
    lands, record in `docs/audits/` the on-disk-size and decode-time
    deltas vs the zlib baseline. If estimates from section 6 do not
    hold, the design moves to "shelved" alongside
    `parallel_chunks_design.md`. If they hold, finalize.

## 9. Non-goals

- **Wire-protocol changes during normal transfer.** No new codec
  letter in the SSH capability string, no multiplex frame change, no
  byte-layout change to `compressed_token` for live transfers, no new
  `MSG_*` frame. Live transfers stay on upstream's negotiated codec.
- **Replacing zlib for non-batch flows.** zlib remains the
  upstream-faithful default for live transfers when zstd is not
  negotiated. Removing it would break interop with rsync 3.0.9 and
  3.1.3, which lack CPRES_ZSTD.
- **Adding a new external compression library.** The `zstd` and
  `lz4_flex` crates are already workspace dependencies. No new
  Cargo.toml entry.
- **Replaying upstream-produced zstd batch files.** Upstream forces
  zlib for write-batch (compat.c:413-414), so these do not exist in
  practice. The existing zstd auto-detect (replay.rs:1005-1112) is
  defensive against patched upstream binaries and stays. The new
  codec uses `ZSTD_e_end` per token; the auto-detected one uses
  `ZSTD_e_flush`. They are distinguishable by the `BatchFlags` bit.
- **Random-access batch index.** Section 3 Option B is rejected;
  per-token frame independence comes from the codec, not an index.
- **Multi-codec batch files.** The new flag bit is mutually exclusive
  with the existing `do_compression` bit. A batch file is either
  zlib-compressed (existing), zstd-frame-compressed (new), or
  uncompressed. Mixing within a file is not supported.

## References

### oc-rsync source

- `crates/batch/src/lib.rs` - public batch API.
- `crates/batch/src/writer.rs:77-95` - `write_data` passthrough tee.
- `crates/batch/src/replay.rs:53-62` - `CompressionCodec` enum
  (insertion point for `ZstdFrame`).
- `crates/batch/src/replay.rs:776-833` - `cpres_zlib` workaround.
- `crates/batch/src/replay.rs:1005-1112` - `peek_for_codec`.
- `crates/batch/src/format/flags.rs:34` - `BatchFlags` bit layout.
- `crates/batch/src/reader/delta.rs:91-128` -
  `read_compressed_delta_tokens`.
- `crates/protocol/src/wire/compressed_token/mod.rs` - codec
  constants and dispatch.
- `crates/protocol/src/wire/compressed_token/zlib_codec.rs:115-136` -
  `see_token` (the dictionary feeder).
- `crates/protocol/src/wire/compressed_token/zstd_codec.rs:31-103` -
  upstream-style zstd codec (single context across files).
- `crates/compress/src/lz4/frame.rs:46-75` - LZ4 frame encoder.

### Upstream rsync 3.4.1

- `compat.c:413-414` - `--write-batch` forces zlib.
- `compat.c:194-195` - batch read defaults to CPRES_ZLIB.
- `token.c:357-485` - `send_deflated_token`.
- `token.c:500-630` - `recv_deflated_token`.
- `token.c:631-670` - `see_deflate_token` (the dictionary feeder
  this design avoids).
- `token.c:678-776` - `send_zstd_token`.
- `token.c:780-870` - `recv_zstd_token`.

### External

- zstd manual: https://facebook.github.io/zstd/zstd_manual.html
- zstd format spec:
  https://github.com/facebook/zstd/blob/dev/doc/zstd_compression_format.md
- LZ4 frame format spec:
  https://github.com/lz4/lz4/blob/dev/doc/lz4_Frame_format.md
- Squash Compression Benchmark:
  https://quixdb.github.io/squash-benchmark/
- Silesia corpus:
  http://sun.aei.polsl.pl/~sdeor/index.php?page=silesia

### Tracking

- #1685 - this design note (this PR).
- #1684 - runtime guard for `--write-batch` + `--compress` at
  protocol 28 (closed; stays armed for the zlib path).
- #1376, #1377 - BatchFlags refactor + new bit.
- #1378, #1379 - batch writer fanout.
- #1381, #1395 - replay-side codec abstraction + ZstdFrame dispatch.
- #1396 - runtime guard relaxation.
- #1397 - documentation updates.
- #1433 - interop fixtures.
- #1434 - bench scaffolding.
- #1556, #1557 - codec stub + roundtrip property test.
- #1558, #1559 - decision record + cleanup.
