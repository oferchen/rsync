// Parsing of the long-form options upstream `server_options()` sends after the
// compact flag string, plus detection of client-only batch flags that must
// never reach the daemon.
/// Applies long-form arguments from the client to the server configuration.
///
/// Upstream rsync's `server_options()` (options.c:2755-2998) sends many options
/// as long-form arguments that are not encoded in the compact flag string.
/// The daemon must parse these to correctly configure the transfer.
///
/// Returns `Some(rejection)` when an argument must abort the session rather
/// than be applied. The caller surfaces that as an `@ERROR` and exits instead
/// of letting the argument drive a silent connection close mid file-list
/// framing, or - worse for a bad value - be silently ignored. See
/// [`ClientArgRejection`] for the two upstream rules involved.
///
/// # Upstream Reference
///
/// - `options.c:1460-1465` - daemon-mode unknown option error path
/// - `options.c:2836-2847` - delete mode variants
/// - `options.c:2854-2855` - `--size-only`
/// - `options.c:2896-2897` - `--ignore-errors`
/// - `options.c:2906` - `--numeric-ids`
/// - `options.c:2909` - `--use-qsort`
/// - `options.c:2755-2758` - `--compress-level=N`
fn apply_long_form_args(
    client_args: &[String],
    config: &mut ServerConfig,
) -> Option<ClientArgRejection> {
    // Positional path args follow the standalone `.` separator. Upstream
    // `glob_expand_module()` consumes them through a different code path, so
    // the daemon's option parser only validates the option region.
    let dot_position = client_args.iter().position(|a| a == ".");

    let mut rejection: Option<ClientArgRejection> = None;
    let mut i = 0;
    while i < client_args.len() {
        let arg = &client_args[i];
        if dot_position.is_some_and(|dot| i >= dot) {
            i += 1;
            continue;
        }
        match arg.as_str() {
            // upstream: options.c:2836-2847 - delete mode variants
            "--delete" | "--delete-before" | "--delete-during" => {
                config.flags.delete = true;
            }
            "--delete-delay" => {
                config.flags.delete = true;
                config.deletion.late_delete = true;
            }
            // upstream: generator.c:2427-2428 - only --delete-after defers the
            // delete *decision* to after the transfer; --delete-delay decides
            // during the walk (generator.c:2315) and defers only the unlink.
            "--delete-after" => {
                config.flags.delete = true;
                config.deletion.late_delete = true;
                config.deletion.delete_after = true;
            }
            "--delete-excluded" => {
                config.flags.delete = true;
            }
            // upstream: options.c:2856-2857 - --stats sets do_stats which causes
            // INFO_STATS to level 2+. Without this flag, the generator does not
            // emit NDX_DEL_STATS during the goodbye phase and the client sender's
            // "Number of deleted files" line stays at zero on daemon uploads.
            "--stats" => {
                config.do_stats = true;
            }
            // upstream: options.c:2854-2855
            "--size-only" => {
                config.file_selection.size_only = true;
            }
            // upstream: options.c:2896-2897
            "--ignore-errors" => {
                config.deletion.ignore_errors = true;
            }
            // upstream: options.c:2899-2900
            "--copy-unsafe-links" => {
                config.flags.copy_unsafe_links = true;
            }
            // upstream: options.c:2902-2903
            "--safe-links" => {
                config.flags.safe_links = true;
            }
            // upstream: options.c:2905-2906 - an explicit client --numeric-ids
            // sets `numeric_ids = 1` (drops the wire name-list entirely).
            "--numeric-ids" => {
                config.flags.numeric_ids = core::server::NumericIds::Explicit;
            }
            // upstream: options.c:2976-2977 - `--no-implied-dirs` forwarded to
            // the sender on a pull. The daemon-sender must omit implied parent
            // dirs from the flist at protocol < 30 (flist.c:2468); protocol >= 30
            // always sends them (flist.c:2257-2258).
            "--no-implied-dirs" => {
                config.flags.no_implied_dirs = true;
            }
            // upstream: options.c:2908-2909
            "--use-qsort" => {
                config.qsort = true;
            }
            // upstream: options.c:2918-2919
            "--ignore-existing" => {
                config.file_selection.ignore_existing = true;
            }
            // upstream: options.c:2922-2923
            "--existing" => {
                config.file_selection.existing_only = true;
            }
            // upstream: options.c:2870-2871
            "--ignore-missing-args" => {
                config.file_selection.ignore_missing_args = true;
            }
            "--delete-missing-args" => {
                config.file_selection.delete_missing_args = true;
            }
            // upstream: options.c:2951-2960
            "--inplace" => {
                config.write.inplace = true;
            }
            // upstream: options.c:1722-1726 - OPT_APPEND increments append_mode
            // on the server side. A second `--append` (append_mode == 2) is the
            // wire encoding of `--append-verify`; the client never sends the
            // long-form `--append-verify` to a server.
            "--append" => {
                if config.flags.append {
                    config.flags.append_verify = true;
                }
                config.flags.append = true;
            }
            // upstream: options.c:2891-2892
            "--delay-updates" => {
                config.write.delay_updates = true;
            }
            // upstream: options.c:2930-2931
            "--fsync" => {
                config.write.fsync = true;
            }
            // oc-specific: `--zero-copy` opts the daemon-sender's socket write
            // side into io_uring SEND_ZC. The client forwards it only when the
            // user requested it; `--no-zero-copy` pins the policy to Disabled.
            // Neither has an upstream `server_options()` counterpart, so they
            // are only sent when both ends are oc-rsync (same precedent as
            // `--io-uring-depth`). The default (flag absent) leaves the policy
            // at `Auto`, keeping the transfer byte- and behavior-identical.
            "--zero-copy" => {
                config.write.zero_copy_policy = fast_io::ZeroCopyPolicy::Enabled;
            }
            "--no-zero-copy" => {
                config.write.zero_copy_policy = fast_io::ZeroCopyPolicy::Disabled;
            }
            // upstream: options.c:2996-2997 - --mkpath forwarded to the daemon
            // receiver on a push. Gates dest-arg path creation (main.c:738
            // make_path vs main.c:796 single do_mkdir).
            "--mkpath" => {
                config.flags.mkpath = true;
            }
            "--no-mkpath" => {
                config.flags.mkpath = false;
            }
            // upstream: options.c:2197-2199 - `--old-dirs`/`--old-d` set
            // xfer_dirs=4, resolved to recurse=1 plus an appended `- /*/*`
            // filter. server_options() never forwards these deprecated flags; a
            // client encodes them as `-r` in the compact flag string and sends
            // `- /*/*` over the wire filter list. Consumed here without mkpath
            // semantics so a stray forward is not mistaken for a positional path.
            "--old-dirs" | "--old-d" => {}
            // upstream: options.c:2849 - backup
            "--backup" => {
                config.flags.backup = true;
            }
            // Two-arg options: upstream sends option and value as separate args.
            // upstream: options.c:2933-2941 - reference directories
            "--compare-dest" => {
                if let Some(dir) = client_args.get(i + 1) {
                    config.reference_directories.push(ReferenceDirectory::new(ReferenceDirectoryKind::Compare, std::path::PathBuf::from(dir)));
                    i += 1;
                }
            }
            "--copy-dest" => {
                if let Some(dir) = client_args.get(i + 1) {
                    config.reference_directories.push(ReferenceDirectory::new(ReferenceDirectoryKind::Copy, std::path::PathBuf::from(dir)));
                    i += 1;
                }
            }
            "--link-dest" => {
                if let Some(dir) = client_args.get(i + 1) {
                    config.reference_directories.push(ReferenceDirectory::new(ReferenceDirectoryKind::Link, std::path::PathBuf::from(dir)));
                    i += 1;
                }
            }
            // upstream: options.c:2805-2808 - backup-dir as separate args
            "--backup-dir" => {
                config.flags.backup = true;
                if let Some(dir) = client_args.get(i + 1) {
                    config.backup_dir = Some(dir.to_owned());
                    i += 1;
                }
            }
            // upstream: options.c:2809-2811 - suffix as separate args
            // When --backup-dir is specified without explicit --suffix,
            // upstream changes the default suffix from "~" to "" and sends
            // --suffix as a two-arg form (not --suffix=VALUE).
            "--suffix" | "--backup-suffix" => {
                if let Some(suffix) = client_args.get(i + 1) {
                    config.backup_suffix = Some(suffix.to_owned());
                    i += 1;
                }
            }
            // upstream: options.c:2925-2927 - temp-dir as separate args
            "--temp-dir" => {
                if let Some(dir) = client_args.get(i + 1) {
                    config.temp_dir = Some(std::path::PathBuf::from(dir));
                    i += 1;
                }
            }
            // upstream: options.c:3052-3056 - `if (partial_dir && am_sender)`
            // emits `--partial-dir` and its value as two argv entries via
            // `safe_arg("", partial_dir)`, then `--delay-updates` when that is
            // also active. The receiving side stages each incoming temp file
            // through this directory and looks there for a resume basis
            // (`cleanup.c:handle_partial_dir`), so a daemon that consumes the
            // adjacent `--delay-updates` but drops this value honours the
            // staging request with nowhere to stage.
            "--partial-dir" => {
                if let Some(dir) = client_args.get(i + 1) {
                    config.partial_dir = Some(std::path::PathBuf::from(dir));
                    config.has_partial_dir = true;
                    i += 1;
                }
            }
            // upstream: options.c:2818-2823 - --compress-choice, --new-compress, --old-compress
            "--new-compress" => {
                config.flags.compress = true;
                if config.connection.compression_level.is_none() {
                    config.connection.compression_level =
                        Some(compress::zlib::CompressionLevel::Default);
                }
            }
            "--old-compress" => {
                config.flags.compress = true;
                if config.connection.compression_level.is_none() {
                    config.connection.compression_level =
                        Some(compress::zlib::CompressionLevel::Default);
                }
            }
            _ => {
                // upstream: options.c:2818-2823 - --compress-choice=ALGO
                if let Some(_choice) = arg
                    .strip_prefix("--compress-choice=")
                    .or_else(|| arg.strip_prefix("--zc="))
                {
                    // Mark compression as active. The actual algorithm is parsed
                    // later from client_args in run_server_with_handshake().
                    config.flags.compress = true;
                    if config.connection.compression_level.is_none() {
                        config.connection.compression_level =
                            Some(compress::zlib::CompressionLevel::Default);
                    }
                // upstream: options.c:2755-2758
                } else if let Some(level_str) = arg.strip_prefix("--compress-level=") {
                    if let Ok(level) = level_str.parse::<u32>() {
                        if let Ok(cl) = compress::zlib::CompressionLevel::from_numeric(level) {
                            config.connection.compression_level = Some(cl);
                        }
                    }
                // upstream: options.c:2825-2828
                } else if let Some(val) = arg.strip_prefix("--max-delete=") {
                    if let Ok(n) = val.parse::<i64>() {
                        if n >= 0 {
                            config.deletion.max_delete = Some(n as u64);
                        }
                    }
                // upstream: options.c:2998-3001 - `server_options()` forwards
                // `--min-size`/`--max-size` (as one `--opt=VALUE` token, see
                // safe_arg at options.c:2716-2720) only when the local end is
                // the sender, i.e. only to a daemon that is RECEIVING a push.
                // That is the one direction where the filter runs on the
                // daemon: enforcement lives in the generator
                // (generator.c:2118-2133), which is the receiving side. A
                // dropped value therefore lets a push deposit exactly the
                // files the client asked to exclude.
                } else if let Some(val) = arg.strip_prefix("--max-size=") {
                    match parse_transfer_size_limit("max-size", val) {
                        Ok(limit) => config.file_selection.max_file_size = Some(limit),
                        Err(message) => {
                            rejection.get_or_insert(ClientArgRejection::InvalidValue(message));
                        }
                    }
                } else if let Some(val) = arg.strip_prefix("--min-size=") {
                    match parse_transfer_size_limit("min-size", val) {
                        Ok(limit) => config.file_selection.min_file_size = Some(limit),
                        Err(message) => {
                            rejection.get_or_insert(ClientArgRejection::InvalidValue(message));
                        }
                    }
                // upstream: options.c - server_options() forwards `--modify-window=NUM`.
                // The daemon receiver's quick-check honours it via same_time() so
                // files within the window are not needlessly re-transferred.
                } else if let Some(val) = arg.strip_prefix("--modify-window=") {
                    if let Ok(n) = val.trim_start_matches('+').parse::<i64>() {
                        config.file_selection.modify_window = ::metadata::ModifyWindow::from_secs(n);
                    }
                // upstream: options.c:2953-2954 - the client forwards the block
                // size as a standalone `-B%u` token, and options.c:1795-1805
                // parses it back into the same `block_size` global. The daemon
                // decodes the client argv here rather than through the
                // `--server` argv parser, so it needs its own arm; both call the
                // one shared bound check.
                } else if let Some(val) = arg
                    .strip_prefix("-B")
                    .or_else(|| arg.strip_prefix("--block-size="))
                {
                    if let Ok(size) = parse_block_size_arg(val, config.protocol) {
                        config.block_size = size;
                    }
                // upstream: options.c:2874 - a negative modify_window is
                // forwarded via the short `-@%d` spelling (e.g. `-@-1`) for
                // nanosecond-exact comparison (util1.c:1482).
                } else if let Some(val) = arg.strip_prefix("-@") {
                    if let Ok(n) = val.parse::<i64>() {
                        config.file_selection.modify_window = ::metadata::ModifyWindow::from_secs(n);
                    }
                // Fallback: =value format for reference directories and backup options.
                // Handles both upstream (two-arg) and legacy (=value) formats.
                } else if let Some(dir) = arg.strip_prefix("--backup-dir=") {
                    config.flags.backup = true;
                    config.backup_dir = Some(dir.to_owned());
                } else if let Some(suffix) = arg.strip_prefix("--suffix=") {
                    config.backup_suffix = Some(suffix.to_owned());
                } else if let Some(suffix) = arg.strip_prefix("--backup-suffix=") {
                    config.backup_suffix = Some(suffix.to_owned());
                } else if let Some(dir) = arg.strip_prefix("--link-dest=") {
                    config.reference_directories.push(ReferenceDirectory::new(ReferenceDirectoryKind::Link, std::path::PathBuf::from(dir)));
                } else if let Some(dir) = arg.strip_prefix("--compare-dest=") {
                    config.reference_directories.push(ReferenceDirectory::new(ReferenceDirectoryKind::Compare, std::path::PathBuf::from(dir)));
                } else if let Some(dir) = arg.strip_prefix("--copy-dest=") {
                    config.reference_directories.push(ReferenceDirectory::new(ReferenceDirectoryKind::Copy, std::path::PathBuf::from(dir)));
                } else if let Some(dir) = arg.strip_prefix("--temp-dir=") {
                    config.temp_dir = Some(std::path::PathBuf::from(dir));
                } else if let Some(dir) = arg.strip_prefix("--partial-dir=") {
                    config.partial_dir = Some(std::path::PathBuf::from(dir));
                    config.has_partial_dir = true;
                } else if let Some(path) = arg.strip_prefix("--files-from=") {
                    config.file_selection.files_from_path = Some(path.to_owned());
                // upstream: options.c:2912 / 2915 - --usermap=SPEC / --groupmap=SPEC.
                // After unbackslash_arg / secluded-args delivery the spec arrives
                // verbatim (`*:1234` wildcards intact) so we hand it directly to
                // the metadata parser. Without this step the daemon-mode receiver
                // would silently discard `--groupmap` / `--usermap` and the
                // wildcard would never take effect on the destination - the
                // regression captured by upstream's daemon-groupmap-wild test
                // (issue #829).
                //
                // upstream: uidlist.c:parse_name_map() parses the spec.
                // A malformed spec leaves the field unset rather than aborting
                // the session because upstream's daemon path falls through to
                // its default id-mapping when parsing fails and the receiver
                // still completes the transfer with unmapped ids.
                } else if let Some(spec) = arg.strip_prefix("--usermap=") {
                    if let Ok(mapping) = ::metadata::UserMapping::parse(spec) {
                        config.user_mapping = Some(mapping);
                    }
                } else if let Some(spec) = arg.strip_prefix("--groupmap=") {
                    if let Ok(mapping) = ::metadata::GroupMapping::parse(spec) {
                        config.group_mapping = Some(mapping);
                    }
                } else if arg == "--from0" {
                    // upstream: options.c:940 - --from0 sets NUL-delimited mode
                    // for --files-from content read from the protocol stream.
                    config.file_selection.from0 = true;
                // upstream: options.c:785,975 - --log-format is the deprecated
                // alias for --out-format. The server parses it to set
                // stdout_format_has_i (options.c:2345-2348): `%i` sets has_i = 1
                // (itemize significant items) and `%I` sets has_i = 2, the `-ii`
                // level that also itemizes unchanged entries. The client
                // forwards `--log-format=%i%I` for `-ii` (options.c:164-175).
                } else if let Some(fmt) = arg
                    .strip_prefix("--log-format=")
                    .or_else(|| arg.strip_prefix("--out-format="))
                {
                    if fmt.contains("%i") {
                        config.flags.info_flags.itemize = true;
                    }
                    if fmt.contains("%I") {
                        config.flags.info_flags.itemize_unchanged = true;
                    }
                } else if rejection.is_none() && is_client_only_flag_reaching_daemon(arg) {
                    // upstream: options.c:1460-1465 - the daemon's popt loop
                    // emits `rsync: <BAD>: <err> (in daemon mode)` on the
                    // first unrecognised option and jumps to `daemon_error:`
                    // (options.c:1480-1482), exiting `RERR_SYNTAX`. We mirror
                    // that fail-loud surface for batch-family flags that the
                    // client-side sanitiser should have stripped. Catching
                    // them here converts the previously silent connection
                    // close at protocol byte ~2241725 into an explicit
                    // `@ERROR` frame plus non-zero exit.
                    rejection = Some(ClientArgRejection::Unrecognized(arg.clone()));
                }
            }
        }
        i += 1;
    }

    rejection
}

/// Why a client argument aborts the session instead of being applied.
///
/// Upstream reaches these two outcomes through the same `parse_arguments()`
/// failure return, but by different routes and with different text, so the
/// daemon needs to tell them apart to report either one faithfully.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ClientArgRejection {
    /// A client-only flag (the write/read-batch family) reached the daemon.
    ///
    /// upstream: `options.c:1460-1465` - the daemon-mode popt loop emits
    /// `rsync: <BAD>: <err> (in daemon mode)` and jumps to `daemon_error:`
    /// (`options.c:1480-1482`), exiting `RERR_SYNTAX`.
    Unrecognized(String),
    /// A recognised option carried a value upstream's own parser rejects.
    ///
    /// The payload is the message verbatim, already in upstream's
    /// `--%s=%s is %s` shape (`options.c:1253`).
    InvalidValue(String),
}

/// Parses a `--max-size`/`--min-size` value the way upstream's popt case does.
///
/// upstream: `options.c:1808-1817` - both options call
/// `parse_size_arg(arg, 'b', "<name>", 0, -1, False)`, and a failure aborts
/// option parsing rather than falling back to a default. Ignoring a bad value
/// here would re-open the very hole this arm closes: the option would be
/// dropped, silently, on a peer-supplied argument.
fn parse_transfer_size_limit(opt_name: &str, value: &str) -> Result<u64, String> {
    // upstream: options.c:1172-1175 - the digit scan leaves the cursor on the
    // terminator, so the suffix switch takes `def_suf` and `strtod("")` gives
    // 0. An empty value is exactly `=0`, not "no limit"; the shared parser
    // rejects the empty string, so the rule is applied per option (the same
    // placement the CLI uses, since `--max-alloc` must keep rejecting it).
    let text = if value.is_empty() { "0" } else { value };

    // upstream: options.c:1169 + :1216-1221 - with max_value = -1 (what :1809
    // and :1815 pass) the ceiling is `(ssize_t)(SIZE_MAX / 2)`, and the range
    // check runs against the `double` returned by strtod with a STRICT
    // `dsize >= size_max` clause. `(double)(SIZE_MAX / 2)` rounds to 2^63, so
    // the boundary is 2^63 in DOUBLE space, not in integer space.
    //
    // Measured against real rsync 3.5.0: the largest accepted value is
    // 9223372036854774784 (2^63 - 1024, the greatest double below 2^63), while
    // 9223372036854775807 (`i64::MAX`) is already reported "too large".
    // Comparing as integers would accept a band of values upstream refuses,
    // so the comparison is kept in `f64` deliberately.
    const SIZE_MAX_AS_DOUBLE: f64 = (i64::MAX as u64 + 1) as f64;

    match ::core::bandwidth::parse_size_arg(text, b'b') {
        Ok(parsed) if parsed.bytes as f64 >= SIZE_MAX_AS_DOUBLE => {
            Err(size_arg_error(opt_name, value, "too large"))
        }
        Ok(parsed) => u64::try_from(parsed.bytes)
            .map_err(|_| size_arg_error(opt_name, value, "too large")),
        Err(::core::bandwidth::SizeArgError::Invalid) => {
            Err(size_arg_error(opt_name, value, "invalid"))
        }
        Err(::core::bandwidth::SizeArgError::TooLarge) => {
            Err(size_arg_error(opt_name, value, "too large"))
        }
    }
}

/// Renders upstream's size-argument failure text.
///
/// upstream: `options.c:1253` - `snprintf(err_buf, .., "--%s=%s is %s",
/// opt_name, size_arg, err)`. The `(max: N)` suffix upstream appends at
/// `:1254-1258` applies only when the option declares a bound; `--max-size`
/// and `--min-size` pass `max_value = -1`, so no suffix is emitted.
fn size_arg_error(opt_name: &str, value: &str, reason: &str) -> String {
    format!("--{opt_name}={value} is {reason}")
}

/// Reports whether `arg` is a client-only flag that should never reach the
/// daemon.
///
/// `--write-batch`, `--only-write-batch`, and `--read-batch` set up local
/// batch-file recording or replay on the CLIENT side only. Upstream
/// `options.c:server_options()` deliberately omits them from the argv sent
/// to the server; the only related token upstream emits is the literal
/// `--only-write-batch=X` placeholder at `options.c:2832-2833`, which
/// carries no real path. Encountering one here means the client-side
/// sanitiser failed - the previous behaviour was a silent connection close
/// in the middle of file-list framing. Surface this as a Rule-12 fail-loud
/// `@ERROR` instead.
///
/// Both bare-flag (`--write-batch`) and key=value (`--write-batch=PATH`)
/// forms are detected.
///
/// # Upstream Reference
///
/// - `options.c:784-786` - `read-batch`, `write-batch`, `only-write-batch`
///   popt entries (client-only)
/// - `options.c:1444-1449` - daemon-mode unknown option error path
fn is_client_only_flag_reaching_daemon(arg: &str) -> bool {
    let bare = arg.split('=').next().unwrap_or(arg);
    matches!(bare, "--write-batch" | "--only-write-batch" | "--read-batch")
}
