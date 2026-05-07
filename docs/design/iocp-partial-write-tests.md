# IOCP partial-write resubmission tests (#1931)

This note specifies the regression coverage that closes task #1931:
unit-level proof that the IOCP disk-commit batch handles short
`WriteFile` completions without losing or duplicating bytes. The wiring
itself landed with #1868; the underlying writer infrastructure landed
with the IOCP socket and buffer work in #1928. What remains is targeted
test coverage so the partial-write path stays honest as the surrounding
code evolves.

## 1. Scope

- IOCP writer surface under test: `IocpDiskBatch::submit_write_batch`
  and `drain_completions` in `crates/fast_io/src/iocp/disk_batch.rs`,
  and the `IocpWriter` overlapped writer in
  `crates/fast_io/src/iocp/file_writer.rs`. Both arrived as part of
  #1928 and are exercised end-to-end by the disk-commit pipeline.
- Out of scope: disk-full coverage (#1932), high-concurrency stress
  (#1871), CI matrix wiring (#1900), pump-based dispatch (#1898).

## 2. Partial-write semantics

`WriteFile` for an overlapped handle is permitted to complete with a
byte count strictly less than the requested length. The kernel
contract is "best effort up to the buffer length", and short writes
are observable in practice when:

- the write crosses a quota boundary or filesystem extent allocation
  step;
- the destination volume reports backpressure from a filter driver;
- the I/O manager splits a large `WriteFile` across cache flushes.

The batch handles this by holding every in-flight op in
`Vec<Pin<Box<OverlappedOp>>>`, matching completions by `OVERLAPPED *`
identity, and rescheduling the unwritten tail at the original offset
plus the bytes the kernel acknowledged. Zero-byte completions surface
as `io::ErrorKind::WriteZero` to abort the transfer rather than spin.

## 3. Test harness

Real partial writes are non-deterministic. Two harness pieces drive the
behaviour from a unit test:

- **Small ring-buffer sink.** A test-only `MockOverlappedSink` trait
  feature-gated under `#[cfg(test)]` injects a buffer-bounded
  `WriteFile` shim. The sink wraps an in-memory ring with a fixed
  capacity (default 4 KB) and acknowledges only the bytes that fit
  before the next drain. Subsequent submissions resume from the
  recorded offset. The shim records every `(offset, len, written)`
  triple so assertions inspect the resubmission shape directly.
- **Deterministic completion injector.** A `CompletionInjector` queues
  synthetic completion entries that the drain loop consumes via the
  same code path as a real `GetQueuedCompletionStatusEx` reap. This
  keeps the test single-threaded and avoids racing the kernel's I/O
  manager. The injector lives next to `disk_batch::tests` and is
  reused by future tasks (#1932 disk-full, #1871 stress).

Test cases the harness drives:

- Single 1 MB submission that the sink truncates to 4 KB chunks;
  assert `bytes_written == 1 MB` and that resubmissions walk the tail
  with strictly increasing offsets.
- Two interleaved submissions of 256 KB each; assert ordering by
  `OVERLAPPED *` identity, not submission index, since IOCP completes
  out-of-order within a file.
- Zero-byte completion on a non-empty submission; assert
  `ErrorKind::WriteZero` and that no further submissions enter the
  ring.
- Drain that returns `ERROR_INSUFFICIENT_BUFFER` on the first call;
  assert the dynamic-growth path lands the next reap (parity with
  #1930).

## 4. Memory pressure scenario

The harness models memory pressure with a sink configured for a 64-byte
ring against a 1 MB submission. Each drain ack 64 bytes; the loop
performs 16 384 resubmissions. Assertions:

- `total_written` matches the source payload byte-for-byte through a
  digest computed over the assembled output.
- The in-flight queue depth never exceeds the configured
  `concurrent_ops` cap. The sink never sees more than that many
  outstanding ops simultaneously.
- `Drop` of `IocpDiskBatch` mid-transfer drains every pending op
  before returning, leaving zero leaked `OverlappedOp` allocations.
  The test uses a `Weak` to a sentinel inside the buffer to confirm
  reclamation.
- A peak-RSS check against the test process: the resident set after
  the run is within 10 percent of the pre-run baseline. The ring
  buffer's bounded size prevents unbounded queueing.

## 5. References

- Code under test: `crates/fast_io/src/iocp/disk_batch.rs`,
  `crates/fast_io/src/iocp/file_writer.rs`,
  `crates/fast_io/src/iocp/overlapped.rs`.
- Wiring point: `crates/transfer/src/disk_commit/process.rs`,
  `crates/transfer/src/disk_commit/writer.rs`.
- Issues: #1868 (wiring closed), #1898, #1929, #1930 (in-flight
  IOCP work), #1931 (this task), #1932, #1871, #1900 (downstream).
