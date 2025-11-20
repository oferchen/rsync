use std::env;
use std::ffi::OsString;
use std::path::PathBuf;

use crate::frontend::arguments::short_options::expand_short_options;
use crate::frontend::command_builder::clap_command;
use crate::frontend::execution::{
    parse_checksum_seed_argument, parse_compress_level_argument, parse_human_readable_level,
};
use crate::frontend::filter_rules::{collect_filter_arguments, locate_filter_arguments};
use crate::frontend::progress::{NameOutputLevel, ProgressSetting};
use core::client::{AddressMode, DeleteMode, HumanReadableMode, StrongChecksumChoice};

use super::{BandwidthArgument, ParsedArgs, detect_program_name, env_protect_args_default};

pub(crate) fn parse_args<I, S>(arguments: I) -> Result<ParsedArgs, clap::Error>
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
    let args = expand_short_options(&command, args);
    let (filter_indices, rsync_filter_indices) = locate_filter_arguments(&args);
    let mut matches = command.try_get_matches_from(args)?;

    let show_help = matches.get_flag("help");
    let show_version = matches.get_flag("version");
    let mut human_readable = matches
        .remove_one::<OsString>("human-readable")
        .map(|value| parse_human_readable_level(value.as_os_str()))
        .transpose()?;
    if matches.get_flag("no-human-readable") {
        human_readable = Some(HumanReadableMode::Disabled);
    }
    let mut dry_run = matches.get_flag("dry-run");
    let list_only = matches.get_flag("list-only");
    let mkpath = tri_state_flag_positive_first(&matches, "mkpath", "no-mkpath").unwrap_or(false);
    let prune_empty_dirs =
        tri_state_flag_negative_first(&matches, "prune-empty-dirs", "no-prune-empty-dirs");
    let omit_link_times =
        tri_state_flag_negative_first(&matches, "omit-link-times", "no-omit-link-times");
    if list_only {
        dry_run = true;
    }
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
        .map(|values| values.collect())
        .unwrap_or_default();
    let protect_args = if matches.get_flag("no-protect-args") {
        Some(false)
    } else if matches.get_flag("protect-args") {
        Some(true)
    } else {
        env_protect_args_default()
    };
    let address_mode = if matches.get_flag("ipv4") {
        AddressMode::Ipv4
    } else if matches.get_flag("ipv6") {
        AddressMode::Ipv6
    } else {
        AddressMode::Default
    };
    let bind_address_raw = matches.remove_one::<OsString>("address");
    let sockopts = matches.remove_one::<OsString>("sockopts");
    let blocking_io = tri_state_flag_positive_first(&matches, "blocking-io", "no-blocking-io");
    let archive = matches.get_flag("archive");
    let recursive_override = tri_state_flag_negative_first(&matches, "recursive", "no-recursive");
    let recursive = if recursive_override == Some(false) {
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
    let max_delete = matches.remove_one::<OsString>("max-delete");
    let min_size = matches.remove_one::<OsString>("min-size");
    let max_size = matches.remove_one::<OsString>("max-size");
    let block_size = matches.remove_one::<OsString>("block-size");
    let modify_window = matches.remove_one::<OsString>("modify-window");

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
    } else if delete_during_flag || delete_flag {
        DeleteMode::During
    } else {
        DeleteMode::Disabled
    };

    if delete_excluded && !delete_mode.is_enabled() {
        delete_mode = DeleteMode::During;
    }
    if max_delete.is_some() && !delete_mode.is_enabled() {
        delete_mode = DeleteMode::During;
    }
    let mut backup = matches.get_flag("backup");
    let backup_dir = matches.remove_one::<OsString>("backup-dir");
    let backup_suffix = matches.remove_one::<OsString>("suffix");
    if backup_dir.is_some() || backup_suffix.is_some() {
        backup = true;
    }
    let compress_flag = matches.get_flag("compress");
    let no_compress = matches.get_flag("no-compress");
    let mut compress = if no_compress { false } else { compress_flag };
    let no_open_noatime = matches.get_flag("no-open-noatime");
    let open_noatime_flag = matches.get_flag("open-noatime");
    let open_noatime = if no_open_noatime {
        false
    } else {
        open_noatime_flag
    };
    let compress_level_opt = matches.get_one::<OsString>("compress-level").cloned();
    if let Some(ref value) = compress_level_opt {
        if let Ok(setting) = parse_compress_level_argument(value.as_os_str()) {
            compress = !setting.is_disabled();
        }
    }
    let iconv = matches.remove_one::<OsString>("iconv");
    let no_iconv = matches.get_flag("no-iconv");
    let owner = tri_state_flag_positive_first(&matches, "owner", "no-owner");
    let group = tri_state_flag_positive_first(&matches, "group", "no-group");
    let usermap_values = matches
        .remove_many::<OsString>("usermap")
        .map(|values| values.collect::<Vec<_>>())
        .unwrap_or_default();
    if usermap_values.len() > 1 {
        return Err(clap::Error::raw(
            clap::error::ErrorKind::TooManyValues,
            "You can only specify --usermap once.",
        ));
    }
    let usermap = usermap_values.into_iter().next();
    let groupmap_values = matches
        .remove_many::<OsString>("groupmap")
        .map(|values| values.collect::<Vec<_>>())
        .unwrap_or_default();
    if groupmap_values.len() > 1 {
        return Err(clap::Error::raw(
            clap::error::ErrorKind::TooManyValues,
            "You can only specify --groupmap once.",
        ));
    }
    let groupmap = groupmap_values.into_iter().next();
    let chown = matches.remove_one::<OsString>("chown");
    let chmod = matches
        .remove_many::<OsString>("chmod")
        .map(|values| values.collect())
        .unwrap_or_default();
    let perms = tri_state_flag_positive_first(&matches, "perms", "no-perms");
    let executability =
        tri_state_flag_positive_first(&matches, "executability", "no-executability");
    let super_mode = tri_state_flag_positive_first(&matches, "super", "no-super");
    let times = tri_state_flag_positive_first(&matches, "times", "no-times");
    let omit_dir_times =
        tri_state_flag_positive_first(&matches, "omit-dir-times", "no-omit-dir-times");
    let acls = tri_state_flag_positive_first(&matches, "acls", "no-acls");
    let xattrs = tri_state_flag_positive_first(&matches, "xattrs", "no-xattrs");
    let numeric_ids = tri_state_flag_positive_first(&matches, "numeric-ids", "no-numeric-ids");
    let hard_links = tri_state_flag_positive_first(&matches, "hard-links", "no-hard-links");
    let links = tri_state_flag_positive_first(&matches, "links", "no-links");
    let sparse = tri_state_flag_positive_first(&matches, "sparse", "no-sparse");
    let copy_links = tri_state_flag_positive_first(&matches, "copy-links", "no-copy-links");
    let copy_dirlinks = matches.get_flag("copy-dirlinks");
    let copy_unsafe_links_option = if matches.get_flag("copy-unsafe-links") {
        Some(true)
    } else if matches.get_flag("no-copy-unsafe-links") || matches.get_flag("safe-links") {
        Some(false)
    } else {
        None
    };
    let keep_dirlinks =
        tri_state_flag_positive_first(&matches, "keep-dirlinks", "no-keep-dirlinks");
    let safe_links = matches.get_flag("safe-links") || copy_unsafe_links_option == Some(true);
    let force = tri_state_flag_positive_first(&matches, "force", "no-force");
    let copy_devices = matches.get_flag("copy-devices");
    let devices = if matches.get_flag("devices") || matches.get_flag("archive-devices") {
        Some(true)
    } else if matches.get_flag("no-devices") {
        Some(false)
    } else {
        None
    };
    let specials = if matches.get_flag("specials") || matches.get_flag("archive-devices") {
        Some(true)
    } else if matches.get_flag("no-specials") {
        Some(false)
    } else {
        None
    };
    let relative = tri_state_flag_positive_first(&matches, "relative", "no-relative");
    let one_file_system =
        tri_state_flag_positive_first(&matches, "one-file-system", "no-one-file-system");
    let implied_dirs = tri_state_flag_positive_first(&matches, "implied-dirs", "no-implied-dirs");
    let msgs_to_stderr = tri_state_flag_positive_first(&matches, "msgs2stderr", "no-msgs2stderr");
    let outbuf = matches.remove_one::<OsString>("outbuf");
    let stats = matches.get_flag("stats");
    let partial_flag = matches.get_flag("partial") || matches.get_count("partial-progress") > 0;
    let no_partial = matches.get_flag("no-partial");
    let preallocate = matches.get_flag("preallocate");
    let fsync = tri_state_flag_positive_first(&matches, "fsync", "no-fsync");
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
    let link_dest_args: Vec<OsString> = matches
        .remove_many::<OsString>("link-dest")
        .map(|values| values.collect())
        .unwrap_or_default();
    let link_dests = link_dest_args.iter().map(PathBuf::from).collect();
    let link_destinations = link_dest_args;
    let remove_source_files =
        matches.get_flag("remove-source-files") || matches.get_flag("remove-sent-files");
    let inplace = tri_state_flag_positive_first(&matches, "inplace", "no-inplace");
    let append_verify_flag = matches.get_flag("append-verify");
    let append = if append_verify_flag || matches.get_flag("append") {
        Some(true)
    } else if matches.get_flag("no-append") {
        Some(false)
    } else {
        None
    };
    let whole_file = tri_state_flag_positive_first(&matches, "whole-file", "no-whole-file");
    let progress_setting = if matches.get_flag("progress") {
        ProgressSetting::PerFile
    } else if matches.get_flag("no-progress") {
        ProgressSetting::Disabled
    } else {
        ProgressSetting::Unspecified
    };
    let itemize_changes_flag = matches.get_flag("itemize-changes");
    let no_itemize_changes_flag = matches.get_flag("no-itemize-changes");
    let name_level = if itemize_changes_flag && !no_itemize_changes_flag {
        NameOutputLevel::UpdatedOnly
    } else {
        NameOutputLevel::Disabled
    };
    let name_overridden = itemize_changes_flag || no_itemize_changes_flag;
    let mut verbosity = matches.get_count("verbose") as u8;
    if matches.get_flag("no-verbose") {
        verbosity = 0;
    }
    if matches.get_flag("quiet") {
        verbosity = 0;
    }
    let remainder = matches
        .remove_many::<OsString>("args")
        .map(|values| values.collect())
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
                            message.text().to_string(),
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
                    message.text().to_string(),
                ));
            }
        },
        None => None,
    };

    let compress_level = matches.remove_one::<OsString>("compress-level");
    let compress_choice = matches.remove_one::<OsString>("compress-choice");
    let skip_compress = matches.remove_one::<OsString>("skip-compress");
    let no_bwlimit = matches.get_flag("no-bwlimit");
    let bwlimit = if no_bwlimit {
        Some(BandwidthArgument::Disabled)
    } else {
        matches
            .remove_one::<OsString>("bwlimit")
            .map(BandwidthArgument::Limit)
    };
    let excludes = matches
        .remove_many::<OsString>("exclude")
        .map(|values| values.collect())
        .unwrap_or_default();
    let includes = matches
        .remove_many::<OsString>("include")
        .map(|values| values.collect())
        .unwrap_or_default();
    let compare_destinations = matches
        .remove_many::<OsString>("compare-dest")
        .map(|values| values.collect())
        .unwrap_or_default();
    let copy_destinations = matches
        .remove_many::<OsString>("copy-dest")
        .map(|values| values.collect())
        .unwrap_or_default();
    let exclude_from = matches
        .remove_many::<OsString>("exclude-from")
        .map(|values| values.collect())
        .unwrap_or_default();
    let include_from = matches
        .remove_many::<OsString>("include-from")
        .map(|values| values.collect())
        .unwrap_or_default();
    let filters: Vec<OsString> = matches
        .remove_many::<OsString>("filter")
        .map(|values| values.collect())
        .unwrap_or_default();
    let rsync_filter_shortcuts = rsync_filter_indices.len();
    let filter_args = collect_filter_arguments(&filters, &filter_indices, &rsync_filter_indices);
    let cvs_exclude = matches.get_flag("cvs-exclude");
    let files_from = matches
        .remove_many::<OsString>("files-from")
        .map(|values| values.collect())
        .unwrap_or_default();
    let from0 = matches.get_flag("from0");
    let disable_from0 = matches.get_flag("no-from0");
    let from0 = from0 && !disable_from0;
    let info = matches
        .remove_many::<OsString>("info")
        .map(|values| values.collect())
        .unwrap_or_default();
    let debug = matches
        .remove_many::<OsString>("debug")
        .map(|values| values.collect())
        .unwrap_or_default();
    let ignore_existing = matches.get_flag("ignore-existing");
    let existing = matches.get_flag("existing");
    let update = matches.get_flag("update");
    let password_file = matches.remove_one::<OsString>("password-file");
    let protocol = matches.remove_one::<OsString>("protocol");
    let timeout = matches.remove_one::<OsString>("timeout");
    let contimeout = matches.remove_one::<OsString>("contimeout");
    let stop_after = matches.remove_one::<OsString>("stop-after");
    let stop_at_option = matches.remove_one::<OsString>("stop-at");
    let out_format = matches.remove_one::<OsString>("out-format");
    let itemize_changes = itemize_changes_flag && !no_itemize_changes_flag;
    let mut no_motd = matches.get_flag("no-motd");
    if matches.get_flag("motd") {
        no_motd = false;
    }

    Ok(ParsedArgs {
        program_name,
        show_help,
        show_version,
        human_readable,
        dry_run,
        list_only,
        remote_shell,
        connect_program,
        remote_options,
        rsync_path,
        protect_args,
        address_mode,
        bind_address: bind_address_raw,
        sockopts,
        blocking_io,
        archive,
        recursive,
        recursive_override,
        inc_recursive,
        dirs,
        delete_mode,
        delete_excluded,
        delete_missing_args,
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
        skip_compress,
        open_noatime,
        no_open_noatime,
        iconv,
        owner,
        group,
        chown,
        usermap,
        groupmap,
        chmod,
        perms,
        super_mode,
        times,
        omit_dir_times,
        omit_link_times,
        acls,
        numeric_ids,
        hard_links,
        links,
        sparse,
        copy_links,
        copy_dirlinks,
        copy_unsafe_links: copy_unsafe_links_option,
        keep_dirlinks,
        safe_links,
        devices,
        copy_devices,
        specials,
        force,
        relative,
        one_file_system,
        implied_dirs,
        mkpath,
        prune_empty_dirs,
        verbosity,
        progress: progress_setting,
        name_level,
        name_overridden,
        stats,
        partial,
        preallocate,
        fsync,
        delay_updates,
        partial_dir,
        temp_dir,
        log_file,
        log_file_format,
        write_batch,
        only_write_batch,
        read_batch,
        link_dests,
        remove_source_files,
        inplace,
        append,
        append_verify: append_verify_flag,
        msgs_to_stderr,
        outbuf,
        itemize_changes,
        whole_file,
        excludes,
        includes,
        compare_destinations,
        copy_destinations,
        link_destinations,
        exclude_from,
        include_from,
        filters: filter_args,
        cvs_exclude,
        rsync_filter_shortcuts,
        files_from,
        from0,
        info,
        debug,
        xattrs,
        no_motd,
        password_file,
        protocol,
        timeout,
        contimeout,
        stop_after,
        stop_at: stop_at_option,
        out_format,
        daemon_port,
        no_iconv,
        executability,
    })
}

fn tri_state_flag_positive_first(
    matches: &clap::ArgMatches,
    positive: &str,
    negative: &str,
) -> Option<bool> {
    tri_state_flag_with_order(matches, positive, negative, true)
}

fn tri_state_flag_negative_first(
    matches: &clap::ArgMatches,
    positive: &str,
    negative: &str,
) -> Option<bool> {
    tri_state_flag_with_order(matches, positive, negative, false)
}

fn tri_state_flag_with_order(
    matches: &clap::ArgMatches,
    positive: &str,
    negative: &str,
    prefer_positive_on_tie: bool,
) -> Option<bool> {
    let positive_present = matches.get_flag(positive);
    let negative_present = matches.get_flag(negative);

    match (positive_present, negative_present) {
        (true, false) => Some(true),
        (false, true) => Some(false),
        (false, false) => None,
        (true, true) => {
            let positive_index = last_occurrence(matches, positive);
            let negative_index = last_occurrence(matches, negative);
            match (positive_index, negative_index) {
                (Some(pos), Some(neg)) => {
                    if pos > neg {
                        Some(true)
                    } else if neg > pos {
                        Some(false)
                    } else if prefer_positive_on_tie {
                        Some(true)
                    } else {
                        Some(false)
                    }
                }
                (Some(_), None) => Some(true),
                (None, Some(_)) => Some(false),
                (None, None) => Some(prefer_positive_on_tie),
            }
        }
    }
}

fn last_occurrence(matches: &clap::ArgMatches, id: &str) -> Option<usize> {
    matches.indices_of(id).and_then(|indices| indices.max())
}
