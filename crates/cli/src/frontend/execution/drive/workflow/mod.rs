#![deny(unsafe_code)]

mod fallback_plan;
mod operands;
mod preflight;

use fallback_plan::{FallbackArgumentsContext, build_fallback_arguments};
use operands::ensure_transfer_operands_present;
#[cfg(test)]
pub(crate) use operands::render_missing_operands_stdout;
use preflight::{
    maybe_print_help_or_version, resolve_bind_address, resolve_desired_protocol, resolve_timeout,
    validate_feature_support, validate_stdin_sources_conflict,
};

use super::super::{
    extract_operands, load_file_list_operands, parse_chown_argument, resolve_file_list_entries,
    transfer_requires_remote,
};
use super::messages::{fail_with_custom_fallback, fail_with_message};
use super::module_listing::{ModuleListingInputs, maybe_handle_module_listing};
use crate::frontend::{
    arguments::{ParsedArgs, StopRequest},
    execution::chown::ParsedChown,
};
use metadata::MetadataSettings;
use rsync_core::client::HumanReadableMode;
use rsync_logging::MessageSink;
use std::io::Write;
use std::path::PathBuf;

use super::{config, filters, metadata, options, summary, validation};
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
        remote_shell,
        connect_program,
        daemon_port,
        remote_options,
        rsync_path,
        protect_args,
        address_mode,
        bind_address: bind_address_raw,
        archive,
        recursive: _recursive,
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
        block_size,
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
        stop_after,
        stop_at,
        out_format,
    } = parsed;

    let password_file = password_file.map(PathBuf::from);
    let human_readable_setting = human_readable;
    let human_readable_mode = human_readable_setting.unwrap_or(HumanReadableMode::Disabled);
    let human_readable_enabled = human_readable_mode.is_enabled();

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

    if let Some(code) = maybe_print_help_or_version(show_help, show_version, program_name, stdout) {
        return code;
    }

    let bind_address = match resolve_bind_address(bind_address_raw.as_ref(), stderr) {
        Ok(address) => address,
        Err(code) => return code,
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
        block_size: &block_size,
        modify_window: &modify_window,
        compress_flag,
        no_compress,
        compress_level: &compress_level,
        skip_compress: &skip_compress,
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
        block_size_override,
        modify_window_setting,
        compress,
        compress_disabled,
        compression_level_override,
        compress_level_cli,
        skip_compress_list,
        compression_setting,
    } = match options::derive_settings(stdout, stderr, settings_inputs) {
        options::SettingsOutcome::Proceed(settings) => *settings,
        options::SettingsOutcome::Exit(code) => return code,
    };

    let numeric_ids_option = numeric_ids;
    let whole_file_option = whole_file;

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
        },
    ) {
        return exit_code;
    }

    let files_from_used = !files_from.is_empty();
    let implied_dirs_option = implied_dirs;
    let implied_dirs = implied_dirs_option.unwrap_or(true);
    let requires_remote_fallback = transfer_requires_remote(&remainder, &file_list_operands);
    let fallback_required = requires_remote_fallback;

    let fallback_context = FallbackArgumentsContext {
        required: fallback_required,
        info: &info,
        debug_flags: &debug_flags_list,
        protect_args,
        progress_setting,
        delete_mode,
        delete_excluded,
        max_delete_limit,
        password_file: &password_file,
        dry_run,
        list_only,
        remote_shell: remote_shell.as_ref(),
        remote_options: &remote_options,
        connect_program: connect_program.as_ref(),
        daemon_port,
        bind_address: bind_address.as_ref(),
        human_readable: human_readable_setting,
        archive,
        min_size: &min_size,
        max_size: &max_size,
        block_size: &block_size,
        checksum,
        checksum_choice_arg: checksum_choice_arg.as_ref(),
        checksum_seed,
        size_only,
        ignore_existing,
        ignore_missing_args,
        update,
        modify_window: modify_window_setting,
        compress,
        compress_disabled,
        compress_level_cli: compress_level_cli.as_ref(),
        skip_compress: skip_compress.as_ref(),
        parsed_chown: parsed_chown.as_ref(),
        owner,
        group,
        chmod: &chmod,
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
        one_file_system,
        implied_dirs: implied_dirs_option,
        mkpath,
        prune_empty_dirs,
        verbosity,
        progress_enabled: progress_mode.is_some(),
        stats,
        partial,
        preallocate,
        delay_updates,
        partial_dir: partial_dir.as_ref(),
        temp_dir: temp_dir.as_ref(),
        backup,
        backup_dir: &backup_dir,
        backup_suffix: &backup_suffix,
        link_dests: &link_dests,
        remove_source_files,
        append,
        append_verify,
        inplace,
        msgs_to_stderr,
        whole_file_option,
        fallback_bwlimit: fallback_bwlimit.as_ref(),
        excludes: &excludes,
        includes: &includes,
        exclude_from: &exclude_from,
        include_from: &include_from,
        filters: &filters,
        rsync_filter_shortcuts,
        compare_destinations: &compare_destinations,
        copy_destinations: &copy_destinations,
        link_destinations: &link_destinations,
        cvs_exclude,
        files_from_used,
        file_list_entries: &file_list_operands,
        from0,
        desired_protocol,
        timeout: timeout_setting,
        connect_timeout: connect_timeout_setting,
        fallback_out_format: fallback_out_format.as_ref(),
        no_motd,
        address_mode,
        rsync_path: rsync_path.as_ref(),
        remainder: &remainder,
        stop_request: stop_request.clone(),
        #[cfg(feature = "acl")]
        acls,
        #[cfg(feature = "xattr")]
        xattrs,
        itemize_changes,
    };
    let fallback_args = match build_fallback_arguments(fallback_context, stderr) {
        Ok(args) => args,
        Err(code) => return code,
    };

    let numeric_ids = numeric_ids_option.unwrap_or(false);

    if let Some(exit_code) = validation::validate_local_only_options(
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
        one_file_system,
        chmod: &chmod,
    }) {
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
        bind_address,
        dry_run,
        list_only,
        delete_mode,
        delete_excluded,
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
        stdout,
        stderr,
        summary::TransferExecutionInputs {
            config,
            fallback_args,
            msgs_to_stderr,
            progress_mode,
            human_readable_mode,
            itemize_changes,
            stats,
            verbosity,
            list_only,
            out_format_template: out_format_template.as_ref(),
            name_level,
            name_overridden,
        },
    )
}
