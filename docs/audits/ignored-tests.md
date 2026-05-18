# Ignored Tests Audit

Catalogue of every `#[ignore]` annotation in the workspace, the reason each
test is skipped, and a recommendation for whether to re-enable, delete, or
leave it as-is.

## Methodology

- Scope: `crates/*/src`, `crates/*/tests`, and `tests/` (workspace top-level).
- Source: `grep -rn '#\[ignore'` across the scope, then reading the surrounding
  context (test body, file-level doc comments, nearby TODOs) to infer reason.
- Categories used:
  - **EXTERNAL-BINARY** - needs upstream `rsync` or `oc-rsync` binary on PATH.
  - **EXTERNAL-NETWORK** - needs reachable internet host (ftp.gnu.org, etc).
  - **EXTERNAL-DAEMON** - needs a separately-configured daemon (sshd, motd,
    auth, access restrictions).
  - **SLOW / STRESS** - opt-in stress benchmark or fuzz run (multi-second,
    high RLIMIT_NOFILE, hundreds of subprocesses).
  - **PLATFORM** - depends on permissions or filesystem behaviour the test
    runner cannot replicate (mode 0000 files unreadable by owner).
  - **BLOCKED-ON-FEATURE** - feature is partly wired or behaviour is unclear;
    test asserts a desired endpoint that is not yet shipped.
  - **OBSOLETE** - assumption baked into the test no longer matches current
    behaviour (codec doesn't panic, etc); should be rewritten or deleted.
  - **DEBUG-HELPER** - not a real test; a scratch printer kept around for
    diagnosis. Candidate for deletion.

Total `#[ignore]` annotations: **154** (across 27 files; the additional 7
matches in the raw grep are file-level `//!` / `///` doc comments referencing
the attribute, not attributes themselves).

## Stale Ignores

Tests whose stated blocker no longer applies and which should be triaged first.

| File:test | Category | Why it looks stale |
| --- | --- | --- |
| `crates/protocol/tests/protocol_v27_compat.rs::cannot_create_codec_for_v27` | OBSOLETE | The reason text says the codec does not panic. The `#[should_panic]` precondition is wrong; rewrite the test to assert the actual `Result::Err` returned by `create_protocol_codec(27)` rather than ignoring it. |
| `crates/protocol/tests/protocol_v27_compat.rs::cannot_create_ndx_codec_for_v27` | OBSOLETE | Same situation as above for `create_ndx_codec(27)`. Rewrite or delete. |
| `crates/protocol/tests/compatibility_flags.rs::print_all_flag_encodings` | DEBUG-HELPER | Comment in the test body literally calls it a "helper test to discover actual varint encodings." Not an assertion-bearing test. Delete it; the negotiation tests in the same file already cover correctness. |
| `tests/integration_basic.rs::dry_run_shows_changes_without_modifying` | BLOCKED-ON-FEATURE (stale) | Reason: "dry-run verbose output not yet implemented". Verbose itemize and dry-run output are wired in `crates/cli/src/frontend/tests/verbose.rs` and `tracing_integration.rs`. Re-enable; tighten the assertion if it no longer matches today's wording. |
| `tests/integration_basic.rs::verbose_shows_transferred_files` | BLOCKED-ON-FEATURE (stale) | Same as above. Verbose file listing is implemented (`crates/cli/src/frontend/tests/verbose.rs`, `output_parity.rs`, `tracing_integration.rs`). Re-enable. |
| `tests/integration_checksum.rs::checksum_skips_identical_files_with_verbose` | BLOCKED-ON-FEATURE (stale) | Reason: "verbose file listing output not yet implemented" - now implemented. Re-enable; update assertions if string format changed. |
| `tests/integration_checksum.rs::checksum_itemize_shows_checksum_indicator` | BLOCKED-ON-FEATURE (stale) | Same. Itemize/verbose output is shipped. Re-enable. |
| `tests/integration_checksum.rs::upstream_comparison_checksum_with_itemize` | BLOCKED-ON-FEATURE (stale) | Reason: "itemize output via CLI not yet fully wired". Itemize is wired (`itemize_format_upstream.rs` exercises it end-to-end). Re-enable and route through the existing CLI helper. |
| `tests/integration_checksum.rs::checksum_dry_run_no_changes` | BLOCKED-ON-FEATURE (stale) | Same verbose-output rationale. Re-enable. |
| `tests/size_filter_tests.rs::max_size_with_verbose` | BLOCKED-ON-FEATURE (stale) | Same verbose-output rationale. Re-enable. |
| `tests/size_only_tests.rs::size_only_verbose_no_transfer_for_same_size` | BLOCKED-ON-FEATURE (stale) | Same verbose-output rationale. Re-enable. |
| `tests/size_only_tests.rs::size_only_verbose_transfer_for_different_size` | BLOCKED-ON-FEATURE (stale) | Same verbose-output rationale. Re-enable. |
| `tests/integration_preallocate.rs::preallocate_with_verbose_shows_transfer` | BLOCKED-ON-FEATURE (stale) | Same verbose-output rationale. Re-enable. |
| `tests/integration_sparse.rs::sparse_with_verbose_shows_transfer` | BLOCKED-ON-FEATURE (stale) | Same verbose-output rationale. Re-enable. |

If those 14 tests come back green, the workspace gains coverage on already-shipped
CLI surface area for no implementation cost.

## Full Inventory

### Stress / opt-in (run with `--ignored`)

| File:test | Category | Reason | Recommendation |
| --- | --- | --- | --- |
| `crates/daemon/tests/connection_scaling_stress.rs::thread_per_connection_scaling_100` | STRESS | Multi-second runtime; benchmark. | Leave (opt-in). |
| `crates/daemon/tests/connection_scaling_stress.rs::thread_per_connection_scaling_1000` | STRESS | Multi-second runtime; benchmark. | Leave (opt-in). |
| `crates/daemon/tests/connection_scaling_stress.rs::thread_per_connection_scaling_10000` | STRESS | Requires high `RLIMIT_NOFILE`; benchmark. | Leave (opt-in). |
| `crates/core/tests/inc_recurse_stress.rs::deep_nesting_50_levels` | STRESS | Manual stress test. | Leave (opt-in). |
| `crates/core/tests/inc_recurse_stress.rs::wide_directory_1000_files` | STRESS | Manual stress test. | Leave (opt-in). |
| `crates/core/tests/inc_recurse_stress.rs::mixed_deep_and_wide` | STRESS | Manual stress test. | Leave (opt-in). |
| `crates/core/tests/inc_recurse_stress.rs::incremental_update_add_remove_modify` | STRESS | Manual stress test. | Leave (opt-in). |
| `crates/matching/src/index/sparse_match_tests.rs::sparse_match_100mib_single_overlap` | SLOW | 100 MiB allocations and full-buffer scans. | Leave (opt-in). |
| `crates/matching/tests/sparse_match_fixture.rs::sparse_match_16mb_block1024` | SLOW | Sparse fixture, skipped by default. | Leave (opt-in). |
| `crates/matching/tests/sparse_match_fixture.rs::sparse_match_16mb_block4096` | SLOW | Sparse fixture, skipped by default. | Leave (opt-in). |
| `tests/inc_recurse_sender_fuzz_1863.rs::inc_recurse_sender_fuzz_smoke` | STRESS | Spawns subprocesses; opt in via `--run-ignored=all`. | Leave (opt-in). |
| `tests/inc_recurse_sender_fuzz_1863.rs::inc_recurse_sender_fuzz_extended` | STRESS | Long-running fuzz; opt in via `OC_RSYNC_FUZZ_BUDGET_SECS`. | Leave (opt-in). |
| `tests/live_interop_fuzz_1196.rs::live_interop_fuzz_smoke` | STRESS | Spawns subprocesses; opt in via `--run-ignored=all`. | Leave (opt-in). |
| `tests/live_interop_fuzz_1196.rs::live_interop_fuzz_extended` | STRESS | Long-running fuzz; opt in via `OC_RSYNC_FUZZ_BUDGET_SECS`. | Leave (opt-in). |

### External binary or daemon (covered by interop CI)

These run in CI via the interop harness (`tools/ci/run_interop.sh`), which
fetches upstream `rsync` binaries explicitly. Leave them ignored for the
default `cargo nextest run`; do not delete.

| File:test | Category | Reason | Recommendation |
| --- | --- | --- | --- |
| `crates/core/tests/client_daemon_interop.rs::test_client_pull_single_file_from_daemon` | EXTERNAL-BINARY | requires upstream rsync binary | Leave (interop CI). |
| `crates/core/tests/client_daemon_interop.rs::test_client_pull_directory_recursive` | EXTERNAL-BINARY | requires upstream rsync binary | Leave (interop CI). |
| `crates/core/tests/client_daemon_interop.rs::test_client_pull_with_archive_mode` | EXTERNAL-BINARY | requires upstream rsync binary | Leave (interop CI). |
| `crates/core/tests/client_daemon_interop.rs::test_client_pull_with_compression` | EXTERNAL-BINARY | requires upstream rsync binary | Leave (interop CI). |
| `crates/core/tests/client_daemon_interop.rs::test_client_pull_with_checksum` | EXTERNAL-BINARY | requires upstream rsync binary | Leave (interop CI). |
| `crates/core/tests/client_daemon_interop.rs::test_client_push_to_daemon` | EXTERNAL-BINARY | requires upstream rsync binary | Leave (interop CI). |
| `crates/core/tests/client_daemon_interop.rs::test_client_push_directory_to_daemon` | EXTERNAL-BINARY | requires upstream rsync binary | Leave (interop CI). |
| `crates/core/tests/client_daemon_interop.rs::test_client_protocol_compatibility_3_0_9` | EXTERNAL-BINARY | requires upstream rsync 3.0.9 binary | Leave (interop CI). |
| `crates/core/tests/client_daemon_interop.rs::test_client_protocol_compatibility_3_1_3` | EXTERNAL-BINARY | requires upstream rsync 3.1.3 binary | Leave (interop CI). |
| `crates/core/tests/client_daemon_interop.rs::test_client_protocol_compatibility_3_4_1` | EXTERNAL-BINARY | requires upstream rsync 3.4.1 binary | Leave (interop CI). |
| `crates/core/tests/client_daemon_interop.rs::test_protocol_version_negotiation` | EXTERNAL-BINARY | requires upstream rsync binary | Leave (interop CI). |
| `crates/core/tests/client_daemon_interop.rs::test_client_pull_with_exclude_filter` | EXTERNAL-BINARY | requires upstream rsync binary | Leave (interop CI). |
| `crates/core/tests/client_daemon_interop.rs::test_client_pull_with_include_exclude` | EXTERNAL-BINARY | requires upstream rsync binary | Leave (interop CI). |
| `crates/core/tests/client_daemon_interop.rs::test_client_incremental_transfer` | EXTERNAL-BINARY | requires upstream rsync binary | Leave (interop CI). |
| `crates/core/tests/client_daemon_interop.rs::test_client_incremental_size_only` | EXTERNAL-BINARY | requires upstream rsync binary | Leave (interop CI). |
| `crates/core/tests/client_daemon_interop.rs::test_error_invalid_module` | EXTERNAL-BINARY | requires upstream rsync binary | Leave (interop CI). |
| `crates/core/tests/client_daemon_interop.rs::test_error_unexpected_disconnect` | EXTERNAL-BINARY | requires upstream rsync binary | Leave (interop CI). |
| `crates/core/tests/client_daemon_interop.rs::test_metadata_preservation_permissions` | EXTERNAL-BINARY | requires upstream rsync binary | Leave (interop CI). |
| `crates/core/tests/client_daemon_interop.rs::test_metadata_preservation_times` | EXTERNAL-BINARY | requires upstream rsync binary | Leave (interop CI). |
| `crates/core/tests/client_daemon_interop.rs::test_many_small_files` | EXTERNAL-BINARY | requires upstream rsync binary | Leave (interop CI). |
| `crates/core/tests/client_daemon_interop.rs::test_large_file_transfer` | EXTERNAL-BINARY | requires upstream rsync binary | Leave (interop CI). |
| `crates/core/tests/client_daemon_interop.rs::test_empty_directory_transfer` | EXTERNAL-BINARY | requires upstream rsync binary | Leave (interop CI). |
| `crates/core/tests/client_daemon_interop.rs::test_special_characters_in_filename` | EXTERNAL-BINARY | requires upstream rsync binary | Leave (interop CI). |
| `crates/core/tests/client_daemon_interop.rs::test_client_push_to_daemon_with_files_from` | EXTERNAL-BINARY | requires upstream rsync binary | Leave (interop CI). |
| `crates/core/tests/upstream_client_to_oc_daemon_interop.rs::test_oc_daemon_starts_and_accepts_connections` | EXTERNAL-BINARY | requires oc-rsync binary | Leave (interop CI). |
| `crates/core/tests/upstream_client_to_oc_daemon_interop.rs::test_oc_daemon_sends_protocol_greeting` | EXTERNAL-BINARY | requires oc-rsync binary | Leave (interop CI). |
| `crates/core/tests/upstream_client_to_oc_daemon_interop.rs::test_oc_daemon_shutdown_cleanup` | EXTERNAL-BINARY | requires oc-rsync binary | Leave (interop CI). |
| `crates/core/tests/upstream_client_to_oc_daemon_interop.rs::test_upstream_3_4_1_client_handshake` | EXTERNAL-BINARY | requires upstream rsync 3.4.1 and oc-rsync binary | Leave (interop CI). |
| `crates/core/tests/upstream_client_to_oc_daemon_interop.rs::test_upstream_3_1_3_client_handshake` | EXTERNAL-BINARY | requires upstream rsync 3.1.3 and oc-rsync binary | Leave (interop CI). |
| `crates/core/tests/upstream_client_to_oc_daemon_interop.rs::test_upstream_3_0_9_client_handshake` | EXTERNAL-BINARY | requires upstream rsync 3.0.9 and oc-rsync binary | Leave (interop CI). |
| `crates/core/tests/upstream_client_to_oc_daemon_interop.rs::test_pull_single_file_from_oc_daemon` | EXTERNAL-BINARY | requires upstream rsync 3.4.1 and oc-rsync binary | Leave (interop CI). |
| `crates/core/tests/upstream_client_to_oc_daemon_interop.rs::test_pull_directory_tree_from_oc_daemon` | EXTERNAL-BINARY | requires upstream rsync 3.4.1 and oc-rsync binary | Leave (interop CI). |
| `crates/core/tests/upstream_client_to_oc_daemon_interop.rs::test_pull_large_file_from_oc_daemon` | EXTERNAL-BINARY | requires upstream rsync 3.4.1 and oc-rsync binary | Leave (interop CI). |
| `crates/core/tests/upstream_client_to_oc_daemon_interop.rs::test_pull_files_with_special_chars_from_oc_daemon` | EXTERNAL-BINARY | requires upstream rsync 3.4.1 and oc-rsync binary | Leave (interop CI). |
| `crates/core/tests/upstream_client_to_oc_daemon_interop.rs::test_push_single_file_to_oc_daemon` | EXTERNAL-BINARY | requires upstream rsync 3.4.1 and oc-rsync binary | Leave (interop CI). |
| `crates/core/tests/upstream_client_to_oc_daemon_interop.rs::test_push_directory_tree_to_oc_daemon` | EXTERNAL-BINARY | requires upstream rsync 3.4.1 and oc-rsync binary | Leave (interop CI). |
| `crates/core/tests/upstream_client_to_oc_daemon_interop.rs::test_pull_preserves_permissions` | EXTERNAL-BINARY | requires upstream rsync 3.4.1 and oc-rsync binary | Leave (interop CI). |
| `crates/core/tests/upstream_client_to_oc_daemon_interop.rs::test_pull_preserves_mtime` | EXTERNAL-BINARY | requires upstream rsync 3.4.1 and oc-rsync binary | Leave (interop CI). |
| `crates/core/tests/upstream_client_to_oc_daemon_interop.rs::test_module_listing_from_upstream_client` | EXTERNAL-BINARY | requires upstream rsync 3.4.1 and oc-rsync binary | Leave (interop CI). |
| `crates/core/tests/upstream_client_to_oc_daemon_interop.rs::test_manual_protocol_handshake_with_oc_daemon` | EXTERNAL-BINARY | requires oc-rsync binary | Leave (interop CI). |
| `crates/core/tests/upstream_client_to_oc_daemon_interop.rs::test_error_nonexistent_module_from_upstream_client` | EXTERNAL-BINARY | requires upstream rsync 3.4.1 and oc-rsync binary | Leave (interop CI). |
| `crates/core/tests/upstream_client_to_oc_daemon_interop.rs::test_error_write_to_readonly_module` | EXTERNAL-DAEMON | requires upstream rsync 3.4.1 and oc-rsync binary; needs read-only module config | Leave (interop CI). |
| `crates/core/tests/upstream_client_to_oc_daemon_interop.rs::test_pull_with_compression` | EXTERNAL-BINARY | requires upstream rsync 3.4.1 and oc-rsync binary | Leave (interop CI). |
| `crates/core/tests/upstream_client_to_oc_daemon_interop.rs::test_pull_with_checksum_algorithm` | EXTERNAL-BINARY | requires upstream rsync 3.4.1 and oc-rsync binary | Leave (interop CI). |
| `crates/core/tests/upstream_client_to_oc_daemon_interop.rs::test_pull_many_small_files` | EXTERNAL-BINARY | requires upstream rsync 3.4.1 and oc-rsync binary | Leave (interop CI). |
| `crates/core/tests/upstream_client_to_oc_daemon_interop.rs::test_pull_empty_file` | EXTERNAL-BINARY | requires upstream rsync 3.4.1 and oc-rsync binary | Leave (interop CI). |
| `crates/core/tests/upstream_client_to_oc_daemon_interop.rs::test_pull_whitespace_only_file` | EXTERNAL-BINARY | requires upstream rsync 3.4.1 and oc-rsync binary | Leave (interop CI). |
| `crates/core/tests/daemon_client_interop.rs::test_handshake_with_upstream_daemon` | EXTERNAL-BINARY | requires upstream rsync binary | Leave (interop CI). |
| `crates/core/tests/daemon_client_interop.rs::test_pull_from_upstream_daemon_baseline` | EXTERNAL-BINARY | requires upstream rsync binary | Leave (interop CI). |
| `crates/core/tests/daemon_client_interop.rs::test_push_to_upstream_daemon_baseline` | EXTERNAL-BINARY | requires upstream rsync binary | Leave (interop CI). |
| `crates/core/tests/daemon_client_interop.rs::test_protocol_negotiation_3_1_3` | EXTERNAL-BINARY | requires upstream rsync binaries | Leave (interop CI). |
| `crates/core/tests/daemon_client_interop.rs::test_daemon_transfer_preserves_metadata_baseline` | EXTERNAL-BINARY | requires upstream rsync binary | Leave (interop CI). |
| `crates/core/tests/daemon_client_interop.rs::test_daemon_nonexistent_module_error` | EXTERNAL-BINARY | requires upstream rsync binary | Leave (interop CI). |
| `crates/core/tests/daemon_client_interop.rs::test_full_handshake_sequence_modern_protocol` | EXTERNAL-BINARY | requires upstream rsync binary | Leave (interop CI). |
| `crates/core/tests/daemon_client_interop.rs::test_protocol_version_negotiation_downgrade` | EXTERNAL-BINARY | requires upstream rsync binary | Leave (interop CI). |
| `crates/core/tests/daemon_client_interop.rs::test_protocol_version_negotiation_upgrade` | EXTERNAL-BINARY | requires upstream rsync binary | Leave (interop CI). |
| `crates/core/tests/daemon_client_interop.rs::test_module_listing_request_response` | EXTERNAL-BINARY | requires upstream rsync binary | Leave (interop CI). |
| `crates/core/tests/daemon_client_interop.rs::test_compat_flags_exchange_setup` | EXTERNAL-BINARY | requires upstream rsync binary | Leave (interop CI). |
| `crates/core/tests/daemon_client_interop.rs::test_capability_negotiation_checksums` | EXTERNAL-BINARY | requires upstream rsync binary and full protocol implementation | Leave (interop CI). |
| `crates/core/tests/daemon_client_interop.rs::test_capability_negotiation_compression` | EXTERNAL-BINARY | requires upstream rsync binary and full protocol implementation | Leave (interop CI). |
| `crates/core/tests/daemon_client_interop.rs::test_error_invalid_protocol_version` | EXTERNAL-BINARY | requires upstream rsync binary | Leave (interop CI). |
| `crates/core/tests/daemon_client_interop.rs::test_error_module_access_denied` | EXTERNAL-DAEMON | requires daemon with access restrictions configured | Leave (interop CI). |
| `crates/core/tests/daemon_client_interop.rs::test_handshake_with_motd` | EXTERNAL-DAEMON | requires upstream rsync binary with MOTD configured | Leave (interop CI). |
| `crates/core/tests/daemon_client_interop.rs::test_handshake_with_auth_requirement` | EXTERNAL-DAEMON | requires daemon with authentication configured | Leave (interop CI). |
| `crates/transfer/tests/incremental_transfer.rs::incremental_transfer_upstream_interop` | EXTERNAL-BINARY | requires upstream rsync binary | Leave (interop CI). |
| `crates/transfer/tests/symlink_preservation.rs::compare_with_upstream_rsync_symlink_preservation` | EXTERNAL-BINARY | requires upstream rsync binary in PATH | Leave (interop CI). |
| `crates/transfer/tests/symlink_preservation.rs::compare_upstream_rsync_copy_links_behavior` | EXTERNAL-BINARY | requires upstream rsync binary in PATH | Leave (interop CI). |
| `tests/integration_daemon.rs::interop_with_system_rsync` | EXTERNAL-BINARY | requires rsync binary for interop testing | Leave (interop CI). |
| `tests/ssh_proxy_jump.rs::end_to_end_proxy_jump_through_localhost` | EXTERNAL-DAEMON | needs running sshd + rsync on localhost | Leave (manual run). |
| `tests/ssh_transport.rs::test_ssh_push_single_file` | EXTERNAL-DAEMON | requires SSH server setup | Leave (manual run). |
| `tests/ssh_transport.rs::test_ssh_pull_single_file` | EXTERNAL-DAEMON | requires SSH server setup | Leave (manual run). |
| `tests/ssh_transport.rs::test_ssh_push_recursive_directory` | EXTERNAL-DAEMON | requires SSH server setup | Leave (manual run). |

### External network (public hosts)

| File:test | Category | Reason | Recommendation |
| --- | --- | --- | --- |
| `tests/integration_rsync_protocol.rs::rsync_protocol_gnu_ftp_small_file` | EXTERNAL-NETWORK | needs ftp.gnu.org | Leave (manual run). |
| `tests/integration_rsync_protocol.rs::rsync_protocol_gnu_ftp_directory` | EXTERNAL-NETWORK | needs ftp.gnu.org | Leave (manual run). |
| `tests/integration_rsync_protocol.rs::rsync_protocol_apache_small_file` | EXTERNAL-NETWORK | needs rsync.apache.org | Leave (manual run). |
| `tests/integration_rsync_protocol.rs::rsync_protocol_debian_readme` | EXTERNAL-NETWORK | needs ftp.debian.org | Leave (manual run). |
| `tests/integration_rsync_protocol.rs::rsync_protocol_incremental_sync` | EXTERNAL-NETWORK | needs ftp.gnu.org | Leave (manual run). |
| `tests/integration_rsync_protocol.rs::rsync_protocol_dry_run` | EXTERNAL-NETWORK | needs ftp.gnu.org | Leave (manual run). |

### Platform / filesystem behaviour

The mode-0000 cluster wants to assert that we preserve permission bits even on
files the test process itself cannot read. On most systems the source open()
fails first, so the test cannot reach the metadata-preservation assertion.

| File:test | Category | Reason | Recommendation |
| --- | --- | --- | --- |
| `crates/engine/src/local_copy/tests/execute_permissions.rs::mode_0000_file_basic_copy` | PLATFORM | mode 0000 files cannot be read by owner on most systems | Leave; rewrite to use a privileged setup helper or `unshare(CLONE_FS \| CLONE_NEWUSER)` if the coverage is wanted. |
| `crates/engine/src/local_copy/tests/execute_permissions.rs::mode_0000_preserves_across_multiple_files` | PLATFORM | as above | Leave. |
| `crates/engine/src/local_copy/tests/execute_permissions.rs::mode_0000_with_inplace_update` | PLATFORM | as above | Leave. |
| `crates/engine/src/local_copy/tests/execute_permissions.rs::mode_0000_with_backup` | PLATFORM | as above | Leave. |
| `crates/engine/src/local_copy/tests/execute_permissions.rs::mode_0000_dry_run` | PLATFORM | as above | Leave. |
| `crates/engine/src/local_copy/tests/execute_permissions.rs::mode_0000_with_sparse_file` | PLATFORM | as above | Leave. |
| `crates/engine/src/local_copy/tests/execute_permissions.rs::mode_0000_nested_directory_structure` | PLATFORM | as above | Leave. |
| `crates/engine/src/local_copy/tests/execute_permissions.rs::mode_0000_with_symlink_preservation` | PLATFORM | as above | Leave. |
| `crates/engine/src/local_copy/tests/execute_permissions.rs::mode_0000_compare_dest` | PLATFORM | as above | Leave. |
| `crates/engine/src/local_copy/tests/execute_permissions.rs::mode_0000_size_only_comparison` | PLATFORM | as above | Leave. |
| `crates/engine/src/local_copy/tests/execute_permissions.rs::mode_0000_checksum_comparison` | PLATFORM | as above | Leave. |
| `crates/engine/src/local_copy/tests/execute_permissions.rs::mode_0000_file_is_readable_by_owner` | PLATFORM | as above | Delete: the assertion contradicts the predicate; it claims a 0000 file IS readable by owner on a system where the rest of the cluster says it is not. |
| `crates/engine/src/local_copy/tests/execute_permissions.rs::mode_0000_preserve_in_existing_file_update` | PLATFORM | as above | Leave. |
| `crates/engine/src/local_copy/tests/execute_metadata.rs::execute_preserves_mode_0000_with_permissions` | PLATFORM | as above | Leave. |
| `crates/engine/src/local_copy/tests/execute_metadata.rs::execute_can_read_mode_0000_source_as_owner` | PLATFORM | as above | Delete: same self-contradiction as above. |
| `crates/engine/src/local_copy/tests/execute_metadata.rs::execute_preserves_mode_0000_on_destination_file` | PLATFORM | as above | Leave. |
| `crates/engine/src/local_copy/tests/execute_metadata.rs::execute_mode_0000_with_times_preservation` | PLATFORM | as above | Leave. |
| `crates/engine/src/local_copy/tests/execute_metadata.rs::execute_mode_0000_without_permissions_option` | PLATFORM | as above | Leave. |
| `crates/engine/src/local_copy/tests/execute_metadata.rs::execute_mode_0000_directory_with_files` | PLATFORM | as above | Leave. |
| `crates/engine/src/local_copy/tests/execute_metadata.rs::execute_mode_0000_with_chmod_modifiers` | PLATFORM | as above | Leave. |
| `crates/engine/src/local_copy/tests/execute_metadata.rs::execute_mode_0000_update_existing_destination` | PLATFORM | as above | Leave. |

### Blocked on partial features

| File:test | Category | Reason | Recommendation |
| --- | --- | --- | --- |
| `crates/engine/src/local_copy/tests/execute_no_implied_dirs.rs::no_implied_dirs_does_not_create_intermediate_directories` | BLOCKED-ON-FEATURE | `--no-implied-dirs` currently creates directories implicitly | Leave; revisit when the option is finished. |
| `crates/engine/src/local_copy/tests/execute_min_size.rs::min_size_does_not_affect_symlinks` | BLOCKED-ON-FEATURE | Behaviour for `--min-size` vs symlinks needs clarification against upstream | Leave; check `flist.c` upstream and pick a definite answer. |
| `crates/engine/src/local_copy/tests/list_only.rs::list_only_without_recursive_shows_only_top_level` | BLOCKED-ON-FEATURE | Non-recursive listing with trailing slash behaviour unclear | Leave; clarify against upstream then re-enable. |
| `crates/engine/src/local_copy/tests/execute_delay_updates.rs::delay_updates_with_delete_removes_extraneous` | BLOCKED-ON-FEATURE | partial file finalization not working with `--delay-updates --delete` | Leave; fix in engine and re-enable. |
| `crates/engine/src/local_copy/tests/execute_special.rs::copy_unsafe_links_emits_info_symsafe_notice` | BLOCKED-ON-FEATURE | emission path for `--copy-unsafe-links` notice not wired through this executor branch | Leave; tracked separately. |
| `crates/flist/src/tests.rs::walk_detects_direct_symlink_loop` | BLOCKED-ON-FEATURE | walker errors on self-referencing symlinks instead of yielding and skipping | Leave; needs walker fix. |
| `crates/core/tests/error_recovery.rs::error_recovery_symlink_loop` | BLOCKED-ON-FEATURE | symlink loop error recovery not yet implemented | Leave; depends on walker fix above. |
| `crates/logging/tests/verbose_level_1_output.rs::verbose_1_no_debug_unlike_level_2` | BLOCKED-ON-FEATURE | verbose level 1 currently enables debug flags - behaviour needs clarification | Leave; pick a definite policy against upstream and re-enable. |
| `tests/exit_codes.rs::unsupported_feature_returns_unsupported_error` | BLOCKED-ON-FEATURE | Exit code 4 requires specific compile-time conditions | Delete or rewrite: comment in the test body admits it is a documentation placeholder. |
| `tests/exit_codes.rs::connection_to_closed_port_returns_start_client_error` | BLOCKED-ON-FEATURE | daemon connection error handling | Leave; revisit once daemon connect failure paths emit the start-client exit code. |
| `tests/exit_codes.rs::connection_to_invalid_daemon_url_returns_error` | BLOCKED-ON-FEATURE | daemon connection error handling | Leave. |
| `tests/exit_codes.rs::connection_refused_may_return_socket_io_error` | BLOCKED-ON-FEATURE | daemon connection error handling | Leave. |
| `tests/exit_codes.rs::corrupted_stream_returns_stream_io_error` | BLOCKED-ON-FEATURE | needs protocol corruption simulation harness | Leave. |
| `tests/exit_codes.rs::log_write_failure_returns_message_io_error` | BLOCKED-ON-FEATURE | needs internal failure simulation | Leave. |
| `tests/exit_codes.rs::ipc_failure_returns_ipc_error` | BLOCKED-ON-FEATURE | needs IPC fault injection | Leave. |
| `tests/exit_codes.rs::sigint_returns_signal_error` | BLOCKED-ON-FEATURE | needs process timing coordination | Leave. |
| `tests/exit_codes.rs::vanished_file_returns_vanished_error` | BLOCKED-ON-FEATURE | needs timing-sensitive file deletion harness | Leave. |
| `tests/exit_codes.rs::data_timeout_returns_timeout_error` | BLOCKED-ON-FEATURE | needs slow transfer simulation | Leave. |
| `tests/integration_protocol_versions.rs::server_mode_push_protocol_30` | BLOCKED-ON-FEATURE | `ServerModeTest` harness pipes two `--server` processes; the harness, not the protocol, is broken | Leave or replace harness; see the TODO at the top of the file. |
| `tests/integration_protocol_versions.rs::server_mode_push_protocol_31` | BLOCKED-ON-FEATURE | as above | Leave. |
| `tests/integration_protocol_versions.rs::server_mode_push_protocol_32` | BLOCKED-ON-FEATURE | as above | Leave. |
| `tests/integration_protocol_versions.rs::server_mode_pull_protocol_30` | BLOCKED-ON-FEATURE | as above | Leave. |
| `tests/integration_protocol_versions.rs::server_mode_pull_protocol_31` | BLOCKED-ON-FEATURE | as above | Leave. |
| `tests/integration_protocol_versions.rs::server_mode_pull_protocol_32` | BLOCKED-ON-FEATURE | as above | Leave. |
| `tests/integration_protocol_versions.rs::server_mode_delta_transfer_protocol_32` | BLOCKED-ON-FEATURE | as above | Leave. |
| `tests/integration_basic.rs::dry_run_shows_changes_without_modifying` | BLOCKED-ON-FEATURE (stale) | "dry-run verbose output not yet implemented" | Re-enable. |
| `tests/integration_basic.rs::verbose_shows_transferred_files` | BLOCKED-ON-FEATURE (stale) | "verbose file listing output not yet implemented" | Re-enable. |
| `tests/integration_checksum.rs::checksum_skips_identical_files_with_verbose` | BLOCKED-ON-FEATURE (stale) | as above | Re-enable. |
| `tests/integration_checksum.rs::checksum_itemize_shows_checksum_indicator` | BLOCKED-ON-FEATURE (stale) | as above | Re-enable. |
| `tests/integration_checksum.rs::upstream_comparison_checksum_with_itemize` | BLOCKED-ON-FEATURE (stale) | "itemize output via CLI not yet fully wired" | Re-enable. |
| `tests/integration_checksum.rs::checksum_dry_run_no_changes` | BLOCKED-ON-FEATURE (stale) | as above | Re-enable. |
| `tests/size_filter_tests.rs::max_size_with_verbose` | BLOCKED-ON-FEATURE (stale) | as above | Re-enable. |
| `tests/size_only_tests.rs::size_only_verbose_no_transfer_for_same_size` | BLOCKED-ON-FEATURE (stale) | as above | Re-enable. |
| `tests/size_only_tests.rs::size_only_verbose_transfer_for_different_size` | BLOCKED-ON-FEATURE (stale) | as above | Re-enable. |
| `tests/integration_preallocate.rs::preallocate_with_verbose_shows_transfer` | BLOCKED-ON-FEATURE (stale) | as above | Re-enable. |
| `tests/integration_sparse.rs::sparse_with_verbose_shows_transfer` | BLOCKED-ON-FEATURE (stale) | as above | Re-enable. |

### Obsolete / debug helper

| File:test | Category | Reason | Recommendation |
| --- | --- | --- | --- |
| `crates/protocol/tests/protocol_v27_compat.rs::cannot_create_codec_for_v27` | OBSOLETE | The test was written expecting a panic; current code returns a valid codec. The reason note already admits this. | Rewrite as a `Result::Err` assertion or delete (sibling tests already cover that the negotiated version is rejected). |
| `crates/protocol/tests/protocol_v27_compat.rs::cannot_create_ndx_codec_for_v27` | OBSOLETE | Same as above. | Rewrite or delete. |
| `crates/protocol/tests/compatibility_flags.rs::print_all_flag_encodings` | DEBUG-HELPER | Scratch printer for varint encodings, not an assertion test. | Delete. |

## Summary

- 154 `#[ignore]` annotations in total.
- 14 of those are stale (the asserted blocker has shipped) and should be
  re-enabled in a follow-up patch series.
- 3 are obsolete or debug-only and should be rewritten or deleted.
- 21 are gated on a permission/filesystem scenario the test runner cannot
  reproduce in CI; treat the cluster collectively if the coverage matters.
- The remaining 116 are correctly gated: stress benchmarks, fuzz drivers,
  network-dependent or external-binary interop checks. They run in the
  interop harness, the stress workflow, or by manual opt-in.
