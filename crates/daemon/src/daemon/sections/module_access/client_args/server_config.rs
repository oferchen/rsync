// Construction of the `ServerConfig` from the parsed client arguments: server
// role detection, per-module output-verbosity limiting, and the main
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

/// Largest verbosity level the `info_verbosity[]` / `debug_verbosity[]` tables
/// describe. upstream: options.c:247 `#define MAX_VERBOSITY ((int)(sizeof
/// debug_verbosity / sizeof debug_verbosity[0]) - 1)`, whose table
/// (options.c:238-245) runs 0..=5.
const MAX_VERBOSITY: u8 = 5;

/// Caps the client's requested verbosity at the module's `max verbosity`,
/// yielding the effective per-connection output level.
///
/// This limits the LEVEL only: the client's option string is never rewritten,
/// so the `-e.<caps>` capability payload it carries reaches the capability
/// decoder byte-for-byte.
///
/// upstream: clientserver.c:1141 `limit_output_verbosity(lp_max_verbosity(i))`
/// runs after `parse_arguments()` and only lowers the entries of the
/// `info_levels[]` / `debug_levels[]` output arrays (options.c:537-560); it
/// leaves `verbose` and the parsed argv untouched. A `level` above
/// `MAX_VERBOSITY` returns early (options.c:542-543), applying no limit at all.
fn limit_output_verbosity(requested: u8, max_verbosity: i32) -> u8 {
    if max_verbosity > i32::from(MAX_VERBOSITY) {
        return requested;
    }
    let max = u8::try_from(max_verbosity).unwrap_or(0);
    requested.min(max)
}

/// Applies the module's `max verbosity` to a freshly parsed [`ServerConfig`]
/// and returns the effective output level.
///
/// The requested level is whatever `ParsedServerFlags` counted, and that scan
/// stops at the `-e` capability separator, so the `v` that
/// `maybe_add_e_option` (options.c:3040) always appends inside `-e.<caps>` is
/// never mistaken for a `-v`. That letter is the client's
/// `CF_VARINT_FLIST_FLAGS` / negotiated-strings advertisement
/// (compat.c:730-733); upstream's popt hands everything after `-e` to
/// `shell_cmd` (options.c:823 `{"rsh", 'e', POPT_ARG_STRING, &shell_cmd, ...}`)
/// and only ever `strchr`es it, so nothing here may read it as option letters
/// and nothing may rewrite it - `cfg.flag_string` stays byte-identical to what
/// the client sent.
///
/// `cfg.flags` is capped alongside the returned level because oc gates some
/// server-side output on the flags rather than on the thread-local
/// `logging::VerbosityConfig`, while upstream's single capped `info_levels[]`
/// array governs every output decision.
fn apply_output_verbosity_limit(cfg: &mut ServerConfig, max_verbosity: i32) -> u8 {
    let level = limit_output_verbosity(cfg.flags.verbose_level, max_verbosity);
    cfg.flags.verbose_level = level;
    cfg.flags.verbose = level > 0;
    level
}

/// Applies module directives that force transfer-time behavior onto the
/// per-session [`ServerConfig`], mirroring the daemon-only overrides upstream
/// applies in `rsync_module()` after the client argv is parsed.
///
/// - `ignore errors = yes` forces error-tolerant deletion regardless of the
///   client's flags. upstream: clientserver.c:1111-1112 -
///   `if (lp_ignore_errors(module_id)) ignore_errors = 1;`.
/// - `numeric ids` forces `numeric_ids = -1` for the session. upstream:
///   clientserver.c:1201-1204 -
///   `if (!numeric_ids && (use_chroot ? lp_numeric_ids(module_id) != False &&
///   !*lp_name_converter(module_id) : lp_numeric_ids(module_id) == True))
///   numeric_ids = -1;`. The directive is a BOOL3 tri-state: under chroot an
///   unset OR yes value forces it on (unless a `name converter` maps names
///   inside the chroot), because there is no `/etc/passwd` in the chroot;
///   without chroot only an explicit yes forces it, and an explicit no is
///   never overridden. It is forced only when the client did not already
///   request it. Upstream sets the sentinel `-1` (not `1`) so the uid/gid
///   name-list stays on the wire; oc mirrors that with
///   `NumericIds::DaemonForced`, distinct from the client's explicit
///   `NumericIds::Explicit` which also drops the wire list.
fn apply_module_transfer_directives(module: &ModuleDefinition, cfg: &mut ServerConfig) {
    // upstream: clientserver.c:821 `module_id = i` - selecting a module makes
    // `module_id >= 0` for the rest of the server process, and that is the sole
    // condition under which `full_fname()` (util1.c:1290) appends
    // ` (in MODULE)` after the closing quote of every path it renders into an
    // error or warning. Record the name so the transfer layer reproduces it.
    cfg.connection.daemon_module = Some(module.name.clone());

    // upstream: clientserver.c:864,993 - `module_chdir` is the normalized
    // module path and the server `chdir()`s into it before serving, which is
    // why `full_fname()` (util1.c:1285) only ever prints the part of a path
    // below `module_dirlen`. oc-rsync keeps absolute paths, so record the root
    // and let the transfer layer strip it.
    cfg.connection.daemon_module_root = Some(module.path.clone());

    // upstream: clientserver.c:1111-1112
    if module.ignore_errors {
        cfg.deletion.ignore_errors = true;
    }

    // upstream: clientserver.c:1201-1204
    //   if (!numeric_ids
    //    && (use_chroot ? lp_numeric_ids(module_id) != False
    //                       && !*lp_name_converter(module_id)
    //                   : lp_numeric_ids(module_id) == True))
    //       numeric_ids = -1;
    //
    // `numeric ids` is a BOOL3: `None` = unset, `Some(true)` = True,
    // `Some(false)` = False. Under chroot an unset OR yes value (`!= False`)
    // forces numeric ids on - inside the chroot there is no `/etc/passwd`, so
    // name<->id resolution is impossible - unless a `name converter` maps names
    // inside the chroot. Without chroot only an explicit yes (`== True`) forces
    // it. An explicit no (`Some(false)`) is never overridden. The daemon-forced
    // `-1` state suppresses local name resolution but keeps the uid/gid name-
    // list on the wire (`numeric_ids <= 0`), so a real upstream client whose
    // own `numeric_ids` is `0` still has its transmitted list consumed.
    let module_forces_numeric_ids = if module.use_chroot {
        module.numeric_ids != Some(false) && module.name_converter.is_none()
    } else {
        module.numeric_ids == Some(true)
    };
    if cfg.flags.numeric_ids.is_off() && module_forces_numeric_ids {
        cfg.flags.numeric_ids = core::server::NumericIds::DaemonForced;
    }

    // upstream: loadparm `open noatime` - the module directive makes the
    // daemon (as sender) open source files with O_NOATIME. It can only enable
    // the flag, never clear a value the client already requested.
    if module.open_noatime {
        cfg.write.open_noatime = true;
    }
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

    // upstream: main.c:1203-1204 + util1.c:804 (glob_expand_module) - receivers
    // resolve their destination by joining the module path with the client's
    // module-relative tail (e.g. `upload/realdir/` -> module + `realdir/`).
    // Senders (pull requests) split each positional the same way so the
    // sender's per-source `dir/fn` (flist.c:2338-2349) walks the requested
    // sub-tree instead of the entire module root. The original argv[0] is
    // always the module root; legacy tests that push straight into the module
    // root keep that behaviour.
    // Both resolvers are total: they collapse `..` the way upstream's
    // `sanitize_path` does at depth 0, so the result is confined to the module
    // root by construction and there is no "resolves outside module root"
    // rejection to represent. Upstream has no such daemon error either - a
    // traversing tail is rewritten and served (util1.c:1183).
    let positional_args: Vec<OsString> = if role == ServerRole::Receiver {
        let dest = resolve_receiver_dest(std::path::Path::new(&module.path), client_args, &module.name);
        vec![OsString::from(dest.as_os_str())]
    } else {
        resolve_sender_sources(std::path::Path::new(&module.path), client_args, &module.name)
            .into_iter()
            .map(|p| OsString::from(p.as_os_str()))
            .collect()
    };

    match ServerConfig::from_flag_string_and_args(
        role,
        flag_string,
        positional_args,
    ) {
        Ok(mut cfg) => {
            // upstream: clientserver.c:1141 `limit_output_verbosity(lp_max_verbosity(i))`
            // caps the per-connection log verbosity once the module is selected.
            // Each oc-rsync connection runs on its own worker thread whose
            // thread-local `logging::VerbosityConfig` starts at level 0, so seed
            // it here; without that, daemon-side `info_log!`/`debug_log!`
            // emissions stay silent regardless of the client's `-v`, since the
            // daemon's startup `apply_verbosity` only seeded the accept loop.
            crate::daemon::apply_verbosity(apply_output_verbosity_limit(
                &mut cfg,
                module.max_verbosity,
            ));

            // Parse long-form arguments that upstream rsync sends via server_options()
            // (options.c:2737-2980). The compact flag string only covers single-char
            // flags; these long-form options must be parsed separately.
            //
            // Rule 12 fail-loud: when a client-only batch flag slips past the
            // client-side sanitiser, surface an explicit `@ERROR` here rather
            // than silently dropping the option and continuing into a wire
            // path that closes mid file-list framing.
            //
            // upstream: options.c:1460-1465 - daemon-mode unknown option
            // emits `rsync: <BAD>: <err> (in daemon mode)` and exits
            // `RERR_SYNTAX` via `daemon_error:` (options.c:1480-1482).
            if let Some(rejection) = apply_long_form_args(client_args, &mut cfg) {
                let (log_text, error_text) = match rejection {
                    ClientArgRejection::Unrecognized(offender) => (
                        format!(
                            "module '{}': refusing client-only flag '{offender}' in daemon mode",
                            ctx.request,
                        ),
                        format!("{offender}: unrecognized option (in daemon mode)"),
                    ),
                    // upstream: options.c:907-918 `option_error()` reports the
                    // parser's own `err_buf` text, so the value's own message
                    // is what reaches the client - not the unknown-option one.
                    ClientArgRejection::InvalidValue(message) => (
                        format!("module '{}': {message}", ctx.request),
                        message.clone(),
                    ),
                };
                if let Some(log) = ctx.log_sink {
                    let message = rsync_warning!(log_text).with_role(Role::Daemon);
                    log_message(log, &message);
                }
                let error = AtError::message(error_text);
                send_error(ctx.reader.get_mut(), ctx.limiter, &error)?;
                return Ok(None);
            }

            // upstream: options.c:2755-2758 - when -z is in the compact flag string
            // but no explicit --compress-level=N was sent, default to level 6 (the
            // upstream default). Without this, compression_level stays None and the
            // transfer pipeline won't activate token-level compression.
            if cfg.flags.compress && cfg.connection.compression_level.is_none() {
                cfg.connection.compression_level = Some(compress::zlib::CompressionLevel::Default);
            }

            // upstream: main.c:1217-1224 calls `check_alt_basis_dirs()` after
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
            // that should skip per-file compression during transfer. However,
            // token.c:225 set_compression()'s per-file suffix lookup is compiled
            // out under `#if 0` ("No compression algorithms currently allow
            // mid-stream changing of the level."), so a non-`*` suffix list has no
            // per-file wire effect. The only live case is a bare `*`, which
            // collapses the whole zlib stream to store (level 0) at init
            // (token.c:206-211) - still deflated framing, never plain tokens.
            if let Some(dont_compress) = module.dont_compress.as_deref() {
                if dont_compress_is_match_all(dont_compress) {
                    cfg.connection.dont_compress_match_all = true;
                }
            }

            // upstream: clientserver.c:714-718 - `iconv_opt = lp_charset(i);
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

            // upstream: options.c:2390-2397 - the daemon sender paces its own
            // outbound socket writes (io.c:846,861) at the client's forwarded
            // `--bwlimit` capped by the daemon-wide bwlimit. `ctx.limiter`
            // already holds that daemon-side cap, and it is the SAME limiter that
            // throttles the pre-transfer `@RSYNCD:` text phase
            // (session_runtime.rs `write_limited`); the bulk phase runs on the
            // separate `run_server_with_handshake` writer stack, so carrying the
            // rate here installs one limiter per phase with no double-throttle.
            // A receiver ignores this (main.c:1068), so it is set unconditionally.
            {
                let client_rate = client_args.iter().find_map(|arg| {
                    arg.strip_prefix("--bwlimit=")
                        .and_then(|value| parse_bandwidth_limit(value).ok())
                        .and_then(|components| components.rate())
                });
                let daemon_cap = ctx.limiter.as_ref().map(BandwidthLimiter::limit_bytes);
                // upstream: options.c:2390 `if (daemon_bwlimit && (!bwlimit ||
                // bwlimit > daemon_bwlimit)) bwlimit = daemon_bwlimit;`
                let effective = match (client_rate, daemon_cap) {
                    (Some(client), Some(cap)) => Some(client.min(cap)),
                    (Some(client), None) => Some(client),
                    (None, Some(cap)) => Some(cap),
                    (None, None) => None,
                };
                cfg.connection.bwlimit =
                    effective.map(|rate| BandwidthLimitComponents::new(Some(rate)));
            }

            // upstream: clientserver.c:1120-1121 - `fake super = yes` on the
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
                    let error = AtError::message(err.to_string());
                    send_error(ctx.reader.get_mut(), ctx.limiter, &error)?;
                    return Ok(None);
                }
            }

            // upstream: clientserver.c:997-998 - `munge_symlinks = lp_munge_symlinks(i)`
            // with `!use_chroot || module_dirlen` as the auto default. The bit is
            // purely daemon-config-driven (no client-side override) and travels
            // through the transfer layer so the sender strips `/rsyncd-munged/`
            // on `readlink()` and the receiver prepends it on `symlink()` writes.
            cfg.munge_symlinks = module.effective_munge_symlinks();

            // upstream: clientserver.c:1111-1112 (`ignore errors`) and
            // clientserver.c:1201-1204 (`numeric ids`) - module directives that
            // force transfer behavior for the session once client args are known.
            apply_module_transfer_directives(module, &mut cfg);

            Ok(Some(cfg))
        }
        Err(err) => {
            let error = AtError::message(format!("failed to configure server: {err}"));
            send_error(ctx.reader.get_mut(), ctx.limiter, &error)?;
            Ok(None)
        }
    }
}
