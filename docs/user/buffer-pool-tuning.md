# Buffer pool tuning

This guide describes the I/O buffer pool used by `oc-rsync` and `oc-rsyncd`,
how it sizes itself by default, and how to tune it for high-concurrency
daemon deployments.

## Overview

oc-rsync maintains a process-wide pool of reusable I/O buffers to reduce
allocation overhead during file transfers. The pool uses a two-level
architecture:

1. **Thread-local fast path** - each worker thread caches one buffer with
   zero synchronization. This absorbs 95%+ of acquire/return operations
   under typical rayon workloads.
2. **Central pool** - a lock-free queue stores overflow buffers. Only
   accessed on thread-local miss.

Buffers are handed out through RAII guards that automatically return them
to the pool on drop. Callers never see raw buffers.

## Default behavior

| Parameter | Default | Source |
|-----------|---------|--------|
| Buffer count | One per hardware thread | `std::thread::available_parallelism()` |
| Buffer size | 128 KiB | `COPY_BUFFER_SIZE` |
| Byte budget | 32 MiB | `DEFAULT_BYTE_BUDGET` |
| Memory cap | None (uncapped) | - |

The **byte budget** is a soft cap on the total bytes of buffers retained
in the pool. When a buffer return would push retained bytes past the
budget, the buffer is deallocated instead of retained. Acquires never
block - callers always get a buffer (fresh allocation on pool miss).

The byte budget guards against unbounded retention when adaptive buffer
sizing creates large buffers (up to 1 MiB for files >= 256 MiB). Without
it, a handful of large-file transfers could leave 1 MiB buffers in the
pool that accumulate past a reasonable memory budget.

The **memory cap** (when configured via `--max-alloc`) is a hard ceiling on
outstanding (checked-out) memory. When the cap is reached, acquires block
until a buffer is returned. This provides backpressure for memory-constrained
environments.

## Adaptive buffer sizing

The pool selects buffer sizes based on file size to balance memory
consumption against throughput:

| File size | Buffer size |
|-----------|-------------|
| < 64 KB | 8 KB |
| 64 KB - 1 MB | 32 KB |
| 1 MB - 64 MB | 128 KB |
| 64 MB - 256 MB | 512 KB |
| >= 256 MB | 1 MB |

## Environment variables

| Variable | Effect |
|----------|--------|
| `OC_RSYNC_BUFFER_POOL_SIZE` | Override buffer count (positive integer) |
| `OC_RSYNC_BYTE_BUDGET` | Override byte budget in bytes; `0` disables |
| `OC_RSYNC_BUFFER_POOL_STATS` | Set to `1` to print pool telemetry on exit |

## CLI flags

| Flag | Effect |
|------|--------|
| `--max-alloc=SIZE` | Sets the byte budget on pool retention |

When `--max-alloc` is specified, it overrides the default 32 MiB byte
budget.

## Daemon deployments

### Shared pool architecture

`oc-rsyncd` uses a thread-per-connection model. All connections share a
single process-wide buffer pool. This is intentional - pooled buffers are
reused across connections, so the pool size scales with the degree of
parallelism (worker threads), not with the number of connections.

### Memory impact calculation

The pool's maximum retained memory is bounded by:

    retained_bytes <= min(byte_budget, buffer_count * max_buffer_size)

With defaults:

    retained_bytes <= min(32 MiB, num_cpus * 128 KiB)

On a 16-core server: `min(32 MiB, 2 MiB) = 2 MiB` retained.

Outstanding (checked-out) memory scales with active I/O operations:

    outstanding_bytes = active_workers * buffer_size_per_worker

At 100 concurrent transfers on a 16-core server, only 16 workers run
simultaneously (rayon thread pool), so outstanding memory is roughly:

    16 * 128 KiB = 2 MiB (standard buffers)
    16 * 1 MiB = 16 MiB (worst case with large-file adaptive buffers)

### Tuning recommendations

For most daemon deployments, the defaults are appropriate. The pool
self-sizes to hardware parallelism and the 32 MiB byte budget prevents
runaway retention.

**Memory-constrained environments** (containers, VMs with < 1 GiB RAM):

```ini
# In systemd unit or container env:
OC_RSYNC_BYTE_BUDGET=8388608     # 8 MiB byte budget
OC_RSYNC_BUFFER_POOL_SIZE=4      # 4 buffers max
```

Or via CLI:

```sh
oc-rsyncd --max-alloc=8M ...
```

**High-throughput servers** (many large-file transfers, ample RAM):

```ini
OC_RSYNC_BYTE_BUDGET=67108864    # 64 MiB byte budget
OC_RSYNC_BUFFER_POOL_SIZE=32     # 32 buffers max
```

**Disabling the byte budget** (not recommended, but available):

```ini
OC_RSYNC_BYTE_BUDGET=0           # Unbounded retention
```

### Monitoring

Set `OC_RSYNC_BUFFER_POOL_STATS=1` to print pool telemetry to stderr
when the process exits:

```
BufferPool stats: reuses=12345 allocations=67 growths=0 byte_overflows=3 hit_rate=99.5%
```

Key metrics:
- **reuses** - buffer acquisitions satisfied from pool (higher is better).
- **allocations** - fresh allocations due to pool miss.
- **byte_overflows** - returns rejected by the byte budget (buffer
  deallocated instead of retained).
- **hit_rate** - reuse percentage; > 95% indicates the pool is well-sized.
