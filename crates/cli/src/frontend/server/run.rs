//! Server mode execution - orchestrates argument parsing and server startup.

use std::ffi::OsString;
use std::fmt;
use std::io::{self, Write};

use core::branding::Brand;
use core::message::Role;
use core::rsync_error;
use logging_sink::MessageSink;

use super::flags::{detect_secluded_args_flag, parse_server_long_flags};
use super::parse::{
    parse_server_checksum_seed, parse_server_flag_string_and_args, parse_server_size_limit,
    parse_server_stop_after, parse_server_stop_at,
};

/// Runs the native server implementation when `--server` is requested.
pub(crate) fn run_server_mode<Out, Err>(
    args: &[OsString],
    stdout: &mut Out,
    stderr: &mut Err,
) -> i32
where
    Out: Write,
    Err: Write,
{
    // Route through the session-level driver facade so the `tokio-transfer`
    // feature can select the tokio-hosted driver. The signature and result are
    // identical to `core::server::run_server_stdio`; only the driver changes.
    // Default builds forward straight to the threaded path (ASY-3).
    use core::server::{ServerConfig, ServerRole};
    use core::session::run_server_stdio;

    let program_brand =
        super::super::detect_program_name(args.first().map(OsString::as_os_str)).brand();

    // Force inherited stdio to blocking mode before any read/write hits the
    // multiplex-frame writer. Pipes from a parent rsync process are nominally
    // blocking, but a parent that left `O_NONBLOCK` set would surface as
    // `Resource temporarily unavailable (os error 11)` from `write_all_retry`
    // and abort the transfer with no recovery path. Best-effort: log on
    // failure but continue, mirroring upstream's tolerance for fcntl edge
    // cases. upstream: io.c::writefd_unbuffered relies on blocking stdio.
    if let Err(e) = fast_io::force_blocking_stdio() {
        write_server_error(
            stderr,
            program_brand,
            format!("failed to set stdio blocking mode: {e}"),
        );
    }

    // Detect secluded-args mode: `-s` flag appears as a standalone argument
    // after --server. upstream: options.c - protect_args in server mode.
    let secluded_args = detect_secluded_args_flag(args);

    let mut stdin = io::stdin().lock();

    // When secluded-args is active, the client splits its argv: the
    // server-options head (--server, --sender, packed flag string, and
    // value-bearing long flags) travels on the command line, while the
    // trailing positional args (the `.` separator and path arguments)
    // stream over stdin as NUL-delimited bytes terminated by an empty
    // string. Keep the command-line argv tail and append the stdin
    // payload, skipping the synthetic "rsync" arg0 the wire prepends.
    //
    // upstream: main.c::read_args() merges cmdline args with stdin args
    // under --protect-args / secluded-args. rsync.c:283
    // send_protected_args() rewrites args[i] to "rsync" at the NULL
    // split inserted by options.c:2745; io.c:1308 read_args() then
    // re-runs parse_arguments() on the server side.
    let effective_args: Vec<OsString>;
    let effective_slice: &[OsString] = if secluded_args {
        match protocol::secluded_args::recv_secluded_args(&mut stdin, None) {
            Ok(received_args) => {
                // Discard the synthetic "rsync" arg0 from the wire and
                // prepend the command-line tail so the server-options
                // head (flag string + long flags) is in effective_args.
                let mut received_iter = received_args.into_iter();
                let _arg0 = received_iter.next();
                let cmdline_tail = args.iter().skip(1).cloned();
                effective_args = cmdline_tail
                    .chain(received_iter.map(OsString::from))
                    .collect();
                &effective_args
            }
            Err(e) => {
                write_server_error(
                    stderr,
                    program_brand,
                    format!("failed to read secluded args: {e}"),
                );
                return 1;
            }
        }
    } else {
        &args[1..]
    };

    let long_flags = parse_server_long_flags(effective_slice);

    let (flag_string, positional_args) = parse_server_flag_string_and_args(effective_slice);

    // upstream: main.c server_sender check - default to Receiver when neither
    // --sender nor --receiver is specified.
    let role = if long_flags.is_sender {
        ServerRole::Generator
    } else {
        ServerRole::Receiver
    };

    let mut config =
        match ServerConfig::from_flag_string_and_args(role, flag_string, positional_args) {
            Ok(cfg) => cfg,
            Err(e) => {
                write_server_error(
                    stderr,
                    program_brand,
                    format!("invalid server arguments: {e}"),
                );
                return 1;
            }
        };

    // upstream: main.c:1271 - "keep_dirlinks = 0; /* Must be disabled on the
    // sender. */". keep-dirlinks is a receiver-only feature (it follows a
    // destination dir-symlink); the sender reads the source, never the
    // destination, so force it off whenever this process is the sender
    // (Generator role). The client still forwards `-K` so the peer receiver
    // honours it; only the sender's own copy is cleared.
    if role == ServerRole::Generator {
        config.flags.keep_dirlinks = false;
    }

    // Apply value-bearing flags, returning parse errors to the client.
    // upstream: options.c - server_options() sends these as `--flag=value`.
    if let Err(code) = apply_value_flags(&mut config, &long_flags, stderr, program_brand) {
        return code;
    }

    // upstream: options.c set_output_verbosity - the packed `-v` count maps to
    // info/debug levels via info_verbosity[] (options.c:239-243) before any
    // explicit --info override is layered on. Without this, the server thread's
    // thread-local verbosity stays at the zero default, so `-vv` never raises
    // info.name to 2 and the itemize line for an unchanged entry
    // (INFO_GTE(NAME, 2), generator.c:582-583) is suppressed - exactly the
    // gap that fails the upstream `itemize` test under SSH `--server` mode,
    // where `-vv` arrives as packed `v` letters rather than `--info=name2`.
    if config.flags.verbose_level > 0 {
        logging::init(logging::VerbosityConfig::from_verbose_level(
            config.flags.verbose_level,
        ));
    }

    // upstream: options.c parse_output_words - server-side info parsing
    // silently ignores unknown tokens so a newer client can forward names
    // this build has not learned yet. The well-formed empty/level errors
    // still surface so malformed input is not swallowed entirely. Applied
    // after the verbose-derived base so an explicit `--info` overrides it,
    // matching upstream's verbose-then-info ordering.
    if !long_flags.info.is_empty() {
        match super::super::execution::parse_info_flags_server(&long_flags.info) {
            Ok(settings) => {
                // Apply resolved info levels to the thread-local config so
                // info_log! callsites on the server side respect the client's
                // --info settings.
                settings.apply_to_thread_local();
            }
            Err(message) => {
                write_server_error(stderr, program_brand, message.text().to_owned());
                return 1;
            }
        }
    }

    // Boolean and move-only flags applied after value parsing releases its borrow.
    config.deletion.ignore_errors = long_flags.ignore_errors;
    config.write.fsync = long_flags.fsync;
    config.write.io_uring_policy = long_flags.io_uring_policy;
    config.write.zero_copy_policy = long_flags.zero_copy_policy;
    config.write.write_devices = long_flags.write_devices;
    // upstream: options.c:2493 - server always trusts sender (am_server implies trust)
    config.trust_sender = true;
    config.qsort = long_flags.qsort;
    config.file_selection.files_from_path = long_flags.files_from;
    config.file_selection.from0 = long_flags.from0;
    config.write.inplace = long_flags.inplace;
    // upstream: options.c:2400-2412 - append mode implies inplace; the transfer
    // layer derives that internally (transfer_ops.rs `use_inplace = inplace ||
    // append`), so only the append flags need forwarding. append_verify
    // (append_mode == 2) folds the on-disk prefix into the whole-file checksum
    // (receiver.c:357, match.c:373). Mirrors the daemon long-form parser.
    config.flags.append = long_flags.append;
    config.flags.append_verify = long_flags.append_verify;
    config.file_selection.size_only = long_flags.size_only;
    // upstream: options.c:2893 - bare --partial (no compact 'P' letter) tells the
    // receiver to keep interrupted temp files. OR with the compact value so a
    // legacy client that still packs 'P' is not clobbered.
    if long_flags.partial {
        config.flags.partial = true;
    }
    // upstream: options.c:2760-2765 - --specials / --no-specials override the
    // specials bit that the compact 'D' letter set to preserve_devices's value.
    if let Some(specials) = long_flags.specials {
        config.flags.specials = specials;
    }
    config.file_selection.ignore_existing = long_flags.ignore_existing;
    config.file_selection.existing_only = long_flags.existing_only;
    config.flags.numeric_ids = core::server::NumericIds::from_client(long_flags.numeric_ids);
    config.flags.delete = long_flags.delete;
    // upstream: options.c:2964-2965 - `--remove-source-files` is forwarded
    // long-form when the client requested sender-side removal. The flag is
    // consumed by the sender's `successful_send()` after each transferred
    // file is acknowledged.
    config.flags.remove_source_files = long_flags.remove_source_files;
    // upstream: options.c:2987 / flist.c:1419 - `--copy-devices` is forwarded to
    // the remote sender on a pull. As the server-side sender, this process must
    // convert each block/char device into a regular file and stream its bytes.
    config.flags.copy_devices = long_flags.copy_devices;
    // upstream: options.c:2996-2997 - `--mkpath` is forwarded long-form to the
    // server receiver on a push. The receiver gates dest-arg path creation on
    // this flag: without it, a missing ancestor chain is an error
    // (`main.c:788` single `do_mkdir`); with it, the whole chain is created
    // (`main.c:736` `make_path`).
    config.flags.mkpath = long_flags.mkpath;
    // upstream: options.c:2747-2748 / generator.c:1249 - `--list-only` forwarded
    // by the client tells the server to render the flist without writing to the
    // destination (`TransferFlags::skip_dest_writes`).
    config.flags.list_only = long_flags.list_only;
    // upstream: options.c:2046-2048 - do_stats sets info_levels[INFO_STATS] >= 2.
    // The server-side flag must be set so the generator emits NDX_DEL_STATS
    // during the goodbye phase (generator.c:2377,2422).
    config.do_stats = long_flags.stats;
    config.reference_directories = long_flags.reference_directories;
    // upstream: options.c:2886-2890 - `--partial-dir DIR` forwarded by the
    // sender. The server-side receiver moves interrupted temp files into this
    // directory and looks for resume basis files there. Without applying this
    // value, transfers that pin `--protocol=28` (where the client cannot
    // forward `partial_dir` in the compat flag string) leave the receiver
    // with no partial-dir at all - in that case the receiver's normal commit
    // path runs, which is what the regression test
    // `symlink-dirlink-basis_test.py` exercises through `lsh.sh`.
    if let Some(dir) = &long_flags.partial_dir {
        let path = std::path::PathBuf::from(dir);
        config.partial_dir = Some(path);
        config.has_partial_dir = true;
    }
    // upstream: options.c:2891-2892 - `--delay-updates` rides alongside
    // `--partial-dir` whenever both are active.
    if long_flags.delay_updates {
        config.write.delay_updates = true;
    }

    // upstream: options.c:2345-2348 - the server parses --log-format to set
    // stdout_format_has_i, which controls generator itemize output. `%i` sets
    // has_i = 1 (itemize significant items); `%I` sets has_i = 2, the `-ii`
    // level that also itemizes unchanged entries. The client forwards
    // `--log-format=%i%I` for `-ii` (options.c:164-175 server_options), so a
    // server that sees `%I` must also emit unchanged rows.
    if let Some(fmt) = &long_flags.log_format {
        if fmt.contains("%i") || fmt.contains("%I") {
            config.flags.info_flags.itemize = true;
        }
        if fmt.contains("%I") {
            config.flags.info_flags.itemize_unchanged = true;
        }
    }

    // upstream: rsync.c:85-147 setup_iconv() - server opens iconv against the
    // wire's UTF-8 charset using the local-side spec forwarded by the client
    // (options.c:2716-2723). Without this wiring the receiver/generator skip
    // the iconv hook and write/read raw bytes verbatim, breaking transfers
    // with --iconv=LOCAL,REMOTE where the on-disk filenames differ between
    // the two sides.
    if let Some(spec) = &long_flags.iconv {
        use core::client::IconvSetting;
        match IconvSetting::parse(spec) {
            Ok(setting) => config.connection.iconv = setting.resolve_converter(),
            Err(e) => {
                write_server_error(
                    stderr,
                    program_brand,
                    format!("invalid --iconv value '{spec}': {e}"),
                );
                return 1;
            }
        }
    }

    // upstream: options.c:2800-2805 - `--compress-choice`, `--new-compress`, and
    // `--old-compress` carry the explicit codec when the negotiated algorithm is
    // not the default CPRES_ZLIB. Without forwarding it into `ServerConfig`, the
    // SSH server path skips compression entirely (handshake.client_args is None
    // in SSH mode), so the receiver tries to decode upstream's compressed token
    // stream as plain tokens and eventually misaligns onto a multiplex frame
    // boundary.
    if let Some(name) = &long_flags.compress_choice {
        match protocol::CompressionAlgorithm::parse(name) {
            Ok(algo) => config.connection.compress_choice = Some(algo),
            Err(e) => {
                write_server_error(
                    stderr,
                    program_brand,
                    format!("invalid compression algorithm '{name}': {e}"),
                );
                return 1;
            }
        }
    }

    // upstream: options.c:2754-2758 - `--compress-level=N` forwarded by the
    // client sets `do_compression_level` on the server so its codec compresses
    // at the same level. The value is the numeric 0-9 that the client already
    // clamped before forwarding.
    if let Some(value) = &long_flags.compression_level {
        match value
            .trim()
            .parse::<u32>()
            .map_err(|e| e.to_string())
            .and_then(|n| {
                compress::zlib::CompressionLevel::from_numeric(n).map_err(|e| e.to_string())
            }) {
            Ok(level) => config.connection.compression_level = Some(level),
            Err(e) => {
                write_server_error(
                    stderr,
                    program_brand,
                    format!("invalid compression level '{value}': {e}"),
                );
                return 1;
            }
        }
    }

    // SEC-1.p extension: engage the Landlock allowlist on the receiver-side
    // `--server` path before any token-loop work. The lsh.sh-style invocation
    // exercises upstream `chdir-symlink-race` and `bare-do-open-symlink-race`
    // tests that swap a destination subdir for a symlink pointing outside the
    // requested root; without a kernel-enforced ruleset the receiver follows
    // the symlink and chmod's a file outside the destination tree. We confine
    // the calling thread to the (already-canonicalised, pre-flight-mkdir'd)
    // destination root so any subsequent path-based syscall that escapes the
    // tree gets EACCES from the kernel.
    //
    // Apply only when the receiver actually has a destination root to confine
    // to: the sender role and stat-only invocations have no write target, and
    // engaging an empty allowlist would deny their reads. Sandbox failures
    // surface as Unavailable on pre-5.13 kernels (SEC-1 *at* helpers remain
    // the sole defense) and Error on a kernel that advertised support but
    // returned an unexpected status; the latter is treated as a hard refusal
    // because the intended sandbox did not engage.
    if role == ServerRole::Receiver {
        if let Some(dest) = config.args.last() {
            let dest_path = std::path::PathBuf::from(dest);
            if let Some(root) = dest_path
                .canonicalize()
                .ok()
                .or_else(|| dest_path.parent().and_then(|p| p.canonicalize().ok()))
            {
                use fast_io::landlock::{LandlockOutcome, is_supported, restrict_to_module_paths};
                if is_supported() {
                    let canonical_root = root.clone();
                    let mut allowed = vec![root];

                    // UTS-V3-D: a remote files-from path (upstream
                    // `options.c:2944` -> server gets `--files-from <path>`)
                    // sits outside the destination tree. The receiver
                    // opens it in `forward_files_from_to_sender` to push
                    // filenames back to the sender; the landlock allowlist
                    // must include the file's parent or the open fails
                    // with EACCES (observed as the
                    // "Permission denied (os error 13)" branch when the
                    // receiver reached the new forwarder).
                    if let Some(path) = config.file_selection.files_from_path.as_deref() {
                        if path != "-" {
                            let p = std::path::PathBuf::from(path);
                            if let Some(canon) = p
                                .canonicalize()
                                .ok()
                                .or_else(|| p.parent().and_then(|d| d.canonicalize().ok()))
                            {
                                allowed.push(canon);
                            }
                        }
                    }

                    // upstream: generator.c:1356 - with --keep-dirlinks the
                    // receiver follows a destination symlink that resolves to a
                    // directory and writes through it, which upstream permits
                    // even when the target lives outside the transfer root. The
                    // Landlock allowlist would otherwise deny those writes
                    // (EACCES), so extend it with the canonical targets of any
                    // pre-existing destination symlink-to-directory entries. Only
                    // symlinks already present before the transfer are added, so
                    // the escape defense against a mid-transfer symlink swap is
                    // preserved for paths the user did not pre-establish.
                    if config.flags.keep_dirlinks {
                        collect_keep_dirlink_targets(&canonical_root, &mut allowed);
                    }

                    let allowed_refs: Vec<&std::path::Path> =
                        allowed.iter().map(|p| p.as_path()).collect();
                    match restrict_to_module_paths(&allowed_refs) {
                        LandlockOutcome::Enforced(_) | LandlockOutcome::Unavailable => {}
                        LandlockOutcome::Error(e) => {
                            write_server_error(
                                stderr,
                                program_brand,
                                format!("landlock sandbox engage failed: {e}"),
                            );
                            return 1;
                        }
                    }
                }
            }
        }
    }

    // upstream: main.c:1262 `start_server()` returns into `exit_cleanup(0)`,
    // which on a clean exit just runs `close_all()` + `exit()`. The kernel
    // closes the inherited stdio descriptors as the process tears down, and
    // the peer (whether upstream rsync over SSH, lsh.sh, or `--rsh=fake_rsh`)
    // observes EOF on its read side without any half-close dance in our
    // userspace. `handle_goodbye()` (receiver/transfer/phases.rs:145 +
    // generator/transfer/goodbye.rs:47) has already exchanged NDX_DONE in
    // both directions before `run_server_stdio` returns, so the protocol
    // handshake is complete; the only thing left is for the process to exit
    // and let the kernel close FDs.
    //
    // The previous flush + `shutdown_stdio_write` + `drain_stdin_until_eof`
    // sequence deadlocked under lsh.sh: shutdown(SHUT_WR) is ENOTSOCK on
    // pipes, the dup2(/dev/null) fallback only closes our copy of the pipe
    // FD (lsh.sh, the parent shell, still holds an inherited copy), so the
    // peer never sees EOF and the drain loops forever. Match upstream and
    // let process exit do the work.
    match run_server_stdio(config, &mut stdin, stdout, None) {
        Ok(_stats) => 0,
        Err(e) => {
            write_server_error(stderr, program_brand, format!("server error: {e}"));
            1
        }
    }
}

/// Collects the canonical directory targets of pre-existing destination
/// symlink-to-directory entries so they can be added to the `--keep-dirlinks`
/// Landlock allowlist.
///
/// Walks the real directory structure beneath `root` without descending into
/// symlinks (avoiding loops); each symlink whose target resolves to a directory
/// contributes its canonical path. The walk is bounded in depth and total
/// entries so a hostile or pathological destination tree cannot stall receiver
/// startup. Mirrors upstream's `--keep-dirlinks` trust model: the destination
/// symlinks the user pre-established are followed even when their targets sit
/// outside the transfer root (`generator.c:1356`).
fn collect_keep_dirlink_targets(root: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    const MAX_DEPTH: usize = 32;
    const MAX_ENTRIES: usize = 100_000;

    let mut seen: std::collections::HashSet<std::path::PathBuf> = std::collections::HashSet::new();
    let mut stack: Vec<(std::path::PathBuf, usize)> = vec![(root.to_path_buf(), 0)];
    let mut visited = 0usize;

    while let Some((dir, depth)) = stack.pop() {
        if depth > MAX_DEPTH {
            continue;
        }
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            visited += 1;
            if visited > MAX_ENTRIES {
                return;
            }
            let path = entry.path();
            // Classify without following the symlink so loops are impossible.
            let Ok(lst) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            if lst.file_type().is_symlink() {
                // A symlink that resolves to a directory is a kept dirlink;
                // allowlist its canonical target so writes through it succeed.
                if let Ok(canon) = std::fs::canonicalize(&path) {
                    if std::fs::metadata(&canon).is_ok_and(|m| m.file_type().is_dir())
                        && seen.insert(canon.clone())
                    {
                        out.push(canon);
                    }
                }
            } else if lst.file_type().is_dir() {
                stack.push((path, depth + 1));
            }
        }
    }
}

/// Applies value-bearing flags to the server config, returning early on parse errors.
fn apply_value_flags<Err: Write>(
    config: &mut core::server::ServerConfig,
    long_flags: &super::flags::ServerLongFlags,
    stderr: &mut Err,
    brand: Brand,
) -> Result<(), i32> {
    if let Some(seed_str) = &long_flags.checksum_seed {
        match parse_server_checksum_seed(seed_str) {
            Ok(seed) => config.checksum_seed = Some(seed),
            Err(msg) => {
                write_server_error(stderr, brand, msg);
                return Err(1);
            }
        }
    }

    if let Some(algo_str) = &long_flags.checksum_choice {
        match protocol::ChecksumAlgorithm::parse(algo_str) {
            Ok(algo) => config.checksum_choice = Some(algo),
            Err(e) => {
                write_server_error(stderr, brand, format!("invalid --checksum-choice: {e}"));
                return Err(1);
            }
        }
    }

    if let Some(size_str) = &long_flags.min_size {
        match parse_server_size_limit(size_str, "--min-size") {
            Ok(size) => config.file_selection.min_file_size = Some(size),
            Err(msg) => {
                write_server_error(stderr, brand, msg);
                return Err(1);
            }
        }
    }

    if let Some(size_str) = &long_flags.max_size {
        match parse_server_size_limit(size_str, "--max-size") {
            Ok(size) => config.file_selection.max_file_size = Some(size),
            Err(msg) => {
                write_server_error(stderr, brand, msg);
                return Err(1);
            }
        }
    }

    // upstream: options.c - server_options() forwards `--modify-window=NUM`.
    // The receiver's quick-check honours it via same_time() so files within
    // the window are not needlessly re-transferred over the network.
    if let Some(window_str) = &long_flags.modify_window {
        match super::super::execution::parse_modify_window_argument(std::ffi::OsStr::new(
            window_str,
        )) {
            Ok(window) => config.file_selection.modify_window = window,
            Err(msg) => {
                write_server_error(stderr, brand, msg.text().to_owned());
                return Err(1);
            }
        }
    }

    // upstream: options.c:1943-1950 - server-side `--max-alloc` is parsed and
    // applied to the local allocator. We forward it from the client and
    // enforce the cap on the server's buffer pool.
    if let Some(alloc_str) = &long_flags.max_alloc {
        match super::super::execution::parse_max_alloc_argument(std::ffi::OsStr::new(alloc_str)) {
            Ok(limit) => {
                if let Ok(limit_usize) = usize::try_from(limit)
                    && limit_usize > 0
                {
                    let cfg = engine::local_copy::GlobalBufferPoolConfig {
                        byte_budget: Some(limit_usize),
                        ..engine::local_copy::GlobalBufferPoolConfig::default()
                    };
                    let _ = engine::local_copy::init_global_buffer_pool(cfg);
                }
            }
            Err(message) => {
                write_server_error(stderr, brand, message.to_string());
                return Err(1);
            }
        }
    }

    if let Some(when_str) = &long_flags.stop_at {
        match parse_server_stop_at(when_str) {
            Ok(deadline) => config.stop_at = Some(deadline),
            Err(msg) => {
                write_server_error(stderr, brand, msg);
                return Err(1);
            }
        }
    }

    if let Some(mins_str) = &long_flags.stop_after {
        match parse_server_stop_after(mins_str) {
            Ok(deadline) => config.stop_at = Some(deadline),
            Err(msg) => {
                write_server_error(stderr, brand, msg);
                return Err(1);
            }
        }
    }

    if let Some(max_del_str) = &long_flags.max_delete {
        match max_del_str.parse::<u64>() {
            Ok(limit) => config.deletion.max_delete = Some(limit),
            Err(_) => {
                write_server_error(
                    stderr,
                    brand,
                    format!("invalid --max-delete value '{max_del_str}'"),
                );
                return Err(1);
            }
        }
    }

    if let Some(depth_str) = &long_flags.io_uring_depth {
        match depth_str.parse::<u32>() {
            Ok(parsed) => match fast_io::validate_io_uring_depth(parsed) {
                Ok(depth) => config.write.io_uring_depth = Some(depth),
                Err(e) => {
                    write_server_error(
                        stderr,
                        brand,
                        format!("invalid --io-uring-depth value '{depth_str}': {e}"),
                    );
                    return Err(1);
                }
            },
            Err(_) => {
                write_server_error(
                    stderr,
                    brand,
                    format!("invalid --io-uring-depth value '{depth_str}'"),
                );
                return Err(1);
            }
        }
    }

    Ok(())
}

fn write_server_error<Err: Write>(stderr: &mut Err, brand: Brand, text: impl fmt::Display) {
    let mut sink = MessageSink::with_brand(stderr, brand);
    let mut message = rsync_error!(1, "{}", text);
    message = message.with_role(Role::Server);
    if super::super::write_message(&message, &mut sink).is_err() {
        let _ = writeln!(sink.writer_mut(), "{text}");
    }
}

#[cfg(all(test, unix))]
mod keep_dirlink_target_tests {
    use std::fs;
    use std::os::unix::fs::symlink;

    use super::collect_keep_dirlink_targets;

    /// A destination symlink resolving to a directory contributes its canonical
    /// target so the `--keep-dirlinks` Landlock allowlist permits writes through
    /// it; a symlink-to-file and a plain directory contribute nothing.
    #[test]
    fn collects_only_symlink_to_dir_targets() {
        let scratch = tempfile::tempdir().expect("tempdir");
        let root = scratch.path().join("dest");
        fs::create_dir(&root).unwrap();

        // A symlink-to-directory whose target lives outside the dest root.
        let outside = scratch.path().join("outside");
        fs::create_dir(&outside).unwrap();
        symlink(&outside, root.join("dlink")).unwrap();

        // A symlink-to-file (not a dirlink) and a plain directory: neither is a
        // kept dirlink target.
        let file = scratch.path().join("f.txt");
        fs::write(&file, b"x").unwrap();
        symlink(&file, root.join("flink")).unwrap();
        fs::create_dir(root.join("realdir")).unwrap();

        let mut out = Vec::new();
        collect_keep_dirlink_targets(&root, &mut out);

        let canon_outside = fs::canonicalize(&outside).unwrap();
        assert!(
            out.contains(&canon_outside),
            "the symlink-to-directory target must be allowlisted: {out:?}"
        );
        assert_eq!(
            out.len(),
            1,
            "only the symlink-to-directory contributes a target: {out:?}"
        );
    }

    /// A nested symlink-to-directory (below the root) is also collected, since
    /// upstream follows kept dirlinks at any depth.
    #[test]
    fn collects_nested_symlink_to_dir_targets() {
        let scratch = tempfile::tempdir().expect("tempdir");
        let root = scratch.path().join("dest");
        let sub = root.join("sub");
        fs::create_dir_all(&sub).unwrap();

        let outside = scratch.path().join("outside");
        fs::create_dir(&outside).unwrap();
        symlink(&outside, sub.join("dlink")).unwrap();

        let mut out = Vec::new();
        collect_keep_dirlink_targets(&root, &mut out);

        let canon_outside = fs::canonicalize(&outside).unwrap();
        assert!(
            out.contains(&canon_outside),
            "a nested kept-dirlink target must be allowlisted: {out:?}"
        );
    }
}
