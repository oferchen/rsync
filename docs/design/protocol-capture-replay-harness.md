# Protocol capture/replay harness for the test-support crate

Design note for a wire-protocol capture/replay test harness that lives
under `crates/test-support/src/protocol_capture.rs`. Sister to the
wire-format differential fuzzer in `docs/design/wire-format-differential-fuzzer.md`
and the behavioural protocol fuzzer in `docs/design/protocol-fuzzing-harness.md`.
This is design only - no Rust code lands with this PR. Implementation
lands in follow-up PRs as described in section 11.

## 1. Motivation

Wire-format regressions are expensive to debug from logs alone. A
typical incident: a transfer to upstream rsync 3.4.1 fails with
"protocol data stream corruption", the daemon logs are empty, the
receiver-side stack trace points at `recv_msg`, and the only artifact
is a multi-megabyte mixed stdout/stderr capture that has already
passed through the multiplex demultiplexer. The bytes that triggered
the failure are gone. Reproducing requires re-running the original
transfer (often impossible because the source tree has moved on) or
hand-crafting a reproducer from upstream source code.

A capture/replay harness changes the failure mode. The harness
records the raw multiplex frames in both directions during a
transfer, with their tags and monotonic timestamps, and stores them
in a self-describing file. Replay drives the same bytes through
either side of the protocol stack as if a real peer had sent them.
The same capture seeds the wire-format differential fuzzer
(`docs/design/wire-format-differential-fuzzer.md`) and feeds the
behavioural fuzzer's regression suite
(`docs/design/protocol-fuzzing-harness.md`).

Motivating observations:

- Wire bugs are non-local: a single corrupt varint in a delta token
  derails the next ten frames, producing stderr that points at the
  symptom, not the cause. Frame-level capture preserves the cause.
- Production traffic is often the only place a bug reproduces. A
  capture taken once, replayed as often as needed, drops the
  reproduction cost from "rebuild a 100k-file tree" to "load a 200
  KB binary file".
- Differential fuzzing needs known-good seeds. The wire-fuzzer corpus
  in `tests/wire-corpus/` is hand-curated today; capture/replay
  populates it automatically from any successful transfer.
- Interop debugging boils down to "what did upstream send here?". A
  capture from a working upstream-to-upstream session is the answer,
  replayed against oc-rsync's decoder for byte-by-byte comparison.

The harness is test-only infrastructure. Captures are produced
behind a feature flag in `test-support`; release builds carry
neither the recorder nor the on-disk reader.

## 2. Scope

The harness records and replays:

- **Multiplex frames**: every `MessageFrame` flowing through
  `crates/protocol/src/multiplex/io/recv.rs::recv_msg` and
  `multiplex/io/send.rs::send_msg`, plus the vectored variants
  (`send_msgs_vectored`) and keepalive frames (`send_keepalive`).
- **Pre-multiplex envelope bytes**: the 4-byte header
  (`MessageHeader::encode`) and payload bytes as they appear on the
  socket, before demultiplexing.
- **Greeting and handshake bytes**: the legacy `@RSYNCD:` greeting
  and the binary protocol-30+ handshake. These precede multiplex
  activation and are captured as raw bytes with no frame tag.
- **Direction metadata**: every frame carries a logical direction
  (`SenderToReceiver`, `ReceiverToSender`, `GeneratorToSender`,
  `GeneratorToReceiver`).
- **Monotonic capture timestamps**: nanoseconds since the recorder
  started. The clock is monotonic, never wall-clock, so replay
  ordering is deterministic across timezones and DST boundaries.

Out of scope (see section 9 for the explicit list):

- TLS/SSH cipher state. SSH transcripts are recorded after the SSH
  layer, on the rsync wire. The harness is not a TLS recorder.
- File content not carried in the wire stream. Side-channel data
  (e.g. extended attributes resolved out-of-band) is not part of the
  capture.
- Replay of timing. Replay is structurally faithful, not
  realtime-faithful. Timestamps are diagnostic, not enforced.

## 3. Capture format

The capture is a binary file with a fixed header followed by a
chunked sequence of frame records. Endianness is little-endian
throughout, matching the rsync wire format and the multiplex header
byte order documented in `crates/protocol/src/multiplex/mod.rs:9-14`.

### 3.1 File header (32 bytes)

```text
offset  size  field          description
  0      8    magic          ASCII "OCRWCAP\0"
  8      2    format_version u16 LE; current = 1
 10      2    flags          u16 LE bitfield (see 3.5)
 12      4    proto_version  u32 LE; rsync protocol version (28-32)
 16      8    started_at     u64 LE; monotonic ns since recorder start (always 0 here)
 24      4    record_count   u32 LE; total number of frame records, for fast skim
 28      4    crc32c_header  u32 LE; CRC32C over bytes 0..28
```

### 3.2 Frame record

Each frame record is a self-describing chunk:

```text
offset  size  field          description
  0      4    record_len     u32 LE; total record length including this field
  4      1    record_kind    u8;   0=frame, 1=raw, 2=marker, 3=note (see 3.4)
  5      1    direction      u8;   0=sender->receiver, 1=receiver->sender,
                                    2=generator->sender, 3=generator->receiver
  6      1    msg_code       u8;   MessageCode value (Data=0, Info=2, ...);
                                    0xff for raw/marker/note records
  7      1    reserved       u8;   must be 0
  8      8    timestamp_ns   u64 LE; ns since file header started_at
 16      4    payload_len    u32 LE; payload byte count
 20      N    payload        N bytes; N == payload_len
 20+N    4    crc32c         u32 LE; CRC32C over bytes 0..20+N
```

### 3.3 Record alignment

Records are byte-packed with no padding. The trailing CRC32C lets a
diff tool resync after a corrupt record by scanning forward for the
next valid record_len. Self-describing records make the format
diffable via a thin pretty-printer (see section 6).

### 3.4 Record kinds

- `frame` (0): a fully framed multiplex message. `msg_code` matches
  the `MessageCode` enum in
  `crates/protocol/src/envelope/message_code.rs`. Payload is the
  post-header bytes of one frame.
- `raw` (1): pre-multiplex bytes (greeting, handshake, post-shutdown
  trailer). `msg_code` is 0xff. Payload is whatever bytes flowed on
  the socket.
- `marker` (2): synthetic marker emitted by the recorder
  (`SessionStart`, `MultiplexEnabled`, `KeepaliveBoundary`,
  `SessionEnd`). Payload is a UTF-8 string naming the marker.
- `note` (3): operator annotation. Stored verbatim in capture; never
  emitted during replay. Use case: tagging a capture as "minimized
  reproducer for #1234" so the diff tool can surface it.

### 3.5 Header flags

```text
bit  meaning
 0   capture was taken on the sender side
 1   capture was taken on the receiver side
 2   capture covers a daemon (TCP) session
 3   capture covers an SSH session
 4   capture covers a local-copy session (no socket; in-process pipe)
 5   capture is a minimized reproducer (record_count is post-shrink)
 6   capture has been re-encoded canonically (timestamps zeroed)
 7   reserved (must be 0)
```

Bits 0-1 are mutually exclusive within a single capture file; a
two-sided capture is two files. Bits 2-4 are mutually exclusive.

### 3.6 Why a custom format

Alternatives considered and rejected:

- **pcap/pcapng**: built for IP packets, not application frames.
  Frame boundaries and message tags would live in opaque comments.
- **JSON / JSONL**: payloads need base64 framing and cannot
  represent invalid-UTF-8 raw bytes cleanly.
- **CBOR / msgpack**: no CRC contract, and byte-level diff tooling
  is weaker than for a fixed layout with explicit lengths.

A fixed binary format with named offsets fits the diffable,
replayable, self-describing requirement.

## 4. Replay modes

Three replay modes cover the use cases identified in section 1.

### 4.1 One-shot deterministic replay against a stub peer

The harness instantiates an in-process `StubPeer` and feeds it the
capture's frames in recorded direction order. The stub asserts on
divergence: an unexpected frame, an unexpected payload, or an
unexpected close.

```rust
use test_support::protocol_capture::{CaptureFile, Replayer, StubPeer};

#[test]
fn replays_v32_push_handshake() {
    let cap = CaptureFile::open("tests/captures/v32_push_basic.ocrwcap").unwrap();
    let stub = StubPeer::expect_from(&cap);
    let mut replayer = Replayer::new(cap);
    replayer.drive(stub).expect("capture replays cleanly");
}
```

This is the cheapest mode. It runs in process, has no socket, and is
the default for unit tests. It is the mode the differential fuzzer
uses to confirm a replay is "still good" before applying mutations.

### 4.2 Interactive replay with a single live peer

The harness substitutes for one side of the protocol. The live peer
is either `oc-rsync` or upstream `rsync`, started in subprocess.
The harness pipes recorded frames to the peer's stdin (or socket)
and validates the peer's responses against the capture's
opposite-direction frames.

This mode is for interop debugging: take a capture from a known-good
session, then re-drive only one side of it against a candidate
build. Divergence localizes the bug to the side under test.

```rust
use test_support::protocol_capture::{CaptureFile, InteractiveReplayer, PeerSide};

#[test]
#[ignore = "spawns rsync subprocess; opt-in"]
fn drives_upstream_receiver_with_oc_rsync_capture() {
    let cap = CaptureFile::open("tests/captures/v32_pull_acl.ocrwcap").unwrap();
    let mut replayer = InteractiveReplayer::new(cap, PeerSide::ReceiverIsLive);
    replayer.spawn_peer("rsync", &["--server", "--protocol=32", "..."]);
    replayer.run().expect("upstream receiver matches capture");
}
```

Tests in this mode are gated behind `#[ignore]` and a
`PROTOCOL_CAPTURE_INTERACTIVE=1` environment guard, mirroring the
pattern used by `tools/ci/run_interop.sh` for live-rsync work.

### 4.3 Bisected mid-stream injection

The harness replays the capture verbatim up to a configurable
injection offset (frame index N), at which point it substitutes a
mutated frame for frame N and continues. Both sides observe the
mutated frame from frame N onward.

This is the mode the wire-format differential fuzzer uses (see
`docs/design/wire-format-differential-fuzzer.md` section 4 for
mutation strategies). Bisected injection is also useful for hand
debugging: an operator can binary-search for the smallest offset at
which a given mutation triggers a divergence.

```rust
let cap = CaptureFile::open("tests/captures/v32_push_basic.ocrwcap")?;
let mut replayer = Replayer::new(cap);
replayer.inject_at(42, |frame| {
    let mut bytes = frame.payload().to_vec();
    bytes[0] ^= 0x01;
    frame.set_payload(bytes);
});
replayer.drive(stub_peer)?;
```

The `inject_at` callback is a pure function over the frame; the
harness re-frames automatically after mutation, enforcing the
24-bit payload limit documented at
`crates/protocol/src/multiplex/mod.rs:14`.

## 5. API surface

The public API lives entirely in
`crates/test-support/src/protocol_capture.rs`. Only the types listed
below are pub; everything else is implementation detail.

```rust
pub struct CaptureFile { /* ... */ }
pub struct Recorder { /* ... */ }
pub struct Replayer { /* ... */ }
pub struct InteractiveReplayer { /* ... */ }
pub struct StubPeer { /* ... */ }
pub struct FrameRecord {
    pub direction: Direction,
    pub kind: RecordKind,
    pub msg_code: Option<MessageCode>,
    pub timestamp_ns: u64,
    pub payload: Vec<u8>,
}

pub enum Direction { SenderToReceiver, ReceiverToSender,
                     GeneratorToSender, GeneratorToReceiver }
pub enum RecordKind { Frame, Raw, Marker, Note }
pub enum PeerSide { SenderIsLive, ReceiverIsLive }
```

`CaptureFile` is the parsed file. `Recorder` is the writer side, used
in test fixtures that wrap a real session. `Replayer` and
`InteractiveReplayer` are the two non-stub replay engines.
`StubPeer` is the in-process assertion oracle for one-shot replay.

`Recorder` integrates with the production protocol stack via a thin
shim trait that the multiplex layer already exposes for test use:
the `MplexReader` and `MplexWriter` types in
`crates/protocol/src/multiplex/reader.rs` and `writer.rs`. The
recorder wraps an existing reader/writer pair, captures bytes as
they flow, and forwards transparently.

```rust
use test_support::protocol_capture::Recorder;
use protocol::{MplexReader, MplexWriter};

let recorder = Recorder::new("captures/session.ocrwcap")?;
let (rx, tx) = (MplexReader::new(socket_rx), MplexWriter::new(socket_tx));
let (rx, tx) = recorder.wrap_pair(rx, tx);
// Use rx/tx as normal; recorder writes to disk as bytes flow.
```

Production code does not depend on `test-support`; only test code
does, and only when explicitly wired in.

## 6. Storage layout and tooling

### 6.1 On-disk layout

Captures live under `tests/captures/<protocol>/<scenario>.ocrwcap`.
The naming convention mirrors `tests/wire-corpus/` from the
differential-fuzzer design:

```text
tests/captures/
  v28/push/basic.ocrwcap
  v29/pull/acl.ocrwcap
  v30/push/hardlinks.ocrwcap
  v31/pull/xattr.ocrwcap
  v32/push/sparse.ocrwcap
  v32/pull/zstd.ocrwcap
  manifest.toml
```

`manifest.toml` records, per capture: protocol version, direction,
SHA-256 of the file, the upstream rsync source identity that
produced it (when applicable), and the human-readable scenario
description. The manifest is self-describing enough that a CI script
can verify capture freshness without running the full interop
matrix.

### 6.2 Pretty-printer

A `cargo run -p test-support --bin ocrwcap-print` companion (lands
in a follow-up; not part of this design's scope) reads the binary
format and emits a one-record-per-line text representation:

```text
0001  +000000000ns sender->receiver  Data        len=4096  [hash 8a3c...]
0002  +000087431ns receiver->sender  Info        len=27    "...transferring foo.bin"
0003  +000201118ns sender->receiver  Data        len=4096  [hash 7d11...]
0004  +000291002ns receiver->sender  Stats       len=18    [decoded as DeltaStats]
```

Diffing two captures with `diff -u` then surfaces the structural
difference directly, which is the diffability requirement from
section 1.

## 7. Compatibility considerations

### 7.1 Multi-protocol replay

Captures embed the negotiated protocol version in the file header
(`proto_version` at offset 12). The replayer enforces version
matching at open time. A capture taken at v28 cannot be replayed
against a v32-only stub peer: the version-aware decoder paths in
`crates/protocol/src/version/protocol_version/mod.rs` would diverge
on, for example, the v28/v29 4-byte LE varint boundary documented
in the differential-fuzzer note.

For multi-protocol scenarios (a transfer that exercises the
handshake's version-down-negotiation path), the capture covers the
full handshake including the version exchange. Replay drives the
peer through the recorded negotiation; if the peer offers a
different version range, replay fails fast with a structured error
rather than masking the divergence.

### 7.2 Endianness

The on-disk format is little-endian throughout. Captures are
portable across hosts because:

- The wire protocol is little-endian at all points oc-rsync touches
  (varint encoding in `crates/protocol/src/varint/`, multiplex
  envelope in `multiplex/mod.rs:11-13`).
- The capture file header uses the same byte order, so a single
  byte-swap path on big-endian hosts is unnecessary.
- The CRC32C polynomial and byte order are pinned to the IETF
  RFC 3720 specification.

A capture produced on aarch64 macOS replays bit-identically on
x86_64 Linux. The format documents this invariant explicitly so
future contributors do not introduce host-endian fields.

### 7.3 Capability flags

Captures are taken after capability negotiation. The recorded
capability byte string (the `-e.LsfxCIvu`-style suffix per the
`SSH capability string` note in the project memory) is part of the
handshake bytes. Replay drives the peer through the recorded
capabilities; mismatched capabilities produce a fast structured
error.

### 7.4 Format version evolution

The `format_version` field at offset 8 starts at 1. Future format
changes bump the version and add migration logic in `CaptureFile::open`.
The reader rejects unknown versions explicitly. This prevents the
silent-misread failure mode that has bitten pcap consumers.

## 8. Integration with existing fuzzers

### 8.1 Wire-format differential fuzzer (`docs/design/wire-format-differential-fuzzer.md`)

The differential fuzzer's mutation strategies (single-byte flip,
length-field corrupt, truncate, repeat, byte insert, byte delete)
operate on byte slices today. Captures from this harness become
mutation seeds: each `frame` record's payload is a candidate
mutation target.

The capture format pre-segments the byte stream into frames and
labels each with its `MessageCode`. The fuzzer can therefore bias
mutation toward, say, only `MessageCode::Data` payloads (the most
common frame type), or only `MessageCode::Stats` payloads (the
rarest, where coverage gaps tend to hide). This is more targeted
than the offset-uniform mutation the differential fuzzer applies
today.

### 8.2 Behavioural protocol fuzzer (`docs/design/protocol-fuzzing-harness.md`)

The behavioural fuzzer's regression suite stores reproducers as
`Scenario` JSON. The capture/replay harness adds a binary
counterpart: when a behavioural-fuzzer divergence is shrunk via
`cargo fuzz tmin`, the resulting bytes are also written as a
capture file, so the regression test runs both the JSON-driven
end-to-end scenario and the byte-level replay against a stub peer.
Two oracles, one regression entry, deterministic minimization on
both axes.

### 8.3 Existing wire-byte fuzz harness (`crates/protocol/fuzz/`)

The fuzz harness consumes raw bytes via libFuzzer. Captures
unblock its corpus seeding: the existing `seed_corpus.sh` (per the
fuzzer design) can extract per-frame payloads from any capture and
deposit them into `crates/protocol/fuzz/corpus/<target>/` without
hand-curation.

## 9. Non-goals

- **Not a man-in-the-middle proxy.** The harness records bytes that
  flow through the rsync stack inside one process; it does not
  intercept on the wire between two processes. A real MITM proxy
  belongs in a separate tool with its own threat model.
- **Not a network simulator.** Replay does not introduce latency,
  packet loss, or reordering. The bytes flow as fast as the replay
  loop can drive them. Network conditions are not part of the
  reproducer surface.
- **Not a substitute for live interop tests.** The fixed interop
  matrix in `tools/ci/run_interop.sh` runs against real upstream
  binaries on real fixtures. Replay is faster and more diagnostic,
  but it cannot catch a divergence that arises from a third-party
  upstream version not previously captured. Live interop stays the
  primary correctness gate.
- **Not a recorder for SSH cipher state.** Captures sit above the
  SSH layer. SSH key exchange, cipher negotiation, and payload
  framing are out of scope; the harness records the rsync bytes
  that flow inside the SSH tunnel, not the tunnel itself.
- **Not a wire-level encryption recorder.** Daemon mode does not
  encrypt; SSH mode is recorded post-decrypt. There is no plaintext
  capture of an encrypted session.
- **Not a replacement for `tcpdump`-based pcap captures.** pcap
  captures the raw socket bytes including TCP framing and
  retransmits. Captures here are application-level, post-mux. Both
  formats coexist; the pcap captures referenced in the behavioural
  fuzzer (#2075) cover the wire-byte-equivalence oracle, while
  ocrwcap captures cover the frame-level replay oracle.

## 10. Risks and mitigations

**Capture drift across protocol versions.** A capture taken at v32
becomes useless when a future revision changes a frame layout.
Mitigation: the manifest pins the upstream rsync identity and the
proto_version; a CI smoke test verifies that committed captures
still parse on every PR.

**Capture size on disk.** A 100k-file transfer with `--debug=all`
produces hundreds of MB. Mitigation: captures are trimmed by
default to frames that contributed to the failure mode under
investigation. The `Recorder` accepts a max-size cap and a per-
`MessageCode` filter (e.g. drop all `Info` frames).

**Operator data leakage.** Captures may contain file path bytes,
xattr names, and other operator-derived strings. Captures committed
to the repository are produced from `target/interop/` synthetic
fixtures only, mirroring the discipline in
`docs/design/wire-format-differential-fuzzer.md` section 11. The
`Recorder` refuses to write a capture whose source path is outside
the configured fixture root.

**Replay against a moving stub.** If `StubPeer`'s expectations
evolve, old captures fail. Mitigation: the stub is keyed by
file_version; format_version=1 has a locked stub. Evolution ships a
new stub.

**False sense of coverage.** A green replay proves only that the
capture's bytes still parse. The harness is a regression-prevention
tool, not a correctness oracle. Test names make this explicit.

## 11. Tracking

Follow-up work items, named here for traceability and not added to
the persistent tracker:

- **Format and core types**: implement
  `crates/test-support/src/protocol_capture.rs` with the file
  header, frame record, `CaptureFile`, `FrameRecord`, `Direction`,
  `RecordKind`. Unit tests verify roundtrip and CRC validation.
- **Recorder**: implement the `Recorder` shim around `MplexReader`
  and `MplexWriter`. Integration test wraps an existing
  `crates/protocol/tests/golden_protocol_v28_wire.rs` scenario,
  records, then verifies the capture reproduces the expected wire
  bytes.
- **One-shot replay and stub peer**: implement `Replayer` and
  `StubPeer`. Add a starter capture suite at
  `tests/captures/v28/`, `v29/`, `v30/`, `v31/`, `v32/` covering
  push, pull, with and without compression.
- **Interactive replay**: implement `InteractiveReplayer`. Test with
  upstream rsync 3.4.1 in the `rsync-profile` container.
- **Bisected injection**: implement `inject_at` for `Replayer`.
  Wire into the differential fuzzer's seed pipeline so any captured
  frame becomes a mutation candidate.
- **Pretty-printer**: implement
  `crates/test-support/src/bin/ocrwcap-print.rs`. The output
  format is the line-per-record format from section 6.2.
- **Fuzzer seed extractor**: implement
  `tools/ci/extract_capture_seeds.sh` that walks the capture tree
  and deposits per-frame payloads into the wire-fuzzer corpus.
- **CI smoke test**: add a fast `cargo nextest` test that opens
  every committed capture and verifies CRC and format_version. Runs
  on every PR; catches capture-format regressions immediately.
- **Documentation**: extend `crates/test-support/README.md` with a
  capture/replay section once the API stabilizes.

Each follow-up is a separately reviewable PR. The order above is
the intended landing order: format and core types first (no
production-code touch), then the recorder, then one-shot replay,
then the more advanced modes. The harness is usable for regression
work after the third item lands.

## 12. References

- `crates/protocol/src/multiplex/mod.rs` - multiplex framing
  documentation and `MessageFrame`/`recv_msg`/`send_msg` exports.
- `crates/protocol/src/envelope/message_code.rs` - `MessageCode`
  enum, mirroring upstream `enum msgcode` from `io.c`.
- `crates/protocol/src/multiplex/reader.rs`,
  `crates/protocol/src/multiplex/writer.rs` - `MplexReader` and
  `MplexWriter` wrappers the recorder shims around.
- `docs/design/wire-format-differential-fuzzer.md` - byte-level
  mutation harness; consumes captures as mutation seeds.
- `docs/design/protocol-fuzzing-harness.md` - behavioural
  end-to-end fuzz harness; shares the regression-test surface.
- `crates/test-support/src/lib.rs` - existing test-support entry
  point; `protocol_capture` joins as a sibling module.
- `tools/ci/run_interop.sh` - live interop matrix; the harness
  complements but does not replace it.
