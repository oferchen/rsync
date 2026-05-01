# Protocol 28-32 Interop Test Matrix

This document is the operator and contributor reference for what the oc-rsync
interop harness actually exercises. It maps protocol versions to upstream
rsync releases, lists capability differences across protocol 28-32, and
enumerates every scenario in `tools/ci/run_interop.sh` together with the
versions it runs against and the directions tested.

For the higher-level "does this feature work against upstream X" answer, see
[`protocol-compatibility.md`](../protocol-compatibility.md). This matrix is
the lower-level harness inventory it summarises.

**Sources of truth:**

- Harness driver: `tools/ci/run_interop.sh`
- Known-failure conf: `tools/ci/known_failures.conf`
- Known-failure dashboard: `tools/ci/check_known_failures.sh`
- oc-rsync protocol type: `crates/protocol/src/version/protocol_version/mod.rs`
- oc-rsync capability table: `crates/protocol/src/version/protocol_version/capabilities.rs`
- oc-rsync compat flags: `crates/protocol/src/compatibility/known.rs`
- Upstream `PROTOCOL_VERSION` macros: `target/interop/upstream-src/rsync-{3.0.9,3.1.3,3.4.1}/rsync.h`
- Upstream capability dispatch: `target/interop/upstream-src/rsync-3.4.1/compat.c`

---

## 1. Protocol version to upstream version mapping

`PROTOCOL_VERSION` is the value an upstream binary advertises during
negotiation. `MIN_PROTOCOL_VERSION` and `MAX_PROTOCOL_VERSION` define the
window each release will accept on the wire.

| Upstream release | `PROTOCOL_VERSION` | `MIN_PROTOCOL_VERSION` | `MAX_PROTOCOL_VERSION` | Notes |
|------------------|--------------------|------------------------|------------------------|-------|
| rsync 3.0.9      | 30                 | 20                     | 40                     | First release with binary handshake; wire compat down to 28 still useful for legacy peers. |
| rsync 3.1.3      | 31                 | 20                     | 40                     | Adds `CF_SAFE_FLIST` always-on, `--info=progress2`. |
| rsync 3.4.1      | 32                 | 20                     | 40                     | Newest release. Adds checksum negotiation (`-e.LsfxCIvu`), `CF_ID0_NAMES`, `--crtimes`. |

oc-rsync supports the inclusive range protocol 28..32. The supported set is
declared in
`crates/protocol/src/version/protocol_version/mod.rs:104` via
`declare_supported_protocols!(32, 31, 30, 29, 28)` and exposed through
`ProtocolVersion::{V28..V32, OLDEST, NEWEST}`. The negotiation boundary
(`BINARY_NEGOTIATION_INTRODUCED`) is protocol 30; protocols 28-29 use the
legacy ASCII `@RSYNCD:` daemon greeting.

Note on the harness wording: comments inside `tools/ci/known_failures.conf`
refer to "rsync 3.0.9 speaks protocol 28". The advertised binary protocol of
rsync 3.0.9 is in fact 30 (verified above against `rsync.h`); the "28" in
those comments tracks the *minimum* protocol 3.0.9 will accept. The forced
protocol matrix in section 3 is what actually exercises 28 and 29.

---

## 2. Capability differences by protocol version

Capabilities below are sourced from upstream `compat.c` and oc-rsync's
`capabilities.rs`. "Y" means the capability is available; blank means not
present at that protocol version.

### Wire encoding and negotiation

| Capability                           | 28 | 29 | 30 | 31 | 32 | Source |
|--------------------------------------|----|----|----|----|----|--------|
| Legacy ASCII `@RSYNCD:` negotiation  | Y  | Y  |    |    |    | `uses_legacy_ascii_negotiation` |
| Binary negotiation handshake         |    |    | Y  | Y  | Y  | `uses_binary_negotiation`, upstream `compat.c:710` |
| Fixed-size flist flag encoding       | Y  | Y  |    |    |    | `uses_fixed_encoding` |
| Varint flist flag encoding (`CF_VARINT_FLIST_FLAGS`) |    |    | Y  | Y  | Y  | `uses_varint_flist_flags`, upstream `compat.c:117-125,729-732` |
| 2-byte iflags after NDX              |    | Y  | Y  | Y  | Y  | `supports_iflags`, upstream `sender.c:180-187` |
| Extended file flags                  | Y  | Y  | Y  | Y  | Y  | `supports_extended_flags` |

### File list and recursion

| Capability                                    | 28 | 29 | 30 | 31 | 32 | Source |
|-----------------------------------------------|----|----|----|----|----|--------|
| Multi-phase transfer (`max_phase = 2`)        |    | Y  | Y  | Y  | Y  | `supports_multi_phase` |
| File list timing stats                        |    | Y  | Y  | Y  | Y  | `supports_flist_times` |
| Incremental recursion (`CF_INC_RECURSE`)      |    |    | Y  | Y  | Y  | `supports_inc_recurse`, upstream `compat.c:712,720` |
| Inline hardlink dev/ino in flist              |    |    | Y  | Y  | Y  | `supports_inline_hardlinks` |
| Safe file list (`CF_SAFE_FLIST`) negotiable   |    |    | Y  | Y  | Y  | `uses_safe_file_list` |
| Safe file list always on                      |    |    |    | Y  | Y  | `safe_file_list_always_enabled`, upstream `compat.c:775` |
| `CF_ID0_NAMES` (uid/gid 0 names on wire)      |    |    | Y  | Y  | Y  | upstream `compat.c:727,749` |

### Filter rules

| Capability                                                       | 28 | 29 | 30 | 31 | 32 | Source |
|------------------------------------------------------------------|----|----|----|----|----|--------|
| Old-style single-character filter prefixes only (`legal_len=1`)  | Y  |    |    |    |    | `uses_old_prefixes`, upstream `exclude.c:1530,1675` |
| Multi-char prefixes (e.g. `dir-merge`, `:`)                      |    | Y  | Y  | Y  | Y  | upstream `exclude.c:1530` |
| Sender-only / receiver-only modifiers (`s`, `r`)                 |    | Y  | Y  | Y  | Y  | `supports_sender_receiver_modifiers`, upstream `exclude.c:1567-1571` |
| Perishable modifier (`p`)                                        |    |    | Y  | Y  | Y  | `supports_perishable_modifier`, upstream `exclude.c:1350` |

### Multiplex, goodbye, and statistics

| Capability                                   | 28 | 29 | 30 | 31 | 32 | Source |
|----------------------------------------------|----|----|----|----|----|--------|
| Multiplexed I/O (MSG_* frames)               | Y  | Y  | Y  | Y  | Y  | `supports_multiplex_io` (>=23) |
| `NDX_DONE` goodbye exchange                  | Y  | Y  | Y  | Y  | Y  | `supports_goodbye_exchange` (>=24) |
| Generator-to-sender messages                 |    |    | Y  | Y  | Y  | `supports_generator_messages` |
| 3-way extended goodbye / `MSG_IO_TIMEOUT`    |    |    |    | Y  | Y  | `supports_extended_goodbye` |
| Delete stats (`NDX_DEL_STATS`)               |    |    |    | Y  | Y  | `supports_delete_stats` |

### Compression and checksums

| Capability                                              | 28 | 29 | 30 | 31 | 32 | Source |
|---------------------------------------------------------|----|----|----|----|----|--------|
| Hardcoded zlib compression                              | Y  | Y  |    |    |    | upstream `compat.c:383,556-564` |
| Vstring negotiation for codecs (zlibx, zstd, lz4)       |    |    | Y  | Y  | Y  | upstream `compat.c:556-564,729-732` |
| Hardcoded MD4 strong checksum                           | Y  | Y  |    |    |    | upstream `compat.c:414,552,859` |
| MD5 / negotiated checksums (XXH3, XXH128, MD4 fallback) |    |    | Y  | Y  | Y  | `supports_checksum_negotiation`; SSH path needs `-e.LsfxCIvu` |
| `CF_INPLACE_PARTIAL_DIR`                                |    |    | Y  | Y  | Y  | upstream `compat.c:725-726,777` |
| `CF_CHKSUM_SEED_FIX`                                    |    |    | Y  | Y  | Y  | upstream `compat.c:723-724,747` |
| `CF_AVOID_XATTR_OPTIM` (xattr optimisation skip)        |    |    | Y  | Y  | Y  | upstream `compat.c:721-722,746` |
| `--crtimes` (creation-time preservation)                |    |    |    |    | Y  | upstream `compat.c:750-753` (requires varint flist flags) |

### ACLs and xattrs

ACL (`-A`) and xattr (`-X`) wire formats were added at protocol 30. At
protocol < 30 upstream hard-exits with `RERR_PROTOCOL` (`compat.c:655-661`
for `-A`, `compat.c:662-668` for `-X`). See
[Section 6 - Known failures](#6-known-failures-and-limitations).

---

## 3. Standard interop test matrix

`run_interop.sh` runs three concentric matrices:

1. **Native protocol matrix** - one parallel subshell per upstream version
   (3.0.9, 3.1.3, 3.4.1) calling `run_comprehensive_interop_case` with no
   `--protocol` flag. The peer's native protocol number is whatever
   `PROTOCOL_VERSION` it was compiled with.
2. **Forced protocol matrix** - sequential calls to
   `run_comprehensive_interop_case "3.4.1" ... "--protocol=N"` for
   `N in {28,29,30,31,32}`. The 3.4.1 binary is forced down to each
   protocol via `--protocol=N`.
3. **Standalone scenarios** - `run_standalone_interop_tests` invokes the
   per-scenario `test_*` functions against the 3.4.1 binary. See
   [Section 4](#4-extended-and-standalone-scenarios).

Each scenario in matrix 1 and matrix 2 is run **twice** per upstream peer:

- `[upstream X -> oc]` - upstream client, oc-rsync daemon receiver (pull
  semantics, see [Section 5 - Direction semantics](#5-direction-semantics)).
- `[oc -> upstream X]` - oc-rsync client, upstream daemon receiver (push).

Plus, when `ssh` is on `PATH`, one additional `oc-rsync local SSH` transfer
per case.

### Core scenarios (every upstream version)

These scenarios are appended unconditionally to the `scenarios` array in
`run_comprehensive_interop_case` (`tools/ci/run_interop.sh:9072-9088`).

| Scenario      | Flags                       | 3.0.9 | 3.1.3 | 3.4.1 | Native + forced 28-32 |
|---------------|-----------------------------|-------|-------|-------|-----------------------|
| archive       | `-av`                       | Y     | Y     | Y     | Y |
| relative      | `-avR`                      | Y     | Y     | Y     | Y |
| checksum      | `-avc`                      | Y     | Y     | Y     | Y |
| compress      | `-avz`                      | Y     | Y     | Y     | Y |
| whole-file    | `-avW`                      | Y     | Y     | Y     | Y |
| delta         | `-av --no-whole-file -I`    | Y     | Y     | Y     | Y |
| inplace       | `-av --inplace`             | Y     | Y     | Y     | Y |
| numeric-ids   | `-av --numeric-ids`         | Y     | Y     | Y     | Y |
| symlinks      | `-rlptv`                    | Y     | Y     | Y     | Y |
| hardlinks     | `-avH`                      | Y     | Y     | Y     | Y |
| delete        | `-av --delete`              | Y     | Y     | Y     | Y |
| exclude       | `-av --exclude=*.log`       | Y     | Y     | Y     | Y |
| permissions   | `-rlpv`                     | Y     | Y     | Y     | Y |
| itemize       | `-avi`                      | Y     | Y     | Y     | Y |
| acls          | `-avA`                      | Y     | Y     | Y     | Y (KF at proto<=29, see below) |

### Extended scenarios (3.4.1 only)

These are appended only when `version == 3.4.1`
(`run_interop.sh:9094-9140`).

| Scenario              | Flags                                           |
|-----------------------|-------------------------------------------------|
| xattrs                | `-avX`                                          |
| one-file-system       | `-avx`                                          |
| whole-file-replace    | `-avW`                                          |
| delay-updates         | `-av --delay-updates`                           |
| recursive-only        | `-rv`                                           |
| delete-after          | `-av --delete-after`                            |
| delete-during         | `-av --delete-during`                           |
| include-exclude       | `-rv --include=*.txt --include=*/ --exclude=*`  |
| filter-rule           | `-av --exclude=*.tmp`                           |
| merge-filter          | `-av -FF`                                       |
| exclude-from          | `-av --exclude-from=exclude_patterns.txt`       |
| size-only             | `-av --size-only`                               |
| ignore-times          | `-av --ignore-times`                            |
| checksum-skip         | `-avc` (identical files preset)                 |
| checksum-content      | `-avc` (size-equal, content-different preset)   |
| copy-links            | `-avL`                                          |
| safe-links            | `-rlptv --safe-links`                           |
| existing              | `-av --existing`                                |
| backup                | `-av --backup`                                  |
| backup-dir            | `-av --backup --backup-dir=.backups`            |
| link-dest             | `-av --link-dest=link_ref`                      |
| max-delete            | `-av --delete --max-delete=1`                   |
| update                | `-av --update`                                  |
| dry-run               | `-avn`                                          |
| sparse                | `-avS`                                          |
| partial               | `-av --partial`                                 |
| append                | `-av --append`                                  |
| bwlimit               | `-av --bwlimit=10000`                           |
| compress-level-1      | `-avz --compress-level=1`                       |
| compress-level-9      | `-avz --compress-level=9`                       |
| protocol-30           | `-av --protocol=30`                             |
| protocol-31           | `-av --protocol=31` (skipped on 3.0.x peers)    |
| compress-delta        | `-avz --no-whole-file -I`                       |
| devices               | `-avD`                                          |
| compare-dest          | `-av --compare-dest=compare_ref`                |
| files-from            | `-av --files-from=filelist.txt`                 |
| hardlinks-relative    | `-avHR`                                         |
| hardlinks-delete      | `-avH --delete`                                 |
| hardlinks-numeric     | `-avH --numeric-ids`                            |
| hardlinks-checksum    | `-avHc`                                         |
| hardlinks-existing    | `-avH --existing`                               |
| inc-recursive-delete  | `-av --inc-recursive --delete`                  |
| inc-recursive-symlinks| `-rlptv --inc-recursive`                        |
| hardlinks-inc-recursive | `-avH --inc-recursive`                        |
| compress-zstd         | `-avz --compress-choice=zstd` (gated on `--version` advertising zstd) |
| compress-lz4          | `-avz --compress-choice=lz4` (gated on `--version` advertising lz4) |

### Conditional scenario gates

| Scenario        | Gate (in `run_interop.sh`)                                                  |
|-----------------|------------------------------------------------------------------------------|
| protocol-31     | Removed for 3.0.x peers because rsync 3.0.x maxes out at protocol 30.       |
| inc-recursive   | Appended only when forced protocol is empty or >= 30.                       |
| compress-zstd   | Appended only when upstream `rsync --version` reports `zstd`.               |
| compress-lz4    | Appended only when upstream `rsync --version` reports `lz4`.                |
| SSH transfer    | Appended once per case if `command -v ssh` succeeds.                        |

### Forced protocol matrix

The forced-protocol pass is sequential and only against 3.4.1
(`run_interop.sh:9402-9430`). Every scenario above (subject to the gates)
is replayed in both directions with the protocol pinned via `--protocol=N`:

| Forced protocol | Scenarios run                                                              | Notes |
|-----------------|----------------------------------------------------------------------------|-------|
| 28              | Core + 3.4.1 extended, minus `inc-recursive` (proto>=30 gate)              | Exercises legacy ASCII negotiation. |
| 29              | Core + 3.4.1 extended, minus `inc-recursive`                                | Adds multi-char filter prefixes vs proto 28. |
| 30              | Core + 3.4.1 extended, including `inc-recursive`                            | First binary handshake. |
| 31              | Core + 3.4.1 extended, including `inc-recursive`                            | Adds `safe_file_list_always_enabled`, delete stats. |
| 32              | Core + 3.4.1 extended, including `inc-recursive`                            | oc-rsync's primary advertised version. |

Forced-protocol failures are reported via `::warning::` rather than as a
hard CI failure (`run_interop.sh:9421-9425`).

---

## 4. Extended and standalone scenarios

Standalone scenarios live in `run_standalone_interop_tests`
(`run_interop.sh:8697`). They run only against rsync 3.4.1 - in the harness
this is described as "newest available upstream binary" with 3.4.1 as the
preferred choice (`run_interop.sh:9440-9449`). Each entry in `test_names`
is paired with a `test_*` function in `test_funcs`; the loop at
`run_interop.sh:8846` runs them in order and treats a return code of 2 as
"known failure".

| Scenario name                        | Implementation function                  | Versions exercised |
|--------------------------------------|------------------------------------------|--------------------|
| write-batch-read-batch               | `test_write_batch_read_batch`            | 3.4.1 |
| write-batch-read-batch-compressed    | `test_write_batch_read_batch_compressed` | 3.4.1 |
| upstream-compressed-batch-oc-reads   | `test_upstream_compressed_batch_oc_reads`| 3.4.1 |
| oc-compressed-batch-upstream-reads   | `test_oc_compressed_batch_upstream_reads`| 3.4.1 |
| compressed-batch-delta-interop       | `test_compressed_batch_delta_interop`    | 3.4.1 |
| upstream-compressed-batch-self-roundtrip | `test_upstream_compressed_batch_self_roundtrip` | 3.4.1 |
| batch-framing-multifile              | `test_batch_framing_multifile`           | 3.4.1 |
| info-progress2                       | `test_info_progress2`                    | 3.4.1 |
| large-file-2gb                       | `test_large_file_2gb`                    | 3.4.1 |
| file-vanished                        | `test_file_vanished`                     | 3.4.1 |
| copy-unsafe-safe-links               | `test_copy_unsafe_safe_links`            | 3.4.1 |
| pre-post-xfer-exec                   | `test_pre_post_xfer_exec`                | 3.4.1 |
| read-only-module                     | `test_read_only_module`                  | 3.4.1 |
| wrong-password-auth                  | `test_wrong_password_auth`               | 3.4.1 |
| iconv                                | `test_iconv`                             | 3.4.1 |
| hardlinks-comprehensive              | `test_hardlinks_comprehensive`           | 3.4.1 |
| inc-recurse-comprehensive            | `test_inc_recurse_comprehensive`         | 3.4.1 |
| inc-recurse-sender-push              | `test_inc_recurse_sender_push`           | 3.4.1 |
| unicode-names                        | `test_unicode_names`                     | 3.4.1 |
| special-chars                        | `test_special_chars`                     | 3.4.1 |
| empty-dir                            | `test_empty_dir`                         | 3.4.1 |
| delete-after                         | `test_delete_after`                      | 3.4.1 |
| hardlinks                            | `test_hardlinks`                         | 3.4.1 |
| many-files                           | `test_many_files`                        | 3.4.1 |
| sparse                               | `test_sparse`                            | 3.4.1 |
| whole-file                           | `test_whole_file`                        | 3.4.1 |
| dry-run                              | `test_dry_run`                           | 3.4.1 |
| filter-rules                         | `test_filter_rules`                      | 3.4.1 |
| up:no-change                         | `test_no_change_upstream`                | 3.4.1 |
| oc:no-change                         | `test_no_change_oc`                      | 3.4.1 |
| inplace                              | `test_inplace`                           | 3.4.1 |
| append                               | `test_append`                            | 3.4.1 |
| delay-updates                        | `test_delay_updates`                     | 3.4.1 |
| compress-level                       | `test_compress_level`                    | 3.4.1 |
| zstd-negotiation                     | `test_zstd_negotiation`                  | 3.4.1 |
| files-from                           | `test_files_from`                        | 3.4.1 |
| trust-sender                         | `test_trust_sender`                      | 3.4.1 |
| partial-dir                          | `test_partial_dir`                       | 3.4.1 |
| deep-nesting                         | `test_deep_nesting`                      | 3.4.1 |
| modify-window                        | `test_modify_window`                     | 3.4.1 |
| delete-excluded                      | `test_delete_excluded`                   | 3.4.1 |
| permissions-only                     | `test_permissions_only`                  | 3.4.1 |
| timestamps-only                      | `test_timestamps_only`                   | 3.4.1 |
| max-connections                      | `test_max_connections`                   | 3.4.1 |
| exclude-include-precedence           | `test_exclude_include_precedence`        | 3.4.1 |
| delete-with-filters                  | `test_delete_with_filters`               | 3.4.1 |
| delete-filter-protect                | `test_delete_filter_protect`             | 3.4.1 |
| delete-filter-risk                   | `test_delete_filter_risk`                | 3.4.1 |
| ff-filter-shortcut                   | `test_ff_filter_shortcut`                | 3.4.1 |
| acl-xattr-graceful-degradation-309   | `test_acl_xattr_graceful_degradation_309`| 3.4.1 *and* 3.0.9 (cross-version graceful degradation) |
| log-format-daemon                    | `test_log_format_daemon`                 | 3.4.1 |
| up:symlinks                          | `test_symlinks_upstream`                 | 3.4.1 |
| oc:symlinks                          | `test_symlinks_oc`                       | 3.4.1 |
| daemon-server-side-filter            | `test_daemon_server_side_filter`         | 3.4.1 |
| daemon-filter-exclude-glob           | `test_daemon_filter_exclude_glob`        | 3.4.1 |
| daemon-filter-exclude-anchored       | `test_daemon_filter_exclude_anchored`    | 3.4.1 |
| daemon-filter-include-exclude-star   | `test_daemon_filter_include_exclude_star`| 3.4.1 |
| daemon-filter-directive-types        | `test_daemon_filter_directive_types`     | 3.4.1 |
| daemon-filter-overlapping-rules      | `test_daemon_filter_overlapping_rules`   | 3.4.1 |
| daemon-filter-from-files             | `test_daemon_filter_from_files`          | 3.4.1 |
| daemon-filter-include-from-files     | `test_daemon_filter_include_from_files`  | 3.4.1 |
| daemon-filter-push-direction         | `test_daemon_filter_push_direction`      | 3.4.1 |
| delta-stats                          | `test_delta_stats`                       | 3.4.1 |
| daemon-filter-doublestar             | `test_daemon_filter_doublestar`          | 3.4.1 |
| daemon-filter-charclass              | `test_daemon_filter_charclass`           | 3.4.1 |
| daemon-filter-question-mark          | `test_daemon_filter_question_mark`       | 3.4.1 |
| link-dest                            | `test_link_dest`                         | 3.4.1 |
| copy-dest                            | `test_copy_dest`                         | 3.4.1 |
| numeric-ids-standalone               | `test_numeric_ids_standalone`            | 3.4.1 |

### `acl-xattr-graceful-degradation-309`

This is the only standalone scenario that intentionally cross-versions to
3.0.9. The harness drives upstream rsync 3.0.9 (which lacks compiled-in
ACL/xattr support) in both directions to verify oc-rsync degrades gracefully
(`run_interop.sh:6561-6669`).

---

## 5. Direction semantics

The `direction:name` keys in `tools/ci/known_failures.conf` and the
`[upstream X -> oc]` / `[oc -> upstream X]` log lines in `run_interop.sh`
use a fixed legend.

| Direction key   | Client       | Server / receiver | Wire URL form                                  | Use case |
|-----------------|--------------|-------------------|------------------------------------------------|----------|
| `up`            | upstream rsync | oc-rsync daemon  | `rsync://127.0.0.1:${oc_port}/interop`         | Operator runs upstream rsync and pushes into an oc-rsync daemon. |
| `oc`            | oc-rsync     | upstream rsync daemon | `rsync://127.0.0.1:${upstream_port}/interop` | Operator runs oc-rsync and pushes into an upstream daemon. |
| `standalone`    | varies       | varies            | local paths or single-binary scenarios         | Batch interop, daemon hooks, auth, iconv, large-file. |

Note on "push" vs "pull": rsync's daemon protocol is symmetric in direction
but asymmetric in role. In every harness scenario the source tree is the
fixture (`comp_src`) and the destination is a per-tag temp directory, so
the *transfer direction* is always source -> daemon. The `up`/`oc` prefix
identifies which implementation hosts the *client process* (the side that
opens the TCP connection and selects the module). For SSH transfers the
`oc:ssh-transfer` key is used; only oc-rsync runs as the SSH client in the
harness.

For interactive use:

- **oc-rsync pulling from upstream daemon**: `oc-rsync -av rsync://host/mod/ dest/` is also covered, because the daemon protocol negotiation is identical regardless of which side reads vs writes the actual file bytes. The `comp_run_scenario` helper always uses source -> daemon; pulls are exercised indirectly via the `up:` direction (upstream client reading from oc-rsync daemon's module path).
- **SSH push and pull**: covered by `run_ssh_interop_test` with oc-rsync as the SSH client; gated on `command -v ssh`.

---

## 6. Known failures and limitations

The harness consults `is_known_failure_from_conf`
(`tools/ci/known_failures.conf:50`) to decide whether a failure is a hard
CI break or a tracked upstream limitation. The dashboard
(`tools/ci/check_known_failures.sh`) re-runs each tracked failure
individually and reports `FIXED` / `FAILING` / `SKIPPED`.

### Unconditional known failures

| Key                                                | Method     | Description |
|----------------------------------------------------|------------|-------------|
| `standalone:delta-stats`                           | standalone | oc-rsync delta engine does not engage in daemon mode; sends all data as literals (matched=0). Fixable only by reworking the daemon code path through the delta engine. |
| `standalone:upstream-compressed-batch-self-roundtrip` | standalone | Upstream rsync 3.4.1 cannot read its own compressed delta batch files (`token.c:608` inflate -3, `compat.c:194-195`). oc-rsync reads them correctly. |

### Protocol <= 29 known failures (`up` direction only)

These are upstream rsync limitations - upstream itself fails identically
when the same protocol is forced. Source: `known_failures.conf:65-93`.

| Key                | Reason |
|--------------------|--------|
| `up:acls`          | `compat.c:655-661` hard-exits with `RERR_PROTOCOL`; ACL wire format added in proto 30. |
| `up:xattrs`        | `compat.c:662-668` hard-exits with `RERR_PROTOCOL`; xattr wire format added in proto 30. |
| `up:compress-zstd` | `compat.c:556-564,729-742` - `v` compat flag (vstring negotiation) requires proto >= 30. Falls back to zlib at proto < 30. |
| `up:compress-lz4`  | Same as `up:compress-zstd`. |

### Protocol <= 28 known failures (`up` direction only)

| Key                | Reason |
|--------------------|--------|
| `up:merge-filter`  | `exclude.c:1530` sets `legal_len=1` at proto < 29; the `dir-merge` (`:`) prefix needs `legal_len >= 2`. |

### How to interpret the dashboard

`check_known_failures.sh` runs each `DASHBOARD_ENTRIES` row exactly once
and emits a markdown summary. A `FIXED` row indicates a tracked failure
that is now passing - the appropriate action is to remove that row from
`tools/ci/known_failures.conf`. A `FAILING` row means the documented
upstream limitation is still reproducible. `SKIPPED` rows occur when the
relevant upstream binary is not available in the environment.

---

## 7. Cross-references

- [`docs/protocol-compatibility.md`](../protocol-compatibility.md) - higher-level pass/fail summary that draws from the same harness.
- [`docs/filter-coverage-matrix.md`](../filter-coverage-matrix.md) - filter rule coverage; this matrix's `daemon-filter-*` standalone scenarios feed its "Daemon Filter Test" column.
- [`docs/daemon/filter-precedence.md`](../daemon/filter-precedence.md) - daemon filter evaluation order tested by the `daemon-filter-*` standalone scenarios.
- [`docs/INTEROP.md`](../INTEROP.md) - operator runbook for executing the harness locally.
- [`docs/interop_testing.md`](../interop_testing.md) - additional interop test infrastructure notes.
- [`docs/PROTOCOL.md`](../PROTOCOL.md) - wire format reference.
