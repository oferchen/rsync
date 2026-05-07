# IOCP high-concurrency stress test (#1871)

This note specifies the stress harness that gates the Windows IOCP
disk-commit path before it is declared production-ready. The harness
sits next to the existing batch-level unit tests in
`crates/fast_io/src/iocp/disk_batch.rs` and the policy-plumbing tests in
`crates/transfer/src/disk_commit/tests.rs`. No wire-protocol changes; no
upstream-compatible behaviour changes.

## 1. Scope

- Target: `IocpDiskBatch` (#1717 foundation work) and the symmetric
  `Writer::Iocp` dispatch wired in #1928. End-to-end coverage runs
  through `spawn_disk_thread` to mirror the production call graph.
- Out of scope: pump-based dispatch (`crates/fast_io/src/iocp/pump.rs`),
  the overlapped TCP socket layer (`iocp/socket.rs`), and CLI flag
  surfaces. Those belong to #1898 / #1899 respectively.

## 2. Workload

- 10000 concurrent file commits driven through the disk-commit thread.
- 32 producer worker threads feed `FileMessage::Begin` items into the
  bounded SPSC channel that fronts `spawn_disk_thread`. The single
  consumer is the disk thread itself; the 32 producers exercise the
  reorder buffer and the bounded channel back-pressure
  (`crates/transfer/src/reorder_buffer.rs:55-64`).
- File-size mix: 70% small (4-64 KB, single submission per file), 25%
  medium (256 KB-4 MB, multi-submission with `concurrent_ops = 4`), 5%
  large (8-32 MB, drains at `COMPLETION_DRAIN_BATCH = 64` granularity).
- Payloads are deterministic (seeded `ChaCha8Rng`) so per-file digests
  are recomputed from the seed at verification time without retaining
  the bytes in memory.

## 3. Metrics

- Throughput in MiB/s aggregated over the run, broken out by size class.
- Completion-port queue depth, sampled once per `GetQueuedCompletionStatusEx`
  drain via a counter on `IocpDiskBatch::in_flight.len()` exposed by a
  test-only accessor; the harness records min, p50, p99, max per drain.
- Submission-to-completion latency captured per op from the
  `OverlappedOp` submit timestamp to the matching drain entry; reported
  as p50 / p99 / p999.
- Error counts by class: `ERROR_DISK_FULL`, `ERROR_INSUFFICIENT_BUFFER`
  (#1930 in flight), `ERROR_INVALID_PARAMETER` (#1929 in flight),
  `WriteZero`, partial-write resubmission count.
- Handle-leak probe: `Process32First`-based handle count snapshotted
  before and after the run; tolerated delta is zero.

## 4. Failure injection

- Disk-full: pre-fill a temp directory mounted on a small VHDX so the
  Nth write hits `ERROR_DISK_FULL`. Assert the disk thread aborts
  cleanly and that every queued completion drains before the batch
  drops; no port handle leaks.
- ENOMEM / `STATUS_INSUFFICIENT_RESOURCES`: lower the test process's
  working-set ceiling via `SetProcessWorkingSetSizeEx` so non-paged-pool
  pressure forces overlapped submissions to fail. Assert the failure
  path surfaces a typed `IocpError` instead of hanging on an unreaped
  completion.
- `ERROR_INSUFFICIENT_BUFFER` retry: cap the drain entry array to a
  size below `in_flight.len()` and confirm the buffer-growth path
  (tracked under #1930) does not regress.
- Synthetic short writes: a custom file-system filter (or a wrapped
  `WriteFile` shim under test cfg) returns half the requested bytes so
  the resubmission walker exercises the unwritten-tail path.

## 5. Pass criteria

- Every committed file matches its expected per-seed digest.
- No process-handle delta at exit; `IocpDiskBatch::Drop` finalises with
  zero pending completions.
- p99 submission-to-completion latency below 50 ms on the
  `windows-latest` runner under the documented 32-thread fan-in.
- Throughput within 10% of a baseline captured by #1899 for the same
  workload mix; regressions trip the harness.
- All injected failures surface as typed errors and the disk thread
  exits via the documented shutdown path.

## 6. Test placement and CI

- File: `crates/transfer/tests/disk_commit_iocp_stress.rs`, gated by
  `#[cfg(all(target_os = "windows", feature = "iocp"))]`.
- Marked `#[ignore]` by default; opted in by a nightly job
  (`.github/workflows/iocp-stress.yml`) using
  `cargo nextest run --features iocp -- --ignored` on
  `windows-latest`. The PR-time matrix entry tracked under #1900
  remains unchanged.
- Failure-injection cases each run as their own `#[test]` so a single
  injected fault failing does not mask the rest. The high-volume
  10000-file body is one test that runs after the injection cases pass.

## 7. References

- IOCP backend: `crates/fast_io/src/iocp/{disk_batch,completion_port,overlapped,error}.rs`.
- Disk-commit pipeline:
  `crates/transfer/src/disk_commit/{thread,process,writer}.rs`.
- Reorder buffer: `crates/transfer/src/reorder_buffer.rs`.
- Existing wiring design: `docs/design/iocp-transfer-pipeline-wiring.md`.
- Issue refs: #1717 (foundation), #1928 (overlapped socket layer
  cementing `OverlappedOp` invariants), #1899 (IOCP benchmark and
  status surfaces), #1929 (`writer_from_file` reopen flow, in flight),
  #1930 (`ERROR_INSUFFICIENT_BUFFER` retry growth, in flight), #1871
  (this harness).
