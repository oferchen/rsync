# Bandwidth-Constrained Checksum Benchmark Harness (CBP-1)

## Summary

Existing checksum benchmarks (`crates/checksums/benches/`) measure raw
throughput and pipelining overhead on in-memory data. They answer "how
fast is each algorithm?" but not the question that drives algorithm
selection in production: "does the checksum algorithm matter given the
link speed?"

This design specifies a benchmark harness that answers that question by
measuring checksum CPU cost relative to data arrival rate under
realistic bandwidth constraints. The harness determines, for each
network tier, whether checksum computation is on the critical path or
fully masked by I/O wait.

## 1. Motivation

Rsync protocol negotiation selects a checksum algorithm based on
capability strings (`-e.LsfxCIvu`). When both sides support XXH3, it
replaces MD5, saving ~34% CPU on aarch64 (measured empirically on
Apple M-series). But this saving only matters if the CPU is the
bottleneck - on a 10 Mbps WAN link, even MD5 finishes each block
before the next one arrives, making algorithm selection irrelevant to
wall-clock transfer time.

The harness produces a matrix showing, for each (algorithm, bandwidth,
block-size, file-size) tuple, whether the checksum is:

- **Idle-dominant** - CPU finishes before the next block arrives; time
  is pure I/O wait.
- **Overlapped** - CPU and I/O partially overlap; faster algorithms
  reduce tail latency but not median throughput.
- **CPU-bound** - checksum is on the critical path; algorithm choice
  directly impacts transfer throughput.

## 2. Benchmark Scenarios

### 2.1 Bandwidth tiers

| Tier | Effective rate | Represents |
|------|---------------|------------|
| Local NVMe | 3,500 MB/s | Local-copy mode (basis reads) |
| 10 GbE | 1,000 MB/s | Data-center replication |
| 1 Gbps LAN | 115 MB/s | Office LAN, cloud VPC |
| 100 Mbps WAN | 11.5 MB/s | Site-to-site VPN, cross-region |
| 10 Mbps constrained | 1.15 MB/s | Cellular backhaul, throttled SSH |

The "Local NVMe" tier serves as the pure-CPU ceiling - block data is
memory-resident and checksum throughput is the only variable. Lower
tiers add simulated I/O delay per block.

### 2.2 Algorithms under test

All algorithms used in the oc-rsync protocol pipeline:

| Algorithm | Role | Digest size |
|-----------|------|-------------|
| Rolling (rsum) | Weak checksum, every block | 4 B |
| MD4 | Strong checksum, protocol < 30 | 16 B |
| MD5 | Strong checksum, protocol >= 30 (fallback) | 16 B |
| XXH3-64 | Strong checksum, negotiated | 8 B |
| XXH3-128 | Strong checksum, negotiated (lower collision) | 16 B |
| SHA-256 | Daemon authentication | 32 B |

SHA-1, SHA-512, and XXH64 are included for completeness but are not
expected to appear on the critical path during file transfer.

### 2.3 File size matrix

| Size | Represents |
|------|-----------|
| 1 KB | Config files, dotfiles |
| 64 KB | Small source files |
| 1 MB | Typical documents, images |
| 100 MB | VM images, database dumps |
| 1 GB | Large archives, backup sets |

### 2.4 Block size matrix

| Block size | Context |
|------------|---------|
| 700 B | Upstream default for small files (`DEFAULT_BLOCK_SIZE`) |
| 4 KB | Page-aligned, common for medium files |
| 32 KB | Large-file tier in generator growth curve |
| 64 KB | Pipelining bench default |
| 128 KB | `MAX_BLOCK_SIZE_V30` ceiling |

## 3. Metrics Captured

### 3.1 Per-algorithm, per-block-size metrics

| Metric | Unit | Method |
|--------|------|--------|
| Raw throughput | MB/s | criterion `Throughput::Bytes` |
| CPU time per block | ns | criterion wall-clock / iterations |
| CPU cycles per byte | cycles/B | `perf stat` (Linux) or Instruments (macOS) |
| Throughput at bandwidth | MB/s | min(algorithm_throughput, link_rate) |
| Idle fraction | % | (1 - cpu_time / inter_block_arrival) * 100 |
| Tail latency (p99) | ns | criterion extended stats |

### 3.2 Derived per-(algorithm, bandwidth) summary

- **Break-even bandwidth** - the link speed at which the checksum
  becomes the bottleneck. Computed as `block_size / cpu_time_per_block`.
- **CPU headroom** - at a given bandwidth, the ratio
  `algorithm_throughput / link_rate`. Values > 1 mean the CPU is idle
  between blocks; values < 1 mean the algorithm cannot keep up.
- **Algorithm-switch benefit** - for each bandwidth tier, the
  wall-clock speedup from switching MD5 to XXH3 (or MD4 to XXH3).
  Meaningful only when CPU headroom < 2x for the slower algorithm.

## 4. Bandwidth Simulation Strategy

### 4.1 Approach: Decoupled measurement with analytical model

Rather than rate-limiting a reader (which introduces scheduling noise
from sleep/wake cycles), the harness uses a two-phase design:

**Phase 1 - Raw CPU measurement (criterion):**
Measure each algorithm's per-block digest time with data resident in
L1/L2 cache. This gives the true CPU cost independent of I/O. The
existing bench infrastructure (`checksums_benchmark.rs`) already does
this; the new harness extends it with the full block-size and
file-size matrix.

**Phase 2 - Analytical overlay (post-processing script):**
A Python or Rust script reads criterion JSON output and computes, for
each bandwidth tier, the effective transfer rate:

```
effective_rate = min(link_rate, block_size / cpu_time_per_block)
idle_fraction = max(0, 1 - (cpu_time_per_block / (block_size / link_rate)))
```

This avoids benchmark noise from timer-based rate limiting while
producing the same insight. The script emits a table and an optional
SVG chart.

### 4.2 Validation: Rate-limited reader (optional, Linux-only)

For empirical validation of the analytical model, an optional bench
group uses a `RateLimitedReader` wrapper that enforces a byte budget
per wall-clock interval using `thread::sleep` between reads. This
confirms that the analytical predictions match measured behavior at
selected operating points (1 Gbps, 100 Mbps) within 5% tolerance.

```rust
struct RateLimitedReader<R> {
    inner: R,
    bytes_per_sec: u64,
    bytes_read_this_interval: u64,
    interval_start: Instant,
    interval_ns: u64,
}
```

The rate-limited path uses `criterion::measurement::WallTime` and
reports throughput as seen by the consumer (checksum + wait combined).

### 4.3 Why not `tc` or `trickle`?

External rate limiters operate at the socket layer and cannot
constrain in-process `Read` from memory buffers. They add kernel
scheduling jitter that makes micro-benchmarks unreliable. The
analytical approach is both simpler and more reproducible.

## 5. SIMD vs Scalar Comparison

### 5.1 Methodology

For algorithms with SIMD paths (rolling checksum, MD4, MD5), the bench
runs each block-size point twice:

1. **SIMD (default)** - normal dispatch through
   `accumulate_chunk_dispatch` / `md4_digest_batch` / `md5_digest_batch`.
2. **Scalar-only** - force the scalar path by calling the
   `_scalar_fallback` variant directly or gating via
   `CHECKSUMS_FORCE_SCALAR=1` env var.

The ratio (SIMD throughput / scalar throughput) is the "SIMD uplift
factor". Combined with the bandwidth model, this reveals the link
speeds at which SIMD acceleration actually changes the outcome.

### 5.2 Platform matrix

| Platform | SIMD tiers available |
|----------|---------------------|
| x86_64 | AVX-512, AVX2, SSE2, scalar |
| aarch64 | NEON, scalar |
| Other | scalar only |

Each tier is selectable via feature detection override (env var). The
bench captures all available tiers on the host and reports per-tier
throughput.

### 5.3 Multi-buffer batch width

For MD4 and MD5, the SIMD batch path processes N blocks in parallel
across SIMD lanes. The bench sweeps batch width {1, 4, 8, 16} to show
how lane utilization interacts with block size and bandwidth tier. At
700 B blocks on a 100 Mbps link, the inter-block arrival time is 487
us - long enough that batching is irrelevant. At NVMe speed with 128 KB
blocks, batching is critical.

## 6. Integration with Existing Infrastructure

### 6.1 Criterion bench file

New file: `crates/checksums/benches/bandwidth_constrained_benchmark.rs`

Registered in `crates/checksums/Cargo.toml`:

```toml
[[bench]]
name = "bandwidth_constrained_benchmark"
harness = false
```

### 6.2 Bench groups

| Group name | Parameterization |
|------------|------------------|
| `bw_raw_throughput/{algo}/{block_size}` | All algorithms x all block sizes |
| `bw_file_signature/{algo}/{file_size}/{block_size}` | Full file signature generation |
| `bw_simd_vs_scalar/{algo}/{block_size}` | SIMD vs forced-scalar |
| `bw_rate_limited/{algo}/{bandwidth}/{block_size}` | Empirical validation (opt-in) |

### 6.3 Post-processing script

`tools/bench/checksum_bandwidth_report.py`:

- Reads `target/criterion/` JSON output.
- Emits a markdown table to stdout and optionally an SVG heatmap.
- Columns: algorithm, block size, raw MB/s, break-even bandwidth,
  CPU headroom at each tier.
- Exit code 0 if no algorithm is CPU-bound at its expected operating
  tier (regression gate).

### 6.4 CI integration

The bandwidth bench runs only on the `oc-rsync-bench` container (Arch
Linux, consistent hardware) via the benchmark workflow. It does not
run on every PR. Results are appended to release notes as a chart.

## 7. Implementation Plan

| Step | Description | Tracked by |
|------|-------------|-----------|
| CBP-2 | Implement raw throughput bench with full matrix | Issue |
| CBP-3 | Implement SIMD vs scalar comparison bench | Issue |
| CBP-4 | Implement `RateLimitedReader` validation bench | Issue |
| CBP-5 | Write `checksum_bandwidth_report.py` post-processor | Issue |
| CBP-6 | Wire into benchmark workflow, document in README | Issue |

## 8. Expected Outcomes

Based on preliminary numbers from `checksums_benchmark.rs` (8 KB blocks):

| Algorithm | Approx throughput (MB/s) | Break-even BW |
|-----------|--------------------------|----------------|
| XXH3-64 | ~15,000 | >> 10 GbE (never CPU-bound) |
| XXH3-128 | ~12,000 | >> 10 GbE |
| Rolling | ~8,000 | >> 10 GbE |
| MD5 (SIMD batch) | ~3,000 | ~3 Gbps |
| MD4 (SIMD batch) | ~2,500 | ~2.5 Gbps |
| MD5 (scalar) | ~800 | ~800 Mbps |
| MD4 (scalar) | ~600 | ~600 Mbps |
| SHA-256 | ~1,500 | ~1.5 Gbps |

Key predictions:

1. **At 1 Gbps and below**, algorithm choice is irrelevant - all
   algorithms finish before the next block arrives. The checksum
   negotiation feature (`-e.LsfxCIvu`) provides no wall-clock benefit.
2. **At 10 GbE**, MD5 scalar becomes the bottleneck on non-SIMD
   hosts. XXH3 negotiation saves ~15% wall-clock time. SIMD MD5 still
   has headroom.
3. **Local NVMe copy**, checksum is always on the critical path. XXH3
   vs MD5 matters; SIMD vs scalar matters. This is where algorithm
   selection and SIMD acceleration pay off.

## 9. Success Criteria

The harness is complete when:

1. `cargo bench -p checksums --bench bandwidth_constrained_benchmark`
   runs and produces criterion output for all matrix points.
2. `tools/bench/checksum_bandwidth_report.py` generates a correct
   markdown table from the criterion output.
3. The analytical model and rate-limited empirical measurements agree
   within 5% at the 1 Gbps and 100 Mbps operating points.
4. Results are reproducible across runs on the bench container (< 3%
   coefficient of variation on 10 runs).

## 10. Non-Goals

- This harness does not test end-to-end transfer throughput. That
  belongs in `scripts/benchmark.sh` with real file I/O.
- This harness does not benchmark the rolling checksum's `roll()`
  operation under bandwidth constraints - roll is always CPU-bound and
  already covered by `checksums_benchmark.rs`.
- This harness does not measure memory allocation overhead. The
  `BufferPool` bench in the engine crate covers that.
- No wire protocol changes result from this work.
