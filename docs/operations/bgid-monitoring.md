# BGID monitoring for long-lived daemons

Buffer Group IDs (bgids) name the kernel-side provided buffer rings
(`IORING_REGISTER_PBUF_RING`) used by the io_uring fast path. The id is
a 16-bit field (`u16`), so the namespace holds exactly 65 536 values.
A long-lived daemon that churns through many sessions will allocate and
release bgids over its lifetime; if sessions leak bgids or the churn
rate exceeds the recycling rate, the namespace can exhaust and force
sessions onto the slower non-registered receive path.

This guide covers the warning signals, the metrics available for
monitoring, and the remediation steps for bgid exhaustion.

## 1. Warning signals

### Namespace pressure (50 % occupancy)

```
WARN fast_io::buffer_ring: io_uring bgid occupancy crossed 50% of the 16-bit namespace
     in_flight=32768 namespace=65536
```

Fires when the number of concurrently allocated bgids exceeds 32 768
(half the namespace). Throttled to at most once per 30 seconds so it
does not flood logs under sustained pressure. This is an early warning -
there is still headroom, but the daemon is running hotter than expected.

### Namespace exhaustion

```
WARN fast_io::buffer_ring: BGID space exhausted, falling back to non-registered receive path
     fresh_used=65536 free_list_len=0 exhausted_count=17 in_flight=65535 peak_used=65535
```

Fires when `BgidAllocator::allocate` returns `BgidAllocError::Exhausted`
- all 65 536 bgids have been issued and none are available in the
free-list. Throttled to at most once per 60 seconds. Each warning
includes:

- `fresh_used` - monotonic counter value (always 65 536 at exhaustion).
- `free_list_len` - number of recycled bgids available (always 0 at this
  point).
- `exhausted_count` - cumulative exhaustion events since process start.
- `in_flight` - bgids currently checked out.
- `peak_used` - high-water mark for concurrent bgid occupancy.

A rising `exhausted_count` across successive warnings indicates ongoing
pressure rather than a one-time spike.

## 2. Metrics API

All metrics are process-wide and accessible from any thread. They are
suitable for export to Prometheus, StatsD, or any pull-based monitoring
system.

### Individual counters

| Function | Returns | Description |
|----------|---------|-------------|
| `bgid_exhausted_count()` | `u64` | Cumulative exhaustion events. Monotonic. |
| `bgid_inflight()` | `u16` | Current bgids checked out (not recycled). |
| `bgid_peak_used()` | `u16` | High-water mark for concurrent occupancy. |
| `BgidAllocator::remaining()` | `u32` | Counter headroom + free-list entries. |

### Snapshot

`bgid_snapshot()` returns a `BgidSnapshot` struct with all four fields
sampled in a single call:

```rust
let snap = fast_io::bgid_snapshot();
log::info!(
    "bgid: exhausted={} in_flight={} peak={} remaining={}",
    snap.exhausted_count,
    snap.in_flight,
    snap.peak_used,
    snap.remaining,
);
```

### Per-session tracking

`BgidSessionStats` captures the baseline counters at session start and
computes the delta at session end:

```rust
let stats = fast_io::BgidSessionStats::new();
// ... run the session ...
let exhaustions = stats.exhaustions_since_start();
if exhaustions > 0 {
    tracing::warn!(
        exhaustions,
        delta = stats.in_flight_delta(),
        "session experienced bgid exhaustion",
    );
}
```

- `exhaustions_since_start()` - exhaustion events during this session.
- `in_flight_delta()` - net change in checked-out bgids. A positive
  value after session teardown suggests a leak.
- `start_in_flight()` / `current_in_flight()` - absolute counts at
  session start and now.

## 3. Alerting recommendations

| Condition | Severity | Action |
|-----------|----------|--------|
| `bgid_exhausted_count() > 0` | Warning | Investigate session churn rate. |
| `bgid_inflight() > 32768` | Warning | Approaching namespace limit. |
| `exhausted_count` rising in successive snapshots | Critical | Active sessions are failing over to the slow path. |
| `BgidSessionStats::in_flight_delta() > 0` at session end | Warning | Possible bgid leak - rings not dropped on session close. |

## 4. Remediation

### Reduce concurrent sessions

Each daemon session with io_uring PBUF_RING support consumes one or
two bgids. The practical ceiling is approximately 32 000 concurrent
sessions per process. Use `--max-connections` to cap admission below
the namespace limit.

### Verify ring cleanup

Every `BufferRing` with `allocator_owned = true` returns its bgid to
the free-list on `Drop`. If `bgid_inflight()` keeps rising without
falling back after sessions close, a `BufferRing` instance is being
leaked (not dropped). Check for `Arc` cycles or `mem::forget` on ring
holders.

### Restart the daemon

If the namespace is truly exhausted with no leaks (all bgids are in
active use), the only recovery is to wait for sessions to end or
restart the process. The bgid namespace is per-process and resets on
restart.

### Monitor the free-list

A healthy daemon under steady-state churn shows `bgid_inflight()`
oscillating within a band and `BgidAllocator::remaining()` never
approaching zero. Sustained growth in `bgid_inflight()` without
corresponding session growth is the primary indicator of a leak.

## 5. Cross-references

| Topic | Location |
|-------|----------|
| BGID allocation architecture | `docs/architecture/bgid-lifecycle.md` |
| BGID exhaustion audit | `docs/audits/io-uring-bgid-exhaustion.md` |
| Namespace primer | `docs/audits/io-uring-bgid-namespace.md` |
| Allocator source | `crates/fast_io/src/io_uring/buffer_ring/allocator.rs` |
| Per-session stats source | `crates/fast_io/src/io_uring/buffer_ring/allocator.rs` (`BgidSessionStats`) |
