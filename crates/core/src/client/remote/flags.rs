//! Shared flag builder functions for remote transfer modules.
//!
//! Contains logic for building server flag strings and converting client filter
//! rules to wire format, shared between SSH and daemon transfer orchestration.
//! The flag ordering and encoding mirror upstream `options.c:server_options()`.
//!
//! # Upstream Reference
//!
//! - `options.c:server_options()` - Compact flag string generation
//! - `options.c:parse_arguments()` - Server-side flag parsing
//! - `exclude.c:send_rules()` - Filter rule wire format

use protocol::filters::{FilterRuleWireFormat, RuleType};

use super::super::config::{ClientConfig, DeleteMode, FilterRuleKind, FilterRuleSpec};
use super::super::error::ClientError;
use crate::server::ServerConfig;

/// Builds the compact server flag string from client configuration.
///
/// Constructs a single-character flag string (e.g., `-logDtpr`) encoding the
/// transfer options negotiated between client and server. The flag order matches
/// upstream `server_options()`.
pub(crate) fn build_server_flag_string(config: &ClientConfig) -> String {
    let mut flags = String::from("-");

    // upstream: options.c:2169-2173 - --files-from disables recursion and
    // enables xfer_dirs. options.c:2188 - --files-from implies --relative.
    let files_from_active = config.files_from().is_active();
    let effective_recursive = config.recursive() && !files_from_active;
    let effective_relative = config.relative_paths() || files_from_active;

    if config.links() {
        flags.push('l');
    }
    if config.preserve_owner() {
        flags.push('o');
    }
    if config.preserve_group() {
        flags.push('g');
    }
    if config.preserve_devices() || config.preserve_specials() {
        flags.push('D');
    }
    if config.preserve_times() {
        flags.push('t');
    }
    if config.preserve_atimes() {
        flags.push('U');
    }
    if config.preserve_permissions() {
        flags.push('p');
    }
    if effective_recursive {
        flags.push('r');
    }
    // upstream: options.c:2704 - 'z' is only sent when the compression
    // algorithm is the default (no explicit --compress-choice). For
    // explicitly chosen algorithms (lz4, zstd), the 'z' flag is omitted
    // and --compress-choice=ALGO is sent as a long-form arg instead.
    if config.compress()
        && config.compression_algorithm()
            == compress::algorithm::CompressionAlgorithm::default_algorithm()
    {
        flags.push('z');
    }
    if config.checksum() {
        flags.push('c');
    }
    if config.preserve_hard_links() {
        flags.push('H');
    }
    if config.ignore_times() {
        flags.push('I');
    }
    #[cfg(all(any(unix, windows), feature = "acl"))]
    if config.preserve_acls() {
        flags.push('A');
    }
    #[cfg(all(unix, feature = "xattr"))]
    if config.preserve_xattrs() {
        flags.push('X');
    }
    // upstream: 'n' = dry_run (!do_xfers), NOT numeric_ids.
    // numeric_ids is always sent as long-form --numeric-ids (options.c:2887-2888).
    if config.dry_run() {
        flags.push('n');
    }
    // upstream: 'd' = --dirs (xfer_dirs without recursion), NOT delete.
    // delete variants are always sent as long-form --delete-* (options.c:2818-2827).
    // upstream: options.c:2620 - xfer_dirs=1 when files_from is active.
    let effective_dirs = config.dirs() || files_from_active;
    if effective_dirs && !effective_recursive {
        flags.push('d');
    }
    // upstream: options.c:2622 - whole_file, but not when --append is active
    // (append requires delta transfer to append only new data)
    if config.whole_file() && !config.append() {
        flags.push('W');
    }
    if config.sparse() {
        flags.push('S');
    }
    for _ in 0..config.one_file_system_level() {
        flags.push('x');
    }
    if effective_relative {
        flags.push('R');
    }
    if config.partial() {
        flags.push('P');
    }
    if config.update() {
        flags.push('u');
    }
    if config.preserve_crtimes() {
        flags.push('N');
    }
    // upstream: options.c:764 - 'L' = copy_links (resolve symlinks).
    if config.copy_links() {
        flags.push('L');
    }

    // upstream: options.c:2750-2762 - itemize-changes is forwarded via
    // --log-format=%i in the long-form args, not as a compact flag.

    flags
}

/// Converts client filter rules to wire format.
///
/// Maps [`FilterRuleSpec`] (client-side representation) to [`FilterRuleWireFormat`]
/// (protocol wire representation) for transmission to the remote server.
///
/// The `FilterRuleWireFormat::pattern` field stores the bare pattern with the
/// leading `/` (anchored) and trailing `/` (directory-only) stripped. Those
/// modifiers are encoded as separate flags and re-emitted by the wire
/// serializer; leaving them in the pattern produces a double `/` on the wire
/// (e.g. `*//` for `--include='*/'`) which upstream rsync misinterprets.
///
/// upstream: `exclude.c:1238-1240` - leading `/` is parsed as the
/// `FILTRULE_ABS_PATH` modifier. `exclude.c:get_rule_prefix()` (and
/// `serialize_rule` here) re-append the trailing `/` for directory-only rules.
pub(crate) fn build_wire_format_rules(
    client_rules: &[FilterRuleSpec],
) -> Result<Vec<FilterRuleWireFormat>, ClientError> {
    let mut wire_rules = Vec::new();

    for spec in client_rules {
        let rule_type = match spec.kind() {
            FilterRuleKind::Include => RuleType::Include,
            FilterRuleKind::Exclude => RuleType::Exclude,
            FilterRuleKind::Clear => RuleType::Clear,
            FilterRuleKind::Protect => RuleType::Protect,
            FilterRuleKind::Risk => RuleType::Risk,
            FilterRuleKind::DirMerge => RuleType::DirMerge,
            FilterRuleKind::ExcludeIfPresent => {
                // upstream: ExcludeIfPresent is transmitted as Exclude with 'e' flag
                // (FILTRULE_EXCLUDE_SELF).
                let (pattern, anchored, directory_only) = split_pattern_modifiers(spec.pattern());
                wire_rules.push(FilterRuleWireFormat {
                    rule_type: RuleType::Exclude,
                    pattern,
                    anchored,
                    directory_only,
                    no_inherit: false,
                    cvs_exclude: false,
                    word_split: false,
                    // upstream: 'e' flag = FILTRULE_EXCLUDE_SELF.
                    exclude_from_merge: true,
                    xattr_only: spec.is_xattr_only(),
                    sender_side: spec.applies_to_sender(),
                    receiver_side: spec.applies_to_receiver(),
                    perishable: spec.is_perishable(),
                    negate: spec.is_negated(),
                });
                continue;
            }
        };

        let (pattern, anchored, directory_only) = split_pattern_modifiers(spec.pattern());
        let mut wire_rule = FilterRuleWireFormat {
            rule_type,
            pattern,
            anchored,
            directory_only,
            no_inherit: false,
            cvs_exclude: false,
            word_split: false,
            exclude_from_merge: false,
            xattr_only: spec.is_xattr_only(),
            sender_side: spec.applies_to_sender(),
            receiver_side: spec.applies_to_receiver(),
            perishable: spec.is_perishable(),
            negate: spec.is_negated(),
        };

        if let Some(options) = spec.dir_merge_options() {
            wire_rule.no_inherit = !options.inherit_rules();
            wire_rule.word_split = options.uses_whitespace();
            wire_rule.exclude_from_merge = options.excludes_self();
        }

        wire_rules.push(wire_rule);
    }

    Ok(wire_rules)
}

/// Separates anchor (`/`) and directory (`/`) modifiers from the pattern body.
///
/// Returns `(pattern, anchored, directory_only)` where `pattern` excludes both
/// the leading and trailing `/` when present. Bare `/` (which represents both
/// modifiers on the root) is preserved as the pattern so it round-trips through
/// the wire correctly.
fn split_pattern_modifiers(raw: &str) -> (String, bool, bool) {
    if raw == "/" {
        return (raw.to_owned(), true, false);
    }
    let anchored = raw.starts_with('/');
    let directory_only = raw.len() > 1 && raw.ends_with('/');
    let start = usize::from(anchored);
    let end = raw.len() - usize::from(directory_only);
    (raw[start..end].to_owned(), anchored, directory_only)
}

/// Applies common server flags from client configuration to a server config.
///
/// Sets the fields that are shared across both SSH and daemon transfer paths
/// for both receiver and generator roles: `trust_sender`, `qsort`, `inplace`,
/// `min_file_size`, `max_file_size`, `do_stats`, `late_delete`, and `itemize`.
pub(crate) fn apply_common_server_flags(config: &ClientConfig, server_config: &mut ServerConfig) {
    server_config.trust_sender = config.trust_sender();
    server_config.qsort = config.qsort();
    server_config.write.inplace = config.inplace();
    server_config.has_partial_dir = config.partial_directory().is_some();
    server_config.partial_dir = config.partial_directory().map(std::path::Path::to_path_buf);
    server_config.file_selection.min_file_size = config.min_file_size();
    server_config.file_selection.max_file_size = config.max_file_size();
    // upstream: options.c:2046-2048 - do_stats sets INFO_STATS to level 2+
    server_config.do_stats = config.stats();
    // upstream: generator.c:124 - EARLY_DELETE_DONE_MSG = !(delete_during==2 || delete_after)
    server_config.deletion.late_delete =
        matches!(config.delete_mode(), DeleteMode::Delay | DeleteMode::After);
    // upstream: options.c:2881-2885 - copy_unsafe_links and safe_links are long-form only
    server_config.flags.copy_unsafe_links = config.copy_unsafe_links();
    server_config.flags.safe_links = config.safe_links();
    // upstream: syscall.c do_open / do_open_nofollow propagate O_NOATIME when set.
    server_config.write.open_noatime = config.open_noatime();
    // upstream: options.c:2750-2762 - itemize_changes is forwarded to the remote
    // as --log-format=%i, but the local ServerConfig also needs the flag set so
    // the generator's maybe_emit_itemize() produces client-side output via callback.
    server_config.flags.info_flags.itemize = config.itemize_changes();
    // upstream: flist.c::iconv_for_local and options.c::recv_iconv_settings -
    // when --iconv is configured, the local process must transcode file-list
    // entries between the local and remote charsets. Without this bridge the
    // CLI parses --iconv, validates it, and forwards it to the remote peer
    // over SSH/daemon, but the in-process file-list reader and writer never
    // see a converter and silently pass raw bytes through.
    server_config.connection.iconv = config.iconv().resolve_converter();
    // upstream: flist.c:send_file_list() - missing_args controls ENOENT handling
    // for top-level source paths and --files-from entries.
    server_config.file_selection.ignore_missing_args = config.ignore_missing_args();
    server_config.file_selection.delete_missing_args = config.delete_missing_args();
    // upstream: options.c:89 do_compression_threads, token.c:701 ZSTD_c_nbWorkers
    server_config.connection.compression_threads = config.compression_threads();
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::filters::RuleType;

    #[test]
    fn server_flag_string_includes_recursive() {
        let config = ClientConfig::builder().recursive(true).build();
        let flags = build_server_flag_string(&config);
        assert!(flags.contains('r'), "expected 'r' in flags: {flags}");
    }

    #[test]
    fn server_flag_string_includes_preservation_flags() {
        let config = ClientConfig::builder()
            .times(true)
            .permissions(true)
            .owner(true)
            .group(true)
            .build();

        let flags = build_server_flag_string(&config);
        assert!(flags.contains('t'), "expected 't' in flags: {flags}");
        assert!(flags.contains('p'), "expected 'p' in flags: {flags}");
        assert!(flags.contains('o'), "expected 'o' in flags: {flags}");
        assert!(flags.contains('g'), "expected 'g' in flags: {flags}");
    }

    #[test]
    fn server_flag_string_includes_z_for_default_algorithm() {
        // upstream: options.c:2704 - 'z' is sent for the default compression
        // algorithm (no explicit --compress-choice).
        let config = ClientConfig::builder()
            .compress(true)
            .compression_algorithm(compress::algorithm::CompressionAlgorithm::default_algorithm())
            .build();
        let flags = build_server_flag_string(&config);
        assert!(
            flags.contains('z'),
            "expected 'z' for default algorithm: {flags}"
        );
    }

    #[cfg(feature = "lz4")]
    #[test]
    fn server_flag_string_omits_z_for_lz4() {
        // upstream: options.c:2704 - 'z' NOT sent for non-default algorithms.
        // LZ4 uses --compress-choice=lz4 as a long-form arg instead.
        let config = ClientConfig::builder()
            .compress(true)
            .compression_algorithm(compress::algorithm::CompressionAlgorithm::Lz4)
            .build();
        let flags = build_server_flag_string(&config);
        assert!(!flags.contains('z'), "should not send 'z' for lz4: {flags}");
    }

    #[test]
    fn server_flag_string_omits_itemize_compact_flag() {
        // upstream: options.c:2750-2762 - itemize is sent via --log-format=%i,
        // not as a compact flag character in the flag string.
        let config = ClientConfig::builder().itemize_changes(true).build();
        let flags = build_server_flag_string(&config);
        assert!(
            !flags.contains(".i"),
            "itemize should not appear as compact flag: {flags}"
        );
    }

    #[test]
    fn apply_common_server_flags_sets_itemize() {
        let config = ClientConfig::builder().itemize_changes(true).build();
        let mut server_config = ServerConfig::default();
        apply_common_server_flags(&config, &mut server_config);
        assert!(server_config.flags.info_flags.itemize);
    }

    #[test]
    fn apply_common_server_flags_itemize_default_false() {
        let config = ClientConfig::default();
        let mut server_config = ServerConfig::default();
        apply_common_server_flags(&config, &mut server_config);
        assert!(!server_config.flags.info_flags.itemize);
    }

    #[test]
    fn apply_common_server_flags_propagates_ignore_missing_args() {
        let config = ClientConfig::builder().ignore_missing_args(true).build();
        let mut server_config = ServerConfig::default();
        apply_common_server_flags(&config, &mut server_config);
        assert!(server_config.file_selection.ignore_missing_args);
    }

    #[test]
    fn apply_common_server_flags_propagates_delete_missing_args() {
        let config = ClientConfig::builder().delete_missing_args(true).build();
        let mut server_config = ServerConfig::default();
        apply_common_server_flags(&config, &mut server_config);
        assert!(server_config.file_selection.delete_missing_args);
    }

    #[test]
    fn apply_common_server_flags_missing_args_default_false() {
        let config = ClientConfig::default();
        let mut server_config = ServerConfig::default();
        apply_common_server_flags(&config, &mut server_config);
        assert!(!server_config.file_selection.ignore_missing_args);
        assert!(!server_config.file_selection.delete_missing_args);
    }

    #[test]
    fn apply_common_server_flags_propagates_compression_threads() {
        let threads = std::num::NonZeroU8::new(4).unwrap();
        let config = ClientConfig::builder()
            .compression_threads(Some(threads))
            .build();
        let mut server_config = ServerConfig::default();
        apply_common_server_flags(&config, &mut server_config);
        assert_eq!(server_config.connection.compression_threads, Some(threads));
    }

    #[test]
    fn apply_common_server_flags_compression_threads_default_none() {
        let config = ClientConfig::default();
        let mut server_config = ServerConfig::default();
        apply_common_server_flags(&config, &mut server_config);
        assert_eq!(server_config.connection.compression_threads, None);
    }

    #[test]
    fn converts_empty_filter_list() {
        let rules = build_wire_format_rules(&[]).expect("should convert empty list");
        assert_eq!(rules.len(), 0);
    }

    #[test]
    fn converts_simple_exclude_rule() {
        let spec = FilterRuleSpec::exclude("*.log");
        let rules = build_wire_format_rules(&[spec]).expect("should convert exclude rule");

        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].rule_type, RuleType::Exclude);
        assert_eq!(rules[0].pattern, "*.log");
        assert!(!rules[0].anchored);
        assert!(!rules[0].directory_only);
    }

    #[test]
    fn converts_simple_include_rule() {
        let spec = FilterRuleSpec::include("*.txt");
        let rules = build_wire_format_rules(&[spec]).expect("should convert include rule");

        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].rule_type, RuleType::Include);
        assert_eq!(rules[0].pattern, "*.txt");
        assert!(!rules[0].anchored);
        assert!(!rules[0].directory_only);
    }

    #[test]
    fn detects_anchored_pattern() {
        let spec = FilterRuleSpec::exclude("/tmp");
        let rules = build_wire_format_rules(&[spec]).expect("should convert anchored rule");

        assert_eq!(rules.len(), 1);
        assert!(rules[0].anchored);
        // The leading `/` becomes the `anchored` flag and is stripped from
        // the pattern body so that the wire serializer does not emit it
        // twice (once as the prefix modifier and once as a literal).
        assert_eq!(rules[0].pattern, "tmp");
    }

    #[test]
    fn detects_directory_only_pattern() {
        let spec = FilterRuleSpec::exclude("cache/");
        let rules = build_wire_format_rules(&[spec]).expect("should convert directory-only rule");

        assert_eq!(rules.len(), 1);
        assert!(rules[0].directory_only);
        // Trailing `/` becomes the `directory_only` flag; the pattern body
        // is the bare name so that serialization appends a single slash on
        // the wire (otherwise upstream sees `cache//` and misparses it).
        assert_eq!(rules[0].pattern, "cache");
    }

    #[test]
    fn directory_only_wildcard_round_trips_without_double_slash() {
        // upstream: `--include='*/'` produces wire bytes `+ */` with one
        // trailing slash. Storing the slash both in the pattern and via
        // the `directory_only` flag would double-encode it on the wire and
        // cause upstream rsync to treat the rule as `*/` instead of `*`,
        // matching only at depth 1 and breaking deeper directory traversal
        // when combined with a trailing `--exclude='*'`.
        let spec = FilterRuleSpec::include("*/");
        let rules = build_wire_format_rules(&[spec]).expect("should convert dir wildcard");

        assert_eq!(rules.len(), 1);
        assert!(rules[0].directory_only);
        assert_eq!(rules[0].pattern, "*");
    }

    #[test]
    fn anchored_directory_only_preserves_both_flags() {
        let spec = FilterRuleSpec::exclude("/build/");
        let rules =
            build_wire_format_rules(&[spec]).expect("should convert anchored directory rule");

        assert_eq!(rules.len(), 1);
        assert!(rules[0].anchored);
        assert!(rules[0].directory_only);
        assert_eq!(rules[0].pattern, "build");
    }

    #[test]
    fn bare_root_slash_is_preserved() {
        // `/` is the rare degenerate case where the slash is both anchored
        // and (formally) directory-only. Keep the slash in the pattern so
        // the wire is not empty after stripping.
        let spec = FilterRuleSpec::exclude("/");
        let rules = build_wire_format_rules(&[spec]).expect("should convert bare slash rule");

        assert_eq!(rules.len(), 1);
        assert!(rules[0].anchored);
        assert!(!rules[0].directory_only);
        assert_eq!(rules[0].pattern, "/");
    }

    #[test]
    fn preserves_sender_receiver_flags() {
        let spec = FilterRuleSpec::exclude("*.tmp")
            .with_sender(true)
            .with_receiver(false);
        let rules = build_wire_format_rules(&[spec]).expect("should convert side flags");

        assert_eq!(rules.len(), 1);
        assert!(rules[0].sender_side);
        assert!(!rules[0].receiver_side);
    }

    #[test]
    fn preserves_perishable_flag() {
        let spec = FilterRuleSpec::exclude("*.swp").with_perishable(true);
        let rules = build_wire_format_rules(&[spec]).expect("should convert perishable flag");

        assert_eq!(rules.len(), 1);
        assert!(rules[0].perishable);
    }

    #[test]
    fn preserves_xattr_only_flag() {
        let spec = FilterRuleSpec::exclude("user.*").with_xattr_only(true);
        let rules = build_wire_format_rules(&[spec]).expect("should convert xattr_only flag");

        assert_eq!(rules.len(), 1);
        assert!(rules[0].xattr_only);
    }

    #[test]
    fn converts_all_rule_types() {
        use engine::local_copy::DirMergeOptions;

        let specs = vec![
            FilterRuleSpec::include("*.txt"),
            FilterRuleSpec::exclude("*.log"),
            FilterRuleSpec::clear(),
            FilterRuleSpec::protect("important"),
            FilterRuleSpec::risk("temp"),
            FilterRuleSpec::dir_merge(".rsync-filter", DirMergeOptions::new()),
        ];

        let rules = build_wire_format_rules(&specs).expect("should convert all rule types");

        assert_eq!(rules.len(), 6);
        assert_eq!(rules[0].rule_type, RuleType::Include);
        assert_eq!(rules[1].rule_type, RuleType::Exclude);
        assert_eq!(rules[2].rule_type, RuleType::Clear);
        assert_eq!(rules[3].rule_type, RuleType::Protect);
        assert_eq!(rules[4].rule_type, RuleType::Risk);
        assert_eq!(rules[5].rule_type, RuleType::DirMerge);
    }

    #[test]
    fn transmits_exclude_if_present_rules() {
        let specs = vec![
            FilterRuleSpec::exclude("*.log"),
            FilterRuleSpec::exclude_if_present(".git"),
            FilterRuleSpec::include("*.txt"),
        ];

        let rules = build_wire_format_rules(&specs).expect("should transmit ExcludeIfPresent");

        assert_eq!(rules.len(), 3);
        assert_eq!(rules[0].rule_type, RuleType::Exclude);
        assert_eq!(rules[0].pattern, "*.log");
        assert!(!rules[0].exclude_from_merge);

        assert_eq!(rules[1].rule_type, RuleType::Exclude);
        assert_eq!(rules[1].pattern, ".git");
        assert!(rules[1].exclude_from_merge);

        assert_eq!(rules[2].rule_type, RuleType::Include);
        assert_eq!(rules[2].pattern, "*.txt");
    }

    #[test]
    fn handles_dir_merge_options() {
        use engine::local_copy::DirMergeOptions;

        let options = DirMergeOptions::new()
            .inherit(false)
            .exclude_filter_file(true)
            .use_whitespace();

        let spec = FilterRuleSpec::dir_merge(".rsync-filter", options);
        let rules = build_wire_format_rules(&[spec]).expect("should convert dir_merge options");

        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].rule_type, RuleType::DirMerge);
        assert!(rules[0].no_inherit);
        assert!(rules[0].exclude_from_merge);
        assert!(rules[0].word_split);
    }

    #[test]
    fn server_flag_string_files_from_suppresses_r_adds_d_and_r_upper() {
        // upstream: options.c:2169-2188 - --files-from sets recurse=0,
        // xfer_dirs=1, relative_paths=1.
        use crate::client::config::FilesFromSource;

        let config = ClientConfig::builder()
            .recursive(true)
            .times(true)
            .files_from(FilesFromSource::LocalFile("/tmp/list.txt".into()))
            .build();
        let flags = build_server_flag_string(&config);
        assert!(
            !flags.contains('r'),
            "should suppress 'r' with --files-from: {flags}"
        );
        assert!(
            flags.contains('d'),
            "should add 'd' (xfer_dirs) with --files-from: {flags}"
        );
        assert!(
            flags.contains('R'),
            "should add 'R' (relative) with --files-from: {flags}"
        );
    }

    #[test]
    fn server_flag_string_no_files_from_keeps_r() {
        let config = ClientConfig::builder().recursive(true).build();
        let flags = build_server_flag_string(&config);
        assert!(
            flags.contains('r'),
            "should keep 'r' without files-from: {flags}"
        );
        assert!(
            !flags.contains('d'),
            "should not add 'd' without files-from: {flags}"
        );
    }
}
