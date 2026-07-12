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
use crate::client::DirMergeEnforcedKind;
use crate::server::ServerConfig;

/// Builds the compact server flag string from client configuration.
///
/// Constructs a single-character flag string (e.g., `-logDtpr`) encoding the
/// transfer options negotiated between client and server. The flag order matches
/// upstream `server_options()`.
pub(crate) fn build_server_flag_string(config: &ClientConfig) -> String {
    let mut flags = String::from("-");

    // upstream: options.c:2628-2629 - `if (quiet && msgs2stderr) 'q'`. The
    // default `msgs2stderr` is 2 (nonzero), so plain `-q` packs 'q';
    // `--no-msgs2stderr` (msgs2stderr == 0) suppresses it. The local-half
    // ServerConfig parser ignores 'q' (transfer/flags.rs), so packing it here
    // is inert for the in-process receiver and meaningful only on the wire.
    if config.quiet() && config.msgs2stderr() != Some(false) {
        flags.push('q');
    }

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
    // upstream: options.c:2677-2678 - `if (preserve_devices) argstr[x++] = 'D';
    // /* ignore preserve_specials here */`. The compact 'D' tracks devices only;
    // specials ride as long-form --specials/--no-specials on the wire.
    if config.preserve_devices() {
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
    // upstream: options.c:2644-2648 - only send 'W' when explicitly set
    // (whole_file > 0). The default for remote transfers is no-whole-file
    // (delta mode); upstream never sends --no-whole-file because it is the
    // default. Sending 'W' unconditionally when the tri-state defaults to
    // true forces the remote generator to skip basis-file checksums, so the
    // sender falls back to whole-file even when a basis exists.
    if config.whole_file_raw() == Some(true) && !config.append() {
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
    // upstream: options.c has NO compact 'P' letter. keep_partial rides as the
    // long-form --partial (daemon: build_full_daemon_args; local ServerConfig:
    // propagated via server_config.flags.partial in the *_server_config sites).
    // upstream: options.c:2630-2631 - `make_backups` rides in the compact
    // flag string as `b`. Emitting `--backup` as a separate long arg lands
    // as a positional path on upstream server arg parsers that do not
    // consult popt for long flags.
    if config.backup() {
        flags.push('b');
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
    // upstream: options.c:2658-2659 - 'k' = copy_dirlinks. Packed alongside
    // 'L' so the in-process push sender (build_server_config_for_generator)
    // sets `flags.copy_dirlinks` and the flist walker transmits a
    // symlink-to-directory as a real directory; also forwarded to a remote
    // daemon sender on a pull. Both dereference on the sender exactly like
    // copy_links.
    if config.copy_dirlinks() {
        flags.push('k');
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
/// Maps a spec's two-sided applicability onto the wire's `FILTRULES_SIDES`
/// encoding: a rule that applies to both sides carries *neither* side bit on
/// the wire, while a one-sided rule carries the matching bit.
///
/// oc-rsync's [`FilterRuleSpec`] represents "applies to both sides" as
/// `applies_to_sender() && applies_to_receiver()`, but upstream's wire format
/// encodes that default as no side flag at all. A naive copy of both booleans
/// would serialize a plain `--exclude` as `-sr` instead of `- `.
///
/// upstream: exclude.c:1566-1572 (`get_rule_prefix`) emits `s` iff
/// `FILTRULE_SENDER_SIDE` and `r` iff `FILTRULE_RECEIVER_SIDE`; a both-sides
/// rule has neither bit set (exclude.c:1331 masks `FILTRULES_SIDES`).
fn wire_sender_side(spec: &FilterRuleSpec) -> bool {
    spec.applies_to_sender() && !spec.applies_to_receiver()
}

/// Receiver-side counterpart of [`wire_sender_side`].
fn wire_receiver_side(spec: &FilterRuleSpec) -> bool {
    spec.applies_to_receiver() && !spec.applies_to_sender()
}

pub(crate) fn build_wire_format_rules(
    client_rules: &[FilterRuleSpec],
    delete_excluded: bool,
) -> Result<Vec<FilterRuleWireFormat>, ClientError> {
    let mut wire_rules = Vec::new();

    for spec in client_rules {
        // upstream: exclude.c:1330-1332 add_rule() applies an implicit
        // FILTRULE_SENDER_SIDE when --delete-excluded is active and the
        // rule carries neither FILTRULES_SIDES nor merge/dir-merge. The
        // flag drives `get_rule_prefix()` (exclude.c:1566-1568) to emit
        // the `s` modifier on the wire and `send_rules()` (line 1605) to
        // elide the rule from the receiver's view because it has already
        // been applied locally on the sender.
        let mut spec_owned;
        let spec_ref = if delete_excluded {
            spec_owned = spec.clone();
            spec_owned.apply_implicit_sender_side_for_delete_excluded();
            &spec_owned
        } else {
            spec
        };
        let spec = spec_ref;
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
                    // upstream: 'e' flag = FILTRULE_EXCLUDE_SELF.
                    exclude_from_merge: true,
                    xattr_only: spec.is_xattr_only(),
                    sender_side: wire_sender_side(spec),
                    receiver_side: wire_receiver_side(spec),
                    perishable: spec.is_perishable(),
                    negate: spec.is_negated(),
                    ..FilterRuleWireFormat::default()
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
            xattr_only: spec.is_xattr_only(),
            sender_side: wire_sender_side(spec),
            receiver_side: wire_receiver_side(spec),
            perishable: spec.is_perishable(),
            negate: spec.is_negated(),
            ..FilterRuleWireFormat::default()
        };

        if let Some(options) = spec.dir_merge_options() {
            wire_rule.no_inherit = !options.inherit_rules();
            wire_rule.word_split = options.uses_whitespace();
            wire_rule.exclude_from_merge = options.excludes_self();
            // upstream: exclude.c:1227-1237 - the `-`/`+` modifier on a
            // dir-merge rule sets FILTRULE_NO_PREFIXES (and FILTRULE_INCLUDE for
            // `+`), so the per-directory file's bare lines are taken as literal
            // excludes (or includes) rather than prefixed rules. Without
            // carrying this on the wire, the remote sender parses the merge file
            // with the strict short-form parser and rejects a bare pattern like
            // `file3` as an unrecognised rule.
            match options.enforced_kind() {
                Some(DirMergeEnforcedKind::Exclude) => wire_rule.no_prefixes = true,
                Some(DirMergeEnforcedKind::Include) => {
                    wire_rule.no_prefixes = true;
                    wire_rule.no_prefixes_include = true;
                }
                None => {}
            }
            // upstream: exclude.c:1248-1254 - the `C` modifier on a dir-merge
            // rule sets FILTRULE_CVS_IGNORE on the wire. Without this, `-f:C`
            // would round-trip through the remote shell as a bare dir-merge
            // missing the CVS flag, so the remote sender could neither
            // default the empty pattern to `.cvsignore` nor activate
            // CVS-style whitespace parsing of the per-directory file.
            wire_rule.cvs_exclude = options.is_cvs_mode();
            // upstream: exclude.c:1566-1572 get_rule_prefix() emits `s`/`r` for a
            // side-restricted dir-merge. A `:s`/`:r` dir-merge's side lives in
            // DirMergeOptions, NOT in the spec's applies_to_* booleans that
            // wire_sender_side()/wire_receiver_side() read (FilterRuleSpec::dir_merge
            // hardcodes both true). Without carrying it here, a `:s .filt`
            // serializes with no side flag, so the receiver's --delete pass loads
            // it (and any nested dir-merge) as two-sided and wrongly protects
            // flist-absent files from deletion. Mirror the both-sides==no-flag
            // convention exactly.
            let dm_sender = options.applies_to_sender();
            let dm_receiver = options.applies_to_receiver();
            wire_rule.sender_side = dm_sender && !dm_receiver;
            wire_rule.receiver_side = dm_receiver && !dm_sender;
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
    // upstream: receiver.c:855 - append mode implies inplace; the sum_head
    // block-skip (generator.c:786) and flength derivation (sender.c:89) on both
    // the local sender (push) and receiver (pull) roles gate on these flags, so
    // they must be carried onto the in-process ServerConfig for SSH and daemon.
    server_config.flags.append = config.append();
    server_config.flags.append_verify = config.append_verify();
    server_config.has_partial_dir = config.partial_directory().is_some();
    server_config.partial_dir = config.partial_directory().map(std::path::Path::to_path_buf);
    server_config.file_selection.min_file_size = config.min_file_size();
    server_config.file_selection.max_file_size = config.max_file_size();
    // upstream: generator.c:quick_check_ok() -> same_time() applies the
    // `--modify-window` tolerance on the receiver. For a remote-shell pull the
    // local client IS the receiver, so carry the window onto its config; the
    // server-side receiver (push) picks it up from the forwarded
    // `--modify-window=NUM` arg. `modify_window()` is None when unset (window 0).
    server_config.file_selection.modify_window = config.modify_window().unwrap_or(0);
    // upstream: options.c:2046-2048 - do_stats sets INFO_STATS to level 2+
    server_config.do_stats = config.stats();
    // upstream: generator.c:124 - EARLY_DELETE_DONE_MSG = !(delete_during==2 || delete_after)
    server_config.deletion.late_delete =
        matches!(config.delete_mode(), DeleteMode::Delay | DeleteMode::After);
    // upstream: options.c `delete_excluded` - the receiver's delete pass must
    // treat filter-excluded (non-protected) entries as deletable. For a
    // remote-shell pull the receiver builds its deletion chain from the local
    // CLI filter rules, so the flag has to be carried onto this local receiver
    // config (the wire-side sender conversion in build_wire_format_rules only
    // affects what the remote sender hides, not local delete protection).
    server_config.deletion.delete_excluded = config.delete_excluded();
    // upstream: delete.c:156 - `--max-delete` is enforced by the generator,
    // which for a remote-shell pull runs on the local client (the receiver).
    // The `--max-delete=NUM` server arg is forwarded to the remote sender too,
    // but a pull sender never deletes, so the cap must be carried onto this
    // local receiver config or it is silently ignored (unbounded deletion).
    server_config.deletion.max_delete = config.max_delete();
    logging::debug_log!(
        Del,
        2,
        "receiver config: delete_excluded={} delete_mode={:?} max_delete={:?}",
        config.delete_excluded(),
        config.delete_mode(),
        config.max_delete()
    );
    // upstream: options.c:2881-2885 - copy_unsafe_links and safe_links are long-form only
    server_config.flags.copy_unsafe_links = config.copy_unsafe_links();
    server_config.flags.safe_links = config.safe_links();
    // upstream: flist.c:1419 / options.c:2987 - `--copy-devices` converts device
    // entries into regular files on the SENDER so their contents stream like a
    // file. The flag is long-form only; on a push the local client IS the sender
    // and never sends `--copy-devices` over the wire (`if (copy_devices &&
    // !am_sender)`), so the in-process sender must carry it here. On a pull the
    // local half is the receiver, where the flag is inert (the remote sender does
    // the conversion), so setting it unconditionally is safe.
    server_config.flags.copy_devices = config.copy_devices();
    // upstream: syscall.c do_open / do_open_nofollow propagate O_NOATIME when set.
    server_config.write.open_noatime = config.open_noatime();
    // upstream: options.c:2750-2762 - itemize_changes is forwarded to the remote
    // as --log-format=%i, but the local ServerConfig also needs the flag set so
    // the generator's maybe_emit_itemize() produces client-side output via callback.
    server_config.flags.info_flags.itemize = config.itemize_changes();
    // upstream: generator.c:575-576 - `-ii` (stdout_format_has_i > 1) and
    // `--info=name2` make the generator emit itemize rows for unchanged
    // entries too; the local receiver-generator needs the same gate.
    server_config.flags.info_flags.itemize_unchanged = config.itemize_unchanged();
    // upstream: the in-process local receiver/generator (SSH pull/push) needs
    // the client's verbose level so per-file `info_log!(Name)` rows and the
    // `receiving incremental file list` banner fire - both gated on
    // `flags.verbose && client_mode` (receiver pipeline.rs / setup.rs).
    // `build_server_flag_string` never sends 'v' to the remote and this local
    // ServerConfig is built from that same string, so `verbose` stays false
    // without this. Mirrors the daemon path (daemon_transfer server_config.rs).
    server_config.flags.verbose = config.verbosity() > 0;
    server_config.flags.verbose_level = config.verbosity();
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
    // upstream: compat.c:819 parse_checksum_choice(1) - an explicit
    // --checksum-choice=ALGO forces the negotiated checksum for the transfer.
    // Carry it onto this local ServerConfig so the in-process generator/receiver
    // half (SSH and daemon transfers alike) forwards it into the capability
    // negotiator's checksum_override; without this the choice is silently
    // dropped at protocol >= 30 (binary negotiation).
    server_config.checksum_choice = config.checksum_protocol_override();
    // upstream: compat.c:543-544 - `if (do_compression && !compress_choice)`
    // gates the compression vstring list, so an explicit --compress-choice (also
    // its --new-compress/--old-compress aliases) sets compress_choice and
    // suppresses the list. Carry that explicit choice onto the local half here so
    // the client-sender's TransferConfig has connection.compress_choice = Some(..)
    // and transfer/src/lib.rs's send_compression gate stays false; without this
    // the client sent an extra vstring the peer never expected and desynced the
    // stream. Non-explicit `-z`/`-zz` leaves compress_choice None (vstring
    // negotiation), matching the daemon-push path in daemon_transfer.
    if config.explicit_compress_choice()
        && let Ok(algo) =
            protocol::CompressionAlgorithm::parse(config.compression_algorithm().name())
    {
        server_config.connection.compress_choice = Some(algo);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::filters::RuleType;

    // upstream: options.c has NO compact 'P' letter; keep_partial is conveyed
    // long-form (--partial) or via propagated ServerConfig.flags.partial.
    // build_server_flag_string must never pack 'P'.
    #[test]
    fn server_flag_string_never_packs_partial_p() {
        let config = ClientConfig::builder().partial(true).build();
        let flags = build_server_flag_string(&config);
        assert!(!flags.contains('P'), "must not pack compact 'P': {flags}");
    }

    // upstream: options.c:2677-2678 - the compact 'D' tracks preserve_devices
    // only. specials-only must NOT pack 'D' (it rides as --specials long-form).
    #[test]
    fn server_flag_string_specials_only_omits_d() {
        let config = ClientConfig::builder().specials(true).build();
        let flags = build_server_flag_string(&config);
        assert!(
            !flags.contains('D'),
            "specials-only must not pack 'D': {flags}"
        );
    }

    #[test]
    fn server_flag_string_devices_packs_d() {
        let config = ClientConfig::builder().devices(true).build();
        let flags = build_server_flag_string(&config);
        assert!(flags.contains('D'), "devices must pack 'D': {flags}");
    }

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
    fn apply_common_server_flags_wires_explicit_compress_choice() {
        // upstream: compat.c:543-544 - an explicit --compress-choice sets
        // compress_choice, which suppresses the compression vstring list. Carry
        // it onto the local half so the client-sender's send_compression gate
        // (transfer/src/lib.rs: do_compression && compress_choice.is_none())
        // stays false and no stray vstring desyncs the stream.
        let config = ClientConfig::builder()
            .compress(true)
            .compression_algorithm(compress::algorithm::CompressionAlgorithm::Zlib)
            .build();
        assert!(config.explicit_compress_choice());
        let mut server_config = ServerConfig::default();
        apply_common_server_flags(&config, &mut server_config);
        assert_eq!(
            server_config.connection.compress_choice,
            Some(protocol::CompressionAlgorithm::Zlib),
            "explicit --compress-choice must land on connection.compress_choice"
        );
    }

    #[test]
    fn apply_common_server_flags_default_compress_leaves_choice_none() {
        // Plain `-z` (no explicit choice) must leave compress_choice None so the
        // sender still negotiates via the vstring list, matching upstream.
        let config = ClientConfig::builder().compress(true).build();
        assert!(!config.explicit_compress_choice());
        let mut server_config = ServerConfig::default();
        apply_common_server_flags(&config, &mut server_config);
        assert_eq!(server_config.connection.compress_choice, None);
    }

    #[test]
    fn converts_empty_filter_list() {
        let rules = build_wire_format_rules(&[], false).expect("should convert empty list");
        assert_eq!(rules.len(), 0);
    }

    #[test]
    fn converts_simple_exclude_rule() {
        let spec = FilterRuleSpec::exclude("*.log");
        let rules = build_wire_format_rules(&[spec], false).expect("should convert exclude rule");

        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].rule_type, RuleType::Exclude);
        assert_eq!(rules[0].pattern, "*.log");
        assert!(!rules[0].anchored);
        assert!(!rules[0].directory_only);
    }

    #[test]
    fn converts_simple_include_rule() {
        let spec = FilterRuleSpec::include("*.txt");
        let rules = build_wire_format_rules(&[spec], false).expect("should convert include rule");

        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].rule_type, RuleType::Include);
        assert_eq!(rules[0].pattern, "*.txt");
        assert!(!rules[0].anchored);
        assert!(!rules[0].directory_only);
    }

    #[test]
    fn detects_anchored_pattern() {
        let spec = FilterRuleSpec::exclude("/tmp");
        let rules = build_wire_format_rules(&[spec], false).expect("should convert anchored rule");

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
        let rules =
            build_wire_format_rules(&[spec], false).expect("should convert directory-only rule");

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
        let rules = build_wire_format_rules(&[spec], false).expect("should convert dir wildcard");

        assert_eq!(rules.len(), 1);
        assert!(rules[0].directory_only);
        assert_eq!(rules[0].pattern, "*");
    }

    #[test]
    fn anchored_directory_only_preserves_both_flags() {
        let spec = FilterRuleSpec::exclude("/build/");
        let rules = build_wire_format_rules(&[spec], false)
            .expect("should convert anchored directory rule");

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
        let rules =
            build_wire_format_rules(&[spec], false).expect("should convert bare slash rule");

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
        let rules = build_wire_format_rules(&[spec], false).expect("should convert side flags");

        assert_eq!(rules.len(), 1);
        assert!(rules[0].sender_side);
        assert!(!rules[0].receiver_side);
    }

    #[test]
    fn preserves_perishable_flag() {
        let spec = FilterRuleSpec::exclude("*.swp").with_perishable(true);
        let rules =
            build_wire_format_rules(&[spec], false).expect("should convert perishable flag");

        assert_eq!(rules.len(), 1);
        assert!(rules[0].perishable);
    }

    #[test]
    fn preserves_xattr_only_flag() {
        let spec = FilterRuleSpec::exclude("user.*").with_xattr_only(true);
        let rules =
            build_wire_format_rules(&[spec], false).expect("should convert xattr_only flag");

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

        let rules = build_wire_format_rules(&specs, false).expect("should convert all rule types");

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

        let rules =
            build_wire_format_rules(&specs, false).expect("should transmit ExcludeIfPresent");

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
        let rules =
            build_wire_format_rules(&[spec], false).expect("should convert dir_merge options");

        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].rule_type, RuleType::DirMerge);
        assert!(rules[0].no_inherit);
        assert!(rules[0].exclude_from_merge);
        assert!(rules[0].word_split);
    }

    /// `-f:C` (and `--filter=:C`) is parsed locally into a DirMerge spec with
    /// the `cvs_mode` option set. The wire encoder must forward that as
    /// `cvs_exclude=true` so the remote sender can default the empty pattern
    /// to `.cvsignore` (upstream `exclude.c:1404-1408`) and enable CVS-style
    /// whitespace parsing of the merge file. Without this, `-f:C` reached the
    /// remote rsync with the `C` modifier stripped, producing an empty merge
    /// filename and silently disabling per-directory `.cvsignore` lookup.
    #[test]
    fn dir_merge_cvs_mode_forwards_cvs_exclude_flag() {
        use engine::local_copy::DirMergeOptions;

        let options = DirMergeOptions::new()
            .use_whitespace()
            .allow_comments(false)
            .allow_list_clearing(true)
            .inherit(false)
            .cvs_mode(true);

        let spec = FilterRuleSpec::dir_merge(".cvsignore", options);
        let rules = build_wire_format_rules(&[spec], false)
            .expect("should forward cvs_mode to wire format");

        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].rule_type, RuleType::DirMerge);
        assert!(rules[0].cvs_exclude);
        assert!(rules[0].no_inherit);
        assert!(rules[0].word_split);
    }

    /// upstream: exclude.c:1330-1332 - --delete-excluded applies an implicit
    /// FILTRULE_SENDER_SIDE to bare include/exclude rules. The wire encoder
    /// is the user-visible surface for this: `get_rule_prefix()` emits an
    /// `s` modifier in the rule prefix. Without this, oc-rsync's wire output
    /// would diverge from upstream's `- *.tmp` vs `-s *.tmp` byte stream.
    #[test]
    fn delete_excluded_marks_bare_exclude_sender_side() {
        let spec = FilterRuleSpec::exclude("*.tmp");
        let rules = build_wire_format_rules(&[spec], true)
            .expect("delete_excluded should apply implicit sender_side");

        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].rule_type, RuleType::Exclude);
        assert!(
            rules[0].sender_side,
            "implicit FILTRULE_SENDER_SIDE must be applied"
        );
        assert!(
            !rules[0].receiver_side,
            "receiver_side must be cleared by the implicit flag"
        );
    }

    #[test]
    fn delete_excluded_marks_bare_include_sender_side() {
        let spec = FilterRuleSpec::include("keep/**");
        let rules = build_wire_format_rules(&[spec], true).expect("delete_excluded include");

        assert_eq!(rules.len(), 1);
        assert!(rules[0].sender_side);
        assert!(!rules[0].receiver_side);
    }

    /// A rule that explicitly carries a side hint (`s`, `r`, `show`, `hide`)
    /// must be left untouched - upstream `exclude.c:1331` masks against
    /// `FILTRULES_SIDES` and only applies the implicit flag when neither
    /// side bit is set.
    #[test]
    fn delete_excluded_leaves_explicit_sender_rule_alone() {
        let spec = FilterRuleSpec::hide("*.tmp");
        let rules = build_wire_format_rules(&[spec], true).expect("hide rule with delete_excluded");

        assert_eq!(rules.len(), 1);
        assert!(rules[0].sender_side);
        assert!(!rules[0].receiver_side);
    }

    #[test]
    fn delete_excluded_leaves_explicit_receiver_rule_alone() {
        let spec = FilterRuleSpec::exclude("keep.txt").with_sender(false);
        let rules =
            build_wire_format_rules(&[spec], true).expect("receiver-only with delete_excluded");

        assert_eq!(rules.len(), 1);
        assert!(!rules[0].sender_side);
        assert!(rules[0].receiver_side);
    }

    /// Protect/Risk and DirMerge rules must not gain the implicit flag.
    /// `exclude.c:1331` excludes merge rules from the mask; Protect/Risk
    /// already restrict to the receiver side, so neither match the
    /// "no FILTRULES_SIDES bit" precondition.
    #[test]
    fn delete_excluded_leaves_protect_rule_alone() {
        let spec = FilterRuleSpec::protect("keep");
        let rules = build_wire_format_rules(&[spec], true).expect("protect with delete_excluded");

        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].rule_type, RuleType::Protect);
        assert!(!rules[0].sender_side);
        assert!(rules[0].receiver_side);
    }

    #[test]
    fn delete_excluded_leaves_dir_merge_rule_alone() {
        use engine::local_copy::DirMergeOptions;

        let spec = FilterRuleSpec::dir_merge(".rsync-filter", DirMergeOptions::new());
        let rules = build_wire_format_rules(&[spec], true).expect("dir_merge with delete_excluded");

        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].rule_type, RuleType::DirMerge);
        // DirMerge defaults apply to both sides, which upstream encodes as
        // neither FILTRULES_SIDES bit (no `s`/`r` modifier on the wire).
        assert!(!rules[0].sender_side);
        assert!(!rules[0].receiver_side);
    }

    /// Without --delete-excluded the implicit flag must not be applied even
    /// for bare include/exclude rules. A plain exclude applies to both sides,
    /// which upstream serializes with neither FILTRULES_SIDES bit (`- *.tmp`,
    /// not `-sr *.tmp`); the wire encoder must carry no side flag.
    ///
    /// upstream: exclude.c:1566-1572 - a both-sides rule emits no `s`/`r`.
    #[test]
    fn no_delete_excluded_leaves_bare_rule_untouched() {
        let spec = FilterRuleSpec::exclude("*.tmp");
        let rules = build_wire_format_rules(&[spec], false).expect("no delete_excluded");

        assert_eq!(rules.len(), 1);
        assert!(!rules[0].sender_side);
        assert!(!rules[0].receiver_side);
    }

    /// Wire-byte parity: when `--delete-excluded` is active, the rule must
    /// serialize through `build_rule_prefix()` with the `s` modifier on the
    /// wire. This is the byte stream upstream rsync 3.4.4 emits for the same
    /// CLI input.
    ///
    /// upstream: exclude.c:1566-1568 `get_rule_prefix()` emits `s` when
    /// `FILTRULE_SENDER_SIDE` is set and (`!for_xfer` or proto >= 29).
    #[test]
    fn delete_excluded_wire_prefix_carries_s_modifier() {
        use protocol::ProtocolVersion;
        use protocol::filters::build_rule_prefix;

        let spec = FilterRuleSpec::exclude("*.tmp");
        let rules = build_wire_format_rules(&[spec], true)
            .expect("delete_excluded should apply implicit sender_side");

        let proto = ProtocolVersion::from_supported(32).unwrap();
        let prefix = build_rule_prefix(&rules[0], proto).expect("prefix must serialize");

        // Upstream 3.4.4 emits `-s ` for `--exclude *.tmp` under
        // `--delete-excluded`. Without the implicit flag, the prefix would
        // be `- `, diverging from the upstream wire.
        assert_eq!(prefix, "-s ");
    }

    /// Wire-byte parity: a plain `--exclude` (applies to both sides) must
    /// serialize as `- pattern` with no `s`/`r` modifier. oc-rsync's spec
    /// encodes "both sides" as both `applies_to_*` booleans, but upstream
    /// carries neither FILTRULES_SIDES bit, so a naive copy would emit the
    /// divergent `-sr ` prefix instead of `- `.
    ///
    /// upstream: exclude.c:1566-1572 - `get_rule_prefix()` emits no side
    /// modifier when neither `FILTRULE_SENDER_SIDE` nor `FILTRULE_RECEIVER_SIDE`
    /// is set.
    #[test]
    fn plain_exclude_wire_prefix_has_no_side_modifier() {
        use protocol::ProtocolVersion;
        use protocol::filters::build_rule_prefix;

        let spec = FilterRuleSpec::exclude("*.tmp");
        let rules = build_wire_format_rules(&[spec], false).expect("plain exclude");

        let proto = ProtocolVersion::from_supported(32).unwrap();
        let prefix = build_rule_prefix(&rules[0], proto).expect("prefix must serialize");

        assert_eq!(prefix, "- ");
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

    // upstream: options.c:2644-2648 - 'W' is only sent when whole_file > 0
    // (explicitly forced). The default for remote transfers is auto (-1),
    // which does NOT send 'W'. Sending 'W' unconditionally causes the
    // remote generator to skip basis-file checksums, defeating delta.
    #[test]
    fn server_flag_string_omits_w_when_whole_file_not_set() {
        let config = ClientConfig::builder().build();
        let flags = build_server_flag_string(&config);
        assert!(
            !flags.contains('W'),
            "default config must not include 'W' (whole-file): {flags}"
        );
    }

    #[test]
    fn server_flag_string_includes_w_when_whole_file_explicit() {
        let config = ClientConfig::builder().whole_file(true).build();
        let flags = build_server_flag_string(&config);
        assert!(
            flags.contains('W'),
            "explicit whole_file(true) must include 'W': {flags}"
        );
    }

    #[test]
    fn server_flag_string_omits_w_when_whole_file_false() {
        let config = ClientConfig::builder().whole_file(false).build();
        let flags = build_server_flag_string(&config);
        assert!(
            !flags.contains('W'),
            "explicit whole_file(false) must not include 'W': {flags}"
        );
    }
}
