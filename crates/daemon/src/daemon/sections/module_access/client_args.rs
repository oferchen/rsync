// Client argument reading and server configuration building.
//
// After the daemon sends `@RSYNCD: OK`, the client transmits its command-line
// arguments (the same arguments that `server_options()` would produce for an
// SSH-mode server invocation). The daemon parses these to configure the
// transfer engine with the correct flags, paths, and options.
//
// upstream: io.c:1292 - `read_args()` reads null/newline-terminated arguments.
// options.c:2737-2980 - `server_options()` emits the long-form options.
// clientserver.c:1059-1073 - two-phase secluded-args reading.

/// Reverses `safe_arg()`'s backslash escaping of an option arg, in place.
///
/// Walks the string from left to right collapsing every `\X` sequence into
/// `X` (where `X` is any non-NUL character). A trailing lone backslash is
/// preserved verbatim, matching upstream's `if (*f == '\\' && f[1])` guard.
///
/// This is the receive-side counterpart to upstream's `options.c:safe_arg()`
/// client-side escaper. Under non-protect_args daemon mode, upstream rsync
/// 3.4.4 began calling this on every option arg in `read_args()` so that
/// shell metacharacters such as `*`, `?`, `;` round-trip through the wire
/// regardless of remote-shell behaviour.
///
/// # Upstream Reference
///
/// - `io.c:1295-1306` - `unbackslash_arg(char *s)` in rsync 3.4.4.
fn unbackslash_arg(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 1 < bytes.len() {
            i += 1;
        }
        out.push(bytes[i]);
        i += 1;
    }
    // upstream: `unbackslash_arg` operates on a C string in place; we walk
    // the original UTF-8 bytes and only drop ASCII backslashes that precede
    // another byte, so the result remains valid UTF-8 whenever the input was.
    String::from_utf8(out).unwrap_or_else(|err| {
        String::from_utf8_lossy(err.as_bytes()).into_owned()
    })
}

/// Applies [`unbackslash_arg`] to every option arg that precedes the `.`
/// CWD marker, mirroring upstream `io.c:1336-1359`'s split between option
/// and file args. File args after the dot are left verbatim because upstream
/// dispatches them through `glob_expand()` rather than `unbackslash_arg()`.
fn unescape_phase1_option_args(args: Vec<String>) -> Vec<String> {
    let mut out = Vec::with_capacity(args.len());
    let mut past_dot = false;
    for arg in args {
        if past_dot {
            out.push(arg);
        } else {
            let is_dot = arg == ".";
            out.push(unbackslash_arg(&arg));
            if is_dot {
                past_dot = true;
            }
        }
    }
    out
}

/// Reads client arguments sent after module approval.
///
/// After the daemon sends "@RSYNCD: OK", the client sends its command-line
/// arguments (e.g., "--server", "-r", "-a", "."). This mirrors upstream's
/// `read_args()` function in io.c:1292.
///
/// For protocol >= 30: arguments are null-byte terminated
/// For protocol < 30: arguments are newline terminated
/// An empty argument marks the end of the list.
fn read_client_arguments<R: BufRead>(
    reader: &mut R,
    protocol: Option<ProtocolVersion>,
) -> io::Result<Vec<String>> {
    let use_nulls = protocol.is_some_and(|p| p.as_u8() >= 30);
    let mut arguments = Vec::new();

    loop {
        if use_nulls {
            let mut buf = Vec::new();
            let bytes_read = reader.read_until(b'\0', &mut buf)?;

            if bytes_read == 0 {
                break;
            }

            if buf.last() == Some(&b'\0') {
                buf.pop();
            }

            if buf.is_empty() {
                break;
            }

            let arg = String::from_utf8_lossy(&buf).into_owned();
            arguments.push(arg);
        } else {
            let line = match read_trimmed_line(reader)? {
                Some(line) => line,
                None => break,
            };

            if line.is_empty() {
                break;
            }

            arguments.push(line);
        }
    }

    Ok(arguments)
}

/// Checks whether phase-1 args contain the secluded-args (`-s`) flag.
///
/// Upstream rsync uses `popt` which handles both standalone `-s` and bundled
/// short options like `-logDtprs`. We must detect both forms since protocol
/// 28/29 clients (rsync 3.0.x, 3.1.x) commonly bundle `-s` into compact
/// flag strings.
///
/// The `-e` option consumes the rest of the string as its parameter (the
/// capability string, e.g. `.iLsfxCIvu`). Characters after `e` must NOT be
/// treated as flags - the `s` in `sfxCIvu` is SYMLINK_ICONV, not secluded-args.
///
/// upstream: options.c:792 - `{secluded-args, 's', POPT_ARG_VAL, &protect_args, 1}`
fn has_secluded_args_flag(args: &[String]) -> bool {
    args.iter().any(|a| {
        if a == "-s" || a == "--protect-args" || a == "--secluded-args" {
            return true;
        }
        // Check for `-s` bundled in compact flag strings like `-logDtprs`.
        // A compact flag string starts with `-` but not `--`, and contains
        // single-character flags. Stop scanning at `e` since `-e` consumes
        // the remainder as its parameter (the capability string).
        // upstream: options.c uses popt which knows `-e` takes an argument.
        if let Some(rest) = a.strip_prefix('-') {
            if !rest.starts_with('-') && rest.len() > 1 {
                for ch in rest.chars() {
                    if ch == 'e' {
                        // `-e` consumes the rest as its argument
                        return false;
                    }
                    if ch == 's' {
                        return true;
                    }
                }
            }
        }
        false
    })
}

/// Reads and logs client arguments, handling the two-phase secluded-args
/// protocol when the client sends `--protect-args` / `-s`.
///
/// Phase 1: read the standard null/newline-terminated argument list.
/// If phase-1 args contain `-s`, proceed to phase 2.
/// Phase 2: read the full argument list via `recv_secluded_args()`.
///
/// Returns the effective client arguments on success, or sends an error
/// and returns `None`.
///
/// # Upstream Reference
///
/// - `clientserver.c:1059-1073` - two-phase `read_args()` for protect_args
fn read_and_log_client_args(
    ctx: &mut ModuleRequestContext<'_>,
    negotiated_protocol: Option<ProtocolVersion>,
) -> io::Result<Option<Vec<String>>> {
    let phase1_args = match read_client_arguments(ctx.reader, negotiated_protocol) {
        Ok(args) => args,
        Err(err) => {
            let payload = format!("@ERROR: failed to read client arguments: {err}");
            send_error_and_exit(ctx.reader.get_mut(), ctx.limiter, ctx.messages, &payload)?;
            return Ok(None);
        }
    };

    // Detect secluded-args flag in phase-1 args.
    // upstream: clientserver.c:1066 - if (protect_args && ret)
    // upstream: options.c:792 - `-s` is a short option for `--secluded-args`
    // Protocol 28/29 clients may bundle `-s` into compact flag strings like `-logDtprs`.
    let has_secluded = has_secluded_args_flag(&phase1_args);

    let client_args = if has_secluded {
        // Phase 2: read the real args via secluded-args wire format.
        // upstream: clientserver.c:1068-1071 - read_args with rl_nulls=1
        match protocol::secluded_args::recv_secluded_args(ctx.reader, None) {
            Ok(full_args) => {
                // First element is "rsync" (set by upstream send_protected_args),
                // skip it to get the actual server arguments.
                if full_args.first().is_some_and(|a| a == "rsync") {
                    full_args.into_iter().skip(1).collect()
                } else {
                    full_args
                }
            }
            Err(err) => {
                let payload = format!("@ERROR: failed to read secluded args: {err}");
                send_error_and_exit(ctx.reader.get_mut(), ctx.limiter, ctx.messages, &payload)?;
                return Ok(None);
            }
        }
    } else {
        // upstream: clientserver.c:1073 - first `read_args()` call passes
        // `unescape=1` so option args that the client escaped with
        // `safe_arg()` are restored before parsing. File args after the `.`
        // separator are NOT unescaped here because upstream funnels them
        // through `glob_expand()` which handles shell metacharacters itself.
        unescape_phase1_option_args(phase1_args)
    };

    if let Some(log) = ctx.log_sink {
        let args_str = client_args.join(" ");
        let text = format!(
            "module '{}' from {} ({}): client args: {}",
            ctx.request,
            ctx.effective_host().unwrap_or("unknown"),
            ctx.peer_ip,
            args_str
        );
        let message = rsync_info!(text).with_role(Role::Daemon);
        log_message(log, &message);
    }

    Ok(Some(client_args))
}

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

/// Extracts the positional path arguments sent by the client after the `.`
/// separator and strips the leading module-name component from each so the
/// receiver can resolve them relative to the on-disk module path.
///
/// Mirrors upstream `read_args()` (io.c:1295) and `glob_expand_module()`
/// (util1.c:804): everything before a standalone `.` in the wire arg list is
/// options/flags; everything after is the client's positional paths. Each
/// positional begins with the module name (e.g. `upload/realdir/` when the
/// module is `upload`), which is the prefix `glob_expand_module()` strips
/// before the path is handed to the server-side option parser.
///
/// Returns the stripped relative paths in original order. A path that does
/// not start with the module name is returned as-is so the caller can still
/// see it (this matches upstream's loose prefix match - it only strips when
/// the prefix is present).
fn extract_module_relative_paths(client_args: &[String], module_name: &str) -> Vec<String> {
    let mut dot_seen = false;
    let mut out = Vec::new();
    for arg in client_args {
        if !dot_seen {
            if arg == "." {
                dot_seen = true;
            }
            continue;
        }
        // upstream: util1.c:813-814 - `if (strncmp(arg, base, base_len) == 0)
        // arg += base_len;` - strips the bare module name. The remainder may
        // be empty (then represents the module root), start with `/`
        // (subpath), or be the rest of a longer arg sharing the prefix.
        let stripped = if let Some(rest) = arg.strip_prefix(module_name) {
            // Only strip when the next char is `/` or end-of-string so we do
            // not chop the prefix of a sibling module that merely shares a
            // string prefix (e.g. `uploads/` vs module `upload`).
            if rest.is_empty() || rest.starts_with('/') {
                rest.trim_start_matches('/').to_owned()
            } else {
                arg.clone()
            }
        } else {
            arg.clone()
        };
        out.push(stripped);
    }
    out
}

/// Resolves the receiver's on-disk destination directory from the client's
/// positional path args.
///
/// Mirrors the post-`change_dir(module_chdir)` behaviour upstream relies on:
/// after upstream's `glob_expand_module()` strips the module name, the
/// receiver's `get_local_name()` (main.c:697) interprets the remaining path
/// as relative to the module root on disk. Because oc-rsync does not chdir
/// per connection, we resolve that join explicitly.
///
/// Returns the module path itself when no positional was supplied or when
/// the stripped tail is empty (push directly into the module root).
fn resolve_receiver_dest(
    module_path: &std::path::Path,
    client_args: &[String],
    module_name: &str,
) -> std::path::PathBuf {
    let positionals = extract_module_relative_paths(client_args, module_name);
    // upstream: main.c:1203-1204 - `local_name = get_local_name(flist, argv[0])`
    // uses the FIRST remaining positional (after the `.` placeholder has been
    // consumed by `do_server_recv` at line 1166). For a receiver that
    // translates to the last wire positional - the destination.
    let Some(last) = positionals.last() else {
        return module_path.to_path_buf();
    };
    let tail = last.trim();
    if tail.is_empty() || tail == "." {
        return module_path.to_path_buf();
    }
    // Defensive: a path arriving here that is absolute on the host OS
    // (Unix `is_absolute()` or a leading `/` that Windows treats as
    // drive-relative) cannot be allowed to escape the module root. Strip
    // the leading separators and join the remainder so the destination is
    // always under `module_path`. Tests cover both forms cross-platform.
    let rel = std::path::Path::new(tail);
    if rel.is_absolute() || tail.starts_with('/') || tail.starts_with('\\') {
        let stripped = tail.trim_start_matches(['/', '\\']);
        return module_path.join(stripped);
    }
    module_path.join(rel)
}

/// Resolves the sender's on-disk source paths from the client's positional
/// path args for a pull request (Generator role).
///
/// Mirrors upstream's `glob_expand_module()` + `chdir(module_chdir)` ordering:
/// once the module name has been stripped, upstream's daemon-mode sender sees
/// argv positionals as paths relative to the module root, and the sender's
/// per-arg `dir/fn` split (flist.c:2338-2349) chops the last `/` so the wire
/// emits `fn` as the file-list name. We don't chdir, so each positional is
/// resolved by joining the stripped tail with `module_path`. The trailing
/// slash (if any) is preserved so the sender's existing dotdir branch can
/// trigger when the client wrote `module/sub/` instead of `module/sub`.
///
/// Returns `[module_path]` when no positional was supplied or when every
/// stripped tail is empty, matching the pre-existing "pull from module root"
/// behaviour exactly.
///
/// Sub-paths that contain `..` segments or that resolve to a host-absolute
/// path are rejected (returns `None`) so a malicious client cannot enumerate
/// files outside the module root via a crafted `rsync://host/mod/../etc/...`
/// URL. This is defense-in-depth on top of the chroot / Landlock sandbox.
///
/// # Upstream Reference
///
/// - `util1.c:804 glob_expand_module()` - strips the module name from each arg
/// - `clientserver.c:992 change_dir(module_chdir, CD_NORMAL)` - relativises args
/// - `flist.c:2338-2349 send_file_list()` - `dir/fn` split per positional
fn resolve_sender_sources(
    module_path: &std::path::Path,
    client_args: &[String],
    module_name: &str,
) -> Option<Vec<std::path::PathBuf>> {
    let positionals = extract_module_relative_paths(client_args, module_name);
    if positionals.is_empty() {
        return Some(vec![module_path.to_path_buf()]);
    }
    let mut sources = Vec::with_capacity(positionals.len());
    let mut all_empty = true;
    for raw in &positionals {
        let tail = raw.trim();
        if tail.is_empty() || tail == "." {
            sources.push(module_path.to_path_buf());
            continue;
        }
        all_empty = false;
        // Reject `..` traversal segments so the joined path cannot escape the
        // module root. Upstream rsync's sender-side `sanitize_path()` and the
        // chroot wall it lives behind cover this; oc-rsync mirrors the check
        // explicitly because the daemon does not always chroot.
        let trimmed = tail.trim_start_matches(['/', '\\']);
        for component in std::path::Path::new(trimmed).components() {
            if matches!(component, std::path::Component::ParentDir) {
                return None;
            }
        }
        // Preserve the trailing slash so the sender can detect a dotdir-style
        // source (upstream flist.c:2312-2322 appends `.` and sets DOTDIR_NAME
        // for any `fbuf[len-1] == '/'`). Upstream rsync joins module-relative
        // paths with a literal `/` regardless of host OS (util1.c pathjoin()),
        // so build the result the same way instead of going through
        // PathBuf::join, which on Windows inserts `\` and on macOS leaves
        // a trailing `/` that doubles when we re-append.
        let trailing = tail.ends_with('/') || tail.ends_with('\\');
        let mut buf = module_path.as_os_str().to_owned();
        let needs_leading_sep = !buf
            .as_encoded_bytes()
            .last()
            .is_some_and(|b| *b == b'/' || *b == b'\\');
        if needs_leading_sep {
            buf.push("/");
        }
        buf.push(trimmed);
        if trailing
            && !buf
                .as_encoded_bytes()
                .last()
                .is_some_and(|b| *b == b'/' || *b == b'\\')
        {
            buf.push("/");
        }
        sources.push(std::path::PathBuf::from(buf));
    }
    if all_empty {
        Some(vec![module_path.to_path_buf()])
    } else {
        Some(sources)
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

    // upstream: clientserver.c - clamp verbose to lp_max_verbosity(i)
    let flag_string = clamp_verbose_flags(&flag_string, module.max_verbosity);

    // upstream: main.c:1203-1204 + util1.c:804 (glob_expand_module) - receivers
    // resolve their destination by joining the module path with the client's
    // module-relative tail (e.g. `upload/realdir/` -> module + `realdir/`).
    // Senders (pull requests) split each positional the same way so the
    // sender's per-source `dir/fn` (flist.c:2338-2349) walks the requested
    // sub-tree instead of the entire module root. The original argv[0] is
    // always the module root; legacy tests that push straight into the module
    // root keep that behaviour.
    let positional_args: Vec<OsString> = if role == ServerRole::Receiver {
        let dest = resolve_receiver_dest(std::path::Path::new(&module.path), client_args, &module.name);
        vec![OsString::from(dest.as_os_str())]
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

            // upstream: after chroot + chdir, reference directory paths resolve
            // relative to the module root. We don't chdir, so resolve relative
            // paths explicitly against the module path.
            let module_path = std::path::Path::new(&module.path);
            for ref_dir in &mut cfg.reference_directories {
                if ref_dir.path.is_relative() {
                    ref_dir.path = module_path.join(&ref_dir.path);
                }
            }

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

/// Applies long-form arguments from the client to the server configuration.
///
/// Upstream rsync's `server_options()` (options.c:2737-2980) sends many options
/// as long-form arguments that are not encoded in the compact flag string.
/// The daemon must parse these to correctly configure the transfer.
///
/// Returns `Some(offender)` when a client-only flag (write/read-batch family)
/// reaches the daemon. The caller surfaces that as an `@ERROR` and exits
/// instead of letting the unrecognised option drive a silent connection
/// close mid file-list framing. Upstream mirrors this at
/// `options.c:1444-1449` with `rsync: <BAD>: <err> (in daemon mode)` then
/// `daemon_error:` (`options.c:1464-1466`) exiting `RERR_SYNTAX`.
///
/// # Upstream Reference
///
/// - `options.c:1444-1449` - daemon-mode unknown option error path
/// - `options.c:2818-2829` - delete mode variants
/// - `options.c:2836-2837` - `--size-only`
/// - `options.c:2878-2879` - `--ignore-errors`
/// - `options.c:2888` - `--numeric-ids`
/// - `options.c:2891` - `--use-qsort`
/// - `options.c:2737-2740` - `--compress-level=N`
fn apply_long_form_args(client_args: &[String], config: &mut ServerConfig) -> Option<String> {
    // Positional path args follow the standalone `.` separator. Upstream
    // `glob_expand_module()` consumes them through a different code path, so
    // the daemon's option parser only validates the option region.
    let dot_position = client_args.iter().position(|a| a == ".");

    let mut unknown: Option<String> = None;
    let mut i = 0;
    while i < client_args.len() {
        let arg = &client_args[i];
        if dot_position.is_some_and(|dot| i >= dot) {
            i += 1;
            continue;
        }
        match arg.as_str() {
            // upstream: options.c:2818-2829 - delete mode variants
            "--delete" | "--delete-before" | "--delete-during" => {
                config.flags.delete = true;
            }
            "--delete-after" | "--delete-delay" => {
                config.flags.delete = true;
                config.deletion.late_delete = true;
            }
            "--delete-excluded" => {
                config.flags.delete = true;
            }
            // upstream: options.c:2838-2839 - --stats sets do_stats which causes
            // INFO_STATS to level 2+. Without this flag, the generator does not
            // emit NDX_DEL_STATS during the goodbye phase and the client sender's
            // "Number of deleted files" line stays at zero on daemon uploads.
            "--stats" => {
                config.do_stats = true;
            }
            // upstream: options.c:2836-2837
            "--size-only" => {
                config.file_selection.size_only = true;
            }
            // upstream: options.c:2878-2879
            "--ignore-errors" => {
                config.deletion.ignore_errors = true;
            }
            // upstream: options.c:2881-2882
            "--copy-unsafe-links" => {
                config.flags.copy_unsafe_links = true;
            }
            // upstream: options.c:2884-2885
            "--safe-links" => {
                config.flags.safe_links = true;
            }
            // upstream: options.c:2887-2888
            "--numeric-ids" => {
                config.flags.numeric_ids = true;
            }
            // upstream: options.c:2890-2891
            "--use-qsort" => {
                config.qsort = true;
            }
            // upstream: options.c:2893-2897
            "--ignore-existing" => {
                config.file_selection.ignore_existing = true;
            }
            // upstream: options.c:2899-2900
            "--existing" => {
                config.file_selection.existing_only = true;
            }
            // upstream: options.c:817-818
            "--ignore-missing-args" => {
                config.file_selection.ignore_missing_args = true;
            }
            "--delete-missing-args" => {
                config.file_selection.delete_missing_args = true;
            }
            // upstream: options.c:2933-2942
            "--inplace" => {
                config.write.inplace = true;
            }
            "--append" => {
                config.flags.append = true;
            }
            // upstream: options.c:2934-2935
            "--delay-updates" => {
                config.write.delay_updates = true;
            }
            // upstream: options.c:2964-2965
            "--fsync" => {
                config.write.fsync = true;
            }
            // upstream: options.c:2849 - backup
            "--backup" => {
                config.flags.backup = true;
            }
            // Two-arg options: upstream sends option and value as separate args.
            // upstream: options.c:2915-2923 - reference directories
            "--compare-dest" => {
                if let Some(dir) = client_args.get(i + 1) {
                    config.reference_directories.push(ReferenceDirectory {
                        path: std::path::PathBuf::from(dir),
                        kind: ReferenceDirectoryKind::Compare,
                    });
                    i += 1;
                }
            }
            "--copy-dest" => {
                if let Some(dir) = client_args.get(i + 1) {
                    config.reference_directories.push(ReferenceDirectory {
                        path: std::path::PathBuf::from(dir),
                        kind: ReferenceDirectoryKind::Copy,
                    });
                    i += 1;
                }
            }
            "--link-dest" => {
                if let Some(dir) = client_args.get(i + 1) {
                    config.reference_directories.push(ReferenceDirectory {
                        path: std::path::PathBuf::from(dir),
                        kind: ReferenceDirectoryKind::Link,
                    });
                    i += 1;
                }
            }
            // upstream: options.c:2787-2790 - backup-dir as separate args
            "--backup-dir" => {
                config.flags.backup = true;
                if let Some(dir) = client_args.get(i + 1) {
                    config.backup_dir = Some(dir.to_owned());
                    i += 1;
                }
            }
            // upstream: options.c:2791-2793 - suffix as separate args
            // When --backup-dir is specified without explicit --suffix,
            // upstream changes the default suffix from "~" to "" and sends
            // --suffix as a two-arg form (not --suffix=VALUE).
            "--suffix" | "--backup-suffix" => {
                if let Some(suffix) = client_args.get(i + 1) {
                    config.backup_suffix = Some(suffix.to_owned());
                    i += 1;
                }
            }
            // upstream: options.c:2907-2909 - temp-dir as separate args
            "--temp-dir" => {
                if let Some(dir) = client_args.get(i + 1) {
                    config.temp_dir = Some(std::path::PathBuf::from(dir));
                    i += 1;
                }
            }
            // upstream: options.c:2800-2805 - --compress-choice, --new-compress, --old-compress
            "--new-compress" => {
                config.flags.compress = true;
                if config.connection.compression_level.is_none() {
                    config.connection.compression_level =
                        Some(compress::zlib::CompressionLevel::Default);
                }
            }
            "--old-compress" => {
                config.flags.compress = true;
                if config.connection.compression_level.is_none() {
                    config.connection.compression_level =
                        Some(compress::zlib::CompressionLevel::Default);
                }
            }
            _ => {
                // upstream: options.c:2800-2805 - --compress-choice=ALGO
                if let Some(_choice) = arg
                    .strip_prefix("--compress-choice=")
                    .or_else(|| arg.strip_prefix("--zc="))
                {
                    // Mark compression as active. The actual algorithm is parsed
                    // later from client_args in run_server_with_handshake().
                    config.flags.compress = true;
                    if config.connection.compression_level.is_none() {
                        config.connection.compression_level =
                            Some(compress::zlib::CompressionLevel::Default);
                    }
                // upstream: options.c:2737-2740
                } else if let Some(level_str) = arg.strip_prefix("--compress-level=") {
                    if let Ok(level) = level_str.parse::<u32>() {
                        if let Ok(cl) = compress::zlib::CompressionLevel::from_numeric(level) {
                            config.connection.compression_level = Some(cl);
                        }
                    }
                // upstream: options.c:2807-2810
                } else if let Some(val) = arg.strip_prefix("--max-delete=") {
                    if let Ok(n) = val.parse::<i64>() {
                        if n >= 0 {
                            config.deletion.max_delete = Some(n as u64);
                        }
                    }
                // Fallback: =value format for reference directories and backup options.
                // Handles both upstream (two-arg) and legacy (=value) formats.
                } else if let Some(dir) = arg.strip_prefix("--backup-dir=") {
                    config.flags.backup = true;
                    config.backup_dir = Some(dir.to_owned());
                } else if let Some(suffix) = arg.strip_prefix("--suffix=") {
                    config.backup_suffix = Some(suffix.to_owned());
                } else if let Some(suffix) = arg.strip_prefix("--backup-suffix=") {
                    config.backup_suffix = Some(suffix.to_owned());
                } else if let Some(dir) = arg.strip_prefix("--link-dest=") {
                    config.reference_directories.push(ReferenceDirectory {
                        path: std::path::PathBuf::from(dir),
                        kind: ReferenceDirectoryKind::Link,
                    });
                } else if let Some(dir) = arg.strip_prefix("--compare-dest=") {
                    config.reference_directories.push(ReferenceDirectory {
                        path: std::path::PathBuf::from(dir),
                        kind: ReferenceDirectoryKind::Compare,
                    });
                } else if let Some(dir) = arg.strip_prefix("--copy-dest=") {
                    config.reference_directories.push(ReferenceDirectory {
                        path: std::path::PathBuf::from(dir),
                        kind: ReferenceDirectoryKind::Copy,
                    });
                } else if let Some(dir) = arg.strip_prefix("--temp-dir=") {
                    config.temp_dir = Some(std::path::PathBuf::from(dir));
                } else if let Some(path) = arg.strip_prefix("--files-from=") {
                    config.file_selection.files_from_path = Some(path.to_owned());
                } else if arg == "--from0" {
                    // upstream: options.c:940 - --from0 sets NUL-delimited mode
                    // for --files-from content read from the protocol stream.
                    config.file_selection.from0 = true;
                // upstream: options.c:773,963 - --log-format is the deprecated
                // alias for --out-format. The server parses it to set
                // stdout_format_has_i (options.c:2327-2331) which drives itemize
                // emission. We only need the %i presence, not the full format.
                } else if let Some(fmt) = arg
                    .strip_prefix("--log-format=")
                    .or_else(|| arg.strip_prefix("--out-format="))
                {
                    if fmt.contains("%i") {
                        config.flags.info_flags.itemize = true;
                    }
                } else if unknown.is_none() && is_client_only_flag_reaching_daemon(arg) {
                    // upstream: options.c:1444-1449 - the daemon's popt loop
                    // emits `rsync: <BAD>: <err> (in daemon mode)` on the
                    // first unrecognised option and jumps to `daemon_error:`
                    // (options.c:1464-1466), exiting `RERR_SYNTAX`. We mirror
                    // that fail-loud surface for batch-family flags that the
                    // client-side sanitiser should have stripped. Catching
                    // them here converts the previously silent connection
                    // close at protocol byte ~2241725 into an explicit
                    // `@ERROR` frame plus non-zero exit.
                    unknown = Some(arg.clone());
                }
            }
        }
        i += 1;
    }

    unknown
}

/// Reports whether `arg` is a client-only flag that should never reach the
/// daemon.
///
/// `--write-batch`, `--only-write-batch`, and `--read-batch` set up local
/// batch-file recording or replay on the CLIENT side only. Upstream
/// `options.c:server_options()` deliberately omits them from the argv sent
/// to the server; the only related token upstream emits is the literal
/// `--only-write-batch=X` placeholder at `options.c:2832-2833`, which
/// carries no real path. Encountering one here means the client-side
/// sanitiser failed - the previous behaviour was a silent connection close
/// in the middle of file-list framing. Surface this as a Rule-12 fail-loud
/// `@ERROR` instead.
///
/// Both bare-flag (`--write-batch`) and key=value (`--write-batch=PATH`)
/// forms are detected.
///
/// # Upstream Reference
///
/// - `options.c:784-786` - `read-batch`, `write-batch`, `only-write-batch`
///   popt entries (client-only)
/// - `options.c:1444-1449` - daemon-mode unknown option error path
fn is_client_only_flag_reaching_daemon(arg: &str) -> bool {
    let bare = arg.split('=').next().unwrap_or(arg);
    matches!(bare, "--write-batch" | "--only-write-batch" | "--read-batch")
}

/// Resolves the daemon module's `charset =` directive into a
/// [`FilenameConverter`] that mirrors upstream rsync's iconv setup.
///
/// Upstream `clientserver.c:712-716` sets `iconv_opt = lp_charset(i)` and
/// calls `setup_iconv()` whenever the value is non-empty. `setup_iconv()`
/// (`rsync.c:87-140`) then opens two iconv handles:
///
/// - `ic_send = iconv_open(UTF8, charset)` - convert local-charset bytes to
///   UTF-8 wire bytes when sending file lists.
/// - `ic_recv = iconv_open(charset, UTF8)` - convert UTF-8 wire bytes back
///   to the local charset when receiving file lists.
///
/// When the directive includes a comma (`charset = LOCAL,REMOTE`), upstream
/// honours the server side by using the segment after the comma
/// (`rsync.c:118-120`, `am_server` branch). When no comma is present, the
/// whole value is the charset. The literal value `.` and the empty string
/// both resolve to the locale default per `rsync.c:125-126`.
///
/// Our [`FilenameConverter`] models the same direction pair: `local_to_remote`
/// matches `ic_send` (local -> wire UTF-8), and `remote_to_local` matches
/// `ic_recv` (wire UTF-8 -> local). Therefore a daemon-side converter is
/// built with the daemon's local charset on the local side and the literal
/// `"UTF-8"` on the remote (wire) side.
///
/// Returns `None` when the directive is absent, empty, or unrecognised by
/// `encoding_rs`. An unrecognised charset is treated as a soft failure: we
/// log via `tracing` (when enabled) and fall through to identity conversion,
/// matching the lenient behaviour `IconvSetting::resolve_converter` already
/// uses on the client side.
///
/// # Upstream Reference
///
/// - `clientserver.c:712-716` - `iconv_opt = lp_charset(i); setup_iconv();`
/// - `rsync.c:87-140` - `setup_iconv()` opens `ic_send` and `ic_recv`.
/// - `loadparm.c` - `charset` module parameter.
fn resolve_module_charset_converter(charset: Option<&str>) -> Option<FilenameConverter> {
    let raw = charset?.trim();
    if raw.is_empty() {
        return None;
    }

    // upstream: rsync.c:118-120 - on the server side, the segment after the
    // comma is the effective local charset; the segment before the comma
    // describes the peer's local charset and is irrelevant to the daemon.
    let local_part = match raw.split_once(',') {
        Some((_, remote)) => remote.trim(),
        None => raw,
    };

    // upstream: rsync.c:125-126 - empty or "." means "use locale default".
    // Our converter treats UTF-8 as the locale default, matching
    // `converter_from_locale`.
    if local_part.is_empty() || local_part == "." {
        return Some(FilenameConverter::identity());
    }

    match FilenameConverter::new(local_part, "UTF-8") {
        Ok(converter) => Some(converter),
        Err(_error) => {
            #[cfg(feature = "tracing")]
            tracing::warn!(
                charset = %local_part,
                error = %_error,
                "module 'charset' directive: unsupported encoding; daemon will not transcode filenames",
            );
            None
        }
    }
}

/// Parses the module's `incoming chmod` / `outgoing chmod` directives into the
/// typed [`metadata::ChmodModifiers`] used by the receiver and sender code
/// paths. Returns a human-readable error message when a spec is malformed so
/// the caller can wrap it in an `@ERROR` reply.
///
/// # Upstream Reference
///
/// - `options.c:parse_chmod()` - canonical chmod-spec grammar
/// - `clientserver.c:rsync_module()` - daemon-side arming of `daemon_chmod_modes`
fn parse_daemon_chmod_specs(
    module: &ModuleRuntime,
) -> Result<
    (
        Option<metadata::ChmodModifiers>,
        Option<metadata::ChmodModifiers>,
    ),
    String,
> {
    let incoming = parse_one_chmod_spec("incoming chmod", module.incoming_chmod.as_deref())?;
    let outgoing = parse_one_chmod_spec("outgoing chmod", module.outgoing_chmod.as_deref())?;
    Ok((incoming, outgoing))
}

fn parse_one_chmod_spec(
    directive: &'static str,
    spec: Option<&str>,
) -> Result<Option<metadata::ChmodModifiers>, String> {
    match spec {
        Some(text) => metadata::ChmodModifiers::parse(text)
            .map(Some)
            .map_err(|err| format!("invalid '{directive}' directive '{text}': {err}")),
        None => Ok(None),
    }
}

#[cfg(test)]
mod daemon_chmod_spec_tests {
    use super::parse_one_chmod_spec;

    #[test]
    fn parse_one_chmod_spec_returns_none_for_unset_directive() {
        let result = parse_one_chmod_spec("incoming chmod", None).expect("ok");
        assert!(result.is_none());
    }

    #[test]
    fn parse_one_chmod_spec_accepts_numeric_class_action_form() {
        // upstream parse_chmod() accepts bare octal (`644`), prefix-letter
        // (`F600`), class-action-perms (`u+x`), and combined forms.
        for spec in ["644", "F600", "u+x", "Du+x,Fg-r,Fo-r"] {
            let parsed = parse_one_chmod_spec("incoming chmod", Some(spec))
                .unwrap_or_else(|err| panic!("spec '{spec}' must parse: {err}"));
            assert!(parsed.is_some(), "spec '{spec}' produced no clauses");
        }
    }

    #[test]
    fn parse_one_chmod_spec_surfaces_directive_name_on_error() {
        let err = parse_one_chmod_spec("outgoing chmod", Some("bogus"))
            .expect_err("malformed spec must error");
        assert!(
            err.contains("outgoing chmod"),
            "error '{err}' must name the offending directive",
        );
        assert!(
            err.contains("bogus"),
            "error '{err}' must include the offending spec text",
        );
    }
}

#[cfg(test)]
mod iconv_charset_converter_tests {
    use super::resolve_module_charset_converter;

    #[test]
    fn iconv_charset_returns_none_for_missing_directive() {
        assert!(resolve_module_charset_converter(None).is_none());
    }

    #[test]
    fn iconv_charset_returns_none_for_empty_directive() {
        assert!(resolve_module_charset_converter(Some("")).is_none());
        assert!(resolve_module_charset_converter(Some("   ")).is_none());
    }

    #[test]
    fn iconv_charset_dot_means_locale_default() {
        let converter = resolve_module_charset_converter(Some(".")).expect("dot should resolve");
        assert!(converter.is_identity());
    }

    #[test]
    fn iconv_charset_comma_with_dot_resolves_to_identity() {
        // upstream: rsync.c:118-120 - server side honours the post-comma value.
        // upstream: rsync.c:125-126 - "." means "use locale default".
        let converter =
            resolve_module_charset_converter(Some("UTF-8,.")).expect("dot remote should resolve");
        assert!(converter.is_identity());
    }

    #[cfg(feature = "iconv")]
    #[test]
    fn iconv_charset_resolves_simple_charset() {
        let converter =
            resolve_module_charset_converter(Some("ISO-8859-1")).expect("charset should resolve");
        // encoding_rs aliases ISO-8859-1 to windows-1252 internally.
        assert_eq!(converter.local_encoding_name(), "windows-1252");
        assert_eq!(converter.remote_encoding_name(), "UTF-8");
    }

    #[cfg(feature = "iconv")]
    #[test]
    fn iconv_charset_uses_segment_after_comma() {
        // upstream: rsync.c:118-120 - server side honours the post-comma value.
        let converter = resolve_module_charset_converter(Some("UTF-8,ISO-8859-1"))
            .expect("charset should resolve");
        assert_eq!(converter.local_encoding_name(), "windows-1252");
        assert_eq!(converter.remote_encoding_name(), "UTF-8");
    }

    #[cfg(feature = "iconv")]
    #[test]
    fn iconv_charset_returns_none_for_unknown_charset() {
        assert!(resolve_module_charset_converter(Some("not-a-real-charset")).is_none());
    }

    #[cfg(feature = "iconv")]
    #[test]
    fn iconv_charset_trims_whitespace() {
        let converter = resolve_module_charset_converter(Some("  ISO-8859-1  "))
            .expect("trimmed charset should resolve");
        assert_eq!(converter.local_encoding_name(), "windows-1252");
    }

    #[cfg(feature = "iconv")]
    #[test]
    fn iconv_charset_round_trip_latin1_utf8() {
        // Verify the converter actually transcodes correctly: a Latin-1 byte
        // sequence containing U+00E9 ('é' as 0xE9) should round-trip through
        // UTF-8 wire encoding and back.
        let converter =
            resolve_module_charset_converter(Some("ISO-8859-1")).expect("charset should resolve");

        let local_bytes = b"caf\xe9.txt"; // 'café.txt' in Latin-1
        let wire = converter
            .local_to_remote(local_bytes)
            .expect("local_to_remote");
        assert_eq!(wire.as_ref(), "café.txt".as_bytes());

        let round_trip = converter.remote_to_local(&wire).expect("remote_to_local");
        assert_eq!(round_trip.as_ref(), local_bytes);
    }
}
