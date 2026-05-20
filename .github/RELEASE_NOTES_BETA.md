<!--
Scaffold for the upcoming beta release notes.

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

Wire-compatible with upstream rsync 3.4.2 (and back-compat with 3.4.1, protocol 32).

This is the first **beta** of oc-rsync. The 0.6.x line shipped as alpha while
wire-feature completeness, interop, coverage, and unsafe-code hygiene closed
out. Beta declares those four bars met as of the tagged commit; see
*Beta-readiness criteria* below for the exact conditions.

### Install

See the install/toolchain header in `.github/RELEASE_TEMPLATE.md` (Homebrew,
prebuilt packages, toolchain variants). This document only covers what is
new since v0.6.2.

---

### Beta-readiness criteria

This release is the first build that meets all four criteria simultaneously:

1. **Wire-feature completeness vs upstream rsync 3.4.1.** No remaining
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

### Highlights

<!--
Pick 3-5 user-visible items from "What's New" / "Wire-protocol fidelity"
that justify the milestone, written for an operator who has been running
0.6.2 and is deciding whether to upgrade. Suggested anchors (refine at
tag time):

- `--iconv` is now correct end-to-end (local-copy + pull from upstream daemon)
- `-X --fake-super` round-trips cleanly against upstream rsync 3.4.x
- Sender now honours `user.rsync.%stat` so fake-super -> fake-super -> fake-super preserves original mode/uid/gid/rdev
- `--version` lists compiled-in Cargo features so deployments can audit which optional codepaths shipped
- Engine `force_insert` counter is exposed for operators tuning concurrent-delta reorder pressure
-->

- TBD: top user-visible improvement #1
- TBD: top user-visible improvement #2
- TBD: top user-visible improvement #3
- TBD: top user-visible improvement #4
- TBD: top user-visible improvement #5

---

### What's New

Grouped by the same categories the `.github/release.yml` auto-labeller uses,
so the rendered release matches the GitHub-generated changelog.

#### Features

- `feat(transfer)`: honour source-side `user.rsync.%stat` in `--fake-super`
  sender; preserves original mode/uid/gid/rdev across a fake-super round
  trip (#4578).
- `feat(version)`: list compiled-in Cargo features in `--version` output so
  deployments can audit which optional codepaths shipped (#4547).
- `feat(engine)`: expose `force_insert` counter on `DeltaConsumer::stats`
  for concurrent-delta receiver tuning (#4553).

#### Performance

- TBD once benchmark deltas land. See *Performance* below for the chart
  placeholder. No `perf:`-prefixed PRs landed between v0.6.2 and the
  scaffold cut; the perf story for beta is the cumulative architecture
  (threaded pipeline, io_uring, IOCP writers, reflink/clone) carried over
  from 0.6.x, validated by the chart.

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
  IOCP)
- `fix(ci)`: skip oc-rsync daemon scenarios in Windows interop smoke
  (#4550).

#### CI/CD

- TBD. No `ci:`-prefixed PRs landed between v0.6.2 and the scaffold cut.
  CI hardening shows up under *Bug Fixes* / *Other Changes* this cycle
  (Windows interop smoke skip, Windows path normalisation in test
  fixtures, `enforce-limits` removal).

#### Documentation

- `docs(audits)`: triage non-required CI workflow failure cells (BR-5b).
- `docs(audits)`: record workspace `cargo llvm-cov` baseline (BR-4a).
- `docs(audits)`: record Windows hard-coded Unix path test audit (#4554).
- `docs(audits)`: eliminate-path matrix for `tools/ci/known_failures.conf`.
- `docs`: refresh IOCP wiring and signal-handler unsafe attribution
  (#4574).
- `docs(compress)`: fix protocol-threshold comments and document module
  layout.
- `docs(engine)`: fix broken intra-doc links in `concurrent_delta`
  (#4557).
- `docs(engine,transfer)`: fix broken intra-doc links in `lib.rs` and
  submodules.
- `docs(bandwidth)`: drop banner/divider and restatement comments
  (#4569); drop restating test comments and replace stale line
  references.
- `docs(embedding)`: drop restatement comments in lib and example
  (#4568).
- `docs(logging-sink)`: remove restatement comments in tests (#4564).
- `docs(branding)`: drop restatement comments in `validate_versions`
  (#4563).

#### Other Changes

Includes `chore:`, `style:`, `test:`, and `refactor:` PRs.

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
- `test(protocol)`: pin filter `legal_len=1` parity at proto <= 28
  (#4572); fix `stress_long_paths_10k` roundtrip on Windows (#4565);
  build flist stress paths with forward slashes (#4561).
- `test(daemon)`: normalise Windows temp paths for `--module` fixtures
  (#4559).
- `test(engine)`: cover `force_insert` drops, duplicates, payload,
  monotonicity (#4556); ignore flaky
  `surviving_threads_keep_inserting_after_lock` test.
- `test(flist)`: fix `error_path_preservation` on Windows (#4549).
- `test(fuzz)`: consolidate ACL/xattr wire decoders into
  `acl_xattr_wire` fuzz target.
- `chore`: stop tracking `AGENTS.md`; treat as local-only.
- `chore`: remove enforce-limits CI check and xtask command (#4538).
- `chore(tests)`: remove stale TODO markers from interop placeholders.
- `chore(cli)`: delete stale `server_tests` targeting refactored-away
  API.

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
  indexed against the sender's wire-byte (scan) order. `iconv_reorder_suppressed()`
  now centralises the predicate and is checked in `receive_file_list`,
  `receive_extra_file_lists` (INC_RECURSE), and the streaming
  `IncrementalFileListReceiver::collect_sorted` fallback. Upstream
  reference: `options.c:2051-2056` `need_unsorted_flist`, `flist.c:2496-2498`.
  *Closes BR-2e (#2490).*
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

### Known limitations

These carry over from v0.6.2 and remain accurate as of beta. They are
reproduced here so beta users see the architectural trade-offs before
deploying, without having to chase down `README.md`.

- **io_uring kernel requirement.** Provided buffer rings (PBUF_RING)
  require Linux **5.19+**; older 5.6-5.18 kernels fall back to standard
  buffered I/O via runtime probing.
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
- **SSH compression interaction.** When the SSH cipher already performs
  compression (e.g., `Compression yes` in `ssh_config`), running
  `oc-rsync -z` will compress payloads twice. There is currently no
  auto-detection / auto-disable path; operators should pick one layer.
- **Daemon TLS.** Native TLS is not built into the daemon. Deploy
  `oc-rsync --daemon` behind `stunnel`, `ssh -L`, or a reverse proxy
  that terminates TLS. See `docs/deployment/daemon-tls.md` and
  `SECURITY.md`.
- **Windows IOCP scope.** IOCP is wired for socket I/O (daemon and SSH
  transports) and for the receive-side disk-write pipeline
  (`transfer::disk_commit` dispatches `Writer::Iocp` when the IOCP
  backend is selected on Windows). File reads still use standard
  buffered I/O; extending IOCP to the read path is tracked in WPG-1.
- **`.rsync-filter` per-directory inheritance.** Inheritance semantics
  match upstream for the common cases tested in the interop suite, but
  exhaustive parity against upstream's filter-tree corner cases (deeply
  nested merges, anchored vs unanchored interactions) is still being
  validated.
- **`--checksum-seed` / `--fuzzy`.** These flags are accepted and
  exercised in the common path; deeper conformance audits against
  upstream rsync 3.4.1 are tracked separately.
- **Windows Tier-1C ACLs.** NTFS DACL round-trip via `windows-rs`
  `GetNamedSecurityInfoW`/`SetNamedSecurityInfoW`; deny ACEs, inherited
  ACEs, the SACL, non-`rwx` access bits, and unresolvable SIDs are
  dropped with a one-time warning. SDDL fidelity payload,
  `--audit-acls`, `--fail-on-windows-acl-loss`, and `--windows-acls` are
  planned. See `docs/platform-notes.md` and
  `docs/design/windows-ntfs-acl-support.md`.
- **Windows symlinks and device nodes.** `create_symlinks` is
  `cfg(not(unix))` no-op; `create_fifo` and `create_device_node` are
  no-ops on non-Unix.

---

### Upgrade guide

Most operators upgrading from v0.6.2 will not need to change anything.
The wire-protocol fixes above only activate when the corresponding flags
(`--iconv`, `--fake-super`, `-X --fake-super`) are in use, and they
strictly close gaps - they do not change semantics for transfers that
were already working.

- **Flag defaults:** no flag default changed between v0.6.2 and this
  beta. The full default set is documented in `oc-rsync --help` and
  `docs/oc-rsync.1.md`.
- **New env vars:** none introduced this cycle. `OC_RSYNC_SPILL_THRESHOLD_BYTES`
  and `OC_RSYNC_SPILL_DIR` (introduced in 0.6.x) remain the canonical way to
  enable disk-backed receiver spill; the planned `--spill-dir` and
  `--spill-threshold-bytes` CLI flags are still tracked under STN-11 and
  will shadow the env vars once they land.
- **Removed CI surface:** the `enforce-limits` check and the corresponding
  xtask command were removed (#4538). External pipelines that gated on
  the `enforce-limits` workflow should drop that requirement; LoC is no
  longer policed in CI.
- **AGENTS.md no longer tracked.** Treat as local-only. No runtime
  effect.

If you previously worked around the `--iconv` local-copy no-op (#4579)
or the `--iconv` pull-from-upstream-daemon abort (#4576) by avoiding
those configurations, those workarounds can be dropped after upgrading.

---

### Security notes

See `SECURITY.md` for the full policy. Highlights relevant to this
release:

#### Upstream rsync CVE status

oc-rsync is not vulnerable to the upstream rsync CVE cluster disclosed
in early 2025, and the v0.6.2 audit against rsync 3.4.2 fixes carries
forward:

| CVE | Upstream Issue | oc-rsync Status | Reason |
|-----|---------------|-----------------|--------|
| CVE-2024-12084 | Heap overflow in checksum parsing | Not vulnerable | Rust `Vec<u8>` handles dynamic sizing |
| CVE-2024-12085 | Uninitialized stack buffer leak | Not vulnerable | Rust requires initialization |
| CVE-2024-12086 | Server leaks client files | Not vulnerable | Strict path validation |
| CVE-2024-12087 | Path traversal via `--inc-recursive` | Not vulnerable | Path sanitization |
| CVE-2024-12088 | `--safe-links` bypass | Mitigated | Rust path handling |
| CVE-2024-12747 | Symlink race condition | Mitigated | TOCTOU is OS-level |

#### Upstream rsync 3.4.2 audits (carried over from v0.6.2)

Equivalent code paths verified safe in oc-rsync: compressed-stream
negative-token decoder bounds (#2225), xattr `qsort` element-count
parity (#2226), `clean_fname()` buffer-underflow parity (#2227),
allocator zeroing pattern (#2228), Y2038 safety in syscall paths (#2229),
ACL ID mapping for non-root users (#2230, closes #618), FreeBSD
many-xattrs handling parity (#2231), "Directory has vanished" error
path (#2232), removal of multiple leading slashes (#2233), daemon
`chrono::Local` pre-init before `chroot` (#2234), `--open-noatime`
propagation through sender source-file opens (#2236), AVX2
`get_checksum1` `mul_one` uninitialised-regression audit (#2222), MD4
`get_checksum2` `buf1` uninitialised-regression audit (#2223), and the
SIMD vs scalar self-test cross-validating AVX2/SSE2/NEON paths at
startup (#2224).

#### Monitoring

`tools/ci/check_upstream_release.sh` runs weekly via GitHub Actions and
opens a tracking issue when a new upstream rsync release ships, so new
CVEs are surfaced automatically. See `SECURITY.md` for the full
subscriber list.

#### Hardening notes

The hardening notes in `SECURITY.md` (buffer pool bounds checks, io_uring
buffer-group ID namespace, SSH double compression, daemon TLS
termination, daemon module hardening) apply unchanged.

---

### Reporting issues

Use the GitHub issue tracker at https://github.com/oferchen/rsync/issues.
For security-sensitive reports, follow the disclosure path in
`SECURITY.md`.
