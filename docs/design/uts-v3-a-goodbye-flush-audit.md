# UTS-V3.A cluster-A goodbye-flush regression audit

Tracks UTS-V3.A.E.1 / .E.2 / .E.5 / .E.6 / .E.7 / .E.8. Companion to
the already-completed UTS-V3.A.SHUTDOWN root-cause note (#4344) and the
UTS-V3.A.E.5.a/.b commit-set + repro fixture work (#4468, #4469).

The cluster comprises four upstream-testsuite failures whose wire byte
streams all terminate at roughly the same byte position - well before
the daemon-sender's NDX_DONE goodbye envelope reaches the receiver.
This is a TCP teardown-ordering bug at the daemon-sender, not a
codec-level problem: the receiver reports
`connection unexpectedly closed (N bytes received so far) [receiver]`.

## 1. Cutoff position table

Numbers captured from upstream-testsuite runs on the Linux host
(192.168.21.226) and from container tcpdump in `rsync-profile`. The
cluster spans two distinct sub-clusters: one near 615 KB (single
small-file `-zz` pull) and one near 2.25 MB (the larger transfers).

| Test                                | Run date   | Cutoff byte | Sub-cluster |
|------------------------------------:|------------|------------:|------------:|
| daemon-gzip-download                | 2026-06-14 |     615 161 | A1 (~615 KB)|
| daemon-gzip-download (earlier run)  | 2026-06-12 |     612 413 | A1 (~615 KB)|
| daemon-refuse-compress              | 2026-06-14 |   2 251 130 | A2 (~2.25 MB)|
| alt-dest --copy-dest (lsh.sh)       | 2026-06-14 |   2 251 388 | A2 (~2.25 MB)|
| batch-mode daemon --write-batch     | 2026-06-14 |   2 251 135 | A2 (~2.25 MB)|

**Consensus boundary**: ~2.25 MB for the A2 cluster (spread of 258
bytes across three transports), and ~614 KB for the A1 daemon-gzip-
download case. Same root cause: the daemon-sender's last few hundred
bytes of MSG_DATA + NDX_DONE envelope are queued in the multiplex
writer buffer and never drained before the socket FIN/RST.

The 612413 vs 615161 spread in the daemon-gzip-download case reflects
small variation in `-zz` block-flush boundaries within the upstream
deflate stream - not a different bug.

## 2. Existing flush / drop inventory

Generator transfer hot path (`crates/transfer/src/generator/`):

| Site                                         | Pattern                            |
|----------------------------------------------|------------------------------------|
| `transfer/transfer_loop.rs:146`              | `flush_with_count(writer)?`        |
| `transfer/transfer_loop.rs:175`              | `flush_with_count(&mut *writer)`   |
| `transfer/transfer_loop.rs:214`              | `flush_with_count(&mut *writer)`   |
| `transfer/transfer_loop.rs:237`              | `flush_with_count(&mut *writer)`   |
| `transfer/transfer_loop.rs:367`              | `flush_with_count(writer)?` (dry)  |
| `transfer/transfer_loop.rs:600`              | `flush_with_count(&mut *writer)`   |
| `transfer/orchestrator.rs:65`                | `writer.flush()?` (server mode)    |
| `transfer/orchestrator.rs:152-162`           | `handle_goodbye_with_finalizer` driving `finalize_compression` between write and read |
| `transfer/orchestrator.rs:170`               | `writer.finalize_compression()` (idempotent defense-in-depth) |
| `transfer/stats.rs:45`                       | `writer.flush()?`                  |
| `transfer/goodbye.rs:146`                    | `writer.flush()` after NDX_DONE    |
| `protocol_io.rs:64 / 84 / 595`               | mid-loop explicit `writer.flush`   |

Daemon teardown (`crates/daemon/src/`): the connection thread relies on
`shutdown(Write)` + read-drain to clear queued bytes (see PR #5740 and
#5737). The transfer-graph `ServerWriter` itself is dropped by the
generator orchestrator returning, with no explicit `shutdown_send_side`
call - the daemon teardown closes the socket from outside.

There is no `drop(writer)`-only pattern relied on for correctness on
the generator hot path: every wire-byte-emitting site is bracketed by
`flush_with_count`. The gap is at the **boundary between the
`ServerWriter` Drop and the kernel-level socket close**: the
multiplex writer's last flush happens before goodbye, the
`finalize_compression` runs inside `handle_goodbye_with_finalizer`,
but there is no daemon-sender-side equivalent of upstream's
`io_flush(FULL_FLUSH)` *after* `read_final_goodbye` returns. The
defense-in-depth call at `orchestrator.rs:170` runs *only* on success;
any early-close branch returns without re-flushing.

## 3. Wire byte ~2,251,000 decoding

Multiplex framing is `MSG_DATA(tag=7)` headers + 32 KB IO buffer. At
2 251 130 the stream sits ~17 KB past the 2 097 152 (2 MiB) boundary -
i.e. inside the 69th 32 KB MSG_DATA chunk after multiplex start. For
batch-mode + daemon-gzip-upload the wire encodes deflate-compressed
file data; the boundary does not coincide with a flist record nor a
keep-alive frame, ruling out a per-chunk encoder fault.

The cluster cut at the **last MSG_DATA frame the sender enqueued
before transitioning to the goodbye phase**. The generator emits a
final `flush_with_count` on phase transition, then writes
`MSG_INFO`/`MSG_STATS` envelopes (`stats.rs:45`), then enters
`handle_goodbye_with_finalizer`. The cut byte lands at the *kernel
send-buffer tail when the daemon's TCP socket is shut down* - the user-
space writer reports success, the bytes never leave the host.

The 258-byte spread across A2 transports is the difference between
last-MSG_DATA-frame boundaries under different codec/transport combos.

## 4. Bisect-candidate commit set

`git log --oneline --since='2026-05-15' crates/transfer/src/generator/
 crates/daemon/src/` identifies the daemon-teardown commits as the
plausible regression introducer set. All ride on the same root area
(socket lifecycle around the generator transfer-loop exit):

| SHA       | PR    | Date       | Summary                                              |
|-----------|-------|------------|------------------------------------------------------|
| 6955fd881 | #5737 | 2026-06-12 | yield to kernel + SO_LINGER on accepted TCP socket   |
| 4cbd5146d | #5740 | 2026-06-12 | replace teardown sleep with SO_LINGER + drain-close  |
| 4633940fb | -     | 2026-06-13 | unhang upstream daemon testsuite                     |
| b3ef5e0c4 | -     | 2026-06-14 | drain peer goodbye without half-close                |
| cbb4128d7 | #5765 | 2026-06-14 | drain peer goodbye without half-close (merged)       |
| 32e465f29 | -     | -          | flush -zz codec state before daemon-sender goodbye   |
| 54a5b7f8d | -     | -          | flush zlib end-of-stream before daemon-sender close  |

**Single most likely culprit**: PR #5740 (`4cbd5146d`). It replaced a
tactical 50 ms `thread::sleep` with the structural drain-then-close
pattern, but the new pattern issues `shutdown(Write)` from the daemon
*before* the in-flight write half of the `ServerWriter` has been
guaranteed-drained by the transfer subsystem. The flush in `goodbye.rs`
runs against the multiplex `ServerWriter`, but the underlying socket
shutdown can race the kernel write-queue drain on the loopback when
the writer buffer holds a partial frame at the cluster cut point.

PR #5765 (`cbb4128d7`) was the partial mitigation; it removed the half-
close but did not introduce the missing explicit `drain-before-shutdown`
barrier inside the daemon-sender exit path.

## 5. Proposed drain barrier API

Add an explicit drain-before-shutdown barrier into the daemon-sender
exit path so the user-space `ServerWriter` is fully flushed *and* the
kernel-level send buffer is drained before any FIN is emitted.

Two-step shape:

```rust
// crates/transfer/src/writer/server.rs
impl<W: Write + AsRawFd> ServerWriter<W> {
    /// Drain user-space buffers (multiplex frame + codec trailer)
    /// before returning. Idempotent. Surfaces I/O errors except for
    /// peer-already-closed which is reported as Ok(()).
    pub fn flush_all_pending(&mut self) -> io::Result<()> { ... }

    /// Half-close the send side of the underlying socket after the
    /// final user-space byte hit the kernel. Bounded by `timeout`
    /// (default: 5s for loopback, configurable via daemon-side knob).
    /// Surfaces a typed error on timeout - never silently swallows.
    pub fn shutdown_send_side(self, timeout: Duration) -> io::Result<()> { ... }
}
```

Placement (generator orchestrator post-goodbye):

```rust
self.handle_goodbye_with_finalizer(reader, writer, ..., |w| {
    w.finalize_compression().or_else(early_close_is_ok)
})?;

// NEW: explicit drain barrier replacing the implicit Drop-time close
writer.flush_all_pending()?;
// Daemon-sender path only; client-mode keeps stdio open for SSH.
if !self.config.connection.client_mode {
    writer.shutdown_send_side(Duration::from_secs(5))?;
}
```

The barrier is a no-op for SSH client mode (parent process owns
stdio teardown). For daemon-sender it replaces the SO_LINGER + drain
sequence with an explicit per-direction shutdown that is observable
from the wire byte count.

## 6. Test plan

Regression nextest in `crates/transfer/tests/`:

- `daemon_sender_emits_final_byte_before_fin` - drive a daemon-sender
  end-to-end (loopback TcpListener), assert that wire byte count at
  the receiver equals the byte count reported by the sender's
  `flush_all_pending` return *before* FIN reaches the kernel.
- Four cluster-A repros wired in nextest using the deterministic
  fixture already built in UTS-V3.A.E.5.b (#4469) - one per cluster-A
  test, asserting NDX_DONE envelope reaches the receiver.

CI integration:

- Wire goodbye-flush smoke into `IFX-15.a` upstream-testsuite CI cell.
- Add 100-iteration stress run for the deterministic repro in nightly.

Sequence-of-completion (cross-ref): #4358 → #4359 → #4360.

## 7. Cross-references

- UTS-V3.A parent (#4291, completed)
- UTS-V3.A.SHUTDOWN root-cause (#4344, completed) - declared this a
  shutdown-ordering race
- UTS-V3.A.E.5.a/.b (#4468 / #4469, completed) - commit set + repro
  fixture
- This audit closes #4352, #4353, #4357 and specs the work for #4358
  (drain barrier impl), #4359 (regression test), #4360 (re-run cluster
  A tests)
- Sub-cluster A1 was first surfaced as UTS-9.REOPEN (#3958-#3962); the
  partial mitigation there (`UTS-9.REOPEN.4`) did not generalize to A2

## 8. Status

Audit-only. No code changes. Subsequent tasks:

- UTS-V3.A.E.7 (#4358): implement `flush_all_pending` +
  `shutdown_send_side`, wire into daemon-sender exit
- UTS-V3.A.E.8 (#4359): nextest regression for final-byte-before-FIN
- UTS-V3.A.E.9 (#4360): re-run all four cluster-A upstream tests, gate
  cluster closure on green CI + 14-day bake on `IFX-15.a` cell
