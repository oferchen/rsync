# SSH Transport: Explicit Backpressure Controls

Tracking issue: #1892. Followup to `docs/design/ssh-transport-async-io-eval.md`
(the umbrella eval), `ssh-async-default-linux.md` (#1890), and
`ssh-decouple-delta-from-socket-read.md` (#1891). This document
specifies the user-visible knob (`--ssh-max-in-flight-bytes`) and the
internal mechanism that enforces it.

## 1. Why an explicit knob

The umbrella eval and #1891 together leave back-pressure implicit:

- Network -> reader: kernel TCP receive window and pipe buffer.
- Reader -> applier: bounded `crossbeam-channel` queue at the
  multiplex-demux output (#1891 section 5.2).
- Applier -> disk: SPSC pipeline
  (`crates/transfer/src/pipeline/spsc.rs`).

All three are sized by defaults baked at compile time. The umbrella
eval's hybrid recommendation gives the SSH transport a tokio runtime
on Linux; the runtime amortises the buffering across many concurrent
connections in the daemon shape (option (a)). On the daemon, the sum
of per-connection in-flight buffers is the metric that matters:
operators want a cap on total transport memory, not on each
connection separately.

`--ssh-max-in-flight-bytes=N` is that cap, per connection. It is the
single number that sets:

- The reader -> applier queue depth derived from #1891 section 5.2.
- The multiplex writer's outstanding-bytes ceiling on the *send*
  side.
- The advisory hint published to the bench harness so trigger C in
  `ssh-async-default-linux.md` section 3.3 can be measured.

Without an explicit knob, the only path to tune memory pressure is
recompilation. With the knob, an operator can shrink memory at the
cost of throughput (small N) or accept more memory for higher
high-RTT throughput (large N) without rebuilding.

## 2. CLI surface

```
--ssh-max-in-flight-bytes=N
```

Accepts `N` in the same suffix grammar as
`--max-size` and `--min-size` (bytes, optional `K`/`M`/`G` suffix).
Default value: `4M` (4 MiB). Rationale in section 4 below.

Alias env var: `OC_RSYNC_SSH_MAX_IN_FLIGHT_BYTES=N`. Matches the
existing `OC_RSYNC_SSH_NET` convention from
`crates/rsync_io/src/ssh/connect.rs:446`.

CLI flag wins over env var. Both default to `4M`.

The flag is feature-gated on `async-ssh` because the back-pressure
machinery is part of the async transport (the sync transport
back-pressures on socket reads, with no separate buffer to size).
Without `async-ssh`, passing the flag prints a warning and is
ignored; the warning text names the feature.

Documented in:

- `crates/cli/src/frontend/arguments/parsed_args/mod.rs` alongside
  the existing `--ssh-*` flags (lines 559 - 580).
- `xtask/src/commands/docs/` man-page generator.

## 3. Internal: counting semaphore at the multiplex writer

The mechanism is a counting semaphore that tracks "bytes outstanding
on the wire and in transport-side buffers". The cap is N. Three
sites consume from the semaphore; one releases.

Consumers:

1. **Multiplex writer enqueue**. Before
   `MultiplexWriter::write_frame` issues the underlying
   `AsyncWrite::write_all`, it `acquire`s `frame.len()` permits. If
   fewer than `frame.len()` permits are available, the write parks
   on the semaphore. This back-pressures the upstream rsync sender
   via the SSH pipe / TCP receive window.

2. **Reader -> applier queue at the receiving side** (#1891).
   `frame.len()` permits acquired when the frame is enqueued, held
   until the applier finishes processing and the bytes are committed
   to disk (or released to the SPSC pipeline if the SPSC ownership
   transfer is the commitment point).

3. **In-flight bytes inside the SPSC pipeline**. If the SPSC slot
   currently in the disk-commit thread's hand carries bytes that
   were originally counted against the semaphore, those bytes
   remain in-flight until written. The SPSC implementation owns the
   release point.

Releases:

- After `MultiplexWriter::write_frame` completes the underlying
  `write_all`, it releases `frame.len()` permits.
- After the applier dispatches a `MSG_DATA` frame and the bytes are
  either committed to disk or transferred to the SPSC pipeline
  (whichever is the agreed boundary), it releases `frame.len()`
  permits.

```rust
struct InFlightSemaphore {
    /// Total permits. Equal to --ssh-max-in-flight-bytes.
    capacity: usize,
    /// Permits available.
    available: tokio::sync::Semaphore,
}

impl InFlightSemaphore {
    fn new(capacity: usize) -> Self {
        Self {
            capacity,
            available: tokio::sync::Semaphore::new(capacity),
        }
    }

    async fn acquire(&self, bytes: usize) -> InFlightPermit {
        let bytes = bytes.min(self.capacity);
        let permit = self.available.acquire_many(bytes as u32).await
            .expect("semaphore never closed");
        InFlightPermit { _permit: permit }
    }
}

struct InFlightPermit {
    _permit: tokio::sync::SemaphorePermit<'_>,
}
// Dropping the permit releases the bytes.
```

`tokio::sync::Semaphore` chosen because the multiplex writer is
async; the applier consumes via a sync facade
(`Handle::current().block_on(sem.acquire(...))` is forbidden by
migration plan R3; instead the applier accepts permits handed off
from the reader thread, where the reader thread - already async
under #1890 - acquires them).

A purely sync alternative is `crossbeam-utils::sync::WaitGroup` or
a plain `std::sync::Mutex<usize>` plus `Condvar`; rejected because
the multiplex writer side is async by the time this design ships
(after #1890 flips the Linux default).

### 3.1 Daemon vs CLI scope

On the daemon (umbrella eval option (a)), each `SshConnection`
owns its own `InFlightSemaphore`. The flag value applies *per
connection*. The daemon operator wanting a process-wide cap
multiplies by the expected fan-out; this design does not add a
process-wide cap because the operator's natural cap is the
listener's connection limit
(`daemon-tokio-async-listener-impl.md`).

On the CLI (umbrella eval option (b)), one connection per
invocation; the flag is the process cap.

## 4. Default value: 4 MiB

The 4 MiB default is picked to match typical TCP window-scaled
receive buffer sizes on modern Linux:

- Linux default `net.core.rmem_default` is 208 KiB; `rmem_max` is
  4 MiB on most stock kernels. With TCP window scaling, the
  per-socket receive buffer reaches that ceiling under bandwidth
  pressure.
- 4 MiB also matches the kernel pipe buffer ceiling reachable
  via `fcntl(F_SETPIPE_SZ)` without `CAP_SYS_RESOURCE`.

Setting the semaphore equal to the typical TCP receive buffer
sizes the in-flight bytes envelope to one "network round" of data:
enough to saturate the BDP on a typical link (1 Gbps * 30 ms = 3.75
MiB, just under 4 MiB), without absorbing more than the kernel
already buffers per-socket.

For the high-RTT trigger row (`SLOW_LINK_NS_PER_BYTE = 200`,
~50 ms RTT) the BDP is small (1 Mbps * 50 ms = 6.25 KiB);
4 MiB is comfortably enough.

For the fan-out daemon row (100 concurrent connections), the
aggregate in-flight cap is 400 MiB. Operators concerned about
that aggregate can shrink the per-connection cap to e.g. 1 MiB
without losing performance on individual high-RTT transfers
(the BDP fits in 1 MiB on most links of interest).

## 5. Interaction with #1891 queue depth

The reader -> applier queue from #1891 section 5.2 is one
component of the in-flight bytes envelope. Translation:

```
queue_depth = max(1, max_in_flight_bytes / typical_frame_size)
```

where `typical_frame_size = MplexWriter::DEFAULT_MAX_FRAME_SIZE =
8192` (from `crates/protocol/src/multiplex/writer.rs:88`).

With the 4 MiB default, queue depth = `4 * 1024 * 1024 / 8192 =
512` frames. This is much larger than the default depth of 4 from
#1891 section 4.2; the two are reconciled as follows.

The queue depth in #1891 is a *count* bound. The semaphore here is
a *byte* bound. The applier respects both: it dequeues when the
queue has an entry, but the *write* / *apply* commits release
semaphore permits in byte units. A 16 MiB literal frame consumes
16 MiB of semaphore permits regardless of queue depth.

The combined bound is: at most `min(queue_depth, ceil(N / 1)) =
queue_depth` *frames* in flight, summing to at most `N` *bytes*.
The semaphore is the strict bound; the queue is a smaller
capacity bound that prevents pathological per-frame fragmentation.

For implementation, the queue from #1891 step 4 (configurable
depth) becomes a derived value of `N`. Operators set `N`; queue
depth follows.

## 6. Failure modes

### 6.1 Deadlock when N is smaller than one frame

A frame larger than N can never acquire its permits and blocks the
reader thread forever. The applier, waiting on the queue, also
blocks forever. Classic deadlock.

The multiplex protocol allows frames up to
`MAX_PAYLOAD_LENGTH = 16 MiB` (from
`crates/protocol/src/envelope/constants.rs:5`). Setting
N < 16 MiB risks the deadlock on any frame that uses the full
payload.

**Mitigation**: enforce `effective_N = max(user_N, MAX_PAYLOAD_LENGTH)`
at config-build time. The user's CLI value is preserved for logging
purposes; the semaphore is sized to `effective_N`.

The CLI parser logs a warning when `user_N < MAX_PAYLOAD_LENGTH`:

```
warning: --ssh-max-in-flight-bytes=N is below the multiplex max
frame size (16M). The effective cap is 16M; small frames will
still observe N-sized back-pressure on the cumulative inflight.
```

The wording explicitly says "the effective cap is 16M" so an
operator who set `--ssh-max-in-flight-bytes=512K` understands
that a single 8 MiB literal frame will still pass through.

### 6.2 Permit leak under panic

If the applier panics mid-frame, the `InFlightPermit` is dropped
and the permits are released. No leak. Verified by a panic-isolation
test (similar to the rayon bridge discipline named in
`tokio-spawn-blocking-rayon.md`).

If the reader panics with permits held, the same Drop logic
releases. The applier sees `Recv(Disconnected)` on the queue and
exits cleanly.

### 6.3 Cancellation during permit hold

Tokio's `Semaphore::acquire_many` future is cancel-safe. If the
async reader task is cancelled mid-acquire, no permits are taken.
If cancelled after acquire but before release, the `InFlightPermit`
drops on cancellation cleanup and releases the permits.

The umbrella eval section 3.3 risk 2 (`spawn_blocking` cancellation
gap) does not apply because the applier on the sync side holds
permits handed off by the reader, not acquired directly. The
reader's cancellation propagates by dropping the permit before the
applier sees it.

### 6.4 N = 0 or negative

The CLI parser rejects `N <= 0` with an exit-code-1 argument error.
Section 6.1's mitigation kicks in for any positive N below
`MAX_PAYLOAD_LENGTH`.

### 6.5 Memory-pressure cascade

When N is set very small (e.g. 64 KiB), section 6.1 raises
`effective_N` to 16 MiB and back-pressure becomes loose. Operators
who genuinely need tighter memory caps must address it at a higher
layer (`ulimit`, cgroups), not via this flag. The warning in 6.1
makes this clear.

## 7. Five-step implementation plan

1. **Add the CLI flag and env var.** Plumb
   `--ssh-max-in-flight-bytes=N` and
   `OC_RSYNC_SSH_MAX_IN_FLIGHT_BYTES=N` through
   `crates/cli/src/frontend/arguments/` into `CoreConfig`. Default
   `4 * 1024 * 1024`. Gate: a parser test for the suffix grammar
   and a precedence test (CLI > env > default).

2. **Implement `InFlightSemaphore` in `rsync_io`.** Add the type
   from section 3 to a new module
   `crates/rsync_io/src/ssh/inflight.rs`. Gate: unit tests for
   acquire / release / drop, plus a panic-isolation test that
   confirms permits are returned when the holder panics.

3. **Wire the semaphore into the multiplex writer.** Hook
   `InFlightSemaphore::acquire` into
   `MultiplexWriter::write_frame`. Release on completion. Gate: an
   integration test sends a 16 MiB frame through a 4 MiB-capped
   writer and confirms the effective cap rises to 16 MiB (per
   section 6.1). A second test confirms two concurrent 2 MiB frames
   serialise through a 3 MiB cap.

4. **Wire the semaphore into the #1891 reader -> applier path.**
   The reader acquires permits on dequeue from the SSH read and
   hands them to the applier alongside the frame. Applier releases
   on apply completion. Gate: a memory cap test confirms that
   sustained 16 MiB literal frames keep the resident bytes under
   `effective_N + slack` (slack = one frame for the in-flight
   acquire).

5. **Derive #1891's queue depth from the flag and document the
   interaction.** Replace the standalone `OC_RSYNC_SSH_QUEUE_DEPTH`
   env var from #1891 step 4 with the derivation in section 5.
   Update `ssh-async-default-linux.md` section 3.3 trigger C to
   measure the bench with the default `--ssh-max-in-flight-bytes=4M`.
   Gate: a bench sweep over `[1M, 4M, 16M, 64M]` confirms 4 MiB is
   the wall-clock knee on the high-RTT trigger row, with smaller
   values losing throughput and larger values not adding measurable
   wins.

Steps 1 - 2 can land before #1891. Steps 3 - 4 require #1891 to be
in. Step 5 lands together with the bench-driven decision in
`ssh-async-default-linux.md` step 4.

## 8. Trigger conditions

This design lands when all hold:

- **Trigger 1**: #1891's reader -> applier split has shipped (or
  ships in the same PR series). The semaphore is most useful when
  it scopes both the writer and the receiver-side queue; landing
  it on the writer alone is half the win.
- **Trigger 2**: the bench in step 5 confirms 4 MiB is the
  reasonable default. If the bench picks a different knee (e.g.
  the high-RTT trigger row wants 16 MiB), the default changes; the
  flag's existence does not.
- **Trigger 3**: no platform regression in the queue derivation
  on macOS / Windows once the async transport ships there.
  Until then, the flag is honoured on Linux only (matching
  `ssh-async-default-linux.md`); on other platforms it is parsed,
  logged, and ignored.

If trigger 2 picks a much larger default (>= 64 MiB), revisit
section 4: the rationale stops matching the kernel TCP buffer
sizing and the design's premise (couple the cap to one BDP)
needs restating. Likely the bench shows the BDP argument holds
and the default lands within an order of magnitude of 4 MiB.

## 9. Open questions deferred to implementation

- Whether the flag should accept `--ssh-max-in-flight-bytes=0`
  to mean "unbounded" (i.e. only the kernel buffers apply). Rejected
  in this design (section 6.4); revisit if operators request it.
- Whether the daemon should expose an aggregate cap across all
  connections. Defer to a separate design once the per-connection
  cap is shipped and operator feedback names the need.
- Whether the bytes accounted include the multiplex 4-byte header.
  Lean towards yes (the header occupies pipe-buffer bytes too); the
  step 3 PR resolves on review.
