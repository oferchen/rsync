# UTS-NEXTEST-EDGE.a: shipped runtests inventory by operational-sanity value

Tracks: UTS-NEXTEST-EDGE.a (inventory phase). Parent series: UTS-NEXTEST-EDGE.

Companions:

- `docs/design/nxt-1-nextest-harness.md` - cross-binary harness shape, defines
  the NXT-2..NXT-8 curated port targets.
- `docs/design/uts-nextest-edge-b-test-harness.md` - internal-only test harness
  primitives (`OcRsyncDaemonHarness`, `DirDiff`, ...).
- `docs/audits/uts-nextest-edge-a-runtests-inventory.md` - sibling inventory of
  the 111-test 3.5.0dev `*_test.py` expansion. This document covers the 57
  *shipped* `.test` scripts in upstream's last tagged release (3.4.4); it does
  not duplicate the 3.5.0dev audit.

## 1. Purpose

The NXT-1 harness (PR #5994) wires up the cross-binary primitives but does
not enumerate every shipped upstream test, only the seven NXT-2..NXT-8
targets it expects to absorb first. The 30-test 3.5.0dev Tier-1 list in the
sibling audit reflects pre-release tests not yet on a stable rsync release.

This document does the inventory step against the *currently shipped*
testsuite (rsync 3.4.4, the latest GA tag), ranks each shipped `.test` by
operational-sanity value, cross-references each against the NXT-2..NXT-8 list
and any in-tree port already shipped, and identifies the High-value gap that
deserves additional NXT-* sub-tasks beyond the seven NXT-1 already named.

Inventory only. No code, no test changes.

## 2. Operational-sanity ranking criteria

Each shipped `.test` is scored on a single qualitative band:

- **High value.** Test exercises a wire-protocol invariant, a
  security-sensitive path (filter scoping, sandbox enforcement,
  secluded-args, symlink TOCTOU), or a historical regression hotspot for
  oc-rsync (goodbye flush, mode-0 sentinel, MSG_INFO framing, `path = /`
  module resolution). One regression here is a silent data-divergence or a
  security finding; PR-time catch is mandatory.

- **Medium value.** Test exercises feature parity that is not
  security-critical: ACL/xattr round-trip, hardlink dedup counts, atime/
  crtime preservation, depth-3 itemize, fuzzy scorer ranking. Failure is
  user-visible (wrong file mode at the destination) but not silent data
  loss; PR-time catch is desirable but not blocking.

- **Low value.** Test exercises upstream-rsync-specific quirks oc-rsync does
  not need to mirror (man-page text, internal-tool tests, `t_unsafe`
  byname). Already covered by unit-level parity tests. Porting buys only
  marginal coverage relative to the wall-time cost.

- **Skip.** Environment-dependent or root-only tests already covered by the
  sudo CI cells in the existing interop workflow (`tools/ci/run_interop.sh`
  + the `linux-sudo` matrix entry). Re-porting under the standard nextest
  cell either self-skips or adds nothing.

The bar for High is intentionally high: every High-value test is a candidate
for a NXT-* port that ships with the NXT-1 harness primitive. The Medium
list is the Tier-2 backlog; the Low and Skip lists are the rationale for
non-ports so future sub-tasks do not re-litigate them.

## 3. Ranked inventory

Tests are listed in lexical order. The "NXT today" column cites:

- `NXT-N` for an upstream test already explicitly targeted by the NXT-1
  curated list (NXT-2..NXT-8).
- `uts_*` for an in-tree nextest already shipped that pins the same scenario.
- `-` when nothing in-tree covers the scenario today.

The "Recommended" column proposes a numerical sub-task ID continuing the
NXT-* series past NXT-8 (NXT-9, NXT-10, ...). NXT-9 onwards are the
follow-up tasks this inventory unlocks; this document does not file them,
the synthesis section names which to file first.

| # | Test | Topic | Sanity | NXT today | Recommended |
|--:|---|---|---|---|---|
|  1 | 00-hello.test | smoke | Low | - | none (covered by `crates/cli/tests/output_parity.rs`) |
|  2 | acls-default.test | metadata/ACL | High | - | NXT-9 |
|  3 | acls.test | metadata/ACL | Medium | - | NXT-10 (Tier-2 batch) |
|  4 | alt-dest-symlink-race.test | security/symlink | High | - | NXT-11 |
|  5 | alt-dest.test | transfer/alt-dest | Medium | `uts_2_dest_root_mkdir.rs`, `uts_2_efg_ssh_alt_dest_verification.rs`, NXT-8 | none (NXT-8 covers SSH-mode mtime gap; UTS-2 covers root mkdir) |
|  6 | atimes.test | metadata/timestamps | Medium | - | NXT-12 (Tier-2) |
|  7 | backup.test | transfer/backup | Medium | - | NXT-13 (Tier-2) |
|  8 | bare-do-open-symlink-race.test | security/symlink | High | - | NXT-14 |
|  9 | batch-mode.test | transfer/batch | High | NXT-7 | covered by NXT-7 |
| 10 | chdir-symlink-race.test | security/symlink | High | NXT-5, `uts_nextest_chdir_symlink_race.rs` | covered |
| 11 | chgrp.test | metadata/ownership | Skip | - | none (sudo cell) |
| 12 | chmod-option.test | transfer/daemon-chmod | High | - | NXT-15 |
| 13 | chmod-symlink-race.test | security/symlink | High | - | NXT-16 |
| 14 | chmod-temp-dir.test | transfer/temp-dir | High | - | NXT-17 |
| 15 | chmod.test | transfer/mode | Medium | - | NXT-18 (Tier-2) |
| 16 | chown.test | metadata/ownership | Skip | - | none (sudo cell) |
| 17 | clean-fname-underflow.test | protocol/parser | High | - | NXT-19 |
| 18 | copy-dest-source-symlink.test | security/symlink | High | - | NXT-20 |
| 19 | crtimes.test | metadata/timestamps | Medium | - | NXT-21 (Tier-2) |
| 20 | daemon-chroot-acl.test | daemon/security | High | - | NXT-22 |
| 21 | daemon-gzip-download.test | daemon/codec | High | NXT-2, `uts_9_daemon_gzip_download_goodbye.rs` (`#[ignore]`) | covered by NXT-2 |
| 22 | daemon-gzip-upload.test | daemon/codec | High | `uts_nextest_daemon_gzip_zz.rs` | covered |
| 23 | daemon-refuse-compress.test | daemon/refuse | High | - | NXT-23 |
| 24 | daemon.test | daemon/basic | High | - | NXT-24 |
| 25 | delay-updates.test | transfer/delay-updates | Medium | - | NXT-25 (Tier-2) |
| 26 | delete.test | transfer/delete | Medium | `uts_nextest_daemon_delete_stats.rs` | NXT-26 (Tier-2; delete-deep variant) |
| 27 | devices.test | metadata/devices | Skip | - | none (sudo cell, `mknod`) |
| 28 | dir-sgid.test | transfer/mode | Medium | - | NXT-27 (Tier-2) |
| 29 | duplicates.test | transfer/flist | Medium | - | NXT-28 (Tier-2) |
| 30 | exclude-lsh.test | transfer/filter | High | NXT-3, `crates/cli/tests/exclude_lsh_six_leg.rs` (in-tree, recent PR #5985-area) | covered by NXT-3 |
| 31 | exclude.test | filter | Low | - | none (covered by `crates/filters/tests/`) |
| 32 | executability.test | transfer/mode | Medium | - | NXT-29 (Tier-2) |
| 33 | files-from.test | transfer/files-from | High | `uts_files_from_ssh_push_relative.rs` | NXT-30 (broaden to 4-leg matrix) |
| 34 | fuzzy.test | transfer/fuzzy | Low | - | none (covered by `crates/transfer/tests/fuzzy_*.rs`) |
| 35 | hands.test | smoke | Low | - | none (richly-populated tree; spot-covered) |
| 36 | hardlinks.test | transfer/hardlinks | Medium | `uts_nextest_hardlinks_inc_recurse.rs` | NXT-31 (Tier-2; non-INC-RECURSE variant) |
| 37 | itemize.test | logging/itemize | Medium | - | NXT-32 (Tier-2) |
| 38 | longdir.test | transfer/path | Medium | - | NXT-33 (Tier-2) |
| 39 | merge.test | transfer/multi-source | Medium | - | NXT-34 (Tier-2) |
| 40 | missing.test | transfer/missing | Medium | - | NXT-35 (Tier-2) |
| 41 | mkpath.test | transfer/mkpath | Medium | - | NXT-36 (Tier-2) |
| 42 | open-noatime.test | metadata/atime | Medium | - | NXT-37 (Tier-2) |
| 43 | protected-regular.test | transfer/security | High | - | NXT-38 |
| 44 | proxy-response-line-too-long.test | transport/proxy | High | - | NXT-39 |
| 45 | relative.test | transfer/relative | Medium | - | NXT-40 (Tier-2) |
| 46 | safe-links.test | transfer/symlink | High | - | NXT-41 |
| 47 | secure-relpath-validation.test | security/path | High | - | NXT-42 |
| 48 | sender-flist-symlink-leak.test | daemon/security | High | - | NXT-43 |
| 49 | simd-checksum.test | checksums | Skip | - | none (covered by `crates/checksums/` parity tests) |
| 50 | ssh-basic.test | transport/ssh | Medium | - | NXT-44 (Tier-2) |
| 51 | symlink-dirlink-basis.test | transfer/symlink | Medium | `uts_dd_symlink_dirlink_basis_topologies.rs` | covered |
| 52 | symlink-ignore.test | transfer/symlink | Medium | - | NXT-45 (Tier-2) |
| 53 | trimslash.test | helper | Skip | - | none (path-normalization unit test) |
| 54 | unsafe-byname.test | helper | Skip | - | none (direct `t_unsafe` call) |
| 55 | unsafe-links.test | transfer/symlink | High | - | NXT-46 |
| 56 | wildmatch.test | helper | Skip | - | none (covered by filter property tests) |
| 57 | xattrs.test | metadata/xattr | Medium | - | NXT-47 (Tier-2) |

## 4. Distribution

Counts by sanity band:

| Band | Count |
|---|--:|
| High | 23 |
| Medium | 23 |
| Low | 4 |
| Skip | 7 |
| Total | 57 |

Of the 23 High-value tests:

- **Already pinned in-tree or by NXT-1 curated targets**: 6
  - `batch-mode.test` (NXT-7)
  - `chdir-symlink-race.test` (NXT-5 + `uts_nextest_chdir_symlink_race.rs`)
  - `daemon-gzip-download.test` (NXT-2; `uts_9_*` is `#[ignore]`d, NXT-2
    will lift the gate)
  - `daemon-gzip-upload.test` (`uts_nextest_daemon_gzip_zz.rs`)
  - `exclude-lsh.test` (NXT-3 + `crates/cli/tests/exclude_lsh_six_leg.rs`)
  - `files-from.test` (`uts_files_from_ssh_push_relative.rs` covers the
    one-leg SSH push case; NXT-30 widens to the 4-leg matrix - listed as
    a Tier-2 broadening, not a fresh High-value gap)

- **Not yet ported (Tier-1 gap)**: 17
  - `acls-default.test`
  - `alt-dest-symlink-race.test`
  - `bare-do-open-symlink-race.test`
  - `chmod-option.test`
  - `chmod-symlink-race.test` (cousin of NXT-5; same family, distinct
    invariant: receiver-side `chmod()` symlink follow)
  - `chmod-temp-dir.test`
  - `clean-fname-underflow.test`
  - `copy-dest-source-symlink.test`
  - `daemon-chroot-acl.test`
  - `daemon-refuse-compress.test`
  - `daemon.test`
  - `protected-regular.test`
  - `proxy-response-line-too-long.test`
  - `safe-links.test`
  - `secure-relpath-validation.test`
  - `sender-flist-symlink-leak.test`
  - `unsafe-links.test`

(NXT-4 `daemon-refuse_test.py` from the 3.5.0dev expansion targets the
*broader* refuse case; the shipped `daemon-refuse-compress.test` is the
narrower compress-only variant and is therefore not de-duplicated against
NXT-4 - this inventory bounds to shipped tests.)

## 5. High-value gap analysis

Of the 23 High-value shipped tests, **17 are not yet pinned at PR-time** by
either an in-tree nextest or one of the NXT-2..NXT-8 curated NXT-1 targets.
That is the Tier-1 follow-up backlog for UTS-NEXTEST-EDGE.

Recommended priority within the 17 (highest first):

1. `daemon.test` (NXT-24) - foundational daemon round-trip; every other
   daemon-* test is downstream of this passing.
2. `daemon-chroot-acl` (NXT-22) - GHSA-rjfm-3w2m-jf4f-class hostname ACL
   bypass during chroot; security-sensitive.
3. `sender-flist-symlink-leak` (NXT-43) - daemon-module attacker scenario;
   sender-side flist generator following attacker symlink out of module.
4. `clean-fname-underflow` (NXT-19) - crafted merge filename hits
   `clean_fname()` over `--server`; must reject not crash. Signal-death is
   a release-blocker class.
5. `proxy-response-line-too-long` (NXT-39) - off-by-one stack OOB; same
   class as #4.
6. `secure-relpath-validation` (NXT-42) - path component splitter; bare
   `..` and `subdir/..` already known to slip past the front door.
7. `acls-default` (NXT-9) - silent ACL drop on inherited POSIX default ACL;
   high failure severity, low wall-time cost.
8. `alt-dest-symlink-race` (NXT-11), `bare-do-open-symlink-race` (NXT-14),
   `chmod-symlink-race` (NXT-16), `copy-dest-source-symlink` (NXT-20) -
   symlink TOCTOU family, share harness primitives; port as one wave.
9. `safe-links` (NXT-41), `unsafe-links` (NXT-46) - symlink-policy
   semantics; silent symlink drop class.
10. `protected-regular` (NXT-38) - Linux-only `fs.protected_regular` gate;
    `--inplace` must still write.
11. `chmod-temp-dir` (NXT-17), `chmod-option` (NXT-15) - daemon-side chmod
    + cross-fs rename.
12. `daemon-refuse-compress` (NXT-23) - narrow refuse-options coverage.

Items 1-7 (priorities 1-7) are the proposed UTS-NEXTEST-EDGE.q-w follow-up
sub-tasks - the q-w letters continue the parent series past .o naming.
Items 8-12 are .x-z and beyond; expect to file them as a single wave after
the harness primitives land.

## 6. Medium-value backlog

The 23 Medium tests are listed in section 3 column "Recommended" as NXT-10
.. NXT-47 (Tier-2). Port these in batches by target crate after the High-
value gap closes. Splitting Tier-2 by crate keeps each PR reviewable: one
batch for `crates/metadata/tests/operational/` (atimes, crtimes, xattrs,
open-noatime), one for `crates/transfer/tests/operational/`, and so on.

A subset of Medium tests collapses if the harness primitive already exists
- e.g. `acls.test` becomes a depth-property variant of `acls-default.test`
and can ship in the same PR.

## 7. Tests deliberately not ported

| Test | Reason |
|---|---|
| 00-hello.test | `--version`/`--info=help` golden-tested in `crates/cli/tests/output_parity.rs`; weird-name + `lsh.sh` round-trip duplicates basic transfer coverage. |
| exclude.test | Filter precedence + per-dir filter files covered by `crates/filters/tests/complex_combinations.rs` + `negated_rules.rs`. |
| fuzzy.test | Root-level fuzzy covered by `crates/transfer/tests/fuzzy_level2_basis_search.rs`. |
| hands.test | Richly-populated tree end-to-end smoke; spot-covered by many other tests. |
| simd-checksum.test | SIMD vs scalar parity covered by `crates/checksums/` property tests. |
| trimslash.test | Path-normalization unit test; already covered. |
| unsafe-byname.test | Direct `t_unsafe` helper call; unit-test surface. |
| wildmatch.test | Filter property tests already cover wildmatch. |
| chgrp.test | Root-only; sudo CI cell. |
| chown.test | Root-only; sudo CI cell. |
| devices.test | Root-only for real `mknod`; sudo CI cell. |

## 8. Reconciliation with the 3.5.0dev audit

The sibling audit (`docs/audits/uts-nextest-edge-a-runtests-inventory.md`)
inventoried 111 `_test.py` files from the pre-release 3.5.0dev testsuite
and identified 30 Tier-1 ports. The two documents are complementary:

- **This inventory** is bounded to the 57 shipped `.test` scripts; every
  user running a tagged upstream rsync today is running this exact set.
  This is the "guard the shipped contract" view.
- **The 3.5.0dev audit** is the forward-looking view: as soon as 3.5.0
  ships, the surface widens to those 111 tests. The 30 Tier-1 ports there
  include cases (`reverse-daemon-delta_test.py`, `link-dest-relative-basis_test.py`,
  `delete-missing-args-files-from_test.py`) that have no shipped counterpart
  and are therefore not in this document.

When 3.5.0 ships and `target/interop/upstream-src/rsync-3.5.0/testsuite/`
becomes the new ground truth, this document is superseded by the 3.5.0dev
audit's Tier-1 list (or a refresh of it). Until then, the two are
non-overlapping work streams.

## 9. Speed budget per ported test

- Per-test wall time budget: <= 5 s P95 (matches NXT-1 section 4 contract).
- Hard cap: 30 s via harness internal timeout (matches NXT-1 section 4
  contract).
- Total expected wall-time for the 17 not-yet-ported High items: ~85 s
  P95. That fits the existing `upstream-compat` cell budget (NXT-1
  section 11 rollback criterion is 5-minute cell wall time).

## 10. Next steps

1. Land this inventory PR (UTS-NEXTEST-EDGE.a).
2. File priority-1..7 sub-tasks as UTS-NEXTEST-EDGE.q-w (one per test) once
   the NXT-1 harness implementation PR (separate from the design PR) lands.
3. Port the symlink-TOCTOU wave (items 8-9 in section 5) as a single PR
   batch since they share `LshRunnerStub` + symlink-swap helpers.
4. Reassess Tier-2 after the High-value gap closes; some collapse to one-
   liner variants of already-shipped tests.

## 11. Acceptance criteria for UTS-NEXTEST-EDGE.a

This inventory PR is acceptance-ready when:

1. Every shipped 3.4.4 `.test` file appears in section 3 with a sanity
   band and a recommendation column entry.
2. Each High-value test that is already pinned (in-tree or by NXT-1
   curated targets) cites the specific covering test path.
3. The 15-item High-value gap matches a hand count of section 3 rows
   whose "NXT today" column is `-`.
4. The reconciliation with the 3.5.0dev audit (section 8) names the
   non-overlapping cases by file.

This document files no new sub-tasks; sub-task filing is step 2 of
section 10.
