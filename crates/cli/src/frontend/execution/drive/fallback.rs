#![deny(unsafe_code)]

use std::ffi::OsString;
use std::io::Write;
use std::path::{Path, PathBuf};

use rsync_core::client::{
    AddressMode, DeleteMode, HumanReadableMode, RemoteFallbackArgs, TransferTimeout,
};
use rsync_logging::MessageSink;
use rsync_protocol::ProtocolVersion;

use crate::frontend::execution::info_flags_include_progress;
use crate::frontend::password::load_password_file;
use crate::frontend::progress::ProgressSetting;

use super::messages::fail_with_message;

/// Inputs required to construct [`RemoteFallbackArgs`].
pub(crate) struct FallbackInputs {
    pub(crate) required: bool,
    pub(crate) info_flags: Vec<OsString>,
    pub(crate) debug_flags: Vec<OsString>,
    pub(crate) protect_args: Option<bool>,
    pub(crate) progress_setting: ProgressSetting,
    pub(crate) delete_mode: DeleteMode,
    pub(crate) delete_excluded: bool,
    pub(crate) max_delete_limit: Option<u64>,
    pub(crate) password_file: Option<PathBuf>,
    pub(crate) dry_run: bool,
    pub(crate) list_only: bool,
    pub(crate) remote_shell: Option<OsString>,
    pub(crate) remote_options: Vec<OsString>,
    pub(crate) connect_program: Option<OsString>,
    pub(crate) daemon_port: Option<u16>,
    pub(crate) bind_address: Option<rsync_core::client::BindAddress>,
    pub(crate) human_readable: Option<HumanReadableMode>,
    pub(crate) archive: bool,
    pub(crate) delete_for_fallback: bool,
    pub(crate) min_size: Option<OsString>,
    pub(crate) max_size: Option<OsString>,
    pub(crate) checksum: bool,
    pub(crate) checksum_choice_arg: Option<OsString>,
    pub(crate) checksum_seed: Option<u32>,
    pub(crate) size_only: bool,
    pub(crate) ignore_existing: bool,
    pub(crate) ignore_missing_args: bool,
    pub(crate) update: bool,
    pub(crate) modify_window: Option<u64>,
    pub(crate) compress: bool,
    pub(crate) compress_disabled: bool,
    pub(crate) compress_level_cli: Option<OsString>,
    pub(crate) skip_compress: Option<OsString>,
    pub(crate) chown_spec: Option<OsString>,
    pub(crate) owner: Option<bool>,
    pub(crate) group: Option<bool>,
    pub(crate) chmod: Vec<OsString>,
    pub(crate) perms: Option<bool>,
    pub(crate) super_mode: Option<bool>,
    pub(crate) times: Option<bool>,
    pub(crate) omit_dir_times: Option<bool>,
    pub(crate) omit_link_times: Option<bool>,
    pub(crate) numeric_ids_option: Option<bool>,
    pub(crate) hard_links: Option<bool>,
    pub(crate) copy_links: Option<bool>,
    pub(crate) copy_dirlinks: bool,
    pub(crate) copy_unsafe_links: Option<bool>,
    pub(crate) keep_dirlinks: Option<bool>,
    pub(crate) safe_links: bool,
    pub(crate) sparse: Option<bool>,
    pub(crate) devices: Option<bool>,
    pub(crate) specials: Option<bool>,
    pub(crate) relative: Option<bool>,
    pub(crate) one_file_system: Option<bool>,
    pub(crate) implied_dirs: Option<bool>,
    pub(crate) mkpath: bool,
    pub(crate) prune_empty_dirs: Option<bool>,
    pub(crate) verbosity: u8,
    pub(crate) progress_enabled: bool,
    pub(crate) stats: bool,
    pub(crate) partial: bool,
    pub(crate) preallocate: bool,
    pub(crate) delay_updates: bool,
    pub(crate) partial_dir: Option<PathBuf>,
    pub(crate) temp_dir: Option<PathBuf>,
    pub(crate) backup: bool,
    pub(crate) backup_dir: Option<PathBuf>,
    pub(crate) backup_suffix: Option<OsString>,
    pub(crate) link_dests: Vec<PathBuf>,
    pub(crate) remove_source_files: bool,
    pub(crate) append: Option<bool>,
    pub(crate) append_verify: bool,
    pub(crate) inplace: Option<bool>,
    pub(crate) msgs_to_stderr: bool,
    pub(crate) whole_file_option: Option<bool>,
    pub(crate) fallback_bwlimit: Option<OsString>,
    pub(crate) excludes: Vec<OsString>,
    pub(crate) includes: Vec<OsString>,
    pub(crate) exclude_from: Vec<OsString>,
    pub(crate) include_from: Vec<OsString>,
    pub(crate) filters: Vec<OsString>,
    pub(crate) rsync_filter_shortcuts: u8,
    pub(crate) compare_destinations: Vec<OsString>,
    pub(crate) copy_destinations: Vec<OsString>,
    pub(crate) link_destinations: Vec<OsString>,
    pub(crate) cvs_exclude: bool,
    pub(crate) files_from_used: bool,
    pub(crate) file_list_entries: Vec<OsString>,
    pub(crate) from0: bool,
    pub(crate) desired_protocol: Option<ProtocolVersion>,
    pub(crate) timeout: TransferTimeout,
    pub(crate) connect_timeout: TransferTimeout,
    pub(crate) out_format: Option<OsString>,
    pub(crate) no_motd: bool,
    pub(crate) address_mode: AddressMode,
    pub(crate) rsync_path: Option<OsString>,
    pub(crate) remainder: Vec<OsString>,
    #[cfg(feature = "acl")]
    pub(crate) acls: Option<bool>,
    #[cfg(feature = "xattr")]
    pub(crate) xattrs: Option<bool>,
    pub(crate) itemize_changes: bool,
}

/// Builds the remote fallback arguments when required.
pub(crate) fn build_fallback_args<Err>(
    inputs: FallbackInputs,
    stderr: &mut MessageSink<Err>,
) -> Result<Option<RemoteFallbackArgs>, i32>
where
    Err: Write,
{
    if !inputs.required {
        return Ok(None);
    }

    let mut info_flags = inputs.info_flags;
    let debug_flags = inputs.debug_flags;
    if inputs.protect_args.unwrap_or(false)
        && matches!(inputs.progress_setting, ProgressSetting::Unspecified)
        && !info_flags_include_progress(&info_flags)
    {
        info_flags.push(OsString::from("progress2"));
    }

    let delete_for_fallback = inputs.delete_for_fallback;

    let daemon_password = match inputs.password_file.as_ref() {
        Some(path) if path == Path::new("-") => match load_password_file(path) {
            Ok(bytes) => Some(bytes),
            Err(message) => return Err(fail_with_message(message, stderr)),
        },
        _ => None,
    };

    let args = RemoteFallbackArgs {
        dry_run: inputs.dry_run,
        list_only: inputs.list_only,
        remote_shell: inputs.remote_shell,
        remote_options: inputs.remote_options,
        connect_program: inputs.connect_program,
        port: inputs.daemon_port,
        bind_address: inputs
            .bind_address
            .map(|address| address.raw().to_os_string()),
        protect_args: inputs.protect_args,
        human_readable: inputs.human_readable,
        archive: inputs.archive,
        delete: delete_for_fallback,
        delete_mode: inputs.delete_mode,
        delete_excluded: inputs.delete_excluded,
        max_delete: inputs.max_delete_limit,
        min_size: inputs.min_size,
        max_size: inputs.max_size,
        checksum: inputs.checksum,
        checksum_choice: inputs.checksum_choice_arg,
        checksum_seed: inputs.checksum_seed,
        size_only: inputs.size_only,
        ignore_existing: inputs.ignore_existing,
        ignore_missing_args: inputs.ignore_missing_args,
        update: inputs.update,
        modify_window: inputs.modify_window,
        compress: inputs.compress,
        compress_disabled: inputs.compress_disabled,
        compress_level: inputs.compress_level_cli,
        skip_compress: inputs.skip_compress,
        chown: inputs.chown_spec,
        owner: inputs.owner,
        group: inputs.group,
        chmod: inputs.chmod,
        perms: inputs.perms,
        super_mode: inputs.super_mode,
        times: inputs.times,
        omit_dir_times: inputs.omit_dir_times,
        omit_link_times: inputs.omit_link_times,
        numeric_ids: inputs.numeric_ids_option,
        hard_links: inputs.hard_links,
        copy_links: inputs.copy_links,
        copy_dirlinks: inputs.copy_dirlinks,
        copy_unsafe_links: inputs.copy_unsafe_links,
        keep_dirlinks: inputs.keep_dirlinks,
        safe_links: inputs.safe_links,
        sparse: inputs.sparse,
        devices: inputs.devices,
        specials: inputs.specials,
        relative: inputs.relative,
        one_file_system: inputs.one_file_system,
        implied_dirs: inputs.implied_dirs,
        mkpath: inputs.mkpath,
        prune_empty_dirs: inputs.prune_empty_dirs,
        verbosity: inputs.verbosity,
        progress: inputs.progress_enabled,
        stats: inputs.stats,
        partial: inputs.partial,
        preallocate: inputs.preallocate,
        delay_updates: inputs.delay_updates,
        partial_dir: inputs.partial_dir,
        temp_directory: inputs.temp_dir,
        backup: inputs.backup,
        backup_dir: inputs.backup_dir,
        backup_suffix: inputs.backup_suffix,
        link_dests: inputs.link_dests,
        remove_source_files: inputs.remove_source_files,
        append: inputs.append,
        append_verify: inputs.append_verify,
        inplace: inputs.inplace,
        msgs_to_stderr: inputs.msgs_to_stderr,
        whole_file: inputs.whole_file_option,
        bwlimit: inputs.fallback_bwlimit,
        excludes: inputs.excludes,
        includes: inputs.includes,
        exclude_from: inputs.exclude_from,
        include_from: inputs.include_from,
        filters: inputs.filters,
        rsync_filter_shortcuts: inputs.rsync_filter_shortcuts,
        compare_destinations: inputs.compare_destinations,
        copy_destinations: inputs.copy_destinations,
        link_destinations: inputs.link_destinations,
        cvs_exclude: inputs.cvs_exclude,
        info_flags,
        debug_flags,
        files_from_used: inputs.files_from_used,
        file_list_entries: inputs.file_list_entries,
        from0: inputs.from0,
        password_file: inputs.password_file,
        daemon_password,
        protocol: inputs.desired_protocol,
        timeout: inputs.timeout,
        connect_timeout: inputs.connect_timeout,
        out_format: inputs.out_format,
        no_motd: inputs.no_motd,
        address_mode: inputs.address_mode,
        fallback_binary: None,
        rsync_path: inputs.rsync_path,
        remainder: inputs.remainder,
        #[cfg(feature = "acl")]
        acls: inputs.acls,
        #[cfg(feature = "xattr")]
        xattrs: inputs.xattrs,
        itemize_changes: inputs.itemize_changes,
    };

    Ok(Some(args))
}
