// Wire-level reading of the client argument list sent after `@RSYNCD: OK`,
// including the two-phase secluded-args protocol and backslash-unescaping.
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

/// Merges secluded-args phase 1 (cmdline) and phase 2 (stdin) arg lists.
///
/// In secluded-args mode upstream rsync splits the daemon argv at the
/// `NULL` it injects at `options.c:2745`. Phase 1 (read from the cmdline,
/// `clientserver.c:395-407`) carries the args that precede that NULL:
/// `--server`, `--sender`, the compact flag string built by `argstr`
/// (`options.c:2620-2731`), and `--iconv=...` if any. Phase 2 (read from
/// stdin via `send_protected_args`, `rsync.c:283-320`) carries every arg
/// that follows, beginning with the synthetic `"rsync"` arg0
/// (`rsync.c:295`) and ending with the `.` separator plus the positional
/// paths.
///
/// Upstream feeds each phase through `parse_arguments()` in turn
/// (`clientserver.c:1079, 1085`) so the popt globals carry the union
/// forward. We do not maintain popt globals, so the equivalent is to
/// concatenate the two phases and re-run our single-pass option parser
/// on the result. Without the prepend, the daemon never sees the compact
/// flag string or `--sender` from phase 1, and the wildcard `--groupmap`
/// value never reaches the receiver alongside `-z` / `-r` / `-l`,
/// breaking the `daemon-groupmap-wild` test under secluded-args mode
/// (upstream issue #829).
///
/// The synthetic `"rsync"` arg0 from phase 2 is dropped before the merge
/// so the daemon's parser does not treat it as a positional path.
///
/// # Upstream Reference
///
/// - `clientserver.c:1059-1086` - two-phase `read_args()` + `parse_arguments()`
/// - `options.c:2614-2745` - `server_options()` placement of `--server`,
///   `--sender`, `argstr`, and the secluded-args NULL split point
/// - `rsync.c:283-320` - `send_protected_args()` rewrites the NULL slot
///   with `"rsync"` and streams the rest as NUL-separated bytes
fn merge_secluded_args(phase1: Vec<String>, mut phase2: Vec<String>) -> Vec<String> {
    if phase2.first().is_some_and(|a| a == "rsync") {
        phase2.remove(0);
    }
    let mut merged = Vec::with_capacity(phase1.len() + phase2.len());
    merged.extend(phase1);
    merged.extend(phase2);
    merged
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
/// upstream: options.c:804 - `{secluded-args, 's', POPT_ARG_VAL, &protect_args, 1}`
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
/// - `clientserver.c:1073-1087` - two-phase `read_args()` for protect_args
fn read_and_log_client_args(
    ctx: &mut ModuleRequestContext<'_>,
    negotiated_protocol: Option<ProtocolVersion>,
) -> io::Result<Option<Vec<String>>> {
    let phase1_args = match read_client_arguments(ctx.reader, negotiated_protocol) {
        Ok(args) => args,
        Err(err) => {
            let payload = format!("@ERROR: failed to read client arguments: {err}");
            send_error(ctx.reader.get_mut(), ctx.limiter, &payload)?;
            return Ok(None);
        }
    };

    // Detect secluded-args flag in phase-1 args.
    // upstream: clientserver.c:1080 - if (protect_args && ret)
    // upstream: options.c:804 - `-s` is a short option for `--secluded-args`
    // Protocol 28/29 clients may bundle `-s` into compact flag strings like `-logDtprs`.
    let has_secluded = has_secluded_args_flag(&phase1_args);

    let client_args = if has_secluded {
        // Phase 2: read the real args via secluded-args wire format.
        // upstream: clientserver.c:1082-1085 - read_args with rl_nulls=1.
        //
        // The two phases carry disjoint slices of the original argv:
        // - Phase 1 (cmdline): `--server`, `--sender`, the compact flag
        //   string (`-slogDtprIzxe.iLsfxCIvu`), and `--iconv=...` if any.
        //   These appear before the `NULL` upstream inserts at
        //   `options.c:2745` and must reach the daemon's option parser
        //   or the negotiated compact flags (`-r`, `-l`, `-z`, ...) and
        //   the role marker `--sender` are silently dropped.
        // - Phase 2 (stdin): every long-form option emitted after that
        //   NULL (`--list-only`, `--log-format`, `--usermap`, `--groupmap`,
        //   ...), the `.` separator, and the positional paths.
        //
        // Upstream feeds each phase through `parse_arguments()` in turn
        // (`clientserver.c:1079, 1085`) so the popt globals carry the
        // union forward. We mirror that by concatenating phase 1 ahead
        // of phase 2 so a single pass over the merged list sees both the
        // compact flag string and the long options that fix the
        // `daemon-groupmap-wild` test (`--groupmap=*:GID`).
        //
        // Phase 2 args are emitted verbatim by upstream `safe_arg()` -
        // the `if (!protect_args ...)` guard at `options.c:2551` skips
        // the WILD_CHARS escape when `protect_args` is set - so no
        // `unbackslash_arg()` pass is needed on either phase.
        match protocol::secluded_args::recv_secluded_args(ctx.reader, None) {
            Ok(full_args) => merge_secluded_args(phase1_args, full_args),
            Err(err) => {
                let payload = format!("@ERROR: failed to read secluded args: {err}");
                send_error(ctx.reader.get_mut(), ctx.limiter, &payload)?;
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
