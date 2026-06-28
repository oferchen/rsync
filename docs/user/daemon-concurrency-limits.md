# Daemon concurrency limits

This guide describes the concurrency envelope of `oc-rsyncd`, why it has a
soft ceiling near ten thousand concurrent connections, and how to size
`--max-connections` for the workloads you actually run. The numbers here are
design-intent envelopes, not benchmark-validated upper bounds; per-scale
measurements are tracked under the D10K series (#2830-#2836) and will
replace the figures in this guide once they land.

## Overview

`oc-rsyncd` uses a thread-per-connection model. Every accepted TCP
connection runs on its own OS thread for the lifetime of the transfer, the
same shape upstream `rsyncd` has used since the daemon was introduced. The
advantages are familiar:

- Simple to reason about. One thread, one connection, one stack frame.
- Low per-connection scheduling overhead while the working set fits in
  cache. The kernel scheduler handles fairness with no userspace runtime
  on top.
- Trivially observable. `top -H`, `htop`, `ps -L`, and `/proc/<pid>/task/`
  show one entry per in-flight connection. Per-connection CPU and state
  are visible without instrumentation.
- Crashes in one connection do not unwind the others. The daemon uses
  `catch_unwind` on the worker boundary; an aborted worker logs and
  releases its slot.

The trade-off is a soft ceiling on concurrent connections rooted in
per-thread stack memory and scheduler overhead. The next section quantifies
the envelope.

## Soft ceiling at ~10K concurrent connections

Each spawned worker reserves an 8 MiB virtual stack by default (the Rust
standard library default on Linux and macOS). At ten thousand connections
that is roughly 80 GiB of address space for thread stacks alone, before any
per-connection working memory, file descriptors, or kernel buffers. RSS
grows only with pages the worker actually touches, but the address-space
reservation is real and competes with mmaped basis files, page cache, and
io_uring submission rings.

In practice the model behaves like this:

| Concurrency | Behaviour |
|------------:|-----------|
| 1 - few hundred | Optimal. Thread-creation cost amortises across the transfer; scheduler stays out of the way. |
| Low thousands  | Graceful degradation. Stack reservations dominate the virtual-memory map; context-switch rate climbs but throughput stays acceptable. |
| ~8K - ~12K     | Practical ceiling. Address-space pressure squeezes mmaped basis files and io_uring rings; `pthread_create` latency becomes visible against short transfers; scheduler queues grow. |
| > ~12K         | Saturation. Connections queue or are refused; tail latency degrades sharply. |

The exact crossover depends on host RAM, `vm.max_map_count`, `ulimit -s`,
`ulimit -n`, and the fraction of transfers that are short module listings
versus long bulk copies. The bench harness landed under D10K-1 (#2830) is
the apparatus for replacing these qualitative bands with measured numbers
at the W100, W1k, and W10k waypoints.

## Admission gate: --max-connections

`--max-connections=N` caps the number of concurrent worker threads the
daemon will spawn. When the cap is reached, new TCP sockets are accepted,
sent the upstream-compatible refusal string
`@ERROR: max connections (N) reached -- try again later`, and disconnected
cleanly. Existing connections continue to run. The cap shipped in
**v0.6.2**; older builds had no admission gate and would happily spawn
threads until the OS refused.

The flag has a per-module equivalent in `oc-rsyncd.conf`:

```ini
[backups]
    path = /srv/backups
    max connections = 32
```

Per-module caps stack with the daemon-wide `--max-connections`. The
effective limit for any module is the lower of the two. Setting only a
per-module cap leaves the daemon-wide pool unbounded, which is rarely what
you want for an internet-exposed listener.

## Operational recommendations

Pick a cap, do not leave the daemon uncapped. The right number depends on
workload shape and host budget. Use the rules of thumb below as a starting
point, then validate against your own traffic.

### Sizing from available memory

A conservative first cut:

```
max_connections = (available_RAM_MiB / 2) - reserved_overhead_MiB
```

The factor of two reflects the 8 MiB stack reservation plus a roughly
equal-sized working budget per connection (file buffers, basis maps, delta
state, kernel socket buffers). `reserved_overhead_MiB` should cover the
kernel, page cache headroom, and any non-rsync resident set on the host.
On a 32 GiB host that reserves 4 GiB for the OS and page cache, the
formula gives `(32 768 / 2) - 4 096 = 12 288`, so a cap of roughly twelve
thousand would saturate the host. Practical caps land well below that
ceiling.

### By workload shape

- **Backup servers, large transfers.** Set a low cap, often **10 - 50**.
  Long-lived workers each consume substantial buffer memory and disk
  bandwidth; over-subscription causes scheduler thrashing and disk-queue
  contention rather than higher throughput. A handful of concurrent
  multi-gigabyte transfers will saturate most NICs and SSDs on their own.
- **Many-small-files workloads.** Intermediate caps in the **100 - 500**
  range usually fit. Per-file overhead dominates throughput, so adding
  workers raises aggregate IOPS until you hit the disk queue depth.
- **Module listings and probes.** Short connections (`rsync rsync://host/`
  with no path) finish in milliseconds. Caps in the **500 - 2000** range
  are usually safe, but at this scale the thread-creation cost becomes a
  meaningful fraction of the work; see D10K-2..D10K-5 (#2831-#2834) for
  the bench plan that quantifies the crossover.
- **Above ~1000 concurrent.** Re-evaluate the topology. Horizontal
  scaling with multiple daemon hosts behind a connection-aware load
  balancer is usually a better answer than pushing a single host into
  the saturation band. Stickiness on the load balancer matters: rsync
  transfers are stateful for the duration of the connection but
  independent across connections, so any L4 or L7 distribution works.

### Tuning the host

Pair `--max-connections` with the OS limits the daemon actually consumes:

- `ulimit -n` (`LimitNOFILE` under systemd). Each connection needs at
  least one socket and a handful of file descriptors per active transfer.
  Budget at least ten descriptors per concurrent connection plus a
  margin.
- `ulimit -u` (`LimitNPROC`). The thread cap. Must exceed
  `--max-connections` plus the daemon's own thread pool (logging, io_uring,
  rayon worker pool).
- `vm.max_map_count`. Each thread stack and every mmaped basis file
  consumes one VMA. The default of 65 530 is comfortable up to a few
  thousand connections; raise it for higher caps.
- `net.core.somaxconn` and the listen backlog. Bursty arrival rates need
  backlog headroom or new connections see `ECONNREFUSED` before the
  accept loop runs.

## Spreading accept load: acceptor threads

By default the daemon binds one listener socket per address family and runs
a single accept loop over it. On a host fielding a high rate of short-lived
connections, that one accept loop can become the bottleneck before any
worker thread saturates.

The `acceptor threads` global directive binds N `SO_REUSEPORT` listener
replicas per family instead of one:

```
# /etc/oc-rsyncd.conf
acceptor threads = 4
```

Each replica gets its own acceptor thread, and the kernel load-balances
inbound connections across them. This is an oc-rsync extension with no
upstream equivalent (upstream forks one child per accepted connection from
a single listener) and changes only kernel socket behaviour, never the
wire. The default of 1 preserves the historical single-listener model.

Notes:

- `SO_REUSEPORT` is a Linux/BSD feature. On platforms without it the
  replicas still bind, but the kernel does not load-balance across them, so
  values above 1 provide no benefit there.
- Size N to the number of CPUs you want fielding accepts, not to
  `--max-connections`. A handful (2-8) is typically enough; the accept path
  is cheap once a connection lands on a worker thread.
- The per-daemon `--max-connections` cap and its admission counter are
  global across all acceptor threads, so adding replicas never loosens the
  concurrency ceiling.

## Why not async

A `tokio`-based runtime would lift the per-connection ceiling by an order
of magnitude. Tasks cost a few kilobytes of heap rather than 8 MiB of
address space, so a host that tops out near ten thousand threads could in
principle hold tens of thousands of idle or lightly-active connections.

The trade-off is single-stream throughput. The current synchronous worker
calls `read` and `write` directly, hands the basis file to `io_uring` with
fixed buffers, and uses blocking `mmap` for the strong-checksum pass. Each
of those would need an async boundary in a `tokio` daemon, and several -
`mmap`, fsync, fixed-buffer io_uring submissions - are inherently
blocking on Linux. The pragmatic choices (offloading to
`tokio::spawn_blocking`) reintroduce a thread pool with its own ceiling;
see the russh spawn-blocking ceiling for what that looks like at the SSH
boundary.

The decision is deferred pending bench evidence from ASY-4 (#2777). The
async migration plan and the case for keeping the synchronous worker model
are documented under `docs/design/async-migration-plan.md` and
`docs/design/daemon-async-accept-sync-workers.md`. `oc-rsyncd` will not
switch concurrency models without measured numbers from ASY-4 and the
embeddability check in ASY-5.

## Tracking

The D10K task series carries the bench harness and per-scale measurements
behind the figures in this guide:

| Task | Issue | Scope |
|------|-------|-------|
| D10K-1 | #2830 | Bench harness for daemon thread-per-connection ceiling. Landed. |
| D10K-2 | #2831 | W100 measurement: 100 concurrent connections. |
| D10K-3 | #2832 | W1k measurement: 1 000 concurrent connections. |
| D10K-4 | #2833 | W10k measurement: 10 000 concurrent connections (exploratory ceiling). |
| D10K-5 | #2834 | Per-scale comparison versus upstream `rsyncd`. |
| D10K-6 | #2835 | This document. |

The 8 MiB stack figure, the 80 GiB at-10k extrapolation, and the
qualitative bands above are design-intent envelopes. D10K-2..D10K-5 will
replace them with measured upper bounds; this guide will be revised in
place once those numbers land.

## References

- `docs/DAEMON_PROCESS_MODEL.md` - upstream fork versus oc-rsync thread
  model comparison; covers crash isolation, fd inheritance, and the
  `max connections` admission semantics.
- `docs/design/daemon-thread-per-conn-bench.md` - the D10K bench plan,
  including the W100 / W1k / W10k waypoints this guide references.
- `docs/design/daemon-async-accept-sync-workers.md` - the case for the
  current synchronous worker model and the thresholds at which an async
  accept loop becomes worth the complexity.
- `docs/design/async-migration-plan.md` - full async migration plan,
  ASY-1..ASY-12 sequencing, and the trade-offs gating ASY-4 / ASY-5.
- Upstream `rsync` daemon model: `target/interop/upstream-src/rsync-3.4.1/`
  `main.c` (`fork()` per accepted connection, child runs
  `start_daemon()`).
