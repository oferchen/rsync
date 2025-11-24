use std::ffi::{OsStr, OsString};
use std::path::Path;

use tempfile::NamedTempFile;

use super::super::args::RemoteFallbackArgs;
use super::helpers::{fallback_error, prepare_file_list, push_human_readable, push_toggle};
use crate::client::{AddressMode, ClientError, DeleteMode, IconvSetting, TransferTimeout};
use crate::fallback::{
    CLIENT_FALLBACK_ENV, FallbackOverride, describe_missing_fallback_binary,
    fallback_binary_is_self, fallback_binary_path, fallback_override,
};

/// Prepared command invocation for the legacy fallback binary.
pub(crate) struct PreparedInvocation {
    pub binary: OsString,
    pub args: Vec<OsString>,
    pub daemon_password: Option<Vec<u8>>,
    pub files_from_temp: Option<NamedTempFile>,
}

/// Builds the command-line arguments and supporting artefacts required to spawn the
/// legacy `rsync` fallback process.
pub(crate) fn prepare_invocation(
    args: RemoteFallbackArgs,
) -> Result<PreparedInvocation, ClientError> {
    let RemoteFallbackArgs {
        dry_run,
        list_only,
        remote_shell,
        remote_options,
        connect_program,
        port,
        bind_address,
        sockopts,
        blocking_io,
        protect_args,
        human_readable: human_readable_mode,
        eight_bit_output,
        archive,
        recursive,
        inc_recursive,
        dirs,
        delete,
        delete_mode,
        delete_excluded,
        max_delete,
        min_size,
        max_size,
        block_size,
        checksum,
        checksum_choice,
        checksum_seed,
        size_only,
        ignore_times,
        ignore_existing,
        existing,
        ignore_missing_args,
        delete_missing_args,
        update,
        modify_window,
        compress,
        compress_disabled,
        compress_level,
        compress_choice,
        skip_compress,
        open_noatime,
        iconv,
        stop_after,
        stop_at,
        chown,
        owner,
        group,
        usermap,
        groupmap,
        chmod,
        executability,
        perms,
        super_mode,
        times,
        omit_dir_times,
        omit_link_times,
        numeric_ids,
        hard_links,
        links,
        copy_links,
        copy_dirlinks,
        copy_unsafe_links,
        keep_dirlinks,
        safe_links,
        sparse,
        fuzzy,
        devices,
        copy_devices,
        write_devices,
        specials,
        relative,
        one_file_system,
        implied_dirs,
        mkpath,
        prune_empty_dirs,
        verbosity,
        progress,
        stats,
        itemize_changes,
        partial,
        preallocate,
        fsync,
        delay_updates,
        partial_dir,
        temp_directory,
        backup,
        backup_dir,
        backup_suffix,
        link_dests,
        remove_source_files,
        append,
        append_verify,
        inplace,
        msgs_to_stderr,
        outbuf,
        whole_file,
        bwlimit,
        excludes,
        includes,
        exclude_from,
        include_from,
        filters,
        rsync_filter_shortcuts,
        compare_destinations,
        copy_destinations,
        link_destinations,
        cvs_exclude,
        info_flags,
        debug_flags,
        files_from_used,
        file_list_entries,
        from0,
        password_file,
        daemon_password,
        protocol,
        timeout,
        connect_timeout,
        out_format,
        log_file,
        log_file_format,
        no_motd,
        address_mode,
        fallback_binary,
        rsync_path,
        mut remainder,
        write_batch,
        only_write_batch,
        read_batch,
        #[cfg(feature = "acl")]
        acls,
        #[cfg(feature = "xattr")]
        xattrs,
    } = args;

    let mut command_args = Vec::new();
    if archive {
        command_args.push(OsString::from("-a"));
    }
    match recursive {
        Some(true) => command_args.push(OsString::from("--recursive")),
        Some(false) => command_args.push(OsString::from("--no-recursive")),
        None => {}
    }
    match inc_recursive {
        Some(true) => command_args.push(OsString::from("--inc-recursive")),
        Some(false) => command_args.push(OsString::from("--no-inc-recursive")),
        None => {}
    }
    match dirs {
        Some(true) => command_args.push(OsString::from("--dirs")),
        Some(false) => command_args.push(OsString::from("--no-dirs")),
        None => {}
    }
    if dry_run {
        command_args.push(OsString::from("--dry-run"));
    }
    if list_only {
        command_args.push(OsString::from("--list-only"));
    }
    if delete {
        command_args.push(OsString::from("--delete"));
        match delete_mode {
            DeleteMode::Before => command_args.push(OsString::from("--delete-before")),
            DeleteMode::After => command_args.push(OsString::from("--delete-after")),
            DeleteMode::Delay => command_args.push(OsString::from("--delete-delay")),
            DeleteMode::During => command_args.push(OsString::from("--delete-during")),
            DeleteMode::Disabled => {}
        }
    }
    if delete_excluded {
        command_args.push(OsString::from("--delete-excluded"));
    }
    if backup {
        command_args.push(OsString::from("--backup"));
    }
    if let Some(dir) = backup_dir {
        command_args.push(OsString::from("--backup-dir"));
        command_args.push(dir.into_os_string());
    }
    if let Some(suffix) = backup_suffix {
        command_args.push(OsString::from("--suffix"));
        command_args.push(suffix);
    }
    if let Some(limit) = max_delete {
        let mut arg = OsString::from("--max-delete=");
        arg.push(limit.to_string());
        command_args.push(arg);
    }
    if let Some(spec) = min_size {
        let mut arg = OsString::from("--min-size=");
        arg.push(spec);
        command_args.push(arg);
    }
    if let Some(spec) = max_size {
        let mut arg = OsString::from("--max-size=");
        arg.push(spec);
        command_args.push(arg);
    }
    if let Some(spec) = block_size {
        let mut arg = OsString::from("--block-size=");
        arg.push(spec);
        command_args.push(arg);
    }
    match checksum {
        Some(true) => command_args.push(OsString::from("--checksum")),
        Some(false) => command_args.push(OsString::from("--no-checksum")),
        None => {}
    }
    if let Some(choice) = checksum_choice {
        let mut arg = OsString::from("--checksum-choice=");
        arg.push(choice);
        command_args.push(arg);
    }
    if let Some(seed) = checksum_seed {
        let mut arg = OsString::from("--checksum-seed=");
        arg.push(seed.to_string());
        command_args.push(arg);
    }
    if size_only {
        command_args.push(OsString::from("--size-only"));
    }
    if ignore_times {
        command_args.push(OsString::from("--ignore-times"));
    }
    if ignore_existing {
        command_args.push(OsString::from("--ignore-existing"));
    }
    if existing {
        command_args.push(OsString::from("--existing"));
    }
    if ignore_missing_args {
        command_args.push(OsString::from("--ignore-missing-args"));
    }
    if delete_missing_args {
        command_args.push(OsString::from("--delete-missing-args"));
    }
    if update {
        command_args.push(OsString::from("--update"));
    }
    if let Some(window) = modify_window {
        let mut arg = OsString::from("--modify-window=");
        arg.push(window.to_string());
        command_args.push(arg);
    }
    if compress {
        command_args.push(OsString::from("--compress"));
    } else if compress_disabled {
        command_args.push(OsString::from("--no-compress"));
        if whole_file.is_none() {
            command_args.push(OsString::from("--no-whole-file"));
        }
    }
    if let Some(level) = compress_level {
        command_args.push(OsString::from("--compress-level"));
        command_args.push(level);
    }

    if let Some(choice) = compress_choice {
        let mut arg = OsString::from("--compress-choice=");
        arg.push(choice);
        command_args.push(arg);
    }

    if let Some(spec) = skip_compress {
        let mut arg = OsString::from("--skip-compress=");
        arg.push(spec);
        command_args.push(arg);
    }
    if let Some(flag) = open_noatime {
        command_args.push(if flag {
            OsString::from("--open-noatime")
        } else {
            OsString::from("--no-open-noatime")
        });
    }
    match &iconv {
        IconvSetting::Unspecified => {}
        IconvSetting::Disabled => command_args.push(OsString::from("--no-iconv")),
        IconvSetting::LocaleDefault => {
            command_args.push(OsString::from("--iconv=."));
        }
        IconvSetting::Explicit { local, remote } => {
            let mut arg = OsString::from("--iconv=");
            arg.push(local);
            if let Some(remote) = remote {
                arg.push(",");
                arg.push(remote);
            }
            command_args.push(arg);
        }
    }
    if let Some(value) = stop_after {
        let mut arg = OsString::from("--stop-after=");
        arg.push(value);
        command_args.push(arg);
    }
    if let Some(value) = stop_at {
        let mut arg = OsString::from("--stop-at=");
        arg.push(value);
        command_args.push(arg);
    }
    if let Some(prefix) = write_batch {
        let mut arg = OsString::from("--write-batch=");
        arg.push(prefix);
        command_args.push(arg);
    }
    if let Some(prefix) = only_write_batch {
        let mut arg = OsString::from("--only-write-batch=");
        arg.push(prefix);
        command_args.push(arg);
    }
    if let Some(prefix) = read_batch {
        let mut arg = OsString::from("--read-batch=");
        arg.push(prefix);
        command_args.push(arg);
    }

    if let Some(spec) = chown {
        let mut arg = OsString::from("--chown=");
        arg.push(spec);
        command_args.push(arg);
    }
    if let Some(spec) = usermap {
        let mut arg = OsString::from("--usermap=");
        arg.push(&spec);
        command_args.push(arg);
    }
    if let Some(spec) = groupmap {
        let mut arg = OsString::from("--groupmap=");
        arg.push(&spec);
        command_args.push(arg);
    }

    push_toggle(&mut command_args, "--owner", "--no-owner", owner);
    push_toggle(&mut command_args, "--group", "--no-group", group);
    for spec in chmod {
        let mut arg = OsString::from("--chmod=");
        arg.push(&spec);
        command_args.push(arg);
    }
    push_toggle(
        &mut command_args,
        "--executability",
        "--no-executability",
        executability,
    );
    push_toggle(&mut command_args, "--perms", "--no-perms", perms);
    push_toggle(&mut command_args, "--super", "--no-super", super_mode);
    push_toggle(&mut command_args, "--times", "--no-times", times);
    push_toggle(
        &mut command_args,
        "--omit-dir-times",
        "--no-omit-dir-times",
        omit_dir_times,
    );
    push_toggle(
        &mut command_args,
        "--omit-link-times",
        "--no-omit-link-times",
        omit_link_times,
    );
    push_toggle(
        &mut command_args,
        "--numeric-ids",
        "--no-numeric-ids",
        numeric_ids,
    );
    push_toggle(
        &mut command_args,
        "--hard-links",
        "--no-hard-links",
        hard_links,
    );
    push_toggle(&mut command_args, "--links", "--no-links", links);
    push_toggle(
        &mut command_args,
        "--copy-links",
        "--no-copy-links",
        copy_links,
    );
    if copy_dirlinks {
        command_args.push(OsString::from("--copy-dirlinks"));
    }
    push_toggle(
        &mut command_args,
        "--copy-unsafe-links",
        "--no-copy-unsafe-links",
        copy_unsafe_links,
    );
    push_toggle(
        &mut command_args,
        "--keep-dirlinks",
        "--no-keep-dirlinks",
        keep_dirlinks,
    );
    if safe_links {
        command_args.push(OsString::from("--safe-links"));
    }
    push_toggle(&mut command_args, "--sparse", "--no-sparse", sparse);
    push_toggle(&mut command_args, "--fuzzy", "--no-fuzzy", fuzzy);
    push_toggle(&mut command_args, "--devices", "--no-devices", devices);
    if copy_devices {
        command_args.push(OsString::from("--copy-devices"));
    }
    if write_devices {
        command_args.push(OsString::from("--write-devices"));
    }
    push_toggle(&mut command_args, "--specials", "--no-specials", specials);
    push_toggle(&mut command_args, "--relative", "--no-relative", relative);
    push_toggle(
        &mut command_args,
        "--one-file-system",
        "--no-one-file-system",
        one_file_system,
    );
    push_toggle(
        &mut command_args,
        "--implied-dirs",
        "--no-implied-dirs",
        implied_dirs,
    );
    if mkpath {
        command_args.push(OsString::from("--mkpath"));
    }
    push_toggle(
        &mut command_args,
        "--prune-empty-dirs",
        "--no-prune-empty-dirs",
        prune_empty_dirs,
    );
    push_toggle(&mut command_args, "--inplace", "--no-inplace", inplace);
    #[cfg(feature = "acl")]
    push_toggle(&mut command_args, "--acls", "--no-acls", acls);
    push_toggle(
        &mut command_args,
        "--whole-file",
        "--no-whole-file",
        whole_file,
    );
    #[cfg(feature = "xattr")]
    push_toggle(&mut command_args, "--xattrs", "--no-xattrs", xattrs);

    for _ in 0..verbosity {
        command_args.push(OsString::from("-v"));
    }
    if progress {
        command_args.push(OsString::from("--progress"));
    }
    if stats {
        command_args.push(OsString::from("--stats"));
    }
    if itemize_changes {
        command_args.push(OsString::from("--itemize-changes"));
    }
    if partial {
        command_args.push(OsString::from("--partial"));
    }
    if preallocate {
        command_args.push(OsString::from("--preallocate"));
    }
    push_toggle(&mut command_args, "--fsync", "--no-fsync", fsync);
    if delay_updates {
        command_args.push(OsString::from("--delay-updates"));
    }
    if let Some(dir) = partial_dir {
        command_args.push(OsString::from("--partial-dir"));
        command_args.push(dir.into_os_string());
    }
    if let Some(dir) = temp_directory {
        command_args.push(OsString::from("--temp-dir"));
        command_args.push(dir.into_os_string());
    }
    for dir in link_dests {
        let mut arg = OsString::from("--link-dest=");
        arg.push(dir);
        command_args.push(arg);
    }
    if remove_source_files {
        command_args.push(OsString::from("--remove-source-files"));
    }
    if append_verify {
        command_args.push(OsString::from("--append-verify"));
    } else {
        push_toggle(&mut command_args, "--append", "--no-append", append);
    }
    push_toggle(
        &mut command_args,
        "--msgs2stderr",
        "--no-msgs2stderr",
        msgs_to_stderr,
    );
    if let Some(mode) = outbuf {
        let mut arg = OsString::from("--outbuf=");
        arg.push(mode);
        command_args.push(arg);
    }

    if let Some(enabled) = protect_args {
        let flag = if enabled {
            "--protect-args"
        } else {
            "--no-protect-args"
        };
        command_args.push(OsString::from(flag));
    }

    push_human_readable(&mut command_args, human_readable_mode);

    if eight_bit_output {
        command_args.push(OsString::from("--8-bit-output"));
    }

    if let Some(limit) = bwlimit {
        command_args.push(OsString::from("--bwlimit"));
        command_args.push(limit);
    }

    if let Some(format) = out_format {
        command_args.push(OsString::from("--out-format"));
        command_args.push(format);
    }
    if let Some(path) = log_file {
        command_args.push(OsString::from("--log-file"));
        command_args.push(path.into_os_string());
    }
    if let Some(format) = log_file_format {
        command_args.push(OsString::from("--log-file-format"));
        command_args.push(format);
    }

    for exclude in excludes {
        command_args.push(OsString::from("--exclude"));
        command_args.push(exclude);
    }
    for include in includes {
        command_args.push(OsString::from("--include"));
        command_args.push(include);
    }
    for path in exclude_from {
        command_args.push(OsString::from("--exclude-from"));
        command_args.push(path);
    }
    for path in include_from {
        command_args.push(OsString::from("--include-from"));
        command_args.push(path);
    }
    if cvs_exclude {
        command_args.push(OsString::from("--cvs-exclude"));
    }
    for _ in 0..rsync_filter_shortcuts {
        command_args.push(OsString::from("-F"));
    }
    for filter in filters {
        command_args.push(OsString::from("--filter"));
        command_args.push(filter);
    }

    for path in compare_destinations {
        command_args.push(OsString::from("--compare-dest"));
        command_args.push(path);
    }

    for path in copy_destinations {
        command_args.push(OsString::from("--copy-dest"));
        command_args.push(path);
    }

    for path in link_destinations {
        command_args.push(OsString::from("--link-dest"));
        command_args.push(path);
    }

    for flag in info_flags {
        let mut arg = OsString::from("--info=");
        arg.push(&flag);
        command_args.push(arg);
    }

    for flag in debug_flags {
        let mut arg = OsString::from("--debug=");
        arg.push(&flag);
        command_args.push(arg);
    }

    let files_from_temp =
        prepare_file_list(&file_list_entries, files_from_used, from0).map_err(|error| {
            fallback_error(format!(
                "failed to prepare file list for fallback rsync invocation: {error}"
            ))
        })?;

    if let Some(temp) = files_from_temp.as_ref() {
        command_args.push(OsString::from("--files-from"));
        command_args.push(temp.path().as_os_str().to_os_string());
        if from0 {
            command_args.push(OsString::from("--from0"));
        }
    }

    if let Some(path) = password_file {
        command_args.push(OsString::from("--password-file"));
        command_args.push(path.into_os_string());
    }

    if let Some(protocol) = protocol {
        command_args.push(OsString::from("--protocol"));
        command_args.push(OsString::from(protocol.to_string()));
    }

    match timeout {
        TransferTimeout::Default => {}
        TransferTimeout::Disabled => {
            command_args.push(OsString::from("--timeout"));
            command_args.push(OsString::from("0"));
        }
        TransferTimeout::Seconds(value) => {
            command_args.push(OsString::from("--timeout"));
            command_args.push(OsString::from(value.get().to_string()));
        }
    }

    match connect_timeout {
        TransferTimeout::Default => {}
        TransferTimeout::Disabled => {
            command_args.push(OsString::from("--contimeout"));
            command_args.push(OsString::from("0"));
        }
        TransferTimeout::Seconds(value) => {
            command_args.push(OsString::from("--contimeout"));
            command_args.push(OsString::from(value.get().to_string()));
        }
    }

    if no_motd {
        command_args.push(OsString::from("--no-motd"));
    }

    for option in remote_options {
        command_args.push(OsString::from("--remote-option"));
        command_args.push(option);
    }

    if let Some(program) = connect_program {
        command_args.push(OsString::from("--connect-program"));
        command_args.push(program);
    }

    if let Some(shell) = remote_shell {
        command_args.push(OsString::from("-e"));
        command_args.push(shell);
    }

    match address_mode {
        AddressMode::Default => {}
        AddressMode::Ipv4 => command_args.push(OsString::from("--ipv4")),
        AddressMode::Ipv6 => command_args.push(OsString::from("--ipv6")),
    }

    if let Some(port) = port {
        let mut arg = OsString::from("--port=");
        arg.push(port.to_string());
        command_args.push(arg);
    }

    if let Some(address) = bind_address {
        let mut arg = OsString::from("--address=");
        arg.push(address);
        command_args.push(arg);
    }

    if let Some(opts) = sockopts {
        let mut arg = OsString::from("--sockopts=");
        arg.push(opts);
        command_args.push(arg);
    }

    match blocking_io {
        Some(true) => command_args.push(OsString::from("--blocking-io")),
        Some(false) => command_args.push(OsString::from("--no-blocking-io")),
        None => {}
    }

    if let Some(path) = rsync_path {
        let mut arg = OsString::from("--rsync-path=");
        let path_str = path.to_string_lossy();
        if path_str.contains(|c: char| c.is_whitespace()) {
            let escaped = path_str.replace("'", "'\\''");
            let quoted = format!("'{escaped}'");
            arg.push(quoted);
        } else {
            arg.push(path);
        }
        command_args.push(arg);
    }

    command_args.append(&mut remainder);

    let binary = if let Some(path) = fallback_binary {
        path
    } else {
        match fallback_override(CLIENT_FALLBACK_ENV) {
            Some(FallbackOverride::Disabled) => {
                return Err(fallback_error(format!(
                    "remote transfers are unavailable because {CLIENT_FALLBACK_ENV} is disabled; set {CLIENT_FALLBACK_ENV} to point to an upstream rsync binary"
                )));
            }
            Some(other) => other
                .resolve_or_default(OsStr::new("rsync"))
                .unwrap_or_else(|| OsString::from("rsync")),
            None => OsString::from("rsync"),
        }
    };

    let Some(resolved_binary) = fallback_binary_path(binary.as_os_str()) else {
        let diagnostic =
            describe_missing_fallback_binary(binary.as_os_str(), &[CLIENT_FALLBACK_ENV]);
        let display = Path::new(binary.as_os_str()).display();
        return Err(fallback_error(format!(
            "failed to launch fallback rsync binary '{display}': {diagnostic}"
        )));
    };

    if fallback_binary_is_self(&resolved_binary) {
        let display = resolved_binary.display();
        return Err(fallback_error(format!(
            "failed to launch fallback rsync binary '{display}': fallback resolution points to this oc-rsync executable; install upstream rsync or set {CLIENT_FALLBACK_ENV} to a different path"
        )));
    }

    Ok(PreparedInvocation {
        binary: resolved_binary.into_os_string(),
        args: command_args,
        daemon_password,
        files_from_temp,
    })
}
