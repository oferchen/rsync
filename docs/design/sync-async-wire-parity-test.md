# ASY-11.a: Sync vs async wire-byte parity test design

Status: Design spec. Defines the test architecture that gates ASY-12
(feature flip-to-on) by proving the `tokio-transfer` async path
produces wire-byte-identical output to the existing synchronous path.
Without this gate, the async pipeline cannot ship as default-on.

Cross-links:

- `docs/design/asy-2-tokio-runtime-feature.md` - defines the
  `tokio-transfer` feature flag and its wire-byte parity requirement
  (section 7).
- `docs/design/asy-3-async-boundary-spec.md` - per-boundary contracts;
  boundaries 1, 2, 4, 5 (wire `.await`) are the primary subjects.
- `scripts/wire-equivalence-tcpdump.sh` - existing upstream-vs-oc-rsync
  wire capture infrastructure (daemon mode, tcpdump/tshark).
- `crates/protocol/tests/golden_protocol_v28_flist.rs`,
  `golden_protocol_v29_flist.rs` - existing golden byte test patterns.
- `crates/protocol/tests/msg_info_coalescing_parity.rs` - precedent for
  logical-equivalence testing when physical framing varies.

## 1. Problem statement

The `tokio-transfer` feature replaces blocking `read`/`write`/`flush`
calls on the wire with async `.await` equivalents. Buffering behaviour,
flush timing, and task scheduling differ between the two paths. If the
async path emits even one byte differently on the wire, interop with
upstream rsync breaks silently. We need an automated regression gate
that fails loudly whenever the two paths diverge.

## 2. Test architecture

### 2.1 Capture layer

Introduce a `WireCapture` adapter that wraps both `Read + Write` (sync)
and `AsyncRead + AsyncWrite` (async) transports. The adapter records
every byte written to and read from the wire into an in-memory `Vec<u8>`
log, timestamped at frame boundaries (multiplex frame headers). This
lives in a test-only module:

```
crates/transfer/src/test_support/wire_capture.rs   (cfg(test))
```

The capture adapter is zero-cost when not compiled into tests - it is
gated behind `#[cfg(test)]` and never appears in release builds.

### 2.2 Dual-path harness

Each parity test runs the same transfer scenario twice:

1. **Sync path** - standard threaded pipeline via `core::session()`.
   Transport is a pair of in-memory byte streams
   (`io::Cursor<Vec<u8>>` or `pipe::PipeReader`/`PipeWriter`) wrapped
   in `WireCapture`.

2. **Async path** - tokio pipeline via `core::session()` with
   `tokio-transfer` enabled. Transport is the same byte-stream pair
   wrapped in async `WireCapture`. Runs inside `#[tokio::test]`.

Both paths operate on identical source trees (created by
`setup_test_dirs()`) and identical configuration (`CoreConfig`). The
harness collects the captured wire bytes from each run and feeds them
to the comparator.

### 2.3 In-process loopback transport

Rather than requiring a TCP daemon and tcpdump, parity tests use an
in-process loopback: both sender and receiver run in the same process
(or the same tokio runtime), connected by a duplex byte channel. This
avoids network jitter, OS scheduling noise, and platform dependencies
that would make tests flaky. The loopback channels are:

- **Sync:** `std::io::pipe()` (Rust 1.87+) or
  `os_pipe::pipe()` for the duplex pair.
- **Async:** `tokio::io::duplex(buf_size)` providing
  `DuplexStream` halves.

Both are wrapped by `WireCapture` to log bytes at the write boundary.

### 2.4 Comparison output

On mismatch, the test emits:

- First divergent byte offset.
- 64-byte hex context window around the divergence (both sides).
- Decoded multiplex frame at that offset (tag, length, payload preview).
- Full wire dumps written to `target/test-artifacts/asy-11/` for
  post-mortem analysis.

## 3. Comparison strategy

### 3.1 Strict byte-level comparison (default)

The primary assertion is `sync_wire_bytes == async_wire_bytes`. This is
the strongest guarantee and the one ASY-2 section 7 requires. Any
divergence fails the test unconditionally.

### 3.2 Semantic equivalence mode (secondary)

A secondary comparator parses both byte streams into a sequence of
multiplex frames `(MessageCode, payload_bytes)` and compares the
logical stream. This catches bugs where strict comparison fails due to
an acceptable divergence (section 5) but the logical content is still
correct.

The semantic comparator:

- Splits wire bytes into multiplex frames (4-byte header + payload).
- Groups DATA frames contiguously (coalescing splits at frame
  boundaries is acceptable per MIF-5).
- Compares the ordered sequence of `(code, concatenated_payload)` tuples.

Both comparators run on every test. The strict comparator gates CI
pass/fail. The semantic comparator provides diagnostic output when
strict fails, helping classify whether a divergence is a real bug or a
known-acceptable case needing an allowlist entry.

## 4. Test matrix

Each dimension below is crossed with the others. The full matrix is
large; CI runs a pragmatic subset (marked with priority).

### 4.1 Transfer direction

| Mode | Priority | Notes |
|------|----------|-------|
| Local push (sender + receiver in-process) | P0 | Fastest, no daemon/SSH |
| Daemon push (`rsync://`) | P1 | Exercises daemon multiplex path |
| Daemon pull (`rsync://`) | P1 | Receiver-side wire |
| SSH push | P2 | Requires mock SSH transport |
| SSH pull | P2 | Requires mock SSH transport |

### 4.2 Protocol features

| Feature | Priority | Wire impact |
|---------|----------|-------------|
| INC_RECURSE on | P0 | Segmented flist, NDX_DONE markers |
| INC_RECURSE off | P0 | Monolithic flist |
| Compression off | P0 | Baseline |
| Compression zlib | P1 | Deflate framing |
| Compression zstd | P1 | Zstd framing |
| `--checksum` | P1 | Full-file checksum exchange |
| `--whole-file` | P0 | No delta, simpler wire |
| Delta transfer (modified files) | P0 | Token stream with COPY/DATA |
| `--delete` | P1 | NDX_DEL_STATS in goodbye phase |

### 4.3 File corpus

| Corpus | Purpose |
|--------|---------|
| Empty tree | Edge case: flist end-marker only |
| Single small file (< 1 block) | Whole-file DATA token |
| Single large file (multi-block) | Delta COPY + DATA tokens |
| Mixed tree (dirs, files, symlinks) | Full flist diversity |
| Sparse file | Sparse-aware token framing |
| Files with xattrs/ACLs | Extended metadata wire format |

### 4.4 CI subset

The required CI job runs the **P0** subset: local push/pull, INC_RECURSE
on/off, no compression, whole-file and delta, with the mixed-tree
corpus. This keeps CI wall time under 60 seconds for the parity tests.

P1 tests run in the nightly interop matrix. P2 (SSH) runs only in the
weekly full-matrix job where the SSH mock transport is available.

## 5. Known acceptable divergences

The following divergences are anticipated and handled via allowlisting
in the semantic comparator rather than failing strict comparison:

### 5.1 MSG_INFO frame coalescing boundaries

The async path may coalesce MSG_INFO frames differently due to
`tokio::io::BufWriter` flush timing. Logical content is identical but
physical frame boundaries may shift. This is already validated as safe
by `msg_info_coalescing_parity.rs`.

**Mitigation:** The strict comparator normalizes MSG_INFO frame
boundaries before comparison - it concatenates consecutive MSG_INFO
payloads and compares the aggregate. DATA frame boundaries are NOT
normalized; they must match exactly.

### 5.2 Keepalive frame insertion points

Under the async path, keepalive frames (`MSG_NOOP`) may appear at
different points in the stream due to timer task scheduling. Keepalives
carry no payload and are stripped by both comparators before comparison.

### 5.3 Non-divergences (must match exactly)

The following must be byte-identical with zero tolerance:

- File list encoding (flist entries, end-of-list markers).
- Delta token stream (DATA tokens, COPY tokens, checksums).
- Checksum exchange (sum head, block checksums, file checksums).
- Goodbye handshake (NDX_DONE, NDX_DEL_STATS, exit codes).
- Compression frame boundaries and compressed payloads.
- Version negotiation and capability exchange.

## 6. CI integration

### 6.1 Test location

```
crates/transfer/tests/sync_async_wire_parity.rs
```

This is an integration test in the `transfer` crate, gated behind
`#[cfg(feature = "async")]` since it requires both paths to be compiled.

### 6.2 Feature requirements

The test compiles and runs only when both the sync and async paths are
available:

```toml
# In crates/transfer/Cargo.toml [dev-dependencies]
tokio = { workspace = true, features = ["macros", "rt-multi-thread"] }

# Test requires the "async" feature to be active
# Run: cargo nextest run -p transfer --features async -E 'test(wire_parity)'
```

### 6.3 CI workflow integration

Add to the existing `_test-features.yml` reusable workflow which
already tests feature-gated code:

```yaml
- name: Wire parity (sync vs async)
  run: |
    cargo nextest run -p transfer --features async \
      -E 'test(wire_parity)' --color never
```

This runs on the nextest(stable) Linux job. No additional runners or
containers are needed since the tests use in-process loopback.

### 6.4 Relationship to existing golden tests

The existing `golden_protocol_v28_*.rs` and `golden_protocol_v29_*.rs`
tests validate that oc-rsync's wire output matches expected byte
sequences (upstream compatibility). ASY-11 parity tests are orthogonal:
they validate that the sync and async paths produce identical output to
each other - not to a fixed golden file. This means:

- Golden tests catch upstream compatibility regressions.
- Parity tests catch sync/async divergence regressions.
- Both must pass for ASY-12 gate flip.

### 6.5 Relationship to wire-equivalence-tcpdump

The existing `scripts/wire-equivalence-tcpdump.sh` compares oc-rsync
output against upstream rsync using actual TCP captures. ASY-11 parity
tests are cheaper (in-process, no tcpdump, cross-platform) and catch a
different class of bug (internal path divergence rather than upstream
divergence). Both are complementary; neither replaces the other.

## 7. Implementation plan

| Step | Ticket | Dependency |
|------|--------|------------|
| Implement `WireCapture` adapter (sync + async) | ASY-11.b | None |
| Implement in-process loopback transport | ASY-11.c | ASY-11.b |
| Implement strict + semantic comparators | ASY-11.d | ASY-11.b |
| Write P0 parity tests (local, INC_RECURSE, whole-file, delta) | ASY-11.e | ASY-11.c, ASY-11.d |
| Write P1 parity tests (daemon, compression, checksum, delete) | ASY-11.f | ASY-11.e |
| Wire into CI (`_test-features.yml`) | ASY-11.g | ASY-11.e |
| Write P2 parity tests (SSH mock) | ASY-11.h | ASY-11.f |

ASY-11.e is the gate for ASY-12. The async feature cannot flip to
default-on until ASY-11.e passes on all three CI platforms
(Linux/macOS/Windows).

## 8. Success criteria

1. P0 parity tests pass with strict byte-level comparison on
   Linux, macOS, and Windows.
2. Zero regressions introduced in existing golden tests or interop
   harness.
3. Test wall time < 60 seconds for the P0 subset on CI runners.
4. Any future async boundary change that alters wire output fails the
   parity test before reaching master.

## 9. Risks and mitigations

| Risk | Impact | Mitigation |
|------|--------|------------|
| Async buffering differs from sync | Wire divergence | `WireCapture` intercepts at the raw transport level, before any buffering layer. Both paths use identical buffer sizes. |
| Tokio task scheduling non-determinism | Flaky tests | In-process loopback with synchronous-like duplex channel. No real network. Keepalives stripped. |
| Test requires both feature paths compiled | Increased CI build time | Feature is additive (async depends on sync types). Single `--features async` flag enables both. Incremental compilation keeps overhead < 30s. |
| Coalescing normalization hides real bugs | False negative | Normalization is limited to MSG_INFO only. DATA, flist, and delta frames use strict comparison with zero normalization. |
