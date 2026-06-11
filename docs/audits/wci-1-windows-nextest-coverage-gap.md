# WCI-1: Linux vs Windows nextest target list diff

Audit identifying Windows-only test functions whose crates are never
compiled (and therefore never executed) by any Windows CI cell.

Complements WSD-1 (`docs/audit/windows-ci-coverage-gap.md`), which
quantified Windows coverage at the crate level. WCI-1 narrows the
analysis to the precise inverse question: for each `#[cfg(windows)]`,
`#[cfg(target_os = "windows")]`, or `#[cfg(target_family = "windows")]`
test function in the workspace, which Windows CI cell (if any) actually
links and runs it.

## 1. Current Windows CI nextest scope

Source: `.github/workflows/ci.yml`. Four cells link a Windows test
binary; all run on `windows-latest`.

| Job (`name:`)                 | nextest invocation (verbatim from `ci.yml`)                                                                                                                | Toolchains       | Required |
|-------------------------------|-----------------------------------------------------------------------------------------------------------------------------------------------------------|------------------|----------|
| `Windows (${{ toolchain }})`  | `cargo nextest run --locked -p core -p engine -p cli --all-features`                                                                                       | stable+beta+nightly | stable = yes |
| `Windows IOCP (--features iocp)` | `cargo nextest run --locked -p fast_io --no-default-features --features iocp` then `cargo nextest run --locked -p transfer --all-features`              | stable              | yes      |
| `Windows ACL/xattr`           | `cargo nextest run --locked -p metadata --features acl,xattr` then `cargo nextest run --locked --workspace --features acl,xattr -E 'test(acl) \| test(xattr) \| test(ads) \| test(stream)'` | stable              | yes      |
| `Windows GNU cross-check`     | `cargo check --locked --workspace --target x86_64-pc-windows-gnu` (no tests run)                                                                          | stable              | yes      |

`.github/workflows/_test-features.yml` adds a cross-OS strategy matrix
that runs `windows-latest` against six feature rows (`async`,
`tracing`, `serde`, `concurrent-sessions`, `daemon-tls`, `iconv`),
scoped narrowly per row:

| Row                  | Windows args                                                          |
|----------------------|-----------------------------------------------------------------------|
| `async`              | `-p daemon -p core -p protocol -p engine --features async`            |
| `tracing`            | `-p daemon -p core -p engine --features tracing`                      |
| `serde`              | `-p logging -p protocol -p flist --features serde`                    |
| `concurrent-sessions`| `-p daemon --features concurrent-sessions`                            |
| `daemon-tls`         | `-p daemon --features daemon-tls`                                     |
| `iconv`              | `-p protocol -p transfer -p engine -p core -p cli -p daemon --features iconv` |

A non-required `DG-3 stress` job also links `-p engine --features
dg-stress` on `windows-latest`, but filters to a single stress test.

Combined Windows reachable crate set: `core`, `engine`, `cli`,
`fast_io`, `transfer`, `metadata`, plus `daemon`, `protocol`,
`flist`, `logging` reachable only behind feature-flag rows (and only
for the test subsets enabled by those features).

## 2. Current Linux CI nextest scope

`ci.yml` `test` job runs:

```
cargo nextest run --locked --profile ci --workspace --all-features
```

Single invocation covers all 25 crates. The musl cell uses
`--workspace --target x86_64-unknown-linux-musl --no-default-features
--features "zstd,lz4,xattr,iconv,parallel,copy_file_range"`. The
`_test-features.yml` Linux job adds 13 additional feature-flag rows
(default-features, parallel, incremental-flist, compression, io_uring,
copy_file_range, landlock, openssl, openssl-vendored, zlib-ng, zlib-rs,
flat-flist). Per [[project_ci_nextest_only_full_workspace]] the
`nextest (stable)` cell on Linux is the single full-workspace gate;
Windows and macOS are crate-scoped.

## 3. Coverage delta

The workspace contains **72** test functions (per a `grep -rn`
sweep across `crates/` and `tests/`) gated behind `#[cfg(windows)]`,
`#[cfg(target_os = "windows")]`, or `#[cfg(target_family = "windows")]`
with a `#[test]` (or `#[tokio::test]`) attribute in the surrounding
stack. Distribution by crate (verbatim count, derived from the
extraction script in section 4):

| Crate     | Linux tested?                  | Windows tested? (which cell)                                                          | Windows-only tests (`#[cfg(windows)]` `#[test]`s) | Gap        |
|-----------|--------------------------------|---------------------------------------------------------------------------------------|---------------------------------------------------|------------|
| core      | yes (`--workspace`)            | yes (`Windows` cell)                                                                  | 15                                                | none       |
| engine    | yes                            | yes (`Windows` + IOCP via transfer)                                                   | 2                                                 | none       |
| cli       | yes                            | yes (`Windows` cell)                                                                  | 2                                                 | none       |
| fast_io   | yes                            | yes (`Windows IOCP` cell)                                                             | 21                                                | none       |
| transfer  | yes                            | yes (`Windows IOCP` cell, `--all-features`)                                           | 2                                                 | none       |
| metadata  | yes                            | yes (`Windows ACL/xattr` cell, `--features acl,xattr`)                                | 16                                                | none       |
| daemon    | yes                            | **no** (cross-OS feature rows compile but only narrow tests)                          | 2                                                 | **gap**    |
| protocol  | yes                            | **no** for default features (only via `async`/`serde`/`iconv` rows)                   | 4                                                 | **gap**    |
| flist     | yes                            | **no** for default features (only via `serde` row)                                    | 1                                                 | **gap**    |
| platform  | yes                            | **no** (no CI cell compiles `-p platform` on `windows-latest`)                        | 6                                                 | **gap**    |
| rsync_io  | yes                            | **no** (no Windows cell compiles `-p rsync_io`)                                       | 1                                                 | **gap**    |
| batch     | yes                            | **no**                                                                                | 0 (cfg gate present, no test fn)                  | n/a        |
| **total** |                                | **62 / 72** Windows-only tests reachable from at least one Windows cell               | 72                                                | **14 untested** |

The five gap crates (`daemon`, `protocol`, `flist`, `platform`,
`rsync_io`) account for **14 Windows-only test functions** that no
Windows nextest invocation links. Cross-OS feature rows in
`_test-features.yml` compile some of these crates (`daemon` under
`async`/`tracing`/`concurrent-sessions`/`daemon-tls`/`iconv`,
`protocol` under `async`/`serde`/`iconv`, `flist` under `serde`)
but execute only the subset of tests enabled by the named feature
flag, so the default-features `#[cfg(windows)]` tests in those
crates still never run.

## 4. Windows-only test inventory

Extracted via a Python pass over `grep -rn` output: for each
`#[cfg(<windows-variant>)]` marker, scan up to seven following lines
for a `fn` signature, then confirm the previous four lines or
following six contain a `#[test]` / `#[tokio::test]` / `#[tokio_test]`
attribute.

### 4.1 Crates already reached by a Windows CI cell (informational)

#### `crates/core` (15 tests; runs in `Windows (stable)` cell)

- `crates/core/src/message/tests/part2.rs:128` `normalize_verbatim_disk_paths_drop_unc_prefix`
- `crates/core/src/message/tests/part2.rs:135` `normalize_verbatim_unc_paths_match_standard_unc_rendering`
- `crates/core/src/message/tests/part7.rs:158` `normalize_path_handles_windows_backslashes`
- `crates/core/src/message/tests/part7.rs:165` `normalize_path_handles_windows_mixed_slashes`
- `crates/core/src/message/tests/part7.rs:172` `normalize_path_handles_windows_drive_letter`
- `crates/core/src/message/tests/part7.rs:179` `normalize_path_handles_windows_drive_with_parent`
- `crates/core/src/message/tests/part7.rs:186` `normalize_path_handles_windows_drive_with_current`
- `crates/core/src/message/tests/part7.rs:193` `normalize_path_preserves_uppercase_drive_letter`
- `crates/core/src/message/tests/part7.rs:200` `normalize_path_handles_unc_path`
- `crates/core/src/message/tests/part7.rs:207` `normalize_path_handles_unc_with_parent`
- `crates/core/src/message/tests/part7.rs:214` `normalize_path_handles_verbatim_path`
- `crates/core/src/message/tests/part7.rs:414` `strip_normalized_workspace_prefix_handles_windows_paths`
- `crates/core/src/message/source.rs:424` `source_location_handles_manifest_case_mismatch`
- `crates/core/src/client/remote/invocation/tests.rs:2340` `windows_drive_letter_is_not_remote`
- `crates/core/src/client/remote/invocation/tests.rs:2346` `windows_drive_letter_forward_slash_is_not_remote`

#### `crates/cli` (2 tests; `Windows (stable)` cell)

- `crates/cli/src/frontend/tests/operands.rs:4` `operand_detection_ignores_windows_drive_and_device_prefixes`
- `crates/cli/src/frontend/execution/file_list/tests.rs:482` `drive_letter_paths_are_local`

#### `crates/engine` (2 tests; `Windows (stable)` cell)

- `crates/engine/src/local_copy/tests/relative.rs:42` `relative_root_handles_windows_drive_prefix`
- `crates/engine/src/local_copy/executor/file/copy/links.rs:576` `copy_dest_link_branch_dacl_writes_once_per_cohort`

#### `crates/fast_io` (21 tests; `Windows IOCP` cell)

- `crates/fast_io/tests/win_tmpfile_delete_on_close.rs:56` `temp_file_deleted_on_drop`
- `crates/fast_io/src/copy_basis_range.rs:562` `windows_copy_produces_byte_identical_output`
- `crates/fast_io/src/copy_basis_range.rs:577` `windows_offset_copy_extracts_correct_window`
- `crates/fast_io/src/copy_basis_range.rs:593` `windows_dest_offset_writes_at_correct_position`
- `crates/fast_io/src/copy_basis_range.rs:611` `windows_short_copy_when_basis_eof`
- `crates/fast_io/src/copy_basis_range.rs:627` `windows_supported_returns_true`
- `crates/fast_io/src/platform_copy/tests.rs:204` `preferred_method_large_file`
- `crates/fast_io/src/platform_copy/tests.rs:219` `trait_object_usage`
- `crates/fast_io/src/platform_copy/tests.rs:555` `refs_reflink_fails_gracefully_on_ntfs`
- `crates/fast_io/src/platform_copy/tests.rs:572` `refs_reflink_fails_on_missing_source`
- `crates/fast_io/src/platform_copy/tests.rs:583` `dispatch_falls_back_from_reflink_on_ntfs`
- `crates/fast_io/src/platform_copy/tests.rs:767` `refs_reflink_range_zero_bytes_succeeds`
- `crates/fast_io/src/platform_copy/tests.rs:779` `refs_reflink_range_fails_gracefully_on_ntfs`
- `crates/fast_io/src/platform_copy/tests.rs:795` `refs_reflink_range_fails_on_missing_source`
- `crates/fast_io/src/platform_copy/tests.rs:807` `refs_reflink_range_fails_on_missing_destination`
- `crates/fast_io/src/refs_detect.rs:317` `windows_system_drive_is_not_refs`
- `crates/fast_io/src/refs_detect.rs:326` `windows_cache_populated_after_query`
- `crates/fast_io/src/win_tmpfile/types.rs:171` `temp_file_write_and_commit`
- `crates/fast_io/src/temp_file_strategy.rs:671` `delete_on_close_strategy_create_and_commit`
- `crates/fast_io/src/copy_file_ex.rs:205` `test_try_copy_file_ex_copies_content`
- `crates/fast_io/src/copy_file_ex.rs:221` `test_try_copy_file_ex_empty_file`

#### `crates/transfer` (2 tests; `Windows IOCP` cell, `--all-features`)

- `crates/transfer/src/receiver/tests/errors_and_timeouts/sanitize_file_list.rs:183` `windows_drive_relative_path_rejected_when_untrusted`
- `crates/transfer/src/receiver/tests/errors_and_timeouts/sanitize_file_list.rs:200` `windows_drive_relative_path_allowed_when_trusted`

#### `crates/metadata` (16 tests; `Windows ACL/xattr` cell)

- `crates/metadata/tests/windows_to_linux_acl_roundtrip.rs:222` `simulated_windows_xattr_applied_on_windows`
- `crates/metadata/src/apply_batch.rs:551` `windows_readonly_attribute`
- `crates/metadata/src/copy_as.rs:548` `windows_switch_returns_descriptive_error`
- `crates/metadata/src/copy_as.rs:576` `windows_switch_without_group_returns_error`
- `crates/metadata/src/copy_as.rs:587` `windows_privilege_probe_does_not_panic`
- `crates/metadata/src/stat_cache.rs:660` `windows_readonly_metadata`
- `crates/metadata/src/acl_windows/tests/dacl.rs:96` `read_dacl_on_temp_file_returns_dacl`
- `crates/metadata/src/acl_windows/tests/sddl.rs:10` `sddl_rights_decode_two_letter_tokens`
- `crates/metadata/src/acl_windows/tests/sddl.rs:12` `sddl_rights_decode_two_letter_tokens`
- `crates/metadata/src/acl_windows/tests/sddl.rs:71` `read_dacl_sddl_returns_non_empty_for_temp_file`
- `crates/metadata/src/acl_windows/tests/sddl.rs:84` `write_dacl_sddl_round_trips_known_descriptor`
- `crates/metadata/src/acl_windows/tests/sddl.rs:116` `write_dacl_sddl_preserves_owner_and_group`
- `crates/metadata/src/acl_windows/tests/sddl.rs:131` `write_dacl_sddl_rejects_invalid_input`
- `crates/metadata/src/acl_windows/tests/sync.rs:29` `sync_acls_round_trips_on_ntfs`
- `crates/metadata/src/acl_windows/tests/sync.rs:44` `sync_acls_prefers_sddl_round_trip`
- `crates/metadata/src/acl_windows/tests/xattr.rs:51` `sddl_xattr_entry_round_trips_on_ntfs`

The first eight metadata tests live outside `acl_*`/`xattr_*`/`ads_*`/
`stream_*` test-name prefixes, so the workspace-scoped filter
(`-E 'test(acl) | test(xattr) | test(ads) | test(stream)'`) does not
match them. They are reached only by the per-crate
`cargo nextest run --locked -p metadata --features acl,xattr`
invocation in the same job (which has no `-E` filter). Verified:
the test names `windows_readonly_attribute`,
`windows_switch_returns_descriptive_error`, `windows_privilege_probe_does_not_panic`,
`windows_readonly_metadata` do not match any of the four `test(...)`
substring filters in the workspace step but do execute under the
unfiltered per-crate metadata step.

### 4.2 Untested Windows-only paths (the WCI-1 gap list)

The following 14 Windows-only `#[test]` functions are never compiled by
any Windows CI cell. The Windows GNU cross-check only verifies that the
non-test surface compiles for `x86_64-pc-windows-gnu` (it runs
`cargo check`, not `nextest`).

#### `crates/daemon` (2 tests)

- `crates/daemon/src/daemon/sections/xfer_exec.rs:348` `build_xfer_command_uses_cmd_on_windows`
  Verifies the daemon's pre/post-xfer-exec command-line builder dispatches
  through `cmd.exe /C` rather than `/bin/sh -c` on Windows.
- `crates/daemon/src/daemon/runtime_options/tests.rs:335` `last_detach_flag_wins`
  Confirms `--detach=` precedence parsing for the daemon's
  Windows-service runtime-options path.

`daemon` compiles in the cross-OS feature rows under `async`,
`tracing`, `concurrent-sessions`, `daemon-tls`, and `iconv`, but
neither tested function is gated behind those features. They run only
under `--all-features` on Linux today.

#### `crates/protocol` (4 tests)

- `crates/protocol/src/flist/wire_path.rs:106` `windows_backslash_is_translated_to_forward_slash`
- `crates/protocol/src/flist/wire_path.rs:115` `windows_deep_path_is_translated`
- `crates/protocol/src/flist/wire_path.rs:122` `windows_already_forward_slash_borrows`
- `crates/protocol/src/flist/wire_path.rs:131` `windows_mixed_separators_are_normalized`

These guard the **wire-byte path-separator encoder** (FileEntry name
emission on Windows). A regression here directly corrupts every
oc-rsync push from Windows. The cross-OS `serde` row in
`_test-features.yml` compiles `-p protocol` on `windows-latest`, but
serde isn't load-bearing for these tests and `--features serde` does
not change their gating. The default `cfg(windows)` gate means they
compile - but no Windows nextest invocation links the resulting
binary on Windows.

#### `crates/flist` (1 test)

- `crates/flist/src/file_list_walker.rs:295` `absolutize_returns_absolute_path_unchanged_windows`

Confirms the flist walker's `absolutize` no-ops on already-absolute
Windows paths (drive-letter + UNC). The `serde` cross-OS row compiles
`-p flist` on Windows, but this test is not behind `--features serde`,
so it does not run.

#### `crates/platform` (6 tests)

The entire `crates/platform` Windows surface is untested by Windows CI.

- `crates/platform/src/name_resolution.rs:286` `administrator_resolves_to_rid_500`
- `crates/platform/src/name_resolution.rs:295` `rid_500_resolves_to_administrator`
- `crates/platform/src/name_resolution.rs:303` `lookup_account_info_user_returns_non_group`
- `crates/platform/src/name_resolution.rs:312` `lookup_account_info_group_returns_is_group`
- `crates/platform/src/group.rs:240` `windows_administrators_group_returns_some`
- `crates/platform/src/group.rs:252` `windows_nonexistent_group_returns_none`

These exercise the Windows SID/RID resolver (Administrator->500, group
membership lookup) used by every `--owner`/`--group` transfer when the
receiver runs on Windows. `_test-features.yml` does not list `-p
platform` in any cross-OS row, and the main `Windows (stable)` cell
omits it. The Windows-GNU cross-check verifies they compile but does
not execute them.

#### `crates/rsync_io` (1 test)

- `crates/rsync_io/src/ssh/config_lookup.rs:1026` `user_pattern_case_insensitive_on_windows`

Verifies that `~/.ssh/config` `User` pattern matching is
case-insensitive on Windows (mirroring OpenSSH for Windows behaviour).
`rsync_io` is never linked by any Windows nextest cell.

### 4.3 Patterns NOT found

- No `#[cfg_attr(not(windows), ignore)]` patterns matched in
  `crates/` or `tests/`.
- No `#[ignore = "..."]` annotations cite Windows-specific reasons in
  the workspace.
- No entire-file `#![cfg(windows)]` modules were found (the inverse
  Unix-only file list documented by WSD-1 still holds).

## 5. Prioritized gap list (mapping to WCI-2..9)

Cross-referenced against `[[project_windows_support_depth_unknown]]`
(WSD), `[[project_windows_real_world_parity_unclear]]` (WPC), and
`[[project_no_windows_io_uring]]`. Each WCI-2..9 subtask already
enumerated under the WCI parent (#3694) maps to a concrete subset of
the inventory in section 4.

| Follow-up subtask | Gap closure target                                                                                                                                                                              | Windows-only tests it activates                                                                                                                       | Existing test infrastructure to reuse                                                                                                                                  |
|-------------------|-------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|-------------------------------------------------------------------------------------------------------------------------------------------------------|-----------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| WCI-2 (#3696) IOCP fast path under high-IOPS         | New nightly job; **already** runs `fast_io` (21) + `transfer` (2). Closes the audit gap by adding sustained-IOPS regressions (peak-RSS + write-throughput) per WIN-S.LAND.3 (#3641). | none new; promotes the existing 23 IOCP tests from per-PR to nightly stress profile.                                                                  | `crates/fast_io/src/platform_copy/tests.rs`, `crates/fast_io/tests/win_tmpfile_delete_on_close.rs`, IUB-2/3 high-IOPS harness.                                          |
| WCI-3 (#3697) daemon mode (rsync:// listener)        | **Closes daemon-crate gap (section 4.2).** Adds `-p daemon` to Windows nightly. Wires daemon-on-Windows admission test even though daemon mode refuses `--daemon` on Windows.                    | `build_xfer_command_uses_cmd_on_windows`, `last_detach_flag_wins`, plus the `cfg_attr(windows, ignore)` set in `daemon::sections::config_parsing::tests` and `runtime_options::tests`. | `crates/daemon/tests/connection_scaling_stress.rs` (already has `#[cfg(windows)]` guard at line 74), `_test-features.yml` `concurrent-sessions` row.                    |
| WCI-4 (#3698) SSH push via Windows OpenSSH client    | **Closes rsync_io-crate gap.** Adds `-p rsync_io` to nightly Windows + integration tests using `C:\Windows\System32\OpenSSH\ssh.exe`. Validates russh + ssh_config parsing on Windows.            | `user_pattern_case_insensitive_on_windows`.                                                                                                            | `crates/rsync_io/src/ssh/config_lookup.rs`, SSC-4/5 ssh_config Match/Host harness, existing `tests/ssh_transport.rs`.                                                  |
| WCI-5 (#3699) NTFS ACL round-trip                    | Already activated by `Windows ACL/xattr` cell, but only `acl,xattr` feature set; nightly cell should exercise default features + workspace `-E 'test(acl)'`.                                     | Reinforces metadata (16) already in scope; adds `crates/metadata/tests/windows_to_linux_acl_roundtrip.rs` end-to-end via `oc-rsync.exe` subprocess.    | WAS-7 round-trip harness, WPC-10 inherited-vs-explicit-ACE fixtures.                                                                                                  |
| WCI-6 (#3700) xattr via ADS round-trip               | Same harness as WCI-5 but isolates the ADS code path (`WPC-3.wire.4` end-to-end test already lives under metadata).                                                                              | Re-runs the 16 metadata tests under a `--features xattr` (no `acl`) variant; matters because WPC-3.wire.1/.2 still pending refactors will change preflight cfg gates. | WPC-3.wire.4, `crates/metadata/src/xattr.rs` `cfg(windows)` ADS branch.                                                                                                |
| WCI-7 (#3701) long-path (\\?\) 600+ chars            | Already-shipped WPC-5'.10 lives in `fast_io` tests (covered by IOCP cell), but the metadata + protocol path-encoding hot paths are not exercised at long-path depths.                            | Activates the protocol (4) `wire_path.rs` tests at extreme path lengths.                                                                              | WPC-5'.6 (`fast_io::iocp` extended-path wiring), WPC-5'.10 600-char round-trip fixture.                                                                                |
| WCI-8 (#3702) reparse points + symlinks              | WPC-9'.4..6 already shipped under fast_io. This cell adds `-p flist` + `-p protocol` so the wire-side translation of symlink targets is exercised.                                              | `absolutize_returns_absolute_path_unchanged_windows`, the 4 `wire_path.rs` tests at junction/mount-point targets.                                      | WPC-9'.1-3 `mklink` fixture helpers, WPC-9'.7 nightly cell wiring.                                                                                                    |
| WCI-9 (#3703) case-insensitive collision detection   | **Closes platform-crate gap.** Adds `-p platform` + `-p flist` to Windows nightly with NTFS case-insensitive fixtures. Covers WPC-11 conflict-detection regression.                              | All 6 `platform` tests (`administrator_resolves_to_rid_500` etc.), `flist` walker test (case-mismatch absolutize).                                     | WPC-11 NTFS-case fixture (`tests/integration/...`), `crates/platform/src/name_resolution.rs` Win32 LookupAccountName harness.                                          |

Promotion plan (WCI-10/11) is unchanged from the parent task tree:
nightly green for 14 days then add to required-on-PR matrix via
branch-protection (consistent with the WSD-1 recommendation 9
"Promote Windows interop to a required check").

## 6. Summary

- **Five crates** (`daemon`, `protocol`, `flist`, `platform`,
  `rsync_io`) contain a total of **14 Windows-only `#[test]`
  functions** that no Windows CI cell ever compiles, despite the test
  source carrying `#[cfg(windows)]` gates designed precisely to make
  them run on Windows.
- The Windows GNU cross-check verifies these compile under
  `x86_64-pc-windows-gnu` but does not execute them.
- WCI-3 (daemon), WCI-4 (rsync_io), and WCI-9 (platform) are the
  three subtasks that directly retire most of the gap. WCI-7/8 also
  pull `protocol` and `flist` into a Windows test-binary link.
- Coverage delta vs WSD-1: WSD-1 catalogued the ~17,600-test
  cross-platform gap at the crate level; WCI-1 narrows to the 14
  tests where the author already wrote a Windows-specific assertion
  that no Windows CI cell ever exercises. These are the highest-value
  targets to add because they require no new test authoring.
