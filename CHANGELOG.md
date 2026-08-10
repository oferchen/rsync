# Changelog

All notable changes to oc-rsync are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project adheres
to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

oc-rsync is wire-compatible with upstream rsync 3.4.3 (protocol 32). Release
tags are mirrored on GitHub at <https://github.com/oferchen/rsync/releases>.

## [0.6.3] - 2026-06-05

### Security

- SEC-1 status promoted to MOSTLY FIXED reflecting `.f/.g/.h/.i/.j/.k/.l/.m/.n` ship state (#4691)
- Partial-mitigation status for CVE-2026-29518 / CVE-2026-43619 via SEC-1 `*at` chain (SEC-1.o-partial) (#4672)
- `renameat` sandbox helper for atomic in-sandbox renames (SEC-1.j) (#4693)
- `fchmodat`/`fchownat`/`utimensat` sandbox helpers for metadata application (SEC-1.i) (#4690)
- `mkdirat`/`symlinkat`/`linkat` sandbox helpers for create-path operations (SEC-1.h) (#4683)
- Replace `remove_file`/`remove_dir` with `unlinkat` in `fast_io` + `transfer` (SEC-1.g) (#4671)
- Replace `lstat`/`symlink_metadata` with `fstatat(AT_SYMLINK_NOFOLLOW)` (SEC-1.f) (#4668)

### Features

- `pre-xfer exec` / `post-xfer exec` daemon directives with `RSYNC_ARG#` env vars and stdout capture (#5503)
- `--password-command` option for daemon authentication (#5500)
- Forward `--stop-at` deadline to remote server in SSH transfers (#5499)
- Forward `--remote-option` (`-M`) args to remote rsync process (#5498)
- Wire `--compress-threads` through transfer pipeline to zstd encoder (#5496)
- Embed filter rules in batch replay scripts (#5495)
- Wire `--info` subcategory dispatch to thread-local verbosity config (#5494)
- Parse missing upstream `rsyncd.conf` directives and warn on unknown keys (#5489)
- `--delay-updates` final rename sweep in remote receiver (#5398)
- `--partial` / `--partial-dir` file retention on interrupt (#5388)
- `--info=progress2` sliding-window rate, format, and parsing (#5382)
- Wire progress tracker into daemon transfer pipeline (#5383)
- `--ignore-missing-args` and `--delete-missing-args` flags (#5384)
- Handle invalid byte sequences in `FilenameConverter` (#5385)
- Handle progress2 interaction with `--outbuf` and terminal detection
- Stamp `mtime=0` on retained partial files for plain `--partial` (#5430)
- Negate modifier (`!`) for filter rules (#5426)
- Daemon-over-remote-shell mode for SSH with `::` operands (#5364)
- `--server --daemon` remote-shell daemon mode over stdio (#5353)
- `flush_workers`/`drain_inflight` barrier API on `ParallelDeltaApplier` (FFB-2) (#4665)
- Warn when `rsync --compress` meets SSH `-C` (double-compression detection, SSC-1) (#4667)
- Warn on SSH stderr socketpair-to-pipe fallback (SSF-2) (#4663)
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

- Align daemon `@ERROR` responses with upstream rsync wording (#5504)
- Forward `--trust-sender` and `--checksum-seed` to remote server (#5501)
- Wire `--contimeout` to embedded SSH (russh) connection path (#5497)
- Increase default daemon listen backlog from 5 to 128 (#5487)
- Suppress descendant matchers for anchored wildcard filter patterns (#5441)
- Build delta signature before backup rename to prevent false vanished error (#5440)
- Skip parent directory preparation in dry-run mode (#5439)
- Re-apply directory mtimes after transfer to prevent clobbering by child writes (#5442)
- Emit directory records before children in itemize output (#5432)
- Apply umask masking for chmod clauses without explicit who-specifier (#5428)
- Implement `dest_mode()` computation for non-preserve-perms transfers (#5427)
- Deduplicate repeated source operands to prevent duplicate transfers (#5425)
- Handle embedded `/./` markers in `--files-from` entries (#5433)
- Follow symlinks when emitting implied parent directories (#5436)
- Preserve directory mtime after deferred deletions (#5431)
- Force dry-run mode for `--only-write-batch` local transfers (#5424)
- Allow `--rsync-path` on local copies to match upstream behavior
- Gracefully skip daemon scenarios when upstream rsync cannot bind
- Remove erroneous CAP assertion from daemon config test (#5367)
- Align daemon module listing protocol with upstream behavior (#5366)
- Remove stale SEC-1.j TODO comments from completed task (#5365)
- Use socketpair instead of pipes for RSYNC_CONNECT_PROG child stdin (#5363)
- Detect inetd/connect-program stdin socket in standalone daemon (#5359)
- Build tls/getgroups helpers for upstream testsuite and remove last known failures (#5358)
- Run daemon protocol over stdio for remote-shell and connect-program modes (#5357)
- Add `build_capability_string_suffix` and remove ssh-basic from known failures (#5356)
- Embed capability string in compact flag string for server mode (#5352)
- Prevent deadlock in sync bridge multi-chunk wire parity test (#5351)
- Add `.nojekyll` to prevent Liquid template errors in GitHub Pages (#5349)
- Upstream testsuite hardlinks test compatibility (#5346)
- Resolve relative `OC_RSYNC_BIN` path in upstream testsuite runner (#5345)
- Remove chmod-temp-dir from upstream testsuite known failures (#5344)
- Export `setfacl_nodef` in upstream testsuite harness for ACL tests (#5343)
- Apply metadata before rename to match upstream `finish_transfer` semantics (#5338)
- Parse secluded-args and capability string from compact server flag string (#5336)
- Inherit `P_LOCAL` directives from global `rsyncd.conf` section into module context (#5334)
- Update clap error message assertion for clap 4.6 wording (#5331)
- Preserve atime independently of mtime in local copy metadata path (#5328)
- Unlink destination before cross-device copy in temp-dir fallback (#5327)
- Widen `open_daemon_stream` visibility for cross-module re-export (#5323)
- Use explicit builder in `to_builder_allows_modification` test (#5322)
- Align debug flag level tests with upstream clamping behavior (#5321)
- Wire `--old-args` through client config to unblock upstream 00-hello test (#5320)
- Clamp `--debug` flag levels to `MAX_OUT_LEVEL` instead of rejecting (#5319)
- Preserve original wire NDX for INC_RECURSE gap echo-back (#5318)
- Support `RSYNC_CONNECT_PROG` and double-colon syntax in daemon transport (#5317)
- Implement `-VV` JSON output and remove atimes from known failures (#5316)
- Gate `kqueue_stub` `c_int` import on non-unix only (#4429)
- Import `FileReader` trait for `IoUringFileReader::open` (#4452)
- Clippy compliance in `nvme_data_path` bench (#4454)

### Changed

- Enable parallel receive-delta by default via Path B heuristic (PIP-3 + PIP-5) (#4666)

### Refactoring

- Comment cleanup for daemon crate (#5362)
- Rename `apply_chunk_parallel` to `apply_one_chunk` for clarity (RJN-2) (#4660)
- Extract `spill/tempfile.rs` (SPL-3) (#4434)
- Channel-based drain shutdown for delete emitter (ATU-4) (#4401)
- MPE `traversal.rs` audit followup (#4380)
- Replace `lock().expect()` in `delete/emitter` (#4379)
- Replace `lock().expect()` in `delete/plan_map.rs` (#4375)
- Extract `spill/error.rs` (SPL-2) (#4345)
- Replace bare `io::ErrorKind::Other` with typed errors (#4377)

### Tests

- IP/CIDR host ACL allow/deny validation tests (#5502)
- `--partial` interrupt parity interop tests (#5480)
- Wire-byte parity for batched generator flush (#5463)
- Validate progress2 output format matches upstream rsync (#5392)
- `--delay-updates` sweep tests for remote transfer path (#5397)
- Interop test for no-partial mid-transfer temp file removal
- `--partial-dir` mid-transfer interrupt interop tests (#5395)
- Verify `mtime=0` partial files are not skipped by `--update` (#5389)
- Interop tests for `--partial` mid-transfer kill retention
- `--iconv=utf8,latin1` filename round-trip integration test
- `CleanupManager` integration tests for disk commit thread
- FFV-5/6/7 tests for `--files-from` vanished file handling
- `--iconv` with non-ASCII filter rules interop tests
- `--delay-updates` interrupt leaves files in partial-dir
- Comprehensive symlink-swap attack regression for SEC-1 sandbox (SEC-1.m) (#4675)
- Legitimate symlink transfers must not regress under SEC-1 sandbox (SEC-1.n) (#4678)
- Socketpair-to-pipe fallback warning fires exactly once (SSF-4) (#4684)
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

- User guide for partial file interrupt behavior (#5437)
- Document `--partial` interrupt semantics (#5399)
- Add interop compatibility status document (#5361)
- Publish interop compatibility status document (#5360)
- **SSH transport**: documented the opt-in `rsync_io/ssh-socketpair-stderr`
  Cargo feature - what it does (socketpair-backed SSH stderr instead of an
  anonymous pipe), why it exists (avoid deadlock when chatty remote children
  fill the 64 KiB pipe buffer), when to enable it, and platform constraints.
  Added `docs/ssh-transport.md` and cross-linked from the Cargo features
  table in `README.md` (#2377).
- Refresh spill layout and migration status (SPL-12) (#4394).
- Cross-platform CI hazard preflight audit (#4427).
- BR-6 beta-readiness sign-off check-in (#4692)
- Close WPG-1 as deferred to post-beta Windows hardware capture (#4688)
- Close PIP-4: interop suite exercises parallel-receive-delta path via PIP-5 default flip (#4689) [SUPERSEDED: PIP-7 (#4730) proved the dispatch scaffolding was a side-effect-only no-op; PIP-8 tore out the dead receiver-side wiring, and the proper integration is tracked by PIP-9]
- Close FFB-3/FFB-4/PIP-2 as satisfied by FFB-1 design + PIP-3+5 wire-up (#4677)
- Close RJN-4 as N/A after RJN-3 was rename-only (#4686)
- Defer RJN-3 (fanout) and RJN-4 (bench) as N/A after RJN-2 rename (#4676)
- Close ABW-3 as N/A pending per-file `Mutex` refactor (#4685)
- Defer ABW-2/3/4 pending BR-3j.f bench evidence (ABW-1 audit closure) (#4673)
- `apply_batch_parallel` verify-vs-write overlap audit (ABW-1) (#4670)
- Pre-frame IUS-4 SEND_ZC opt-in vs default-on decision (#4687)
- IORING_OP_SEND_ZC kernel compatibility matrix (IUS-2) (#4664)
- `--zero-copy` SEND_ZC build-time dependency note (IUS-1) (#4661)
- `flush_workers` barrier API design for `ParallelDeltaApplier` (FFB-1) (#4659)
- Token loop vs `ParallelDeltaApplier` migration surface audit (PIP-1) (#4657)
- `apply_chunk_parallel` call sites and per-chunk dispatch benefit audit (RJN-1) (#4656)
- SSH stderr socketpair-to-pipe fallback site audit (SSF-1) (#4658)
- Document `ssh-socketpair-stderr` feature and fallback warnings (SSF-3) (#4669)
- README warning for SSH+rsync double-compression (SSC-2) (#4655)
- Evaluate `ssh_config` parsers for SSC-3 double-compression detection (#4674)
- Formalize SEC-1.h `mknodat` deferral and document re-open triggers (#4694)
- Plan re-fold of SEC-1 `*at` helper modules post SEC-1.j ship (#4695)
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

- Add iconv feature to CI test matrix (#5386)
- Install `libxxhash-dev` and guard grep pipeline in upstream testsuite (#5350)
- Add upstream rsync testsuite workflow with UPASS detection (#5342)
- Standardize cache keys and add missing `CARGO_TERM_COLOR` (#5341)
- Align ci-skip interop job names with `ci.yml` check names (#5340)
- Fix ci-skip path filters to avoid overlap with `ci.yml` (#5339)
- Add `--no-tests=warn` to async-wire-parity workflow (#5337)
- Add nextest `--profile ci`, `--locked`, and missing timeouts (#5335)
- Pin all GitHub Actions to SHA hashes (#5333)
- Standardize cache keys on `Cargo.lock` (#5332)
- Fix daemon bench workflows using wrong package name (#5330)
- Fix xargs flag conflict and proc/status race in daemon concurrency CI (#5329)
- Remove job-level `if` conditions that broke push-triggered CI runs (#5326)
- Reduce runner contention by limiting non-required jobs to schedule (#5324)
- Matrix benchmark-release and harden `parallel_determinism` (#4443)
- Apply top quick wins from workflow audit (#4432)
- Weekly fuzz coverage report workflow (FCV-9) (#4403)
- mmap vs read_fixed+SQPOLL basis-read characterization bench (SMR-1) (#4387)
- Production io_uring path vs stdlib baseline bench (IUD-9) (#4398)

### Other Changes

- Triage environment-dependent upstream testsuite known failures (#5355)
- Triage environment-dependent upstream testsuite known failures as root (#5354)
- Format crtime test builder chain inline (#5325)
- Add SAFETY comments to the remaining 21 unsafe blocks (#4440)
- Consolidate cross-crate deps into `[workspace.dependencies]` (#4436)
- Gate Unix-only test modules and deny broken rustdoc links (#4430)

### Performance

- Add million-file RSS benchmark scaffold (#5478)
- Add DashMap concurrent-access benchmark scaffold (#5479)
- Add checksum wall-clock benchmark scaffold (#5476)
- Add daemon connection scaling benchmark scaffold (#5475)
- Add `copy_basis_range` benchmark scaffold (#5474)
- Add concurrent session scaling benchmark scaffold (#5473)
- Add bandwidth-constrained checksum benchmark scaffold (#5472)
- Add SEND_ZC zero-copy benchmark scaffold (#5477)
- Tune russh client config for faster SSH handshake (#5490)
- Optimize generator no-change scan path (#5466)
- Optimize no-change scan path for 100K-file scale (#5468)
- Eliminate redundant stat calls in metadata no-change path (#5492)
- Add `metadata_unchanged` fast-path for no-change generator scan (#5462)
- Unify multiplex flush discipline across transfer roles (#5464)
- Compact `FileEntry` from 88 to 80 bytes per entry (#5481)
- Reduce per-file overhead in SSH push no-change scan path (#5471)
- Eliminate redundant file reads in SSH push sender path (#5470)
- Eliminate redundant stat syscalls in SSH pull path (#5469)
- Implement remaining checksum overhead optimizations (#5465)
- Reclaim completed INC_RECURSE flist segments to reduce RSS (#5467)
- Increase checksum read buffer from 64KB to 256KB (#5460)
- Add BufReader wrapping for SSH pull read path (#5461)
- Remove intermediate BufReader from whole-file transfer (#5459)
- Tune mimalloc arena reservation and purge delay for lower RSS (#5488)
- Reuse readdir buffer across recursive directory traversal (#5484)
- Replace `Path::join` with `PathBuf::push/pop` in traversal (#5483)
- Eliminate heap allocations in `format_decimal_bytes` (#5486)
- Use move semantics for `ClientEvent` conversion (#5485)
- Pre-size `Vec<LocalCopyRecord>` to eliminate growth copies (#5482)
- Scaffold PIP-6 end-to-end parallel-vs-sequential bench harness (#4679)
- Scaffold BR-3j.f DashMap cores-vs-throughput re-bench harness (#4682)
- Scaffold IUS-3 SEND_ZC vs plain SEND bench harness (#4680)
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

[Unreleased]: https://github.com/oferchen/rsync/compare/v0.6.3...HEAD
[0.6.3]: https://github.com/oferchen/rsync/compare/v0.6.2...v0.6.3
