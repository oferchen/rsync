// Parsing of the long-form options upstream `server_options()` sends after the
// compact flag string, plus detection of client-only batch flags that must
// never reach the daemon.
/// Applies long-form arguments from the client to the server configuration.
///
/// Upstream rsync's `server_options()` (options.c:2765-3008) sends many options
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
/// - `options.c:1466-1471` - daemon-mode unknown option error path
/// - `options.c:2846-2857` - delete mode variants
/// - `options.c:2864-2865` - `--size-only`
/// - `options.c:2906-2907` - `--ignore-errors`
/// - `options.c:2916` - `--numeric-ids`
/// - `options.c:2919` - `--use-qsort`
/// - `options.c:2765-2768` - `--compress-level=N`
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
            // upstream: options.c:2846-2857 - delete mode variants
            "--delete" | "--delete-during" => {
                config.flags.delete = true;
            }
            // upstream: compat.c:174-176 - set_allow_inc_recurse() keys on
            // delete_before.
            "--delete-before" => {
                config.flags.delete = true;
                config.deletion.delete_before = true;
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
            // upstream: options.c:3024-3025 - `if (force_delete) args[ac++] =
            // "--force"`. It is the second term of `int del_opts = delete_mode
            // || force_delete ? DEL_RECURSE : 0` (generator.c:1629/2481), which
            // lets a POPULATED directory obstacle be cleared for an incoming
            // non-directory. The stdio server parser has its own arm
            // (`cli/src/frontend/server/flags.rs`); a daemon receiver reaches
            // this parser instead, so both have to set it.
            "--force" => {
                config.flags.force = true;
            }
            // upstream: options.c:2866-2867 - --stats sets do_stats which causes
            // INFO_STATS to level 2+. Without this flag, the generator does not
            // emit NDX_DEL_STATS during the goodbye phase and the client sender's
            // "Number of deleted files" line stays at zero on daemon uploads.
            "--stats" => {
                config.do_stats = true;
            }
            // upstream: options.c:2864-2865
            "--size-only" => {
                config.file_selection.size_only = true;
            }
            // upstream: options.c:2906-2907
            "--ignore-errors" => {
                config.deletion.ignore_errors = true;
            }
            // upstream: options.c:2909-2910
            "--copy-unsafe-links" => {
                config.flags.copy_unsafe_links = true;
            }
            // upstream: options.c:2912-2913
            "--safe-links" => {
                config.flags.safe_links = true;
            }
            // upstream: options.c:2915-2916 - an explicit client --numeric-ids
            // sets `numeric_ids = 1` (drops the wire name-list entirely).
            "--numeric-ids" => {
                config.flags.numeric_ids = core::server::NumericIds::Explicit;
            }
            // upstream: options.c:2986-2987 - `--no-implied-dirs` forwarded to
            // the sender on a pull. The daemon-sender must omit implied parent
            // dirs from the flist at protocol < 30 (flist.c:2708); protocol >= 30
            // always sends them (flist.c:2496-2497).
            "--no-implied-dirs" => {
                config.flags.no_implied_dirs = true;
            }
            // upstream: options.c:2918-2919
            "--use-qsort" => {
                config.qsort = true;
            }
            // upstream: options.c:2928-2929
            "--ignore-existing" => {
                config.file_selection.ignore_existing = true;
            }
            // upstream: options.c:2932-2933
            "--existing" => {
                config.file_selection.existing_only = true;
            }
            // upstream: options.c:2880-2881
            "--ignore-missing-args" => {
                config.file_selection.ignore_missing_args = true;
            }
            "--delete-missing-args" => {
                config.file_selection.delete_missing_args = true;
            }
            // upstream: options.c:2961-2970
            "--inplace" => {
                config.write.inplace = true;
            }
            // upstream: options.c:1728-1732 - OPT_APPEND increments append_mode
            // on the server side. A second `--append` (append_mode == 2) is the
            // wire encoding of `--append-verify`; the client never sends the
            // long-form `--append-verify` to a server.
            "--append" => {
                if config.flags.append {
                    config.flags.append_verify = true;
                }
                config.flags.append = true;
            }
            // upstream: options.c:2901-2902
            "--delay-updates" => {
                config.write.delay_updates = true;
            }
            // upstream: options.c:2940-2941
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
            // upstream: options.c:3006-3007 - --mkpath forwarded to the daemon
            // receiver on a push. Gates dest-arg path creation (main.c:751
            // make_path vs main.c:809 single do_mkdir).
            "--mkpath" => {
                config.flags.mkpath = true;
            }
            "--no-mkpath" => {
                config.flags.mkpath = false;
            }
            // upstream: options.c:2206-2208 - `--old-dirs`/`--old-d` set
            // xfer_dirs=4, resolved to recurse=1 plus an appended `- /*/*`
            // filter. server_options() never forwards these deprecated flags; a
            // client encodes them as `-r` in the compact flag string and sends
            // `- /*/*` over the wire filter list. Consumed here without mkpath
            // semantics so a stray forward is not mistaken for a positional path.
            "--old-dirs" | "--old-d" => {}
            // upstream: options.c:2924 - `if (list_only > 1) args[ac++] =
            // "--list-only"`, forwarded only when the operator asked for a
            // listing explicitly (options.c:809 stores 2, not 1). It reaches
            // whichever end the server plays: a daemon receiver renders the
            // file list without requesting any file, and a daemon sender's
            // flist build descends the named directories instead of listing
            // them as entries.
            "--list-only" => {
                config.flags.list_only = true;
            }
            // upstream: options.c:2927-2929 - a client running `-d --delete`
            // emits `--no-r` so a remote that only got `-d` may still delete.
            // options.c:632 clears the same `recurse` global the compact `r`
            // letter sets, so the negation has to be applied after the flag
            // string has been parsed.
            "--no-r" => {
                config.flags.recursive = false;
            }
            // upstream: options.c:2936-2942 - `preserve_specials` never rides
            // the compact flag string, because `-D` covers devices only. The
            // long form carries it instead: `--no-specials` when devices are
            // preserved but specials are not, `--specials` when specials are
            // preserved without devices (options.c:686-687).
            "--specials" => {
                config.flags.specials = true;
            }
            "--no-specials" => {
                config.flags.specials = false;
            }
            // upstream: options.c:2958-2961 - `--msgs2stderr` /
            // `--no-msgs2stderr` (options.c:619-620) tell the peer where to
            // route its own name and info output.
            "--msgs2stderr" => {
                config.flags.msgs_to_stderr = true;
            }
            "--no-msgs2stderr" => {
                config.flags.msgs_to_stderr = false;
            }
            // upstream: options.c:3069-3070 - `else if (keep_partial &&
            // am_sender)` emits the bare `--partial` (options.c:788) to a
            // server receiver, which then keeps a partially transferred file
            // instead of unlinking it.
            "--partial" => {
                config.flags.partial = true;
            }
            // upstream: options.c:3139-3140 - an `--inplace --sparse` sender
            // emits `--no-W` so the receiver still asks for a delta rather
            // than the whole file. options.c:760 clears the same `whole_file`
            // global the compact `W` letter sets.
            "--no-W" => {
                config.flags.whole_file = false;
            }
            // upstream: options.c:3153-3154 - a `--files-from` transfer with
            // relative paths off emits the long `--no-relative` in place of
            // the compact `R`. options.c:707-708 spell it both ways.
            "--no-relative" | "--no-R" => {
                config.flags.relative = false;
            }
            // upstream: options.c:3163-3166 - `--remove-source-files`, and the
            // deprecated `--remove-sent-files` alias (options.c:744-745), tell
            // a server sender to unlink each source file once the receiver has
            // acknowledged it. Dropping it left the sources in place on every
            // daemon pull that asked for them to be moved.
            "--remove-source-files" | "--remove-sent-files" => {
                config.flags.remove_source_files = true;
            }
            // upstream: options.c:3171-3172 - `if (preallocate_files &&
            // am_sender)` forwards `--preallocate` (options.c:729) to a server
            // receiver so it fallocate()s each destination file before writing.
            "--preallocate" => {
                config.flags.preallocate = true;
            }
            // upstream: options.c:3174-3175 - `--open-noatime` (options.c:658)
            // is forwarded so the server sender opens source files with
            // O_NOATIME and leaves their access times untouched.
            "--open-noatime" => {
                config.write.open_noatime = true;
            }
            // upstream: options.c:2859 - backup
            "--backup" => {
                config.flags.backup = true;
            }
            // Two-arg options: upstream sends option and value as separate args.
            // upstream: options.c:2943-2951 - reference directories
            "--compare-dest" => {
                if let Some(dir) = client_args.get(i + 1) {
                    config.reference_directories.push(ReferenceDirectory::new(
                        ReferenceDirectoryKind::Compare,
                        std::path::PathBuf::from(dir),
                    ));
                    i += 1;
                }
            }
            "--copy-dest" => {
                if let Some(dir) = client_args.get(i + 1) {
                    config.reference_directories.push(ReferenceDirectory::new(
                        ReferenceDirectoryKind::Copy,
                        std::path::PathBuf::from(dir),
                    ));
                    i += 1;
                }
            }
            "--link-dest" => {
                if let Some(dir) = client_args.get(i + 1) {
                    config.reference_directories.push(ReferenceDirectory::new(
                        ReferenceDirectoryKind::Link,
                        std::path::PathBuf::from(dir),
                    ));
                    i += 1;
                }
            }
            // upstream: options.c:2815-2818 - backup-dir as separate args
            "--backup-dir" => {
                config.flags.backup = true;
                if let Some(dir) = client_args.get(i + 1) {
                    config.backup_dir = Some(dir.to_owned());
                    i += 1;
                }
            }
            // upstream: options.c:2819-2821 - suffix as separate args
            // When --backup-dir is specified without explicit --suffix,
            // upstream changes the default suffix from "~" to "" and sends
            // --suffix as a two-arg form (not --suffix=VALUE).
            "--suffix" | "--backup-suffix" => {
                if let Some(suffix) = client_args.get(i + 1) {
                    config.backup_suffix = Some(suffix.to_owned());
                    i += 1;
                }
            }
            // upstream: options.c:2935-2937 - temp-dir as separate args
            "--temp-dir" => {
                if let Some(dir) = client_args.get(i + 1) {
                    config.temp_dir = Some(std::path::PathBuf::from(dir));
                    i += 1;
                }
            }
            // upstream: options.c:3062-3066 - `if (partial_dir && am_sender)`
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
            // upstream: options.c:2828-2833 - --compress-choice, --new-compress, --old-compress
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
                // upstream: options.c:2828-2833 - --compress-choice=ALGO
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
                // upstream: options.c:2765-2768
                } else if let Some(level_str) = arg.strip_prefix("--compress-level=") {
                    if let Ok(level) = level_str.parse::<u32>()
                        && let Ok(cl) = compress::zlib::CompressionLevel::from_numeric(level)
                    {
                        config.connection.compression_level = Some(cl);
                    }
                // upstream: options.c:2835-2838
                } else if let Some(val) = arg.strip_prefix("--max-delete=") {
                    if let Ok(n) = val.parse::<i64>()
                        && n >= 0
                    {
                        config.deletion.max_delete = Some(n as u64);
                    }
                // upstream: options.c:3008-3011 - `server_options()` forwards
                // `--min-size`/`--max-size` (as one `--opt=VALUE` token, see
                // safe_arg at options.c:2726-2730) only when the local end is
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
                // upstream: options.c:2071-2076 - `server_options()` forwards
                // `--max-alloc` (options.c:3039-3040) and the daemon runs the
                // SAME `parse_arguments()` block the client does, so a peer
                // value is both applied and refused there. The daemon decodes
                // the client argv here rather than through the `--server` argv
                // parser, so it needs its own arm; both call the one shared
                // rule in `protocol::max_alloc`.
                //
                // Without this arm the option fell off the end of the chain and
                // was SILENTLY IGNORED, which is exactly the shape 3.5.0's zero
                // refusal exists to close: an older client that predates the
                // check still forwards `--max-alloc=0` on the wire.
                } else if let Some(val) = arg.strip_prefix("--max-alloc=") {
                    match parse_max_alloc_limit(val) {
                        // upstream: util2.c:75 - the rewritten `max_alloc`
                        // global is what my_alloc() consults, so applying it is
                        // the whole point of parsing it.
                        Ok(limit) => ::protocol::set_max_alloc(limit),
                        Err(message) => {
                            rejection.get_or_insert(ClientArgRejection::InvalidValue(message));
                        }
                    }
                // upstream: options.c - server_options() forwards `--modify-window=NUM`.
                // The daemon receiver's quick-check honours it via same_time() so
                // files within the window are not needlessly re-transferred.
                } else if let Some(val) = arg.strip_prefix("--modify-window=") {
                    if let Ok(n) = val.trim_start_matches('+').parse::<i64>() {
                        config.file_selection.modify_window =
                            ::metadata::ModifyWindow::from_secs(n);
                    }
                // upstream: options.c:2963-2964 - the client forwards the block
                // size as a standalone `-B%u` token, and options.c:1801-1811
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
                // upstream: options.c:2884 - a negative modify_window is
                // forwarded via the short `-@%d` spelling (e.g. `-@-1`) for
                // nanosecond-exact comparison (util1.c:1577).
                } else if let Some(val) = arg.strip_prefix("-@") {
                    if let Ok(n) = val.parse::<i64>() {
                        config.file_selection.modify_window =
                            ::metadata::ModifyWindow::from_secs(n);
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
                    config.reference_directories.push(ReferenceDirectory::new(
                        ReferenceDirectoryKind::Link,
                        std::path::PathBuf::from(dir),
                    ));
                } else if let Some(dir) = arg.strip_prefix("--compare-dest=") {
                    config.reference_directories.push(ReferenceDirectory::new(
                        ReferenceDirectoryKind::Compare,
                        std::path::PathBuf::from(dir),
                    ));
                } else if let Some(dir) = arg.strip_prefix("--copy-dest=") {
                    config.reference_directories.push(ReferenceDirectory::new(
                        ReferenceDirectoryKind::Copy,
                        std::path::PathBuf::from(dir),
                    ));
                } else if let Some(dir) = arg.strip_prefix("--temp-dir=") {
                    config.temp_dir = Some(std::path::PathBuf::from(dir));
                } else if let Some(dir) = arg.strip_prefix("--partial-dir=") {
                    config.partial_dir = Some(std::path::PathBuf::from(dir));
                    config.has_partial_dir = true;
                } else if let Some(path) = arg.strip_prefix("--files-from=") {
                    config.file_selection.files_from_path = Some(path.to_owned());
                // upstream: options.c:2922 / 2915 - --usermap=SPEC / --groupmap=SPEC.
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
                // upstream: options.c:2991-2992 - `safe_arg("--checksum-choice",
                // checksum_choice)` forwards the raw spec, and compat.c:544
                // then makes it load-bearing on the WIRE: the checksum vstring
                // is sent only `if (!checksum_choice)`. A daemon that drops the
                // option sends a list its peer never reads, so the exchange
                // desyncs - the failure `negotiate.rs:335-343` already warns
                // about for the reverse direction.
                } else if let Some(spec) = arg.strip_prefix("--checksum-choice=") {
                    match parse_transfer_checksum_choice(spec) {
                        Ok(algorithm) => config.checksum_choice = Some(algorithm),
                        Err(message) => {
                            rejection.get_or_insert(ClientArgRejection::InvalidValue(message));
                        }
                    }
                // upstream: options.c:3056-3059 - `--checksum-seed=%d` is
                // forwarded from an `int` global (options.c:861 is POPT_ARG_INT),
                // and compat.c:823-825 has the SERVER pick the seed and write it
                // on the wire. Dropping the value made `--checksum-seed` a no-op
                // over a daemon while it worked over a remote shell.
                //
                // Parsed as `i32`, because that is the width upstream prints
                // from and the width `write_seed` puts back on the wire; a
                // `u32`-only parse would reject the negative values upstream's
                // `%d` can emit.
                } else if let Some(seed) = arg.strip_prefix("--checksum-seed=") {
                    if let Ok(value) = seed.trim().parse::<i32>() {
                        config.checksum_seed = Some(value);
                    }
                } else if arg == "--from0" {
                    // upstream: options.c:940 - --from0 sets NUL-delimited mode
                    // for --files-from content read from the protocol stream.
                    config.file_selection.from0 = true;
                // upstream: options.c:785,975 - --log-format is the deprecated
                // alias for --out-format. The server parses it to set
                // stdout_format_has_i (options.c:2354-2357): `%i` sets has_i = 1
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
                // `--only-write-batch` is NOT client-only: it rides the same
                // popt table the daemon runs (`options.c:812` -
                // `{"only-write-batch", 0, POPT_ARG_STRING, &batch_name,
                // OPT_ONLY_WRITE_BATCH, ...}`, whose case at
                // `options.c:1785-1787` sets `write_batch = -1`), and a
                // conforming client emits it at `options.c:3026-3027`
                // (`if (write_batch < 0) args[ac++] = "--only-write-batch=X"`).
                // The server-side reset at `options.c:2270-2282` that prints
                // "ignoring --write-batch option sent to server" fires only for
                // `write_batch > 0 || read_batch`, so `write_batch = -1`
                // survives on the daemon. Refusing it turned away upstream
                // 3.5.0 with `@ERROR: unrecognized option (in daemon mode)`.
                //
                // The value is upstream's literal placeholder `X`, never a real
                // path: `main.c:1939` gates `open_batch_files()` on `!am_server`,
                // so the daemon never opens a batch file. What it must take from
                // the flag is the mode switch - `clientserver.c:1195`
                // `if (write_batch < 0) dry_run = 1` - and the receiver body at
                // `receiver.c:1003-1009`, which logs the item and writes nothing
                // while the generator still sends real block checksums.
                //
                // Gated on the receiver role because `server_options()` emits the
                // token only inside its `am_sender` block, so a daemon serving a
                // pull never sees it. This mirrors the sibling `--server` argv
                // parser (`cli/src/frontend/server/run.rs`).
                } else if arg
                    .split('=')
                    .next()
                    .is_some_and(|name| name == "--only-write-batch")
                {
                    if config.role == ServerRole::Receiver {
                        config.flags.only_write_batch = true;
                        config.flags.dry_run = true;
                    }
                } else if rejection.is_none() && is_client_only_flag_reaching_daemon(arg) {
                    // upstream: options.c:1466-1471 - the daemon's popt loop
                    // emits `rsync: <BAD>: <err> (in daemon mode)` on the
                    // first unrecognised option and jumps to `daemon_error:`
                    // (options.c:1486-1488), exiting `RERR_SYNTAX`. We mirror
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
    /// upstream: `options.c:1466-1471` - the daemon-mode popt loop emits
    /// `rsync: <BAD>: <err> (in daemon mode)` and jumps to `daemon_error:`
    /// (`options.c:1486-1488`), exiting `RERR_SYNTAX`.
    Unrecognized(String),
    /// A recognised option carried a value upstream's own parser rejects.
    ///
    /// The payload is the message verbatim, already in upstream's
    /// `--%s=%s is %s` shape (`options.c:1259`).
    InvalidValue(String),
}

/// Resolves the TRANSFER half of a `--checksum-choice=SPEC` value.
///
/// upstream: `checksum.c:196-202 parse_checksum_choice()` - the spec is split on
/// its first `,`; the part before names the transfer sum and the part after
/// names the whole-file sum, and without a comma both take the same name. Only
/// the transfer sum reaches the wire negotiation, which is the single thing
/// `ServerConfig::checksum_choice` feeds, so the file half is left to the client
/// that chose it.
///
/// upstream: `checksum.c:127-160 parse_csum_name()` - a missing name or the
/// case-insensitive literal `auto` resolves to md5 rather than being an error,
/// and any other unknown name aborts with `RERR_UNSUPPORTED`. oc's client
/// resolves `auto` the same way before forwarding
/// (`core::client::config::enums::checksum::transfer_protocol_override`), so
/// both ends land on the same algorithm and skip the vstring exchange together.
fn parse_transfer_checksum_choice(spec: &str) -> Result<::protocol::ChecksumAlgorithm, String> {
    let transfer = spec.split(',').next().unwrap_or(spec);
    if transfer.is_empty() || transfer.eq_ignore_ascii_case("auto") {
        return Ok(::protocol::ChecksumAlgorithm::MD5);
    }
    ::protocol::ChecksumAlgorithm::parse(transfer)
        // upstream: checksum.c:156 - `"unknown checksum name: %s\n"`.
        .map_err(|_| format!("unknown checksum name: {transfer}"))
}

/// Parses a `--max-size`/`--min-size` value the way upstream's popt case does.
///
/// upstream: `options.c:1814-1823` - both options call
/// `parse_size_arg(arg, 'b', "<name>", 0, -1, False)`, and a failure aborts
/// option parsing rather than falling back to a default. Ignoring a bad value
/// here would re-open the very hole this arm closes: the option would be
/// dropped, silently, on a peer-supplied argument.
fn parse_transfer_size_limit(opt_name: &str, value: &str) -> Result<u64, String> {
    // upstream: options.c:1178-1181 - the digit scan leaves the cursor on the
    // terminator, so the suffix switch takes `def_suf` and `strtod("")` gives
    // 0. An empty value is exactly `=0`, not "no limit"; the shared parser
    // rejects the empty string, so the rule is applied per option (the same
    // placement the CLI uses, since `--max-alloc` must keep rejecting it).
    let text = if value.is_empty() { "0" } else { value };

    // upstream: options.c:1175 + :1216-1221 - with max_value = -1 (what :1809
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
        Ok(parsed) => {
            u64::try_from(parsed.bytes).map_err(|_| size_arg_error(opt_name, value, "too large"))
        }
        Err(::core::bandwidth::SizeArgError::Invalid) => {
            Err(size_arg_error(opt_name, value, "invalid"))
        }
        Err(::core::bandwidth::SizeArgError::TooLarge) => {
            Err(size_arg_error(opt_name, value, "too large"))
        }
    }
}

/// Parses a peer-forwarded `--max-alloc` value into a byte ceiling.
///
/// upstream: `options.c:2072-2076` - `parse_size_arg(max_alloc_arg, 'B',
/// "max-alloc", 1024*1024, -1, True)` followed by the zero refusal. The value
/// RULES live in `protocol::max_alloc` because the client's own CLI applies
/// exactly the same ones; only the front-end parse is restated here, since this
/// parser reads raw wire strings rather than an `OsStr` argv.
fn parse_max_alloc_limit(value: &str) -> Result<usize, String> {
    // upstream: options.c:1178-1181 - the digit scan leaves the cursor on the
    // terminator and `strtod("")` gives 0, so an empty value is exactly `=0`.
    // For `--max-alloc` that resolves to the zero refusal rather than a parse
    // error, which is what upstream reports for a forwarded `--max-alloc=`.
    let text = if value.is_empty() { "0" } else { value };

    // upstream: options.c:2073 passes def_suf 'B'.
    let bytes = match ::core::bandwidth::parse_size_arg(text, b'B') {
        Ok(parsed) => u64::try_from(parsed.bytes).unwrap_or(u64::MAX),
        Err(::core::bandwidth::SizeArgError::Invalid) => {
            return Err(size_arg_error("max-alloc", value, "invalid"));
        }
        Err(::core::bandwidth::SizeArgError::TooLarge) => {
            return Err(size_arg_error("max-alloc", value, "too large"));
        }
    };

    let limit = ::protocol::max_alloc::validate_max_alloc(bytes, value)?;
    usize::try_from(limit).map_err(|_| size_arg_error("max-alloc", value, "too large"))
}

/// Renders upstream's size-argument failure text.
///
/// upstream: `options.c:1259` - `snprintf(err_buf, .., "--%s=%s is %s",
/// opt_name, size_arg, err)`. The `(max: N)` suffix upstream appends at
/// `:1254-1258` applies only when the option declares a bound; `--max-size`
/// and `--min-size` pass `max_value = -1`, so no suffix is emitted.
fn size_arg_error(opt_name: &str, value: &str, reason: &str) -> String {
    format!("--{opt_name}={value} is {reason}")
}

/// Reports whether `arg` is a client-only flag that should never reach the
/// daemon.
///
/// `--write-batch` and `--read-batch` set up local batch-file recording or
/// replay on the CLIENT side only. Upstream `options.c:server_options()`
/// deliberately omits them from the argv sent to the server. Encountering one
/// here means the client-side sanitiser failed - the previous behaviour was a
/// silent connection close in the middle of file-list framing. Surface this as
/// a Rule-12 fail-loud `@ERROR` instead.
///
/// `--only-write-batch` is deliberately NOT in this set: upstream emits it to
/// the server on purpose (`options.c:3026-3027`) and keeps `write_batch = -1`
/// there, so it is handled as a real option by the caller.
///
/// Both bare-flag (`--write-batch`) and key=value (`--write-batch=PATH`)
/// forms are detected.
///
/// # Upstream Reference
///
/// - `options.c:810-811` - `read-batch` and `write-batch` popt entries
/// - `options.c:2270-2282` - the server-side reset that fires for
///   `write_batch > 0 || read_batch`
/// - `options.c:1450-1455` - daemon-mode unknown option error path
fn is_client_only_flag_reaching_daemon(arg: &str) -> bool {
    let bare = arg.split('=').next().unwrap_or(arg);
    matches!(bare, "--write-batch" | "--read-batch")
}
