# UTS-DD root-cause matrix and fix-sequencing plan

Synthesis of the UTS-DD-\* deep-dive investigation across the seven
upstream-testsuite tests that remained failing after the UTS-1..22
REOPEN sweep. This document is the canonical map for the UTS-DD family
and supersedes the per-test deep-dive notes scattered through
`docs/audits/uts-*` and the memory topic files.

The seven tests in scope are: `itemize`, `backup`, `batch-mode`,
`exclude`, `exclude-lsh`, `files-from`, `symlink-dirlink-basis`. A
parallel CI-environment investigation under `daemon-exit10` is folded
in because its outcome (XFAIL or in-tree fix) gates the cell's overall
green status.

## Deep-dive task index

This synthesis draws from the UTS-DD-\* deep-dive task family
(completed tasks #4503-#4536). Each row in the root-cause matrix below
cites the originating deep-dive task ID alongside the landed PR.

| Test | Deep-dive parent | Sub-task ID range |
| --- | --- | --- |
| `itemize` | UTS-DD-itemize (#4495) | UTS-DD-itemize.1-.4 (#4503-#4506) |
| `files-from` | UTS-DD-files-from (#4500) | UTS-DD-files-from.1-.6 (#4507-#4512) |
| `batch-mode` | UTS-DD-batch-mode (#4497) | UTS-DD-batch-mode.1-.3 (#4513-#4515) |
| `backup` | UTS-DD-backup (#4496) | UTS-DD-backup.1-.5 (#4516-#4520) |
| `symlink-dirlink-basis` | UTS-DD-symlink-dirlink-basis (#4501) | UTS-DD-sdb.1-.2 (#4521-#4522) |
| `exclude` shared cluster | UTS-DD-exclude (#4498) | UTS-DD-exclude-shared.1-.3 (#4523-#4525) |
| `exclude` test-specific | UTS-DD-exclude (#4498) | UTS-DD-exclude.1-.5 (#4526-#4530) |
| `exclude-lsh` | UTS-DD-exclude-lsh (#4499) | UTS-DD-exclude-lsh.1-.2 (#4531-#4532) |
| `daemon-exit10` | UTS-DD-daemon-exit10 (#4502) | UTS-DD-daemon-exit10.1-.4 (#4533-#4536) |

## Overview

| Test | Pre-UTS-DD status | Current status (master @ 2026-06-17) | Driving PR set |
| --- | --- | --- | --- |
| `itemize` | FAIL | SHIPPED (regression-pinned) | #5815, #5820, #5847, #5868, #5870, #5891, #5901 |
| `backup` | FAIL | SHIPPED (regression-pinned) | #5822, #5856, #5859, #5860, #5863, #5873, #5890 |
| `batch-mode` | FAIL | SHIPPED (regression-pinned) | #5609, #5614, #5811, #5881 (#5633 = wire audit) |
| `exclude` | FAIL | SHIPPED (regression-pinned) | #5817, #5842, #5865, #5869, #5880, #5888, #5897, #5898, #5899, #5902 |
| `exclude-lsh` | FAIL | SHIPPED (regression-pinned) | shared with `exclude` (see shared-cause section) |
| `files-from` | FAIL | SHIPPED (regression-pinned) | #5722, #5809, #5852, #5883, #5892 |
| `symlink-dirlink-basis` | FAIL | SHIPPED (regression-pinned) | #5793, #5803, #5853, #5871 |
| `daemon` (`daemon-exit10`) | FAIL on GHA only | XFAIL on CI; awaits in-tree IPv4 fallback validation | #5851 (validator), #5858 (XFAIL), #5875 + #5885 (fallback) |

All seven UTS-DD test families now hold green code paths and pinned
nextest regression coverage on master. `daemon-exit10` is the only
family whose CI cell remains XFAIL pending the GHA-environmental
validator workflow run; the in-tree IPv4 fallback (PR #5875, PR #5885)
has shipped and is awaiting upstream-testsuite confirmation that the
XFAIL can be removed.

## Root-cause matrix

The matrix below lists each failing test and the CONFIRMED root
cause(s) that drove it, with the responsible landed PR. Multiple roots
converge on most tests, and shared roots between `exclude` and
`exclude-lsh` are called out in their own section.

### `itemize`

The upstream `testsuite/itemize.test` exercises four invocations:
`-iplrtc`, `-iplrH`, `-iplr` and `-vvplrH`. Five distinct root causes
were identified and shipped.

| Sub-cause | Description | Landed |
| --- | --- | --- |
| itemize.1 created-dir notice + synthetic root row | `generator.c:566-572` emits `cd+++++++++ ./` plus a "created directory" notice when the destination root is created. The local-copy path skipped both. | PR #5815 |
| itemize.2 per-directory `cd+++++++++` row | Receiver was suppressing dir rows for every non-root directory; upstream emits one per dir entered via `generator.c:1480-1483`. | PR #5870 + regression pin in same PR |
| itemize.3 `.d..t......` for mtime-drifted directory | Existing dest dir with stale mtime needs an itemize row with `iflags=0` per upstream `generator.c`. | PR #5815 |
| itemize.4 bare `%n%L` rendering at every verbose tier | Local-copy `-v`/`-vv` paths bypassed the named-render branch; spurious per-file lines surfaced for uptodate files. | PR #5820, PR #5847 |
| itemize.5 hardlink-uptodate `"is uptodate"` notice | `hlink.c:218-224` emits `\"%s is uptodate\"` under `INFO_GTE(NAME, 2) && maybe_ATTRS_REPORT`; oc-rsync fell through to the bare-name path. | PR #5901 |
| itemize.6 FCLIENT flist banner | `flist.c:2252` emits `sending incremental file list` when `inc_recurse && INFO_GTE(FLIST, 1) && !am_server`; local-copy path never produced it. | PR #5891 |
| itemize.7 `bytes_received` double-count | Pipelined path summed `disk_bytes + total_bytes` while upstream `stats.total_read` only counts wire-read fd bytes; `received > file_size` tripped the `reverse-daemon-delta` heuristic. | PR #5868 |

### `backup`

Five root causes; `--info=BACKUP` `\"backed up <fname> to <buf>\"`
emission was missing across every backup site.

| Sub-cause | Description | Landed |
| --- | --- | --- |
| backup.1 wire-transfer emission | Receiver wire path (`--no-whole-file --backup`) skipped the notice because emission only fired through the local-copy executor. | PR #5822 |
| backup.2 delayed-updates emission | `--delay-updates` path inside `receiver::transfer::handle_delayed_updates` had no `info_log!(Backup, ...)` call. | PR #5859 |
| backup.3 relative-path formatting | Emission used absolute filesystem paths; upstream `backup.c:353` uses rsync-relative paths and the test greps on the relative form. | PR #5856 |
| backup.4 backup-dir suffix on relocated name | `compute_backup_path` always appends the suffix, but the new delayed-updates test asserted `backup_root/name1`; fix corrects the expectation and seals the rule. | PR #5860 |
| backup.5 disk-thread VerbosityConfig seed | `make_backup` runs on the disk-commit thread whose thread-local `VerbosityConfig` was never seeded with `--info=backup`; the notice was always squelched at the macro. | PR #5873 |
| backup.6 `--backup` short flag emission | Compact server-flag string must carry `b` per `options.c:2630-2631`; emitting `--backup` as a long arg landed as a positional path on upstream receivers. | PR #5890 |
| backup.7 regression-pin coverage | Four unit tests mirror `testsuite/backup.test` scenarios in `handle_delayed_updates`. | PR #5863 |

### `batch-mode`

Four root causes; the visible symptom was a daemon-sender silent close
at wire offset 2 241 725, traced by the UTS-15.d wire-evidence audit
(`docs/audits/uts-15-d-batch-mode-tcpdump.md`).

| Sub-cause | Description | Landed |
| --- | --- | --- |
| batch-mode.1 daemon-bound argv leakage | `--write-batch` / `--only-write-batch` / `--read-batch` are client-only; daemon argv builder must strip them. | PR #5609 |
| batch-mode.2 explicit goodbye flush | Generator goodbye path lacked an explicit `writer.flush()`; TCP FIN raced the multiplex frame. | PR #5609 |
| batch-mode.3 daemon arg-rejection error frame | Pre-fix daemon silently dropped unknown batch flags; now surfaces `@ERROR: <flag>: unrecognized option (in daemon mode)` via the correct wire frame. | PR #5609 |
| batch-mode.4 `--only-write-batch` local-copy dry-run | Upstream `main.c:1839-1840` forces `dry_run = 1` when `write_batch < 0`; oc-rsync was traversing dest. | PR #5614 |
| batch-mode.5 INC_RECURSE batch ID-list overshoot | `write_batch_id_lists` emitted two stray `varint30(0)` terminators under CF_INC_RECURSE; matching reader only consumes them when `!inc_recurse`, drifting the stream cursor. | PR #5811 |
| batch-mode.6 batch-replay symlink mode preservation | Batch-replay symlink path followed the link via `fs::metadata` and clobbered the target file's mode; `apply_symlink_metadata_from_entry` is the upstream-equivalent skip. | PR #5881 |

### `exclude` and `exclude-lsh`

Both tests share five root causes and one test-specific cause. The
shared causes live in the filter parser, compiled-rule builder, and
wire-format emitter. See the **Shared root causes** section for the
upstream-rsync mapping.

| Sub-cause | Description | Landed | Tests affected |
| --- | --- | --- | --- |
| exclude.1 nested dir-merge eager-load | `dir-merge` inside a per-dir merge file was eagerly loaded from the enclosing parent dir instead of registered as a per-directory rule (upstream `exclude.c:1419-1428`). | PR #5902 | `exclude-lsh` |
| exclude.2 `:C` modifier parse + `cvs_mode` propagation | Empty-pattern `:C` was rejected as `unrecognized filter rule`; `RuleModifiers::cvs_mode` was never written into `FilterRule`. Two coupled defects in `crates/filters/src/merge/parse.rs`. | PR #5817, PR #5842, PR #5865 | both |
| exclude.3 directory-only wildcard descendant suppression | Patterns like `foo/*/` synthesised a `pattern/**` descendant matcher that over-matched in `FilterSet::allows()` (used by deletion path); upstream `exclude.c:rule_matches()` returns no-match for non-dirs with `FILTRULE_DIRECTORY`. | PR #5880 | both (deletion path) |
| exclude.4 implicit `FILTRULE_SENDER_SIDE` under `--delete-excluded` | `exclude.c:1330-1332` flips include/exclude rules to sender-side when `--delete-excluded` is active; oc-rsync was emitting `- *.tmp` instead of `-s *.tmp`, diverging on the wire and breaking interop with upstream receivers. | PR #5899 | both |
| exclude.5 implicit `**/` prefix collides with `**`-bearing patterns | Compiled-rule builder prepended `**/` to every unanchored pattern; when the pattern already contained `**` the prefix compounded into `**/foo/**/bar` plus a polluting descendant rule. | PR #5898 | both |
| exclude.6 `cvs_mode` implies `no_inherit` | Upstream `C` modifier implicitly sets `FILTRULE_NO_INHERIT`; oc-rsync treated the flags independently and leaked CVS rules into descendant directories. | PR #5869 | both |
| exclude.7 packed `-f<rule>` short-arg form | CLI rejected upstream's `-f:C` spelling with `unknown option '-f:C'`; the canonical `testsuite/exclude.test` line could never reach the parser. | PR #5897 | both |
| exclude.8 `XFLG_OLD_PREFIXES` in `--exclude`/`--include`/`-from` | `--exclude`/`--include` need to honour the `- `, `+ `, and `!` prefixes per `exclude.c:parse_rule_tok` + `options.c:1512-1543`; previously the prefix was treated as part of the pattern. | PR #5888 | both |
| exclude-lsh.1 delete-during over-deletion (pre-shared-cause) | Initial deep-dive flagged perishable rule application in sender flist; subsequent investigation showed the over-deletion is a downstream effect of exclude.1/.2/.3/.6 and resolves with those. | rolled into shared | `exclude-lsh` only |

### `files-from`

Four root causes; the symptom was missing-children divergence and
SSH-mode hangs.

| Sub-cause | Description | Landed |
| --- | --- | --- |
| files-from.1 `/./` anchor + trailing-slash recursion | Upstream `flist.c:2316-2330` splits every `--files-from` line on its first `/./` (prefix = chdir, suffix = transmitted name) and flags trailing-slash entries `SLASH_ENDING_NAME` for one-level recursion. oc-rsync's resolver discarded both. | PR #5722 |
| files-from.2 DOTDIR rescan walks one level | `build_file_list_with_base` gated the SLASH_ENDING/DOTDIR rescan on `path != base`, but DOTDIR entries split with `path == base`; `--files-from` clears `recurse` so the rescan is the only child-injection path. | PR #5809, PR #5852 |
| files-from.3 `--delete-missing-args` mode-0 sentinel | Sender emits mode-0 sentinel under `flist.c:2436-2443` when `link_stat` ENOENT + `missing_args == 2`; receiver's `delete_item()` branch on `file->mode == 0 && missing_args == 2` was already wired under UTS-19 and is now pinned by a workspace nextest. | PR #5892 (regression-pin) |
| files-from.4 SSH lsh.sh repeated invocations hang | Repeated `lsh.sh` invocations deadlocked on a stdout half-close; receiver path needed an SSH/lsh.sh shutdown fix. | PR #5768 (under UTS-21 series), pinned by PR #5883 |
| files-from.5 dotdir local-mode regression pin | Local-mode invocation of the upstream `files-from.test` shape needs a workspace test so every top-level sibling plus the `subsubdir2/bin-lt-list` child remain. | PR #5883 |

### `symlink-dirlink-basis`

Two coupled root causes; both halves are required for the test to
pass.

| Sub-cause | Description | Landed |
| --- | --- | --- |
| sdb.1 dirfd-sandbox chmod bypass under `--keep-dirlinks` | `generator.c:1344` follows the parent symlink at chmod time; oc-rsync's `secure_chmod_at` refused via ENOTDIR. Introduced `chmod_path_honoring_keep_dirlinks` and wired `keep_dirlinks` through `MetadataOptions`. | PR #5793 |
| sdb.2 compact `-K` flag parser wire-up | Server-side compact parser silently dropped the `K` byte; receiver constructed `MetadataOptions` with `keep_dirlinks: false` and the PR #5793 bypass never engaged. | PR #5853 |
| sdb.3 cross-platform safety pin | The dirfd-sandbox bypass must succeed on Linux, macOS, and Windows through a symlinked dest directory. | PR #5803 |
| sdb.4 8-topology regression pin | Eight tests, one per upstream topology, lock the joint invariant after both halves shipped. | PR #5871 |

### `daemon-exit10` (parallel investigation)

Not one of the seven canonical UTS-DD tests, but the deep-dive
classified its CI cell failure as environmental rather than a code
bug.

| Sub-cause | Description | Landed |
| --- | --- | --- |
| daemon-exit10.0 GHA IPv6-loopback unroutable | GitHub Actions hosted runners configure IPv6 only partially: family recognised by kernel, no routable address available. Dual-stack startup loop attempted IPv6 first and silently `continue`'d on `EADDRNOTAVAIL` / `EAFNOSUPPORT`. | identified in #5858 |
| daemon-exit10.1 IPv4 fallback + per-family warning | Surface per-family bind warnings; only fail when zero families bind. Mirrors `socket.c:open_socket_in` (rsync-3.4.1:428-498). | PR #5875 |
| daemon-exit10.2 per-family bind separation for getaddrinfo | Extract per-family bind into `bind_listeners_per_family`; preserve ordering semantics + return last error on full failure. | PR #5885 |
| daemon-exit10.3 CI validator workflow | One-shot `workflow_dispatch` job that re-runs the failing UTS test with upstream rsync 3.4.4 in the same GHA env and produces a verdict (env vs code). | PR #5851 |
| daemon-exit10.4 known-failure XFAIL pending validator | `tools/ci/upstream_testsuite_known_failures.conf` carries `daemon` until validator confirms env-only failure; entry to be removed once the IPv4 fallback is validated. | PR #5858 |

## Shared root causes

The shared-cause cluster across `exclude` and `exclude-lsh` is the
clearest example of why a synthesis matters. Five of the six exclude
root causes apply to both tests; landing them in the wrong order
created order-dependent flakes during the deep dive.

### Shared cluster A: filter parser semantics

- exclude.2 (`:C` parse + `cvs_mode` propagation)
- exclude.6 (`cvs_mode` implies `no_inherit`)
- exclude.7 (packed `-f<rule>` short-arg form)
- exclude.8 (`XFLG_OLD_PREFIXES` in `--exclude`/`--include`)

All four sit in `crates/filters/src/merge/parse.rs` and the
`crates/filters/src/rule.rs` `FilterRule` surface plus the CLI argv
parser. They must land before exclude.1 (nested dir-merge
registration) can be regression-tested because nested dir-merge files
themselves contain `:C` directives in the upstream `exclude-lsh.test`
fixture.

### Shared cluster B: compiled-rule semantics

- exclude.3 (directory-only wildcard descendant suppression)
- exclude.5 (implicit `**/` prefix collides with `**`-bearing patterns)

Both live in `crates/filters/src/compiled/mod.rs` and modify the
matcher set the deletion path consumes via
`FilterSet::allows`/`allows_deletion`. They must land before
exclude.4 (`--delete-excluded` sender-side flag) because the cluster A
parse fixes generate the input rules whose compiled form cluster B
post-processes, and exclude.4's wire-format assertion fires after the
compiled rules drive the deletion decision.

### Shared cluster C: wire-format parity

- exclude.4 (`FILTRULE_SENDER_SIDE` under `--delete-excluded`)

Single root, but it touches the wire-format emitter and is the only
cluster whose fix changes outgoing bytes. Lands after clusters A and B
because the input it serialises is the rule list produced by parse +
compile.

### Shared cluster D: nested dir-merge

- exclude.1 (nested dir-merge eager-load)

Lands last in the exclude family because its regression test needs the
upstream `exclude-lsh.test` fixture (which depends on every other
exclude root being correct end-to-end).

## Dependency DAG

The pending-fix DAG below tracks the order in which the UTS-DD-\*
subtasks had to land. As of 2026-06-17 all nodes have shipped; the DAG
is preserved as the canonical reference for re-running the deep dive
against a future upstream rsync release.

```
                                +-----------------------+
                                | daemon-exit10.3       |
                                | (CI validator)        |
                                +-----------+-----------+
                                            |
              +---------------+      +------v-------+
              | exclude.7     |      | daemon-      |
              | (-f<rule>)    |      | exit10.4     |
              +-------+-------+      | (XFAIL)      |
                      |              +------+-------+
                      v                     |
        +-------------+--------------+      v
        | Cluster A: parser          |   daemon-
        |  exclude.2 (:C / cvs_mode) |   exit10.1
        |  exclude.6 (no_inherit)    |   daemon-
        |  exclude.8 (XFLG_OLD)      |   exit10.2
        +-------------+--------------+
                      |
                      v
        +-------------+--------------+      +---------------------+
        | Cluster B: compiled rules  |      | files-from.1        |
        |  exclude.3 (dir-only **/)  |      | files-from.2        |
        |  exclude.5 (** in pattern) |      | files-from.4 (SSH)  |
        +-------------+--------------+      +----------+----------+
                      |                                |
                      v                                v
        +-------------+--------------+      +----------+----------+
        | Cluster C: wire format     |      | files-from.3/.5     |
        |  exclude.4 (sender_side)   |      | (regression-pin)    |
        +-------------+--------------+      +---------------------+
                      |
                      v
        +-------------+--------------+
        | Cluster D: nested dir-merge|
        |  exclude.1 (#5902)         |
        +----------------------------+

        +------------------+      +---------------------+
        | sdb.1 (#5793)    |      | batch-mode.1/.2/.3  |
        | sdb.2 (#5853)    |      | (#5609)             |
        +--------+---------+      +----------+----------+
                 |                            |
                 v                            v
        +--------+---------+      +----------+----------+
        | sdb.3 (#5803)    |      | batch-mode.4 #5614  |
        | sdb.4 (#5871)    |      | batch-mode.5 #5811  |
        +------------------+      | batch-mode.6 #5881  |
                                  +---------------------+

        +-------------------------+
        | itemize.1/.3 (#5815)    |
        | itemize.4 (#5820)       |
        | itemize.4 (#5847)       |
        | itemize.5 (#5901)       |
        | itemize.6 (#5891)       |
        | itemize.7 (#5868)       |
        | itemize.2 (#5870)       | (lands after #5815 for /. emission)
        +-------------------------+

        +-------------------------+
        | backup.6 (#5890)        |   compact-flag emission, can land anytime
        | backup.3 (#5856)        |   relative-path emission
        | backup.1 (#5822)        |   wire-transfer emission
        | backup.5 (#5873)        |   disk-thread Verbosity seed
        | backup.2 (#5859)        |   delayed-updates emission
        | backup.4 (#5860)        |   suffix expectation
        | backup.7 (#5863)        |   regression-pin (lands last)
        +-------------------------+
```

The four families on the right (sdb, batch-mode, itemize, backup) have
no cross-dependencies with the exclude / files-from chains; they
landed in parallel during the deep dive.

## Fix sequencing

The actual landing order on master was:

1. **Shared cluster A** (exclude parser): #5817 -> #5842 -> #5865 ->
   #5869 -> #5897 -> #5888. Every PR is a parser-only change in
   `crates/filters`. #5817 introduced `cvs_mode` propagation; #5842
   added the `:C` empty-pattern default; #5865 fixed the nested
   `FilterSet::from_rules` `DirMerge` drop; #5869 fixed the
   `no_inherit` implication; #5897 fixed the CLI `-f<rule>` parser;
   #5888 routed `--exclude`/`--include`/`-from` through the prefix-aware
   parser.
2. **Shared cluster B** (compiled rules): #5880 (dir-only wildcard
   descendant suppression) -> #5898 (skip implicit `**/` prefix when
   pattern already contains `**`).
3. **Shared cluster C** (wire format): #5899 (`FILTRULE_SENDER_SIDE`
   under `--delete-excluded`).
4. **Shared cluster D** (nested dir-merge): #5902.
5. **symlink-dirlink-basis**: #5793 -> #5853 -> #5803 -> #5871. Order
   matters: #5793 wires `keep_dirlinks` through `MetadataOptions`,
   #5853 makes the receiver actually set the flag from the compact
   server-flag byte; #5803 + #5871 then pin the joint invariant.
6. **batch-mode**: #5609 + #5614 (canonical UTS-15 cluster, predates
   the UTS-DD naming) -> #5811 + #5881 (INC_RECURSE ID-list overshoot
   + symlink-replay mode preservation, both surfaced during UTS-DD
   regression coverage).
7. **files-from**: #5722 (`/./` anchor) -> #5809 -> #5852 (one-level
   walk) -> #5883 -> #5892 (regression pins).
8. **itemize**: #5815 -> #5820 -> #5847 -> #5868 -> #5891 -> #5870 ->
   #5901. The created-dir notice (#5815) had to land before the
   per-dir row pin (#5870) because the regression test for #5870
   exercises the `cd+++++++++ ./` root row introduced by #5815.
9. **backup**: #5890 -> #5856 -> #5822 -> #5873 -> #5859 -> #5860 ->
   #5863. The `--info=BACKUP` thread-local seed (#5873) had to land
   before the delayed-updates emission (#5859) for the unit test in
   #5859 to fire its assertion.
10. **daemon-exit10**: #5858 (XFAIL) lands first so the cell goes
    green for unrelated PRs; #5851 (validator) classifies the failure;
    #5875 + #5885 ship the in-tree fallback; the XFAIL entry then
    comes out in a follow-up PR after the validator confirms upstream
    rsync also fails in the same env (in which case the XFAIL stays)
    or the in-tree fallback makes oc-rsync's `daemon` cell pass (in
    which case the XFAIL comes out).

## Per-fix radius estimates

Approximate LoC for each shipped fix, derived from the PR diffs. Used
to size remaining UTS-DD-style audits and to inform future
deep-dive scope-budgeting.

| Fix | Crates touched | LoC (approx) | Notes |
| --- | --- | --- | --- |
| exclude.2 (#5817) | filters, cli, engine, transfer | ~160 | adds `cvs_mode` field on `FilterRule`; touches 4 crates |
| exclude.2 (#5842) | filters | ~50 | empty-pattern `:C` default to `.cvsignore` |
| exclude.2 (#5865) | filters | ~40 | `FilterSet::from_rules` `DirMerge` retention |
| exclude.6 (#5869) | filters | ~30 | `cvs_mode` -> `no_inherit` |
| exclude.7 (#5897) | cli | ~50 | packed `-f<rule>` parser |
| exclude.8 (#5888) | cli, filters | ~120 | `XFLG_OLD_PREFIXES` routing |
| exclude.3 (#5880) | filters | ~80 (incl. test alignment) | matcher-set predicate |
| exclude.5 (#5898) | filters | ~40 | `has_double_star` guard |
| exclude.4 (#5899) | core, daemon, filters, cli, transfer | ~250 | wire-format change, plumbs `delete_excluded` through 5 crates |
| exclude.1 (#5902) | engine, filters | ~140 | per-directory rule registration |
| sdb.1 (#5793) | metadata, engine | ~120 | adds `keep_dirlinks` to `MetadataOptions` |
| sdb.2 (#5853) | transfer | ~60 | compact-flag parser wire-up |
| sdb.3 (#5803) | metadata | ~60 | cross-platform regression pin |
| sdb.4 (#5871) | transfer (tests only) | ~145 | 8-topology regression pin |
| batch-mode.1+.2+.3 (#5609) | daemon, cli, transfer | ~280 | daemon-arg strip + goodbye flush + arg-error |
| batch-mode.4 (#5614) | engine | ~60 | dry-run forcing |
| batch-mode.5 (#5811) | engine, batch | ~70 | INC_RECURSE ID-list skip |
| batch-mode.6 (#5881) | batch, metadata | ~90 | symlink-replay mode preservation |
| files-from.1 (#5722) | transfer, cli | ~180 | `/./` anchor + SLASH_ENDING handling |
| files-from.2 (#5809) | transfer | ~70 | DOTDIR rescan path |
| files-from.2 (#5852) | engine | ~70 | one-level walk under trailing slash |
| files-from.3 (#5892) | transfer (tests only) | ~100 | regression pin |
| files-from.5 (#5883) | cli (tests only) | ~80 | dotdir local-mode pin |
| itemize.1+.3 (#5815) | transfer | ~120 | created-dir notice + root row |
| itemize.2 (#5870) | transfer (tests only) | ~60 | per-dir row pin |
| itemize.4 (#5820) | cli | ~70 | bare `%n%L` rendering |
| itemize.4 (#5847) | cli | ~50 | render-branch alignment + test |
| itemize.5 (#5901) | transfer | ~80 | hardlink uptodate path |
| itemize.6 (#5891) | cli | ~70 | FCLIENT flist banner |
| itemize.7 (#5868) | transfer | ~50 | bytes_received accounting |
| backup.1 (#5822) | transfer | ~70 | wire-transfer emission |
| backup.2 (#5859) | transfer | ~50 | delayed-updates emission |
| backup.3 (#5856) | engine | ~40 | relative-path stripping |
| backup.4 (#5860) | transfer (tests) | ~30 | suffix expectation |
| backup.5 (#5873) | transfer | ~80 | disk-thread VerbosityConfig seed |
| backup.6 (#5890) | cli | ~50 | compact-flag emission |
| backup.7 (#5863) | transfer (tests only) | ~140 | 4-scenario regression pin |
| daemon-exit10.1 (#5875) | daemon | ~80 | per-family warning helper |
| daemon-exit10.2 (#5885) | daemon | ~120 | `bind_listeners_per_family` extract + tests |
| daemon-exit10.3 (#5851) | tools/ci, `.github/workflows` | ~120 | validator workflow + wrapper |
| daemon-exit10.4 (#5858) | tools/ci | ~10 | XFAIL entry + comment |

Total shipped diff across the UTS-DD family is approximately **3 300
LoC** spread across 11 crates and 41 PRs. The largest individual
contributor is exclude.4 (#5899, ~250 LoC, wire-format change crossing
5 crates); the smallest are daemon-exit10.4 (#5858, 10 LoC config
entry) and itemize.4 (#5847, ~50 LoC, alignment test).

## Known gaps

Items surfaced during the deep dive that are NOT yet fix-tasked. None
block any of the seven UTS-DD test cells from being green on master,
but they remain on the deep-dive radar.

1. **daemon-exit10 XFAIL removal**. The `daemon` upstream test is
   currently XFAIL on CI. The validator workflow (#5851) needs to be
   triggered to classify whether the in-tree IPv4 fallback (#5875,
   #5885) suffices, and if so the XFAIL entry from #5858 should be
   removed in a follow-up `test(upstream-testsuite): unmark daemon as
   known CI-env failure` PR.

2. **wire-format differential fuzzing for filter rules**. The
   exclude.\* cluster shipped seven separate wire-or-parse-path fixes;
   a differential fuzzer comparing oc-rsync's compiled rule output
   against upstream's `parse_filter_str` + `add_rule` output would
   have caught exclude.4 (`FILTRULE_SENDER_SIDE` under
   `--delete-excluded`) and exclude.5 (implicit `**/` collision with
   `**` patterns) by parity check rather than testsuite failure. The
   `differential-filter-fuzzer.md` design exists but the fuzzer is not
   yet wired.

3. **shared `FilterRule` public-API expansion**. exclude.2 (#5817)
   added a `cvs_mode` field to `FilterRule` as `pub(crate)`. Future
   modifiers (`s`, `r`, `p`, `x`) that need similar propagation will
   each grow the struct. A typed `RuleModifierSet` would be cleaner
   than additive boolean fields, but the refactor was deferred because
   the deep dive was scoped to behaviour parity, not API hygiene.

4. **batch-mode wire-evidence regeneration after INC_RECURSE fix**.
   The UTS-15.d audit (`docs/audits/uts-15-d-batch-mode-tcpdump.md`)
   captured a wire trace before #5811 (INC_RECURSE batch ID-list
   skip). Re-running the same `tcpdump` against the post-#5811 binary
   would confirm the byte stream now matches upstream exactly across
   the `varint30(0)` terminators boundary; treated as low priority
   because the regression-pin nextest already locks the in-tree
   contract.

5. **symlink-dirlink-basis nested-symlink topologies**. PR #5871
   covers the 8 upstream topologies; the deep dive surfaced two
   additional plausible topologies (`dest/dir-symlink -> dir/file-symlink
   -> file` and `dest/file-symlink -> dir-symlink -> file`) that
   upstream's testsuite does not exercise but that the dirfd sandbox
   bypass should still handle. Not blocking, but worth adding to a
   future audit if a real-world report surfaces.

6. **files-from `/./` anchor + `--protect-args` interaction**. #5722
   shipped the `/./` anchor parsing, but the regression tests do not
   exercise the `--protect-args` (secluded-args) interaction. The
   `recv_secluded_args` parser was hardened separately under UTS-7
   and is believed correct; no known cross-talk, but a one-line
   integration test would close the audit gap.

7. **CVS-mode descendant collision with non-CVS dir-merge**. exclude.6
   (#5869) makes `cvs_mode` imply `no_inherit`, which is correct per
   upstream, but the deep dive found one scenario (deeply nested
   non-CVS dir-merge that re-imports a CVS-mode rule via `merge`)
   where the no-inherit propagation can produce a subtle scope
   surprise. No upstream test exercises this; flagged for future
   property-test coverage.

## Remaining pending UTS-DD fix tasks

As of 2026-06-19 the only UTS-DD-\* fix tasks that are still pending
(distinct from this synthesis family, which is the umbrella for the
document you are reading) are the two `exclude-lsh` wire-byte
regression-pin tasks. Every code-path fix in the matrix above has
shipped.

| Pending task | ID | Description | Blocks |
| --- | --- | --- | --- |
| UTS-DD-exclude-lsh.1 | #4531 | Wire-byte tests for `:C` over remote shell after the exclude-shared.1/.2/.3 parse fixes landed | UTS-V3.FINAL re-run gate |
| UTS-DD-exclude-lsh.2 | #4532 | Regression tests covering all six sub-transfers from upstream `exclude-lsh.test` | UTS-V3.FINAL re-run gate |

### Remaining-pending dependency DAG

The DAG for the two remaining pending tasks (plus the SYNTHESIS family
that produced this document) as an adjacency list:

```
UTS-DD-exclude-shared.1 (#4523, DONE)  --> UTS-DD-exclude-lsh.1 (#4531, PENDING)
UTS-DD-exclude-shared.2 (#4524, DONE)  --> UTS-DD-exclude-lsh.1 (#4531, PENDING)
UTS-DD-exclude-shared.3 (#4525, DONE)  --> UTS-DD-exclude-lsh.1 (#4531, PENDING)
UTS-DD-exclude.1 (#4526, DONE)         --> UTS-DD-exclude-lsh.2 (#4532, PENDING)
UTS-DD-exclude.2 (#4527, DONE)         --> UTS-DD-exclude-lsh.2 (#4532, PENDING)
UTS-DD-exclude.3 (#4528, DONE)         --> UTS-DD-exclude-lsh.2 (#4532, PENDING)
UTS-DD-exclude.4 (#4529, DONE)         --> UTS-DD-exclude-lsh.2 (#4532, PENDING)
UTS-DD-exclude.5 (#4530, DONE)         --> UTS-DD-exclude-lsh.2 (#4532, PENDING)
UTS-DD-exclude-lsh.1 (#4531, PENDING)  --> UTS-V3.FINAL re-run gate
UTS-DD-exclude-lsh.2 (#4532, PENDING)  --> UTS-V3.FINAL re-run gate
UTS-DD-SYNTHESIS.a..h (#4576-#4583)    --> this document (UTS-DD-SYNTHESIS.f, #4581)
```

The DAG has two leaves: `UTS-DD-exclude-lsh.1` (#4531) and
`UTS-DD-exclude-lsh.2` (#4532). Both are wire-byte regression-pin
tasks, not code-path changes. Neither blocks any other UTS-DD-\* code
fix; they only block the UTS-V3.FINAL re-run gate documented in the
next section.

### Pending-fix sequencing

1. `UTS-DD-exclude-lsh.1` (#4531) - wire-byte test for `:C` over
   remote shell. Lands first because its fixture is the smaller
   sub-transfer and exercises a single parse path. Radius:
   `crates/filters/tests/wire_compat/`, approximately 80 LoC test-only
   nextest fixture, no production code change.
2. `UTS-DD-exclude-lsh.2` (#4532) - all six `exclude-lsh.test`
   sub-transfer regression pins. Lands second because the larger
   matrix subsumes the .1 fixture and reuses its plumbing. Radius:
   `crates/filters/tests/wire_compat/` and
   `crates/transfer/tests/integration/`, approximately 200-260 LoC
   test-only nextest fixtures across two crates, no production code
   change.

Both tasks are test-only and have no inter-fix ordering constraint
between themselves; the recommended sequence keeps the smaller PR
first to surface fixture-pattern issues before the matrix lands.

## Verification plan

Each completed root cause maps to one or more upstream-testsuite tests
that close once the regression pin lands. The remaining-pending tasks
close the same `exclude-lsh.test` upstream test that
`exclude-shared.1-.3` + `exclude.1-.5` already drive on master:

| Root cause cluster | Closing upstream test | Closing task |
| --- | --- | --- |
| Cluster A (parser) | `exclude.test` | exclude.2/.6/.7/.8 (all DONE) |
| Cluster B (compiled rules) | `exclude.test` | exclude.3/.5 (all DONE) |
| Cluster C (wire format) | `exclude.test` | exclude.4 (DONE) |
| Cluster D (nested dir-merge) | `exclude-lsh.test` | exclude.1 (DONE) |
| exclude-lsh wire-byte coverage | `exclude-lsh.test` | UTS-DD-exclude-lsh.1/.2 (PENDING) |
| Goodbye-flush ordering | `batch-mode.test` | batch-mode.1/.2 (DONE) |
| Encoding (compact flag, sentinel) | `files-from.test`, `symlink-dirlink-basis.test` | sdb.2, files-from.3 (DONE) |
| Walk (per-dir row, dotdir rescan) | `itemize.test`, `files-from.test` | itemize.2, files-from.2 (DONE) |
| Basis resolution | `symlink-dirlink-basis.test` | sdb.1 (DONE) |
| Missing args | `files-from.test` | files-from.1/.3 (DONE) |

## Recommended follow-up tasks

This document does not create tasks. The following follow-up units
were surfaced during the synthesis but do not yet have task entries.
The parent agent should `TaskCreate` whichever it chooses to act on.

| Recommended ID | Title | Rationale |
| --- | --- | --- |
| UTS-DD-FOLLOWUP.1 | `daemon-exit10` XFAIL removal once validator workflow confirms in-tree IPv4 fallback | Known gap 1 above; one-line `tools/ci/upstream_testsuite_known_failures.conf` edit |
| UTS-DD-FOLLOWUP.2 | Differential filter-rule fuzzer against upstream `parse_filter_str`+`add_rule` | Known gap 2; would have caught exclude.4 and exclude.5 by parity before testsuite |
| UTS-DD-FOLLOWUP.3 | Typed `RuleModifierSet` replacing the additive boolean fields on `FilterRule` | Known gap 3; API hygiene deferred during deep dive |
| UTS-DD-FOLLOWUP.4 | Post-#5811 tcpdump regen for `batch-mode` wire evidence | Known gap 4; low priority, regression pin already in place |
| UTS-DD-FOLLOWUP.5 | Two extra `symlink-dirlink-basis` nested-symlink topologies beyond the upstream eight | Known gap 5; not upstream-blocking |
| UTS-DD-FOLLOWUP.6 | `files-from` `/./` anchor + `--protect-args` integration test | Known gap 6; close audit gap |
| UTS-DD-FOLLOWUP.7 | Property test for CVS-mode descendant collision with non-CVS `dir-merge` | Known gap 7; flagged for future coverage |

## UTS-V3.FINAL re-run gate

The UTS-V3.FINAL re-run is the canonical end-of-cycle gate that
exercises every UTS-V3.\* fix family against the upstream testsuite on
the Linux host (`192.168.21.226`). Before UTS-V3.FINAL fires, the
fixes ordered in the **Pending-fix sequencing** section above MUST
land:

1. `UTS-DD-exclude-lsh.1` (#4531) MUST be merged into master.
2. `UTS-DD-exclude-lsh.2` (#4532) MUST be merged into master.

UTS-V3.FINAL invocation is blocked until both pending tasks are
green on master, because the `exclude-lsh.test` cell in the upstream
testsuite needs both wire-byte regression pins to attest that the
exclude shared-cluster A-D fixes ship with regression coverage. Once
the two pending tasks land, UTS-V3.FINAL (parent #4341) can fire
without re-running the eight code-path families covered by this
document.

The gate explicitly does NOT block on UTS-DD-FOLLOWUP.\* items
above. Those are recommended follow-ups; none are upstream-test
blockers.

## Cross-references

- `docs/audits/uts-v4-b-cvs-merge-modifier-gap.md` - UTS-V4.B `:C`
  parser gap audit (shipped via PR #5817, #5842).
- `docs/audits/uts-15-d-batch-mode-tcpdump.md` - UTS-15.d batch-mode
  wire-evidence capture.
- `docs/design/uts-nextest-edge-b-test-harness.md` - UTS-NEXTEST-EDGE
  harness used to port upstream-testsuite shapes into the nextest gate
  (the source of many of the regression-pin PRs above).
- `docs/audits/uts-*` - per-test deep-dive notes for the UTS-1..22
  REOPEN family; the UTS-DD family this document covers is the
  follow-on round.
- `tools/ci/upstream_testsuite_known_failures.conf` - the live
  known-failure list; currently carries only `daemon` from this family
  (#5858).
- Upstream sources referenced throughout:
  `target/interop/upstream-src/rsync-3.4.1/{exclude.c,flist.c,generator.c,backup.c,hlink.c,socket.c}`
  and the 3.4.4 mirror for parse-path semantics where the two diverge.
