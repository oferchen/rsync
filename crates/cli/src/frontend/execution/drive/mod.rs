mod messages;
mod module_listing;
mod validation;

use std::collections::HashSet;
use std::env;
use std::ffi::{OsStr, OsString};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use super::{
    CompressLevelArg, DEBUG_HELP_TEXT, INFO_HELP_TEXT, extract_operands,
    info_flags_include_progress, load_file_list_operands, parse_bandwidth_limit,
    parse_bind_address_argument, parse_chown_argument, parse_compress_level,
    parse_compress_level_argument, parse_debug_flags, parse_info_flags, parse_max_delete_argument,
    parse_modify_window_argument, parse_protocol_version_arg, parse_size_limit_argument,
    parse_timeout_argument, transfer_requires_remote,
};
use crate::frontend::{
    arguments::{BandwidthArgument, ParsedArgs},
    command_builder::clap_command,
    defaults::ITEMIZE_CHANGES_FORMAT,
    filter_rules::{
        FilterDirective, append_cvs_exclude_rules, append_filter_rules_from_files,
        apply_merge_directive, merge_directive_options, os_string_to_pattern,
        parse_filter_directive,
    },
    out_format::{OutFormatContext, parse_out_format},
    password::load_password_file,
    progress::{LiveProgress, NameOutputLevel, ProgressSetting, emit_transfer_summary},
    render_help,
};
use messages::{emit_message_with_fallback, fail_with_custom_fallback, fail_with_message};
use module_listing::maybe_handle_module_listing;
use rsync_compress::zlib::CompressionLevel;
use rsync_core::{
    client::{
        BandwidthLimit, ClientConfig, ClientOutcome, ClientProgressObserver, CompressionSetting,
        DeleteMode, DirMergeOptions, FilterRuleKind, FilterRuleSpec, HumanReadableMode,
        RemoteFallbackArgs, RemoteFallbackContext, TransferTimeout, parse_skip_compress_list,
        run_client_or_fallback, run_module_list_with_password_and_options, skip_compress_from_env,
    },
    message::Role,
    rsync_error,
    version::VersionInfoReport,
};
use rsync_logging::MessageSink;
use rsync_meta::ChmodModifiers;
use validation::validate_local_only_options;

pub(crate) fn with_output_writer<'a, Out, Err, R>(
    stdout: &'a mut Out,
    stderr: &'a mut MessageSink<Err>,
    use_stderr: bool,
    f: impl FnOnce(&'a mut dyn Write) -> R,
) -> R
where
    Out: Write + 'a,
    Err: Write + 'a,
{
    if use_stderr {
        let writer: &mut Err = stderr.writer_mut();
        f(writer)
    } else {
        f(stdout)
    }
}

pub(crate) fn execute<Out, Err>(
    parsed: ParsedArgs,
    stdout: &mut Out,
    stderr: &mut MessageSink<Err>,
) -> i32
where
    Out: Write,
    Err: Write,
{
    let ParsedArgs {
        program_name,
        show_help,
        show_version,
        human_readable,
        dry_run,
        list_only,
        remote_shell,
        connect_program,
        daemon_port,
        remote_options,
        rsync_path,
        protect_args,
        address_mode,
        bind_address: bind_address_raw,
        archive,
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
        remainder: raw_remainder,
        bwlimit,
        max_delete,
        min_size,
        max_size,
        modify_window,
        compress: compress_flag,
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
        excludes,
        includes,
        compare_destinations,
        copy_destinations,
        link_destinations,
        exclude_from,
        include_from,
        filters,
        cvs_exclude,
        rsync_filter_shortcuts,
        files_from,
        from0,
        info,
        debug,
        numeric_ids,
        hard_links,
        sparse,
        copy_links,
        copy_dirlinks,
        copy_unsafe_links,
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
        progress: initial_progress,
        name_level: initial_name_level,
        name_overridden: initial_name_overridden,
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
        append_verify,
        msgs_to_stderr,
        itemize_changes,
        whole_file,
        xattrs,
        no_motd,
        password_file,
        protocol,
        timeout,
        contimeout,
        out_format,
    } = parsed;

    let password_file = password_file.map(PathBuf::from);
    let human_readable_setting = human_readable;
    let human_readable_mode = human_readable_setting.unwrap_or(HumanReadableMode::Disabled);
    let human_readable_enabled = human_readable_mode.is_enabled();

    if password_file
        .as_deref()
        .is_some_and(|path| path == Path::new("-"))
        && files_from
            .iter()
            .any(|entry| entry.as_os_str() == OsStr::new("-"))
    {
        let message = rsync_error!(
            1,
            "--password-file=- cannot be combined with --files-from=- because both read from standard input"
        )
        .with_role(Role::Client);
        return fail_with_message(message, stderr);
    }
    let desired_protocol = match protocol {
        Some(value) => match parse_protocol_version_arg(value.as_os_str()) {
            Ok(version) => Some(version),
            Err(message) => return fail_with_message(message, stderr),
        },
        None => None,
    };

    let timeout_setting = match timeout {
        Some(value) => match parse_timeout_argument(value.as_os_str()) {
            Ok(setting) => setting,
            Err(message) => {
                return fail_with_message(message, stderr);
            }
        },
        None => TransferTimeout::Default,
    };

    let connect_timeout_setting = match contimeout {
        Some(value) => match parse_timeout_argument(value.as_os_str()) {
            Ok(setting) => setting,
            Err(message) => {
                return fail_with_message(message, stderr);
            }
        },
        None => TransferTimeout::Default,
    };

    let mut out_format_template = match out_format.as_ref() {
        Some(value) => match parse_out_format(value.as_os_str()) {
            Ok(template) => Some(template),
            Err(message) => {
                return fail_with_message(message, stderr);
            }
        },
        None => None,
    };

    let mut fallback_out_format = out_format.clone();

    if itemize_changes {
        if fallback_out_format.is_none() {
            fallback_out_format = Some(OsString::from(ITEMIZE_CHANGES_FORMAT));
        }
        if out_format_template.is_none() {
            out_format_template = Some(
                parse_out_format(OsStr::new(ITEMIZE_CHANGES_FORMAT))
                    .expect("default itemize-changes format parses"),
            );
        }
    }

    if show_help {
        let help = render_help(program_name);
        if stdout.write_all(help.as_bytes()).is_err() {
            let _ = writeln!(stdout, "{help}");
            return 1;
        }
        return 0;
    }

    if show_version {
        let report = VersionInfoReport::for_client_brand(program_name.brand());
        let banner = report.human_readable();
        if stdout.write_all(banner.as_bytes()).is_err() {
            return 1;
        }
        return 0;
    }

    let bind_address = match bind_address_raw {
        Some(value) => match parse_bind_address_argument(&value) {
            Ok(parsed) => Some(parsed),
            Err(message) => {
                return fail_with_message(message, stderr);
            }
        },
        None => None,
    };

    let remainder = match extract_operands(raw_remainder) {
        Ok(operands) => operands,
        Err(unsupported) => {
            let message = unsupported.to_message();
            let fallback = unsupported.fallback_text();
            return fail_with_custom_fallback(message, fallback, stderr);
        }
    };

    let mut compress = compress_flag;
    let mut progress_setting = initial_progress;
    let mut stats = stats;
    let mut name_level = initial_name_level;
    let mut name_overridden = initial_name_overridden;

    let mut debug_flags_list = Vec::new();

    if !info.is_empty() {
        match parse_info_flags(&info) {
            Ok(settings) => {
                if settings.help_requested {
                    if stdout.write_all(INFO_HELP_TEXT.as_bytes()).is_err() {
                        let _ = write!(stderr.writer_mut(), "{INFO_HELP_TEXT}");
                        return 1;
                    }
                    return 0;
                }

                match settings.progress {
                    ProgressSetting::Unspecified => {}
                    value => progress_setting = value,
                }
                if let Some(value) = settings.stats {
                    stats = value;
                }
                if let Some(level) = settings.name {
                    name_level = level;
                    name_overridden = true;
                }
            }
            Err(message) => {
                return fail_with_message(message, stderr);
            }
        }
    }

    if !debug.is_empty() {
        match parse_debug_flags(&debug) {
            Ok(settings) => {
                if settings.help_requested {
                    if stdout.write_all(DEBUG_HELP_TEXT.as_bytes()).is_err() {
                        let _ = write!(stderr.writer_mut(), "{DEBUG_HELP_TEXT}");
                        return 1;
                    }
                    return 0;
                }

                debug_flags_list = settings.flags;
            }
            Err(message) => {
                return fail_with_message(message, stderr);
            }
        }
    }

    let progress_mode = progress_setting.resolved();

    let bandwidth_limit = match bwlimit.as_ref() {
        Some(BandwidthArgument::Limit(value)) => match parse_bandwidth_limit(value.as_os_str()) {
            Ok(limit) => limit,
            Err(message) => return fail_with_message(message, stderr),
        },
        Some(BandwidthArgument::Disabled) | None => None,
    };

    let fallback_bwlimit = match (bandwidth_limit.as_ref(), bwlimit.as_ref()) {
        (Some(limit), _) => Some(limit.fallback_argument()),
        (None, Some(BandwidthArgument::Limit(_))) => {
            Some(BandwidthLimit::fallback_unlimited_argument())
        }
        (None, Some(BandwidthArgument::Disabled)) => {
            Some(BandwidthLimit::fallback_unlimited_argument())
        }
        (None, None) => None,
    };

    let max_delete_limit = match max_delete {
        Some(ref value) => match parse_max_delete_argument(value.as_os_str()) {
            Ok(limit) => Some(limit),
            Err(message) => return fail_with_message(message, stderr),
        },
        None => None,
    };

    let min_size_limit = match min_size.as_ref() {
        Some(value) => match parse_size_limit_argument(value.as_os_str(), "--min-size") {
            Ok(limit) => Some(limit),
            Err(message) => return fail_with_message(message, stderr),
        },
        None => None,
    };

    let max_size_limit = match max_size.as_ref() {
        Some(value) => match parse_size_limit_argument(value.as_os_str(), "--max-size") {
            Ok(limit) => Some(limit),
            Err(message) => return fail_with_message(message, stderr),
        },
        None => None,
    };

    let modify_window_setting = match modify_window.as_ref() {
        Some(value) => match parse_modify_window_argument(value.as_os_str()) {
            Ok(window) => Some(window),
            Err(message) => return fail_with_message(message, stderr),
        },
        None => None,
    };

    let compress_level_setting = match compress_level {
        Some(ref value) => match parse_compress_level(value.as_os_str()) {
            Ok(setting) => Some(setting),
            Err(message) => return fail_with_message(message, stderr),
        },
        None => None,
    };

    let mut compression_level_override = None;
    if let Some(ref setting) = compress_level_setting {
        match setting {
            CompressLevelArg::Disable => {
                compress = false;
            }
            CompressLevelArg::Level(level) => {
                if !no_compress {
                    compress = true;
                    compression_level_override = Some(CompressionLevel::precise(*level));
                }
            }
        }
    }

    let compress_disabled =
        no_compress || matches!(compress_level_setting, Some(CompressLevelArg::Disable));
    let compress_level_cli = match (compress_level_setting, compress_disabled) {
        (Some(CompressLevelArg::Level(level)), false) => {
            Some(OsString::from(level.get().to_string()))
        }
        (Some(CompressLevelArg::Disable), _) => Some(OsString::from("0")),
        _ => None,
    };

    let skip_compress_list = if let Some(value) = skip_compress.as_ref() {
        match parse_skip_compress_list(value.as_os_str()) {
            Ok(list) => Some(list),
            Err(message) => {
                return fail_with_message(message, stderr);
            }
        }
    } else {
        match skip_compress_from_env("RSYNC_SKIP_COMPRESS") {
            Ok(value) => value,
            Err(message) => {
                return fail_with_message(message, stderr);
            }
        }
    };

    let mut compression_setting = CompressionSetting::default();
    if let Some(ref value) = compress_level {
        match parse_compress_level_argument(value.as_os_str()) {
            Ok(setting) => {
                compression_setting = setting;
                compress = !setting.is_disabled();
            }
            Err(message) => return fail_with_message(message, stderr),
        }
    }

    let numeric_ids_option = numeric_ids;
    let whole_file_option = whole_file;

    #[allow(unused_variables)]
    let preserve_acls = acls.unwrap_or(false);

    #[cfg(not(feature = "acl"))]
    if preserve_acls {
        let message =
            rsync_error!(1, "POSIX ACLs are not supported on this client").with_role(Role::Client);
        let fallback = "POSIX ACLs are not supported on this client".to_string();
        return fail_with_custom_fallback(message, fallback, stderr);
    }

    let parsed_chown = match chown.as_ref() {
        Some(value) => match parse_chown_argument(value.as_os_str()) {
            Ok(parsed) => Some(parsed),
            Err(message) => return fail_with_message(message, stderr),
        },
        None => None,
    };

    let owner_override_value = parsed_chown.as_ref().and_then(|value| value.owner());
    let group_override_value = parsed_chown.as_ref().and_then(|value| value.group());
    let chown_spec = parsed_chown.as_ref().map(|value| value.spec().clone());

    #[cfg(not(feature = "xattr"))]
    if xattrs.unwrap_or(false) {
        let message = rsync_error!(1, "extended attributes are not supported on this client")
            .with_role(Role::Client);
        let fallback = "extended attributes are not supported on this client".to_string();
        return fail_with_custom_fallback(message, fallback, stderr);
    }

    let mut file_list_operands = match load_file_list_operands(&files_from, from0) {
        Ok(operands) => operands,
        Err(message) => return fail_with_message(message, stderr),
    };

    if let Some(exit_code) = maybe_handle_module_listing(
        &file_list_operands,
        &remainder,
        daemon_port,
        desired_protocol,
        password_file.as_deref(),
        no_motd,
        address_mode,
        bind_address.as_ref(),
        connect_program.as_ref(),
        timeout_setting,
        connect_timeout_setting,
        stdout,
        stderr,
    ) {
        return exit_code;
    }

    let files_from_used = !files_from.is_empty();
    let implied_dirs_option = implied_dirs;
    let implied_dirs = implied_dirs_option.unwrap_or(true);
    let requires_remote_fallback = transfer_requires_remote(&remainder, &file_list_operands);
    let fallback_required = requires_remote_fallback;

    let append_for_fallback = if append_verify { Some(true) } else { append };
    let fallback_one_file_system = one_file_system;

    let fallback_args = if fallback_required {
        let mut fallback_info_flags = info.clone();
        let fallback_debug_flags = debug_flags_list.clone();
        if protect_args.unwrap_or(false)
            && matches!(progress_setting, ProgressSetting::Unspecified)
            && !info_flags_include_progress(&fallback_info_flags)
        {
            fallback_info_flags.push(OsString::from("progress2"));
        }
        let delete_for_fallback =
            delete_mode.is_enabled() || delete_excluded || max_delete_limit.is_some();
        let daemon_password = match password_file.as_deref() {
            Some(path) if path == Path::new("-") => match load_password_file(path) {
                Ok(bytes) => Some(bytes),
                Err(message) => return fail_with_message(message, stderr),
            },
            _ => None,
        };
        Some(RemoteFallbackArgs {
            dry_run,
            list_only,
            remote_shell: remote_shell.clone(),
            remote_options: remote_options.clone(),
            connect_program: connect_program.clone(),
            port: daemon_port,
            bind_address: bind_address
                .as_ref()
                .map(|address| address.raw().to_os_string()),
            protect_args,
            human_readable: human_readable_setting,
            archive,
            delete: delete_for_fallback,
            delete_mode,
            delete_excluded,
            max_delete: max_delete_limit,
            min_size: min_size.clone(),
            max_size: max_size.clone(),
            checksum,
            checksum_choice: checksum_choice_arg.clone(),
            checksum_seed,
            size_only,
            ignore_existing,
            ignore_missing_args,
            update,
            modify_window: modify_window_setting,
            compress,
            compress_disabled,
            compress_level: compress_level_cli.clone(),
            skip_compress: skip_compress.clone(),
            chown: chown_spec.clone(),
            owner,
            group,
            chmod: chmod.clone(),
            perms,
            super_mode,
            times,
            omit_dir_times,
            omit_link_times,
            numeric_ids: numeric_ids_option,
            hard_links,
            copy_links,
            copy_dirlinks,
            copy_unsafe_links,
            keep_dirlinks,
            safe_links,
            sparse,
            devices,
            specials,
            relative,
            one_file_system: fallback_one_file_system,
            implied_dirs: implied_dirs_option,
            mkpath,
            prune_empty_dirs,
            verbosity,
            progress: progress_mode.is_some(),
            stats,
            partial,
            preallocate,
            delay_updates,
            partial_dir: partial_dir.clone(),
            temp_directory: temp_dir.clone(),
            backup,
            backup_dir: backup_dir.clone().map(PathBuf::from),
            backup_suffix: backup_suffix.clone(),
            link_dests: link_dests.clone(),
            remove_source_files,
            append: append_for_fallback,
            append_verify,
            inplace,
            msgs_to_stderr,
            whole_file: whole_file_option,
            bwlimit: fallback_bwlimit.clone(),
            excludes: excludes.clone(),
            includes: includes.clone(),
            exclude_from: exclude_from.clone(),
            include_from: include_from.clone(),
            filters: filters.clone(),
            rsync_filter_shortcuts,
            compare_destinations: compare_destinations.clone(),
            copy_destinations: copy_destinations.clone(),
            link_destinations: link_destinations.clone(),
            cvs_exclude,
            info_flags: fallback_info_flags,
            debug_flags: fallback_debug_flags,
            files_from_used,
            file_list_entries: file_list_operands.clone(),
            from0,
            password_file: password_file.clone(),
            daemon_password,
            protocol: desired_protocol,
            timeout: timeout_setting,
            connect_timeout: connect_timeout_setting,
            out_format: fallback_out_format.clone(),
            no_motd,
            address_mode,
            fallback_binary: None,
            rsync_path: rsync_path.clone(),
            remainder: remainder.clone(),
            #[cfg(feature = "acl")]
            acls,
            #[cfg(feature = "xattr")]
            xattrs,
            itemize_changes,
        })
    } else {
        None
    };

    let numeric_ids = numeric_ids_option.unwrap_or(false);

    if let Some(exit_code) = validate_local_only_options(
        fallback_required,
        desired_protocol,
        password_file.as_ref(),
        connect_program.as_ref(),
        rsync_path.as_ref(),
        &remote_options,
        stderr,
    ) {
        return exit_code;
    }

    let mut transfer_operands = Vec::with_capacity(file_list_operands.len() + remainder.len());
    transfer_operands.append(&mut file_list_operands);
    transfer_operands.extend(remainder);

    if transfer_operands.is_empty() {
        let usage = clap_command(program_name.as_str())
            .render_usage()
            .to_string();
        if writeln!(stdout, "{usage}").is_err() {
            let _ = writeln!(stderr.writer_mut(), "{usage}");
        }

        let message = rsync_error!(
            1,
            "missing source operands: supply at least one source and a destination"
        )
        .with_role(Role::Client);
        return fail_with_message(message, stderr);
    }

    let preserve_owner = if parsed_chown
        .as_ref()
        .and_then(|value| value.owner())
        .is_some()
    {
        true
    } else if let Some(value) = owner {
        value
    } else if super_mode == Some(true) {
        true
    } else {
        archive
    };
    let preserve_group = if parsed_chown
        .as_ref()
        .and_then(|value| value.group())
        .is_some()
    {
        true
    } else if let Some(value) = group {
        value
    } else if super_mode == Some(true) {
        true
    } else {
        archive
    };
    let preserve_permissions = if let Some(value) = perms {
        value
    } else if super_mode == Some(true) {
        true
    } else {
        archive
    };
    let preserve_times = times.unwrap_or(archive);
    let omit_dir_times_setting = omit_dir_times.unwrap_or(false);
    let omit_link_times_setting = omit_link_times.unwrap_or(false);
    let preserve_devices = devices.unwrap_or(archive);
    let preserve_specials = specials.unwrap_or(archive);
    let preserve_hard_links = hard_links.unwrap_or(false);
    let sparse = sparse.unwrap_or(false);
    let copy_links = copy_links.unwrap_or(false);
    let copy_unsafe_links = copy_unsafe_links.unwrap_or(false);
    let keep_dirlinks_flag = keep_dirlinks.unwrap_or(false);
    let relative = relative.unwrap_or(false);
    let one_file_system = fallback_one_file_system.unwrap_or(false);

    let mut chmod_modifiers: Option<ChmodModifiers> = None;
    for spec in &chmod {
        let spec_text = spec.to_string_lossy();
        let trimmed = spec_text.trim();
        match ChmodModifiers::parse(trimmed) {
            Ok(parsed) => {
                if let Some(existing) = &mut chmod_modifiers {
                    existing.extend(parsed);
                } else {
                    chmod_modifiers = Some(parsed);
                }
            }
            Err(error) => {
                let formatted = format!(
                    "failed to parse --chmod specification '{}': {}",
                    spec_text, error
                );
                let message = rsync_error!(1, formatted).with_role(Role::Client);
                return fail_with_message(message, stderr);
            }
        }
    }

    let mut builder = ClientConfig::builder()
        .transfer_args(transfer_operands)
        .address_mode(address_mode)
        .connect_program(connect_program.clone())
        .bind_address(bind_address.clone())
        .dry_run(dry_run)
        .list_only(list_only)
        .delete(delete_mode.is_enabled() || delete_excluded || max_delete_limit.is_some())
        .delete_excluded(delete_excluded)
        .max_delete(max_delete_limit)
        .min_file_size(min_size_limit)
        .max_file_size(max_size_limit)
        .backup(backup)
        .backup_directory(backup_dir.clone().map(PathBuf::from))
        .backup_suffix(backup_suffix.clone())
        .bandwidth_limit(bandwidth_limit)
        .compression_setting(compression_setting)
        .compress(compress)
        .compression_level(compression_level_override)
        .owner(preserve_owner)
        .owner_override(owner_override_value)
        .group(preserve_group)
        .group_override(group_override_value)
        .chmod(chmod_modifiers.clone())
        .permissions(preserve_permissions)
        .times(preserve_times)
        .modify_window(modify_window_setting)
        .omit_dir_times(omit_dir_times_setting)
        .omit_link_times(omit_link_times_setting)
        .devices(preserve_devices)
        .specials(preserve_specials)
        .checksum(checksum)
        .checksum_seed(checksum_seed)
        .size_only(size_only)
        .ignore_existing(ignore_existing)
        .ignore_missing_args(ignore_missing_args)
        .update(update)
        .numeric_ids(numeric_ids)
        .hard_links(preserve_hard_links)
        .sparse(sparse)
        .copy_links(copy_links)
        .copy_dirlinks(copy_dirlinks)
        .copy_unsafe_links(copy_unsafe_links)
        .keep_dirlinks(keep_dirlinks_flag)
        .safe_links(safe_links)
        .relative_paths(relative)
        .one_file_system(one_file_system)
        .implied_dirs(implied_dirs)
        .human_readable(human_readable_enabled)
        .mkpath(mkpath)
        .prune_empty_dirs(prune_empty_dirs.unwrap_or(false))
        .verbosity(verbosity)
        .progress(progress_mode.is_some())
        .stats(stats)
        .debug_flags(debug_flags_list.clone())
        .partial(partial)
        .preallocate(preallocate)
        .partial_directory(partial_dir.clone())
        .temp_directory(temp_dir.clone())
        .delay_updates(delay_updates)
        .extend_link_dests(link_dests.clone())
        .remove_source_files(remove_source_files)
        .inplace(inplace.unwrap_or(false))
        .append(append.unwrap_or(false))
        .append_verify(append_verify)
        .whole_file(whole_file_option.unwrap_or(true))
        .timeout(timeout_setting)
        .connect_timeout(connect_timeout_setting);

    if let Some(choice) = checksum_choice {
        builder = builder.checksum_choice(choice);
    }

    for path in &compare_destinations {
        builder = builder.compare_destination(PathBuf::from(path));
    }

    for path in &copy_destinations {
        builder = builder.copy_destination(PathBuf::from(path));
    }

    for path in &link_destinations {
        builder = builder.link_destination(PathBuf::from(path));
    }
    #[cfg(feature = "acl")]
    {
        builder = builder.acls(preserve_acls);
    }
    #[cfg(feature = "xattr")]
    {
        builder = builder.xattrs(xattrs.unwrap_or(false));
    }

    if let Some(list) = skip_compress_list {
        builder = builder.skip_compress(list);
    }

    builder = match delete_mode {
        DeleteMode::Before => builder.delete_before(true),
        DeleteMode::After => builder.delete_after(true),
        DeleteMode::Delay => builder.delete_delay(true),
        DeleteMode::During | DeleteMode::Disabled => builder,
    };

    let force_event_collection = itemize_changes
        || out_format_template.is_some()
        || !matches!(name_level, NameOutputLevel::Disabled);
    builder = builder.force_event_collection(force_event_collection);

    let mut filter_rules = Vec::new();
    if let Err(message) =
        append_filter_rules_from_files(&mut filter_rules, &exclude_from, FilterRuleKind::Exclude)
    {
        return fail_with_message(message, stderr);
    }
    filter_rules.extend(
        excludes
            .into_iter()
            .map(|pattern| FilterRuleSpec::exclude(os_string_to_pattern(pattern))),
    );
    if let Err(message) =
        append_filter_rules_from_files(&mut filter_rules, &include_from, FilterRuleKind::Include)
    {
        return fail_with_message(message, stderr);
    }
    filter_rules.extend(
        includes
            .into_iter()
            .map(|pattern| FilterRuleSpec::include(os_string_to_pattern(pattern))),
    );
    if cvs_exclude {
        if let Err(message) = append_cvs_exclude_rules(&mut filter_rules) {
            return fail_with_message(message, stderr);
        }
    }

    let mut merge_stack = HashSet::new();
    let merge_base = env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    for filter in &filters {
        match parse_filter_directive(filter.as_os_str()) {
            Ok(FilterDirective::Rule(spec)) => filter_rules.push(spec),
            Ok(FilterDirective::Merge(directive)) => {
                let effective_options =
                    merge_directive_options(&DirMergeOptions::default(), &directive);
                let directive = directive.with_options(effective_options);
                if let Err(message) = apply_merge_directive(
                    directive,
                    merge_base.as_path(),
                    &mut filter_rules,
                    &mut merge_stack,
                ) {
                    return fail_with_message(message, stderr);
                }
            }
            Ok(FilterDirective::Clear) => filter_rules.clear(),
            Err(message) => return fail_with_message(message, stderr),
        }
    }
    if !filter_rules.is_empty() {
        builder = builder.extend_filter_rules(filter_rules);
    }

    let config = builder.build();

    if let Some(args) = fallback_args {
        let outcome = {
            let mut stderr_writer = stderr.writer_mut();
            run_client_or_fallback(
                config,
                None,
                Some(RemoteFallbackContext::new(stdout, &mut stderr_writer, args)),
            )
        };

        return match outcome {
            Ok(ClientOutcome::Fallback(summary)) => summary.exit_code(),
            Ok(ClientOutcome::Local(_)) => {
                unreachable!("local outcome returned without fallback context")
            }
            Err(error) => {
                let message = error.message();
                let fallback = message.to_string();
                emit_message_with_fallback(message, &fallback, stderr);
                error.exit_code()
            }
        };
    }

    let mut live_progress = if let Some(mode) = progress_mode {
        Some(with_output_writer(
            stdout,
            stderr,
            msgs_to_stderr,
            |writer| LiveProgress::new(writer, mode, human_readable_mode),
        ))
    } else {
        None
    };

    let result = {
        let observer = live_progress
            .as_mut()
            .map(|observer| observer as &mut dyn ClientProgressObserver);
        run_client_or_fallback::<io::Sink, io::Sink>(config, observer, None)
    };

    match result {
        Ok(ClientOutcome::Local(summary)) => {
            let summary = *summary;
            let progress_rendered_live = live_progress.as_ref().is_some_and(LiveProgress::rendered);
            let suppress_updated_only_totals = itemize_changes && !stats && verbosity == 0;

            if let Some(observer) = live_progress {
                if let Err(error) = observer.finish() {
                    let _ = with_output_writer(stdout, stderr, msgs_to_stderr, |writer| {
                        writeln!(writer, "warning: failed to render progress output: {error}")
                    });
                }
            }

            let out_format_context = OutFormatContext::default();
            if let Err(error) = with_output_writer(stdout, stderr, msgs_to_stderr, |writer| {
                emit_transfer_summary(
                    &summary,
                    verbosity,
                    progress_mode,
                    stats,
                    progress_rendered_live,
                    list_only,
                    out_format_template.as_ref(),
                    &out_format_context,
                    name_level,
                    name_overridden,
                    human_readable_mode,
                    suppress_updated_only_totals,
                    writer,
                )
            }) {
                let _ = with_output_writer(stdout, stderr, msgs_to_stderr, |writer| {
                    writeln!(
                        writer,
                        "warning: failed to render transfer summary: {error}"
                    )
                });
            }
            0
        }
        Ok(ClientOutcome::Fallback(_)) => {
            unreachable!("fallback outcome returned without fallback args")
        }
        Err(error) => {
            if let Some(observer) = live_progress {
                if let Err(err) = observer.finish() {
                    let _ = with_output_writer(stdout, stderr, msgs_to_stderr, |writer| {
                        writeln!(writer, "warning: failed to render progress output: {err}")
                    });
                }
            }

            let message = error.message();
            emit_message_with_fallback(
                message,
                "rsync error: client functionality is unavailable in this build (code 1)",
                stderr,
            );
            error.exit_code()
        }
    }
}
