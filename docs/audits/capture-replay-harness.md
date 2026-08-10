# Protocol-level capture/replay test harness

RFC, sketch only. No implementation in this PR.

Tracking issue: oc-rsync task #1850. Branch: `feat/capture-replay-harness-rfc-1850`.
Last verified: 2026-05-01.

## Scope

This RFC sketches a protocol-level capture/replay test harness that lives in
the `test-support` crate. The harness records the live wire bytes of a real
oc-rsync transfer to an on-disk log, then replays the log into the receiver or
sender side under test - without spawning real upstream rsync processes or a
TCP daemon. It sits between two coverage tiers we already have:

- Synthetic golden byte tests at `crates/protocol/tests/golden_protocol_v28_*.rs`,
  `crates/protocol/tests/golden_protocol_v29_*.rs`, and
  `crates/protocol/tests/golden_handshakes.rs` - hand-crafted byte arrays for
  individual frames or handshakes, ~1.3k lines per file.
- Live interop at `tools/ci/run_interop.sh` - spawns the upstream `rsync`
  binary against `oc-rsync --daemon` on a non-privileged port, exercising real
  network stacks against versions 3.0.9, 3.1.3, 3.4.1.

A capture/replay harness fills the gap: high-fidelity transcripts of real
transfers, replayable as deterministic regression tests without process
spawning, system rsync packages, or a network listener.

This RFC is docs-only. No Rust code is added.

## Survey of the current state

### `crates/test-support/`

The crate exists today as a tiny shared-test-helpers module
(`crates/test-support/src/lib.rs`, 31 lines total). It currently exports a
single helper, `create_tempdir()` (lines 17-31), that wraps `tempfile::tempdir()`
with retry logic for Windows CI flakes. The crate is registered in the workspace
manifest at `Cargo.toml:154` (`"crates/test-support"`). No other modules exist;
the crate is the natural home for new shared test machinery.

### `crates/protocol/tests/` golden coverage

There is no `crates/protocol/tests/golden/` subdirectory on master. Golden
byte tests live as flat top-level integration test files in
`crates/protocol/tests/`. Representative files:

- `crates/protocol/tests/golden_protocol_v28_wire.rs` (1357 lines) - wire
  format for protocol 28 file entries, encoded inline as expected byte arrays.
  See lines 1-17 for the per-file rationale comment block citing
  `flist.c:send_file_entry()` / `recv_file_entry()`.
- `crates/protocol/tests/golden_handshakes.rs` (1277 lines) - handshake byte
  vectors.
- `crates/protocol/tests/golden_protocol_v28_flist.rs`,
  `golden_protocol_v28_handshake.rs`,
  `golden_protocol_v28_mplex_delta_stats.rs`, `golden_protocol_v29_flist.rs`,
  `golden_protocol_v29_wire.rs`, `lz4_golden_bytes.rs`, `zlib_golden_bytes.rs`,
  `zstd_golden_bytes.rs`, `zstd_daemon_recv_golden.rs`,
  `zstd_interop_golden_bytes.rs`.

These are hand-authored. They cover individual frames and short fragments
well, but transcribing a full multi-file transfer by hand is impractical, and
each file is a wall of literal byte arrays that drift from real wire output
when the sender's emission order changes.

### `tools/ci/run_interop.sh`

The live harness (`tools/ci/run_interop.sh`, 9465 lines including embedded
fixtures and helpers, header at lines 1-50) downloads upstream `.deb`
packages for rsync 3.0.9, 3.1.3, 3.4.1 (or builds from source as a
fallback), starts `oc-rsync --daemon` on a non-privileged port, and runs
push/pull scenarios across the version matrix. It is comprehensive but
heavyweight - it requires `curl`, `tar`, `dpkg`, sudo on some targets, and
several minutes of wall time per CI run. It is unsuitable for fast
property-test loops or for pinning a single regression.

### Multiplex layer (capture surface)

The multiplex frame I/O lives at `crates/protocol/src/multiplex/`, with the
public surface declared at `crates/protocol/src/multiplex/mod.rs:1-39`. The
key entry points are:

- `crates/protocol/src/multiplex/io/send.rs:16` - `send_msg()`
- `crates/protocol/src/multiplex/io/send.rs:32` - `send_frame()`
- `crates/protocol/src/multiplex/io/recv.rs:14` - `recv_msg()`
- `crates/protocol/src/multiplex/io/recv.rs:31` - `recv_msg_into()`
- `crates/protocol/src/multiplex/reader.rs` - `MplexReader` (518 lines)
- `crates/protocol/src/multiplex/writer.rs` - `MplexWriter` (676 lines)
- `crates/protocol/src/multiplex/frame.rs:10` - `MessageFrame { code, payload }`

The 4-byte little-endian envelope header (tag in bits 24-31, payload length in
bits 0-23) is documented inline at `crates/protocol/src/multiplex/mod.rs:9-14`.

### Client orchestration entry points

The project conventions document references `core::session()`, but the actual
public entry points on master are:

- `crates/core/src/client/run/mod.rs:113` - `pub fn run_client(config: ClientConfig)`
- `crates/core/src/client/run/mod.rs:166` - `pub fn run_client_with_observer(...)`

These dispatch to local copy, SSH transfer, or daemon protocol based on the
operands (`crates/core/src/client/run/mod.rs:177-200`). The transport layer
itself is split between `crates/rsync_io/` (handshake, multiplex, daemon and
SSH adapters) and `crates/transfer/` (server-side runner). There is no
single "transport trait" injection point; the natural seam for capture is the
`Read+Write` pair handed to the multiplex layer.

## Goals and non-goals

### Goals

- Record the byte stream of a real oc-rsync transfer with enough fidelity
  to deterministically replay it into either side of the protocol
  (sender or receiver) under test.
- Drive `MplexReader` / `MplexWriter` from a captured log without any
  network or child process.
- Pin tricky wire-protocol edge cases as small, fast, hermetic unit tests.
- Make CI-only flakes locally reproducible by capturing the wire output
  once in CI and replaying the captured log on a developer machine.

### Non-goals

- Not a fuzzer. Mutation-based fuzzing remains the responsibility of tasks
  #1304 (multiplex fuzz), #1365 (flist fuzz), #1196 (filter fuzz) and the
  existing `proptest_*` files in `crates/protocol/tests/`. The harness can
  feed seed corpora to those fuzzers but does not itself mutate input.
- Not a benchmark harness. Capture and replay should incur low overhead but
  benchmarks belong in `scripts/benchmark*.sh`.
- Does not replace the live interop harness `tools/ci/run_interop.sh`.
  Cross-version interop must continue to run real upstream rsync.

## Capture format

A binary log, one record per wire frame. The format is intentionally simple
to keep parser bugs out of the test infrastructure path.

### Header

```
magic        : 4 bytes  "OCRC"            (oc-rsync capture)
schema_ver   : 2 bytes  little-endian u16  (initial value: 1)
proto_ver    : 1 byte                      (rsync wire protocol, e.g. 32)
flags        : 1 byte                      (bit 0: includes_timing,
                                            bit 1: post_decompression,
                                            bit 2: includes_both_directions)
session_id   : 16 bytes  random            (correlate sender + receiver logs)
start_ns     : 8 bytes   little-endian u64 (monotonic ns at capture start)
reserved     : 32 bytes  zero              (room for forward-compat fields)
```

### Per-frame record

```
timestamp_delta_ns : varint   (ns since previous record, or session start)
direction          : 1 byte   (0 = client->server, 1 = server->client)
frame_kind         : 1 byte   (0 = mux frame, 1 = raw bytes pre-mux,
                               2 = handshake, 3 = keepalive, 4 = control)
mux_code           : 1 byte   (only when frame_kind == 0; MessageCode value)
payload_len        : varint   (bytes in payload)
payload            : <payload_len> bytes
```

### Decisions

- **Single file vs split files.** Recommend a single file with a per-record
  direction byte. The two flows are interleaved on the wire and the relative
  ordering matters for replay. Split files force the replayer to merge by
  timestamp, which is fragile.
- **Capture both directions.** Required. A receiver-side replay still needs
  to see the receiver's outbound bytes to verify them.
- **Schema version field.** Schema version 1 is the initial layout; bumping
  the byte at offset 4 is the only mechanism for incompatible changes.
- **Timestamp delta.** Encoded as varints. Delta encoding keeps records
  small for the common case (frames within microseconds of each other) and
  the field is optional - a pure-replay test ignores it.

## Capture surface

The recorder hooks in at the multiplex frame boundary. It is implemented as
an adapter that wraps the `Read+Write` transport handed to the multiplex
layer:

- For SSH transfers, the adapter wraps the `SshReader`/`SshWriter` pair from
  `crates/rsync_io/src/ssh/connection.rs` (`SshReader` at line 213,
  `SshWriter` at line 225).
- For daemon transfers, the adapter wraps the TCP stream returned by the
  daemon negotiation path in `crates/rsync_io/src/daemon/`.
- For local transfers, no capture is meaningful (no wire bytes).

The adapter tees every byte read or written to a buffered `File` writer
behind a varint-prefixed record format. Activation is gated on the env var
`OC_RSYNC_CAPTURE_PATH`; when unset, the adapter is bypassed entirely and
the cost is zero. This mirrors how `OC_RSYNC_FORCE_NO_COMPRESS` is used at
`crates/cli/src/frontend/execution/drive/options.rs:374` and how
`OC_RSYNC_BUILD_OVERRIDE` is consumed at `crates/branding/build.rs:14`.

### Pre- vs post-compression

Compression negotiation (`zlib`, `zlibx`, `zstd`, `lz4`) happens above the
multiplex layer; the recorded payloads are post-decompression. A capture
taken with `--compress-choice=zstd` must be replayable into a build that
negotiated `zlib` or no compression at all, without divergence on every
frame. Capturing post-decompression bytes also makes the captures stable
across compressor library upgrades (e.g., zstd minor versions changing
internal framing).

The flags byte in the header sets bit 1 (`post_decompression`) to record
this choice explicitly so a future raw-byte capture mode can co-exist
without ambiguity.

### Frame kinds included

Recommend including all multiplex codes - `MSG_INFO`, `MSG_ERROR`,
`MSG_DATA`, `MSG_LOG`, `MSG_STATS`, etc. - and labeling each with its
`MessageCode` byte. Filtering to data frames only would discard the very
control-channel behaviour that is hardest to test by hand and most likely
to regress (delete stats messages, deferred error reporting, the
`NDX_DEL_STATS` goodbye sequence). The replayer can opt-in to ignoring
specific codes per test.

## Replay surface

The replayer is a `Read+Write` adapter that satisfies the same trait bounds
as the production transport. It is constructed from a captured log file and
operates in one of two modes.

### Strict byte-equal mode

- On `read(buf)`: returns the next bytes from records flagged
  "server->client" (or "client->server" depending on which side is being
  driven), splitting the payload across calls to match the `buf` length the
  consumer requested.
- On `write(buf)`: compares the bytes against the next outbound record. Any
  divergence (length, content, code, direction) panics with a hex-dump of
  the expected vs actual frame and the record offset.
- Useful for: regression tests that pin every byte. Catches even harmless
  reorderings.

### Structural verification mode

- Parses each record into a `MessageFrame` (via the existing
  `crates/protocol/src/multiplex/io/recv.rs:14` `recv_msg()` path) and
  compares logically:
  - Same `MessageCode`.
  - Same payload length, but payload bytes compared after parsing the
    payload as the appropriate type (file entry, delta token, varint
    sequence) so that semantically equivalent encodings (e.g., file-list
    entries emitted in a different sort order within an INC_RECURSE
    segment) do not flag as divergence.
  - Multi-frame messages reassembled before comparison.
- Useful for: validating that a refactor preserves protocol semantics
  without forcing byte-for-byte reproducibility.

The two modes share the same log format and the same `Replayer` type;
the mode is a runtime flag on construction.

## `test-support` integration

A new submodule:

```
crates/test-support/
    Cargo.toml
    src/
        lib.rs                 // existing helpers + pub mod capture_replay;
        capture_replay/
            mod.rs             // public surface
            recorder.rs        // Recorder<R: Read, W: Write>
            replayer.rs        // Replayer (Read + Write impl)
            session.rs         // CaptureSession header / footer / framing
            log.rs             // file format encode/decode
```

Public types proposed:

- `CaptureSession` - header metadata (schema version, proto version, session
  id, capture start time).
- `Recorder` - tee adapter used by code under test when
  `OC_RSYNC_CAPTURE_PATH` is set.
- `Replayer` - replacement for the production `Read+Write` transport in
  tests. Constructor takes a `Path` to a `.ocrc` log and a `ReplayMode`.

Tests wire it up by replacing the `Read+Write` pair handed to the multiplex
layer at the existing transport seams in `crates/rsync_io/src/ssh/connection.rs`
and the daemon path. The orchestration entry point
`run_client(ClientConfig)` (`crates/core/src/client/run/mod.rs:113`) does
not need a new injection point - the test seam is one layer down, where the
`Read+Write` is constructed.

This honours the project's design-pattern guidance:

- **Strategy Pattern.** `ReplayMode::Strict` vs `ReplayMode::Structural`
  are interchangeable strategies behind one type.
- **Dependency Inversion.** The multiplex layer already depends on
  `Read+Write` traits, not concrete `TcpStream` or `ChildStdin`. The
  harness slots into the same abstraction.

## Use cases

### 1. Pin a wire-format gotcha

Task #1670 (protocol 28 INC_RECURSE flist encoding edge case) is the
canonical example. A capture taken once during a successful transfer
becomes a regression test that runs in milliseconds and fails immediately
if the encoding regresses.

### 2. Property-test the parser by mutating recorded streams

Feed a captured log into `proptest_wire_format_fuzz.rs`
(`crates/protocol/tests/proptest_wire_format_fuzz.rs`) as a seed corpus.
Mutate individual fields (truncate payloads, flip type bytes, inject
oversized lengths) and confirm the parser surfaces a clean error rather
than panicking. The harness produces realistic seeds, not synthetic ones.

### 3. Reproduce CI-only flakes locally

A CI run with `OC_RSYNC_CAPTURE_PATH=$RUNNER_TEMP/flake.ocrc` produces a
log on failure that can be downloaded as a CI artifact and replayed on a
laptop without standing up the full interop container or the multi-version
upstream rsync matrix.

### 4. Bisect with confidence

`git bisect run` against a strict-mode replay test gives byte-precise
fault localisation for protocol regressions. The replay is faster than
the live interop harness by 2-3 orders of magnitude.

## Open questions

### Capture file size

A 100k-file flist transfer produces several megabytes of file-list bytes
plus per-file delta tokens. The format must be streamable - the recorder
writes records in append-only fashion and the replayer reads them
sequentially without slurping the whole file. We should also document a
`zstd`-compressed variant of the on-disk format for committed CI fixtures
(magic prefix `OCRC`+`zstd` framing). Decision: do not gzip/zstd the
recorder's output by default - keeping the raw format makes ad-hoc
inspection (`hexdump`, `xxd`) viable.

### Compression and cipher mismatch

Capturing pre-compression bytes would make captures unreplayable across
codec changes. We capture post-decompression and explicitly flag this in
the header (`flags` bit 1). A future `flags` bit 0 raw-pre-compression
mode is reserved but not in scope for this RFC.

### Multiplex codes to include

Recommend include all (`MSG_INFO`, `MSG_ERROR`, `MSG_LOG`, `MSG_DATA`,
etc.) and label them. The replayer's structural mode can ignore specific
codes per test by configuration.

### Privacy

Captured logs contain real filenames and may contain real file contents
embedded in `MSG_DATA` frames. Tests must run inside `tempfile::TempDir`
populated with synthetic data. The recorder's docstring will warn against
capturing production transfers and committing the result. Recorded logs
checked into the repository must use only synthetic test fixtures.

### Where capture lives at runtime

`OC_RSYNC_CAPTURE_PATH` is consulted at the seam where the `Read+Write`
transport is constructed. The check should follow the existing env-var
patterns: looked up once via `EnvGuard` in tests, via `std::env::var` at
construction time in production builds, never at every read or write call.

### Schema evolution

Schema version 1 is the initial layout. Any incompatible change increments
the schema_ver byte. The replayer will reject a higher schema_ver than it
understands with a clear error pointing at the recorder's version. The
8 bits of the version field cap us at 255 schema iterations - sufficient.

## Phasing

- **Phase 1 (this RFC).** Documented design, surveyed existing test
  surfaces, identified the integration seams. No code.
- **Phase 2.** Implement `Recorder` and the on-disk log format. Land it
  behind `OC_RSYNC_CAPTURE_PATH` so it is dormant unless explicitly
  enabled. Add a unit test that records a small synthetic transfer and
  verifies the log roundtrips cleanly.
- **Phase 3.** Implement `Replayer` in strict mode. Land the first
  regression test that pins task #1670 (protocol 28 INC_RECURSE flist
  encoding). Add CI artifact upload of capture logs on test failure.
- **Phase 4.** Implement structural verification mode. Use it to lock in
  the multiplex frame parser against its existing fuzz corpus.
- **Phase 5 (optional).** Wire the harness into the existing fuzz tasks
  (#1304, #1365, #1196) as a seed-corpus producer.

## Cross-references

- Existing golden coverage in `crates/protocol/tests/golden_protocol_v28_*.rs`,
  `crates/protocol/tests/golden_protocol_v29_*.rs`,
  `crates/protocol/tests/golden_handshakes.rs`. There is no consolidated
  README; this RFC is the first cross-cutting design doc that references
  them collectively.
- Live interop: `tools/ci/run_interop.sh`.
- Related fuzz tasks: #1304 (multiplex fuzz), #1365 (flist fuzz), #1196
  (filter fuzz), #1303 (varint fuzz).
- Related completed work: #1191 (lock-free error collection) - reference
  pattern for adding new test-side machinery without touching production
  hot paths.
- Multiplex frame I/O: `crates/protocol/src/multiplex/mod.rs:1-39`,
  `crates/protocol/src/multiplex/io/send.rs:16,32`,
  `crates/protocol/src/multiplex/io/recv.rs:14,31`,
  `crates/protocol/src/multiplex/frame.rs:10`.
- Client orchestration: `crates/core/src/client/run/mod.rs:113,166`.
- Existing env-var patterns: `crates/cli/src/frontend/execution/drive/options.rs:374`
  (`OC_RSYNC_FORCE_NO_COMPRESS`), `crates/branding/build.rs:14`
  (`OC_RSYNC_BUILD_OVERRIDE`).
- Design-pattern guidance: project's "Strategy Pattern" and "Dependency
  Inversion" notes - both directly applicable here.

## Summary

A protocol-level capture/replay harness in `crates/test-support/` would
fill the gap between hand-written golden byte tests and the heavyweight
upstream-rsync interop harness. The design hooks at the multiplex frame
boundary, captures post-decompression bytes both directions in a single
file with a versioned header, and supports both strict byte-equal and
structural verification modes. Phase 1 is this RFC; phases 2-5 deliver
the recorder, replayer, first regression test, structural mode, and
optional fuzz seed-corpus integration.
