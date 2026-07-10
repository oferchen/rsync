// Parsing of the long-form options upstream `server_options()` sends after the
// compact flag string, plus detection of client-only batch flags that must
// never reach the daemon.
/// Applies long-form arguments from the client to the server configuration.
///
/// Upstream rsync's `server_options()` (options.c:2737-2980) sends many options
/// as long-form arguments that are not encoded in the compact flag string.
/// The daemon must parse these to correctly configure the transfer.
///
/// Returns `Some(offender)` when a client-only flag (write/read-batch family)
/// reaches the daemon. The caller surfaces that as an `@ERROR` and exits
/// instead of letting the unrecognised option drive a silent connection
/// close mid file-list framing. Upstream mirrors this at
/// `options.c:1444-1449` with `rsync: <BAD>: <err> (in daemon mode)` then
/// `daemon_error:` (`options.c:1464-1466`) exiting `RERR_SYNTAX`.
///
/// # Upstream Reference
///
/// - `options.c:1444-1449` - daemon-mode unknown option error path
/// - `options.c:2818-2829` - delete mode variants
/// - `options.c:2836-2837` - `--size-only`
/// - `options.c:2878-2879` - `--ignore-errors`
/// - `options.c:2888` - `--numeric-ids`
/// - `options.c:2891` - `--use-qsort`
/// - `options.c:2737-2740` - `--compress-level=N`
fn apply_long_form_args(client_args: &[String], config: &mut ServerConfig) -> Option<String> {
    // Positional path args follow the standalone `.` separator. Upstream
    // `glob_expand_module()` consumes them through a different code path, so
    // the daemon's option parser only validates the option region.
    let dot_position = client_args.iter().position(|a| a == ".");

    let mut unknown: Option<String> = None;
    let mut i = 0;
    while i < client_args.len() {
        let arg = &client_args[i];
        if dot_position.is_some_and(|dot| i >= dot) {
            i += 1;
            continue;
        }
        match arg.as_str() {
            // upstream: options.c:2818-2829 - delete mode variants
            "--delete" | "--delete-before" | "--delete-during" => {
                config.flags.delete = true;
            }
            "--delete-after" | "--delete-delay" => {
                config.flags.delete = true;
                config.deletion.late_delete = true;
            }
            "--delete-excluded" => {
                config.flags.delete = true;
            }
            // upstream: options.c:2838-2839 - --stats sets do_stats which causes
            // INFO_STATS to level 2+. Without this flag, the generator does not
            // emit NDX_DEL_STATS during the goodbye phase and the client sender's
            // "Number of deleted files" line stays at zero on daemon uploads.
            "--stats" => {
                config.do_stats = true;
            }
            // upstream: options.c:2836-2837
            "--size-only" => {
                config.file_selection.size_only = true;
            }
            // upstream: options.c:2878-2879
            "--ignore-errors" => {
                config.deletion.ignore_errors = true;
            }
            // upstream: options.c:2881-2882
            "--copy-unsafe-links" => {
                config.flags.copy_unsafe_links = true;
            }
            // upstream: options.c:2884-2885
            "--safe-links" => {
                config.flags.safe_links = true;
            }
            // upstream: options.c:2887-2888
            "--numeric-ids" => {
                config.flags.numeric_ids = true;
            }
            // upstream: options.c:2890-2891
            "--use-qsort" => {
                config.qsort = true;
            }
            // upstream: options.c:2893-2897
            "--ignore-existing" => {
                config.file_selection.ignore_existing = true;
            }
            // upstream: options.c:2899-2900
            "--existing" => {
                config.file_selection.existing_only = true;
            }
            // upstream: options.c:817-818
            "--ignore-missing-args" => {
                config.file_selection.ignore_missing_args = true;
            }
            "--delete-missing-args" => {
                config.file_selection.delete_missing_args = true;
            }
            // upstream: options.c:2933-2942
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
            // upstream: options.c:2934-2935
            "--delay-updates" => {
                config.write.delay_updates = true;
            }
            // upstream: options.c:2964-2965
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
            // receiver on a push. Gates dest-arg path creation (main.c:736
            // make_path vs main.c:788 single do_mkdir).
            "--mkpath" => {
                config.flags.mkpath = true;
            }
            "--no-mkpath" | "--old-dirs" => {
                config.flags.mkpath = false;
            }
            // upstream: options.c:2849 - backup
            "--backup" => {
                config.flags.backup = true;
            }
            // Two-arg options: upstream sends option and value as separate args.
            // upstream: options.c:2915-2923 - reference directories
            "--compare-dest" => {
                if let Some(dir) = client_args.get(i + 1) {
                    config.reference_directories.push(ReferenceDirectory {
                        path: std::path::PathBuf::from(dir),
                        kind: ReferenceDirectoryKind::Compare,
                    });
                    i += 1;
                }
            }
            "--copy-dest" => {
                if let Some(dir) = client_args.get(i + 1) {
                    config.reference_directories.push(ReferenceDirectory {
                        path: std::path::PathBuf::from(dir),
                        kind: ReferenceDirectoryKind::Copy,
                    });
                    i += 1;
                }
            }
            "--link-dest" => {
                if let Some(dir) = client_args.get(i + 1) {
                    config.reference_directories.push(ReferenceDirectory {
                        path: std::path::PathBuf::from(dir),
                        kind: ReferenceDirectoryKind::Link,
                    });
                    i += 1;
                }
            }
            // upstream: options.c:2787-2790 - backup-dir as separate args
            "--backup-dir" => {
                config.flags.backup = true;
                if let Some(dir) = client_args.get(i + 1) {
                    config.backup_dir = Some(dir.to_owned());
                    i += 1;
                }
            }
            // upstream: options.c:2791-2793 - suffix as separate args
            // When --backup-dir is specified without explicit --suffix,
            // upstream changes the default suffix from "~" to "" and sends
            // --suffix as a two-arg form (not --suffix=VALUE).
            "--suffix" | "--backup-suffix" => {
                if let Some(suffix) = client_args.get(i + 1) {
                    config.backup_suffix = Some(suffix.to_owned());
                    i += 1;
                }
            }
            // upstream: options.c:2907-2909 - temp-dir as separate args
            "--temp-dir" => {
                if let Some(dir) = client_args.get(i + 1) {
                    config.temp_dir = Some(std::path::PathBuf::from(dir));
                    i += 1;
                }
            }
            // upstream: options.c:2800-2805 - --compress-choice, --new-compress, --old-compress
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
                // upstream: options.c:2800-2805 - --compress-choice=ALGO
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
                // upstream: options.c:2737-2740
                } else if let Some(level_str) = arg.strip_prefix("--compress-level=") {
                    if let Ok(level) = level_str.parse::<u32>() {
                        if let Ok(cl) = compress::zlib::CompressionLevel::from_numeric(level) {
                            config.connection.compression_level = Some(cl);
                        }
                    }
                // upstream: options.c:2807-2810
                } else if let Some(val) = arg.strip_prefix("--max-delete=") {
                    if let Ok(n) = val.parse::<i64>() {
                        if n >= 0 {
                            config.deletion.max_delete = Some(n as u64);
                        }
                    }
                // upstream: options.c - server_options() forwards `--modify-window=NUM`.
                // The daemon receiver's quick-check honours it via same_time() so
                // files within the window are not needlessly re-transferred.
                } else if let Some(val) = arg.strip_prefix("--modify-window=") {
                    if let Ok(n) = val.trim_start_matches('+').parse::<u64>() {
                        config.file_selection.modify_window = n;
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
                    config.reference_directories.push(ReferenceDirectory {
                        path: std::path::PathBuf::from(dir),
                        kind: ReferenceDirectoryKind::Link,
                    });
                } else if let Some(dir) = arg.strip_prefix("--compare-dest=") {
                    config.reference_directories.push(ReferenceDirectory {
                        path: std::path::PathBuf::from(dir),
                        kind: ReferenceDirectoryKind::Compare,
                    });
                } else if let Some(dir) = arg.strip_prefix("--copy-dest=") {
                    config.reference_directories.push(ReferenceDirectory {
                        path: std::path::PathBuf::from(dir),
                        kind: ReferenceDirectoryKind::Copy,
                    });
                } else if let Some(dir) = arg.strip_prefix("--temp-dir=") {
                    config.temp_dir = Some(std::path::PathBuf::from(dir));
                } else if let Some(path) = arg.strip_prefix("--files-from=") {
                    config.file_selection.files_from_path = Some(path.to_owned());
                // upstream: options.c:2904 / 2907 - --usermap=SPEC / --groupmap=SPEC.
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
                // upstream: options.c:773,963 - --log-format is the deprecated
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
                } else if unknown.is_none() && is_client_only_flag_reaching_daemon(arg) {
                    // upstream: options.c:1444-1449 - the daemon's popt loop
                    // emits `rsync: <BAD>: <err> (in daemon mode)` on the
                    // first unrecognised option and jumps to `daemon_error:`
                    // (options.c:1464-1466), exiting `RERR_SYNTAX`. We mirror
                    // that fail-loud surface for batch-family flags that the
                    // client-side sanitiser should have stripped. Catching
                    // them here converts the previously silent connection
                    // close at protocol byte ~2241725 into an explicit
                    // `@ERROR` frame plus non-zero exit.
                    unknown = Some(arg.clone());
                }
            }
        }
        i += 1;
    }

    unknown
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
