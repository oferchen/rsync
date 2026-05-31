# Checksum Algorithm Performance

This document describes the checksum algorithms used by oc-rsync, their
performance characteristics, SIMD acceleration coverage, and practical
guidance for operators.

## Algorithm Inventory

oc-rsync implements the same checksum algorithms as upstream rsync 3.4.1,
organized into two categories: a rolling (weak) checksum for block matching
and strong checksums for collision verification.

### Rolling Checksum (Weak Hash)

| Property | Value |
|----------|-------|
| Algorithm | Adler-32 style (rsync `rsum`) |
| Output size | 32 bits (4 bytes) |
| Purpose | O(1) sliding-window block matching |
| SIMD acceleration | AVX2, SSE2, NEON |
| Approximate throughput | 8,000 MB/s (SIMD), 2,000 MB/s (scalar) |

The rolling checksum enables the delta-transfer algorithm's sliding window.
Every block in the file is hashed with this checksum first; only blocks
whose weak hash matches proceed to strong-checksum verification.

### Strong Checksums

Strong checksums verify that a rolling-checksum match is not a false
positive. The protocol negotiates which algorithm to use.

| Algorithm | Output | Throughput (approx) | Use case |
|-----------|--------|---------------------|----------|
| XXH3-64 | 8 B | 15,000 MB/s (SIMD) | Negotiated, protocol >= 30 |
| XXH3-128 (XXH128) | 16 B | 12,000 MB/s (SIMD) | Negotiated, lower collision probability |
| XXH64 | 8 B | 10,000 MB/s | Legacy fast hash, protocol >= 30 |
| MD5 | 16 B | 500 MB/s (scalar), 1,000 MB/s (OpenSSL), 3,000 MB/s (SIMD batch) | Fallback for protocol >= 30 |
| MD4 | 16 B | 400 MB/s (scalar), 800 MB/s (OpenSSL), 2,500 MB/s (SIMD batch) | Legacy default, protocol < 30 |
| SHA-1 | 20 B | 300-600 MB/s | Rarely preferred in practice |
| SHA-256 | 32 B | 200 MB/s (scalar), 800 MB/s (SHA-NI) | Daemon authentication |

Throughput figures are approximate single-core measurements on modern
x86_64 hardware (2020+ era). Actual throughput varies by CPU generation,
cache hierarchy, and input size.

## Protocol Negotiation Rules

Algorithm selection depends on protocol version and capability negotiation.

### Protocol < 30

MD4 is always used. No negotiation occurs.

### Protocol >= 30 without Capability Negotiation

MD5 is the default when the peer lacks the `CF_VARINT_FLIST_FLAGS` ('v')
capability. No vstring exchange occurs.

### Protocol >= 30 with Capability Negotiation

When both peers support vstring negotiation (indicated by 'v' in the
capability string `-e.LsfxCIvu`), they exchange supported algorithm lists
and converge on the first mutually supported entry. The preference order
matches upstream rsync 3.4.1:

```
xxh128 > xxh3 > xxh64 > md5 > md4 > sha1 > none
```

The client's preference list takes priority - both sides converge on the
first algorithm in the client's list that also appears in the server's list.

### `--checksum-choice` Override

Users can force a specific algorithm via `--checksum-choice=<name>`. The
advertised list is replaced with a single entry, and negotiation verifies
the peer supports it. This bypasses the normal preference order.

## SIMD Acceleration Coverage

oc-rsync uses runtime CPU feature detection to select the fastest available
code path. No recompilation is needed - portable binaries automatically use
SIMD when the host CPU supports it.

### Rolling Checksum SIMD Dispatch

| Architecture | Instruction Set | Bytes/iteration | Detection |
|--------------|----------------|-----------------|-----------|
| x86_64 | AVX2 | 32 | `is_x86_feature_detected!("avx2")` |
| x86_64 | SSE2 | 16 | `is_x86_feature_detected!("sse2")` |
| aarch64 | NEON | 16 | `is_aarch64_feature_detected!("neon")` |
| Other | Scalar | 4 (unrolled) | Always available |

Detection is performed once at first use and cached in a `OnceLock`. All
SIMD paths tail-call into the scalar implementation for trailing bytes,
preserving byte-for-byte parity with upstream.

### XXH3 SIMD (Always Compiled)

The `xxh3` crate provides runtime SIMD detection for one-shot digest
operations. Portable binaries automatically use AVX2 (x86_64) or NEON
(aarch64) when available, providing approximately 3x speedup over scalar.

| Architecture | SIMD path | Scalar fallback |
|--------------|-----------|-----------------|
| x86_64 | AVX2 | Yes |
| aarch64 | NEON | Yes |
| Other | - | Scalar only |

### MD4/MD5 SIMD Batch Hashing

The `simd_batch` module processes multiple independent inputs in parallel
using SIMD lanes. This is especially effective for computing block
signatures across many small blocks simultaneously.

| Backend | Parallel Lanes | Architecture |
|---------|---------------|--------------|
| AVX-512 | 16 | x86_64 (AVX-512F + AVX-512BW) |
| AVX2 | 8 | x86_64 |
| SSE4.1 | 4 | x86_64 |
| SSSE3 | 4 | x86_64 |
| SSE2 | 4 | x86_64 (baseline) |
| NEON | 4 | aarch64 |
| Scalar | 1 | All platforms |

The dispatch ladder selects the widest available backend at runtime.
Parity between all backends is enforced by tests against RFC vectors and
property-based fuzzing.

### SHA-1/SHA-256 Hardware Acceleration

SHA-family algorithms benefit from hardware crypto extensions when
available:

- **x86_64**: SHA-NI instructions (available on AMD Zen+ and Intel
  Ice Lake+)
- **aarch64**: ARM crypto extensions

These must be enabled at compile time via target features; they are not
runtime-detected in the same way as the rolling checksum or XXH3.

### Querying Acceleration Status

oc-rsync exposes runtime queries for SIMD availability:

- **Rolling checksum SIMD**: Reports whether AVX2/SSE2/NEON is active
- **XXH3 runtime SIMD**: Always true (the `xxh3` crate is always compiled)
- **OpenSSL acceleration**: Reports whether OpenSSL-backed MD4/MD5 is linked
- **SIMD batch backend**: Reports which MD5 batch lane width is active

The `--simd=<level>` CLI flag allows pinning dispatch to a specific level
(useful for benchmarking or debugging). Supported values: `auto`, `avx512`,
`avx2`, `sse4`, `neon`, `none`.

## Bandwidth-Constrained Performance Analysis

The practical impact of algorithm choice depends on whether the CPU or the
network is the bottleneck.

### Break-Even Bandwidth

The break-even bandwidth is the link speed at which a given algorithm
becomes the bottleneck. Below this threshold, the network limits throughput
and algorithm choice is irrelevant to wall-clock time.

| Algorithm | Approximate Throughput | Break-Even Bandwidth |
|-----------|----------------------|---------------------|
| XXH3-64 (SIMD) | 15,000 MB/s | >> 10 GbE (never CPU-bound) |
| XXH3-128 (SIMD) | 12,000 MB/s | >> 10 GbE |
| Rolling (SIMD) | 8,000 MB/s | >> 10 GbE |
| MD5 (SIMD batch) | 3,000 MB/s | ~3 Gbps |
| MD4 (SIMD batch) | 2,500 MB/s | ~2.5 Gbps |
| SHA-256 (SHA-NI) | 800 MB/s | ~800 Mbps |
| MD5 (scalar) | 500 MB/s | ~500 Mbps |
| MD4 (scalar) | 400 MB/s | ~400 Mbps |

### Network-Bound Scenarios (Checksum Choice Does Not Matter)

When the network is the bottleneck, all algorithms finish processing each
block before the next block arrives. In these conditions:

- **10 Mbps constrained links** (cellular, throttled SSH): Even MD4 scalar
  finishes each 128 KB block in 320 us, while block arrival takes 100 ms.
  Algorithm choice is irrelevant - 300x headroom.
- **100 Mbps WAN** (site-to-site VPN, cross-region): MD5 scalar finishes
  a 128 KB block in 256 us; block arrival takes 10 ms. Still 40x headroom.
- **1 Gbps LAN** (office, cloud VPC): MD5 scalar finishes a 128 KB block
  in 256 us; block arrival takes 1 ms. Still 4x headroom. Even scalar
  MD4/MD5 is not the bottleneck.

### CPU-Bound Scenarios (Checksum Choice Matters Significantly)

When the link is fast enough that data arrives faster than the CPU can
hash it, algorithm selection directly impacts transfer throughput:

- **10 GbE data center** (1,000 MB/s effective): MD5 scalar (500 MB/s)
  becomes the bottleneck. XXH3 negotiation saves ~50% wall-clock on
  checksum-heavy workloads. SIMD batch MD5 (3,000 MB/s) still has
  headroom.
- **Local NVMe copy** (3,500+ MB/s): Checksum is always on the critical
  path. The difference between XXH3 (15,000 MB/s) and MD5 scalar
  (500 MB/s) is 30x. This is where algorithm selection and SIMD
  acceleration produce the largest benefit.
- **`--checksum` mode** (whole-file hashing): Every byte is checksummed
  regardless of whether it changed. On local or high-bandwidth transfers,
  algorithm choice dominates wall-clock time. Measured gap: upstream rsync
  with XXH128 is 2-3x faster than MD5 on a 500 MB file; with matching
  algorithms, oc-rsync is 2x faster than upstream thanks to rayon
  parallelism.

### Crossover Point Analysis

The crossover point where algorithm choice begins to matter depends on
the interaction of link speed, block size, and SIMD availability:

| Link Speed | MD5 Scalar Headroom | MD5 SIMD Batch Headroom | XXH3 Headroom |
|-----------|--------------------|-----------------------|--------------|
| 100 Mbps | 43x | 260x | >1000x |
| 1 Gbps | 4.3x | 26x | >100x |
| 10 Gbps | 0.43x (bottleneck) | 2.6x | >10x |
| Local NVMe | 0.14x (bottleneck) | 0.86x (marginal) | 4.3x |

Values > 1x indicate the CPU is idle between blocks. Values < 1x indicate
the checksum cannot keep up with data arrival.

**Summary**: On hosts without SIMD, algorithm choice becomes relevant at
1 Gbps. On SIMD-capable hosts, MD5 batch keeps up until approximately
3 Gbps, and XXH3 is never the bottleneck at any realistic link speed.

## Recommendations for Operators

### Default Behavior

When both peers support capability negotiation (the common case for
protocol 30+), oc-rsync automatically selects XXH128 or XXH3 as the
strong checksum. No operator action is needed - the fastest mutually
supported algorithm is chosen by default.

The preference order is:
1. XXH128 (fastest with lowest collision probability)
2. XXH3-64 (fastest)
3. XXH64
4. MD5 (fallback when peer lacks XXH support)
5. MD4 (legacy protocol < 30 only)

### SSH Capability String

The SSH capability string `-e.LsfxCIvu` enables checksum negotiation
between peers. Each letter enables a specific protocol feature:

- `L` - symlink times
- `s` - symlink iconv
- `f` - fuzzy basis
- `x` - xattrs
- `C` - ACLs
- `I` - incremental recursion
- `v` - varint flist flags (enables vstring negotiation)
- `u` - unsigned checksum seed

The critical flag is `v` (varint flist flags). Without it, the peer
does not support algorithm negotiation, and oc-rsync falls back to MD5.
This can cause significant performance loss on high-bandwidth or local
transfers where XXH3 would otherwise be 10-30x faster.

**Recommendation**: Ensure both the local and remote oc-rsync (or rsync
3.2+) support the full capability string. Older rsync versions (< 3.2)
lack 'v' support and will force MD5 fallback.

### `--checksum` Mode Implications

`--checksum` (`-c`) forces whole-file checksum comparison instead of the
default quick-check (size + mtime). This means every byte of every file
is hashed, making algorithm selection critical for performance:

- With XXH128 (negotiated): ~15,000 MB/s throughput. A 500 MB file is
  checksummed in approximately 33 ms per side.
- With MD5 (fallback): ~500 MB/s scalar, ~3,000 MB/s SIMD batch. The
  same file takes 170 ms to 1,000 ms per side depending on SIMD
  availability.

The measured impact on a 500 MB no-change re-sync:
- Upstream rsync (XXH128): 189 ms wall time
- oc-rsync (XXH128, post-CSM-8): comparable performance
- oc-rsync (MD5 fallback): 862 ms wall time (4.6x slower)

**Recommendation**: When using `--checksum` mode with high-bandwidth
links or local transfers, verify that negotiation selects XXH128/XXH3
(visible in debug output). If the peer forces MD5 fallback, consider
upgrading the remote side.

### Forcing Algorithm Selection

Use `--checksum-choice=<algorithm>` to override negotiation:

```sh
# Force XXH128 for maximum speed (both peers must support it)
oc-rsync -a --checksum-choice=xxh128 src/ dst/

# Force MD5 for compatibility testing
oc-rsync -a --checksum-choice=md5 src/ dst/
```

Available algorithm names: `xxh128`, `xxh3`, `xxh64`, `md5`, `md4`,
`sha1`, `none`.

### SIMD Level Control

The `--simd=<level>` flag pins SIMD dispatch for debugging or
benchmarking:

```sh
# Force scalar-only (useful for measuring SIMD benefit)
oc-rsync -a --simd=none src/ dst/

# Force AVX2 maximum (skip AVX-512 even if available)
oc-rsync -a --simd=avx2 src/ dst/
```

This affects the rolling checksum, SIMD batch MD4/MD5, and (indirectly)
any algorithm whose dispatch consults the override. XXH3 uses its own
runtime detection via the `xxh3` crate and is not affected by this flag.

## Performance Summary Table

| Scenario | Dominant Factor | Algorithm Impact |
|----------|----------------|-----------------|
| WAN transfer (< 100 Mbps) | Network latency | None |
| LAN transfer (1 Gbps) | Network bandwidth | Negligible |
| Data center (10 GbE) | CPU (scalar MD5) | Moderate - use XXH3 |
| Local copy (NVMe) | CPU always | High - XXH3 is 30x faster than MD5 |
| `--checksum` local | CPU always | High - 2-5x wall-clock difference |
| `--checksum` over WAN | Network | None |
| Delta transfer (small changes) | Rolling checksum + strong verify | Low - few blocks verified |
| Initial sync (all new) | No checksums on receiver | Sender-side file-list checksum only |

## Upstream Compatibility

All checksum implementations produce output identical to upstream rsync
3.4.1:

- Rolling checksums match `checksum.c:get_checksum1()`
- MD4/MD5 match rsync's seeded checksum paths with proper
  `CHECKSUM_SEED_FIX` handling
- XXH64/XXH3/XXH128 match rsync's modern strong checksum paths
- Negotiation follows upstream `compat.c:534-585`
  (`negotiate_the_strings()`)

Wire-format compatibility is verified by golden byte tests in
`crates/protocol/tests/golden/` and interop tests against upstream rsync
3.0.9, 3.1.3, 3.4.1, and 3.4.2.
