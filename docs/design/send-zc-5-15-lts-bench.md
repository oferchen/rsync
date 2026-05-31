# SZP-5 - SEND_ZC fallback overhead bench on 5.15 LTS

Date: 2026-06-01
Status: design
Predecessor: SZP-4 (per-feature kernel floor matrix)
Tracker: SZP-5

## 1. What we are measuring

When `iouring-send-zc` is compiled in but the kernel lacks
`IORING_OP_SEND_ZC` (5.15 LTS - Ubuntu 22.04, RHEL 9.x), the socket
writer probes the opcode, caches the negative result in an `AtomicI8`,
and falls back to `IORING_OP_SEND`. This bench quantifies whether that
probe-then-fallback path introduces any measurable overhead compared to
never attempting the probe at all.

The concern: `probe_send_zc()` allocates a 4-entry throwaway `IoUring`,
issues `IORING_REGISTER_PROBE`, and tears the ring down. On a kernel
that rejects the probe (or lacks the opcode), this is wasted work. If
the caching is correct (single `AtomicI8::load` on the hot path), the
amortized cost should be negligible. This bench verifies that
expectation under concurrent load.

## 2. A/B comparison

Two build configurations, identical binary otherwise:

| Variant | Cargo feature | Behavior on 5.15 |
|---------|---------------|------------------|
| **A - enabled** | `iouring-send-zc` on | `is_supported()` probes once, caches `false`, all sends use `IORING_OP_SEND` |
| **B - disabled** | `iouring-send-zc` off | Stub `is_supported()` returns `false` immediately; all sends use `IORING_OP_SEND` |

Both variants use `IORING_OP_SEND` for the actual data path - the only
difference is whether the initial probe syscall fires and whether the
`send_zc_active` field resolution traverses the real or stub code path.

## 3. Workload

Daemon-to-client transfer via `rsync://` over loopback, targeting the
concurrency range that production 5.15 deployments encounter:

| Tier | Concurrent transfers | File size | Total data |
|------|---------------------|-----------|------------|
| Light | 10 | 10 MiB each | 100 MiB |
| Medium | 100 | 10 MiB each | 1 GiB |
| Heavy | 1000 | 10 MiB each | 10 GiB |

Each tier runs 5 iterations. The daemon serves from a tmpfs module to
eliminate disk I/O variance. Clients pull via separate TCP connections
(no multiplexing across clients).

### Environment

- Kernel: 5.15.x (Ubuntu 22.04 HWE or RHEL 9.x stock kernel)
- Hardware: 4+ cores, 16+ GiB RAM, loopback (no NIC variance)
- Isolation: `taskset` to pin daemon and clients to separate core sets;
  `nice -20` for the daemon; no other network-intensive processes
- io_uring ring size: default (128 entries) for both variants

## 4. Metrics

### Per-transfer latency

- P50, P95, P99 wall-clock time from connection open to final byte
  received, measured at the client
- Jitter: standard deviation across transfers within a single iteration

### Aggregate throughput

- Total MiB/s across all concurrent transfers
- Reported as the mean of 5 iterations with 95% confidence interval

### Probe overhead (one-time cost)

- Wall-clock duration of `probe_send_zc()` on 5.15 (microseconds)
- Measured independently via a microbench calling `is_supported()` in a
  tight loop (first call = probe, subsequent = cached load)
- Expected: probe takes 10-50 us; cached path takes < 5 ns

### System-level

- CPU utilization (daemon process, `getrusage` before/after)
- Context switches (voluntary + involuntary)
- io_uring SQE submission count (should be identical between A and B
  after the initial probe ring teardown)

## 5. Expected outcome

The probe is a one-time cost cached in a process-wide `AtomicI8`. After
the first call to `is_supported()`, every subsequent call is a single
`Ordering::Relaxed` load - effectively free. The socket writer
construction resolves `send_zc_active = false` identically in both
variants. The data-path code (`submit_send_batch`) is identical.

Expected results:

- **Probe cost**: 10-50 us amortized over the process lifetime - rounds
  to zero at any meaningful transfer volume
- **Per-transfer latency**: indistinguishable between A and B (within
  measurement noise)
- **Aggregate throughput**: indistinguishable between A and B
- **SQE count**: identical (one extra ring setup/teardown for the probe
  in variant A, but this happens before any transfer begins)

## 6. Pass criteria

| Metric | Threshold |
|--------|-----------|
| Throughput delta (A vs B) | < 1% at all concurrency tiers |
| P99 latency delta | < 2% at all concurrency tiers |
| Probe wall-clock | < 100 us (one-time) |

If all three criteria pass, the `iouring-send-zc` feature is safe to
compile-in unconditionally on 5.15 LTS systems with no performance
penalty. The probe cost is negligible and the fallback path is identical
to the disabled path.

## 7. If fail

If throughput or latency delta exceeds the threshold:

1. **Profile the probe path** - is `IoUring::new(4)` expensive on 5.15?
   Some kernels have slow `io_uring_setup` due to mmap overhead. If the
   probe takes > 1 ms, that alone could explain startup latency.

2. **Check caching correctness** - verify with `strace` that
   `io_uring_setup` is called exactly once per process lifetime. A
   missing cache (re-probing per connection) would explain per-transfer
   overhead.

3. **Kernel version floor guard** - if the probe is inherently expensive
   on 5.15, add a `uname()` floor check before the opcode probe:
   ```
   if kernel_version < (6, 0) {
       SEND_ZC_SUPPORTED.store(-1, Ordering::Relaxed);
       return false;
   }
   ```
   This avoids the throwaway ring entirely on known-unsupported kernels.
   The risk is container runtimes that lie about `uname` - but those
   same runtimes would also fail the opcode probe anyway.

4. **Remove the probe attempt below 6.0** - if both (1) and (3) are
   insufficient, gate the entire `probe_send_zc()` call behind a
   compile-time or config-time kernel floor. The `iouring-send-zc`
   feature would then be a no-op (zero cost) on < 6.0 kernels by
   definition.

5. **Document the regression** - if the overhead is real but small
   (1-2%), document it as an acceptable one-time startup cost and keep
   the feature compiled in. The long-term direction is 6.x adoption,
   making this a transitional cost.

## 8. Bench harness location

```
crates/fast_io/benches/szp_5_send_zc_fallback.rs
```

The harness reuses the daemon bench fixture from `szc-a` (loopback
daemon with tmpfs module) but parameterizes builds with and without
`--features iouring-send-zc`. Criterion groups:

- `szp5/probe_latency` - microbench of `is_supported()` first-call vs
  cached
- `szp5/transfer_10` - 10 concurrent, 10 MiB each
- `szp5/transfer_100` - 100 concurrent, 10 MiB each
- `szp5/transfer_1000` - 1000 concurrent, 10 MiB each

## 9. Relation to other SZP items

| Item | Status | Relation |
|------|--------|----------|
| SZP-1 | Done | Kernel distribution survey - confirmed 5.15 is dominant |
| SZP-2 | Done | Fallback signal audit - confirmed silent fallback, no log |
| SZP-3 | Done | CI on 5.15 - ensures the fallback path compiles and runs |
| SZP-4 | Done | Per-feature kernel matrix - documents SEND_ZC floor at 6.0 |
| **SZP-5** | This doc | Bench: is the fallback overhead measurable? |

If SZP-5 passes (expected), the `iouring-send-zc` feature can be
promoted to default-on without harming 5.15 LTS deployments. The feature
becomes purely additive: zero cost on < 6.0, measurable throughput gain
on >= 6.0 (quantified by IUS-3).
