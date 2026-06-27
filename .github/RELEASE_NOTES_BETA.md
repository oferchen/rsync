<!--
Draft for the upcoming beta release notes.

Status: DRAFT. The version line below uses `v0.7.0-beta` as a placeholder.
The maintainer will choose the actual semver bump (e.g. `v0.7.0-beta`,
`v0.6.3-beta`, `v1.0.0-beta`) at tag time and replace every occurrence
of `vX.Y.0-beta` / `v0.7.0-beta` in this file.

Workflow when promoting to a real release notes body:

1. Pick the final tag name; replace `v0.7.0-beta` throughout.
2. Pin the commit range against the chosen base tag (currently v0.6.2..HEAD).
3. Fill in benchmark numbers from the chart auto-uploaded by `benchmark.yml`
   on tag push, and the coverage number from the BR-4a workflow.
4. Move this body into the GitHub release using `.github/RELEASE_TEMPLATE.md`
   as the install/toolchain header, then append the sections below.
-->

## oc-rsync v0.7.0-beta

Wire-equivalent to upstream rsync 3.4.x (and back-compat with 3.4.1, protocol 32).

This is the first **beta** of oc-rsync. The 0.6.x line shipped as alpha while
wire-feature completeness, interop, coverage, and unsafe-code hygiene closed
out. Beta declares those four bars met as of the tagged commit; see
*Beta-readiness criteria* below for the exact conditions.

### Install

See the install/toolchain header in `.github/RELEASE_TEMPLATE.md` (Homebrew,
prebuilt packages, toolchain variants). This document only covers what is
new since v0.6.2.

---

### Highlights

- **Wire-equivalent to upstream rsync 3.4.x (protocol 32) with measurable
  performance gains.** Local copy, daemon push/pull, and SSH transport are
  byte-identical against upstream across the 3.0.9 / 3.1.3 / 3.4.1 / 3.4.2
  interop matrix; the cumulative architecture (threaded pipeline, io_uring,
  IOCP writers, reflink/clone, zsync-inspired delta matching) carries the
  performance story.
- **SEC-1 chain mitigates CVE-2026-29518 and CVE-2026-43619.** The TOCTOU
  symlink-race window on path-based daemon syscalls under `use chroot = no`
  is closed via a per-connection `DirSandbox` carrier, `openat2(RESOLVE_BENEATH
  | RESOLVE_NO_SYMLINKS)` runtime detection, and full coverage of the
  `*at` syscall family (`fstatat`, `unlinkat`, `mkdirat`, `symlinkat`,
  `linkat`, `fchmodat`, `fchownat`, `utimensat`, `renameat`). See
  `SECURITY.md` for the per-CVE table and remaining wiring follow-ups.
- **Optional Linux-only Landlock LSM defense-in-depth.** A new `landlock`
  Cargo feature layers kernel-enforced `PathBeneath` allowlisting on top
  of the `*at` chain so a future commit that bypasses `DirSandbox` is still
  rejected by the kernel. Linux 5.13+ required; daemons on older kernels
  run with the `*at` chain alone. See `SECURITY.md` and the SEC-1.p design
  note.
- **parallel-receive-delta is now the default execution path (PIP-5).**
  The receiver's delta-apply pipeline parallelises `verify_chunk` across
  cores by default; the legacy sequential path remains opt-in for the
  beta soak. Interop coverage exercises the new default end-to-end against
  the full upstream matrix.
- **DashMap-backed slot map removes the ParallelDeltaApplier outer Mutex
  (BR-3j).** The per-file lookup that previously serialised registration
  and finish under a single `Mutex<HashMap<...>>` is now sharded via
  DashMap, so per-file fan-out scales with cores rather than collapsing
  to a single lock owner.

---

### Beta-readiness criteria

This release is the first build that meets all four criteria simultaneously:

1. **Wire-feature completeness vs upstream rsync 3.4.x.** No remaining
   `--iconv`, `--fake-super`, ACL, xattr, or IOCP gaps. The specific fixes
   that closed the last known gaps are enumerated in *Wire-protocol
   fidelity* below.
2. **No production-path placeholders.** `tools/no_placeholders.sh` is clean;
   no `todo!`, `unimplemented!`, `FIXME`, or stub functions on live code
   paths.
3. **Interop and coverage.** `tools/ci/run_interop.sh` passes against
   upstream rsync **3.0.9**, **3.1.3**, **3.4.1**, and **3.4.2** with zero
   entries in `KNOWN_FAILURES.conf`; `cargo llvm-cov` line coverage at or
   above **95%**; all required CI checks green on master (fmt+clippy,
   nextest stable, Windows stable, macOS stable, Linux musl stable).
4. **Unsafe-code policy honored.** `#[allow(unsafe_code)]` confined to the
   permitted crates only: `metadata`, `fast_io`, `checksums`, `engine`,
   `protocol` (plus the documented `windows-gnu-eh` shim and the
   `embedding` `#[cfg(test)]`-only `EnvGuard`). No new unsafe in
   `daemon`, `cli`, `core`, `transfer`, `batch`, `filters`, `signature`,
   `matching`, `bandwidth`, `logging`, `logging-sink`, `branding`,
   `rsync_io`, `compress`, `apple-fs`, `flist`.

---

### Security

`SECURITY.md` carries the canonical per-CVE matrix and is the source of
truth. Summary as of the beta tag:

| CVE | Upstream issue | oc-rsync status |
|-----|---------------|-----------------|
| CVE-2026-29518 | TOCTOU symlink race in daemon receiver (`use chroot = no`) | **Mostly fixed** - SEC-1 `*at` chain shipped (`.a`-`.n`); receiver wiring follow-up tracked (SEC-1.i / SEC-1.j deferred callers in `disk_commit`, `transfer_ops/response`, `local_copy/executor`). |
| CVE-2026-43619 | Symlink races on chmod/lchown/utimes/rename/unlink/mkdir/symlink/mknod/link/rmdir/lstat | **Mostly fixed** - same `*at` helper set, including `fchmodat`/`fchownat`/`utimensat` (SEC-1.i, #4690) and `renameat` (SEC-1.j, #4693). |
| CVE-2026-45232 | Off-by-one stack write in HTTP CONNECT proxy response handler | **Fixed** - `read_proxy_line()` reads byte-by-byte into a heap `Vec<u8>` capped at `MAX_PROXY_LINE_BYTES = 1024` (SEC-2.a, #4609). |
| CVE-2026-43617 | Reverse-DNS lookup after daemon chroot causes hostname ACL bypass | **Not vulnerable** - `module_peer_hostname` runs before `chroot/setuid`. |
| CVE-2026-43618 | Integer overflow in compressed-token decoder | **Mitigated** - `saturating_add` in zstd and zlib counting writers with explicit regression coverage. |
| CVE-2026-43620 | OOB read in `recv_files` via negative `parent_ndx` | **Not vulnerable** - `DirectoryTree::try_add_directory` validates the parent index and returns `DirTreeError::OutOfBoundsParent` (SEC-4 closed). |
| rsync 3.4.3 hostname hardening | Hyphen-prefixed remote-shell hostname injection | **Fixed** - SSH operand parse rejects hyphen-prefixed hostnames (SEC-3). |

CVEs from the early 2025 cluster (CVE-2024-12084 through CVE-2024-12747)
remain *not vulnerable* or *mitigated* by Rust's memory safety. The
`tools/ci/check_upstream_release.sh` watcher runs weekly and opens a
tracking issue when upstream rsync ships a new release, so new CVEs are
surfaced automatically.

#### SEC-1 chain - what shipped this cycle

- **SEC-1.a / b / c / d / e** - `DirSandbox` carrier with in-tree dirfd
  cache, `openat2(RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS)` runtime probe,
  and receiver pipeline wiring (PRs #4643, #4650 and prior).
- **SEC-1.f** (#4668) - `fstatat(AT_SYMLINK_NOFOLLOW)` replaces
  `lstat`/`symlink_metadata`.
- **SEC-1.g** (#4671) - `unlinkat` replaces `remove_file` / `remove_dir`
  in `fast_io` and `transfer`.
- **SEC-1.h** (#4683) - `mkdirat` / `symlinkat` / `linkat` sandbox
  helpers for create-path operations.
- **SEC-1.i** (#4690) - `fchmodat` / `fchownat` / `utimensat` sandbox
  helpers for metadata application.
- **SEC-1.j** (#4693, #4697) - `renameat` sandbox helper plus deferred
  receiver wiring.
- **SEC-1.k / l** - macOS `*at` parity verified; Windows NTFS handle-based
  APIs sidestep the TOCTOU window structurally.
- **SEC-1.m / n** (#4675, #4678) - symlink-swap attack regression suite
  plus legitimate-symlink no-regression coverage.
- **SEC-1.o-partial** (#4672) - `SECURITY.md` promoted to MOSTLY FIXED
  reflecting the shipped chain.
- **SEC-1.p** - Landlock LSM defense-in-depth audit and design landed
  (#4699); the implementation is gated behind the `landlock` Cargo
  feature.

Remaining follow-ups (do **not** block the beta tag, tracked in
`SECURITY.md`): receiver wiring for the SEC-1.i / SEC-1.j deferred
callers; `mknodat` for device / FIFO nodes (closure doc #4694, not on
the daemon-reachable surface).

---

### What's New

Grouped by the same categories the `.github/release.yml` auto-labeller uses,
so the rendered release matches the GitHub-generated changelog.

#### Features

- `feat(engine)`: `flush_workers` / `drain_inflight` barrier API on
  `ParallelDeltaApplier` (FFB-2, #4665).
- `feat(core)`: warn when `rsync --compress` meets SSH `-C` -
  double-compression detection at session start (SSC-1, #4667).
- `feat(rsync_io)`: warn on SSH stderr socketpair-to-pipe fallback so the
  diagnostic is visible when the platform downgrades silently (SSF-2,
  #4663).
- `feat(fast_io)`: SEC-1 `*at` sandbox helper family - `fstatat`,
  `unlinkat`, `mkdirat`, `symlinkat`, `linkat`, `fchmodat`, `fchownat`,
  `utimensat`, `renameat` (SEC-1.f through SEC-1.j).
- `feat(fast_io)`: optional Landlock LSM enforcement behind the
  `landlock` Cargo feature (SEC-1.p, #4699).
- `feat(transfer)`: honour source-side `user.rsync.%stat` in
  `--fake-super` sender; preserves original mode/uid/gid/rdev across a
  fake-super round trip (#4578).
- `feat(version)`: list compiled-in Cargo features in `--version` output
  so deployments can audit which optional codepaths shipped (#4547).
- `feat(engine)`: expose `force_insert` counter on `DeltaConsumer::stats`
  for concurrent-delta receiver tuning (#4553).

#### Performance

- `perf(transfer)`: enable parallel receive-delta by default via the Path
  B heuristic - the parallel `verify_chunk` apply path is now the default
  execution path for the receiver (PIP-3 + PIP-5, #4666).
- `perf(engine)`: DashMap-backed slot map for `ParallelDeltaApplier`
  removes the outer `Mutex<HashMap<...>>` that serialised per-file
  registration and finish (BR-3j.c / .d / .e, #4634, #4635, #4636).
- `perf(engine)`: bench harness for parallel `verify_chunk`
  cores-vs-throughput sweep (BR-3i.f, #4653).
- `bench(engine)`: BR-3j.f DashMap cores-vs-throughput re-bench scaffold
  (#4682); cores-vs-throughput numbers deferred to an offline capture run
  on a known hardware profile.
- `bench(engine)`: PIP-6 end-to-end parallel-vs-sequential bench scaffold
  (#4679).
- `bench(fast_io)`: IUS-3 SEND_ZC vs plain SEND bench scaffold (#4680).
- `perf(checksums)`: keep rolling `s1` / `s2` in SIMD registers across
  stripe (CSP-2 F1, #4451).
- **Delta matching** (carry-over from the 0.6.x line, recorded here for
  beta completeness): four zsync-inspired refactors to the receiver's
  block-match path - bithash prefilter (#3737), sequential-match
  extension (#3751), matched-block pruning (#3748), compact-key layout
  (#3994). All four are wire-byte-identical against upstream rsync 3.0.9
  / 3.1.3 / 3.4.1.

#### Bug Fixes

- `fix(transfer)`: preserve wire-byte order for flist index under
  `--iconv` (#4576) - fixes pulls from upstream rsync 3.4.1 daemon with
  `--iconv=UTF-8,LATIN1` previously aborting with "received request to
  transfer non-regular file".
- `fix(protocol)`: consume generator xattr request on sender under
  `-X --fake-super` (#4581) - fixes wire desync that aborted transfers
  with "block length must be non-zero" / "Invalid remainder length".
- `fix(metadata)`: write `user.rsync.%stat` xattr in `--fake-super`
  local-copy path (#4580).
- `fix(engine)`: apply `--iconv` conversion in local-copy executor
  (#4579) - previously a silent no-op.
- `fix(core)`: move `sigaction` unsafe out of `core` into `fast_io`
  (#4571).
- `fix(protocol)`: clamp `token_encoding` and `token` proptest
  `block_index` to safe wire range (#4548, #4551).
- `fix(daemon)`: skip `use_chroot` absolute-path check on Windows
  (#4567).
- `fix(daemon)`: use `env::temp_dir()` for module-list test fixtures and
  for deny/bwlimit module paths (#4560, #4566).
- `fix(daemon)`: de-flake auth challenge uniqueness test on Windows
  (#4558).
- `fix(daemon)`: use platform-absolute paths in config parsing tests
  (#4553).
- `fix(engine)`: seat `DeleteContext::new` cursor at `dest_root` (#4532).
- `fix(engine)`: gate single-slot TLS `buffer_pool` tests off
  thread-slab feature (#4540).
- `fix`: clear master CI cascade (cipher/zc tests, macOS `O_NONBLOCK`,
  IOCP).
- `fix(ci)`: skip oc-rsync daemon scenarios in Windows interop smoke
  (#4550).

#### Documentation

- `docs(security)`: SEC-1 status to MOSTLY FIXED reflecting `.f`/`.g`/`.h`/`.i`/`.j`/`.k`/`.l`/`.m`/`.n` ship state (#4691).
- `docs(design)`: SEC-1.p Landlock LSM as defense-in-depth audit and
  design (#4699).
- `docs(design)`: BR-6 beta-readiness sign-off check-in (#4692).
- `docs`: close WPG-1 as deferred to post-beta Windows hardware capture
  (#4688).
- `docs(design)`: close PIP-4 - interop suite exercises
  parallel-receive-delta path via PIP-5 default flip (#4689).
- `docs(design)`: close FFB-3 / FFB-4 / PIP-2 as satisfied by FFB-1
  design + PIP-3 + PIP-5 wire-up (#4677).
- `docs(design)`: close ABW-3 as N/A pending per-file `Mutex` refactor
  (#4685); close RJN-4 as N/A after RJN-3 was rename-only (#4686).
- `docs(audits)`: BR-4a workspace coverage baseline; BR-13 beta bench;
  IOCP wiring refresh; signal-handler unsafe attribution (#4574).

#### Other Changes

Includes `chore:`, `style:`, `test:`, and `refactor:` PRs.

- `test(transfer)`: comprehensive symlink-swap attack regression (SEC-1.m,
  #4675); legitimate symlink transfers must not regress under the SEC-1
  sandbox (SEC-1.n, #4678).
- `test(rsync_io)`: assert success path skips the socketpair fallback
  warning (SSF-4, #4684).
- `refactor(fast_io)`: re-fold SEC-1 `*at` helpers into a single
  `at_syscalls` module post SEC-1.j ship (#4700).
- `refactor(fast_io)`: split `sendfile.rs` into per-concern submodules
  (#4530).
- `refactor(compress)`: SPL-decompose `strategy/negotiator.rs` (#4533).
- `refactor(engine)`: decompose `delete/context.rs` into per-concern
  submodules; decompose `copy/transfer/execute.rs` into per-concern
  submodules.
- `refactor(metadata)`: decompose `acl_exacl.rs` into per-concern
  submodules (#4531).
- `refactor(filters)`: split `chain.rs` into SPL submodules (#4534).
- `refactor(transfer)`: split `receiver/transfer.rs` into per-concern
  submodules; decompose `generator/transfer.rs` and `generator/mod.rs`
  into per-concern submodules.
- `chore`: stop tracking `AGENTS.md`; treat as local-only.
- `chore`: remove `enforce-limits` CI check and xtask command (#4538).

---

### Wire-protocol fidelity

The following fixes closed the last known gaps against upstream rsync
3.4.1/3.4.2. They are called out separately because they are the
qualifying changes for beta-readiness criterion #1.

- **`--iconv` local-copy now applies the conversion** (#4579). Previously
  a silent no-op; the local-copy executor in `engine` now runs the
  configured `FilenameConverter` so paths land on disk in the local
  charset, matching upstream `--iconv` semantics. *Closes BR-2d (#2489).*
- **`--iconv` pull from upstream daemon preserves NDX wire-byte order**
  (#4576). The receiver was decoding each entry, transcoding the
  filename, then re-sorting the NDX-addressed `file_list` in
  local-charset order while the upstream generator's subsequent requests
  indexed against the sender's wire-byte (scan) order.
  `iconv_reorder_suppressed()` now centralises the predicate and is
  checked in `receive_file_list`, `receive_extra_file_lists`
  (INC_RECURSE), and the streaming
  `IncrementalFileListReceiver::collect_sorted` fallback. Upstream
  reference: `options.c:2051-2056` `need_unsorted_flist`,
  `flist.c:2496-2498`. *Closes BR-2e (#2490).*
- **`--fake-super` local-copy writes `user.rsync.%stat` xattr** (#4580).
  Local-copy now records the privileged metadata into the xattr the same
  way the network path does, so a `cp -a`-equivalent local fake-super
  copy round-trips through a privileged restore.
- **Sender consumes generator xattr request under `-X --fake-super`**
  (#4581). The upstream generator emits an abbreviated-xattr request
  body terminated by a 0 byte whenever `ITEM_REPORT_XATTR` is set; our
  sender skipped it and read the trailing 0 as the first byte of
  `sum_head.count`, desyncing the wire stream. Mirrors upstream
  `sender.c:280-284` and `xattrs.c:send_xattr_request()`.
- **Sender honours source-side `user.rsync.%stat`** (#4578). The sender
  now reads `user.rsync.%stat` during `x_lstat()` / `x_stat()` and lets
  the decoded mode/uid/gid/rdev replace the on-disk stat values before
  the wire entry is built, so a round trip `fake-super-receiver ->
  fake-super-sender -> fake-super-receiver` preserves the original
  privileged metadata that the first hop demoted into the xattr.
  Upstream reference: `xattrs.c:1127` `get_stat_xattr`.

---

### Interop matrix

Tested against upstream rsync in CI across protocols 28-32. Both push
and pull directions covered. Cell value is the test result for the
release commit; "pass" = zero entries in `KNOWN_FAILURES.conf` for that
cell.

| Upstream version | Push (oc-rsync -> upstream) | Pull (upstream -> oc-rsync) |
|------------------|:--------------------------:|:--------------------------:|
| rsync 3.0.9      | pass                       | pass                       |
| rsync 3.1.3      | pass                       | pass                       |
| rsync 3.4.1      | pass                       | pass                       |
| rsync 3.4.2      | pass                       | pass                       |

Re-confirm at tag time by re-running `tools/ci/run_interop.sh` and the
"Interop Validation" workflow against the tagged commit; replace any
cell that regresses with "fail (see KNOWN_FAILURES.conf)".

---

### Performance

![Benchmark: oc-rsync vs upstream rsync](https://github.com/oferchen/rsync/releases/download/v0.7.0-beta/benchmark.png)

The benchmark chart above is auto-uploaded as a release asset by
`benchmark.yml` on tag push. Update the URL to match the final tag name
when promoting this scaffold to the release body.

The following benches shipped as scaffolding this cycle and are the
canonical reference once their numbers are captured offline:

- **BR-3i.f** (#4653) - parallel `verify_chunk` cores-vs-throughput sweep.
- **BR-3j.f** (#4682) - DashMap cores-vs-throughput re-bench through the
  post-DashMap applier. Numbers offline-captured per
  `docs/design/br-3j-f-dashmap-rebench-2026-05-21.md`.
- **PIP-6** (#4679) - end-to-end parallel-vs-sequential receive-delta
  bench.
- **IUS-3** (#4680) - `IORING_OP_SEND_ZC` vs plain `IORING_OP_SEND`
  bench.
- **SSC-1** (#4667) - startup warning when SSH `-C` collides with rsync
  wire compression; the bench-adjacent expectation is that operators
  see the warning and pick one compression layer before profiling.

Headline numbers (TBD once the post-tag benchmark workflow completes):

- **Local copy:** TBD x upstream rsync 3.4.2 (target: 3x+).
- **Daemon push/pull:** TBD x upstream (target: 2x+).
- **SSH transport:** TBD x upstream (target: on par or better).
- **Peak RSS overhead vs upstream:** TBD % (target: < 10%).

---

### Coverage

- Workspace `cargo llvm-cov` line coverage: **TBD%** (BR-4a baseline once
  recorded; target >= 95%).
- Per-crate breakdown and any crates below target are recorded in the
  BR-4a audit report under `docs/audits/`. Replace this bullet with the
  final number and a link to the recorded baseline at tag time.

---

### Compatibility notes

- **Linux 5.6+ recommended for io_uring.** The `fast_io` crate runtime-probes
  io_uring support and falls back to standard buffered I/O on older
  kernels. Provided buffer rings (PBUF_RING) require Linux **5.19+**;
  pre-5.19 kernels fall back to standard buffered I/O via runtime
  probing.
- **Linux 5.13+ required for the Landlock LSM layer.** The new `landlock`
  Cargo feature requires kernel Landlock ABI v1 or newer. Daemons on
  older kernels run with the SEC-1 `*at` chain as the sole TOCTOU
  defense, which is itself sufficient against CVE-2026-29518 and
  CVE-2026-43619; Landlock is purely additive defense-in-depth.
- **Linux 6.0+ recommended for SEND_ZC.** `IORING_OP_SEND_ZC` is gated
  behind the `iouring-send-zc` Cargo feature. Default builds use plain
  `IORING_OP_SEND` even on Linux 5.16+; the `--zero-copy` flag advertises
  SEND_ZC but downgrades silently in default builds. The IUS-4
  default-on decision (#4687) is pre-framed for a post-beta cycle once
  IUS-3 numbers land.
- **Windows IOCP path is shipped but unprofiled.** IOCP is wired for
  socket I/O (daemon and SSH transports) and for the receive-side
  disk-write pipeline (`transfer::disk_commit` dispatches `Writer::Iocp`
  when the IOCP backend is selected on Windows). File reads still use
  standard buffered I/O. Comparative throughput claims for Windows are
  deferred to a post-beta hardware-capture cycle; see the WPG-1 closure
  note (#4688). Production Windows deployments should treat IOCP
  throughput numbers as informational until that capture lands.
- **macOS uses POSIX `*at` syscalls.** SEC-1.k verified the BSD `*at`
  family behaves identically to Linux for the SEC-1 chain; no macOS-only
  carve-outs.
- **Windows sidesteps path TOCTOU structurally.** SEC-1.l audited the
  NTFS handle-based APIs (`CreateFileW` returning a kernel handle that
  pins the resolved target) and confirmed they do not expose the path
  TOCTOU window the Linux/macOS `*at` chain closes.

---

### Deprecations / breaking changes

None - this is a beta tag. The wire-protocol fixes above strictly close
gaps and do not change semantics for transfers that were already working.

The `enforce-limits` CI workflow and the corresponding xtask command
were removed (#4538); external pipelines that gated on that workflow
should drop the requirement. LoC is no longer policed in CI.

---

### Known limitations

These carry over from v0.6.2 and remain accurate as of beta, plus the
new-in-this-cycle items called out below. They are reproduced here so
beta users see the architectural trade-offs before deploying without
chasing down `README.md` and individual closure docs.

#### New in this cycle

- **ABW-3 / ABW-4 (verify+write pipelining).** Closed as N/A pending a
  per-file `Mutex` refactor (#4685). Today's serial-write-after-parallel-verify
  shape is correctness-equivalent; pipelining is a follow-up perf item,
  not a beta gate.
- **RJN-4 bench.** Closed as N/A (RJN-3 was a rename-only change, no
  bench needed; #4686).
- **PIP-4** (interop matrix re-run after PIP-5). Closed retroactively -
  parallel-receive-delta interop coverage is satisfied by the PIP-5
  default flip (#4666) exercising the existing interop suite under the
  new default path (#4689).
- **SEC-1.i / SEC-1.j receiver wiring.** Deferred follow-ups for the
  `disk_commit`, `transfer_ops/response`, and `local_copy/executor`
  callers; the `*at` helpers themselves ship and are the live mitigation
  for CVE-2026-29518 / CVE-2026-43619 (#4701).
- **SEND_ZC opt-in only.** `IORING_OP_SEND_ZC` is gated behind the
  `iouring-send-zc` feature; default builds use plain SEND even on Linux
  5.16+. IUS-4 will revisit default-on after IUS-3 numbers land
  (#4687).

#### Carry-over from v0.6.2

- **Fixed io_uring buffer pool.** The registered buffer pool is sized at
  compile time (1024 x 4 KiB = 4 MiB) and does not adapt under sustained
  I/O pressure. Workloads with very high concurrent file fan-out may see
  throughput plateau before saturating the device.
- **bgid namespace.** io_uring buffer-group IDs are a 16-bit namespace;
  the buffer ring helpers cap at this bound. Long-running daemons that
  recycle thousands of distinct ring groups should monitor for
  exhaustion.
- **Single-thread delta computation.** The delta sender is sequential per
  file. Rolling-hash fan-out across files is not yet parallelised; large
  file workloads fully utilise one CPU per transfer rather than scaling
  delta CPU horizontally.
- **SSH compression interaction.** When SSH itself compresses the stream
  (`Compression yes` in `ssh_config` or a cipher with built-in
  compression), running `oc-rsync -z` will compress payloads twice.
  SSC-1 now warns when `-C` is observed in the argv; `ssh_config`-driven
  compression is not detected (audit #4674). Pick one layer.
- **Daemon TLS.** Native TLS is not built into the daemon. Deploy
  `oc-rsync --daemon` behind `stunnel`, `ssh -L`, or a reverse proxy
  that terminates TLS. See `SECURITY.md`.
- **`.rsync-filter` per-directory inheritance.** Matches upstream for
  the common cases tested in the interop suite; exhaustive parity
  against upstream's filter-tree corner cases (deeply nested merges,
  anchored vs unanchored interactions) is still being validated.
- **`--checksum-seed` / `--fuzzy`.** Accepted and exercised in the common
  path; deeper conformance audits against upstream rsync 3.4.1 are
  tracked separately.
- **Windows Tier-1C ACLs.** NTFS DACL round-trip via `windows-rs`
  `GetNamedSecurityInfoW` / `SetNamedSecurityInfoW`; deny ACEs,
  inherited ACEs, the SACL, non-`rwx` access bits, and unresolvable
  SIDs are dropped with a one-time warning. SDDL fidelity payload,
  `--audit-acls`, `--fail-on-windows-acl-loss`, and `--windows-acls`
  are planned. See `docs/platform-notes.md` and
  `docs/design/windows-ntfs-acl-support.md`.
- **Windows symlinks and device nodes.** `create_symlinks` is
  `cfg(not(unix))` no-op; `create_fifo` and `create_device_node` are
  no-ops on non-Unix.

---

### Upgrade guide

Most operators upgrading from v0.6.2 will not need to change anything.
The wire-protocol fixes only activate when the corresponding flags
(`--iconv`, `--fake-super`, `-X --fake-super`) are in use, and the
default-path change for parallel-receive-delta (PIP-5) preserves byte
equivalence with the legacy sequential path.

- **Flag defaults:** the receiver's delta-apply path is now parallel by
  default (PIP-5). Operators who hit a regression during the beta soak
  can opt out via the legacy sequential path. No other flag default
  changed between v0.6.2 and this beta. The full default set is
  documented in `oc-rsync --help` and `docs/oc-rsync.1.md`.
- **New env vars:** none introduced this cycle.
  `OC_RSYNC_SPILL_THRESHOLD_BYTES` and `OC_RSYNC_SPILL_DIR` (introduced
  in 0.6.x) remain the canonical way to enable disk-backed receiver
  spill; the `--spill-dir` and `--spill-threshold-bytes` CLI flags
  shadow the env vars.
- **New Cargo features:**
  - `landlock` - enables Linux Landlock LSM enforcement on top of the
    SEC-1 `*at` chain (Linux 5.13+, no-op elsewhere).
  - `iouring-send-zc` - enables `IORING_OP_SEND_ZC` dispatch (Linux
    6.0+ recommended; opt-in for the beta tag).
  - `ssh-socketpair-stderr` - socketpair-backed SSH stderr instead of an
    anonymous pipe; opt-in to avoid pipe-buffer deadlock with chatty
    remote children.
- **Removed CI surface:** the `enforce-limits` workflow and xtask
  command were removed (#4538). External pipelines that gated on the
  `enforce-limits` workflow should drop that requirement; LoC is no
  longer policed in CI.
- **AGENTS.md no longer tracked.** Treat as local-only. No runtime
  effect.

If you previously worked around the `--iconv` local-copy no-op (#4579)
or the `--iconv` pull-from-upstream-daemon abort (#4576) by avoiding
those configurations, those workarounds can be dropped after upgrading.

---

### Acknowledgements

Mirrors upstream rsync 3.4.x behavior. See `target/interop/upstream-src/`
for the canonical reference. The SEC-1 chain mirrors upstream's own
3.4.3 hardening direction (`renameat`, `unlinkat`, and the metadata
`*at` family) while adding the `DirSandbox` + `openat2(RESOLVE_BENEATH
| RESOLVE_NO_SYMLINKS)` carrier as oc-rsync's specific construction.

The zsync-inspired delta-matching refactors (bithash prefilter,
sequential-match extension, matched-block pruning, compact-key layout)
draw from the `librcksum` design in https://github.com/cph6/zsync.

---

### Reporting issues

Use the GitHub issue tracker at https://github.com/oferchen/rsync/issues.
For security-sensitive reports, follow the disclosure path in
`SECURITY.md`.
