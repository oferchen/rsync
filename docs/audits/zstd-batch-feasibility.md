# zstd as a batch-compatible compression alternative

Tracking issue: oc-rsync task #1685.

## Summary

Can the existing `--write-batch` / `--read-batch` path be taught to record a
zstd-compressed token stream, the way upstream rsync's batch path records
zlib-compressed tokens? Conclusion: **feasible-with-caveats**. zstd's
per-frame, self-delimited framing makes batch capture possible without the
streaming-context replay burden that defeats zlib, but interop with
upstream is bounded - upstream's batch header has no algorithm field, and
upstream `compat.c` hard-codes `CPRES_ZLIB` for batch reads. A zstd batch
written by oc-rsync is therefore an oc-rsync-only artefact unless we ship
a magic-byte probe (already scaffolded in `crates/batch/src/replay.rs`).

## What lives where

The encoder is `compress::zstd::CountingZstdEncoder` in
`crates/compress/src/zstd.rs:18-47`, wrapping `zstd::stream::write::Encoder`
from the `zstd` crate. The hot operation for batch compatibility is
`flush()` (`crates/compress/src/zstd.rs:91-93`), which invokes the
underlying encoder's `flush()`. The `zstd` crate maps `Write::flush` onto
`ZSTD_CCtx_flushStream` (`ZSTD_e_flush` end-directive) and emits
self-delimited blocks. Per-token flush parity with upstream
`token.c:send_zstd_token()` is documented inline at
`crates/compress/src/zstd.rs:78-90` and was wired up by PR #1115 / #3047.

The batch tee point is **not** in `crates/batch/src/`, despite the task
hint (the only matches there are test-fn names: `test_batch_writer_create`,
`test_batch_writer_write_header`, etc. in `crates/batch/src/writer.rs:216-296`).
The actual recorder is `MultiplexReader::batch_recorder` in
`crates/transfer/src/reader/multiplex.rs:46`, set via
`ServerReader::set_batch_recorder` in
`crates/transfer/src/reader/server.rs:56-71`. The recorder is attached to
the `MultiplexReader` even when compression is active
(`crates/transfer/src/reader/server.rs:111-135`), so it captures
post-demux **pre-decompression** wire bytes - the same level as upstream's
`io.c:read_buf()` `write_batch_monitor_in` call. The user-facing call
that materialises the batch artefact is `engine::batch::BatchWriter`
(`crates/batch/src/writer.rs:14-95`), driven from
`crates/core/src/client/run/batch.rs:54-130`.

The stale comment block at `crates/core/src/client/run/batch.rs:97-107`
claiming oc-rsync captures "post-decompression" data describes a previous
implementation and contradicts the actual tee site; that line should be
fixed in a follow-up. What is correct today: oc-rsync forces
`do_compression: false` in the batch header
(`crates/core/src/client/run/batch.rs:107`,
`crates/batch/src/format/flags.rs:34-41`), which means the batch body must
be uncompressed regardless of negotiated compression on the live wire.

## Why zlib loses but zstd does not

zlib token compression as upstream uses it is a single inflate context
with `Z_SYNC_FLUSH` boundaries between tokens. Across token boundaries the
inflate dictionary keeps memory of recently emitted bytes, and matched
basis-block bytes are fed back in via upstream `token.c:see_deflate_token()`
so the next literal can resolve back-references that crossed the boundary.
That is why batch replay must walk the token stream in lockstep with a
parallel `see_token()` feed - encoded today as `cpres_zlib` plus the
`see_token()` call at `crates/batch/src/replay.rs:475-479` and
`replay.rs:807-824`. A naive tee that captures only the wire bytes,
without also serialising basis-block content into the dictionary at
replay time, dies with `inflate failed: invalid distance too far back`.

zstd's wire format is structurally different: each call to `flush` /
`ZSTD_e_flush` ends the current block group at a self-delimited boundary
with a magic-bearing frame header. Decompression of a frame does not
require state from the producer beyond what the frame's own header
declares (window size, dictionary id if any). The matching
`see_token` for zstd is a documented noop -
`crates/batch/src/replay.rs:475-479`, "Zstd does not need dictionary
sync". As long as the encoder emits at least one frame boundary per batch
token, replay can decode each token independently. The existing zstd
encoder hits a flush per token (`compress/src/zstd.rs:78-93`) so the
per-token property already holds for live transfers - the same wire bytes
are what we would tee.

The corollary: the dictionary-feed plumbing that exists today for zlib
(`replay.rs:807-824`) is exactly the burden that does not need to ship
for zstd batches.

## Wire-level fit

`BatchFlags::do_compression` is a single bit, gated on protocol >= 29
(`crates/batch/src/format/flags.rs:34-41`, `flags.rs:69-72`). It says
"the body is compressed", not "the body is zlib". Upstream
`compat.c:194-195` resolves the algorithm to `CPRES_ZLIB`
unconditionally for batch reads, and `compat.c:413-414` forces
`compress_choice = "zlib"` for batch writes. Nothing in the file format
distinguishes an oc-rsync zstd batch from an upstream zlib batch on the
wire other than the body itself.

oc-rsync's replay path already plans for this: the zstd auto-detection
probe at `crates/batch/src/replay.rs:985-1017` peeks for the zstd magic
number `0xFD2FB528` (LE: `28 B5 2F FD`) inside the first
`DEFLATED_DATA` block and switches `detected_codec` to `Zstd` when found,
flipping `cpres_zlib = false` so the no-op `see_token()` path is used.
That probe has been sitting in the tree as anticipatory scaffolding for
"hypothetical or patched upstream zstd batches"
(`crates/batch/src/replay.rs:48-62`).

So an oc-rsync-produced zstd batch with `do_compression: true` is
readable by oc-rsync today via the existing probe, and unreadable by
upstream rsync (which will hand it to zlib's `inflate()` and bail).

## Trade-offs

- **Cross-tool interop is one-way.** oc-rsync can already read upstream
  zlib batches (via the see_token feed at `replay.rs:807-824`) and would
  read its own zstd batches (via the magic-byte probe). Stock upstream
  rsync cannot read an oc-rsync zstd batch and there is no on-the-wire
  way to tell it to try - the batch header has no algorithm bit.
- **Protocol 28 is excluded.** `do_compression` does not exist below
  protocol 29. Any zstd batch produced for a 28-only peer would be
  unreadable by upstream regardless of algorithm choice.
- **Header extension is off the table.** A new bit or algorithm byte in
  `BatchFlags` would be a wire-format extension. The project's "no wire
  protocol features for niche perf" rule (see
  `feedback_no_wire_protocol_features.md`) rejects this. The magic-byte
  probe is the only durable detection mechanism.
- **Test surface grows.** Add a self-round-trip cell, plus an interop
  KNOWN_FAILURE entry asserting that upstream rsync cannot read an
  oc-rsync zstd batch. The existing zstd-vs-zlib token tests in
  `crates/compress` stay valid; new coverage lives in
  `crates/batch/tests/`.
- **Comment debt to clear up.** The "oc-rsync captures post-decompression"
  comment at `crates/core/src/client/run/batch.rs:97-107` no longer
  matches the implementation (the recorder lives on `MultiplexReader`).
  Fix that as part of the same PR that flips `do_compression` to true on
  the zstd path.

## Recommendation

**Feasible-with-caveats.** Allow zstd batches when both `--write-batch`
and a zstd-selected compression codec are set, gated for self-round-trip
only, and never claim cross-tool interop with upstream.

Concrete next-step tasks:

1. **Code: opt-in zstd batch capture.** In
   `crates/core/src/client/run/batch.rs:88-114`, set `do_compression =
   negotiated_codec == Zstd` (currently hard-coded `false`). Stop forcing
   `false`; rely on the live wire codec already carried by the
   `CompressedReader`. Keep `do_compression: false` when no compression
   was negotiated. Update the surrounding comment to describe the
   tee-at-MultiplexReader behaviour accurately.
2. **Code: confirm tee bytes are zstd frames.** Audit the
   `MultiplexReader::batch_recorder` write path
   (`crates/transfer/src/reader/multiplex.rs:278-316`) to verify that the
   bytes copied to the recorder are post-demux but pre-`CompressedReader`,
   i.e., raw zstd frame bytes, when zstd compression is active. The
   plumbing in `crates/transfer/src/reader/server.rs:111-135` says yes;
   add an assertion to lock it in.
3. **Tests: self round-trip.** Add a `crates/batch/tests/` integration
   test covering write-batch + zstd + read-batch on a fixture transfer.
   Validate that the existing magic-byte probe at
   `crates/batch/src/replay.rs:1004-1017` switches `detected_codec` to
   `Zstd` and that `cpres_zlib` is false for the run.
4. **Tests: interop KNOWN_FAILURE.** Add an explicit entry to
   `tools/ci/upstream_testsuite_known_failures.conf` documenting that
   upstream rsync cannot read an oc-rsync-produced zstd batch. Keep the
   existing zlib batch interop cells passing.
5. **Docs: surface the asymmetry.** Update the CLI help in
   `crates/cli/src/frontend/command_builder/sections/transfer_behavior_options.rs`
   to note that zstd batches are oc-rsync-to-oc-rsync only, while zlib
   batches stay cross-tool readable. Default `--write-batch` to zlib when
   `--compress` is set without a codec selector, to preserve current
   upstream-compatible behaviour.

If even step 1 is judged too high-cost relative to the niche benefit, the
fallback is to leave `do_compression = false` and continue capturing
post-decompression data only, sacrificing some `--write-batch + --compress`
disk-size wins for a smaller surface.

## References

- `crates/compress/src/zstd.rs:15-93` - `ZstdEncoder` import,
  `CountingZstdEncoder`, per-token `flush()`.
- `crates/transfer/src/reader/multiplex.rs:18-47`,
  `multiplex.rs:278-316` - `batch_recorder` field and tee writes.
- `crates/transfer/src/reader/server.rs:44-135` -
  `set_batch_recorder` / `activate_compression` keep the recorder on
  `MultiplexReader`, capturing pre-decompression bytes.
- `crates/core/src/client/run/batch.rs:54-130` - `BatchWriter`
  construction, `do_compression: false` (with stale comment to fix).
- `crates/batch/src/writer.rs:14-95` - `BatchWriter` struct, header,
  raw data write path.
- `crates/batch/src/format/flags.rs:34-41`, `flags.rs:69-72`,
  `flags.rs:112-119` - bit 8 `do_compression` definition / coding.
- `crates/batch/src/replay.rs:45-62` - `CompressionCodec` enum and
  upstream cite.
- `crates/batch/src/replay.rs:461-479` - `do_compression` decoder gating
  and `cpres_zlib` flag.
- `crates/batch/src/replay.rs:807-824` - per-token `see_token()` feed
  for zlib (the burden zstd avoids).
- `crates/batch/src/replay.rs:968-1017` - `create_compressed_decoder`,
  `detect_compression_codec`, magic-byte probe.
- Upstream: `batch.c:check_batch_flags()`, `compat.c:194-195`,
  `compat.c:413-414`, `token.c:send_deflated_token()`,
  `token.c:send_zstd_token()`, `io.c:read_buf()`.
