# Crate Dependency Map and Modification Impact Analysis

This document maps inter-crate dependencies and classifies change impact to help
contributors assess the blast radius of modifications.

## Dependency Graph

Arrows indicate "depends on" (downstream -> upstream). Only workspace-internal
path dependencies are shown; external crates are omitted.

```
bin (root binary)
  cli, daemon, core, engine, checksums, transfer, fast_io

cli
  core, engine, logging, logging-sink, protocol, compress, metadata,
  rsync_io, fast_io, checksums, daemon (unix)

core
  transfer, protocol, metadata, engine, bandwidth, rsync_io, filters,
  compress, flist, logging, fast_io, branding, checksums

daemon
  compress, core, metadata, platform, protocol, logging-sink, checksums,
  fast_io, bandwidth (dev)

transfer
  protocol, metadata, engine, signature, filters, compress, logging,
  fast_io, checksums, apple-fs (macOS), platform (dev)

engine
  metadata, filters, compress, protocol, bandwidth, logging, signature,
  matching, batch, fast_io, checksums, apple-fs (macOS)

protocol
  compress, logging

signature
  protocol, checksums

matching
  signature, logging, checksums, protocol (dev)

flist
  logging

filters
  logging, protocol (optional)

metadata
  logging, protocol, apple-fs (macOS)

checksums
  fast_io, logging

rsync_io
  protocol, logging

batch
  protocol, metadata

bandwidth
  (no internal deps)

fast_io
  logging

compress
  (no internal deps)

logging
  (no internal deps)

logging-sink
  core

branding
  (no internal deps)

platform
  (no internal deps)

apple-fs
  (no internal deps)

test-support
  (no internal deps)

embedding
  cli, daemon, core

windows-gnu-eh
  (no internal deps)
```

## Impact Analysis

Each crate is classified by downstream dependents count and position in the graph.

### HIGH Impact - Core Infrastructure

Changes here ripple across most of the workspace.

| Crate | Depended On By | Depends On |
|-------|---------------|------------|
| logging | protocol, filters, metadata, checksums, fast_io, flist, rsync_io, engine, transfer, cli, core, matching | (none) |
| protocol | metadata, signature, rsync_io, batch, filters, engine, transfer, core, daemon, cli | compress, logging |
| compress | protocol, engine, transfer, core, daemon, cli | (none) |
| metadata | batch, engine, transfer, core, daemon, cli | logging, protocol, apple-fs |
| fast_io | checksums, engine, transfer, core, daemon, cli | logging |
| core | cli, daemon, logging-sink, embedding | transfer, protocol, metadata, engine, bandwidth, rsync_io, filters, compress, flist, logging, fast_io, branding, checksums |
| engine | transfer, core, cli | metadata, filters, compress, protocol, bandwidth, logging, signature, matching, batch, fast_io, checksums |

### MEDIUM Impact - Mid-Tier

Changes affect a moderate number of dependents.

| Crate | Depended On By | Depends On |
|-------|---------------|------------|
| checksums | signature, matching, engine, transfer, core, daemon, cli | fast_io, logging |
| transfer | core | protocol, metadata, engine, signature, filters, compress, logging, fast_io, checksums |
| filters | engine, transfer, core | logging, protocol (optional) |
| signature | matching, engine, transfer | protocol, checksums |
| rsync_io | core, cli | protocol, logging |

### LOW Impact - Leaf or Narrow

Changes are contained to few or zero dependents.

| Crate | Depended On By | Depends On |
|-------|---------------|------------|
| bandwidth | engine, core, daemon (dev) | (none) |
| matching | engine | signature, logging, checksums |
| batch | engine | protocol, metadata |
| flist | core | logging |
| branding | core | (none) |
| platform | daemon, transfer (dev) | (none) |
| apple-fs | metadata, engine, transfer (macOS) | (none) |
| logging-sink | cli, daemon | core |
| cli | bin, embedding | core, engine, logging, logging-sink, protocol, compress, metadata, rsync_io, fast_io, checksums, daemon |
| daemon | bin, cli (unix), embedding | core, metadata, platform, protocol, logging-sink, compress, checksums, fast_io |
| embedding | (none - library consumer) | cli, daemon, core |
| test-support | (dev-only) | (none) |
| windows-gnu-eh | bin (windows-gnu only) | (none) |

## Critical Paths

Ordered by blast radius (number of transitive dependents):

1. **logging** - 13 direct dependents. Universal infrastructure. Any breaking
   change to the logging API forces updates across the entire workspace.

2. **protocol** - 10 direct dependents. Wire format types and codec shared by
   nearly every crate that touches the network or file list.

3. **compress** - 7 direct dependents (via protocol and directly). Codec trait
   changes propagate widely.

4. **metadata** - 6 direct dependents. File metadata types used everywhere
   files are represented.

5. **fast_io** - 6 direct dependents. Platform I/O abstractions used by
   checksums, engine, transfer, core, daemon, and cli.

6. **core** - 4 direct dependents but is the orchestration hub. API changes
   here affect the binary, embedding crate, and daemon.

7. **engine** - 3 direct dependents (transfer, core, cli). Large crate with
   wide internal surface area.

## Feature Flag Interactions

Feature flags that propagate across crate boundaries:

### io_uring (Linux async I/O)
```
bin[io_uring] -> transfer/io_uring -> fast_io/io_uring (dep:io-uring)
bin[io_uring] -> fast_io/io_uring
```

### iocp (Windows async I/O)
```
bin[iocp] -> transfer/iocp -> fast_io/iocp
bin[iocp] -> fast_io/iocp
```

### client-tls (TLS for rsync:// connections)
```
bin[client-tls] -> core/client-tls (dep:rustls, dep:rustls-pemfile, dep:webpki-roots)
```

### zstd / lz4 / zlib-ng (compression)
```
bin[zstd] -> core/zstd -> engine/zstd + compress/zstd + transfer/zstd + protocol/zstd
bin[lz4]  -> core/lz4  -> engine/lz4  + compress/lz4  + transfer/lz4  + protocol/lz4
```

### acl / xattr (metadata preservation)
```
bin[acl]  -> cli/acl + core/acl -> metadata/acl + engine/acl + transfer/acl
bin[xattr] -> cli/xattr + core/xattr -> transfer/xattr
```

### parallel (multi-core)
```
bin[parallel] -> cli/parallel -> engine/lazy-metadata + checksums/parallel
```

### embedded-ssh (russh transport)
```
bin[embedded-ssh] -> core/embedded-ssh -> rsync_io/embedded-ssh (dep:russh, tokio)
```

### async (tokio runtime)
```
bin[async] -> daemon/async + core/async -> engine/async + transfer/async
```

### mmap-free-basis (experimental)
```
bin[mmap-free-basis] -> engine/mmap-free-basis + fast_io/mmap-free-basis
```

### sender-inc-recurse (deprecated, no-op)
```
bin[sender-inc-recurse] -> core/sender-inc-recurse + transfer/sender-inc-recurse
```

### openssl / openssl-vendored (checksum acceleration)
```
bin[openssl] -> checksums/openssl (dep:openssl)
bin[openssl-vendored] -> checksums/openssl-vendored -> openssl + openssl/vendored
```

## Layered Architecture Summary

```
Layer 4 (binaries):   bin, embedding
Layer 3 (apps):       cli, daemon
Layer 2 (orchestr.):  core
Layer 1 (subsystems): transfer, engine, flist
Layer 0 (shared):     protocol, metadata, checksums, filters, signature,
                      matching, batch, compress, bandwidth, fast_io, rsync_io,
                      logging, logging-sink, branding, platform, apple-fs
```

Changes to Layer 0 crates have the highest blast radius. Changes to Layer 3-4
crates are contained to the binary itself.
