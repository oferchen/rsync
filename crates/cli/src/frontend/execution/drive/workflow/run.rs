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
use crate::frontend::log_format_has;
use crate::frontend::outbuf::parse_outbuf_mode;
use crate::frontend::progress::{ProgressOutputConfig, StderrMode};
use crate::frontend::{
    arguments::{ChecksumThreadsSetting, ParsedArgs, StopRequest},
    execution::{
        chown::ParsedChown, extract_operands, load_file_list_operands, operand_is_remote,
        parse_chown_argument, resolve_file_list_entries, resolve_files_from_source,
        resolve_iconv_setting,
    },
};
use core::client::{BatchConfig, BatchMode, HumanReadableMode};
use core::{message::Role, rsync_error};
use logging::VerbosityConfig;
use logging_sink::MessageSink;
use std::fs::{File, OpenOptions};
use std::io::{self, IsTerminal, Write};
use std::num::NonZeroUsize;
use std::path::PathBuf;

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;

use crate::frontend::execution::{parse_stop_after_argument, parse_stop_at_argument};

/// Main entry point for CLI-driven transfers: parses all arguments, builds config, and runs.
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
        show_io_uring_status,
        show_lsm_status,
        human_readable,
        dry_run,
        list_only,
        remote_shell: _,
        connect_program,
        daemon_port,
        remote_options,
        rsync_path: _,
        protect_args,
        old_args,
        address_mode,
        bind_address: bind_address_raw,
        sockopts,
        tcp_fastopen,
        blocking_io,
        archive,
        recursive,
        recursive_override: _,
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
        compress_threads,
        old_compress: _,
        new_compress: _,
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
        executability,
        super_mode,
        fake_super,
        times,
        omit_dir_times,
        omit_link_times,
        atimes,
        crtimes,
        acls,
        excludes: _,
        includes: _,
        compare_destinations,
        copy_destinations,
        link_destinations,
        exclude_from: _,
        include_from: _,
        filters: _,
        filter_order,
        cvs_exclude,
        apple_double_skip: _,
        rsync_filter_shortcuts: _,
        files_from,
        from0,
        info,
        debug,
        numeric_ids,
        hard_links,
        links,
        sparse,
        sparse_detect,
        fuzzy,
        copy_links,
        copy_dirlinks,
        copy_unsafe_links,
        keep_dirlinks,
        safe_links,
        munge_links,
        trust_sender,
        server_mode: _,
        sender_mode: _,
        detach: _,
        daemon_mode: _,
        config: _,
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
        progress: initial_progress,
        name_level: initial_name_level,
        name_overridden: initial_name_overridden,
        stats,
        eight_bit_output,
        partial,
        preallocate,
        fsync: fsync_option,
        io_uring_policy,
        io_uring_depth,
        zero_copy_policy,
        parallel_delta_scan,
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
        remove_sent_files,
        inplace,
        append,
        append_verify,
        msgs_to_stderr: msgs_to_stderr_option,
        stderr_mode,
        outbuf,
        max_alloc,
        itemize_changes,
        itemize_repeated,
        whole_file,
        xxh64_dedup,
        xattrs,
        no_motd,
        password_file,
        password_command,
        protocol,
        timeout,
        contimeout,
        stop_after,
        stop_at,
        out_format,
        dparam,
        no_iconv,
        prefer_aes_gcm,
        ssh_cipher: _ssh_cipher,
        ssh_connect_timeout: _ssh_connect_timeout,
        ssh_keepalive: _ssh_keepalive,
        ssh_identity: _ssh_identity,
        ssh_no_agent: _ssh_no_agent,
        ssh_strict_host_key_checking: _ssh_strict_host_key_checking,
        ssh_ipv6: _ssh_ipv6,
        ssh_port: _ssh_port,
        jump_host,
        rayon_threads,
        tokio_threads,
        checksum_threads,
        spill_dir,
        spill_threshold_bytes,
        no_spill,
    } = parsed;

    if let Some(level) = simd_override
        && let Err(previous) = checksums::set_simd_override(level)
    {
        let message = rsync_error!(
            1,
            format!(
                "--simd: cannot change SIMD level after initialization \
                 (was {}, requested {})",
                previous.as_cli_str(),
                level.as_cli_str(),
            )
        )
        .with_role(Role::Client);
        return fail_with_message(message, stderr);
    }

    let password_file = password_file.map(PathBuf::from);
    let human_readable_setting = human_readable;
    let human_readable_mode = human_readable_setting.unwrap_or(HumanReadableMode::Grouped);
    let human_readable_enabled = human_readable_mode.is_enabled();
    let stderr_mode_setting = stderr_mode
        .as_ref()
        .and_then(|s| s.to_str())
        .and_then(StderrMode::from_str)
        .unwrap_or_default();

    let msgs_to_stderr_enabled = match stderr_mode_setting {
        StderrMode::All => true,
        StderrMode::Errors | StderrMode::Client => msgs_to_stderr_option.unwrap_or(false),
    };
    // Record the resolved routing so the post-execute final flush in
    // `frontend::mod` directs any leftover diagnostic events (e.g. the
    // backup notice emitted by the local-copy executor) to the same stream
    // the workflow itself uses. Without this, leftover Info events were
    // hardcoded to stderr and invisible to upstream tests that grep stdout.
    crate::frontend::progress::diagnostic::set_msgs_to_stderr(msgs_to_stderr_enabled);

    let verbosity_config = VerbosityConfig::from_verbose_level(verbosity);
    logging::init(verbosity_config);

    let rayon_thread_count = rayon_threads.and_then(|n| NonZeroUsize::new(n as usize));
    let tokio_thread_count = tokio_threads.and_then(|n| NonZeroUsize::new(n as usize));

    // Resolve `--checksum-threads` into the receiver's basis-signature policy
    // and, for the capped form, an implied rayon pool size. This is an
    // oc-only local performance knob: it is never forwarded to the remote
    // server and never changes wire bytes (the signature is byte-identical
    // regardless of thread count).
    let checksum_rayon_cap = match checksum_threads {
        Some(ChecksumThreadsSetting::Auto) => {
            core::server::receiver::set_checksum_threads_policy(
                core::server::receiver::ChecksumThreadsPolicy::Auto,
            );
            None
        }
        Some(ChecksumThreadsSetting::Sequential) => {
            core::server::receiver::set_checksum_threads_policy(
                core::server::receiver::ChecksumThreadsPolicy::Sequential,
            );
            None
        }
        Some(ChecksumThreadsSetting::Capped(n)) => {
            core::server::receiver::set_checksum_threads_policy(
                core::server::receiver::ChecksumThreadsPolicy::Auto,
            );
            NonZeroUsize::new(n as usize)
        }
        None => None,
    };

    // `--rayon-threads` takes precedence over the `--checksum-threads=N` cap
    // when both are supplied; either way the global pool is installed once.
    if let Some(threads) = rayon_thread_count.or(checksum_rayon_cap) {
        super::super::thread_tunables::install_rayon_thread_count(threads, stderr);
    }

    if let Err(code) = validate_stdin_sources_conflict(&password_file, &files_from, stderr) {
        return code;
    }

    // Resolve daemon password from the available sources in precedence order:
    // --password-command > --password-file > RSYNC_PASSWORD (handled later in core)
    let password_override = match crate::frontend::password::resolve_password(
        password_command.as_deref(),
        password_file.as_deref(),
    ) {
        Ok(password) => password,
        Err(message) => return fail_with_message(message, stderr),
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

    if let Some(code) = maybe_print_help_or_version(
        show_help,
        show_version,
        show_io_uring_status,
        show_lsm_status,
        program_name,
        stdout,
    ) {
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

    // upstream: options.c:2465-2471 - with `--files-from` the transferred file
    // set comes from the list, so a client may name exactly one source root and
    // one destination. More than two operands, or a lone operand (a missing
    // destination), is a syntax error (`usage(FERROR); exit RERR_SYNTAX`, exit
    // 1).
    if !files_from.is_empty() && (remainder.len() > 2 || remainder.len() == 1) {
        let message = rsync_error!(
            1,
            "--files-from requires a single source and a single destination argument"
        )
        .with_role(Role::Client);
        return fail_with_message(message, stderr);
    }

    // `--protocol` is resolved once operands are known: upstream accepts it on a
    // local copy (setup_protocol runs there too) but this build only speaks the
    // wire for a remote transfer, so the value is ignored locally and validated
    // against the wire range only when an operand is remote.
    let has_remote_operand = remainder.iter().any(|op| operand_is_remote(op));
    let desired_protocol =
        match resolve_desired_protocol(protocol.as_ref(), has_remote_operand, stderr) {
            Ok(protocol) => protocol,
            Err(code) => return code,
        };

    // upstream: options.c:2055 `if (do_stats) parse_output_words("stats2", ...)`
    // (or "stats3" with `-vv`). The legacy `--stats` flag maps to level 2; with
    // higher verbosity it bumps to level 3. A subsequent `--info=statsN` token
    // overrides this default inside `parse_info_settings`.
    let initial_stats_level: u8 = if stats {
        if verbosity > 1 { 3 } else { 2 }
    } else {
        0
    };

    let settings_inputs = options::SettingsInputs {
        info: &info,
        debug: &debug,
        itemize_changes,
        out_format: out_format.as_ref(),
        initial_progress,
        initial_stats_level,
        initial_name_level,
        initial_name_overridden,
        bwlimit: &bwlimit,
        max_delete: &max_delete,
        min_size: &min_size,
        max_size: &max_size,
        block_size: &block_size,
        max_alloc: &max_alloc,
        modify_window: &modify_window,
        compress_flag,
        no_compress,
        compress_level: &compress_level,
        compress_choice: &compress_choice,
        compress_threads: &compress_threads,
        skip_compress: &skip_compress,
        log_file: log_file.as_ref(),
        log_file_format: log_file_format.as_ref(),
    };

    let options::DerivedSettings {
        out_format_template,
        progress_mode,
        stats_level,
        name_level,
        name_overridden,
        show_copy_method,
        debug_flags_list,
        info_flags_list,
        bandwidth_limit,
        max_delete_limit,
        min_size_limit,
        max_size_limit,
        block_size_override,
        max_alloc_limit,
        modify_window_setting,
        compress,
        compression_level_override,
        skip_compress_list,
        skip_compress_spec,
        compression_setting,
        compression_algorithm,
        compress_choice_name,
        compression_threads,
        log_file_path,
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

    #[allow(unused_variables)] // REASON: used on unix or windows with feature "acl"
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

    // upstream: options.c:2483 - only open files_from locally when the spec
    // is NOT a hostspec. Remote files-from (`:path` or `host:path`) are read
    // by the server, so the client never opens them.
    let files_from_resolved = resolve_files_from_source(&files_from);
    let mut file_list_operands = if files_from_resolved.is_remote() {
        Vec::new()
    } else {
        match load_file_list_operands(&files_from, from0) {
            Ok(operands) => operands,
            Err(message) => return fail_with_message(message, stderr),
        }
    };

    let files_from_active = !files_from.is_empty();

    resolve_file_list_entries(
        &mut file_list_operands,
        &remainder,
        relative.unwrap_or(false),
        files_from_active,
    );

    if let Some(exit_code) = maybe_handle_module_listing(
        stdout,
        stderr,
        ModuleListingInputs {
            file_list_operands: &file_list_operands,
            remainder: &remainder,
            daemon_port,
            desired_protocol,
            password_override: password_override.clone(),
            no_motd,
            address_mode,
            bind_address: bind_address.as_ref(),
            connect_program: connect_program.as_ref(),
            remote_shell: parsed.remote_shell.as_ref(),
            rsync_path: parsed.rsync_path.as_ref(),
            timeout_setting,
            connect_timeout_setting,
            sockopts: sockopts.as_ref(),
            tcp_fastopen,
            blocking_io,
        },
    ) {
        return exit_code;
    }

    let implied_dirs_option = implied_dirs;

    // upstream: options.c:2169-2177 - --files-from disables default recursion,
    // enables xfer_dirs, and implies --relative.
    //
    // Otherwise honour the parser-computed `recursive` flag, which mirrors
    // upstream's `recurse` default of 0 (options.c:112) and the `-r` / `-a` /
    // `--no-recursive` precedence rules (parser/mod.rs:152-158).
    let recursive_effective = if files_from_active {
        false // upstream: options.c:2174 - if (recurse == 1) recurse = 0
    } else {
        recursive
    };

    // upstream: options.c:2176-2177 - xfer_dirs = 1 when files_from is active
    let dirs = if files_from_active { Some(true) } else { dirs };

    // upstream: compat.c:710-748 - local transfers negotiate compat_flags
    // between sender and receiver. For protocol 32 with full capability
    // string (`.LsfxCIvu`), the flags include SAFE_FILE_LIST,
    // AVOID_XATTR_OPTIMIZATION, CHECKSUM_SEED_FIX, INPLACE_PARTIAL_DIR,
    // ID0_NAMES, and VARINT_FLIST_FLAGS. These flags must be written to
    // the batch header so upstream rsync can decode the file list and
    // delta stream.
    //
    // upstream: batch.c - batch files record the compat_flags used during the
    // transfer. INC_RECURSE must be set: without it, upstream's reader calls
    // recv_id_list() after the flist end marker, consuming delta bytes as ID
    // list data and causing "File-list index N not in range" errors. With
    // INC_RECURSE, recv_id_list() is skipped (names are inline in flist).
    // Our flat flist (all entries in the initial segment) is compatible with
    // INC_RECURSE - the reader simply finds no sub-list segments.
    // upstream: compat.c:712-738 - compat_flags written to batch header.
    // CF_INC_RECURSE is deliberately omitted because upstream's --read-batch
    // calls set_allow_inc_recurse() which may disable inc_recurse, causing
    // "Incompatible options specified for inc-recursive batch file" (compat.c:773).
    // Upstream's own -a --write-batch produces unreadable batches for this reason.
    // Match upstream --no-inc-recursive --write-batch behavior.
    let local_batch_compat_flags = {
        use protocol::CompatibilityFlags;
        // ID0_NAMES is omitted because without INC_RECURSE, uid/gid name
        // mappings go through post-flist ID lists (uidlist.c:send_id_lists).
        // Empty ID lists with simple varint30(0) terminators are written;
        // keeping ID0_NAMES off avoids the need to look up id=0 names.
        let flags = CompatibilityFlags::SAFE_FILE_LIST
            | CompatibilityFlags::AVOID_XATTR_OPTIMIZATION
            | CompatibilityFlags::CHECKSUM_SEED_FIX
            | CompatibilityFlags::INPLACE_PARTIAL_DIR
            | CompatibilityFlags::VARINT_FLIST_FLAGS;
        #[cfg(unix)]
        let flags = flags | CompatibilityFlags::SYMLINK_TIMES;
        // upstream: compat.c:716-718 - CF_SYMLINK_ICONV is gated on
        // `#ifdef ICONV_OPTION`. Mirror that with the `iconv` cargo feature
        // so batch headers from iconv-less builds do not advertise a
        // capability the writer cannot honour.
        #[cfg(all(unix, feature = "iconv"))]
        let flags = flags | CompatibilityFlags::SYMLINK_ICONV;
        flags.bits() as i32
    };

    // upstream: compat.c:811-814 setup_protocol() -
    //   if (!checksum_seed) checksum_seed = time(NULL) ^ (getpid() << 6);
    //   write_int(f_out, checksum_seed);
    // The finalised seed is what io.c:2524 start_write_batch() records in the
    // batch header, so an explicit --checksum-seed=N (options.c:847) must flow
    // through unchanged and only an unset seed is derived from time/pid.
    let batch_checksum_seed = explicit_batch_seed(checksum_seed).unwrap_or_else(derive_batch_seed);

    // upstream: batch.c:259 - write_arg(raw_argv[0]) writes the exact binary
    // path the user invoked into the generated BATCH.sh. Mirror that by
    // capturing argv[0] now so the replay script does not require oc-rsync
    // on PATH (e.g. test harnesses and CI invoke by absolute path).
    let invoker = std::env::args_os()
        .next()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| String::from("oc-rsync"));

    let batch_config = if let Some(ref path) = write_batch {
        Some(
            BatchConfig::new(BatchMode::Write, path.to_string_lossy().into_owned(), 32)
                .with_compat_flags(local_batch_compat_flags)
                .with_checksum_seed(batch_checksum_seed)
                .with_invoker(invoker.clone()),
        )
    } else if let Some(ref path) = only_write_batch {
        Some(
            BatchConfig::new(
                BatchMode::OnlyWrite,
                path.to_string_lossy().into_owned(),
                32,
            )
            .with_compat_flags(local_batch_compat_flags)
            .with_checksum_seed(batch_checksum_seed)
            .with_invoker(invoker.clone()),
        )
    } else {
        read_batch
            .as_ref()
            .map(|path| BatchConfig::new(BatchMode::Read, path.to_string_lossy().into_owned(), 32))
    };

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

    // Build transfer operands early so we can check if this is a daemon transfer.
    // upstream: main.c:780-790 - source dir is chdir target, not a transfer source
    // `has_remote_operand` was computed above (protocol resolution needs it).
    let mut transfer_operands = Vec::with_capacity(file_list_operands.len() + remainder.len());
    if files_from_active && !file_list_operands.is_empty() {
        if has_remote_operand {
            // Daemon transfer with --files-from: pass the source directory and
            // destination as operands. The generator reads the file list from
            // files_from_path and uses the source dir as base_dir for resolving
            // relative filenames. Individual file entries must NOT be operands -
            // they corrupt the generator's base_dir derivation (paths.first()).
            // upstream: main.c:1292-1339 - client_run() uses argv[0] as chdir
            // target, filesfrom_fd is a separate channel.
            transfer_operands.extend(remainder);
        } else {
            // Local copy with --files-from: file entries are the source operands.
            // The source dir served only as the base for resolving entries.
            transfer_operands.append(&mut file_list_operands);
            if let Some(dest) = remainder.last() {
                transfer_operands.push(dest.clone());
            }
        }
    } else {
        // No --files-from, or --files-from with empty list: use the
        // original positional args (source + dest). An empty file list
        // with --files-from still needs source+dest operands so the
        // transfer engine can validate them and succeed with zero work.
        transfer_operands.append(&mut file_list_operands);
        transfer_operands.extend(remainder);
    }

    let is_daemon_transfer = transfer_operands.iter().any(|op| operand_is_remote(op));
    if !is_daemon_transfer {
        if let Some(exit_code) = validation::validate_local_only_options(
            password_override.is_some(),
            password_file.is_some() || password_command.is_some(),
            connect_program.as_ref(),
            parsed.rsync_path.as_ref(),
            &remote_options,
            stderr,
        ) {
            return exit_code;
        }
    }

    if let Err(code) =
        ensure_transfer_operands_present(&transfer_operands, program_name, stdout, stderr)
    {
        return code;
    }

    // upstream: batch.c:269-298 - the replay script reconstructs the original
    // command's pass-through options (transfer-affecting flags like -a/-z/
    // --numeric-ids) from raw_argv, eliding the filename operands. Capture the
    // raw argv and operands so the batch script generator can re-emit them.
    let batch_config = batch_config.map(|cfg| {
        // upstream: flist.c:2548 - the writer omits the post-flist id-lists under
        // --numeric-ids (numeric_ids is not a recorded stream flag), so carry it
        // into the batch config for both write and read modes.
        let cfg = cfg.with_numeric_ids(numeric_ids);
        if cfg.is_write_mode() {
            let replay_args: Vec<String> = std::env::args_os()
                .map(|a| a.to_string_lossy().into_owned())
                .collect();
            let operands: Vec<String> = transfer_operands
                .iter()
                .map(|op| op.to_string_lossy().into_owned())
                .collect();
            cfg.with_replay_args(replay_args).with_operands(operands)
        } else {
            cfg
        }
    });

    // upstream: options.c:795 - `--list-only` sets `list_only = 2`, the explicit
    // form that server_options() forwards as `--list-only` (options.c:2747
    // `list_only > 1`). The implicit `list_only |= 1` below never reaches 2, so
    // it is never forwarded. Capture the explicit bit before the OR.
    let list_only_arg = list_only;
    // upstream: options.c:2194-2195 - `if (argc < 2 && !read_batch && !am_server)
    // list_only |= 1;`. A single remote source with no destination (e.g.
    // `host::module` or `rsync://host/module`) implies list-only mode: list the
    // module's contents instead of erroring "need source and destination".
    let list_only =
        list_only || (transfer_operands.len() == 1 && read_batch.is_none() && is_daemon_transfer);

    // upstream: options.c:2187-2188 - relative_paths defaults to 1 when files_from
    let effective_relative = if files_from_active && relative.is_none() {
        Some(true)
    } else {
        relative
    };

    // upstream: options.c:2207-2208 - `if (!relative_paths) implied_dirs = 0;`.
    // Implied directories only exist for relative-rooted transfer paths, so
    // when relative paths are disabled implied_dirs is forced off regardless
    // of any explicit `--implied-dirs`. Otherwise it defaults on.
    let implied_dirs = if effective_relative.unwrap_or(false) {
        implied_dirs_option.unwrap_or(true)
    } else {
        false
    };

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
        atimes,
        crtimes,
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
        relative: effective_relative,
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
        preserve_atimes,
        preserve_crtimes,
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
    // upstream: options.c:2413-2419 - `--write-devices` forces the global
    // inplace flag on, so device targets are written in place rather than via a
    // temp file.
    let inplace_enabled = inplace.unwrap_or(false) || write_devices.unwrap_or(false);
    let append_enabled = append.unwrap_or(false);
    let whole_file_enabled = whole_file_option;

    let checksum_for_config = checksum.unwrap_or(false);
    let fuzzy_level_value = fuzzy.unwrap_or(0);

    // upstream: options.c:2345-2358,2375-2376,2768-2780 - the resolved
    // out-format string tells the server which placeholders it uses. Upstream
    // derives `stdout_format_has_i` from that resolved string, not from the `-i`
    // flag: an explicit `--out-format` without `%i` clears it even under `-i`
    // (so the server arg becomes `%o` or `X`), while `-i` alone installs the
    // default `"%i %n%L"` format whose `%i` is forwarded via `--log-format=%i`.
    // A `%o` directive (without `%i`) forwards `--log-format=%o`; a format with
    // neither `%i` nor `%o` forwards the placeholder `--log-format=X` for a
    // non-verbose client.
    let out_format_forwards_i = match out_format.as_ref() {
        Some(fmt) => log_format_has(fmt, 'i'),
        None => itemize_changes,
    };
    let out_format_has_operation = out_format
        .as_ref()
        .is_some_and(|fmt| !log_format_has(fmt, 'i') && log_format_has(fmt, 'o'));
    let out_format_placeholder = out_format
        .as_ref()
        .is_some_and(|fmt| !log_format_has(fmt, 'i') && !log_format_has(fmt, 'o'));
    // A custom `--out-format` was supplied: route remote per-file output through
    // the client's out-format renderer (default `-v`/`-i` keep the server line).
    let render_out_format_locally = out_format.is_some();

    let config_inputs = config::ConfigInputs {
        transfer_operands,
        desired_protocol,
        address_mode,
        connect_program: connect_program.clone(),
        bind_address,
        sockopts: sockopts.clone(),
        tcp_fastopen,
        blocking_io,
        dry_run,
        list_only,
        list_only_arg,
        quiet,
        msgs2stderr: msgs_to_stderr_option,
        recursive: recursive_effective,
        dirs,
        delete_mode,
        delete_excluded,
        delete_missing_args,
        ignore_errors: ignore_errors.unwrap_or(false),
        max_delete_limit,
        min_size_limit,
        max_size_limit,
        block_size_override,
        rayon_threads: rayon_thread_count,
        tokio_threads: tokio_thread_count,
        max_alloc: max_alloc_limit,
        backup,
        backup_dir: backup_dir.map(PathBuf::from),
        backup_suffix,
        bandwidth_limit,
        compression_setting,
        compress,
        compression_level_override,
        compression_algorithm,
        compress_choice_name,
        compression_threads,
        open_noatime: open_noatime_enabled,
        owner: preserve_owner,
        owner_override: owner_override_value,
        group: preserve_group,
        group_override: group_override_value,
        copy_as,
        chmod_modifiers,
        user_mapping,
        group_mapping,
        executability: preserve_executability,
        permissions: preserve_permissions,
        fake_super: fake_super.unwrap_or(false),
        // upstream: options.c:90 - `am_root > 1` is set only by an explicit
        // --super (not by running as root). Forwarded on a push (options.c:2852).
        super_user: super_mode == Some(true),
        times: preserve_times,
        // The u8 level (0/1/2) drives the doubled `-UU` compact letter; the
        // boolean `preserve_atimes` derived from it still governs local metadata
        // application.
        atimes: atimes.unwrap_or(0),
        crtimes: preserve_crtimes,
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
        sparse_detect: sparse_detect.unwrap_or(engine::SparseDetectStrategy::Auto),
        copy_links,
        copy_dirlinks,
        copy_unsafe_links,
        keep_dirlinks: keep_dirlinks_flag,
        safe_links,
        munge_links: munge_links.unwrap_or(false),
        trust_sender,
        fuzzy_level: fuzzy_level_value,
        links: preserve_symlinks,
        relative_paths: relative,
        one_file_system,
        implied_dirs,
        human_readable: human_readable_enabled,
        mkpath,
        prune_empty_dirs: prune_empty_dirs_flag,
        qsort,
        inc_recursive_send: inc_recursive,
        verbosity,
        progress_mode,
        stats: stats_level > 0,
        debug_flags_list,
        info_flags_list,
        partial,
        preallocate,
        fsync: fsync_flag,
        io_uring_policy,
        io_uring_depth,
        zero_copy_policy,
        parallel_delta_scan,
        cow_policy,
        partial_dir,
        temp_dir,
        delay_updates,
        link_dests,
        remove_source_files,
        remove_sent_files,
        out_format_forwards_i,
        render_out_format_locally,
        out_format_has_operation,
        out_format_placeholder,
        inplace: inplace_enabled,
        append: append_enabled,
        append_verify,
        whole_file: whole_file_enabled,
        xxh64_dedup,
        timeout: timeout_setting,
        connect_timeout: connect_timeout_setting,
        stop_deadline: stop_request.as_ref().map(StopRequest::deadline),
        checksum_choice,
        compare_destinations,
        copy_destinations,
        link_destinations,
        #[cfg(all(any(unix, windows), feature = "acl"))]
        preserve_acls,
        #[cfg(all(any(unix, windows), feature = "xattr"))]
        xattrs: xattrs.unwrap_or(0),
        skip_compress_list,
        skip_compress_spec,
        cvs_exclude,
        itemize_changes,
        out_format_template: out_format_template.clone(),
        log_file_template,
        name_level,
        iconv: iconv_setting,
        remote_shell: parsed.remote_shell.clone(),
        rsync_path: parsed.rsync_path.clone(),
        early_input: early_input.map(PathBuf::from),
        prefer_aes_gcm,
        protect_args,
        old_args: resolve_old_args(old_args, protect_args),
        jump_hosts: jump_host,
        batch_config,
        no_motd,
        password_override,
        remote_options,
        daemon_params: dparam
            .into_iter()
            .map(|s| s.to_string_lossy().into_owned())
            .collect(),
        files_from: files_from_resolved.clone(),
        from0,
        spill_dir,
        spill_threshold_bytes,
        no_spill,
    };

    let builder = config::build_base_config(config_inputs);

    let filter_inputs = filters::FilterInputs {
        order: filter_order,
        from0,
    };

    let builder = match filters::apply_filters(builder, filter_inputs, stderr) {
        Ok(builder) => builder,
        Err(code) => return code,
    };

    if let Err(conflict) = builder.validate() {
        let message = rsync_error!(1, "{}", conflict).with_role(Role::Client);
        return fail_with_message(message, stderr);
    }

    let config = builder.build();

    // upstream: progress.c:234-238 checks `tcgetpgrp(STDOUT_FILENO)` to
    // suppress progress when the process is not in the foreground terminal
    // group. We detect terminal status on the output destination (stdout
    // by default, stderr when msgs_to_stderr is active) to decide between
    // `\r` (in-place overwrite on terminals) and `\n` (readable when piped).
    let progress_is_terminal = if msgs_to_stderr_enabled {
        std::io::stderr().is_terminal()
    } else {
        std::io::stdout().is_terminal()
    };
    let outbuf_mode = outbuf
        .as_ref()
        .and_then(|v| parse_outbuf_mode(v.as_os_str()).ok());
    let progress_output_config = ProgressOutputConfig {
        is_terminal: progress_is_terminal,
        outbuf_mode,
    };

    summary::execute_transfer(
        stdout,
        stderr,
        summary::TransferExecutionInputs {
            config,
            msgs_to_stderr: msgs_to_stderr_enabled,
            stderr_mode: stderr_mode_setting,
            progress_mode,
            progress_output_config,
            human_readable_mode,
            itemize_changes,
            itemize_repeated,
            stats_level,
            verbosity,
            list_only,
            dry_run,
            // `--only-write-batch` (upstream `write_batch < 0`) drives the
            // `" (BATCH ONLY)"` speedup suffix in the summary trailer.
            only_write_batch: only_write_batch.is_some(),
            // `--info=copy` opts into the oc-rsync `Copy method` stats line.
            show_copy_method,
            // `-U`/`--atimes` and `--crtimes` add the ATIME/CRTIME columns to
            // `--list-only` output (upstream: generator.c list_file_entry()).
            show_atimes: preserve_atimes,
            show_crtimes: preserve_crtimes,
            out_format_template: out_format_template.as_ref(),
            name_level,
            name_overridden,
            eight_bit_output,
            log_file: log_file_for_local,
        },
    )
}

/// Resolves the effective `--old-args` setting from the CLI flag and env var.
///
/// upstream: options.c:1952-1964 - when `old_style_args` is not explicitly set
/// (`None`), check `RSYNC_OLD_ARGS` env var. The env var is only honoured when
/// protect_args is not active (upstream: `protect_args <= 0`). When both
/// `--old-args` and `--protect-args` are explicitly set, upstream rejects the
/// combination, but we silently give protect_args precedence (old_args becomes
/// inactive) since the conflict is validated at the CLI layer.
fn resolve_old_args(explicit: Option<bool>, protect_args: Option<bool>) -> Option<bool> {
    if let Some(value) = explicit {
        return Some(value);
    }
    // upstream: options.c:1953 - only check env when !am_server && protect_args <= 0
    if protect_args.unwrap_or(false) {
        return None;
    }
    match std::env::var("RSYNC_OLD_ARGS") {
        Ok(val) if !val.is_empty() => {
            // upstream: old_style_args = atoi(arg) - any non-zero value enables
            let level: i32 = val.parse().unwrap_or(0);
            if level > 0 { Some(true) } else { None }
        }
        _ => None,
    }
}

/// Returns the explicit checksum seed to record in a batch header, or `None`
/// when the seed must be derived from time/pid.
///
/// upstream: compat.c:811-812 `if (!checksum_seed) checksum_seed = ...` - the
/// parsed `--checksum-seed=N` (options.c:847, an `int` defaulting to 0) is only
/// treated as user-supplied when it is non-zero. An explicit `--checksum-seed=0`
/// is indistinguishable from an unset seed and therefore derives a fresh one,
/// exactly like omitting the flag. The bit pattern of `N` is preserved across
/// the `u32 -> i32` cast so that seeds above `i32::MAX` round-trip through the
/// batch header verbatim.
fn explicit_batch_seed(parsed: Option<u32>) -> Option<i32> {
    match parsed {
        Some(n) if n != 0 => Some(n as i32),
        _ => None,
    }
}

/// Derives a checksum seed from the current time and pid.
///
/// upstream: compat.c:812 `checksum_seed = time(NULL) ^ (getpid() << 6)`.
fn derive_batch_seed() -> i32 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i32;
    let pid = std::process::id() as i32;
    timestamp ^ (pid << 6)
}

/// Opens a log file for appending, creating it if it does not exist.
fn open_log_file(path: &PathBuf) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        options.mode(0o666);
    }
    options.open(path)
}

#[cfg(test)]
mod tests {
    use super::{derive_batch_seed, explicit_batch_seed};

    /// An explicit non-zero `--checksum-seed=N` must be recorded in the batch
    /// header verbatim so `--read-batch` replays with the identical seed.
    /// upstream: compat.c:813-814 writes the parsed seed unchanged; io.c:2524
    /// tees that same value into the header. Regressing this (e.g. always
    /// deriving a fresh seed) makes upstream `--read-batch` compute mismatched
    /// checksums against a batch oc-rsync wrote.
    #[test]
    fn explicit_nonzero_seed_flows_through() {
        assert_eq!(explicit_batch_seed(Some(12345)), Some(12345));
    }

    /// Seeds above `i32::MAX` must round-trip by bit pattern, matching upstream's
    /// `int checksum_seed` storage.
    #[test]
    fn explicit_large_seed_preserves_bit_pattern() {
        assert_eq!(
            explicit_batch_seed(Some(0xDEAD_BEEF)),
            Some(0xDEAD_BEEFu32 as i32)
        );
    }

    /// upstream: compat.c:811 `if (!checksum_seed)` treats an explicit
    /// `--checksum-seed=0` as unset, so it derives a fresh seed just like
    /// omitting the flag. Both must return `None` from the explicit-seed
    /// selector so the caller falls back to derivation.
    #[test]
    fn zero_and_unset_seed_derive() {
        assert_eq!(explicit_batch_seed(Some(0)), None);
        assert_eq!(explicit_batch_seed(None), None);
    }

    /// The derivation is `time ^ (pid << 6)`; the pid term is non-zero for any
    /// real process, so the derived seed is a defined value the header can
    /// carry. This guards the fallback path from panicking.
    #[test]
    fn derive_seed_is_defined() {
        let _ = derive_batch_seed();
    }
}
