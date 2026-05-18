# Changelog

All notable changes to oc-rsync are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project adheres
to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

oc-rsync is wire-compatible with upstream rsync 3.4.1 (protocol 32). Release
tags are mirrored on GitHub at <https://github.com/oferchen/rsync/releases>.

## [Unreleased]

### Features

- Adaptive per-file basis-read dispatch in `fast_io` (SMR-3c) (#4441)
- mmap-to-io_uring size threshold dispatch in `fast_io` (SMR-3b) (#4435)
- Wire `SpillGranularity::PerItem` in spill write path (STN-5) (#4428)
- `--spill-dir` and `--spill-threshold-bytes` CLI flags (STN-11) (#4423)
- io_uring file reader behind `iouring-data-reads` feature (IUD-6) (#4410)
- Mark `ssh-socketpair-stderr` as opt-in feature with default-path test (SSE-5) (#4389)
- Env-var overrides for `SpillPolicy` (STN-8/9/10) (#4404)
- Graceful BGID exhaustion fallback with typed error (BGE-6) (#4391)
- Wire `--acls` to Windows DACL (#4388)
- `IORING_OP_SEND_ZC` behind `iouring-send-zc` feature (IUD-7) (#4422)
- `SpillCompression::Zstd` behind `spill-compression` feature (STN-7) (#4416)
- Page-aligned `BufferPool` for IOCP no-buffering (#4374)
- `SpillPolicy.reclaim`: `KeepInMemory` vs `RespillAfterRead` (STN-4) (#4400)
- Typed error variants for `Arc::try_unwrap` failure paths (#4357)
- Opt-in io_uring data-write dispatch for large files (IUD-5) (#4397)
- mmap-free-basis experimental feature in `fast_io` (SMR-3a) (#4438)
- RSS-aware spill trigger (STN-6) (#4421)
- Async stderr drain task for SSH socketpair (#4363)

### Bug Fixes

- Gate `kqueue_stub` `c_int` import on non-unix only (#4429)
- Import `FileReader` trait for `IoUringFileReader::open` (#4452)
- Clippy compliance in `nvme_data_path` bench (#4454)

### Refactoring

- Extract `spill/tempfile.rs` (SPL-3) (#4434)
- Channel-based drain shutdown for delete emitter (ATU-4) (#4401)
- MPE `traversal.rs` audit followup (#4380)
- Replace `lock().expect()` in `delete/emitter` (#4379)
- Replace `lock().expect()` in `delete/plan_map.rs` (#4375)
- Extract `spill/error.rs` (SPL-2) (#4345)
- Replace bare `io::ErrorKind::Other` with typed errors (#4377)

### Tests

- Re-enable stale ignored tests and remove obsolete entries (#4431)
- Windows source to Linux destination ACL round-trip (WAS-7) (#4420)
- Env-var driven E2E spill integration test (STN-14) (#4408)
- Byte-identical regression for io_uring data path (IUD-8) (#4395)
- Isolated unit tests per `SpillPolicy` knob (STN-13) (#4393)
- Fuzz targets for `rsyncd.conf`, auth response, incremental flist (FCV-3) (#4444)
- Thread panic recovery for delete pipeline (MPE-10) (#4376)
- 100K session BGID leak stress (#4373)
- Extend filter parser fuzz edge cases (#4371)
- `NegotiationPrologueSniffer` pre-auth fuzz target (FCV-3 P0) (#4367)
- Legacy greeting + version negotiation fuzz target (#4414)
- Daemon `@RSYNCD` greeting parser fuzz target (FCV-3 P0) (#4409)
- Extend varint decode fuzz target with round-trip (FCV-5) (#4405)

### Documentation

- **SSH transport**: documented the opt-in `rsync_io/ssh-socketpair-stderr`
  Cargo feature - what it does (socketpair-backed SSH stderr instead of an
  anonymous pipe), why it exists (avoid deadlock when chatty remote children
  fill the 64 KiB pipe buffer), when to enable it, and platform constraints.
  Added `docs/ssh-transport.md` and cross-linked from the Cargo features
  table in `README.md` (#2377).
- Refresh spill layout and migration status (SPL-12) (#4394).
- Cross-platform CI hazard preflight audit (#4427).
- Runnable Windows IOCP vs MSYS2 profiling methodology (WPG-1) (#4442)
- SPL-8 still blocked until SPL-3/4 merge (#4439)
- Workspace dependency consolidation opportunities (#4425)
- Workspace rustdoc coverage audit (#4424)
- CI workflow hazards and quick wins (#4419)
- Catalogue ignored tests with re-enable recommendations (#4418)
- mmap-vs-SQPOLL decision framework (SMR-2) (#4417)
- SPL-10 enforce-limits audit (#4413)
- Record recent series completions in agents notes (#4411)
- FCV-3 protocol-parsing fuzz coverage gaps (#4407)
- Windows ACL behavior for `--acls` (WAS-8) (#4406)
- mmap-vs-SQPOLL status table and SHIPPED marker (SMR-5) (#4402)
- WAS-6 Windows hardlink ACL inheritance (#4399)
- Module-level rustdoc on spill submodules (SPL-11) (#4392)
- Add `///` on `pub mod` declarations, round 1 (#4437)
- SMR-4 regression strategy for SQPOLL-on-large-deltas test (#4433)
- Add `///` on remaining `pub mod` declarations, round 2 (#4449)
- Rolling SIMD checksum-sync regression hypothesis (CSP-1) (#4450)
- PRC-3a DACL-POSIX overlap analysis (#4453)

### CI/Build

- Matrix benchmark-release and harden `parallel_determinism` (#4443)
- Apply top quick wins from workflow audit (#4432)
- Weekly fuzz coverage report workflow (FCV-9) (#4403)
- mmap vs read_fixed+SQPOLL basis-read characterization bench (SMR-1) (#4387)
- Production io_uring path vs stdlib baseline bench (IUD-9) (#4398)

### Other Changes

- Add SAFETY comments to the remaining 21 unsafe blocks (#4440)
- Consolidate cross-crate deps into `[workspace.dependencies]` (#4436)
- Gate Unix-only test modules and deny broken rustdoc links (#4430)

### Performance

- Keep rolling `s1`/`s2` in SIMD registers across stripe (CSP-2 F1) (#4451)
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
