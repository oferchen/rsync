// Construction of the `ServerConfig` from the parsed client arguments: server
// role detection, compact-flag verbosity clamping, and the main
// `build_server_config` assembly that wires module directives into the config.
/// Determines the server role based on client arguments.
///
/// The `--sender` flag indicates that the SERVER should act as sender (Generator).
/// When absent, the SERVER should act as receiver (Receiver).
fn determine_server_role(client_args: &[String]) -> ServerRole {
    if client_args.iter().any(|arg| arg == "--sender") {
        ServerRole::Generator
    } else {
        ServerRole::Receiver
    }
}

/// Clamps `v` (verbose) characters in a compact flag string to respect `max_verbosity`.
///
/// When `max_verbosity` is 0 or negative, all `v` characters are removed.
/// When positive, at most `max_verbosity` occurrences of `v` are retained.
///
/// upstream: clientserver.c - the daemon clamps `verbose` to `lp_max_verbosity(i)`
/// after parsing client args.
fn clamp_verbose_flags(flag_string: &str, max_verbosity: i32) -> String {
    if max_verbosity < 0 {
        return flag_string.replace('v', "");
    }

    let max = max_verbosity as usize;
    let mut count = 0usize;
    flag_string
        .chars()
        .filter(|&ch| {
            if ch == 'v' {
                count += 1;
                count <= max
            } else {
                true
            }
        })
        .collect()
}

/// Counts the `v` (verbose) characters in an already-clamped flag string,
/// yielding the effective per-connection verbosity level.
///
/// The input is expected to be the output of [`clamp_verbose_flags`], so the
/// count already respects the module's `max verbosity`. The result seeds the
/// worker thread's [`logging::VerbosityConfig`] via `apply_verbosity`,
/// matching upstream's `limit_output_verbosity(lp_max_verbosity(i))`
/// (clientserver.c:1127). Saturates at [`u8::MAX`].
fn clamped_verbose_level(flag_string: &str) -> u8 {
    let count = flag_string.chars().filter(|&ch| ch == 'v').count();
    u8::try_from(count).unwrap_or(u8::MAX)
}
/// Builds the server configuration from client arguments.
///
/// Returns the configuration on success, or sends an error and returns `None`.
fn build_server_config(
    ctx: &mut ModuleRequestContext<'_>,
    client_args: &[String],
    module: &ModuleRuntime,
) -> io::Result<Option<ServerConfig>> {
    let role = determine_server_role(client_args);

    let flag_string = client_args
        .iter()
        .find(|arg| arg.starts_with('-') && !arg.starts_with("--"))
        .cloned()
        .unwrap_or_default();

    // upstream: clientserver.c - clamp verbose to lp_max_verbosity(i)
    let flag_string = clamp_verbose_flags(&flag_string, module.max_verbosity);

    // upstream: clientserver.c:1127 `limit_output_verbosity(lp_max_verbosity(i))`
    // caps the per-connection log verbosity once the module is selected. Each
    // oc-rsync connection runs on its own worker thread whose thread-local
    // `logging::VerbosityConfig` starts at level 0, so seed it from the clamped
    // client request here. Without this, daemon-side `info_log!`/`debug_log!`
    // emissions during the transfer stay silent regardless of the client's
    // `-v`/`-vv` and the module's `max verbosity`, since the daemon's own
    // startup `apply_verbosity` only seeded the main accept-loop thread.
    crate::daemon::apply_verbosity(clamped_verbose_level(&flag_string));

    // upstream: main.c:1203-1204 + util1.c:804 (glob_expand_module) - receivers
    // resolve their destination by joining the module path with the client's
    // module-relative tail (e.g. `upload/realdir/` -> module + `realdir/`).
    // Senders (pull requests) split each positional the same way so the
    // sender's per-source `dir/fn` (flist.c:2338-2349) walks the requested
    // sub-tree instead of the entire module root. The original argv[0] is
    // always the module root; legacy tests that push straight into the module
    // root keep that behaviour.
    let positional_args: Vec<OsString> = if role == ServerRole::Receiver {
        match resolve_receiver_dest(std::path::Path::new(&module.path), client_args, &module.name) {
            Some(dest) => vec![OsString::from(dest.as_os_str())],
            None => {
                let payload =
                    "@ERROR: requested path resolves outside module root".to_owned();
                send_error_and_exit(ctx.reader.get_mut(), ctx.limiter, ctx.messages, &payload)?;
                return Ok(None);
            }
        }
    } else {
        match resolve_sender_sources(std::path::Path::new(&module.path), client_args, &module.name) {
            Some(sources) => sources
                .into_iter()
                .map(|p| OsString::from(p.as_os_str()))
                .collect(),
            None => {
                let payload =
                    "@ERROR: requested path resolves outside module root".to_owned();
                send_error_and_exit(ctx.reader.get_mut(), ctx.limiter, ctx.messages, &payload)?;
                return Ok(None);
            }
        }
    };

    match ServerConfig::from_flag_string_and_args(
        role,
        flag_string,
        positional_args,
    ) {
        Ok(mut cfg) => {
            // Parse long-form arguments that upstream rsync sends via server_options()
            // (options.c:2737-2980). The compact flag string only covers single-char
            // flags; these long-form options must be parsed separately.
            //
            // Rule 12 fail-loud: when a client-only batch flag slips past the
            // client-side sanitiser, surface an explicit `@ERROR` here rather
            // than silently dropping the option and continuing into a wire
            // path that closes mid file-list framing.
            //
            // upstream: options.c:1444-1449 - daemon-mode unknown option
            // emits `rsync: <BAD>: <err> (in daemon mode)` and exits
            // `RERR_SYNTAX` via `daemon_error:` (options.c:1464-1466).
            if let Some(offender) = apply_long_form_args(client_args, &mut cfg) {
                if let Some(log) = ctx.log_sink {
                    let text = format!(
                        "module '{}': refusing client-only flag '{offender}' in daemon mode",
                        ctx.request,
                    );
                    let message = rsync_warning!(text).with_role(Role::Daemon);
                    log_message(log, &message);
                }
                let payload =
                    format!("@ERROR: {offender}: unrecognized option (in daemon mode)");
                send_error_and_exit(ctx.reader.get_mut(), ctx.limiter, ctx.messages, &payload)?;
                return Ok(None);
            }

            // upstream: options.c:2737-2740 - when -z is in the compact flag string
            // but no explicit --compress-level=N was sent, default to level 6 (the
            // upstream default). Without this, compression_level stays None and the
            // transfer pipeline won't activate token-level compression.
            if cfg.flags.compress && cfg.connection.compression_level.is_none() {
                cfg.connection.compression_level = Some(compress::zlib::CompressionLevel::Default);
            }

            // upstream: main.c:1199-1206 calls `check_alt_basis_dirs()` after
            // `get_local_name(flist, argv[0])` chdir's into the dest directory,
            // so relative basis paths like `--link-dest=../01` resolve against
            // the receiver's destination (a sibling of `dest/00/`), not against
            // the module root. We do not chdir, so resolve relative ref_dirs
            // explicitly against the receiver's dest directory (the positional
            // arg). For sender role positionals are source paths, not a dest;
            // keep the module-root fallback so the legacy compare-dest lookup
            // path stays unchanged.
            //
            // The resolved path is then confined inside the module root: if
            // the lexical climb (`..`) escapes the module tree the ref_dir is
            // silently dropped so the basis lookup falls through to a normal
            // transfer instead of leaking files from outside the module
            // (link-dest-module-escape security pin).
            let module_root_canonical = std::path::Path::new(&module.path)
                .canonicalize()
                .unwrap_or_else(|_| std::path::PathBuf::from(&module.path));
            let resolve_base: std::path::PathBuf = if role == ServerRole::Receiver {
                cfg.args
                    .first()
                    .map(std::path::PathBuf::from)
                    .unwrap_or_else(|| std::path::PathBuf::from(&module.path))
            } else {
                std::path::PathBuf::from(&module.path)
            };
            cfg.reference_directories.retain_mut(|ref_dir| {
                match confine_basis_under_module(
                    &ref_dir.path,
                    &resolve_base,
                    &module_root_canonical,
                ) {
                    Some(resolved) => {
                        ref_dir.path = resolved;
                        true
                    }
                    None => false,
                }
            });

            // upstream: loadparm.c - `temp dir` module parameter provides a
            // default temp directory. The client's --temp-dir takes precedence
            // if already set from apply_long_form_args.
            if cfg.temp_dir.is_none() {
                if let Some(ref dir) = module.temp_dir {
                    cfg.temp_dir = Some(std::path::PathBuf::from(dir));
                }
            }

            // upstream: loadparm.c - `dont compress` parameter specifies suffixes
            // that should skip per-file compression during transfer.
            if let Some(dont_compress) = module.dont_compress.as_deref() {
                if let Some(list) = parse_daemon_dont_compress(dont_compress) {
                    cfg.skip_compress = Some(list);
                }
            }

            // upstream: clientserver.c:712-716 - `iconv_opt = lp_charset(i);
            // if (*iconv_opt) setup_iconv();` resolves the module's `charset =`
            // directive into the iconv handles used for filename transcoding.
            // Without this wiring the daemon would parse `charset = LATIN1` but
            // never apply it, leaving --iconv negotiation a silent no-op.
            cfg.connection.iconv = resolve_module_charset_converter(module.charset.as_deref());

            // upstream: `use_secure_symlinks = am_daemon && !am_chrooted`
            // (clientserver.c:1018). Mark the server-side daemon connection so
            // the receiver's DirSandbox open enforces the symlink-refusal
            // policy instead of silently falling back to path-based syscalls -
            // that fall-back is what reopened the chdir-symlink-race attack
            // window once the original CVE-2026-29518 fix landed.
            cfg.connection.is_daemon_connection = true;

            // upstream: clientserver.c:1106-1107 - `fake super = yes` on the
            // daemon module demotes the receiver's am_root and forces fake-super
            // semantics regardless of whether the client requested --fake-super.
            // The directive is purely daemon-config-driven; client --fake-super
            // is demoted to --super on the wire and never reaches us.
            cfg.fake_super = module.fake_super;

            // upstream: clientserver.c:rsync_module() - the `incoming chmod`
            // and `outgoing chmod` directives feed `parse_chmod(...)` and the
            // parsed clauses arm `daemon_chmod_modes`, applied at flist build
            // time (sender) and at file finalize time (receiver). We delay
            // parsing to module-use rather than module-load so the operator
            // sees the @ERROR live; an invalid spec aborts the session with
            // the same exit semantics as a bad client option.
            match parse_daemon_chmod_specs(module) {
                Ok((incoming, outgoing)) => {
                    cfg.daemon_incoming_chmod = incoming;
                    cfg.daemon_outgoing_chmod = outgoing;
                }
                Err(err) => {
                    let payload = format!("@ERROR: {err}");
                    send_error_and_exit(
                        ctx.reader.get_mut(),
                        ctx.limiter,
                        ctx.messages,
                        &payload,
                    )?;
                    return Ok(None);
                }
            }

            // upstream: clientserver.c:992-993 - `munge_symlinks = lp_munge_symlinks(i)`
            // with `!use_chroot || module_dirlen` as the auto default. The bit is
            // purely daemon-config-driven (no client-side override) and travels
            // through the transfer layer so the sender strips `/rsyncd-munged/`
            // on `readlink()` and the receiver prepends it on `symlink()` writes.
            cfg.munge_symlinks = module.effective_munge_symlinks();

            Ok(Some(cfg))
        }
        Err(err) => {
            let payload = format!("@ERROR: failed to configure server: {err}");
            send_error_and_exit(ctx.reader.get_mut(), ctx.limiter, ctx.messages, &payload)?;
            Ok(None)
        }
    }
}
