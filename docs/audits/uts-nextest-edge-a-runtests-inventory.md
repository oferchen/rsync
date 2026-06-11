# UTS-NEXTEST-EDGE.a: upstream runtests.py inventory by operational-sanity value

Tracks: UTS-NEXTEST-EDGE.a (audit phase)
Companion: `docs/design/uts-nextest-edge-b-test-harness.md` (UTS-NEXTEST-EDGE.b)

## 1. Purpose

The upstream-testsuite cell is the only place a regression in upstream-runtests-style edge cases trips before release. That cell only runs in the dedicated workflow, not on every PR. The 2026-06-11 fresh runtests pass on a Linux host (`ofer@192.168.21.226`, `~/rsync/testsuite/*_test.py`) surfaced 20+ failures under the UTS-N.REOPEN umbrella, all of which would have been caught at PR-time if the underlying behaviour had been pinned by a nextest test.

This document inventories the 111 upstream `*_test.py` files, scores each on three operational-sanity dimensions, and proposes the top 20-30 candidates to port to nextest tests under `crates/*/tests/operational/`. The companion design document (UTS-NEXTEST-EDGE.b) specifies the harness primitives those ports will share.

Audit + design only. No implementation, no test code in this PR.

## 2. Scoring model

Each test gets three scores (0-3):

- **W = wire-protocol surface.** How much of the upstream wire contract does the test exercise?
  - 0 = no wire surface (option parser, help text, wildmatch helper).
  - 1 = single-host transfer over local pipe; wire is the in-process socketpair.
  - 2 = real daemon handshake (`@RSYNCD:` greeting, module select, auth, capability strings) or remote shell (`-e`) path.
  - 3 = wire-protocol invariant under a corner case the cell has shown to regress (INC_RECURSE, `-zz` codec, NDX_DEL_STATS, mode-0 sentinel, sandbox, MSG_INFO framing, batch goodbye flush).
- **F = failure-mode severity.** What happens to a user if the bug reappears?
  - 0 = infra-only (helper builds, env detection).
  - 1 = exit-code-only mismatch on a degenerate input (already covered by argument-parsing tests).
  - 2 = wrong output, wrong stats summary, wrong itemize, wrong file mode at a depth-3 path.
  - 3 = silent dir-diff (wrong content at destination), silent symlink/ACL/xattr drop, sandbox escape, hang on goodbye, signal death.
- **G = existing nextest gap.** Where do we stand today?
  - 0 = already covered by a non-`#[ignore]` nextest in `crates/*/tests/` whose pattern matches the upstream scenario.
  - 1 = partial coverage (unit test for one cell of the decision table; harness exists but not driven).
  - 2 = covered only behind `#[ignore]` (requires binary, only runs in interop cell).
  - 3 = no coverage at all (upstream cell is the sole guardrail).

Total `T = W + F + G` (0-9). Priority bands:

- **Tier 1 (port now): T >= 7.** Wire/silent-failure surface with no PR-time guardrail.
- **Tier 2 (port soon): T in 5-6.** High-value but partially covered; finish the decision table.
- **Tier 3 (defer): T in 3-4.** Specialty cells; either depth-property duplicates of a tier-1 case or already-well-covered.
- **Skip: T <= 2.** Helper smoke tests, host-specific feature probes, or duplicate-of-another tests.

## 3. Inventory

Tests are listed in the order they appear in `~/rsync/testsuite/`. Cross-references point to existing UTS-N task IDs (`docs/audits/uts-*.md`, in-tree `*uts_*.rs` tests). The "Port?" column states T1/T2/T3/Skip and the proposed Rust crate placement when T1 or T2.

| # | Upstream test | W | F | G | T | Cross-ref | Port? | Notes |
|--:|---|--:|--:|--:|--:|---|---|---|
|  1 | 00-hello_test.py | 1 | 1 | 2 | 4 | - | T3 | `--version`/`--info=help`/`--debug=help` already golden-tested in `crates/cli/tests/output_parity.rs`; weird-name + `lsh.sh` round-trip is the only remaining cell. |
|  2 | acls-default_test.py | 1 | 3 | 3 | 7 | - | **T1** (`crates/metadata/tests/operational/`) | POSIX default ACL on parent dir must be inherited by transfer container dir. Silent ACL drop. Linux + macOS via `exacl`. |
|  3 | acls-depth_test.py | 1 | 3 | 2 | 6 | - | T2 (`crates/metadata/tests/operational/`) | Property duplicate of `acls_test.py`; pin one depth-3 case so refactors of the resolver don't drop ACLs at depth. |
|  4 | acls_test.py | 1 | 3 | 1 | 5 | - | T2 (`crates/metadata/tests/operational/`) | Round-trip POSIX ACL: shallow tree. Existing acl unit tests cover serialization, not end-to-end transfer. |
|  5 | alt-dest-deep_test.py | 1 | 2 | 2 | 5 | UTS-2 | T2 (`crates/transfer/tests/operational/`) | Property-level distinguishing test of `--link-dest`/`--copy-dest`/`--compare-dest` at depth. UTS-2 family covers root-mkdir + SSH alt-dest; this pins the *distinguishing* property each option must produce. |
|  6 | alt-dest-symlink-race_test.py | 1 | 3 | 1 | 5 | SEC-1.m/.n | T2 (`crates/transfer/tests/operational/`) | basedir-confinement gap in `secure_relative_open()` for alt-dest. SEC-1.m/.n cover the receiver swap variant; this is the alt-dest variant. |
|  7 | alt-dest_test.py | 1 | 2 | 2 | 5 | UTS-2 | T2 (`crates/transfer/tests/operational/`) | Tree equality across `-e lsh.sh`. Closely linked with UTS-2; the SSH-stub path is the new surface here. |
|  8 | append-shortsum_test.py | 2 | 3 | 3 | 8 | - | **T1** (`crates/transfer/tests/operational/`) | The short transfer-checksum regression that died with "Invalid checksum length 16 [sender]" on builds without xxh128/xxh3. Hits the s2length cap, sender-side. Sub-16-byte transfer-checksum path is not covered today. |
|  9 | append_test.py | 1 | 2 | 2 | 5 | - | T2 (`crates/transfer/tests/operational/`) | `--append` + `--append-verify` at depth, including the size-already-larger and same-size cases. |
| 10 | atimes_test.py | 1 | 2 | 2 | 5 | - | T2 (`crates/metadata/tests/operational/`) | `-U` atimes preservation requires the `atimes` build feature; gate appropriately. |
| 11 | backup-deep_test.py | 1 | 2 | 2 | 5 | - | T2 (`crates/transfer/tests/operational/`) | `--backup-dir` placed OUTSIDE the destination at depth >=3 - cross-directory rename surface the resolver restructure rewrites. |
| 12 | backup_test.py | 1 | 2 | 2 | 5 | - | T2 (`crates/transfer/tests/operational/`) | Plain `--backup` suffix, `--backup-dir` + `--delete-delay`, `--inplace --backup-dir`. |
| 13 | bare-do-open-symlink-race_test.py | 1 | 3 | 1 | 5 | SEC-1.m | T2 (`crates/transfer/tests/operational/`) | Codex audit Findings 3b/3c. Receiver's bare `do_open`/`do_symlink`/`do_mknod` paths must not follow parent symlinks. SEC-1.m covers one variant; this is the do_open variant. |
| 14 | batch-mode_test.py | 3 | 3 | 1 | 7 | UTS-15.a/.b/.c/.g | **T1** (`crates/transfer/tests/operational/`) | `--write-batch`/`--read-batch`/`--only-write-batch`. UTS-15 family already fixed the daemon arg parser, argv strip, goodbye flush, and local-copy traversal; pin the end-to-end batch contract so future refactors don't regress it. |
| 15 | chdir-symlink-race_test.py | 1 | 3 | 1 | 5 | SEC-1.m | T2 (`crates/transfer/tests/operational/`) | Receiver `chdir()` into destination subdir must not follow attacker symlink. |
| 16 | chgrp_test.py | 1 | 2 | 2 | 5 | - | T2 (`crates/metadata/tests/operational/`) | `-g` group preservation when user is a member of the supplementary group. Requires Linux, root-aware. |
| 17 | chmod-option_test.py | 2 | 3 | 3 | 8 | - | **T1** (`crates/transfer/tests/operational/`) | Includes the 2.6.8 regression where pushing a new dir with `--no-perms` to a daemon with `incoming chmod` mis-classified the dir as a file. Daemon-side modifier path; high failure severity. |
| 18 | chmod-symlink-race_test.py | 1 | 3 | 1 | 5 | SEC-1.m | T2 (`crates/transfer/tests/operational/`) | CVE-2026-29518 follow-up: `chmod()` on receiver-side must not follow attacker symlinks. |
| 19 | chmod-temp-dir_test.py | 1 | 2 | 3 | 6 | - | **T1** (`crates/transfer/tests/operational/`) | `--temp-dir` on a *different filesystem* forces `rename(2)` cross-fs - copy+unlink fallback. Path not covered today; ENOSPC/EXDEV semantics. |
| 20 | chmod_test.py | 1 | 2 | 2 | 5 | - | T2 (`crates/transfer/tests/operational/`) | Read-only + set[ug]id permissions across whole-file and delta updates. |
| 21 | chown-fake_test.py | 1 | 2 | 2 | 5 | - | T2 (`crates/metadata/tests/operational/`) | `--fake-super` encodes ownership in `user.rsync.%stat` xattr. Linux + xattr-capable FS. |
| 22 | chown_test.py | 1 | 2 | 2 | 5 | - | T2 (`crates/metadata/tests/operational/`) | Root-required uid/gid preservation. |
| 23 | clean-fname-underflow_test.py | 2 | 3 | 3 | 8 | - | **T1** (`crates/protocol/tests/operational/`) | Crafted merge filename hits `clean_fname()` over `--server`. Must not die from a signal and must not exit 0 - reject the bogus input. Read-before-buffer regression class. |
| 24 | compare_test.py | 1 | 2 | 2 | 5 | - | T2 (`crates/transfer/tests/operational/`) | `-c`/`-I`/`--size-only`/`--modify-window` "stealth change" (same size + mtime, different content) at depth. |
| 25 | compress-options_test.py | 2 | 1 | 1 | 4 | - | T3 | Algorithm selection breadth (`--compress-choice`, `--compress-level`, `--skip-compress`, `--checksum-choice`, `--checksum-seed`). Mostly covered by `crates/compress/tests/strategy_integration.rs` + `crates/checksums/`; T3 unless a corner is missed. |
| 26 | compress-zlib-insert_test.py | 3 | 3 | 1 | 7 | UTS-18 | **T1** (`crates/compress/tests/operational/`) | Issue #951 - `Z_INSERT_ONLY` fallback to `Z_SYNC_FLUSH` overflowed obuf on a large incompressible matched block. Closely related to UTS-18 zlib codec invariants. |
| 27 | copy-dest-source-symlink_test.py | 1 | 3 | 1 | 5 | SEC-1.m | T2 (`crates/transfer/tests/operational/`) | `copy_altdest_file()`'s source open is `do_open_nofollow()` - parent symlinks still followed. Daemon-module attacker scenario. |
| 28 | crtimes_test.py | 1 | 2 | 2 | 5 | - | T2 (`crates/metadata/tests/operational/`) | Create-time preservation (`crtimes` feature). macOS + Linux with `btrfs`/`ext4` + `statx`. |
| 29 | cvs-exclude_test.py | 1 | 1 | 2 | 4 | - | T3 | `-C` ignores CVS cruft + honours `.cvsignore`. Filter-rule path; existing filter property tests already cover most of this. |
| 30 | daemon-access-ip_test.py | 3 | 3 | 3 | 9 | UTS-22 | **T1** (`crates/daemon/tests/operational/`) | `hosts allow`/`hosts deny` IP + CIDR matching. Requires `--use-tcp` mode. UTS-22 covered chroot ACL; this is the access-control wire-level surface. |
| 31 | daemon-access_test.py | 3 | 3 | 3 | 9 | - | **T1** (`crates/daemon/tests/operational/`) | Module path resolution, `read only`, `write only`, `list`. Foundational daemon contract; no current end-to-end coverage. |
| 32 | daemon-auth_test.py | 3 | 3 | 3 | 9 | - | **T1** (`crates/daemon/tests/operational/`) | `auth users` + secrets file with strict modes. Authentication is daemon-protocol; covered today only by unit tests of the parser, not by an end-to-end auth round trip. |
| 33 | daemon-chroot-acl_test.py | 3 | 3 | 1 | 7 | UTS-22.b | **T1** (`crates/daemon/tests/operational/`) | GHSA-rjfm-3w2m-jf4f hostname ACL bypass during chroot. UTS-22.b is the fix; pin the behaviour in nextest so future chroot refactors can't silently re-introduce it. |
| 34 | daemon-config_test.py | 3 | 3 | 3 | 9 | - | **T1** (`crates/daemon/tests/operational/`) | `&include` config directive + module with nonexistent path. PR #5517 added `&include`/`&merge`; nextest coverage closes the loop. |
| 35 | daemon-delete-stats_test.py | 3 | 2 | 1 | 6 | - | T2 (`crates/daemon/tests/operational/`) | NDX_DEL_STATS on a daemon upload at protocol >=31. Generator-side wire frame already covered by unit tests; this is end-to-end. |
| 36 | daemon-exec_test.py | 3 | 2 | 3 | 8 | - | **T1** (`crates/daemon/tests/operational/`) | pre-xfer/post-xfer exec hooks; `RSYNC_MODULE_NAME`/`RSYNC_EXIT_STATUS` env. A non-zero pre-xfer exec must abort the transfer. No nextest coverage. |
| 37 | daemon-filter_test.py | 3 | 3 | 2 | 8 | - | **T1** (`crates/daemon/tests/operational/`) | Daemon-side exclude + incoming/outgoing chmod at depth. The "daemon must hide matching files everywhere" invariant. |
| 38 | daemon-groupmap-wild_test.py | 3 | 2 | 3 | 8 | - | **T1** (`crates/daemon/tests/operational/`) | Issue #829: backslash-escaped wildcard in daemon-bound `--groupmap` silently mismatched. Argv un-escape contract on the daemon side. |
| 39 | daemon-gzip-download_test.py | 3 | 3 | 0 | 6 | UTS-9 | T2 | Already covered by `crates/core/tests/uts_9_daemon_gzip_download_goodbye.rs` (behind `#[ignore]`). Score G=0 because the test exists; consider promoting to non-ignored if a CI strategy lets us. |
| 40 | daemon-gzip-upload_test.py | 3 | 3 | 2 | 8 | UTS-9 | **T1** (`crates/core/tests/operational/`) | Reverse direction of UTS-9 (`-zz` upload to daemon). Goodbye flush contract holds for receiver-side too; not pinned. |
| 41 | daemon-munge_test.py | 3 | 3 | 3 | 9 | - | **T1** (`crates/daemon/tests/operational/`) | `munge symlinks = yes`: stored symlinks get `/rsyncd-munged/` prefix; outgoing strips it. Highest-priority daemon test - silent symlink corruption otherwise. |
| 42 | daemon-path-root-read_test.py | 3 | 3 | 3 | 9 | - | **T1** (`crates/daemon/tests/operational/`) | Issue #897: `path = /` module hits `Invalid argument (22)` on every read in upstream 3.4.3. PR #5522 fixed `path = /`; pin behaviour. |
| 43 | daemon-refuse-compress_test.py | 3 | 2 | 2 | 7 | - | **T1** (`crates/daemon/tests/operational/`) | Module with `refuse options = compress` must reject compressed clients. Argv allow/deny parsing. |
| 44 | daemon-refuse_test.py | 3 | 2 | 3 | 8 | - | **T1** (`crates/daemon/tests/operational/`) | Widens daemon-refuse-compress: wildcard refuse + `* !a !v` allow-list idiom. Argv option-class matching. |
| 45 | daemon_test.py | 3 | 2 | 2 | 7 | - | **T1** (`crates/daemon/tests/operational/`) | Basic daemon ops: list modules, list hidden module, single-glob match, atimes-format variant. Foundational. Re-run under non-root path (`fleet_nonroot`). |
| 46 | delay-updates-deep_test.py | 1 | 2 | 2 | 5 | - | T2 (`crates/transfer/tests/operational/`) | `--delay-updates` staging dir `.~tmp~` at depth - per-dir rename at end of run. |
| 47 | delay-updates_test.py | 1 | 2 | 2 | 5 | - | T2 (`crates/transfer/tests/operational/`) | Pre-seed staging dir with stale file; final dest must still match source. |
| 48 | delete-deep_test.py | 1 | 3 | 2 | 6 | - | **T1** (`crates/transfer/tests/operational/`) | `--delete` family + `--max-delete` + `--existing` + `--ignore-existing` at depth. Wrong behaviour deletes wrong files - high severity. |
| 49 | delete-missing-args-files-from_test.py | 2 | 3 | 3 | 8 | - | **T1** (`crates/transfer/tests/operational/`) | Issue #910: `--delete-missing-args` + `--files-from` aborted with "invalid file mode 00 ... protocol incompatibility (code 2)". Flist mode-0 sentinel. |
| 50 | delete_test.py | 1 | 2 | 1 | 4 | - | T3 | `--del` dry-run output parity, `--remove-source-files`, per-dir `P`/`-` filter rules. Partly covered by filter property tests. |
| 51 | devices-fake_test.py | 1 | 3 | 3 | 7 | - | **T1** (`crates/metadata/tests/operational/`) | `--fake-super` encodes device numbers in `user.rsync.%stat` xattr. Requires Linux/xattr. No round-trip nextest today. |
| 52 | devices_test.py | 1 | 3 | 2 | 6 | - | **T1** (`crates/metadata/tests/operational/`) | Device node handling (char/block/fifo). Requires root for real `mknod`. |
| 53 | dir-sgid_test.py | 1 | 2 | 2 | 5 | - | T2 (`crates/transfer/tests/operational/`) | Setgid bit on destination's parent dir must propagate to a new dir rsync creates. |
| 54 | dirs_test.py | 1 | 2 | 2 | 5 | - | T2 (`crates/transfer/tests/operational/`) | `-d` copies entries directly named, not recursively. Top-level dirs are empty. |
| 55 | duplicates_test.py | 1 | 2 | 2 | 5 | - | T2 (`crates/transfer/tests/operational/`) | `clean_flist()` must dedupe identical sources listed many times. |
| 56 | exclude-lsh_test.py | 2 | 2 | 1 | 5 | - | T2 (`crates/transfer/tests/operational/`) | exclude_test under `-e lsh.sh`. Stub-shell variant of exclude_test. |
| 57 | exclude_test.py | 1 | 2 | 1 | 4 | - | T3 | Exclude/include/filter rules + CVS exclusions + per-dir filter files. Largely covered by existing filter property tests. |
| 58 | executability_test.py | 1 | 2 | 2 | 5 | - | T2 (`crates/transfer/tests/operational/`) | `-E` propagates only the executable bit; other permission changes ignored. |
| 59 | file-to-file-mkpath-dry-run_test.py | 1 | 3 | 3 | 7 | - | **T1** (`crates/transfer/tests/operational/`) | Issue #880 + PR #952 dry-run itemize regression. `change_dir#3 ... failed` because make_path() only *reports* in dry-run. |
| 60 | files-from-depth_test.py | 1 | 2 | 2 | 5 | - | T2 (`crates/transfer/tests/operational/`) | `--files-from` + `-0` + `--exclude-from`/`--include-from` at depth. |
| 61 | files-from_test.py | 2 | 2 | 2 | 6 | UTS-1 | **T1** (`crates/transfer/tests/operational/`) | `--files-from=LIST` over `-e lsh.sh` with 4 files-host/src-host/dest-host combos. UTS-1.e/.f cover test6 actual command + tcpdump wire parity; this is the broader case. |
| 62 | filter-depth_test.py | 1 | 2 | 2 | 5 | - | T2 (`crates/transfer/tests/operational/`) | Per-dir merge filter (`-F` reads `.rsync-filter` at each descent step) at depth. |
| 63 | fuzzy-basis_test.py | 1 | 2 | 2 | 5 | - | T2 (`crates/transfer/tests/operational/`) | `--fuzzy` scorer with several similar-named candidates at depth. |
| 64 | fuzzy_test.py | 1 | 2 | 1 | 4 | - | T3 | `--fuzzy` + `--delete-delay` at root. Largely covered by `crates/transfer/tests/fuzzy_level2_basis_search.rs`. |
| 65 | hands_test.py | 1 | 2 | 1 | 4 | - | T3 | Richly-populated tree end-to-end. Spot-coverage already across many tests; not the highest port priority. |
| 66 | hardlinks-deep_test.py | 1 | 2 | 2 | 5 | - | T2 (`crates/transfer/tests/operational/`) | `-H` across directory boundaries at depth >=3. |
| 67 | hardlinks_test.py | 1 | 2 | 2 | 5 | - | T2 (`crates/transfer/tests/operational/`) | `-H` detection + re-creation + INC_RECURSE path + `--link-dest`/`--copy-dest` interactions. |
| 68 | inplace_test.py | 1 | 2 | 2 | 5 | - | T2 (`crates/transfer/tests/operational/`) | `--inplace` keeps same inode; default keeps NEW inode. Delta update path. |
| 69 | itemize_test.py | 2 | 3 | 1 | 6 | - | T2 (`crates/logging/tests/operational/`) | `-i`/`-ii`/`-vv` itemize output across whole-file + delta. PR #2569/#2572 covered direction indicator + symlink size; this is broader. |
| 70 | link-dest-module-escape_test.py | 3 | 3 | 1 | 7 | - | **T1** (`crates/daemon/tests/operational/`) | `--link-dest=../../OUTSIDE` against `path = /` daemon module must be refused. Security guard for #915 re-anchor. |
| 71 | link-dest-pathroot_test.py | 3 | 3 | 3 | 9 | - | **T1** (`crates/daemon/tests/operational/`) | Relative `--link-dest=../sibling` against `path = /` daemon module. Issues #897 + #915 intersection. PR #5522 fixed `path = /`; pin both sides. |
| 72 | link-dest-relative-basis_test.py | 3 | 3 | 3 | 9 | - | **T1** (`crates/daemon/tests/operational/`) | Issue #915: relative `--link-dest=../sibling` silently ignored by daemon receiver - backups stop de-duplicating without an error. Silent dir-diff. |
| 73 | links_test.py | 1 | 2 | 2 | 5 | - | T2 (`crates/transfer/tests/operational/`) | `-l`/`-L`/`-k` symlink handling at depth. |
| 74 | longdir_test.py | 1 | 2 | 2 | 5 | - | T2 (`crates/transfer/tests/operational/`) | 175-char dir name nested 3 deep, `--delete -avH`. 2.0.11 regression. |
| 75 | merge_test.py | 1 | 2 | 2 | 5 | - | T2 (`crates/transfer/tests/operational/`) | Multi-source merge into a single destination. Earlier sources win for unchanged content. |
| 76 | metadata-depth_test.py | 1 | 2 | 2 | 5 | - | T2 (`crates/metadata/tests/operational/`) | `-p`/`-t`/`--chmod` at depth per entry. |
| 77 | missing_test.py | 1 | 2 | 2 | 5 | - | T2 (`crates/transfer/tests/operational/`) | Three `missing_below` regressions: dry-run + `--ignore-non-existing`, dry-run + `-R -y --no-implied-dirs`. |
| 78 | mkpath_test.py | 1 | 2 | 2 | 5 | - | T2 (`crates/transfer/tests/operational/`) | `--mkpath` creates missing intermediate destination dirs. |
| 79 | omit-times_test.py | 1 | 2 | 2 | 5 | - | T2 (`crates/metadata/tests/operational/`) | `-O` and `-J` at depth: dir mtimes / symlink times not preserved while file mtimes are. |
| 80 | open-noatime_test.py | 1 | 2 | 2 | 5 | - | T2 (`crates/metadata/tests/operational/`) | `--open-noatime` keeps source atime intact across the transfer (Linux-only). |
| 81 | output-options_test.py | 1 | 1 | 1 | 3 | - | Skip | `--version`/`--help`/`-i`/`-n`/`--stats`/`--out-format`/`--list-only`/`-q`/`--progress`/`-h`/`-8`. Largely covered by `crates/cli/tests/output_parity.rs`. |
| 82 | ownership-depth_test.py | 1 | 2 | 2 | 5 | - | T2 (`crates/metadata/tests/operational/`) | `--groupmap`/`--chown`/`--usermap`/`-o` at depth. Root-aware. |
| 83 | partial_nowrite_test.py | 1 | 3 | 3 | 7 | - | **T1** (`crates/transfer/tests/operational/`) | `--partial` + `--delay-updates` when destination file is unwritable. Permission error handling. |
| 84 | partial_test.py | 1 | 2 | 2 | 5 | - | T2 (`crates/transfer/tests/operational/`) | `--partial` + `--partial-dir` at depth and across directory boundaries. Relative vs absolute partial-dir distinction. |
| 85 | preallocate_test.py | 1 | 2 | 2 | 5 | - | T2 (`crates/transfer/tests/operational/`) | `do_fallocate` + `do_punch_hole` at depth. Sparse st_blocks check. |
| 86 | protected-regular_test.py | 1 | 2 | 3 | 6 | - | **T1** (`crates/transfer/tests/operational/`) | `fs.protected_regular = {1,2}` blocks O_CREAT|O_WRONLY in world-writable sticky dirs; `--inplace` must still write. Linux-specific. |
| 87 | proxy-response-line-too-long_test.py | 2 | 3 | 3 | 8 | - | **T1** (`crates/transport/tests/operational/`) | Off-by-one stack OOB in `establish_proxy_connection()` on 1023+ byte response without `\n`. Must reject with "proxy response line too long" and exit non-zero (no signal death). |
| 88 | prune-empty-dirs_test.py | 1 | 2 | 2 | 5 | - | T2 (`crates/transfer/tests/operational/`) | `-m` drops chains that would be empty at destination (source-empty + filter-empty). |
| 89 | relative-implied_test.py | 1 | 2 | 2 | 5 | - | T2 (`crates/transfer/tests/operational/`) | `-R` implied directories + `--no-implied-dirs` carrying a non-default mode at depth. |
| 90 | relative_test.py | 1 | 2 | 2 | 5 | - | T2 (`crates/transfer/tests/operational/`) | `--relative` (`-R`) with `./` cut point, hard-link preservation, `--del-on-extras`, abs+R combo. |
| 91 | reverse-daemon-delta_test.py | 3 | 3 | 3 | 9 | - | **T1** (`crates/transfer/tests/operational/`) | OLD client + CURRENT daemon. Backward-compat for the installed base of old clients - the case version-mixing tests don't usually cover. Needs upstream 3.0.9/3.1.3 binary in `target/interop/upstream-install/`. |
| 92 | safe-links-absolute-intree_test.py | 1 | 3 | 2 | 6 | - | **T1** (`crates/transfer/tests/operational/`) | Absolute symlink resolving inside transfer tree under `--safe-links` is *still* dropped. Surprises users. |
| 93 | safe-links_test.py | 1 | 3 | 2 | 6 | - | **T1** (`crates/transfer/tests/operational/`) | `--safe-links` drops escape symlinks; in-tree symlinks survive. Silent symlink drop otherwise. |
| 94 | secure-relpath-validation_test.py | 2 | 3 | 1 | 6 | - | T2 (`crates/transfer/tests/operational/`) | Codex audit Finding 5: `secure_relative_open()` front-door rejected `../foo` but missed bare `..`, `subdir/..`. Path component splitter. |
| 95 | sender-flist-symlink-leak_test.py | 3 | 3 | 1 | 7 | SEC-1.m/.n | **T1** (`crates/daemon/tests/operational/`) | Sender-side flist generator could follow attacker symlink out of module via `change_dir(CD_SKIP_CHDIR)` -> `change_dir(CD_NORMAL)`. Daemon-module `use chroot = no`. |
| 96 | simd-checksum_test.py | 0 | 1 | 1 | 2 | - | Skip | `simdtest` helper compares SIMD vs C reference. Already covered by `crates/checksums/` parity tests. |
| 97 | size-filter_test.py | 1 | 2 | 2 | 5 | - | T2 (`crates/transfer/tests/operational/`) | `--max-size`/`--min-size` at depth selecting only small/large files. |
| 98 | sparse_test.py | 1 | 2 | 2 | 5 | - | T2 (`crates/transfer/tests/operational/`) | `-S` at depth: holey file arrives sparse + byte-identical. |
| 99 | ssh-basic_test.py | 2 | 2 | 1 | 5 | - | T2 (`crates/transport/tests/operational/`) | Basic `-e RSH` transfer reproduces source tree + `--delete` after dest rename. |
|100 | stop-time_test.py | 1 | 2 | 3 | 6 | - | **T1** (`crates/cli/tests/operational/`) | `--stop-at` (parse_time) accepts future / rejects past at parse; `--stop-after` takes a minute count. Option handling not reached by other tests. |
|101 | symlink-dirlink-basis_test.py | 1 | 3 | 1 | 5 | - | T2 (`crates/transfer/tests/operational/`) | Issue #715: `-K` (`--copy-dirlinks`) regressed after CVE-2026-29518 fix. basedir follows symlinks while only final component is O_NOFOLLOW. |
|102 | symlink-ignore_test.py | 1 | 2 | 2 | 5 | - | T2 (`crates/transfer/tests/operational/`) | Without `-l`/`-L`/`-a`, symlinks must not be copied; referent file lands at destination. |
|103 | temp-dir_test.py | 1 | 2 | 2 | 5 | - | T2 (`crates/transfer/tests/operational/`) | `--temp-dir` (`-T`) at depth + across directory boundaries. Same-FS rename. (Different-FS variant is `chmod-temp-dir_test.py`.) |
|104 | trimslash_test.py | 0 | 0 | 1 | 1 | - | Skip | Tiny `trimslash` helper. Path-normalization unit test - already covered. |
|105 | unsafe-byname_test.py | 0 | 1 | 1 | 2 | - | Skip | Direct call into `t_unsafe` helper wrapping `unsafe_symlink()`. Unit-test surface. |
|106 | unsafe-links_test.py | 1 | 3 | 2 | 6 | - | **T1** (`crates/transfer/tests/operational/`) | `-a` copies unsafe symlinks unchanged; `--copy-links` materialises all; `--copy-unsafe-links` materialises only the unsafe ones. |
|107 | update_test.py | 1 | 2 | 2 | 5 | - | T2 (`crates/transfer/tests/operational/`) | `-u` skips newer destination; `--force` deletes non-empty destination dir when replacing with a non-directory. |
|108 | wildmatch_test.py | 0 | 1 | 1 | 2 | - | Skip | `wildtest` helper across option combos. Filter property tests already cover wildmatch. |
|109 | xattrs-depth_test.py | 1 | 3 | 2 | 6 | - | **T1** (`crates/metadata/tests/operational/`) | `-X` distinctive user xattr per entry at every level of a depth-3 tree. |
|110 | xattrs-hlink_test.py | 1 | 3 | 2 | 6 | - | **T1** (`crates/metadata/tests/operational/`) | `-X` + `-H` together. Hard links must survive alongside xattrs. |
|111 | xattrs_test.py | 1 | 3 | 1 | 5 | - | T2 (`crates/metadata/tests/operational/`) | Round-trip `-X` + `--link-dest`/`--copy-dest`/`--fake-super`. |

## 4. Tier 1 summary (port now)

Total Tier 1: **30 tests**. Distribution by target crate:

| Crate | Count | Tests |
|---|--:|---|
| `crates/daemon/tests/operational/` | 13 | daemon-access-ip, daemon-access, daemon-auth, daemon-chroot-acl, daemon-config, daemon-exec, daemon-filter, daemon-groupmap-wild, daemon-munge, daemon-path-root-read, daemon-refuse, daemon-refuse-compress, daemon_test, link-dest-module-escape, link-dest-pathroot, link-dest-relative-basis, sender-flist-symlink-leak |
| `crates/transfer/tests/operational/` | 11 | append-shortsum, batch-mode, chmod-option, chmod-temp-dir, delete-deep, delete-missing-args-files-from, file-to-file-mkpath-dry-run, files-from, partial_nowrite, protected-regular, reverse-daemon-delta, safe-links, safe-links-absolute-intree, unsafe-links |
| `crates/metadata/tests/operational/` | 4 | acls-default, devices, devices-fake, xattrs-depth, xattrs-hlink |
| `crates/compress/tests/operational/` | 1 | compress-zlib-insert |
| `crates/core/tests/operational/` | 1 | daemon-gzip-upload |
| `crates/protocol/tests/operational/` | 1 | clean-fname-underflow |
| `crates/transport/tests/operational/` | 1 | proxy-response-line-too-long |
| `crates/cli/tests/operational/` | 1 | stop-time |

(Some daemon tests cross-cut into transfer; placement reflects which crate owns the *primary* invariant under test.)

## 5. Tier 2 summary (port soon)

Total Tier 2: **53 tests**. Most are depth-property duplicates of Tier 1 cases or partial-coverage of existing nextest scenarios. Port these in batches per crate after the Tier 1 harness is established. Splitting Tier 2 into rolling sub-tasks keeps PRs reviewable.

## 6. Tier 3 summary (defer) + Skip

Tier 3 (14): 00-hello, compress-options, cvs-exclude, delete_test, exclude_test, fuzzy_test, hands_test - all are either covered by existing parity / property tests today or duplicate of another more focused test.

Skip (5): output-options, simd-checksum, trimslash, unsafe-byname, wildmatch - covered by unit tests or are pure helper checks.

## 7. Cross-reference to existing UTS-N task IDs

Each Tier 1 port that overlaps an in-flight or completed UTS-N task is annotated above. Key clusters:

- **UTS-1 (`-e` lsh.sh wire parity):** files-from_test, exclude-lsh_test - completes UTS-1's coverage of the remote-shell argv split semantics.
- **UTS-2 (alt-dest):** alt-dest_test, alt-dest-deep_test - already partially in `crates/transfer/tests/uts_2_*`; new tests pin the depth + property dimensions.
- **UTS-9 (daemon-gzip):** daemon-gzip-upload_test - mirror of UTS-9's daemon-pull `-zz` test, covering the upload codec direction.
- **UTS-15 (batch-mode):** batch-mode_test - locks the end-to-end contract; UTS-15.a/b/c/g fixed individual frames.
- **UTS-18 (zlib codec):** compress-zlib-insert_test - completes UTS-18's invariant set.
- **UTS-22 (daemon-chroot-acl):** daemon-chroot-acl_test, daemon-access-ip_test, daemon-access_test - widens UTS-22 to cover daemon-access surface.
- **SEC-1.m/.n (symlink swap):** alt-dest-symlink-race, bare-do-open-symlink-race, chdir-symlink-race, chmod-symlink-race, copy-dest-source-symlink, sender-flist-symlink-leak - extends SEC-1 to the full receiver-side symlink-TOCTOU class.

## 8. Tests deliberately not ported

| Test | Reason |
|---|---|
| 00-hello_test.py | Smoke test; `--version`/`--info=help`/`--debug=help` already golden-tested; weird-name round-trip duplicates basic transfer coverage. |
| compress-options_test.py | Algorithm-selection breadth covered by `crates/compress/tests/strategy_integration.rs`. |
| cvs-exclude_test.py | Filter rules covered by `crates/filters/tests/`. |
| delete_test.py | `--del` dry-run + `--remove-source-files` covered by existing transfer tests; per-dir P/-/- rules covered by filter tests. |
| exclude_test.py | Filter precedence covered by `crates/filters/tests/complex_combinations.rs` + `negated_rules.rs`. |
| fuzzy_test.py | Root-level fuzzy already in `crates/transfer/tests/fuzzy_level2_basis_search.rs`. |
| hands_test.py | Richly-populated tree end-to-end smoke; spot-covered by many other tests. |
| output-options_test.py | Output shape covered by `crates/cli/tests/output_parity.rs`. |
| simd-checksum_test.py | SIMD vs scalar parity covered by `crates/checksums/` property tests. |
| trimslash_test.py | Internal helper; unit-test surface. |
| unsafe-byname_test.py | Direct `t_unsafe` helper call; unit-test surface. |
| wildmatch_test.py | Wildmatch covered by filter property tests. |

## 9. Speed budget per ported test

- Average upstream test wall time on `ofer@192.168.21.226`: 0.5-3 s (CPU-bound on small trees), 5-15 s for daemon round-trip cases.
- Target nextest wall-time per ported test: **<= 5 s** (P95). See `docs/design/uts-nextest-edge-b-test-harness.md` for how the harness keeps it bounded (in-process binary lookup, OS-assigned ports, deterministic tree sizes, no `sleep`-based sync).

## 10. Open questions deferred to the design doc

- How to fit a wire-byte capture (`tcpdump` equivalent) into a nextest while keeping the <=5 s budget. See UTS-NEXTEST-EDGE.b section "WireCapture".
- Whether ported tests should be `#[ignore]`d by default (require the binary) or run unconditionally. See UTS-NEXTEST-EDGE.b section "Crate placement convention".
- Rollback criteria if the upstream-testsuite cell remains authoritative anyway. See UTS-NEXTEST-EDGE.b section "Rollback criteria".

## 11. Next steps

1. Land UTS-NEXTEST-EDGE.a (this audit) + UTS-NEXTEST-EDGE.b (harness design) together so reviewers see the inventory and the implementation plan side by side.
2. File one parent issue per Tier 1 test (30 total) referencing this audit.
3. Land the harness primitives from UTS-NEXTEST-EDGE.b as a separate PR.
4. Port Tier 1 in waves of 5-10 PRs grouped by target crate (daemon first, then transfer, then metadata).
5. Reassess Tier 2 after Tier 1 completes - some may collapse if the harness makes them easy enough to write opportunistically.
