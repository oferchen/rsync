# Changelog

All notable changes to oc-rsync are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project adheres
to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

oc-rsync is wire-compatible with upstream rsync 3.4.1 (protocol 32). Release
tags are mirrored on GitHub at <https://github.com/oferchen/rsync/releases>.

## [Unreleased]

### Documentation

- **SSH transport**: documented the opt-in `rsync_io/ssh-socketpair-stderr`
  Cargo feature - what it does (socketpair-backed SSH stderr instead of an
  anonymous pipe), why it exists (avoid deadlock when chatty remote children
  fill the 64 KiB pipe buffer), when to enable it, and platform constraints.
  Added `docs/ssh-transport.md` and cross-linked from the Cargo features
  table in `README.md` (#2377).

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

[Unreleased]: https://github.com/oferchen/rsync/compare/v0.6.1...HEAD
