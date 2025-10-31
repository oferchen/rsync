mod config;
mod fallback;
mod filters;
mod messages;
mod metadata;
mod module_listing;
mod options;
mod summary;
mod validation;

use std::ffi::OsStr;
use std::io::Write;
use std::path::{Path, PathBuf};

use super::{
    extract_operands, load_file_list_operands, parse_bind_address_argument, parse_chown_argument,
    parse_protocol_version_arg, parse_timeout_argument, transfer_requires_remote,
};
use crate::frontend::{arguments::ParsedArgs, command_builder::clap_command, render_help};
use messages::{fail_with_custom_fallback, fail_with_message};
use metadata::MetadataSettings;
use module_listing::maybe_handle_module_listing;
use rsync_core::{
    client::{HumanReadableMode, TransferTimeout},
    message::Role,
    rsync_error,
    version::VersionInfoReport,
};
use rsync_logging::MessageSink;
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

    let settings_inputs = options::SettingsInputs {
        info: &info,
        debug: &debug,
        itemize_changes,
        out_format: out_format.as_ref(),
        fallback_out_format: out_format.clone(),
        initial_progress,
        initial_stats: stats,
        initial_name_level,
        initial_name_overridden,
        bwlimit: &bwlimit,
        max_delete: &max_delete,
        min_size: &min_size,
        max_size: &max_size,
        modify_window: &modify_window,
        compress_flag,
        no_compress,
        compress_level: &compress_level,
        skip_compress: &skip_compress,
    };

    let derived_settings = match options::derive_settings(stdout, stderr, settings_inputs) {
        options::SettingsOutcome::Proceed(settings) => settings,
        options::SettingsOutcome::Exit(code) => return code,
    };

    let options::DerivedSettings {
        out_format_template,
        fallback_out_format,
        progress_setting,
        progress_mode,
        stats,
        name_level,
        name_overridden,
        debug_flags_list,
        bandwidth_limit,
        fallback_bwlimit,
        max_delete_limit,
        min_size_limit,
        max_size_limit,
        modify_window_setting,
        compress,
        compress_disabled,
        compression_level_override,
        compress_level_cli,
        skip_compress_list,
        compression_setting,
    } = derived_settings;

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

    let fallback_file_list_entries = file_list_operands.clone();
    let fallback_remainder = remainder.clone();

    let append_for_fallback = if append_verify { Some(true) } else { append };
    let fallback_one_file_system = one_file_system;

    let delete_for_fallback =
        delete_mode.is_enabled() || delete_excluded || max_delete_limit.is_some();
    let fallback_inputs = fallback::FallbackInputs {
        required: fallback_required,
        info_flags: info.clone(),
        debug_flags: debug_flags_list.clone(),
        protect_args,
        progress_setting,
        delete_mode,
        delete_excluded,
        max_delete_limit,
        password_file: password_file.clone(),
        dry_run,
        list_only,
        remote_shell: remote_shell.clone(),
        remote_options: remote_options.clone(),
        connect_program: connect_program.clone(),
        daemon_port,
        bind_address: bind_address.clone(),
        human_readable: human_readable_setting,
        archive,
        delete_for_fallback,
        min_size: min_size.clone(),
        max_size: max_size.clone(),
        checksum,
        checksum_choice_arg: checksum_choice_arg.clone(),
        checksum_seed,
        size_only,
        ignore_existing,
        ignore_missing_args,
        update,
        modify_window: modify_window_setting,
        compress,
        compress_disabled,
        compress_level_cli: compress_level_cli.clone(),
        skip_compress: skip_compress.clone(),
        chown_spec: chown_spec.clone(),
        owner,
        group,
        chmod: chmod.clone(),
        perms,
        super_mode,
        times,
        omit_dir_times,
        omit_link_times,
        numeric_ids_option,
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
        progress_enabled: progress_mode.is_some(),
        stats,
        partial,
        preallocate,
        delay_updates,
        partial_dir: partial_dir.clone(),
        temp_dir: temp_dir.clone(),
        backup,
        backup_dir: backup_dir.clone().map(PathBuf::from),
        backup_suffix: backup_suffix.clone(),
        link_dests: link_dests.clone(),
        remove_source_files,
        append: append_for_fallback,
        append_verify,
        inplace,
        msgs_to_stderr,
        whole_file_option,
        fallback_bwlimit: fallback_bwlimit.clone(),
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
        files_from_used,
        file_list_entries: fallback_file_list_entries,
        from0,
        desired_protocol,
        timeout: timeout_setting,
        connect_timeout: connect_timeout_setting,
        out_format: fallback_out_format.clone(),
        no_motd,
        address_mode,
        rsync_path: rsync_path.clone(),
        remainder: fallback_remainder,
        #[cfg(feature = "acl")]
        acls,
        #[cfg(feature = "xattr")]
        xattrs,
        itemize_changes,
    };

    let fallback_args = match fallback::build_fallback_args(fallback_inputs, stderr) {
        Ok(args) => args,
        Err(code) => return code,
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

    let metadata = match metadata::compute_metadata_settings(
        archive,
        parsed_chown.as_ref(),
        owner,
        group,
        perms,
        super_mode,
        times,
        omit_dir_times,
        omit_link_times,
        devices,
        specials,
        hard_links,
        sparse,
        copy_links,
        copy_unsafe_links,
        keep_dirlinks,
        relative,
        fallback_one_file_system,
        &chmod,
    ) {
        Ok(settings) => settings,
        Err(message) => return fail_with_message(message, stderr),
    };

    let MetadataSettings {
        preserve_owner,
        preserve_group,
        preserve_permissions,
        preserve_times,
        omit_dir_times: omit_dir_times_setting,
        omit_link_times: omit_link_times_setting,
        preserve_devices,
        preserve_specials,
        preserve_hard_links,
        sparse,
        copy_links,
        copy_unsafe_links,
        keep_dirlinks: keep_dirlinks_flag,
        relative,
        one_file_system,
        chmod_modifiers,
    } = metadata;

    let prune_empty_dirs_flag = prune_empty_dirs.unwrap_or(false);
    let inplace_enabled = inplace.unwrap_or(false);
    let append_enabled = append.unwrap_or(false);
    let whole_file_enabled = whole_file_option.unwrap_or(true);

    let config_inputs = config::ConfigInputs {
        transfer_operands,
        address_mode,
        connect_program: connect_program.clone(),
        bind_address: bind_address.clone(),
        dry_run,
        list_only,
        delete_mode,
        delete_excluded,
        max_delete_limit,
        min_size_limit,
        max_size_limit,
        backup,
        backup_dir: backup_dir.clone().map(PathBuf::from),
        backup_suffix: backup_suffix.clone(),
        bandwidth_limit,
        compression_setting,
        compress,
        compression_level_override,
        owner: preserve_owner,
        owner_override: owner_override_value,
        group: preserve_group,
        group_override: group_override_value,
        chmod_modifiers: chmod_modifiers.clone(),
        permissions: preserve_permissions,
        times: preserve_times,
        modify_window_setting,
        omit_dir_times: omit_dir_times_setting,
        omit_link_times: omit_link_times_setting,
        devices: preserve_devices,
        specials: preserve_specials,
        checksum,
        checksum_seed,
        size_only,
        ignore_existing,
        ignore_missing_args,
        update,
        numeric_ids,
        hard_links: preserve_hard_links,
        sparse,
        copy_links,
        copy_dirlinks,
        copy_unsafe_links,
        keep_dirlinks: keep_dirlinks_flag,
        safe_links,
        relative_paths: relative,
        one_file_system,
        implied_dirs,
        human_readable: human_readable_enabled,
        mkpath,
        prune_empty_dirs: prune_empty_dirs_flag,
        verbosity,
        progress_mode,
        stats,
        debug_flags_list,
        partial,
        preallocate,
        partial_dir: partial_dir.clone(),
        temp_dir: temp_dir.clone(),
        delay_updates,
        link_dests: link_dests.clone(),
        remove_source_files,
        inplace: inplace_enabled,
        append: append_enabled,
        append_verify,
        whole_file: whole_file_enabled,
        timeout: timeout_setting,
        connect_timeout: connect_timeout_setting,
        checksum_choice,
        compare_destinations,
        copy_destinations,
        link_destinations,
        #[cfg(feature = "acl")]
        preserve_acls,
        #[cfg(feature = "xattr")]
        xattrs: xattrs.unwrap_or(false),
        skip_compress_list,
        itemize_changes,
        out_format_template: out_format_template.clone(),
        name_level,
    };

    let builder = config::build_base_config(config_inputs);

    let filter_inputs = filters::FilterInputs {
        exclude_from,
        include_from,
        excludes,
        includes,
        filters,
        cvs_exclude,
    };

    let builder = match filters::apply_filters(builder, filter_inputs, stderr) {
        Ok(builder) => builder,
        Err(code) => return code,
    };

    let config = builder.build();

    summary::execute_transfer(
        config,
        fallback_args,
        stdout,
        stderr,
        msgs_to_stderr,
        progress_mode,
        human_readable_mode,
        itemize_changes,
        stats,
        verbosity,
        list_only,
        out_format_template.as_ref(),
        name_level,
        name_overridden,
    )
}
