//! Top-level [`parse_args`] entry point.
//!
//! Translates `clap` matches into the strongly-typed [`ParsedArgs`] struct by
//! orchestrating the focused flag, value, coercion, and copy-on-write helpers
//! in the sibling modules.

use std::env;
use std::ffi::OsString;
use std::path::PathBuf;

use compress::algorithm::CompressionAlgorithm;

use crate::frontend::arguments::short_options::{
    expand_short_options, hoist_options_before_operands,
};
use crate::frontend::command_builder::clap_command;
use crate::frontend::execution::{parse_checksum_seed_argument, parse_compress_level_argument};
use crate::frontend::filter_rules::{FilterOrderToken, build_filter_order};
use crate::frontend::progress::{NameOutputLevel, ProgressSetting};
use core::client::{
    AddressMode, DeleteMode, HumanReadableMode, StrongChecksumChoice, TcpFastOpenMode,
};

use super::coerce::{parse_checksum_threads, parse_spill_threshold_bytes, parse_thread_count};
use super::cow::{last_occurrence, parse_reflink_mode, resolve_cow_policy};
use super::flags::{tri_state_flag_negative_first, tri_state_flag_positive_first};
use super::values::join_os_values;
use super::{BandwidthArgument, ParsedArgs, detect_program_name, env_protect_args_default};

/// Parses command-line arguments into a structured [`ParsedArgs`] representation.
///
/// This function accepts an iterator of arguments (typically from `std::env::args_os()`)
/// and returns a parsed structure or a Clap error if parsing fails.
///
/// **Warning**: This function is exposed via `cli::test_utils` for integration
/// tests only. It is not part of the stable public API.
pub fn parse_args<I, S>(arguments: I) -> Result<ParsedArgs, clap::Error>
where
    I: IntoIterator<Item = S>,
    S: Into<OsString>,
{
    let mut args: Vec<OsString> = arguments.into_iter().map(Into::into).collect();

    let program_name = detect_program_name(args.first().map(OsString::as_os_str));

    if args.is_empty() {
        args.push(OsString::from(program_name.as_str()));
    }

    let command = clap_command(program_name.as_str());
    let args = hoist_options_before_operands(&command, args);
    let args = expand_short_options(&command, args);
    let mut matches = command.try_get_matches_from(args.clone())?;

    let show_help = matches.get_flag("help");
    let show_version = matches.get_count("version");
    let show_io_uring_status = matches.get_flag("io-uring-status");
    let show_lsm_status = matches.get_flag("lsm-status");

    // Handle human-readable: `-h`/`--human-readable` take no argument and are
    // repeatable, so we count occurrences. upstream: options.c:111 defaults
    // `human_readable` to 1 (rendered as the None case here), options.c:1573
    // increments it per -h, and options.c:617 resets it to 0 for --no-h. A
    // single -h selects base-1000 units; -hh (or more) selects base-1024.
    let mut human_readable = None;
    let h_count = matches.get_count("human-readable");
    if h_count > 0 {
        human_readable = Some(if h_count == 1 {
            HumanReadableMode::DecimalUnits
        } else {
            HumanReadableMode::BinaryUnits
        });
    }

    if matches.get_flag("no-human-readable") {
        human_readable = Some(HumanReadableMode::Raw);
    }
    let dry_run = matches.get_flag("dry-run");
    let list_only = matches.get_flag("list-only");
    let mkpath = tri_state_flag_positive_first(&matches, "mkpath", "no-mkpath").unwrap_or(false);
    let prune_empty_dirs =
        tri_state_flag_negative_first(&matches, "prune-empty-dirs", "no-prune-empty-dirs");
    let omit_link_times =
        tri_state_flag_negative_first(&matches, "omit-link-times", "no-omit-link-times");
    let atimes = tri_state_flag_negative_first(&matches, "atimes", "no-atimes");
    let crtimes = tri_state_flag_negative_first(&matches, "crtimes", "no-crtimes");
    // upstream: options.c:2366-2367 - only `dry_run` sets `do_xfers = 0` (and
    // thus the compact `n` letter); `list_only` does NOT (options.c:2634 "Note:
    // NOT dry_run!"). The receiver skips destination writes under `list_only`
    // independently (see `run_client` mode selection and
    // `TransferFlags::skip_dest_writes`), so we must not conflate the two here.
    let remote_shell = matches
        .remove_one::<OsString>("rsh")
        .filter(|value| !value.is_empty())
        .or_else(|| env::var_os("RSYNC_RSH").filter(|value| !value.is_empty()));
    let rsync_path = matches
        .remove_one::<OsString>("rsync-path")
        .filter(|value| !value.is_empty());
    let connect_program = matches
        .remove_one::<OsString>("connect-program")
        .filter(|value| !value.is_empty());
    let daemon_port = matches.remove_one::<u16>("port");
    let remote_options = matches
        .remove_many::<OsString>("remote-option")
        .map(Iterator::collect)
        .unwrap_or_default();
    let protect_args = if matches.get_flag("no-protect-args") {
        Some(false)
    } else if matches.get_flag("protect-args") {
        Some(true)
    } else {
        env_protect_args_default()
    };
    let old_args = tri_state_flag_negative_first(&matches, "old-args", "no-old-args");
    let address_mode = if matches.get_flag("ipv4") {
        AddressMode::Ipv4
    } else if matches.get_flag("ipv6") {
        AddressMode::Ipv6
    } else {
        AddressMode::Default
    };
    let bind_address_raw = matches.remove_one::<OsString>("address");
    let sockopts = matches.remove_one::<OsString>("sockopts");
    let tcp_fastopen = match matches.remove_one::<OsString>("tcp-fastopen") {
        Some(value) => value
            .to_string_lossy()
            .parse::<TcpFastOpenMode>()
            .map_err(|error| {
                clap::Error::raw(
                    clap::error::ErrorKind::ValueValidation,
                    format!("{error}\n"),
                )
            })?,
        None => TcpFastOpenMode::default(),
    };
    let blocking_io = tri_state_flag_positive_first(&matches, "blocking-io", "no-blocking-io");
    let archive = matches.get_flag("archive");
    // upstream: options.c:631-632 - `--old-dirs`/`--old-d` set xfer_dirs=4, and
    // options.c:2197-2199 resolves that to `recurse = xfer_dirs = 1`
    // unconditionally (after the argv scan), so it forces recursion on even over
    // a `--no-recursive`, and appends the `- /*/*` filter rule (injected below).
    let old_dirs = matches.get_flag("old-dirs");
    let recursive_override = tri_state_flag_negative_first(&matches, "recursive", "no-recursive");
    let recursive = if old_dirs {
        true
    } else if recursive_override == Some(false) {
        false
    } else if archive {
        true
    } else {
        recursive_override.unwrap_or(false)
    };
    let inc_recursive =
        tri_state_flag_positive_first(&matches, "inc-recursive", "no-inc-recursive");
    let dirs = tri_state_flag_negative_first(&matches, "dirs", "no-dirs");
    let delete_flag = matches.get_flag("delete");
    let delete_before_flag = matches.get_flag("delete-before");
    let delete_during_flag = matches.get_flag("delete-during");
    let delete_delay_flag = matches.get_flag("delete-delay");
    let delete_after_flag = matches.get_flag("delete-after");
    let mut ignore_missing_args = matches.get_flag("ignore-missing-args");
    let delete_missing_args = matches.get_flag("delete-missing-args");
    if delete_missing_args {
        ignore_missing_args = true;
    }
    let delete_excluded = matches.get_flag("delete-excluded");
    let ignore_errors =
        tri_state_flag_negative_first(&matches, "ignore-errors", "no-ignore-errors");
    let max_delete = match matches.remove_one::<OsString>("max-delete") {
        Some(value) => {
            let s = value.to_string_lossy();
            // upstream allows -1 to mean "no limit after reporting"
            if s.parse::<i64>().is_err() {
                return Err(clap::Error::raw(
                    clap::error::ErrorKind::ValueValidation,
                    format!("invalid --max-delete value '{s}': must be an integer\n"),
                ));
            }
            Some(value)
        }
        None => None,
    };

    let min_size = matches.remove_one::<OsString>("min-size");
    let max_size = matches.remove_one::<OsString>("max-size");
    let block_size = match matches.remove_one::<OsString>("block-size") {
        Some(value) => {
            let s = value.to_string_lossy();
            if s.parse::<u64>().is_err() {
                return Err(clap::Error::raw(
                    clap::error::ErrorKind::ValueValidation,
                    format!("invalid --block-size value '{s}': must be a positive integer\n"),
                ));
            }
            Some(value)
        }
        None => None,
    };

    let rayon_threads = parse_thread_count(&mut matches, "rayon-threads")?;
    let tokio_threads = parse_thread_count(&mut matches, "tokio-threads")?;
    let checksum_threads = parse_checksum_threads(&mut matches)?;

    let spill_dir = matches
        .remove_one::<OsString>("spill-dir")
        .map(PathBuf::from);
    let spill_threshold_bytes = parse_spill_threshold_bytes(&mut matches)?;
    let no_spill = matches.get_flag("no-spill");

    let modify_window = match matches.remove_one::<OsString>("modify-window") {
        Some(value) => {
            let s = value.to_string_lossy();
            match s.parse::<i32>() {
                Ok(n) if n >= 0 => Some(value),
                Ok(_) => {
                    return Err(clap::Error::raw(
                        clap::error::ErrorKind::ValueValidation,
                        format!(
                            "invalid --modify-window value '{s}': must be a non-negative integer\n"
                        ),
                    ));
                }
                Err(_) => {
                    return Err(clap::Error::raw(
                        clap::error::ErrorKind::ValueValidation,
                        format!(
                            "invalid --modify-window value '{s}': must be a non-negative integer\n"
                        ),
                    ));
                }
            }
        }
        None => None,
    };

    let delete_mode_conflicts = [
        delete_before_flag,
        delete_during_flag,
        delete_delay_flag,
        delete_after_flag,
    ]
    .into_iter()
    .filter(|flag| *flag)
    .count();

    if delete_mode_conflicts > 1 {
        return Err(clap::Error::raw(
            clap::error::ErrorKind::ArgumentConflict,
            "--delete-before, --delete-during, --delete-delay, and --delete-after are mutually exclusive",
        ));
    }

    let mut delete_mode = if delete_before_flag {
        DeleteMode::Before
    } else if delete_delay_flag {
        DeleteMode::Delay
    } else if delete_after_flag {
        DeleteMode::After
    } else if delete_during_flag {
        DeleteMode::During
    } else if delete_flag {
        DeleteMode::DuringDefault
    } else {
        DeleteMode::Disabled
    };

    if delete_excluded && !delete_mode.is_enabled() {
        delete_mode = DeleteMode::DuringDefault;
    }
    if max_delete.is_some() && !delete_mode.is_enabled() {
        delete_mode = DeleteMode::DuringDefault;
    }

    // Mirror upstream: --delete requires --recursive or --dirs
    if delete_mode.is_enabled() && !recursive && dirs != Some(true) {
        return Err(clap::Error::raw(
            clap::error::ErrorKind::MissingRequiredArgument,
            "--delete does not work without --recursive (-r) or --dirs (-d).\n",
        ));
    }

    let mut backup = matches.get_flag("backup");
    let backup_dir = matches.remove_one::<OsString>("backup-dir");
    let backup_suffix = matches.remove_one::<OsString>("suffix");
    if backup_dir.is_some() || backup_suffix.is_some() {
        backup = true;
    }
    let compress_count = matches.get_count("compress");
    let compress_flag = compress_count > 0;
    let no_compress = matches.get_flag("no-compress");
    let mut compress = if no_compress { false } else { compress_flag };
    let no_open_noatime = matches.get_flag("no-open-noatime");
    let open_noatime_flag = matches.get_flag("open-noatime");
    let open_noatime = if no_open_noatime {
        false
    } else {
        open_noatime_flag
    };
    let prefer_aes_gcm = if matches.get_flag("no-aes") {
        Some(false)
    } else if matches.get_flag("aes") {
        Some(true)
    } else {
        None
    };
    let ssh_cipher: Vec<String> = matches
        .remove_one::<OsString>("ssh-cipher")
        .map(|v| {
            v.to_string_lossy()
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default();
    let ssh_connect_timeout = matches
        .remove_one::<OsString>("ssh-connect-timeout")
        .and_then(|v| v.to_string_lossy().parse::<u64>().ok());
    let ssh_keepalive = matches
        .remove_one::<OsString>("ssh-keepalive")
        .and_then(|v| v.to_string_lossy().parse::<u64>().ok());
    let ssh_identity: Vec<PathBuf> = matches
        .remove_many::<OsString>("ssh-identity")
        .map(|vals| vals.map(PathBuf::from).collect())
        .unwrap_or_default();
    let ssh_no_agent = matches.get_flag("ssh-no-agent");
    let ssh_strict_host_key_checking = matches
        .remove_one::<OsString>("ssh-strict-host-key-checking")
        .map(|v| v.to_string_lossy().into_owned());
    let ssh_ipv6 = matches.get_flag("ssh-ipv6");
    let ssh_port = matches
        .remove_one::<OsString>("ssh-port")
        .and_then(|v| v.to_string_lossy().parse::<u16>().ok());
    let jump_host = matches
        .remove_one::<OsString>("jump-host")
        .filter(|v| !v.is_empty());

    let compress_level_opt = matches.get_one::<OsString>("compress-level").cloned();
    if let Some(ref value) = compress_level_opt
        && let Ok(setting) =
            parse_compress_level_argument(value.as_os_str(), CompressionAlgorithm::Zlib)
    {
        // The codec here is only a base estimate for the `compress` flag; the
        // authoritative codec-aware level is finalised in
        // `parse_compression_settings()` once `--compress-choice` is known.
        compress = !setting.is_disabled();
    }
    let iconv = matches.remove_one::<OsString>("iconv");
    let no_iconv = matches.get_flag("no-iconv");
    let owner = tri_state_flag_positive_first(&matches, "owner", "no-owner");
    let group = tri_state_flag_positive_first(&matches, "group", "no-group");
    let usermap = join_os_values(matches.remove_many::<OsString>("usermap"));
    let groupmap = join_os_values(matches.remove_many::<OsString>("groupmap"));
    let chown = matches.remove_one::<OsString>("chown");
    let copy_as = matches.remove_one::<OsString>("copy-as");
    let chmod = matches
        .remove_many::<OsString>("chmod")
        .map(Iterator::collect)
        .unwrap_or_default();
    let perms = tri_state_flag_positive_first(&matches, "perms", "no-perms");
    let executability = if matches.get_flag("executability") {
        Some(true)
    } else {
        None
    };
    let super_mode = tri_state_flag_positive_first(&matches, "super", "no-super");
    let fake_super = tri_state_flag_positive_first(&matches, "fake-super", "no-fake-super");
    let times = tri_state_flag_positive_first(&matches, "times", "no-times");
    let omit_dir_times =
        tri_state_flag_positive_first(&matches, "omit-dir-times", "no-omit-dir-times");
    let acls = tri_state_flag_positive_first(&matches, "acls", "no-acls");
    let xattrs = tri_state_flag_positive_first(&matches, "xattrs", "no-xattrs");
    let numeric_ids = tri_state_flag_positive_first(&matches, "numeric-ids", "no-numeric-ids");
    let hard_links = tri_state_flag_positive_first(&matches, "hard-links", "no-hard-links");
    let links = tri_state_flag_positive_first(&matches, "links", "no-links");
    let sparse = tri_state_flag_positive_first(&matches, "sparse", "no-sparse");
    let sparse_detect = match matches.remove_one::<OsString>("sparse-detect") {
        Some(value) => {
            let text = value.to_string_lossy().into_owned();
            match engine::SparseDetectStrategy::parse(&text) {
                Ok(strategy) => Some(strategy),
                Err(_) => {
                    return Err(clap::Error::raw(
                        clap::error::ErrorKind::ValueValidation,
                        format!(
                            "invalid value for --sparse-detect: '{text}' (expected auto, seek, map, or none)\n",
                        ),
                    ));
                }
            }
        }
        None => None,
    };
    let fuzzy = {
        let count = matches.get_count("fuzzy");
        let negated = matches.get_flag("no-fuzzy");
        if negated && count == 0 {
            Some(0u8)
        } else if count > 0 {
            Some(count.min(2))
        } else {
            None
        }
    };
    let copy_links = if matches.get_flag("copy-links") {
        Some(true)
    } else {
        None
    };
    let copy_dirlinks = matches.get_flag("copy-dirlinks");
    let copy_unsafe_links_option = if matches.get_flag("copy-unsafe-links") {
        Some(true)
    } else if matches.get_flag("safe-links") {
        Some(false)
    } else {
        None
    };
    let keep_dirlinks = if matches.get_flag("keep-dirlinks") {
        Some(true)
    } else {
        None
    };
    let safe_links = matches.get_flag("safe-links") || copy_unsafe_links_option == Some(true);
    let munge_links = tri_state_flag_positive_first(&matches, "munge-links", "no-munge-links");
    let trust_sender = matches.get_flag("trust-sender");
    let server_mode = matches.get_flag("server");
    let sender_mode = matches.get_flag("sender");
    let detach = tri_state_flag_positive_first(&matches, "detach", "no-detach");
    let daemon_mode = matches.get_flag("daemon");
    let config = matches.remove_one::<OsString>("config");
    let force = tri_state_flag_positive_first(&matches, "force", "no-force");
    let qsort = matches.get_flag("qsort");
    let copy_devices = matches.get_flag("copy-devices");
    let archive_devices =
        tri_state_flag_positive_first(&matches, "archive-devices", "no-archive-devices");
    let devices =
        tri_state_flag_positive_first(&matches, "devices", "no-devices").or(archive_devices);
    let specials =
        tri_state_flag_positive_first(&matches, "specials", "no-specials").or(archive_devices);
    let write_devices =
        tri_state_flag_positive_first(&matches, "write-devices", "no-write-devices");
    let relative = tri_state_flag_positive_first(&matches, "relative", "no-relative");
    let one_file_system = {
        let count = matches.get_count("one-file-system");
        let negated = matches.get_flag("no-one-file-system");
        if negated && count == 0 {
            Some(0u8)
        } else if count > 0 {
            Some(count.min(2))
        } else {
            None
        }
    };
    let implied_dirs = tri_state_flag_positive_first(&matches, "implied-dirs", "no-implied-dirs");
    let msgs_to_stderr = tri_state_flag_positive_first(&matches, "msgs2stderr", "no-msgs2stderr");
    let stderr_mode = matches.remove_one::<OsString>("stderr");
    let outbuf = matches.remove_one::<OsString>("outbuf");
    let max_alloc = matches.remove_one::<OsString>("max-alloc");
    let stats = matches.get_flag("stats");
    let eight_bit_output = matches.get_flag("8-bit-output");
    let partial_flag = matches.get_flag("partial") || matches.get_count("partial-progress") > 0;
    let no_partial = matches.get_flag("no-partial");
    let preallocate = matches.get_flag("preallocate");
    let fsync = if matches.get_flag("fsync") {
        Some(true)
    } else {
        None
    };
    let io_uring_policy = if matches.get_flag("io-uring") {
        fast_io::IoUringPolicy::Enabled
    } else if matches.get_flag("no-io-uring") {
        fast_io::IoUringPolicy::Disabled
    } else if matches.get_flag("no-io-uring-sqpoll") {
        // Set the process-wide SQPOLL gate eagerly so that any io_uring
        // ring built later in this process - including ones created from
        // configs with `sqpoll: true` and the session pool's shared rings
        // - honours the opt-out without re-reading the policy.
        fast_io::set_sqpoll_disabled_by_policy();
        fast_io::IoUringPolicy::SqpollOff
    } else {
        fast_io::IoUringPolicy::Auto
    };
    let io_uring_depth = match matches.remove_one::<OsString>("io-uring-depth") {
        Some(value) => {
            let s = value.to_string_lossy();
            let parsed = s.parse::<u32>().map_err(|_| {
                clap::Error::raw(
                    clap::error::ErrorKind::ValueValidation,
                    format!("invalid --io-uring-depth value '{s}': must be a positive integer\n"),
                )
            })?;
            let validated = fast_io::validate_io_uring_depth(parsed).map_err(|e| {
                clap::Error::raw(
                    clap::error::ErrorKind::ValueValidation,
                    format!("invalid --io-uring-depth value '{s}': {e}\n"),
                )
            })?;
            Some(validated)
        }
        None => None,
    };
    let zero_copy_policy = if matches.get_flag("zero-copy") {
        fast_io::ZeroCopyPolicy::Enabled
    } else if matches.get_flag("no-zero-copy") {
        fast_io::ZeroCopyPolicy::Disabled
    } else {
        fast_io::ZeroCopyPolicy::Auto
    };
    // Capture the reflink index before remove_one drains the match data;
    // resolve_cow_policy needs it to break ties against --cow / --no-cow.
    let reflink_index = last_occurrence(&matches, "reflink");
    let reflink_value = matches.remove_one::<OsString>("reflink");
    let reflink_explicit = match reflink_value {
        Some(value) => {
            let text = value.to_string_lossy();
            let policy = parse_reflink_mode(text.as_ref()).ok_or_else(|| {
                clap::Error::raw(
                    clap::error::ErrorKind::InvalidValue,
                    format!(
                        "invalid value '{text}' for '--reflink <MODE>': \
                         expected one of auto, always, never\n"
                    ),
                )
            })?;
            Some(policy)
        }
        None => None,
    };
    let cow_policy = resolve_cow_policy(&matches, reflink_explicit, reflink_index);
    let simd_override = match matches.remove_one::<OsString>("simd") {
        Some(value) => {
            let text = value.to_string_lossy();
            match checksums::SimdLevel::parse_cli(text.as_ref()) {
                Some(level) => Some(level),
                None => {
                    return Err(clap::Error::raw(
                        clap::error::ErrorKind::InvalidValue,
                        format!(
                            "invalid value '{text}' for '--simd <LEVEL>': \
                             expected one of auto, avx512, avx2, sse4, neon, none\n"
                        ),
                    ));
                }
            }
        }
        None => None,
    };
    let delay_updates = matches.get_flag("delay-updates") && !matches.get_flag("no-delay-updates");
    let partial_dir_cli = matches
        .remove_one::<OsString>("partial-dir")
        .map(PathBuf::from);
    let partial_dir = if no_partial {
        None
    } else if let Some(dir) = partial_dir_cli {
        Some(dir)
    } else {
        env::var_os("RSYNC_PARTIAL_DIR")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
    };
    let partial = if no_partial {
        false
    } else {
        partial_flag || partial_dir.is_some()
    };
    let temp_dir = matches
        .remove_one::<OsString>("temp-dir")
        .map(PathBuf::from);
    let log_file = matches.remove_one::<OsString>("log-file");
    let log_file_format = matches.remove_one::<OsString>("log-file-format");
    let write_batch = matches.remove_one::<OsString>("write-batch");
    let only_write_batch = matches.remove_one::<OsString>("only-write-batch");
    let read_batch = matches.remove_one::<OsString>("read-batch");
    let early_input = matches.remove_one::<OsString>("early-input");
    let link_dest_args: Vec<OsString> = matches
        .remove_many::<OsString>("link-dest")
        .map(Iterator::collect)
        .unwrap_or_default();
    let link_dests = link_dest_args.iter().map(PathBuf::from).collect();
    let link_destinations = link_dest_args;
    let remove_source_files =
        matches.get_flag("remove-source-files") || matches.get_flag("remove-sent-files");
    let inplace = tri_state_flag_positive_first(&matches, "inplace", "no-inplace");
    // upstream: options.c:1722-1726 - OPT_APPEND increments append_mode only on
    // the server (`am_server`); a non-server invocation caps it at 1. A second
    // `--append` on the server wire is the encoding of `--append-verify`
    // (append_mode == 2). `--append-verify` sets it directly (options.c:719).
    let append_count = matches.get_count("append");
    let append_verify_flag =
        matches.get_flag("append-verify") || (server_mode && append_count >= 2);
    let append = if append_verify_flag || append_count >= 1 {
        Some(true)
    } else if matches.get_flag("no-append") {
        Some(false)
    } else {
        None
    };
    let whole_file = tri_state_flag_positive_first(&matches, "whole-file", "no-whole-file");
    let xxh64_dedup = matches.get_flag("xxh64-dedup");
    let progress_setting =
        if matches.get_flag("progress") || matches.get_count("partial-progress") > 0 {
            ProgressSetting::PerFile
        } else if matches.get_flag("no-progress") {
            ProgressSetting::Disabled
        } else {
            ProgressSetting::Unspecified
        };
    let itemize_changes_flag = matches.get_count("itemize-changes") > 0;
    // upstream: options.c:1581 increments itemize_changes per `-i`, and
    // options.c:2354 sets `stdout_format_has_i = itemize_changes`; the
    // emit gate at generator.c:582 fires on `stdout_format_has_i > 1`, i.e.
    // the `-i` flag given at least twice.
    let itemize_changes_repeated =
        matches.get_count("itemize-changes") > 1 && !matches.get_flag("no-itemize-changes");
    let no_itemize_changes_flag = matches.get_flag("no-itemize-changes");
    let name_overridden = itemize_changes_flag || no_itemize_changes_flag;
    let mut verbosity = matches.get_count("verbose") as u8;
    if matches.get_flag("no-verbose") {
        verbosity = 0;
    }
    let quiet = matches.get_flag("quiet");
    if quiet {
        verbosity = 0;
    }
    let remainder = matches
        .remove_many::<OsString>("args")
        .map(Iterator::collect)
        .unwrap_or_default();
    let checksum = tri_state_flag_positive_first(&matches, "checksum", "no-checksum");
    let size_only = matches.get_flag("size-only");
    let ignore_times = matches.get_flag("ignore-times");
    let (checksum_choice, checksum_choice_arg) =
        match matches.remove_one::<OsString>("checksum-choice") {
            Some(value) => {
                let text = value.to_string_lossy().into_owned();
                match StrongChecksumChoice::parse(&text) {
                    Ok(choice) => {
                        let normalized = OsString::from(choice.to_argument());
                        (Some(choice), Some(normalized))
                    }
                    Err(message) => {
                        return Err(clap::Error::raw(
                            clap::error::ErrorKind::ValueValidation,
                            message.text().to_owned(),
                        ));
                    }
                }
            }
            None => (None, None),
        };

    let checksum_seed = match matches.remove_one::<OsString>("checksum-seed") {
        Some(value) => match parse_checksum_seed_argument(value.as_os_str()) {
            Ok(seed) => Some(seed),
            Err(message) => {
                return Err(clap::Error::raw(
                    clap::error::ErrorKind::ValueValidation,
                    message.text().to_owned(),
                ));
            }
        },
        None => None,
    };

    let compress_level = matches.remove_one::<OsString>("compress-level");
    let compress_choice = matches.remove_one::<OsString>("compress-choice");
    let compress_threads = matches.remove_one::<OsString>("compress-threads");
    let old_compress = matches.get_flag("old-compress");
    // upstream: options.c:2002 - if (!compress_choice && do_compression > 1)
    //   compress_choice = "zlibx"; -zz selects new-style compression.
    let new_compress = matches.get_flag("new-compress")
        || (compress_count >= 2 && compress_choice.is_none() && !old_compress);
    let skip_compress = matches.remove_one::<OsString>("skip-compress");
    let no_bwlimit = matches.get_flag("no-bwlimit");
    let bwlimit = if no_bwlimit {
        Some(BandwidthArgument::Disabled)
    } else {
        matches
            .remove_one::<OsString>("bwlimit")
            .map(BandwidthArgument::Limit)
    };
    // Capture every filter-producing option in true command-line order before
    // the per-option values below are drained. upstream: options.c dispatches
    // each --include/--exclude/--filter/--include-from/--exclude-from/-C/-F at
    // its argv position, so evaluation is first-match-wins over encounter order.
    let mut filter_order = build_filter_order(&matches, &args);
    let excludes = matches
        .remove_many::<OsString>("exclude")
        .map(Iterator::collect)
        .unwrap_or_default();
    let includes = matches
        .remove_many::<OsString>("include")
        .map(Iterator::collect)
        .unwrap_or_default();
    let compare_destinations = matches
        .remove_many::<OsString>("compare-dest")
        .map(Iterator::collect)
        .unwrap_or_default();
    let copy_destinations = matches
        .remove_many::<OsString>("copy-dest")
        .map(Iterator::collect)
        .unwrap_or_default();
    let exclude_from = matches
        .remove_many::<OsString>("exclude-from")
        .map(Iterator::collect)
        .unwrap_or_default();
    let include_from = matches
        .remove_many::<OsString>("include-from")
        .map(Iterator::collect)
        .unwrap_or_default();
    let _ = matches.remove_many::<OsString>("filter");
    let rsync_filter_shortcuts = matches.get_count("rsync-filter") as usize;
    // parsed.filters is the ordered stream's `--filter`/`-f`/`-F` directives,
    // preserving their command-line position relative to each other.
    let filter_args: Vec<OsString> = filter_order
        .iter()
        .filter_map(|token| match token {
            FilterOrderToken::Filter(rule) => Some(rule.clone()),
            _ => None,
        })
        .collect();
    // upstream: options.c:2197-2199 - once the argv scan is complete, xfer_dirs>=4
    // (set by --old-dirs/--old-d) appends `- /*/*` to the TAIL of the filter list
    // via parse_filter_str(&filter_list, "- /*/*", ...). exclude.c:parse_filter_str
    // appends each rule at the end, so this rule evaluates AFTER every user
    // --include/--exclude/--filter, excluding anything two or more levels deep
    // while still recursing into the immediate children. Injected into the
    // evaluation stream only (not `filter_args`) so `--filter` echoing is
    // unaffected.
    if old_dirs {
        filter_order.push(FilterOrderToken::Filter(OsString::from("- /*/*")));
    }
    let cvs_exclude = matches.get_flag("cvs-exclude");
    let apple_double_skip = matches.get_flag("apple-double-skip");
    let files_from = matches
        .remove_many::<OsString>("files-from")
        .map(Iterator::collect)
        .unwrap_or_default();
    let from0 = matches.get_flag("from0");
    let disable_from0 = matches.get_flag("no-from0");
    let from0 = from0 && !disable_from0;
    let info: Vec<OsString> = matches
        .remove_many::<OsString>("info")
        .map(Iterator::collect)
        .unwrap_or_default();
    // upstream: generator.c:582-583 - the itemize line for an unchanged entry
    // is emitted only when `INFO_GTE(NAME, 2)` is in effect, which `-vv` and
    // `--info=name2` both set. Resolve the effective NAME info level the same
    // way the logging config does (verbose count + any `--info=` override) so
    // `-ivv` surfaces unchanged dirs, files, and symlinks like upstream rather
    // than capping at the updated-only level.
    let name_info_level = {
        let mut cfg = logging::VerbosityConfig::from_verbose_level(verbosity);
        for token in &info {
            if let Some(text) = token.to_str() {
                for part in text.split(',') {
                    let _ = cfg.apply_info_flag(part.trim());
                }
            }
        }
        cfg.info.get(logging::InfoFlag::Name)
    };
    let name_level = if itemize_changes_flag && !no_itemize_changes_flag {
        // upstream: generator.c:575-576 - the itemize emit gate fires for an
        // unchanged entry when `stdout_format_has_i > 1` (i.e. `-i` given at
        // least twice, captured by `itemize_changes_repeated`) OR
        // `INFO_GTE(NAME, 2)` (`-vv` / `--info=name2`, captured by
        // `name_info_level >= 2`). Either path surfaces unchanged rows.
        if name_info_level >= 2 || itemize_changes_repeated {
            NameOutputLevel::UpdatedAndUnchanged
        } else {
            NameOutputLevel::UpdatedOnly
        }
    } else {
        NameOutputLevel::Disabled
    };
    let debug = matches
        .remove_many::<OsString>("debug")
        .map(Iterator::collect)
        .unwrap_or_default();
    let ignore_existing = matches.get_flag("ignore-existing");
    let existing = matches.get_flag("existing");
    let update = matches.get_flag("update");
    let password_file = matches.remove_one::<OsString>("password-file");
    let password_command = matches.remove_one::<OsString>("password-command");
    // upstream: options.c does not range-check --protocol at parse time.
    // Incompatible versions fail during negotiation with RERR_PROTOCOL (exit 2).
    let protocol = matches.remove_one::<OsString>("protocol");
    let timeout = matches.remove_one::<OsString>("timeout");
    let contimeout = matches.remove_one::<OsString>("contimeout");
    let stop_after = matches.remove_one::<OsString>("stop-after");
    let stop_at_option = matches.remove_one::<OsString>("stop-at");
    let out_format = matches.remove_one::<OsString>("out-format");
    let dparam = matches
        .remove_many::<OsString>("dparam")
        .map(Iterator::collect)
        .unwrap_or_default();
    let itemize_changes = itemize_changes_flag && !no_itemize_changes_flag;
    let mut no_motd = matches.get_flag("no-motd");
    if matches.get_flag("motd") {
        no_motd = false;
    }

    Ok(ParsedArgs {
        program_name,
        show_help,
        show_version,
        show_io_uring_status,
        show_lsm_status,
        human_readable,
        dry_run,
        list_only,
        remote_shell,
        connect_program,
        remote_options,
        rsync_path,
        protect_args,
        old_args,
        address_mode,
        bind_address: bind_address_raw,
        sockopts,
        tcp_fastopen,
        blocking_io,
        archive,
        recursive,
        recursive_override,
        inc_recursive,
        dirs,
        delete_mode,
        delete_excluded,
        delete_missing_args,
        ignore_errors,
        backup,
        backup_dir,
        backup_suffix,
        checksum,
        checksum_choice,
        checksum_choice_arg,
        checksum_seed,
        size_only,
        ignore_times,
        ignore_existing,
        existing,
        ignore_missing_args,
        update,
        remainder,
        bwlimit,
        max_delete,
        min_size,
        max_size,
        block_size,
        modify_window,
        compress,
        no_compress,
        compress_level,
        compress_choice,
        compress_threads,
        old_compress,
        new_compress,
        skip_compress,
        open_noatime,
        no_open_noatime,
        iconv,
        owner,
        group,
        chown,
        copy_as,
        usermap,
        groupmap,
        chmod,
        perms,
        super_mode,
        fake_super,
        times,
        omit_dir_times,
        omit_link_times,
        atimes,
        crtimes,
        acls,
        numeric_ids,
        hard_links,
        links,
        sparse,
        sparse_detect,
        fuzzy,
        copy_links,
        copy_dirlinks,
        copy_unsafe_links: copy_unsafe_links_option,
        keep_dirlinks,
        safe_links,
        munge_links,
        trust_sender,
        server_mode,
        sender_mode,
        detach,
        daemon_mode,
        config,
        write_devices,
        devices,
        copy_devices,
        specials,
        force,
        qsort,
        relative,
        one_file_system,
        implied_dirs,
        mkpath,
        prune_empty_dirs,
        verbosity,
        quiet,
        progress: progress_setting,
        name_level,
        name_overridden,
        stats,
        eight_bit_output,
        partial,
        preallocate,
        fsync,
        io_uring_policy,
        io_uring_depth,
        zero_copy_policy,
        cow_policy,
        simd_override,
        delay_updates,
        partial_dir,
        temp_dir,
        log_file,
        log_file_format,
        write_batch,
        only_write_batch,
        read_batch,
        early_input,
        link_dests,
        remove_source_files,
        inplace,
        append,
        append_verify: append_verify_flag,
        msgs_to_stderr,
        stderr_mode,
        outbuf,
        max_alloc,
        itemize_changes,
        itemize_repeated: itemize_changes_repeated,
        whole_file,
        xxh64_dedup,
        excludes,
        includes,
        compare_destinations,
        copy_destinations,
        link_destinations,
        exclude_from,
        include_from,
        filters: filter_args,
        filter_order,
        cvs_exclude,
        apple_double_skip,
        rsync_filter_shortcuts,
        files_from,
        from0,
        info,
        debug,
        xattrs,
        no_motd,
        password_file,
        password_command,
        protocol,
        timeout,
        contimeout,
        stop_after,
        stop_at: stop_at_option,
        out_format,
        daemon_port,
        dparam,
        no_iconv,
        executability,
        prefer_aes_gcm,
        ssh_cipher,
        ssh_connect_timeout,
        ssh_keepalive,
        ssh_identity,
        ssh_no_agent,
        ssh_strict_host_key_checking,
        ssh_ipv6,
        ssh_port,
        jump_host,
        rayon_threads,
        tokio_threads,
        checksum_threads,
        spill_dir,
        spill_threshold_bytes,
        no_spill,
    })
}
