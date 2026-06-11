//! Daemon argument building for client-to-server communication.
//!
//! Builds the daemon argument list mirroring upstream `server_options()` in
//! `options.c`. Supports both single-phase (plain) and two-phase (protect-args)
//! argument exchange protocols.

use std::io::Write;

use protocol::ProtocolVersion;
use transfer::setup::build_capability_string_suffix;

use crate::client::config::{ClientConfig, DeleteMode, IconvSetting, ReferenceDirectoryKind};
use crate::client::error::{ClientError, socket_error};
use crate::client::remote::daemon_transfer::connection::DaemonTransferRequest;
use crate::client::remote::flags;

/// Sends daemon-mode arguments to the server.
///
/// When `--protect-args` / `-s` is active, uses a two-phase protocol
/// matching upstream `clientserver.c:393-408`:
/// - Phase 1: minimal args (`--server [-s] .`) so the daemon knows to
///   expect protected args
/// - Phase 2: full argument list via `send_secluded_args()` wire format
///
/// Without protect-args, sends all arguments in a single phase.
/// For protocol >= 30, strings are null-terminated; for < 30, newline-terminated.
pub(crate) fn send_daemon_arguments<W: Write>(
    stream: &mut W,
    config: &ClientConfig,
    request: &DaemonTransferRequest,
    protocol: ProtocolVersion,
    is_sender: bool,
) -> Result<(), ClientError> {
    let protect = config.protect_args().unwrap_or(false);

    let full_args = build_full_daemon_args(config, request, protocol, is_sender);

    // upstream: clientserver.c:393-405 - phase 1 sends args over the daemon text
    // protocol; with protect-args, only the minimal set is sent so the daemon
    // detects `-s` and stops at the NULL marker, expecting phase-2 secluded args.
    let phase1_args = if protect {
        build_minimal_daemon_args(is_sender)
    } else {
        // upstream: options.c:2590-2997 server_options() wraps every emitted
        // option-with-value through `safe_arg()` before it enters the wire
        // path. Under non-protect_args the daemon (rsync 3.4.4) responds with
        // `unbackslash_arg()` on its side. We mirror both halves here so a
        // value such as `--groupmap=*:1234;foo` round-trips through the
        // remote shell-like text protocol without losing its wildcards.
        full_args
            .iter()
            .map(|arg| safe_arg_for_daemon(arg))
            .collect()
    };

    // upstream: clientserver.c:348-349 - DEBUG_GTE(CMD, 1) emits
    // `print_child_argv("sending daemon args:", sargs)` immediately before the
    // per-arg write loop. `sargs` is the same payload we are about to ship,
    // so emit against `phase1_args` regardless of the protect-args mode.
    protocol::cmd::trace_sending_daemon_args(&phase1_args);

    let terminator = if protocol.as_u8() >= 30 { b'\0' } else { b'\n' };

    for arg in &phase1_args {
        stream.write_all(arg.as_bytes()).map_err(|e| {
            socket_error("send argument to", request.address.socket_addr_display(), e)
        })?;
        stream.write_all(&[terminator]).map_err(|e| {
            socket_error(
                "send terminator to",
                request.address.socket_addr_display(),
                e,
            )
        })?;
    }

    // upstream: empty string signals end of phase-1 argument list.
    stream.write_all(&[terminator]).map_err(|e| {
        socket_error(
            "send final terminator to",
            request.address.socket_addr_display(),
            e,
        )
    })?;

    // upstream: clientserver.c:407-408 send_protected_args(f_out, sargs) +
    // rsync.c:283-320 send_protected_args() - phase 2 sends the real arguments
    // via the secluded-args wire format (null-separated with empty terminator),
    // applying iconvbufs(ic_send, ...) per arg when --iconv is configured.
    if protect {
        let mut secluded = vec!["rsync"];
        secluded.extend(full_args.iter().map(String::as_str));
        // upstream: rsync.c:296-297 - DEBUG_GTE(CMD, 1) emits
        // `print_child_argv("protected args:", args + i + 1)` right before the
        // per-arg `iconvbufs(ic_send, ...)` loop. Upstream's argv begins after
        // the original NULL terminator at `args + i + 1`, which is the
        // post-`"rsync"` payload - emit the matching shape here.
        protocol::cmd::trace_protected_args(&secluded[1..]);
        let iconv_converter = config.iconv().resolve_converter();
        protocol::secluded_args::send_secluded_args(stream, &secluded, iconv_converter.as_ref())
            .map_err(|e| {
                socket_error(
                    "send secluded args to",
                    request.address.socket_addr_display(),
                    e,
                )
            })?;
    }

    stream.flush().map_err(|e| {
        socket_error(
            "flush arguments to",
            request.address.socket_addr_display(),
            e,
        )
    })?;

    Ok(())
}

/// Builds the minimal phase-1 argument list for protect-args daemon mode.
///
/// The daemon only needs `--server [--sender] -s .` to know that
/// secluded args follow in phase 2.
///
/// upstream: clientserver.c:393-405 - sargs has a NULL marker after `-s .`
pub(super) fn build_minimal_daemon_args(is_sender: bool) -> Vec<String> {
    let mut args = vec!["--server".to_owned()];
    if is_sender {
        args.push("--sender".to_owned());
    }
    args.push("-s".to_owned());
    args.push(".".to_owned());
    args
}

/// Builds the full argument list for daemon-mode transfer.
///
/// Mirrors upstream `server_options()` (`options.c:2590-2997`) which builds
/// the argument list sent from client to server.
///
/// In upstream, `am_sender` refers to the CLIENT being the sender (push).
/// In our code, `is_sender` means "daemon is sender" (pull). So upstream's
/// `am_sender` corresponds to `!is_sender` here.
pub(super) fn build_full_daemon_args(
    config: &ClientConfig,
    request: &DaemonTransferRequest,
    protocol: ProtocolVersion,
    is_sender: bool,
) -> Vec<String> {
    let mut args = Vec::new();
    // upstream: options.c:2590-2592
    args.push("--server".to_owned());
    if is_sender {
        args.push("--sender".to_owned());
    }

    // upstream: options.c:2797-2798
    let checksum_choice = config.checksum_choice();
    if let Some(override_algo) = checksum_choice.transfer_protocol_override() {
        args.push(format!("--checksum-choice={}", override_algo.as_str()));
    }

    // upstream: options.c:2594-2713 - single-character flag string (e.g., "-logDtprzc").
    // upstream: options.c:2710 - maybe_add_e_option() appends the capability
    // string directly onto the compact flag string, producing a single argument
    // like `-logDtpre.iLsfxCIvu`. We follow the same format for interop.
    let mut flag_string = flags::build_server_flag_string(config);
    if protocol.as_u8() >= 30 {
        // upstream: compat.c:177-178 daemon 'i' check, compat.c:720
        // set_allow_inc_recurse() - capability flags for protocol 30+.
        let capability_suffix = build_capability_string_suffix(config.inc_recursive_send());
        flag_string.push_str(&capability_suffix);
    }
    if !flag_string.is_empty() {
        args.push(flag_string);
    }

    let we_are_sender = !is_sender;

    // upstream: options.c:2750-2762 - server needs the log-format to generate
    // itemize output via MSG_INFO frames. Only sent when client is sender
    // (push), matching upstream am_sender guard.
    if we_are_sender && config.itemize_changes() {
        args.push("--log-format=%i".to_owned());
    }

    // upstream: options.c:2800-2805 - compress choice is only forwarded when
    // the user explicitly specified --compress-choice, --new-compress, or
    // --old-compress.
    if config.explicit_compress_choice() {
        let algo = config.compression_algorithm();
        let name = algo.name();
        match name {
            "zlibx" => args.push("--new-compress".to_owned()),
            "zlib" => args.push("--old-compress".to_owned()),
            _ => args.push(format!("--compress-choice={name}")),
        }
    }

    // upstream: options.c:2737-2740 - --compress-level=N
    if let Some(level) = config.compression_level() {
        args.push(format!(
            "--compress-level={}",
            compression_level_numeric(level)
        ));
    }

    // upstream: options.c:2807-2839 - sender-specific args.
    if we_are_sender {
        if let Some(max_delete) = config.max_delete() {
            if max_delete > 0 {
                args.push(format!("--max-delete={max_delete}"));
            } else {
                args.push("--max-delete=-1".to_owned());
            }
        }

        // upstream: options.c:2818-2829
        match config.delete_mode() {
            DeleteMode::Before => args.push("--delete-before".to_owned()),
            DeleteMode::Delay => args.push("--delete-delay".to_owned()),
            DeleteMode::During => args.push("--delete-during".to_owned()),
            DeleteMode::After => args.push("--delete-after".to_owned()),
            DeleteMode::Disabled => {}
        }
        if config.delete_excluded() {
            args.push("--delete-excluded".to_owned());
        }
        if config.force_replacements() {
            args.push("--force".to_owned());
        }

        // upstream: options.c:2836-2837
        if config.size_only() {
            args.push("--size-only".to_owned());
        }
    }

    // upstream: options.c:2878-2879
    if config.ignore_errors() {
        args.push("--ignore-errors".to_owned());
    }

    // upstream: options.c:2881-2882
    if config.copy_unsafe_links() {
        args.push("--copy-unsafe-links".to_owned());
    }

    // upstream: options.c:2884-2885
    if config.safe_links() {
        args.push("--safe-links".to_owned());
    }

    // upstream: options.c:2887-2888
    if config.numeric_ids() {
        args.push("--numeric-ids".to_owned());
    }

    // upstream: options.c:2890-2891
    if config.qsort() {
        args.push("--use-qsort".to_owned());
    }

    // upstream: options.c:2893-2925 - sender-only long-form args.
    if we_are_sender {
        if config.ignore_existing() {
            args.push("--ignore-existing".to_owned());
        }
        if config.existing_only() {
            args.push("--existing".to_owned());
        }
        if config.fsync() {
            args.push("--fsync".to_owned());
        }
        if let Some(depth) = config.io_uring_depth() {
            args.push(format!("--io-uring-depth={depth}"));
        }

        // upstream: options.c:2915-2923 - --compare-dest/copy-dest/link-dest
        // sent only when client is sender (push).
        for ref_dir in config.reference_directories() {
            let flag = match ref_dir.kind() {
                ReferenceDirectoryKind::Compare => "--compare-dest=",
                ReferenceDirectoryKind::Copy => "--copy-dest=",
                ReferenceDirectoryKind::Link => "--link-dest=",
            };
            args.push(format!("{flag}{}", ref_dir.path().display()));
        }
    }

    // upstream: options.c:2933-2942
    if config.append() {
        args.push("--append".to_owned());
        if config.append_verify() {
            args.push("--append".to_owned());
        }
    } else if config.inplace() {
        args.push("--inplace".to_owned());
    }

    // upstream: options.c:2787-2795
    if config.backup() {
        args.push("--backup".to_owned());
        if let Some(dir) = config.backup_directory() {
            args.push("--backup-dir".to_owned());
            args.push(dir.display().to_string());
        }
        if let Some(suffix) = config.backup_suffix() {
            args.push(format!("--suffix={}", suffix.to_string_lossy()));
        }
    }

    // upstream: options.c:2964-2965
    if config.remove_source_files() {
        args.push("--remove-source-files".to_owned());
    }

    // upstream: options.c:2944-2956 - --files-from
    {
        use crate::client::config::FilesFromSource;
        let client_is_sender = !is_sender;
        match config.files_from() {
            FilesFromSource::None => {}
            FilesFromSource::Stdin | FilesFromSource::LocalFile(_) => {
                if !client_is_sender {
                    args.push("--files-from=-".to_owned());
                    args.push("--from0".to_owned());
                }
            }
            FilesFromSource::RemoteFile(path) => {
                args.push(format!("--files-from={path}"));
                if config.from0() {
                    args.push("--from0".to_owned());
                }
            }
        }
    }

    // upstream: options.c:2894-2898 - --usermap / --groupmap are forwarded
    // verbatim. With `protect_args` (always on for daemon mode), upstream
    // `safe_arg()` returns the value unchanged (no shell escaping) because
    // the args are shipped over the secluded-args byte stream rather than a
    // shell command line. Wildcards like `*` must reach the receiver intact
    // so `uidlist.c:parse_name_map()` recognises them and installs a
    // `NFLAGS_WILD_NAME_MATCH` rule.
    if let Some(mapping) = config.user_mapping() {
        args.push(format!("--usermap={}", mapping.spec()));
    }
    if let Some(mapping) = config.group_mapping() {
        args.push(format!("--groupmap={}", mapping.spec()));
    }

    // upstream: options.c:2716-2723, options.c:2052-2054 - --iconv forwarding
    // to the remote daemon. When iconv_opt contains a comma, only the
    // post-comma half (daemon's local charset) is forwarded; otherwise the
    // whole string is forwarded as-is. `--iconv=-` (Disabled) and the default
    // (Unspecified) forward nothing because upstream nulls iconv_opt at
    // options.c:2052-2054 before this branch runs. Without this the daemon
    // never enables `ic_recv` and writes wire UTF-8 bytes verbatim.
    match config.iconv() {
        IconvSetting::Unspecified | IconvSetting::Disabled => {}
        IconvSetting::LocaleDefault => args.push("--iconv=.".to_owned()),
        IconvSetting::Explicit { local, remote } => {
            let forwarded = remote.as_deref().unwrap_or(local);
            args.push(format!("--iconv={forwarded}"));
        }
    }

    // upstream: dummy argument representing CWD.
    args.push(".".to_owned());

    let module_path = format!("{}/{}", request.module, request.path);
    args.push(module_path);

    strip_client_only_batch_flags(&mut args);
    args
}

/// Removes `--write-batch`, `--only-write-batch`, and `--read-batch` from a
/// daemon-bound argument vector.
///
/// These are client-local flags: upstream `options.c:server_options()` never
/// emits `--write-batch` or `--read-batch` to the server. The sole exception
/// is `--only-write-batch`, which upstream replaces with the literal token
/// `--only-write-batch=X` at `options.c:2832-2833` to force the server into
/// dry-run mode; the X value carries no real path.
///
/// We never construct daemon argv with batch flags today, but stripping here
/// is defense-in-depth: a future change that wires `remote_options` or any
/// other forwarded list into the daemon path would otherwise silently leak
/// the client's local batch state and cause the daemon to close the wire
/// mid-transfer (the symptom observed in upstream's batch-mode interop).
///
/// Both bare-flag (`--write-batch`) and `key=value` (`--write-batch=PATH`)
/// forms are stripped. The two-arg form (`--write-batch FILE`) drops the
/// following positional value so it does not become an orphan module path.
fn strip_client_only_batch_flags(args: &mut Vec<String>) {
    const CLIENT_ONLY: &[&str] = &["--write-batch", "--only-write-batch", "--read-batch"];

    let mut i = 0;
    while i < args.len() {
        let arg = args[i].as_str();
        let is_bare = CLIENT_ONLY.contains(&arg);
        let is_kv = CLIENT_ONLY
            .iter()
            .any(|flag| arg.starts_with(flag) && arg.as_bytes().get(flag.len()) == Some(&b'='));

        if is_bare {
            args.remove(i);
            if i < args.len() && !args[i].starts_with('-') {
                args.remove(i);
            }
            continue;
        }
        if is_kv {
            args.remove(i);
            continue;
        }
        i += 1;
    }
}

/// Characters that the remote shell wrapper (or upstream `unbackslash_arg`)
/// will interpret unless escaped. Mirrors upstream `options.c:2541`
/// `SHELL_CHARS`. Backslash is included so a literal `\` round-trips intact.
const SHELL_CHARS: &str = "!#$&;|<>(){}\"'` \t\\";

/// Wildcard characters that the remote shell would expand. Mirrors upstream
/// `options.c:2542` `WILD_CHARS`.
const WILD_CHARS: &str = "*?[]";

/// Mirrors upstream `options.c:safe_arg()` (rsync 3.4.4) for non-protect_args
/// daemon transmission.
///
/// Each argument is split at the first `=` (the upstream `opt = "--foo"` /
/// `arg = "value"` convention used throughout `server_options()`). The key
/// portion (`--foo=`) passes through verbatim while the value portion is
/// backslash-escaped: `WILD_CHARS` + `SHELL_CHARS` for option values, and
/// only `SHELL_CHARS` for the trailing filename / module-path argument.
///
/// The daemon side (rsync 3.4.4 `io.c:1295-1306` `unbackslash_arg()`) collapses
/// every `\X` sequence back into `X` before option parsing, so this
/// transformation is a strict inverse of the server-side reader.
///
/// Option flag args that contain neither `=` nor any escapable character
/// (e.g., `--server`, `--sender`, `--numeric-ids`) are returned verbatim
/// to avoid allocation.
fn safe_arg_for_daemon(arg: &str) -> String {
    let (prefix, value, escapes, is_filename_arg) = match arg.find('=') {
        Some(eq_pos) if arg.starts_with("--") => {
            // upstream: safe_arg("--foo", value) -> "--foo=" + escaped value
            // with WILD_CHARS+SHELL_CHARS escapes.
            (&arg[..=eq_pos], &arg[eq_pos + 1..], OPTION_ESCAPES, false)
        }
        _ => {
            // upstream: safe_arg(NULL, arg) - filename / module path arg uses
            // only SHELL_CHARS so wildcards stay shell-expandable.
            ("", arg, SHELL_CHARS, true)
        }
    };

    let needs_work = value.chars().any(|c| c == '\\' || escapes.contains(c));
    if !needs_work {
        return arg.to_owned();
    }
    escape_with(prefix, value, escapes, is_filename_arg)
}

/// Concatenation of `WILD_CHARS` + `SHELL_CHARS` used as the escape set for
/// option values (upstream `options.c:2544` ternary `WILD_CHARS SHELL_CHARS`).
const OPTION_ESCAPES: &str = "*?[]!#$&;|<>(){}\"'` \t\\";

/// Builds `prefix + backslash_escaped(value)` using the given escape set.
///
/// Mirrors upstream `options.c:2583-2590`. For each input byte:
///
/// - `\` is doubled into `\\` so the receiver's `unbackslash_arg` recovers
///   the literal backslash. The one exception is filename args, where an
///   existing `\` before a wildcard is left as-is to preserve the user's
///   intentional wildcard escape.
/// - Any character in `escapes` is prefixed with `\`.
/// - All other characters pass through verbatim.
fn escape_with(prefix: &str, value: &str, escapes: &str, is_filename_arg: bool) -> String {
    let mut out = String::with_capacity(prefix.len() + value.len() + 8);
    out.push_str(prefix);
    let bytes: Vec<char> = value.chars().collect();
    for (i, &ch) in bytes.iter().enumerate() {
        if ch == '\\' {
            // upstream: options.c:2585 - filename args preserve `\<wildcard>`
            // sequences verbatim so the user's deliberate wildcard escape
            // survives. Option args always double the backslash.
            let next = bytes.get(i + 1).copied().unwrap_or('\0');
            if !(is_filename_arg && WILD_CHARS.contains(next)) {
                out.push('\\');
            }
        } else if escapes.contains(ch) {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

/// Converts a [`compress::zlib::CompressionLevel`] to its numeric zlib value.
fn compression_level_numeric(level: compress::zlib::CompressionLevel) -> u32 {
    use compress::zlib::CompressionLevel;
    match level {
        CompressionLevel::None => 0,
        CompressionLevel::Fast => 1,
        CompressionLevel::Default => 6,
        CompressionLevel::Best => 9,
        CompressionLevel::Precise(n) => u32::from(n.get()),
    }
}

#[cfg(test)]
mod safe_arg_tests {
    use super::*;

    // upstream: options.c:2539 safe_arg(NULL, arg) - filename args (no opt)
    // escape only SHELL_CHARS, leaving wildcards intact so the remote shell
    // can still expand them when no remote-shell wrapper is involved.
    #[test]
    fn filename_arg_leaves_wildcards_alone() {
        assert_eq!(safe_arg_for_daemon("file*name"), "file*name");
        assert_eq!(safe_arg_for_daemon("question?path"), "question?path");
    }

    // upstream: options.c:2539 safe_arg(NULL, arg) - SHELL_CHARS get backslash
    // escaped even in filename args.
    #[test]
    fn filename_arg_escapes_shell_chars() {
        assert_eq!(
            safe_arg_for_daemon("file with space"),
            "file\\ with\\ space"
        );
        assert_eq!(
            safe_arg_for_daemon("dangerous;rm -rf /"),
            "dangerous\\;rm\\ -rf\\ /"
        );
    }

    // upstream: options.c:2544 - option args escape WILD_CHARS + SHELL_CHARS
    // because the daemon receiver `unbackslash_arg`s before option parsing.
    #[test]
    fn option_arg_escapes_wildcards_in_value() {
        assert_eq!(
            safe_arg_for_daemon("--groupmap=*:1234"),
            "--groupmap=\\*:1234"
        );
        assert_eq!(
            safe_arg_for_daemon("--usermap=alice:bob,*:1234"),
            "--usermap=alice:bob,\\*:1234"
        );
    }

    // The audit-cited regression: `--groupmap=*:1234;dangerous` must keep
    // both the wildcard and the shell-meta `;` after daemon-side
    // `unbackslash_arg()` reverses the escape.
    #[test]
    fn option_arg_round_trips_shell_meta_value() {
        let arg = "--groupmap=*:1234;dangerous";
        let escaped = safe_arg_for_daemon(arg);
        assert_eq!(escaped, "--groupmap=\\*:1234\\;dangerous");

        // Reverse the escape exactly the way the daemon's `unbackslash_arg`
        // would, byte by byte.
        let mut decoded = Vec::with_capacity(escaped.len());
        let bytes = escaped.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'\\' && i + 1 < bytes.len() {
                i += 1;
            }
            decoded.push(bytes[i]);
            i += 1;
        }
        assert_eq!(String::from_utf8(decoded).unwrap(), arg);
    }

    // Plain option args with no escapable chars are returned verbatim
    // (no allocation churn).
    #[test]
    fn plain_args_pass_through() {
        assert_eq!(safe_arg_for_daemon("--server"), "--server");
        assert_eq!(safe_arg_for_daemon("-logDtprz"), "-logDtprz");
        assert_eq!(safe_arg_for_daemon("."), ".");
        assert_eq!(safe_arg_for_daemon("module/path"), "module/path");
    }

    // upstream: options.c:2585 - filename args preserve `\<wildcard>` so the
    // user's intentional wildcard escape passes through to the remote shell.
    #[test]
    fn filename_arg_preserves_escaped_wildcard() {
        // Filename branch: \* (literal) is kept verbatim because the wildcard
        // is already escaped by the caller.
        assert_eq!(safe_arg_for_daemon("file\\*"), "file\\*");
    }

    // upstream: options.c:2583-2590 - option args always double an embedded
    // backslash so the daemon's `unbackslash_arg` collapses both halves and
    // recovers the original literal `\` plus the wildcard.
    #[test]
    fn option_arg_doubles_pre_escaped_wildcard() {
        // A pre-escaped `\*` in an option value travels as `\\\*`. The
        // daemon's unbackslash_arg turns `\\\*` into `\*` (the literal the
        // user typed). This is the round-trip both halves of the patch are
        // designed to preserve.
        let escaped = safe_arg_for_daemon("--groupmap=\\*:1234");
        assert_eq!(escaped, "--groupmap=\\\\\\*:1234");
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::strip_client_only_batch_flags;

    /// Test helper that exposes the private sanitiser to sibling test
    /// modules in this crate.
    pub(crate) fn strip_for_test(args: &mut Vec<String>) {
        strip_client_only_batch_flags(args);
    }

    #[test]
    fn strips_bare_write_batch() {
        let mut args = vec!["--server".into(), "--write-batch".into(), ".".into()];
        strip_client_only_batch_flags(&mut args);
        assert_eq!(args, vec!["--server", "."]);
    }

    #[test]
    fn strips_bare_write_batch_with_value() {
        let mut args = vec![
            "--server".into(),
            "--write-batch".into(),
            "/tmp/out.batch".into(),
            ".".into(),
        ];
        strip_client_only_batch_flags(&mut args);
        assert_eq!(args, vec!["--server", "."]);
    }

    #[test]
    fn strips_kv_write_batch() {
        let mut args = vec![
            "--server".into(),
            "--write-batch=/tmp/out.batch".into(),
            ".".into(),
        ];
        strip_client_only_batch_flags(&mut args);
        assert_eq!(args, vec!["--server", "."]);
    }

    #[test]
    fn strips_kv_read_batch() {
        let mut args = vec![
            "--server".into(),
            "--read-batch=/tmp/in.batch".into(),
            ".".into(),
        ];
        strip_client_only_batch_flags(&mut args);
        assert_eq!(args, vec!["--server", "."]);
    }

    #[test]
    fn strips_only_write_batch() {
        let mut args = vec![
            "--server".into(),
            "--only-write-batch=/tmp/dry.batch".into(),
            ".".into(),
        ];
        strip_client_only_batch_flags(&mut args);
        assert_eq!(args, vec!["--server", "."]);
    }

    #[test]
    fn leaves_non_batch_args_alone() {
        let mut args = vec![
            "--server".into(),
            "--sender".into(),
            "-logDtprz".into(),
            "--delete-before".into(),
            "--max-delete=10".into(),
            ".".into(),
            "module/path".into(),
        ];
        let original = args.clone();
        strip_client_only_batch_flags(&mut args);
        assert_eq!(args, original);
    }

    #[test]
    fn does_not_swallow_next_flag_when_two_arg_value_missing() {
        let mut args = vec![
            "--server".into(),
            "--write-batch".into(),
            "--sender".into(),
            ".".into(),
        ];
        strip_client_only_batch_flags(&mut args);
        assert_eq!(args, vec!["--server", "--sender", "."]);
    }
}
