use super::*;
use core::version::VersionInfoReport;
use std::borrow::Cow;
use std::ffi::{OsStr, OsString};
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::num::{NonZeroU32, NonZeroU64};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::{Duration, Instant};
use tempfile::{NamedTempFile, tempdir};

// Import daemon internal types and functions needed by tests
// These are all included directly into the daemon module via include!()
use crate::daemon::{
    AddressFamily,
    // Constants from daemon.rs
    BRANDED_CONFIG_ENV,
    ConnectionLimiter,
    FEATURE_UNAVAILABLE_EXIT_CODE,
    HostPattern,
    LEGACY_CONFIG_ENV,
    ModuleConnectionError,
    // From module_state.rs
    ModuleDefinition,
    ModuleRuntime,
    // From sections/cli_args.rs
    ProgramName,
    // From runtime_options.rs
    RuntimeOptions,
    TestSecretsEnvOverride,
    advertised_capability_lines,
    // From sections/module_access.rs
    apply_module_bandwidth_limit,
    clap_command,
    clear_test_hostname_overrides,
    default_secrets_path_if_present,
    // From sections/config_paths.rs
    first_existing_config_path,
    format_bandwidth_rate,
    // From sections/server_runtime.rs
    format_connection_status,
    // From sections/delegation.rs
    legacy_daemon_greeting,
    log_module_bandwidth_change,
    module_peer_hostname,
    open_log_sink,
    // From sections/config_helpers.rs
    parse_auth_user_list,
    parse_boolean_directive,
    // From sections/config_parsing.rs
    parse_config_modules,
    parse_max_connections_directive,
    parse_numeric_identifier,
    parse_refuse_option_list,
    parse_timeout_seconds,
    read_trimmed_line,
    render_help,
    sanitize_module_identifier,
    set_test_hostname_override,
};

// Import from core
use core::{
    bandwidth::{BandwidthLimiter, LimiterChange},
    branding::{self, Brand},
    fallback::{CLIENT_FALLBACK_ENV, DAEMON_FALLBACK_ENV},
};

// Import from protocol
use protocol::ProtocolVersion;

// Import from checksums
use checksums::strong::Md5;

// Import from base64
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD_NO_PAD;

mod support;
use support::*;

include!("tests/chunks/advertised_capability_lines_empty_without_modules.rs");
include!("tests/chunks/advertised_capability_lines_include_authlist_when_required.rs");
include!("tests/chunks/advertised_capability_lines_report_modules_without_auth.rs");
// Disabled per native-only execution requirement (CLAUDE.md):
// include!("tests/chunks/binary_session_delegates_to_configured_fallback.rs");
// include!("tests/chunks/binary_session_delegation_propagates_runtime_arguments.rs");
include!("tests/chunks/builder_allows_brand_override.rs");
include!("tests/chunks/builder_collects_arguments.rs");
include!("tests/chunks/clap_parse_error_is_reported_via_message.rs");
// Disabled per native-only execution requirement (CLAUDE.md):
// include!("tests/chunks/configured_fallback_binary_defaults_to_rsync.rs");
// include!("tests/chunks/configured_fallback_binary_respects_primary_disable.rs");
// include!("tests/chunks/configured_fallback_binary_respects_secondary_disable.rs");
// include!("tests/chunks/configured_fallback_binary_supports_auto_value.rs");
include!("tests/chunks/connection_limiter_enforces_limits_across_guards.rs");
include!("tests/chunks/connection_limiter_open_preserves_existing_counts.rs");
include!("tests/chunks/connection_limiter_propagates_io_errors.rs");
include!("tests/chunks/connection_status_messages_describe_active_sessions.rs");
include!("tests/chunks/default_config_candidates_prefer_legacy_for_upstream_brand.rs");
include!("tests/chunks/default_config_candidates_prefer_oc_branding.rs");
include!("tests/chunks/default_secrets_path_falls_back_to_secondary_candidate.rs");
include!("tests/chunks/default_secrets_path_prefers_primary_candidate.rs");
include!("tests/chunks/default_secrets_path_returns_none_when_absent.rs");
// Disabled per native-only execution requirement (CLAUDE.md):
// include!("tests/chunks/delegate_system_daemon_fallback_env_triggers_delegation.rs");
// include!("tests/chunks/delegate_system_rsync_disable_env_blocks_delegation.rs");
// include!("tests/chunks/delegate_system_rsync_env_false_skips_fallback.rs");
// include!("tests/chunks/delegate_system_rsync_env_triggers_fallback.rs");
// include!("tests/chunks/delegate_system_rsync_fallback_env_triggers_delegation.rs");
// include!("tests/chunks/delegate_system_rsync_falls_back_to_client_override.rs");
// include!("tests/chunks/delegate_system_rsync_invokes_fallback_binary.rs");
// include!("tests/chunks/delegate_system_rsync_propagates_exit_code.rs");
// include!("tests/chunks/delegate_system_rsync_reports_missing_binary_diagnostic.rs");
include!("tests/chunks/first_existing_config_path_falls_back_to_legacy_candidate.rs");
include!("tests/chunks/first_existing_config_path_prefers_primary_candidate.rs");
include!("tests/chunks/first_existing_config_path_returns_none_when_absent.rs");
include!("tests/chunks/format_bandwidth_rate_prefers_largest_whole_unit.rs");
include!("tests/chunks/help_flag_renders_static_help_snapshot.rs");
include!("tests/chunks/log_module_bandwidth_change_ignores_unchanged.rs");
include!("tests/chunks/log_module_bandwidth_change_logs_disable.rs");
include!("tests/chunks/log_module_bandwidth_change_logs_updates.rs");
include!("tests/chunks/module_bwlimit_burst_does_not_raise_daemon_cap.rs");
include!("tests/chunks/module_bwlimit_can_lower_daemon_cap.rs");
include!("tests/chunks/module_bwlimit_cannot_raise_daemon_cap.rs");
include!(
    "tests/chunks/module_bwlimit_configured_unlimited_with_burst_override_clears_daemon_cap.rs"
);
include!(
    "tests/chunks/module_bwlimit_configured_unlimited_without_specified_flag_clears_daemon_cap.rs"
);
include!("tests/chunks/module_bwlimit_configures_unlimited_daemon.rs");
include!("tests/chunks/module_bwlimit_unlimited_clears_daemon_cap.rs");
include!("tests/chunks/module_bwlimit_unlimited_is_noop_when_no_cap.rs");
include!("tests/chunks/module_bwlimit_unlimited_with_burst_override_clears_daemon_cap.rs");
include!("tests/chunks/module_bwlimit_unlimited_with_explicit_burst_preserves_daemon_cap.rs");
include!("tests/chunks/module_bwlimit_updates_burst_without_lowering_limit.rs");
include!("tests/chunks/module_bwlimit_zero_burst_clears_existing_burst.rs");
include!("tests/chunks/module_definition_hostname_allow_matches_exact.rs");
include!("tests/chunks/module_definition_hostname_deny_takes_precedence.rs");
include!("tests/chunks/module_definition_hostname_suffix_matches.rs");
include!("tests/chunks/module_definition_hostname_wildcard_collapses_consecutive_asterisks.rs");
include!("tests/chunks/module_definition_hostname_wildcard_handles_multiple_asterisks.rs");
include!("tests/chunks/module_definition_hostname_wildcard_matches.rs");
include!("tests/chunks/module_definition_hostname_wildcard_treats_question_as_single_character.rs");
include!("tests/chunks/module_peer_hostname_missing_resolution_denies_hostname_only_rules.rs");
include!("tests/chunks/module_peer_hostname_skips_lookup_when_disabled.rs");
include!("tests/chunks/module_peer_hostname_uses_override.rs");
include!("tests/chunks/module_without_bwlimit_inherits_daemon_cap.rs");
include!("tests/chunks/module_without_bwlimit_preserves_daemon_cap.rs");
include!("tests/chunks/oc_help_flag_renders_branded_snapshot.rs");
include!("tests/chunks/oc_version_flag_renders_report.rs");
include!("tests/chunks/parse_auth_user_list_trims_and_deduplicates_case_insensitively.rs");
include!("tests/chunks/parse_boolean_directive_interprets_common_forms.rs");
include!("tests/chunks/parse_config_modules_detects_recursive_include.rs");
include!("tests/chunks/parse_max_connections_directive_handles_zero_and_positive.rs");
include!("tests/chunks/parse_numeric_identifier_rejects_blank_or_invalid_input.rs");
include!("tests/chunks/parse_refuse_option_list_normalises_and_deduplicates.rs");
include!("tests/chunks/parse_timeout_seconds_supports_zero_and_non_zero_values.rs");
include!("tests/chunks/read_trimmed_line_strips_crlf_terminators.rs");
include!("tests/chunks/run_daemon_accepts_valid_credentials.rs");
include!("tests/chunks/run_daemon_denies_module_when_host_not_allowed.rs");
include!("tests/chunks/run_daemon_enforces_bwlimit_during_module_list.rs");
include!("tests/chunks/run_daemon_enforces_module_connection_limit.rs");
include!("tests/chunks/run_daemon_filters_modules_during_list_request.rs");
include!("tests/chunks/run_daemon_handles_binary_negotiation.rs");
// Disabled per native-only execution requirement (CLAUDE.md):
// include!("tests/chunks/binary_session_delegates_inline_module_config.rs");
include!("tests/chunks/run_daemon_handles_parallel_sessions.rs");
include!("tests/chunks/run_daemon_honours_max_sessions.rs");
include!("tests/chunks/run_daemon_lists_modules_on_request.rs");
include!("tests/chunks/run_daemon_lists_modules_with_motd_lines.rs");
include!("tests/chunks/run_daemon_omits_unlisted_modules_from_listing.rs");
include!("tests/chunks/run_daemon_records_log_file_entries.rs");
include!("tests/chunks/run_daemon_refuses_disallowed_module_options.rs");
include!("tests/chunks/run_daemon_rejects_duplicate_session_limits.rs");
include!("tests/chunks/run_daemon_rejects_invalid_max_sessions.rs");
include!("tests/chunks/run_daemon_rejects_invalid_port.rs");
include!("tests/chunks/run_daemon_rejects_unknown_argument.rs");
include!("tests/chunks/run_daemon_requests_authentication_for_protected_module.rs");
include!("tests/chunks/run_daemon_serves_single_legacy_connection.rs");
include!("tests/chunks/run_daemon_writes_and_removes_pid_file.rs");
include!("tests/chunks/runtime_options_allows_relative_path_when_use_chroot_disabled.rs");
include!("tests/chunks/runtime_options_allows_timeout_zero_in_config.rs");
include!("tests/chunks/runtime_options_apply_global_refuse_options.rs");
include!("tests/chunks/runtime_options_bind_accepts_bracketed_ipv6.rs");
include!("tests/chunks/runtime_options_bind_resolves_hostnames.rs");
include!("tests/chunks/runtime_options_branded_config_env_overrides_legacy_env.rs");
include!("tests/chunks/runtime_options_branded_secrets_env_overrides_legacy_env.rs");
include!("tests/chunks/runtime_options_cli_config_overrides_environment_variable.rs");
include!("tests/chunks/runtime_options_cli_modules_inherit_global_refuse_options.rs");
include!("tests/chunks/runtime_options_config_lock_file_respects_cli_override.rs");
include!("tests/chunks/runtime_options_config_pid_file_respects_cli_override.rs");
include!("tests/chunks/runtime_options_default_enables_reverse_lookup.rs");
include!("tests/chunks/runtime_options_default_secrets_path_updates_delegate_arguments.rs");
include!("tests/chunks/runtime_options_global_bwlimit_respects_cli_override.rs");
include!("tests/chunks/runtime_options_inherits_global_secrets_file_from_config.rs");
include!("tests/chunks/runtime_options_inline_module_uses_default_secrets_file.rs");
include!("tests/chunks/runtime_options_inline_module_uses_global_secrets_file.rs");
include!("tests/chunks/runtime_options_ipv4_rejects_ipv6_bind_address.rs");
include!("tests/chunks/runtime_options_ipv6_accepts_ipv6_bind_address.rs");
include!("tests/chunks/runtime_options_ipv6_rejects_ipv4_bind_address.rs");
include!("tests/chunks/runtime_options_ipv6_sets_default_bind_address.rs");
include!("tests/chunks/runtime_options_load_modules_from_config_file.rs");
include!("tests/chunks/runtime_options_loads_boolean_and_id_directives_from_config.rs");
include!("tests/chunks/runtime_options_loads_bwlimit_burst_from_config.rs");
include!("tests/chunks/runtime_options_loads_bwlimit_from_config.rs");
include!("tests/chunks/runtime_options_loads_config_from_branded_environment_variable.rs");
include!("tests/chunks/runtime_options_loads_config_from_legacy_environment_variable.rs");
include!("tests/chunks/runtime_options_loads_global_bwlimit_from_config.rs");
include!("tests/chunks/runtime_options_loads_global_chmod_from_config.rs");
include!("tests/chunks/runtime_options_loads_lock_file_from_config.rs");
include!("tests/chunks/runtime_options_loads_max_connections_from_config.rs");
include!("tests/chunks/runtime_options_loads_modules_from_included_config.rs");
include!("tests/chunks/runtime_options_loads_motd_from_config_directives.rs");
include!("tests/chunks/runtime_options_loads_pid_file_from_config.rs");
include!("tests/chunks/runtime_options_loads_refuse_options_from_config.rs");
include!("tests/chunks/runtime_options_loads_reverse_lookup_from_config.rs");
include!("tests/chunks/runtime_options_loads_secrets_from_branded_environment_variable.rs");
include!("tests/chunks/runtime_options_loads_secrets_from_legacy_environment_variable.rs");
include!("tests/chunks/runtime_options_loads_timeout_from_config.rs");
include!("tests/chunks/runtime_options_loads_unlimited_global_bwlimit_from_config.rs");
include!("tests/chunks/runtime_options_loads_unlimited_max_connections_from_config.rs");
include!("tests/chunks/runtime_options_loads_use_chroot_directive_from_config.rs");
include!("tests/chunks/runtime_options_module_overrides_chmod_directives.rs");
include!("tests/chunks/runtime_options_module_definition_parses_inline_bwlimit_burst.rs");
include!("tests/chunks/runtime_options_module_definition_parses_inline_options.rs");
include!("tests/chunks/runtime_options_module_definition_preserves_escaped_backslash.rs");
include!("tests/chunks/runtime_options_module_definition_rejects_duplicate_inline_option.rs");
include!("tests/chunks/runtime_options_module_definition_rejects_unknown_inline_option.rs");
include!(
    "tests/chunks/runtime_options_module_definition_requires_secrets_for_inline_auth_users.rs"
);
include!("tests/chunks/runtime_options_module_definition_supports_escaped_commas.rs");
include!("tests/chunks/runtime_options_inline_module_inherits_chmod.rs");
include!("tests/chunks/runtime_options_inline_module_overrides_chmod.rs");
include!("tests/chunks/runtime_options_parse_auth_users_and_secrets_file.rs");
include!("tests/chunks/runtime_options_parse_bwlimit_argument.rs");
include!("tests/chunks/runtime_options_parse_bwlimit_argument_with_burst.rs");
include!("tests/chunks/runtime_options_parse_bwlimit_unlimited.rs");
include!("tests/chunks/runtime_options_parse_bwlimit_unlimited_ignores_burst.rs");
include!("tests/chunks/runtime_options_parse_hostname_patterns.rs");
include!("tests/chunks/runtime_options_parse_hosts_allow_and_deny.rs");
include!("tests/chunks/runtime_options_parse_lock_file_argument.rs");
include!("tests/chunks/runtime_options_parse_log_file_argument.rs");
include!("tests/chunks/runtime_options_parse_module_definitions.rs");
include!("tests/chunks/runtime_options_parse_motd_sources.rs");
include!("tests/chunks/runtime_options_parse_no_bwlimit_argument.rs");
include!("tests/chunks/runtime_options_parse_pid_file_argument.rs");
include!("tests/chunks/runtime_options_reject_duplicate_bwlimit.rs");
include!("tests/chunks/runtime_options_reject_duplicate_lock_file_argument.rs");
include!("tests/chunks/runtime_options_reject_duplicate_log_file_argument.rs");
include!("tests/chunks/runtime_options_reject_duplicate_pid_file_argument.rs");
include!("tests/chunks/runtime_options_reject_invalid_bwlimit.rs");
include!("tests/chunks/runtime_options_reject_whitespace_wrapped_bwlimit_argument.rs");
include!("tests/chunks/runtime_options_rejects_config_missing_path.rs");
include!("tests/chunks/runtime_options_rejects_duplicate_bwlimit_in_config.rs");
include!("tests/chunks/runtime_options_rejects_duplicate_global_bwlimit.rs");
include!("tests/chunks/runtime_options_rejects_duplicate_global_chmod.rs");
include!("tests/chunks/runtime_options_rejects_duplicate_global_refuse_options.rs");
include!("tests/chunks/runtime_options_rejects_duplicate_module_across_config_and_cli.rs");
include!("tests/chunks/runtime_options_rejects_duplicate_refuse_options_directives.rs");
include!("tests/chunks/runtime_options_rejects_duplicate_reverse_lookup_directive.rs");
include!("tests/chunks/runtime_options_rejects_duplicate_use_chroot_directive.rs");
include!("tests/chunks/runtime_options_rejects_empty_refuse_options_directive.rs");
include!("tests/chunks/runtime_options_rejects_invalid_boolean_directive.rs");
include!("tests/chunks/runtime_options_rejects_invalid_bwlimit_in_config.rs");
include!("tests/chunks/runtime_options_rejects_invalid_list_directive.rs");
include!("tests/chunks/runtime_options_rejects_invalid_max_connections.rs");
include!("tests/chunks/runtime_options_rejects_invalid_timeout.rs");
include!("tests/chunks/runtime_options_rejects_invalid_uid.rs");
include!("tests/chunks/runtime_options_rejects_ipv4_ipv6_combo.rs");
include!("tests/chunks/runtime_options_rejects_missing_secrets_from_environment.rs");
include!("tests/chunks/runtime_options_rejects_relative_path_with_chroot_enabled.rs");
include!("tests/chunks/runtime_options_rejects_world_readable_secrets_file.rs");
include!("tests/chunks/runtime_options_require_secrets_file_with_auth_users.rs");
include!("tests/chunks/sanitize_module_identifier_preserves_clean_input.rs");
include!("tests/chunks/sanitize_module_identifier_replaces_control_characters.rs");
include!("tests/chunks/version_flag_renders_report.rs");
