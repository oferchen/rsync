use std::io::Write;

use super::super::ClientError;
use super::args::RemoteFallbackArgs;
use crate::{message::Role, rsync_error};

/// Spawns the fallback `rsync` binary with arguments derived from [`RemoteFallbackArgs`].
///
/// The helper forwards the subprocess stdout/stderr into the provided writers and returns
/// the exit status code on success. Errors surface as [`ClientError`] instances with
/// fully formatted diagnostics.
pub fn run_remote_transfer_fallback<Out, Err>(
    stdout: &mut Out,
    stderr: &mut Err,
    args: RemoteFallbackArgs,
) -> Result<i32, ClientError>
where
    Out: Write,
    Err: Write,
{
    let _ = stdout;
    let _ = stderr;
    let _ = args;

    Err(fallback_error(
        "fallback to external rsync binaries is disabled in this build",
    ))
}

fn fallback_error(text: impl Into<String>) -> ClientError {
    let message = rsync_error!(1, "{}", text.into()).with_role(Role::Client);
    ClientError::new(1, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::{AddressMode, DeleteMode, IconvSetting, TransferTimeout};

    fn default_args() -> RemoteFallbackArgs {
        RemoteFallbackArgs {
            dry_run: false,
            list_only: false,
            remote_shell: None,
            remote_options: Vec::new(),
            connect_program: None,
            port: None,
            bind_address: None,
            sockopts: None,
            blocking_io: None,
            protect_args: None,
            human_readable: None,
            eight_bit_output: false,
            archive: false,
            recursive: None,
            inc_recursive: None,
            dirs: None,
            delete: false,
            delete_mode: DeleteMode::Disabled,
            delete_excluded: false,
            max_delete: None,
            min_size: None,
            max_size: None,
            block_size: None,
            checksum: None,
            checksum_choice: None,
            checksum_seed: None,
            size_only: false,
            ignore_times: false,
            ignore_existing: false,
            existing: false,
            ignore_missing_args: false,
            delete_missing_args: false,
            update: false,
            modify_window: None,
            compress: false,
            compress_disabled: false,
            compress_level: None,
            compress_choice: None,
            skip_compress: None,
            open_noatime: None,
            iconv: IconvSetting::Unspecified,
            stop_after: None,
            stop_at: None,
            chown: None,
            owner: None,
            group: None,
            usermap: None,
            groupmap: None,
            chmod: Vec::new(),
            executability: None,
            perms: None,
            super_mode: None,
            times: None,
            omit_dir_times: None,
            omit_link_times: None,
            numeric_ids: None,
            hard_links: None,
            links: None,
            copy_links: None,
            copy_dirlinks: false,
            copy_unsafe_links: None,
            keep_dirlinks: None,
            safe_links: false,
            sparse: None,
            fuzzy: None,
            devices: None,
            copy_devices: false,
            write_devices: false,
            specials: None,
            relative: None,
            one_file_system: None,
            implied_dirs: None,
            mkpath: false,
            prune_empty_dirs: None,
            verbosity: 0,
            progress: false,
            stats: false,
            itemize_changes: false,
            partial: false,
            preallocate: false,
            fsync: None,
            delay_updates: false,
            partial_dir: None,
            temp_directory: None,
            backup: false,
            backup_dir: None,
            backup_suffix: None,
            link_dests: Vec::new(),
            remove_source_files: false,
            append: None,
            append_verify: false,
            inplace: None,
            msgs_to_stderr: None,
            outbuf: None,
            whole_file: None,
            bwlimit: None,
            excludes: Vec::new(),
            includes: Vec::new(),
            exclude_from: Vec::new(),
            include_from: Vec::new(),
            filters: Vec::new(),
            rsync_filter_shortcuts: 0,
            compare_destinations: Vec::new(),
            copy_destinations: Vec::new(),
            link_destinations: Vec::new(),
            cvs_exclude: false,
            info_flags: Vec::new(),
            debug_flags: Vec::new(),
            files_from_used: false,
            file_list_entries: Vec::new(),
            from0: false,
            password_file: None,
            daemon_password: None,
            protocol: None,
            timeout: TransferTimeout::Default,
            connect_timeout: TransferTimeout::Default,
            out_format: None,
            log_file: None,
            log_file_format: None,
            no_motd: false,
            address_mode: AddressMode::Default,
            fallback_binary: None,
            rsync_path: None,
            remainder: Vec::new(),
            write_batch: None,
            only_write_batch: None,
            read_batch: None,
            #[cfg(all(unix, feature = "acl"))]
            acls: None,
            #[cfg(all(unix, feature = "xattr"))]
            xattrs: None,
        }
    }

    #[test]
    fn run_remote_transfer_fallback_returns_error() {
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let args = default_args();

        let result = run_remote_transfer_fallback(&mut stdout, &mut stderr, args);

        assert!(result.is_err());
        let error = result.unwrap_err();
        assert_eq!(error.exit_code(), 1);
        assert!(
            error
                .to_string()
                .contains("fallback to external rsync binaries is disabled")
        );
    }

    #[test]
    fn fallback_error_creates_client_error() {
        let error = fallback_error("test error message");

        assert_eq!(error.exit_code(), 1);
        assert!(error.to_string().contains("test error message"));
    }

    #[test]
    fn fallback_error_accepts_string() {
        let error = fallback_error(String::from("dynamic error"));

        assert_eq!(error.exit_code(), 1);
        assert!(error.to_string().contains("dynamic error"));
    }
}
