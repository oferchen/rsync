# Changelog

All notable changes to oc-rsync are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project adheres
to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

oc-rsync is wire-compatible with upstream rsync 3.4.1 (protocol 32). Release
tags are mirrored on GitHub at <https://github.com/oferchen/rsync/releases>.

## [Unreleased]

### Features

- IOCP concurrent_ops auto-size based on CPU count (#4358)
- IOCP BGID high-water mark with 50%-occupancy warning (#4355)
- Windows TransmitFile primitive behind `--features transmitfile` (#4334)
- Windows DACL/SACL SDDL round-trip support (#4354)
- ssh-socketpair-stderr connection primitive in `rsync_io` (#4348)
- SpillPolicy struct with `ConcurrentDeltaConfig` wiring (#4360)
- SpillableReorderBuffer wiring with parallel receive delta apply (#4319)
- lock_or_recover helpers for Mutex poison recovery in engine (#4342)

### Refactoring

- Decompose `delete/emitter.rs` into focused submodules (#4312)
- Decompose `buffer_pool/tests.rs` by concern (#4318)
- Decompose `io_uring_stub` into mirrored module layout (#4316)
- Split `io_uring/registered_buffers/tests.rs` by concern (#4320)
- Split `receiver/tests.rs` by surface area (#4315)
- Further split `receiver/tests/file_list` and `errors_and_timeouts` (#4321)
- Audit unsafe SAFETY comments and document workspace state (#4323)
- Add SAFETY comments to checksums SIMD unsafe blocks (#4325)
- Add SAFETY comments to `fast_io` unsafe blocks (#4328)

### Tests

- Windows source to Linux destination ACL round-trip (#4366)
- ACL/xattr round-trip parity with upstream rsync 3.4.1 (#4326)
- Add multiplex/flist/decompressor fuzz targets (#4336)
- Extend varint decode fuzz target (#4344)

### Documentation

- **SSH transport**: documented the opt-in `rsync_io/ssh-socketpair-stderr`
  Cargo feature - what it does (socketpair-backed SSH stderr instead of an
  anonymous pipe), why it exists (avoid deadlock when chatty remote children
  fill the 64 KiB pipe buffer), when to enable it, and platform constraints.
  Added `docs/ssh-transport.md` and cross-linked from the Cargo features
  table in `README.md` (#2377).
- Document ssh-socketpair-stderr feature (#4368)
- Document ssh-socketpair-stderr opt-in feature (#4385)
- SpillPolicy user-facing documentation (#4378)
- SpillPolicy public API and env-var surface design (#4340)
- SSH stderr audit and socketpair channel design (#4339)
- Spill mod.rs re-export audit (SPL-9) (#4390)
- spill.rs decomposition plan (#4337)
- BGID lifecycle architecture notes (#4353)
- BGID lifecycle and exhaustion risk audit (#4331)
- io_uring data path design - receive and send (#4349)
- io_uring data path coverage audit (#4343)
- mmap vs SQPOLL+READ_FIXED for basis reads design and bench scaffold (#4329)
- Mutex poison recovery policy (#4359)
- Mutex poison recovery classification audit (#4341)
- Arc::try_unwrap classification audit (#4338)
- Drain error recovery contract (#4356)
- IOCP synchronous blocking point audit (#4332)
- Windows IOCP profiling methodology audit (#4370)
- Windows hardlink ACL inheritance audit (#4350)
- Windows NTFS ACL support design (#4333)
- Document Windows NTFS ACL behaviour and lossy cases (#4346)
- Fuzz coverage matrix and gap analysis (#4335)
- Fuzz coverage gap followups (#4347)

### CI/Build

- Wire unsafe SAFETY comment audit as informational check (#4324)
- Nightly fuzz coverage report per target (#4351)
- Windows per-hotspot drilldown benchmark mode (#4352)
- Windows throughput comparison benchmark vs upstream MSYS2 rsync (#4327)
- Parallel-receive-delta perf benchmark for default-on decision (#4330)
- Consolidate common deps into workspace and align stale versions (#4322)

### Performance

- **Delta matching**: incorporated four zsync-inspired internal optimizations
  to the receiver's block-match path. All four are pure refactors of the
  in-memory match index - wire bytes, capability flags, sum-head fields, and
  golden-byte fixtures are unchanged, and transfers against upstream rsync
  3.0.9 / 3.1.3 / 3.4.1 remain byte-identical.
  - **bithash prefilter** ([#3737](https://github.com/oferchen/rsync/pull/3737),
    commit `3d0391d8`): a 32-bit one-sided bit array gates the strong-checksum
    lookup so non-matching rolling-hash windows are rejected before any
    hashtable probe. Mirrors zsync's `librcksum/rsum.c` bithash gate and
    eliminates roughly seven of every eight post-tag-table misses on the hot
    path.
  - **sequential-match extension** ([#3751](https://github.com/oferchen/rsync/pull/3751),
    commit `6122b507`): after a confirmed block match the receiver attempts to
    extend the run by checking consecutive basis blocks directly, avoiding
    re-entry into the rolling-hash loop while a contiguous span of basis
    blocks keeps matching.
  - **matched-block pruning** ([#3748](https://github.com/oferchen/rsync/pull/3748),
    commit `aa7eb8a4`): once a basis block is consumed by a match it is
    removed from the lookup table so later windows skip duplicate probes.
    Mirrors zsync's `librcksum` post-match prune; duplicate basis blocks are
    handled by the existing strong-checksum gate.
  - **compact-key layout** ([#3994](https://github.com/oferchen/rsync/pull/3994),
    commit `58860a82`): replaces the pointer-chasing
    `FxHashMap<(u16, u16), Vec<usize>>` with a flat open-addressing table
    keyed by packed `(rsum_low, bucket_idx)` entries, giving sequential probes
    cache-friendly access and removing per-bucket heap allocations.

[Unreleased]: https://github.com/oferchen/rsync/compare/v0.6.2...HEAD
