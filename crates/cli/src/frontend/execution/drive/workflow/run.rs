#![deny(unsafe_code)]

use super::operands::ensure_transfer_operands_present;
use super::preflight::{
    maybe_print_help_or_version, resolve_bind_address, resolve_desired_protocol, resolve_timeout,
    validate_feature_support, validate_stdin_sources_conflict,
};
use crate::frontend::execution::drive::messages::fail_with_message;
use crate::frontend::execution::drive::metadata::MetadataSettings;
use crate::frontend::execution::drive::module_listing::{
    ModuleListingInputs, maybe_handle_module_listing,
};
use crate::frontend::execution::drive::{config, filters, metadata, options, summary, validation};
use crate::frontend::{
    arguments::{ParsedArgs, StopRequest},
    execution::{
        chown::ParsedChown, extract_operands, load_file_list_operands, parse_chown_argument,
        resolve_file_list_entries, resolve_iconv_setting,
    },
};
use core::{client::HumanReadableMode, message::Role, rsync_error};
use engine::batch;
use logging::VerbosityConfig;
use logging_sink::MessageSink;
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::PathBuf;

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;

use crate::frontend::execution::{parse_stop_after_argument, parse_stop_at_argument};

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
        remote_shell: _,
        connect_program,
        daemon_port,
        remote_options,
        rsync_path: _,
        protect_args: _,
        old_args: _,
        address_mode,
        bind_address: bind_address_raw,
        sockopts,
        blocking_io,
        archive,
        recursive: _recursive,
        recursive_override,
        inc_recursive: _,
        dirs,
        delete_mode,
        delete_excluded,
        delete_missing_args,
        ignore_errors: _,
        backup,
        backup_dir,
        backup_suffix,
        checksum,
        checksum_choice,
        checksum_choice_arg: _,
        checksum_seed,
        size_only,
        ignore_times,
        ignore_existing,
        existing,
        ignore_missing_args,
        update,
        remainder: raw_remainder,
        bwlimit,
        max_delete,
        min_size,
        max_size,
        block_size,
        modify_window,
        compress: compress_flag,
        no_compress,
        compress_level,
        compress_choice,
        old_compress: _,
        new_compress: _,
        skip_compress,
        open_noatime,
        no_open_noatime,
        iconv,
        owner,
        group,
        chown,
        copy_as: _,
        usermap,
        groupmap,
        chmod,
        perms,
        executability,
        super_mode,
        fake_super: _,
        times,
        omit_dir_times,
        omit_link_times,
        atimes: _,
        crtimes: _,
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
        rsync_filter_shortcuts: _,
        files_from,
        from0,
        info,
        debug,
        numeric_ids,
        hard_links,
        links,
        sparse,
        fuzzy,
        copy_links,
        copy_dirlinks,
        copy_unsafe_links,
        keep_dirlinks,
        safe_links,
        munge_links: _,
        trust_sender: _,
        write_devices,
        devices,
        copy_devices,
        specials,
        force,
        qsort: _,
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
        eight_bit_output: _,
        partial,
        preallocate,
        fsync: fsync_option,
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
        append_verify,
        msgs_to_stderr: msgs_to_stderr_option,
        stderr_mode: _,
        outbuf: _,
        max_alloc: _,
        itemize_changes,
        whole_file,
        xattrs,
        no_motd,
        password_file,
        protocol,
        timeout,
        contimeout,
        stop_after,
        stop_at,
        out_format,
        no_iconv,
    } = parsed;

    let password_file = password_file.map(PathBuf::from);
    let human_readable_setting = human_readable;
    let human_readable_mode = human_readable_setting.unwrap_or(HumanReadableMode::Disabled);
    let human_readable_enabled = human_readable_mode.is_enabled();
    let msgs_to_stderr_enabled = msgs_to_stderr_option.unwrap_or(false);

    // Initialize verbosity system from -v level (--info/--debug flags applied later in derive_settings)
    let verbosity_config = VerbosityConfig::from_verbose_level(verbosity);
    logging::init(verbosity_config);

    if let Err(code) = validate_stdin_sources_conflict(&password_file, &files_from, stderr) {
        return code;
    }

    let desired_protocol = match resolve_desired_protocol(protocol.as_ref(), stderr) {
        Ok(protocol) => protocol,
        Err(code) => return code,
    };

    let timeout_setting = match resolve_timeout(timeout.as_ref(), stderr) {
        Ok(setting) => setting,
        Err(code) => return code,
    };

    let connect_timeout_setting = match resolve_timeout(contimeout.as_ref(), stderr) {
        Ok(setting) => setting,
        Err(code) => return code,
    };

    let stop_request = if let Some(value) = stop_after.as_ref() {
        match parse_stop_after_argument(value.as_os_str()) {
            Ok(deadline) => Some(StopRequest::new_stop_after(value.clone(), deadline)),
            Err(message) => return fail_with_message(message, stderr),
        }
    } else if let Some(value) = stop_at.as_ref() {
        match parse_stop_at_argument(value.as_os_str()) {
            Ok(deadline) => Some(StopRequest::new_stop_at(value.clone(), deadline)),
            Err(message) => return fail_with_message(message, stderr),
        }
    } else {
        None
    };

    let iconv_setting = match resolve_iconv_setting(iconv.as_deref(), no_iconv) {
        Ok(setting) => setting,
        Err(message) => return fail_with_message(message, stderr),
    };

    if let Some(code) = maybe_print_help_or_version(show_help, show_version, program_name, stdout) {
        return code;
    }

    let bind_address = match resolve_bind_address(bind_address_raw.as_ref(), stderr) {
        Ok(address) => address,
        Err(code) => return code,
    };

    let remainder = match extract_operands(raw_remainder) {
        Ok(operands) => operands,
        Err(unsupported) => return fail_with_message(unsupported.to_message(), stderr),
    };

    let settings_inputs = options::SettingsInputs {
        info: &info,
        debug: &debug,
        itemize_changes,
        out_format: out_format.as_ref(),
        initial_progress,
        initial_stats: stats,
        initial_name_level,
        initial_name_overridden,
        bwlimit: &bwlimit,
        max_delete: &max_delete,
        min_size: &min_size,
        max_size: &max_size,
        block_size: &block_size,
        modify_window: &modify_window,
        compress_flag,
        no_compress,
        compress_level: &compress_level,
        compress_choice: &compress_choice,
        skip_compress: &skip_compress,
        log_file: log_file.as_ref(),
        log_file_format: log_file_format.as_ref(),
    };

    let options::DerivedSettings {
        out_format_template,
        progress_setting: _,
        progress_mode,
        stats,
        name_level,
        name_overridden,
        debug_flags_list,
        bandwidth_limit,
        max_delete_limit,
        min_size_limit,
        max_size_limit,
        block_size_override,
        modify_window_setting,
        compress,
        compress_disabled: _,
        compression_level_override,
        compress_level_cli: _,
        skip_compress_list,
        compression_setting,
        compress_choice_cli: _,
        compression_algorithm,
        log_file_path,
        log_file_format_cli: _,
        log_file_template,
    } = match options::derive_settings(stdout, stderr, settings_inputs) {
        options::SettingsOutcome::Proceed(settings) => *settings,
        options::SettingsOutcome::Exit(code) => return code,
    };

    let log_file_path_buf = log_file_path.as_ref().map(PathBuf::from);

    let numeric_ids_option = numeric_ids;
    let whole_file_option = whole_file;
    let open_noatime_setting = if open_noatime {
        Some(true)
    } else if no_open_noatime {
        Some(false)
    } else {
        None
    };
    let open_noatime_enabled = open_noatime_setting.unwrap_or(false);

    #[allow(unused_variables)]
    let preserve_acls = acls.unwrap_or(false);

    if let Err(code) = validate_feature_support(preserve_acls, xattrs, stderr) {
        return code;
    }

    let parsed_chown = match chown.as_ref() {
        Some(value) => match parse_chown_argument(value.as_os_str()) {
            Ok(parsed) => Some(parsed),
            Err(message) => return fail_with_message(message, stderr),
        },
        None => None,
    };

    let owner_override_value = parsed_chown
        .as_ref()
        .and_then(|value: &ParsedChown| value.owner());
    let group_override_value = parsed_chown
        .as_ref()
        .and_then(|value: &ParsedChown| value.group());

    let mut file_list_operands = match load_file_list_operands(&files_from, from0) {
        Ok(operands) => operands,
        Err(message) => return fail_with_message(message, stderr),
    };

    resolve_file_list_entries(
        &mut file_list_operands,
        &remainder,
        relative.unwrap_or(false),
    );

    if let Some(exit_code) = maybe_handle_module_listing(
        stdout,
        stderr,
        ModuleListingInputs {
            file_list_operands: &file_list_operands,
            remainder: &remainder,
            daemon_port,
            desired_protocol,
            password_file: password_file.as_deref(),
            no_motd,
            address_mode,
            bind_address: bind_address.as_ref(),
            connect_program: connect_program.as_ref(),
            timeout_setting,
            connect_timeout_setting,
            sockopts: sockopts.as_ref(),
            blocking_io,
        },
    ) {
        return exit_code;
    }

    let implied_dirs_option = implied_dirs;
    let implied_dirs = implied_dirs_option.unwrap_or(true);
    let recursive_effective = !matches!(recursive_override, Some(false));

    // Create batch configuration if batch mode was requested
    let batch_config = if let Some(ref path) = write_batch {
        Some(batch::BatchConfig::new(
            batch::BatchMode::Write,
            path.to_string_lossy().into_owned(),
            32, // Default protocol version
        ))
    } else if let Some(ref path) = only_write_batch {
        Some(batch::BatchConfig::new(
            batch::BatchMode::OnlyWrite,
            path.to_string_lossy().into_owned(),
            32, // Default protocol version
        ))
    } else {
        read_batch.as_ref().map(|path| {
            batch::BatchConfig::new(
                batch::BatchMode::Read,
                path.to_string_lossy().into_owned(),
                32, // Default protocol version
            )
        })
    };

    // Remote transfers are handled natively by the SSH transport in core::client::run_client_internal
    // No fallback to system rsync is needed anymore

    let numeric_ids = numeric_ids_option.unwrap_or(false);

    let mut log_file_for_local = None;
    if let (Some(path), Some(template)) = (log_file_path_buf.as_ref(), log_file_template.as_ref()) {
        match open_log_file(path) {
            Ok(file) => {
                log_file_for_local = Some(summary::LogFileConfig {
                    file,
                    format: template.clone(),
                });
            }
            Err(error) => {
                let message =
                    rsync_error!(1, "failed to open log file {}: {error}", path.display())
                        .with_role(Role::Client);
                let _ = stderr.write(&message);
            }
        }
    }

    if let Some(exit_code) = validation::validate_local_only_options(
        desired_protocol,
        password_file.as_ref(),
        connect_program.as_ref(),
        parsed.rsync_path.as_ref(),
        &remote_options,
        stderr,
    ) {
        return exit_code;
    }

    let mut transfer_operands = Vec::with_capacity(file_list_operands.len() + remainder.len());
    transfer_operands.append(&mut file_list_operands);
    transfer_operands.extend(remainder);

    if let Err(code) =
        ensure_transfer_operands_present(&transfer_operands, program_name, stdout, stderr)
    {
        return code;
    }

    let metadata = match metadata::compute_metadata_settings(metadata::MetadataInputs {
        archive,
        parsed_chown: parsed_chown.as_ref(),
        owner,
        group,
        executability,
        usermap: usermap.as_ref(),
        groupmap: groupmap.as_ref(),
        perms,
        super_mode,
        times,
        omit_dir_times,
        omit_link_times,
        devices,
        specials,
        hard_links,
        links,
        sparse,
        copy_links,
        copy_unsafe_links,
        keep_dirlinks,
        relative,
        one_file_system,
        chmod: &chmod,
    }) {
        Ok(settings) => settings,
        Err(message) => return fail_with_message(message, stderr),
    };

    let MetadataSettings {
        preserve_owner,
        preserve_group,
        preserve_executability,
        preserve_permissions,
        preserve_times,
        omit_dir_times: omit_dir_times_setting,
        omit_link_times: omit_link_times_setting,
        preserve_devices,
        preserve_specials,
        preserve_hard_links,
        preserve_symlinks,
        sparse,
        copy_links,
        copy_unsafe_links,
        keep_dirlinks: keep_dirlinks_flag,
        relative,
        one_file_system,
        chmod_modifiers,
        user_mapping,
        group_mapping,
    } = metadata;

    let prune_empty_dirs_flag = prune_empty_dirs.unwrap_or(false);
    let fsync_flag = fsync_option.unwrap_or(false);
    let inplace_enabled = inplace.unwrap_or(false);
    let append_enabled = append.unwrap_or(false);
    let whole_file_enabled = whole_file_option.unwrap_or(true);

    let checksum_for_config = checksum.unwrap_or(false);
    let fuzzy_enabled = fuzzy.unwrap_or(false);

    let config_inputs = config::ConfigInputs {
        transfer_operands,
        address_mode,
        connect_program: connect_program.clone(),
        bind_address,
        sockopts: sockopts.clone(),
        blocking_io,
        dry_run,
        list_only,
        recursive: recursive_effective,
        dirs,
        delete_mode,
        delete_excluded,
        delete_missing_args,
        max_delete_limit,
        min_size_limit,
        max_size_limit,
        block_size_override,
        backup,
        backup_dir: backup_dir.clone().map(PathBuf::from),
        backup_suffix: backup_suffix.clone(),
        bandwidth_limit,
        compression_setting,
        compress,
        compression_level_override,
        compression_algorithm,
        open_noatime: open_noatime_enabled,
        owner: preserve_owner,
        owner_override: owner_override_value,
        group: preserve_group,
        group_override: group_override_value,
        chmod_modifiers: chmod_modifiers.clone(),
        user_mapping: user_mapping.clone(),
        group_mapping: group_mapping.clone(),
        executability: preserve_executability,
        permissions: preserve_permissions,
        times: preserve_times,
        modify_window_setting,
        omit_dir_times: omit_dir_times_setting,
        omit_link_times: omit_link_times_setting,
        devices: preserve_devices,
        copy_devices,
        write_devices: write_devices.unwrap_or(false),
        specials: preserve_specials,
        force_replacements: force.unwrap_or(false),
        checksum: checksum_for_config,
        checksum_seed,
        size_only,
        ignore_times,
        ignore_existing,
        existing_only: existing,
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
        fuzzy: fuzzy_enabled,
        links: preserve_symlinks,
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
        fsync: fsync_flag,
        partial_dir: partial_dir.clone(),
        temp_dir: temp_dir.clone(),
        delay_updates,
        link_dests: link_dests.clone(),
        remove_source_files,
        inplace: inplace_enabled,
        append: append_enabled,
        append_verify,
        whole_file: whole_file_enabled,
        force_fallback: batch_config.is_some(),
        timeout: timeout_setting,
        connect_timeout: connect_timeout_setting,
        stop_deadline: stop_request.as_ref().map(StopRequest::deadline),
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
        log_file_template: log_file_template.clone(),
        name_level,
        iconv: iconv_setting.clone(),
        remote_shell: parsed.remote_shell.clone(),
        rsync_path: parsed.rsync_path.clone(),
        batch_config,
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
        stdout,
        stderr,
        summary::TransferExecutionInputs {
            config,
            msgs_to_stderr: msgs_to_stderr_enabled,
            progress_mode,
            human_readable_mode,
            itemize_changes,
            stats,
            verbosity,
            list_only,
            out_format_template: out_format_template.as_ref(),
            name_level,
            name_overridden,
            log_file: log_file_for_local,
        },
    )
}

fn open_log_file(path: &PathBuf) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        options.mode(0o666);
    }
    options.open(path)
}
