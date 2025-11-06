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
use rsync_core::client::{AddressMode, DeleteMode, HumanReadableMode, StrongChecksumChoice};

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
    let mkpath = matches.get_flag("mkpath");
    let prune_empty_dirs = if matches.get_flag("no-prune-empty-dirs") {
        Some(false)
    } else if matches.get_flag("prune-empty-dirs") {
        Some(true)
    } else {
        None
    };
    let omit_link_times = if matches.get_flag("no-omit-link-times") {
        Some(false)
    } else if matches.get_flag("omit-link-times") {
        Some(true)
    } else {
        None
    };
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
    let archive = matches.get_flag("archive");
    let recursive = archive || matches.get_flag("recursive");
    let delete_flag = matches.get_flag("delete");
    let delete_before_flag = matches.get_flag("delete-before");
    let delete_during_flag = matches.get_flag("delete-during");
    let delete_delay_flag = matches.get_flag("delete-delay");
    let delete_after_flag = matches.get_flag("delete-after");
    let ignore_missing_args = matches.get_flag("ignore-missing-args");
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
    let compress_level_opt = matches.get_one::<OsString>("compress-level").cloned();
    if let Some(ref value) = compress_level_opt {
        if let Ok(setting) = parse_compress_level_argument(value.as_os_str()) {
            compress = !setting.is_disabled();
        }
    }
    let owner = if matches.get_flag("owner") {
        Some(true)
    } else if matches.get_flag("no-owner") {
        Some(false)
    } else {
        None
    };
    let group = if matches.get_flag("group") {
        Some(true)
    } else if matches.get_flag("no-group") {
        Some(false)
    } else {
        None
    };
    let chown = matches.remove_one::<OsString>("chown");
    let chmod = matches
        .remove_many::<OsString>("chmod")
        .map(|values| values.collect())
        .unwrap_or_default();
    let perms = if matches.get_flag("perms") {
        Some(true)
    } else if matches.get_flag("no-perms") {
        Some(false)
    } else {
        None
    };
    let super_mode = if matches.get_flag("super") {
        Some(true)
    } else if matches.get_flag("no-super") {
        Some(false)
    } else {
        None
    };
    let times = if matches.get_flag("times") {
        Some(true)
    } else if matches.get_flag("no-times") {
        Some(false)
    } else {
        None
    };
    let omit_dir_times = if matches.get_flag("omit-dir-times") {
        Some(true)
    } else if matches.get_flag("no-omit-dir-times") {
        Some(false)
    } else {
        None
    };
    let acls = if matches.get_flag("acls") {
        Some(true)
    } else if matches.get_flag("no-acls") {
        Some(false)
    } else {
        None
    };
    let xattrs = if matches.get_flag("xattrs") {
        Some(true)
    } else if matches.get_flag("no-xattrs") {
        Some(false)
    } else {
        None
    };
    let numeric_ids = if matches.get_flag("numeric-ids") {
        Some(true)
    } else if matches.get_flag("no-numeric-ids") {
        Some(false)
    } else {
        None
    };
    let hard_links = if matches.get_flag("hard-links") {
        Some(true)
    } else if matches.get_flag("no-hard-links") {
        Some(false)
    } else {
        None
    };
    let sparse = if matches.get_flag("sparse") {
        Some(true)
    } else if matches.get_flag("no-sparse") {
        Some(false)
    } else {
        None
    };
    let copy_links = if matches.get_flag("copy-links") {
        Some(true)
    } else if matches.get_flag("no-copy-links") {
        Some(false)
    } else {
        None
    };
    let copy_dirlinks = matches.get_flag("copy-dirlinks");
    let copy_unsafe_links_option = if matches.get_flag("copy-unsafe-links") {
        Some(true)
    } else if matches.get_flag("no-copy-unsafe-links") || matches.get_flag("safe-links") {
        Some(false)
    } else {
        None
    };
    let keep_dirlinks = if matches.get_flag("keep-dirlinks") {
        Some(true)
    } else if matches.get_flag("no-keep-dirlinks") {
        Some(false)
    } else {
        None
    };
    let safe_links = matches.get_flag("safe-links") || copy_unsafe_links_option == Some(true);
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
    let relative = if matches.get_flag("relative") {
        Some(true)
    } else if matches.get_flag("no-relative") {
        Some(false)
    } else {
        None
    };
    let one_file_system = if matches.get_flag("one-file-system") {
        Some(true)
    } else if matches.get_flag("no-one-file-system") {
        Some(false)
    } else {
        None
    };
    let implied_dirs = if matches.get_flag("implied-dirs") {
        Some(true)
    } else if matches.get_flag("no-implied-dirs") {
        Some(false)
    } else {
        None
    };
    let msgs_to_stderr = matches.get_flag("msgs2stderr");
    let stats = matches.get_flag("stats");
    let partial_flag = matches.get_flag("partial") || matches.get_count("partial-progress") > 0;
    let preallocate = matches.get_flag("preallocate");
    let delay_updates = matches.get_flag("delay-updates") && !matches.get_flag("no-delay-updates");
    let partial_dir = matches
        .remove_one::<OsString>("partial-dir")
        .map(PathBuf::from);
    let partial = partial_flag || partial_dir.is_some();
    let temp_dir = matches
        .remove_one::<OsString>("temp-dir")
        .map(PathBuf::from);
    let link_dest_args: Vec<OsString> = matches
        .remove_many::<OsString>("link-dest")
        .map(|values| values.collect())
        .unwrap_or_default();
    let link_dests = link_dest_args.iter().map(PathBuf::from).collect();
    let link_destinations = link_dest_args;
    let remove_source_files =
        matches.get_flag("remove-source-files") || matches.get_flag("remove-sent-files");
    let inplace = if matches.get_flag("inplace") {
        Some(true)
    } else if matches.get_flag("no-inplace") {
        Some(false)
    } else {
        None
    };
    let append_verify_flag = matches.get_flag("append-verify");
    let append = if append_verify_flag || matches.get_flag("append") {
        Some(true)
    } else if matches.get_flag("no-append") {
        Some(false)
    } else {
        None
    };
    let whole_file = if matches.get_flag("whole-file") {
        Some(true)
    } else if matches.get_flag("no-whole-file") {
        Some(false)
    } else {
        None
    };
    let progress_setting = if matches.get_flag("progress") {
        ProgressSetting::PerFile
    } else if matches.get_flag("no-progress") {
        ProgressSetting::Disabled
    } else {
        ProgressSetting::Unspecified
    };
    let name_level = if matches.get_flag("itemize-changes") {
        NameOutputLevel::UpdatedOnly
    } else {
        NameOutputLevel::Disabled
    };
    let name_overridden = matches.get_flag("itemize-changes");
    let mut verbosity = matches.get_count("verbose") as u8;
    if matches.get_flag("quiet") {
        verbosity = 0;
    }
    let remainder = matches
        .remove_many::<OsString>("args")
        .map(|values| values.collect())
        .unwrap_or_default();
    let checksum = matches.get_flag("checksum");
    let size_only = matches.get_flag("size-only");
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
    let rsync_filter_shortcuts = rsync_filter_indices.len() as u8;
    let filter_args = collect_filter_arguments(&filters, &filter_indices, &rsync_filter_indices);
    let cvs_exclude = matches.get_flag("cvs-exclude");
    let files_from = matches
        .remove_many::<OsString>("files-from")
        .map(|values| values.collect())
        .unwrap_or_default();
    let from0 = matches.get_flag("from0");
    let info = matches
        .remove_many::<OsString>("info")
        .map(|values| values.collect())
        .unwrap_or_default();
    let debug = matches
        .remove_many::<OsString>("debug")
        .map(|values| values.collect())
        .unwrap_or_default();
    let ignore_existing = matches.get_flag("ignore-existing");
    let update = matches.get_flag("update");
    let password_file = matches.remove_one::<OsString>("password-file");
    let protocol = matches.remove_one::<OsString>("protocol");
    let timeout = matches.remove_one::<OsString>("timeout");
    let contimeout = matches.remove_one::<OsString>("contimeout");
    let out_format = matches.remove_one::<OsString>("out-format");
    let itemize_changes = matches.get_flag("itemize-changes");
    let no_motd = matches.get_flag("no-motd");

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
        archive,
        recursive,
        delete_mode,
        delete_excluded,
        backup,
        backup_dir,
        backup_suffix,
        checksum,
        checksum_choice,
        checksum_choice_arg,
        checksum_seed,
        size_only,
        ignore_existing,
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
        skip_compress,
        owner,
        group,
        chown,
        chmod,
        perms,
        super_mode,
        times,
        omit_dir_times,
        omit_link_times,
        acls,
        numeric_ids,
        hard_links,
        sparse,
        copy_links,
        copy_dirlinks,
        copy_unsafe_links: copy_unsafe_links_option,
        keep_dirlinks,
        safe_links,
        devices,
        specials,
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
        delay_updates,
        partial_dir,
        temp_dir,
        link_dests,
        remove_source_files,
        inplace,
        append,
        append_verify: append_verify_flag,
        msgs_to_stderr,
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
        out_format,
        daemon_port,
    })
}
