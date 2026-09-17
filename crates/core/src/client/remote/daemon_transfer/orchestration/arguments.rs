//! Daemon argument building for client-to-server communication.
//!
//! Builds the daemon argument list mirroring upstream `server_options()` in
//! `options.c`. Supports both single-phase (plain) and two-phase (protect-args)
//! argument exchange protocols.

use std::ffi::{OsStr, OsString};
use std::io::Write;

use protocol::ProtocolVersion;
use transfer::setup::build_capability_string_suffix;

use crate::client::config::{
    ClientConfig, DeleteMode, IconvSetting, ReferenceDirectoryKind, StrongChecksumAlgorithm,
    TransferTimeout,
};
use crate::client::error::{ClientError, socket_error};
use crate::client::remote::daemon_transfer::connection::DaemonTransferRequest;
use crate::client::remote::flags;
use crate::client::remote::output_option::{OutputWordKind, make_output_option};

/// Sends daemon-mode arguments to the server.
///
/// When `--protect-args` / `-s` is active, uses a two-phase protocol
/// matching upstream `clientserver.c:393-408`:
/// - Phase 1: role markers + `--secluded-args` + `--iconv=...` (when
///   configured) so the daemon knows to expect protected args and, for a
///   real upstream daemon, parses `--iconv` while `need_unsorted_flist`'s
///   `protect_args != 2` guard still holds (see
///   [`build_minimal_daemon_args`] for the full rationale, including why
///   the long-form `-s` alias is used in place of a bare `-s` and why no
///   standalone `.` is emitted)
/// - Phase 2: remaining argument list via `send_secluded_args()` wire
///   format, with `--iconv` filtered back out since phase 1 already sent it
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

    // upstream: clientserver.c:395-407 - phase 1 sends args over the daemon text
    // protocol; with protect-args, only the minimal set is sent so the daemon
    // detects the secluded-args marker and expects phase-2 secluded args.
    let phase1_args: Vec<OsString> = if protect {
        build_minimal_daemon_args(config, is_sender)
            .into_iter()
            .map(OsString::from)
            .collect()
    } else {
        // upstream: options.c:2608-3015 server_options() wraps every emitted
        // option-with-value through `safe_arg()` before it enters the wire
        // path. Under non-protect_args the daemon responds with
        // `unbackslash_arg()` on its side. We mirror both halves here so a
        // value such as `--groupmap=*:1234;foo` round-trips through the
        // remote shell-like text protocol without losing its wildcards.
        //
        // The path operands are deliberately excluded. Upstream escapes them
        // with `safe_arg(NULL, ...)` only under `if (!daemon_connection)`
        // (main.c:619), and its daemon agrees: `read_args()` un-escapes just
        // the args preceding the `.` and routes everything after it through
        // `glob_expand()` untouched (io.c:1500-1506). Escaping an operand here
        // would ship literal backslashes that no peer removes - upstream 3.5.0
        // then splits the name on them, so `a b.txt` arrives as `/a/ b.txt`.
        let (options, operands) = split_at_operands(&full_args);
        options
            .iter()
            .map(|arg| safe_arg_for_daemon(arg))
            .chain(operands.iter().cloned())
            .collect()
    };

    // upstream: clientserver.c:348-349 - DEBUG_GTE(CMD, 1) emits
    // `print_child_argv("sending daemon args:", sargs)` immediately before the
    // per-arg write loop. `sargs` is the same payload we are about to ship,
    // so emit against `phase1_args` regardless of the protect-args mode.
    protocol::cmd::trace_sending_daemon_args(&phase1_args);

    let terminator = if protocol.as_u8() >= 30 { b'\0' } else { b'\n' };

    for arg in &phase1_args {
        stream.write_all(daemon_arg_wire_bytes(arg)).map_err(|e| {
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
        // upstream: options.c:2734-2745 - `--iconv=...` is emitted before the
        // NULL cutoff, so it already travelled in phase 1
        // (`build_minimal_daemon_args`). Skip it here to avoid sending it
        // twice; `full_args` still carries it for the non-protect single-phase
        // send path above. The filter operates on the arg's raw bytes so the
        // trailing operand (which never matches the ASCII `--iconv=` prefix)
        // keeps its non-UTF-8 bytes intact.
        let payload: Vec<&OsStr> = full_args
            .iter()
            .filter(|a| !daemon_arg_wire_bytes(a).starts_with(b"--iconv="))
            .map(OsString::as_os_str)
            .collect();
        // upstream: rsync.c:296-297 - DEBUG_GTE(CMD, 1) emits
        // `print_child_argv("protected args:", args + i + 1)` right before the
        // per-arg `iconvbufs(ic_send, ...)` loop. Upstream's argv begins after
        // the original NULL terminator at `args + i + 1`, which is the
        // post-`"rsync"` payload - emit the matching shape here.
        protocol::cmd::trace_protected_args(&payload);
        // upstream: rsync.c:283-296 send_protected_args() prepends argv[0]
        // (`"rsync"`) ahead of the payload. Ship every arg as raw bytes so a
        // non-UTF-8 operand path survives verbatim (secluded_args writes bytes
        // unchanged; `send_secluded_args` accepts any `AsRef<[u8]>`).
        let mut secluded: Vec<&[u8]> = Vec::with_capacity(payload.len() + 1);
        secluded.push(b"rsync".as_slice());
        secluded.extend(payload.iter().map(|a| daemon_arg_wire_bytes(a)));
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
/// Upstream's phase 1 wire (`clientserver.c:395-402`) emits each `sargs[]`
/// entry up to the `NULL` marker that `server_options()` inserts at
/// `options.c:2745`. That marker sits AFTER the compact flag string and
/// `--iconv=...` but BEFORE every post-NULL long-form option and the
/// trailing `.` / module path which `do_cmd` appends at `clientserver.c:303`.
/// As a result upstream's phase 1 wire never contains a standalone `.` or
/// a bare `-s`: the `s` for `--secluded-args` is embedded inside the
/// compact flag string (`argstr[x++] = 's'`, `options.c:2622-2623`).
///
/// We emit only the role markers, `--secluded-args`, and `--iconv=...` (when
/// configured) here so that:
///
/// 1. The daemon's `has_secluded_args_flag` check still trips and reads
///    phase 2 via `recv_secluded_args` (`--secluded-args` is in the same
///    detection set as `-s`).
/// 2. The merged arg list never carries a spurious standalone `.` from
///    phase 1 - the only `.` is the one phase 2 supplies as the positional
///    separator, so `apply_long_form_args`'s first-`.` dot_position lookup
///    correctly bounds the option region. A stray phase-1 `.` was dropping
///    every long-form option emitted after the merge boundary, including
///    `--groupmap=*:GID` (upstream issue #829 / daemon-groupmap-wild).
/// 3. The merged arg list never carries a bare `-s` short-form arg that
///    would shadow the real compact flag string in `build_server_config`'s
///    first-short-form-arg picker. The real flag string arrives in phase 2
///    via `build_full_daemon_args`.
/// 4. `--iconv=...`, when configured, is parsed by a real upstream daemon
///    while `protect_args` still reads `1` (not yet forced to `2` at
///    `clientserver.c:1082`), so `options.c:2069-2074`'s `need_unsorted_flist
///    = 1` side effect fires. If `--iconv` were deferred to phase 2 (as
///    every other long-form option is), a real upstream daemon would parse
///    it under `protect_args == 2` and `options.c:2070`'s `protect_args !=
///    2` guard would suppress `need_unsorted_flist`, breaking the sender's
///    and receiver's shared NDX-vs-unsorted-index correlation whenever
///    `-s`/`--secluded-args` and `--iconv` are combined against a real
///    upstream daemon.
///
/// Upstream daemons accept `--secluded-args` as the long-form alias of `-s`
/// (`options.c:804`), so this remains wire-compatible with upstream rsync.
///
/// # Upstream Reference
///
/// - `clientserver.c:303` - `sargs[sargc++] = "."` AFTER `server_options()`
/// - `clientserver.c:395-402` - phase 1 wire writes args until `!sargs[i]`
/// - `clientserver.c:1080-1082` - `protect_args = 2` only takes effect AFTER
///   phase 1's `parse_arguments()` returns
/// - `options.c:2069-2074` - `need_unsorted_flist = 1` guarded by
///   `protect_args != 2`
/// - `options.c:2622-2623` - `argstr[x++] = 's'` when `protect_args`
/// - `options.c:2734-2741` - `--iconv=...` emitted immediately before the
///   NULL cutoff
/// - `options.c:2744-2745` - NULL marker between phase 1 / phase 2 args
/// - `options.c:804` - `--secluded-args` long-form alias for `-s`
pub(super) fn build_minimal_daemon_args(config: &ClientConfig, is_sender: bool) -> Vec<String> {
    let mut args = vec!["--server".to_owned()];
    if is_sender {
        args.push("--sender".to_owned());
    }
    args.push("--secluded-args".to_owned());
    if let Some(arg) = daemon_iconv_arg(config) {
        args.push(arg);
    }
    args
}

/// Builds the `--iconv=...` argument when configured, or `None`.
///
/// Shared by [`build_minimal_daemon_args`] (phase-1, when protect-args is
/// active) and [`build_full_daemon_args`] (the non-protect single-phase send
/// and phase-2's fallback carrier). Mirrors upstream `options.c:2734-2741`.
fn daemon_iconv_arg(config: &ClientConfig) -> Option<String> {
    match config.iconv() {
        IconvSetting::Unspecified | IconvSetting::Disabled => None,
        IconvSetting::LocaleDefault => Some("--iconv=.".to_owned()),
        IconvSetting::Explicit { local, remote } => {
            let forwarded = remote.as_deref().unwrap_or(local);
            Some(format!("--iconv={forwarded}"))
        }
    }
}

/// Builds the full argument list for daemon-mode transfer.
///
/// Mirrors upstream `server_options()` (`options.c:2608-3015`) which builds
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
) -> Vec<OsString> {
    let mut args = Vec::new();
    // upstream: options.c:2608-2610
    args.push("--server".to_owned());
    if is_sender {
        args.push("--sender".to_owned());
    }

    // upstream: options.c server_options() `am_sender` is true when the CLIENT
    // is the sender (a PUSH). Here `is_sender` means the DAEMON is the sender
    // (a PULL), so upstream's `am_sender` corresponds to `!is_sender`.
    let we_are_sender = !is_sender;

    // upstream: options.c:2815-2816 server_options() forwards the RAW
    // --checksum-choice string verbatim - both comma components - gated only on
    // `checksum_choice` being non-null. That pointer is nulled solely for the
    // fully-auto forms (options.c:1997-2003), so forward the full choice
    // whenever it is not fully-auto, mirroring the SSH path (invocation builder)
    // rather than collapsing to the transfer component alone.
    let checksum_choice = config.checksum_choice();
    if checksum_choice.transfer() != StrongChecksumAlgorithm::Auto
        || checksum_choice.file() != StrongChecksumAlgorithm::Auto
    {
        args.push(format!(
            "--checksum-choice={}",
            checksum_choice.to_argument()
        ));
    }

    // upstream: options.c:2612-2731 - single-character flag string (e.g., "-logDtprzc").
    // upstream: options.c:2728 - maybe_add_e_option() appends the capability
    // string directly onto the compact flag string, producing a single argument
    // like `-logDtpre.iLsfxCIvu`. We follow the same format for interop.
    let mut flag_string = flags::build_server_flag_string(config);

    // upstream: options.c:2641-2660 - server_options() packs a direction-
    // specific branch of compact letters. `build_server_flag_string` is
    // role-agnostic and also feeds the local in-process ServerConfig parser
    // (server_config.rs), so the role-gated letters are applied here, on the
    // daemon wire path only. On a daemon PUSH the local client is the sender
    // (`we_are_sender`), so the `am_sender` letters (K/m/O/J/y/E) ride to the
    // remote receiver; on a PULL the remote is the sender, so the `else`-branch
    // letters (L/k) ride to it instead and the local receiver applies
    // omit-dir/link-times, prune-empty-dirs, and fuzzy matching itself.
    if we_are_sender {
        // upstream: options.c:2642-2643 - keep_dirlinks 'K'.
        if config.keep_dirlinks() {
            flag_string.push('K');
        }
        // upstream: options.c:2644-2645 - prune_empty_dirs 'm'.
        if config.prune_empty_dirs() {
            flag_string.push('m');
        }
        // upstream: options.c:2646-2647 - omit_dir_times 'O'.
        if config.omit_dir_times() {
            flag_string.push('O');
        }
        // upstream: options.c:2648-2649 - omit_link_times 'J'.
        if config.omit_link_times() {
            flag_string.push('J');
        }
        // upstream: options.c:2650-2654 - fuzzy_basis 'y', with a second 'y'
        // for level 2 (--fuzzy --fuzzy).
        for _ in 0..config.fuzzy_level() {
            flag_string.push('y');
        }
        // upstream: options.c:2690-2693 - `if (preserve_perms) 'p'; else if
        // (preserve_executability && am_sender) 'E'`. build_server_flag_string
        // already packed 'p' when perms are on; 'E' is its mutually-exclusive
        // sender-only alternative. The local ServerConfig parser ignores 'E'
        // (transfer/flags.rs), so this is a pure wire signal for the remote
        // receiver's generator to keep the executable bit.
        if !config.preserve_permissions() && config.preserve_executability() {
            flag_string.push('E');
        }
    } else {
        // upstream: options.c:2655-2660 - the `!am_sender` (else) branch packs
        // copy_links 'L' and copy_dirlinks 'k'. On a daemon PULL the remote is
        // the sender, so these ride to it to dereference symlinks and
        // dir-symlinks; on a PUSH they are omitted (the local sender
        // dereferences itself). `build_server_flag_string` no longer packs L/k,
        // so the pull wire gets them here.
        if config.copy_links() {
            flag_string.push('L');
        }
        if config.copy_dirlinks() {
            flag_string.push('k');
        }
    }

    if protocol.as_u8() >= 30 {
        // upstream: compat.c:162-181 set_allow_inc_recurse() and
        // options.c:3036 maybe_add_e_option() - `allow_inc_recurse` resolves
        // the option state (`ClientConfig::allow_inc_recurse`, which folds in
        // upstream's `!recurse || use_qsort` gate); the local restriction on
        // top is that 'i' is only advertised when this side actually honors
        // INC_RECURSE on its receive path.
        // For daemon pull (`is_sender=true` means daemon is sender; we are
        // receiver) the receiver clears CF_INC_RECURSE in compat.rs after
        // reading it. If we still advertise 'i' the daemon writes the file
        // list in INC_RECURSE format (trailing NDX_FLIST_EOF), the receiver
        // skips `receive_extra_file_lists`, and the leftover 0xFF marker
        // trips `read_varint` overflow on the next decode.
        // upstream: io.c:1816 read_varint - rejects encodings with extra > 4.
        let we_are_receiver = is_sender;
        let advertise_inc_recurse = config.allow_inc_recurse() && !we_are_receiver;
        let capability_suffix = build_capability_string_suffix(advertise_inc_recurse);
        flag_string.push_str(&capability_suffix);
    }
    if !flag_string.is_empty() {
        args.push(flag_string);
    }

    // upstream: options.c:2747-2748 - `if (list_only > 1) "--list-only"`. Only
    // the EXPLICIT `--list-only` is forwarded (the implicit single-source
    // listing is not). The compact 'n' is NOT packed for list-only.
    if config.list_only_arg() {
        args.push("--list-only".to_owned());
    }

    // upstream: options.c:2782-2785 - `--msgs2stderr` (msgs2stderr == 1) or
    // `--no-msgs2stderr` (== 0); the default (2) forwards nothing.
    match config.msgs2stderr() {
        Some(true) => args.push("--msgs2stderr".to_owned()),
        Some(false) => args.push("--no-msgs2stderr".to_owned()),
        None => {}
    }

    // upstream: options.c:2936-2948 - `if (stdout_format && am_sender)` the
    // server is told a little about the client's out-format via a `--log-format`
    // arg, in a first-match-wins chain. Only sent when the client is the sender
    // (push), matching upstream's `am_sender` guard. The `%i` branches key off
    // `stdout_format_has_i`, which upstream derives from the RESOLVED out-format
    // string (options.c:2345-2358), not the `-i` flag: an explicit
    // `--out-format` without `%i` clears it even under `-i`, while `-i` alone
    // installs the default `"%i %n%L"` format. `%i%I` is the `-ii` form
    // (stdout_format_has_i > 1) that itemizes unchanged entries too; `%o` is
    // forwarded when the format has the `%o` operation directive; the
    // placeholder `X` is forwarded when a non-verbose client set an out-format
    // with neither `%i` nor `%o`.
    if we_are_sender {
        if config.out_format_forwards_i() {
            if config.itemize_unchanged() {
                args.push("--log-format=%i%I".to_owned());
            } else {
                args.push("--log-format=%i".to_owned());
            }
        } else if config.out_format_has_operation() {
            args.push("--log-format=%o".to_owned());
        } else if config.out_format_placeholder() && config.verbosity() == 0 {
            args.push("--log-format=X".to_owned());
        }
    }

    // upstream: options.c:2818-2823 - compress choice is only forwarded when
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

    // upstream: options.c:2755-2758 - --compress-level=N
    if let Some(level) = config.compression_level() {
        args.push(format!(
            "--compress-level={}",
            compression_level_numeric(level)
        ));
    }

    // upstream: options.c:2953-2957 - `asprintf(&arg, "-B%u", (int)block_size)`
    // inside `if (block_size) {`. The SHORT spelling is what upstream puts on
    // the wire, so the daemon arg vector must carry it too: a `--block-size=`
    // token is an oc-only spelling that no upstream daemon parses, and it left
    // the remote generator sizing blocks by the square-root heuristic instead
    // of the requested size.
    if let Some(bs) = config.block_size_override() {
        args.push(format!("-B{}", bs.get()));
    }

    // upstream: options.c:2793-2797 - --timeout=N so both peers enforce the
    // same idle deadline.
    if let TransferTimeout::Seconds(secs) = config.timeout() {
        args.push(format!("--timeout={}", secs.get()));
    }

    // upstream: options.c:2966 - `--bwlimit=%d` forwards the rate in whole KiB
    // (options.c:1718), NOT bytes: the remote peer re-parses the value with a
    // default `K` suffix, so a byte count would be scaled up 1024x and the
    // throttle would effectively vanish.
    if let Some(bwlimit) = config.bandwidth_limit() {
        args.push(format!("--bwlimit={}", bwlimit.server_option_kib()));
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

        // upstream: options.c:2818-2829 - explicit timing variants are always
        // sent; bare --delete (DuringDefault) is suppressed when
        // --delete-excluded is active.
        match config.delete_mode() {
            DeleteMode::Before => args.push("--delete-before".to_owned()),
            DeleteMode::Delay => args.push("--delete-delay".to_owned()),
            DeleteMode::During => args.push("--delete-during".to_owned()),
            DeleteMode::DuringDefault => {
                if !config.delete_excluded() {
                    args.push("--delete".to_owned());
                }
            }
            DeleteMode::After => args.push("--delete-after".to_owned()),
            DeleteMode::Disabled => {}
        }
        if config.delete_excluded() {
            args.push("--delete-excluded".to_owned());
        }
        if config.force_replacements() {
            args.push("--force".to_owned());
        }

        // upstream: options.c:2854-2855
        if config.size_only() {
            args.push("--size-only".to_owned());
        }

        // upstream: options.c:2832-2835 - --min-size / --max-size are emitted
        // only in the `am_sender` branch; the remote receiver's generator then
        // skips files outside the range exactly as the client would.
        if let Some(min) = config.min_file_size() {
            args.push(format!("--min-size={min}"));
        }
        if let Some(max) = config.max_file_size() {
            args.push(format!("--max-size={max}"));
        }

        // upstream: options.c:2852-2857 - sender-only `--super` (am_root > 1)
        // and `--stats` (do_stats). Shared with the SSH push builder via
        // flags::sender_super_stats_args so both transports forward the same
        // trailer on a push.
        args.extend(flags::sender_super_stats_args(config).map(str::to_owned));
    } else if let Some(spec) = config.skip_compress_spec() {
        // upstream: options.c:2858-2860 - `else { if (skip_compress)
        // safe_arg("--skip-compress", skip_compress); }`. Forwarded only on a
        // PULL (the remote sender performs the compression). Only an
        // explicitly-set spec is sent; the built-in default list is never
        // forwarded.
        args.push(format!("--skip-compress={spec}"));
    }

    // upstream: options.c:2863-2864 - `if (max_alloc_arg && max_alloc !=
    // DEFAULT_MAX_ALLOC) --max-alloc`. Not `am_sender` gated: each side owns
    // its own cap, so forwarding lets the remote enforce the same budget.
    // `max_alloc()` is None unless the user supplied a non-default value.
    if let Some(limit) = config.max_alloc() {
        args.push(format!("--max-alloc={limit}"));
    }

    // upstream: options.c:2873-2878 - modify_window forwarded only when set AND
    // `am_sender` (the remote receiver's generator runs the mtime quick-check).
    // A negative window (nanosecond-exact) uses the short `-@%d` spelling; a
    // non-negative window uses `--modify-window=%d`.
    if we_are_sender && let Some(window) = config.modify_window() {
        if window < 0 {
            args.push(format!("-@{window}"));
        } else {
            args.push(format!("--modify-window={window}"));
        }
    }

    // upstream: options.c:2880-2884 - --checksum-seed=N so the remote uses the
    // same seed for rolling and strong checksum generation. Not `am_sender`
    // gated.
    if let Some(seed) = config.checksum_seed() {
        args.push(format!("--checksum-seed={seed}"));
    }

    // upstream: options.c:2896-2897
    if config.ignore_errors() {
        args.push("--ignore-errors".to_owned());
    }

    // upstream: options.c:2899-2900
    if config.copy_unsafe_links() {
        args.push("--copy-unsafe-links".to_owned());
    }

    // upstream: options.c:2902-2903
    if config.safe_links() {
        args.push("--safe-links".to_owned());
    }

    // upstream: options.c:2760-2765 - the compact 'D' letter now tracks
    // preserve_devices only (build_server_flag_string). specials ride separately:
    // `if (preserve_devices) { if (!preserve_specials) --no-specials } else if
    // (preserve_specials) --specials`. --no-specials (not --devices) keeps
    // backward compatibility since -D already carries devices.
    if config.preserve_devices() {
        if !config.preserve_specials() {
            args.push("--no-specials".to_owned());
        }
    } else if config.preserve_specials() {
        args.push("--specials".to_owned());
    }

    // upstream: options.c:2905-2906
    if config.numeric_ids() {
        args.push("--numeric-ids".to_owned());
    }

    // upstream: options.c:2908-2909
    if config.qsort() {
        args.push("--use-qsort".to_owned());
    }

    // upstream: options.c:2911-2943 - sender-only long-form args.
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

        // upstream: options.c:2933-2941 - --compare-dest/copy-dest/link-dest
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

    // upstream: options.c:2945-2949 server_options() - make_output_option()
    // forwards the explicitly-set --info / --debug levels so the daemon peer's
    // diagnostic output matches the client's request. `we_are_sender` (a push)
    // selects the receiving half of the role `where` filter.
    if let Some(arg) = make_output_option(OutputWordKind::Info, config.info_flags(), we_are_sender)
    {
        args.push(arg);
    }
    if let Some(arg) =
        make_output_option(OutputWordKind::Debug, config.debug_flags(), we_are_sender)
    {
        args.push(arg);
    }

    // upstream: options.c:2866-2871 - --delete-missing-args needs the
    // cooperation of both sides, so it is always forwarded to the server.
    // --ignore-missing-args is forwarded only when the local side is the
    // receiver (`!am_sender`); a sender handles ignore by itself. Here
    // `we_are_sender == !is_sender` mirrors upstream `am_sender`, so the
    // ignore branch fires when the daemon is the sender (`is_sender`).
    if config.delete_missing_args() {
        args.push("--delete-missing-args".to_owned());
    } else if config.ignore_missing_args() && !we_are_sender {
        args.push("--ignore-missing-args".to_owned());
    }

    // upstream: options.c:2951-2960
    if config.append() {
        args.push("--append".to_owned());
        if config.append_verify() {
            args.push("--append".to_owned());
        }
    } else if config.inplace() {
        args.push("--inplace".to_owned());
    }

    // upstream: options.c:2886-2894 - `if (partial_dir && am_sender) {
    // --partial-dir ...; if (delay_updates) --delay-updates } else if
    // (keep_partial && am_sender) --partial`. There is no compact 'P'. All are
    // `am_sender` (a daemon PUSH: `we_are_sender`). --delay-updates implies an
    // implicit tmp partial_dir upstream, so it is emitted (suppressing the bare
    // --partial else-branch) even when no explicit --partial-dir was given.
    if we_are_sender {
        if let Some(dir) = config.partial_directory() {
            args.push(format!("--partial-dir={}", dir.display()));
            if config.delay_updates() {
                args.push("--delay-updates".to_owned());
            }
        } else if config.delay_updates() {
            args.push("--delay-updates".to_owned());
        } else if config.partial() {
            args.push("--partial".to_owned());
        }
    }

    // upstream: options.c:2925-2928 - `if (tmpdir) { --temp-dir; safe_arg("",
    // tmpdir); }` inside the `am_sender` block, so the remote receiver writes
    // temp files under the requested directory.
    if we_are_sender && let Some(dir) = config.temp_directory() {
        args.push(format!("--temp-dir={}", dir.display()));
    }

    // upstream: options.c:2648-2649 - `make_backups` rides in the compact
    // flag string as `b` (added by `build_server_flag_string`). `--backup-dir`
    // and `--suffix` remain long-form (`options.c:2807,2813`).
    if config.backup() {
        if let Some(dir) = config.backup_directory() {
            args.push("--backup-dir".to_owned());
            args.push(dir.display().to_string());
        }
        if let Some(suffix) = config.backup_suffix() {
            args.push(format!("--suffix={}", suffix.to_string_lossy()));
        }
    }

    // upstream: options.c:2982-2985 - `if (remove_source_files == 1)
    // "--remove-source-files"; else if (remove_source_files)
    // "--remove-sent-files"`. The deprecated alias is forwarded verbatim when
    // the user typed it, matching upstream byte-for-byte.
    if config.remove_source_files() {
        if config.remove_sent_files() {
            args.push("--remove-sent-files".to_owned());
        } else {
            args.push("--remove-source-files".to_owned());
        }
    }

    // upstream: options.c:2979 - `if (write_devices && am_sender) args[ac++] =
    // "--write-devices"`. Forwarded only when the local side is the sender
    // (`we_are_sender`, a push), so the remote receiver writes into existing
    // device destinations instead of recreating them with mknod.
    if config.write_devices() && we_are_sender {
        args.push("--write-devices".to_owned());
    }

    // upstream: options.c:2987 - `if (copy_devices && !am_sender) args[ac++] =
    // "--copy-devices"`. Forwarded only when the local side is the receiver
    // (a pull, where the daemon is the sender: `is_sender`), so the remote
    // sender reads device contents as regular file data. `is_sender == !am_sender`
    // here (see the module note above).
    if config.copy_devices() && is_sender {
        args.push("--copy-devices".to_owned());
    }

    // upstream: options.c:3167-3168 - `if (mkpath_dest_arg && am_sender)`.
    // The dest-arg path creation is receiver-side, so forward `--mkpath` only
    // on a push (local client is the sender). `!is_sender` mirrors upstream's
    // `am_sender` here (see the module note above).
    if config.mkpath() && !is_sender {
        args.push("--mkpath".to_owned());
    }

    // upstream: options.c:2976-2977 - `if (relative_paths && !implied_dirs &&
    // (!am_sender || protocol_version >= 30)) --no-implied-dirs`. The flag is
    // forwarded only for relative transfers (implied dirs exist solely for
    // relative-rooted paths). The `(!am_sender || protocol_version >= 30)` guard
    // drops the flag on a PUSH below protocol 30 - reachable here because
    // `--protocol=N` caps the version negotiated from the `@RSYNCD:` greeting.
    // Without the relative_paths gate a non-relative transfer with
    // implied_dirs=0 (options.c:2207) would wrongly forward the flag, which the
    // remote sender then stats as a source path.
    if config.relative_paths()
        && !config.implied_dirs()
        && (!we_are_sender || protocol.as_u8() >= 30)
    {
        args.push("--no-implied-dirs".to_owned());
    }

    // upstream: options.c:2990-2991 - `if (preallocate_files && am_sender)
    // --preallocate`. Forwarded only on a PUSH (`we_are_sender`) so the remote
    // receiver preallocates the destination file extents.
    if we_are_sender && config.preallocate() {
        args.push("--preallocate".to_owned());
    }

    // upstream: options.c:2993-2994 - `if (open_noatime && preserve_atimes <= 1)
    // --open-noatime`. Not `am_sender` gated; the side that opens source files
    // for reading suppresses atime updates.
    if config.open_noatime() {
        args.push("--open-noatime".to_owned());
    }

    // upstream: options.c:2962-2980 - server_options() forwards the
    // files-from arg only when the remote peer reads the list. `is_sender`
    // here means the daemon is the sender (PULL), so the local side pushes
    // when `!is_sender`. The direction-aware resolver collapses the single
    // files-from fd so a localhost:path hostspec is never double-sourced.
    {
        let local_is_push = !is_sender;
        let plan = config
            .files_from()
            .resolve_for(local_is_push, config.from0());
        if let Some(arg) = plan.remote_arg {
            args.push(format!("--files-from={arg}"));
            if plan.remote_from0 {
                args.push("--from0".to_owned());
            }
            // upstream: options.c:2972-2973 - `if (!relative_paths)
            // --no-relative` inside the files-from block. A peer that reads the
            // --files-from list defaults relative_paths=1 (options.c:2205-2206);
            // when the client resolved relative off (explicit --no-relative),
            // emit --no-relative so the remote peer overrides that default and
            // flattens each entry to its basename with no implied parent dirs.
            if !config.relative_paths() {
                args.push("--no-relative".to_owned());
            }
        }
    }

    // upstream: options.c:2912-2916 - --usermap / --groupmap are forwarded
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

    // upstream: options.c:2734-2741, options.c:2052-2054 - --iconv forwarding
    // to the remote daemon. When iconv_opt contains a comma, only the
    // post-comma half (daemon's local charset) is forwarded; otherwise the
    // whole string is forwarded as-is. `--iconv=-` (Disabled) and the default
    // (Unspecified) forward nothing because upstream nulls iconv_opt at
    // options.c:2052-2054 before this branch runs. Without this the daemon
    // never enables `ic_recv` and writes wire UTF-8 bytes verbatim.
    //
    // Under protect-args, `send_daemon_arguments` strips this entry back out
    // of the phase-2 payload because `build_minimal_daemon_args` already sent
    // it in phase 1 (see that function's doc comment for why the phase
    // matters to a real upstream daemon's `need_unsorted_flist`).
    if let Some(arg) = daemon_iconv_arg(config) {
        args.push(arg);
    }

    // upstream: options.c:3175-3182 - `server_options()` appends every -M /
    // --remote-option value verbatim, after all other options. That function
    // builds the argv for BOTH transports (clientserver.c:340 for a daemon,
    // main.c:611 for a remote shell), so the daemon path forwards them exactly
    // as the remote-shell path does; omitting them here silently discarded
    // every `-M` on an rsync:// transfer.
    //
    // Byte fidelity is bounded by this builder's `Vec<String>` argv, the same
    // constraint that already applies to --iconv, --usermap and
    // --checksum-choice here; widening the whole vector to OsString is tracked
    // separately.
    for opt in config.remote_options() {
        args.push(opt.to_string_lossy().into_owned());
    }

    // upstream: dummy argument representing CWD.
    args.push(DAEMON_ARG_SEPARATOR.to_owned());

    strip_client_only_batch_flags(&mut args);

    // Every option arg above is ASCII/formatted text and widens to `OsString`
    // losslessly. The module/path operand is the sole argument that can carry a
    // non-UTF-8 byte (e.g. a 0xFF in the requested path), so it is built
    // byte-preserving and appended after the `Vec<String>` option stage - it
    // reaches the secluded-args wire verbatim rather than through a lossy
    // `String`. upstream: clientserver.c:303 appends the module/path operand as
    // raw `char*`, mirroring the SSH path's end-to-end `OsString` operand
    // handling (invocation/builder.rs).
    let mut os_args: Vec<OsString> = args.into_iter().map(OsString::from).collect();
    os_args.push(build_module_operand(request));
    os_args
}

/// Builds the `module/path` operand as a byte-preserving `OsString`.
///
/// The module name is ASCII (daemon config grammar) and joined with a literal
/// `/`; the path is appended verbatim via [`OsString::push`], which concatenates
/// the raw bytes on Unix so a non-UTF-8 filename survives to the wire.
fn build_module_operand(request: &DaemonTransferRequest) -> OsString {
    let mut operand = OsString::with_capacity(request.module.len() + 1 + request.path.len());
    operand.push(&request.module);
    operand.push("/");
    operand.push(&request.path);
    operand
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
            // Drop the trailing batch FILE in the two-arg form, but never
            // consume `.` / `..` - those are the server-role indicators
            // (upstream main.c:1142 sets local_name = "." when --server is
            // a sender) and must reach the daemon-bound argv intact.
            if i < args.len() && !args[i].starts_with('-') && args[i] != "." && args[i] != ".." {
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
/// will interpret unless escaped. Mirrors upstream `options.c:2695`
/// `SHELL_CHARS`. Backslash is included so a literal `\` round-trips intact,
/// and `\n`/`\r` so a newline cannot terminate the command and start a second.
const SHELL_CHARS: &str = "!#$&;|<>(){}\"'` \t\n\r\\";

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
/// The lone `.` that upstream pushes after `server_options()`
/// (`clientserver.c:303`) to stand for the remote CWD. It is also the marker
/// that separates option args from path operands on the wire: the daemon's
/// `read_args()` switches behaviour the moment it sees it (`io.c:1500-1506`).
const DAEMON_ARG_SEPARATOR: &str = ".";

/// Splits a daemon argument vector into `(option args, path operands)` at
/// [`DAEMON_ARG_SEPARATOR`], which stays with the options because the daemon
/// un-escapes it alongside them.
///
/// A vector with no separator is all options - that is the shape
/// `build_minimal_daemon_args` produces, and it carries no operands to protect.
fn split_at_operands(args: &[OsString]) -> (&[OsString], &[OsString]) {
    let separator = OsStr::new(DAEMON_ARG_SEPARATOR);
    let operands_start = args
        .iter()
        .position(|arg| arg.as_os_str() == separator)
        .map_or(args.len(), |dot| dot + 1);
    args.split_at(operands_start)
}

/// Byte-generic wrapper over [`escape_daemon_arg_bytes`] used by the wire path.
///
/// On Unix the escape runs over the operand's raw filesystem bytes, so a
/// non-UTF-8 option value (e.g. `--tmpdir=<path with a 0xFF byte>`) survives
/// verbatim through the daemon's `unbackslash_arg` (upstream `io.c:1441`).
/// Other targets escape the lossy Unicode view, which is exact for the
/// argv-sourced operands they carry; the escape only ever inserts ASCII
/// backslashes, so the result stays valid UTF-8.
fn safe_arg_for_daemon(arg: &OsStr) -> OsString {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::{OsStrExt, OsStringExt};
        OsString::from_vec(escape_daemon_arg_bytes(arg.as_bytes()))
    }
    #[cfg(not(unix))]
    {
        let escaped = escape_daemon_arg_bytes(arg.to_string_lossy().as_bytes());
        OsString::from(String::from_utf8_lossy(&escaped).into_owned())
    }
}

/// Raw wire bytes of a daemon argument. On Unix these are the verbatim
/// filesystem bytes (`OsStrExt::as_bytes`); elsewhere the WTF-8 encoding
/// (`as_encoded_bytes`), which is exact for the Unicode operands non-Unix
/// targets carry.
fn daemon_arg_wire_bytes(arg: &OsStr) -> &[u8] {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        arg.as_bytes()
    }
    #[cfg(not(unix))]
    {
        arg.as_encoded_bytes()
    }
}

/// Whether byte `b` must be backslash-escaped in a daemon arg.
///
/// Mirrors upstream `options.c:2698`
/// `escapes = is_filename_arg ? SHELL_CHARS : WILD_CHARS SHELL_CHARS`: filename
/// args escape only `SHELL_CHARS` (leaving wildcards shell-expandable), while
/// option values also escape `WILD_CHARS`. Deriving the option set from the two
/// owner constants at the point of use reproduces upstream's literal
/// juxtaposition and keeps the sets - including the `\n`/`\r` that stop a value
/// from splitting the newline-terminated proto<30 wire - from drifting apart.
fn daemon_arg_needs_escape(b: u8, is_filename_arg: bool) -> bool {
    SHELL_CHARS.as_bytes().contains(&b) || (!is_filename_arg && WILD_CHARS.as_bytes().contains(&b))
}

/// Backslash-escapes a daemon argument at the byte level - the strict inverse
/// of upstream `unbackslash_arg` (`io.c:1441`, `\X -> X` for any byte X).
///
/// Mirrors upstream `safe_arg()` (`options.c:2693`):
///
/// - The arg is split at the first `=` (upstream's `opt = "--foo"` /
///   `arg = "value"` convention in `server_options()`); the `--foo=` key
///   passes through verbatim while only the value is escaped.
/// - Option values escape `WILD_CHARS` + `SHELL_CHARS`; the filename /
///   module-path form escapes only `SHELL_CHARS` so wildcards stay
///   shell-expandable ([`daemon_arg_needs_escape`]).
/// - `\` is doubled so `unbackslash_arg` recovers the literal, except in the
///   filename form where an existing `\` before a wildcard is left intact
///   (upstream `options.c:2585`) to preserve a deliberate wildcard escape.
/// - Every other byte - crucially including bytes >= 0x80 - passes through
///   untouched, exactly as upstream `safe_arg` leaves non-metacharacter bytes,
///   so a non-UTF-8 path round-trips through escape -> `unbackslash_arg`
///   byte-for-byte unchanged. All escape-set members are ASCII, so byte-level
///   membership never mis-flags a continuation byte of a multibyte sequence.
///
/// Args with neither `=` nor any escapable byte are returned verbatim to avoid
/// allocation.
fn escape_daemon_arg_bytes(arg: &[u8]) -> Vec<u8> {
    let (prefix, value, is_filename_arg): (&[u8], &[u8], bool) =
        match arg.iter().position(|&b| b == b'=') {
            Some(eq_pos) if arg.starts_with(b"--") => (&arg[..=eq_pos], &arg[eq_pos + 1..], false),
            _ => (b"", arg, true),
        };

    let needs_work = value
        .iter()
        .any(|&b| b == b'\\' || daemon_arg_needs_escape(b, is_filename_arg));
    if !needs_work {
        return arg.to_vec();
    }

    let wild = WILD_CHARS.as_bytes();
    let mut out = Vec::with_capacity(prefix.len() + value.len() + 8);
    out.extend_from_slice(prefix);
    for (i, &b) in value.iter().enumerate() {
        if b == b'\\' {
            // upstream: options.c:2585 - filename args preserve `\<wildcard>`
            // sequences verbatim so the user's deliberate wildcard escape
            // survives. Option args always double the backslash.
            let next = value.get(i + 1).copied().unwrap_or(0);
            if !(is_filename_arg && wild.contains(&next)) {
                out.push(b'\\');
            }
        } else if daemon_arg_needs_escape(b, is_filename_arg) {
            out.push(b'\\');
        }
        out.push(b);
    }
    out
}

/// Converts a [`compress::zlib::CompressionLevel`] to its signed wire value.
///
/// upstream: options.c:2922-2923 - `--compress-level=%d` forwards the signed
/// `do_compression_level`, so a negative zstd "fast" level is preserved.
fn compression_level_numeric(level: compress::zlib::CompressionLevel) -> i32 {
    use compress::zlib::CompressionLevel;
    match level {
        CompressionLevel::None => 0,
        CompressionLevel::Fast => 1,
        CompressionLevel::Default => 6,
        CompressionLevel::Best => 9,
        CompressionLevel::Precise(n) => i32::from(n.get()),
        CompressionLevel::PreciseSigned(v) => v,
    }
}

// upstream: clientserver.c carries the daemon operand path as raw `char*`
// end-to-end and io.c:send_secluded_args writes each arg's bytes verbatim, so a
// non-UTF-8 operand (a legal filename) must survive the whole daemon-arg
// emission byte-for-byte. These tests pin that on the wire; they need raw bytes
// that no `String` can represent, so they are Unix-only.
#[cfg(all(test, unix))]
mod operand_byte_fidelity_tests {
    use super::*;
    use std::os::unix::ffi::{OsStrExt, OsStringExt};

    use protocol::ProtocolVersion;

    use crate::client::ClientConfig;
    use crate::client::remote::daemon_transfer::connection::DaemonTransferRequest;

    /// The in-module path bytes `a \<0xFF>b`: an ASCII lead-in, a space, a
    /// literal backslash, a raw 0xFF (the byte a lossy `String` round-trip
    /// destroys, becoming the 3-byte U+FFFD), and a trailing ASCII byte.
    fn non_utf8_path_bytes() -> Vec<u8> {
        let mut p = b"a \\".to_vec();
        p.push(0xFF);
        p.push(b'b');
        p
    }

    fn request_with_non_utf8_path() -> DaemonTransferRequest {
        let mut operand = b"rsync://host/mod/".to_vec();
        operand.extend_from_slice(&non_utf8_path_bytes());
        DaemonTransferRequest::parse_rsync_url(&OsString::from_vec(operand), 873)
            .expect("rsync url with a non-UTF-8 path parses")
    }

    /// The parsed request keeps the operand path byte-for-byte, and the built
    /// argv's trailing operand is `mod/` + those bytes verbatim - 0xFF is never
    /// mapped to U+FFFD. A `String` hop anywhere on this path would replace
    /// 0xFF with the 3-byte U+FFFD, failing both the `.path` and the operand
    /// assertions.
    #[test]
    fn build_full_daemon_args_operand_preserves_non_utf8_bytes() {
        let request = request_with_non_utf8_path();
        assert_eq!(request.path.as_bytes(), non_utf8_path_bytes().as_slice());

        let config = ClientConfig::builder().build();
        let args = build_full_daemon_args(&config, &request, ProtocolVersion::V31, true);
        let operand = args.last().expect("argv has a trailing operand");

        let mut expected = b"mod/".to_vec();
        expected.extend_from_slice(&non_utf8_path_bytes());
        assert_eq!(operand.as_bytes(), expected.as_slice());
        assert!(operand.as_bytes().contains(&0xFF), "0xFF was lost");
        assert!(
            !operand
                .as_bytes()
                .windows(3)
                .any(|w| w == [0xEF, 0xBF, 0xBD]),
            "operand carries U+FFFD - a lossy String hop crept in"
        );
    }

    /// End-to-end golden: with protect-args on (the daemon default), the
    /// operand travels in the phase-2 secluded-args stream, which writes each
    /// arg's bytes verbatim. The emitted wire must carry `mod/a \<0xFF>b`
    /// byte-for-byte (the backslash is NOT doubled - secluded mode does not
    /// escape) and must not carry U+FFFD. Decoding the secluded frame recovers
    /// the exact operand bytes.
    #[test]
    fn secluded_wire_carries_non_utf8_operand_byte_for_byte() {
        let request = request_with_non_utf8_path();
        let config = ClientConfig::builder().protect_args(Some(true)).build();

        let mut wire = Vec::new();
        send_daemon_arguments(&mut wire, &config, &request, ProtocolVersion::V31, true)
            .expect("send daemon args");

        let mut expected_operand = b"mod/".to_vec();
        expected_operand.extend_from_slice(&non_utf8_path_bytes());

        // The operand appears verbatim, framed by a trailing NUL, in the wire.
        let mut framed = expected_operand.clone();
        framed.push(0);
        assert!(
            wire.windows(framed.len()).any(|w| w == framed.as_slice()),
            "secluded wire is missing the verbatim operand frame"
        );
        assert!(wire.contains(&0xFF), "0xFF was dropped from the wire");
        assert!(
            !wire.windows(3).any(|w| w == [0xEF, 0xBF, 0xBD]),
            "wire carries U+FFFD - the operand went through a lossy String"
        );

        // Decode the phase-2 secluded frame byte-for-byte and confirm the last
        // arg is the operand.
        let phase2 = &wire[find_phase2_start(&wire)..];
        let decoded = decode_secluded(phase2);
        assert_eq!(
            decoded.last().map(Vec::as_slice),
            Some(expected_operand.as_slice()),
            "decoded secluded operand must equal the requested path bytes"
        );
        assert_eq!(
            decoded.first().map(Vec::as_slice),
            Some(b"rsync".as_slice()),
            "the secluded payload leads with argv[0]"
        );
    }

    /// Phase 1 is a run of NUL-terminated args ended by an empty string (a lone
    /// NUL), so the first `\0\0` pair marks its end; phase 2 (the secluded
    /// frame) begins immediately after. No phase-1 arg contains an embedded
    /// NUL, so this boundary is unambiguous.
    fn find_phase2_start(wire: &[u8]) -> usize {
        wire.windows(2)
            .position(|w| w == [0, 0])
            .map(|i| i + 2)
            .expect("phase-1 ends with an empty-string NUL terminator")
    }

    /// Splits a secluded frame (null-separated args, empty-string terminator)
    /// into raw byte args, byte-for-byte. Unlike `recv_secluded_args` this does
    /// not UTF-8-decode, so it can recover a non-UTF-8 operand.
    fn decode_secluded(buf: &[u8]) -> Vec<Vec<u8>> {
        let mut args = Vec::new();
        let mut cur = Vec::new();
        for &b in buf {
            if b == 0 {
                if cur.is_empty() {
                    break;
                }
                args.push(std::mem::take(&mut cur));
            } else {
                cur.push(b);
            }
        }
        args
    }
}

#[cfg(test)]
mod safe_arg_tests {
    use super::*;

    /// Escapes an ASCII arg through the byte-generic core the wire path uses.
    /// `safe_arg_for_daemon` is a thin per-platform `OsStr` wrapper over it, so
    /// exercising the core directly keeps the grammar assertions readable.
    fn esc(arg: &str) -> String {
        String::from_utf8(escape_daemon_arg_bytes(arg.as_bytes()))
            .expect("ASCII-only escape keeps the result valid UTF-8")
    }

    // upstream: options.c:2539 safe_arg(NULL, arg) - filename args (no opt)
    // escape only SHELL_CHARS, leaving wildcards intact so the remote shell
    // can still expand them when no remote-shell wrapper is involved.
    #[test]
    fn filename_arg_leaves_wildcards_alone() {
        assert_eq!(esc("file*name"), "file*name");
        assert_eq!(esc("question?path"), "question?path");
    }

    // upstream: options.c:2539 safe_arg(NULL, arg) - SHELL_CHARS get backslash
    // escaped even in filename args.
    #[test]
    fn filename_arg_escapes_shell_chars() {
        assert_eq!(esc("file with space"), "file\\ with\\ space");
        assert_eq!(esc("dangerous;rm -rf /"), "dangerous\\;rm\\ -rf\\ /");
    }

    // upstream: options.c:2544 - option args escape WILD_CHARS + SHELL_CHARS
    // because the daemon receiver `unbackslash_arg`s before option parsing.
    #[test]
    fn option_arg_escapes_wildcards_in_value() {
        assert_eq!(esc("--groupmap=*:1234"), "--groupmap=\\*:1234");
        assert_eq!(
            esc("--usermap=alice:bob,*:1234"),
            "--usermap=alice:bob,\\*:1234"
        );
    }

    // The audit-cited regression: `--groupmap=*:1234;dangerous` must keep
    // both the wildcard and the shell-meta `;` after daemon-side
    // `unbackslash_arg()` reverses the escape.
    #[test]
    fn option_arg_round_trips_shell_meta_value() {
        let arg = "--groupmap=*:1234;dangerous";
        let escaped = esc(arg);
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
        assert_eq!(esc("--server"), "--server");
        assert_eq!(esc("-logDtprz"), "-logDtprz");
        assert_eq!(esc("."), ".");
        assert_eq!(esc("module/path"), "module/path");
    }

    // upstream: options.c:2585 - filename args preserve `\<wildcard>` so the
    // user's intentional wildcard escape passes through to the remote shell.
    #[test]
    fn filename_arg_preserves_escaped_wildcard() {
        // Filename branch: \* (literal) is kept verbatim because the wildcard
        // is already escaped by the caller.
        assert_eq!(esc("file\\*"), "file\\*");
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
        let escaped = esc("--groupmap=\\*:1234");
        assert_eq!(escaped, "--groupmap=\\\\\\*:1234");
    }

    // UTS-8.REOPEN: pin the client-side `--groupmap=*:GID` wire format.
    // Mirrors upstream `options.c:2912-2916` which calls
    // `safe_arg("--groupmap", value)` for the option-arg branch
    // (`is_filename_arg=false`, escape set = `WILD_CHARS + SHELL_CHARS`).
    // The escaped output must be reversible by the daemon's
    // `unbackslash_arg` (mirrored from upstream `io.c:1295-1306`); any drift
    // here would resurface upstream #829 for the wildcard.
    #[test]
    fn groupmap_wildcard_matches_upstream_safe_arg_byte_for_byte() {
        // upstream's safe_arg("--groupmap", "*:42") yields "--groupmap=\*:42":
        //   "--groupmap" + "=" + escape("*") + ":" + "4" + "2"
        // where escape(*) = `\*` because `*` is in WILD_CHARS.
        assert_eq!(esc("--groupmap=*:42"), "--groupmap=\\*:42");

        // Reversing the escape with the daemon-side algorithm
        // (`\X -> X` for any X) must recover the original. This is the
        // round-trip parity asserted on both sides of the wire.
        let original = "--groupmap=*:42";
        let escaped = esc(original);
        let bytes = escaped.as_bytes();
        let mut decoded = Vec::with_capacity(bytes.len());
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'\\' && i + 1 < bytes.len() {
                i += 1;
            }
            decoded.push(bytes[i]);
            i += 1;
        }
        assert_eq!(String::from_utf8(decoded).unwrap(), original);
    }

    // UTS-8.REOPEN: verify every escape character upstream `safe_arg`
    // emits for an option arg (`WILD_CHARS + SHELL_CHARS`) is backslash-escaped
    // on the wire AND survives the `safe_arg_for_daemon` -> daemon-side
    // `unbackslash_arg` round trip. Drift in either escape set would resurface
    // upstream #829 for the dropped character. The escaped-form check is the
    // load-bearing half for the delimiter bytes `\n`/`\r`: a raw newline still
    // round-trips (unbackslash of an unescaped newline is the newline), so only
    // asserting it reaches the wire prefixed with `\` catches the split.
    // Mirrors upstream `options.c:2695-2696` (`SHELL_CHARS`/`WILD_CHARS`).
    #[test]
    fn every_safe_arg_escape_char_round_trips_through_unbackslash() {
        let escape_chars = [
            '*', '?', '[', ']', '!', '#', '$', '&', ';', '|', '<', '>', '(', ')', '{', '}', '"',
            '\'', '`', ' ', '\t', '\n', '\r', '\\',
        ];
        for &ch in &escape_chars {
            let original = format!("--groupmap=prefix{ch}suffix");
            let escaped = esc(&original);
            // The metacharacter must reach the wire backslash-escaped so it can
            // never act as a shell/line delimiter on the daemon side.
            assert!(
                escaped.contains(&format!("\\{ch}")),
                "escape char {ch:?} was not backslash-escaped (got {escaped:?})",
            );
            // Reverse with the same algorithm the daemon's `unbackslash_arg`
            // uses (`\X -> X` for any X).
            let bytes = escaped.as_bytes();
            let mut decoded = Vec::with_capacity(bytes.len());
            let mut i = 0;
            while i < bytes.len() {
                if bytes[i] == b'\\' && i + 1 < bytes.len() {
                    i += 1;
                }
                decoded.push(bytes[i]);
                i += 1;
            }
            let round_trip = String::from_utf8(decoded).unwrap();
            assert_eq!(
                round_trip, original,
                "round-trip failed for {ch:?} (escaped to {escaped:?})",
            );
        }
    }

    // A newline or carriage return in an option value must be backslash-escaped
    // byte-for-byte the way upstream `safe_arg` does (`SHELL_CHARS` includes
    // `\n`/`\r`, options.c:2695). Without the escape a raw `\n` splits the
    // newline-terminated proto<30 daemon arg wire (the reader takes one arg per
    // line), letting a peer-supplied value inject a spurious arg line.
    #[test]
    fn option_value_newline_and_cr_are_escaped_like_upstream() {
        // upstream: safe_arg("--usermap", "a\nb") -> "--usermap=a\\\nb"
        assert_eq!(esc("--usermap=a\nb"), "--usermap=a\\\nb");
        assert_eq!(esc("--usermap=a\rb"), "--usermap=a\\\rb");

        // The escaped value carries no raw delimiter byte, so the proto<30
        // reader sees exactly one arg line, and unbackslash recovers the value.
        for original in ["--usermap=a\nb", "--suffix=x\r\ny"] {
            let escaped = esc(original);
            let value = escaped.split_once('=').unwrap().1;
            for (i, b) in value.bytes().enumerate() {
                if b == b'\n' || b == b'\r' {
                    assert_eq!(
                        value.as_bytes()[i - 1],
                        b'\\',
                        "unescaped delimiter in {escaped:?}",
                    );
                }
            }
            let bytes = escaped.as_bytes();
            let mut decoded = Vec::with_capacity(bytes.len());
            let mut i = 0;
            while i < bytes.len() {
                if bytes[i] == b'\\' && i + 1 < bytes.len() {
                    i += 1;
                }
                decoded.push(bytes[i]);
                i += 1;
            }
            assert_eq!(String::from_utf8(decoded).unwrap(), original);
        }
    }

    // Byte-fidelity golden. A non-UTF-8 option value carrying a raw 0xFF byte,
    // alongside a space and a literal backslash, must survive the
    // escape -> `unbackslash_arg` round trip byte-for-byte. Upstream `safe_arg`
    // (`options.c:2693`) leaves bytes >= 0x80 untouched - they are in no escape
    // set - and `unbackslash_arg` (`io.c:1441`) reverses `\X -> X` for any byte
    // X, so the pair is a strict byte-generic inverse. The previous String-typed
    // escape could not represent 0xFF at all; this pins the byte path that lets
    // a non-UTF-8 path-bearing option value (`--tmpdir=`, `--partial-dir=`, ...)
    // reach the daemon intact.
    #[test]
    fn non_utf8_option_value_round_trips_byte_for_byte() {
        // `--tmpdir=` + value bytes { 'a', ' ', '\\', 0xFF, 'x' }.
        let mut original = b"--tmpdir=a \\".to_vec();
        original.push(0xFF);
        original.push(b'x');

        let escaped = escape_daemon_arg_bytes(&original);

        // Space escaped, backslash doubled (option-arg form), 0xFF verbatim.
        assert_eq!(escaped, b"--tmpdir=a\\ \\\\\xffx".to_vec());
        assert!(escaped.contains(&0xFF), "0xFF byte was dropped or mangled");

        // Reverse exactly the way the daemon's `unbackslash_arg` does
        // (`\X -> X` for any byte X).
        let mut decoded = Vec::with_capacity(escaped.len());
        let mut i = 0;
        while i < escaped.len() {
            if escaped[i] == b'\\' && i + 1 < escaped.len() {
                i += 1;
            }
            decoded.push(escaped[i]);
            i += 1;
        }
        assert_eq!(decoded, original, "escape -> unbackslash was not identity");

        // The wire wrapper must carry the same bytes: an `OsStr` operand with a
        // 0xFF byte escapes to the identical sequence the wire path writes.
        #[cfg(unix)]
        {
            use std::os::unix::ffi::{OsStrExt, OsStringExt};
            let via_wrapper = safe_arg_for_daemon(&OsString::from_vec(original.clone()));
            assert_eq!(via_wrapper.as_bytes(), escaped.as_slice());
        }
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

#[cfg(test)]
mod server_option_fidelity_tests {
    use super::build_full_daemon_args;
    use crate::client::ClientConfig;
    use crate::client::config::StrongChecksumChoice;
    use crate::client::remote::daemon_transfer::connection::DaemonTransferRequest;
    use protocol::ProtocolVersion;

    fn request() -> DaemonTransferRequest {
        DaemonTransferRequest::parse_rsync_url(std::ffi::OsStr::new("rsync://host/mod/path"), 873)
            .expect("valid rsync url")
    }

    /// Builds the daemon argv and renders it as `Vec<String>` for these
    /// option-fidelity assertions. The builder returns `Vec<OsString>` so a
    /// non-UTF-8 operand byte survives to the wire; every arg here is ASCII, so
    /// the lossy render is exact.
    fn args(config: &ClientConfig, is_sender: bool) -> Vec<String> {
        args_at(config, ProtocolVersion::V31, is_sender)
    }

    fn args_at(config: &ClientConfig, protocol: ProtocolVersion, is_sender: bool) -> Vec<String> {
        build_full_daemon_args(config, &request(), protocol, is_sender)
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    // WHY: explicitly-set --info / --debug levels must reach the daemon peer so
    // its diagnostic output matches the client's request (upstream
    // make_output_option, options.c:2947). `del` is receiver-side, so it
    // forwards on a push (`is_sender = false`, the daemon is the receiver);
    // `send` is sender-side, so it forwards on a pull (`is_sender = true`).
    #[test]
    fn daemon_forwards_info_and_debug_when_set() {
        use std::ffi::OsString;
        let config = ClientConfig::builder()
            .info_flags([OsString::from("del1")])
            .debug_flags([OsString::from("send1")])
            .build();

        let push = args(&config, false);
        assert!(
            push.iter().any(|a| a == "--info=del"),
            "push must forward receiver-side --info=del: {push:?}"
        );

        let pull = args(&config, true);
        assert!(
            pull.iter().any(|a| a == "--debug=send"),
            "pull must forward sender-side --debug=send: {pull:?}"
        );

        // Nothing set: no --info / --debug argument at all.
        let off = ClientConfig::builder().build();
        let a = args(&off, false);
        assert!(
            !a.iter()
                .any(|x| x.starts_with("--info=") || x.starts_with("--debug=")),
            "no info/debug flags must yield no --info/--debug arg: {a:?}"
        );
    }

    // upstream: options.c has no compact 'P'; keep_partial rides long-form.
    #[test]
    fn never_packs_compact_p() {
        let config = ClientConfig::builder().partial(true).build();
        let flag = args(&config, false)
            .into_iter()
            .find(|a| a.starts_with('-') && !a.starts_with("--"))
            .unwrap_or_default();
        assert!(!flag.contains('P'), "daemon flag string packed 'P': {flag}");
    }

    // upstream: options.c:2884-2893 - bare --partial on a PUSH (daemon receiver,
    // is_sender=false) without --partial-dir; never on a PULL.
    #[test]
    fn partial_long_form_on_push_only() {
        let config = ClientConfig::builder().partial(true).build();
        let push = args(&config, false);
        assert!(
            push.iter().any(|a| a == "--partial"),
            "push must forward --partial: {push:?}"
        );
        let pull = args(&config, true);
        assert!(
            !pull.iter().any(|a| a == "--partial"),
            "pull must not forward --partial: {pull:?}"
        );
    }

    // upstream: options.c:2760-2765 - devices-without-specials sends --no-specials.
    #[test]
    fn devices_without_specials_emits_no_specials() {
        let config = ClientConfig::builder().devices(true).build();
        let a = args(&config, false);
        assert!(
            a.iter().any(|x| x == "--no-specials"),
            "expected --no-specials: {a:?}"
        );
        assert!(!a.iter().any(|x| x == "--specials"));
    }

    // upstream: options.c:2760-2765 - specials-only sends --specials.
    #[test]
    fn specials_only_emits_specials() {
        let config = ClientConfig::builder().specials(true).build();
        let a = args(&config, false);
        assert!(
            a.iter().any(|x| x == "--specials"),
            "expected --specials: {a:?}"
        );
        assert!(!a.iter().any(|x| x == "--no-specials"));
    }

    // upstream: options.c:3150 - `if (write_devices && am_sender)`. am_sender is
    // a PUSH (daemon receiver, is_sender=false); never forwarded on a PULL.
    #[test]
    fn write_devices_on_push_only() {
        let config = ClientConfig::builder().write_devices(true).build();
        let push = args(&config, false);
        assert!(
            push.iter().any(|a| a == "--write-devices"),
            "push must forward --write-devices: {push:?}"
        );
        let pull = args(&config, true);
        assert!(
            !pull.iter().any(|a| a == "--write-devices"),
            "pull must not forward --write-devices: {pull:?}"
        );
    }

    // upstream: options.c:3158 - `if (copy_devices && !am_sender)`. !am_sender is
    // a PULL (daemon sender, is_sender=true); never forwarded on a PUSH.
    #[test]
    fn copy_devices_on_pull_only() {
        let config = ClientConfig::builder().copy_devices(true).build();
        let pull = args(&config, true);
        assert!(
            pull.iter().any(|a| a == "--copy-devices"),
            "pull must forward --copy-devices: {pull:?}"
        );
        let push = args(&config, false);
        assert!(
            !push.iter().any(|a| a == "--copy-devices"),
            "push must not forward --copy-devices: {push:?}"
        );
    }

    // upstream: options.c:2747-2748 - explicit `--list-only` (list_only > 1) is
    // forwarded; the implicit single-source listing is not.
    #[test]
    fn explicit_list_only_forwarded_but_not_implicit() {
        let explicit = ClientConfig::builder()
            .list_only(true)
            .list_only_arg(true)
            .build();
        let a = args(&explicit, true);
        assert!(
            a.iter().any(|x| x == "--list-only"),
            "explicit --list-only must be forwarded: {a:?}"
        );

        let implicit = ClientConfig::builder().list_only(true).build();
        let a = args(&implicit, true);
        assert!(
            !a.iter().any(|x| x == "--list-only"),
            "implicit list-only must not be forwarded: {a:?}"
        );
    }

    // upstream: options.c:2782-2785 - `--msgs2stderr` / `--no-msgs2stderr`
    // forwarded per the tri-state; the default (None) forwards nothing.
    #[test]
    fn msgs2stderr_tri_state_forwarding() {
        let on = ClientConfig::builder().msgs2stderr(Some(true)).build();
        assert!(args(&on, false).iter().any(|a| a == "--msgs2stderr"));

        let off = ClientConfig::builder().msgs2stderr(Some(false)).build();
        assert!(args(&off, false).iter().any(|a| a == "--no-msgs2stderr"));

        let default = ClientConfig::builder().build();
        assert!(
            !args(&default, false)
                .iter()
                .any(|a| a == "--msgs2stderr" || a == "--no-msgs2stderr")
        );
    }

    // upstream: options.c:2646-2647 - `if (quiet && msgs2stderr) 'q'`. The 'q'
    // letter rides in the compact flag string.
    #[test]
    fn quiet_packs_compact_q() {
        let config = ClientConfig::builder().quiet(true).build();
        let flag = args(&config, false)
            .into_iter()
            .find(|a| a.starts_with('-') && !a.starts_with("--"))
            .unwrap_or_default();
        assert!(flag.contains('q'), "quiet must pack 'q': {flag}");

        let suppressed = ClientConfig::builder()
            .quiet(true)
            .msgs2stderr(Some(false))
            .build();
        let flag = args(&suppressed, false)
            .into_iter()
            .find(|a| a.starts_with('-') && !a.starts_with("--"))
            .unwrap_or_default();
        assert!(
            !flag.contains('q'),
            "quiet + --no-msgs2stderr must not pack 'q': {flag}"
        );
    }

    /// Extracts the compact transfer-flag argument (`-logDtpr...`) so tests can
    /// assert on the sender-only compact letters packed for a daemon push.
    fn compact_flag(config: &ClientConfig, is_sender: bool) -> String {
        args(config, is_sender)
            .into_iter()
            .find(|a| a.starts_with('-') && !a.starts_with("--"))
            .unwrap_or_default()
    }

    // upstream: options.c:2642-2643 - keep_dirlinks packs the sender-only 'K'.
    // The remote receiver must honor -K or every per-file op under a dest
    // dir-symlink is refused by the dirfd sandbox (transfer/flags.rs:508-516),
    // so the letter has to reach the daemon receiver on a push.
    #[test]
    fn keep_dirlinks_packs_k_on_push_only() {
        let config = ClientConfig::builder().keep_dirlinks(true).build();
        assert!(
            compact_flag(&config, false).contains('K'),
            "push must pack 'K': {}",
            compact_flag(&config, false)
        );
        assert!(
            !compact_flag(&config, true).contains('K'),
            "pull must not pack 'K' (local receiver applies it): {}",
            compact_flag(&config, true)
        );
    }

    // upstream: options.c:2644-2645 - prune_empty_dirs packs sender-only 'm'.
    #[test]
    fn prune_empty_dirs_packs_m_on_push_only() {
        let config = ClientConfig::builder().prune_empty_dirs(true).build();
        assert!(compact_flag(&config, false).contains('m'));
        assert!(!compact_flag(&config, true).contains('m'));
    }

    // upstream: options.c:2646-2649 - omit_dir_times 'O' and omit_link_times 'J'
    // are sender-only. The remote receiver's generator must see them to skip
    // stamping dir/symlink mtimes, so they ride the compact string on a push.
    #[test]
    fn omit_times_pack_o_and_j_on_push_only() {
        let config = ClientConfig::builder()
            .omit_dir_times(true)
            .omit_link_times(true)
            .build();
        let push = compact_flag(&config, false);
        assert!(push.contains('O'), "push must pack 'O': {push}");
        assert!(push.contains('J'), "push must pack 'J': {push}");
        let pull = compact_flag(&config, true);
        assert!(
            !pull.contains('O') && !pull.contains('J'),
            "pull omits O/J: {pull}"
        );
    }

    // upstream: options.c:2650-2654 - one 'y' per fuzzy level; 'yy' for level 2.
    // The receiver needs the fuzzy count to enable basis-file guessing.
    #[test]
    fn fuzzy_level_two_packs_yy_on_push_only() {
        let config = ClientConfig::builder().fuzzy_level(2).build();
        let push = compact_flag(&config, false);
        assert_eq!(
            push.matches('y').count(),
            2,
            "level 2 must pack exactly 'yy': {push}"
        );
        assert_eq!(compact_flag(&config, true).matches('y').count(), 0);
    }

    // upstream: options.c:2690-2693 - 'E' (preserve_executability) is packed
    // only when preserve_perms is off AND am_sender. It is the receiver's sole
    // signal to keep the executable bit when perms are not preserved.
    #[test]
    fn executability_packs_e_only_without_perms_on_push() {
        let config = ClientConfig::builder().executability(true).build();
        assert!(
            compact_flag(&config, false).contains('E'),
            "push without perms must pack 'E'"
        );
        assert!(
            !compact_flag(&config, true).contains('E'),
            "pull must not pack 'E'"
        );

        // With perms preserved, upstream packs 'p' and never 'E'.
        let with_perms = ClientConfig::builder()
            .executability(true)
            .permissions(true)
            .build();
        let flag = compact_flag(&with_perms, false);
        assert!(!flag.contains('E'), "perms on must suppress 'E': {flag}");
        assert!(flag.contains('p'), "perms on must pack 'p': {flag}");
    }

    // upstream: options.c:2953-2957 - `asprintf(&arg, "-B%u", (int)block_size)`.
    // The remote generator sizes delta blocks from this token, so both the value
    // and the SPELLING matter: this assertion previously pinned
    // `--block-size=4096`, an oc-only long form that no upstream daemon parses,
    // and that is exactly why an operator's `-B` was silently ignored on every
    // rsync:// transfer. Role-agnostic (both directions).
    #[test]
    fn block_size_forwarded_both_directions_in_the_upstream_short_spelling() {
        let size = std::num::NonZeroU32::new(4096).unwrap();
        let config = ClientConfig::builder()
            .block_size_override(Some(size))
            .build();
        for is_sender in [true, false] {
            let built = args(&config, is_sender);
            assert!(
                built.iter().any(|a| a == "-B4096"),
                "block-size must forward as upstream `-B4096` (is_sender={is_sender}): {built:?}"
            );
            assert!(
                !built.iter().any(|a| a.starts_with("--block-size")),
                "the non-upstream long spelling must be gone (is_sender={is_sender}): {built:?}"
            );
        }
        let off = ClientConfig::builder().build();
        assert!(
            !args(&off, false)
                .iter()
                .any(|a| a.starts_with("-B") || a.starts_with("--block-size")),
            "no block-size when unset"
        );
    }

    // upstream: options.c:2793-2797 - --timeout so both peers share the idle
    // deadline.
    #[test]
    fn timeout_forwarded() {
        let secs = std::num::NonZeroU64::new(60).unwrap();
        let config = ClientConfig::builder()
            .timeout(crate::client::config::TransferTimeout::Seconds(secs))
            .build();
        assert!(args(&config, false).iter().any(|a| a == "--timeout=60"));
    }

    // WHY: upstream options.c:2966 forwards `--bwlimit=%d` in whole KiB
    // (options.c:1718 `bwlimit = (size + 512) / 1024`), NOT bytes/sec. The
    // remote peer re-parses the value with a default `K` suffix (options.c:1714
    // `parse_size_arg(bwlimit_arg, 'K', ...)`), so a byte count of 1048576 would
    // be read as 1048576 KiB and the throttle would balloon 1024x. A rate of
    // 1 MiB/s (1048576 B/s) must therefore travel as `--bwlimit=1024`.
    #[test]
    fn bwlimit_forwarded_in_kib_not_bytes() {
        let limit = crate::client::config::BandwidthLimit::from_bytes_per_second(
            std::num::NonZeroU64::new(1_048_576).unwrap(),
        );
        let config = ClientConfig::builder().bandwidth_limit(Some(limit)).build();
        assert!(
            args(&config, false).iter().any(|a| a == "--bwlimit=1024"),
            "bwlimit must forward as whole KiB: {:?}",
            args(&config, false)
        );
        assert!(
            !args(&config, false)
                .iter()
                .any(|a| a == "--bwlimit=1048576"),
            "bwlimit must NOT forward the raw byte count: {:?}",
            args(&config, false)
        );
    }

    // upstream: options.c:2832-2835 - --min-size/--max-size are am_sender only;
    // the remote receiver's generator skips out-of-range files.
    #[test]
    fn min_max_size_forwarded_on_push_only() {
        let config = ClientConfig::builder()
            .min_file_size(Some(1024))
            .max_file_size(Some(1_048_576))
            .build();
        let push = args(&config, false);
        assert!(push.iter().any(|a| a == "--min-size=1024"));
        assert!(push.iter().any(|a| a == "--max-size=1048576"));
        let pull = args(&config, true);
        assert!(!pull.iter().any(|a| a.starts_with("--min-size")));
        assert!(!pull.iter().any(|a| a.starts_with("--max-size")));
    }

    // upstream: options.c:2863-2864 - --max-alloc forwarded (role-agnostic) so
    // the remote enforces the same allocation cap.
    #[test]
    fn max_alloc_forwarded() {
        let config = ClientConfig::builder()
            .max_alloc(Some(1_073_741_824))
            .build();
        assert!(
            args(&config, false)
                .iter()
                .any(|a| a == "--max-alloc=1073741824")
        );
    }

    // upstream: options.c:2873-2878 - modify_window is am_sender only; a
    // negative (nanosecond-exact) window uses the short `-@%d` spelling.
    #[test]
    fn modify_window_forwarded_on_push_only() {
        let positive = ClientConfig::builder().modify_window(Some(2)).build();
        assert!(
            args(&positive, false)
                .iter()
                .any(|a| a == "--modify-window=2")
        );
        assert!(
            !args(&positive, true)
                .iter()
                .any(|a| a.starts_with("--modify-window") || a.starts_with("-@"))
        );

        let negative = ClientConfig::builder().modify_window(Some(-1)).build();
        assert!(
            args(&negative, false).iter().any(|a| a == "-@-1"),
            "negative window uses -@N: {:?}",
            args(&negative, false)
        );
    }

    // upstream: options.c:2880-2884 - --checksum-seed shared so both sides
    // derive identical rolling/strong checksums.
    #[test]
    fn checksum_seed_forwarded() {
        let config = ClientConfig::builder().checksum_seed(Some(12345)).build();
        assert!(
            args(&config, false)
                .iter()
                .any(|a| a == "--checksum-seed=12345")
        );
    }

    // upstream: options.c:151 stores the seed in an `int` and options.c:3047
    // prints it with `"%d"`, so a negative seed goes over as `-1`. Rendering it
    // unsigned would emit `--checksum-seed=4294967295`, which the daemon's own
    // `POPT_ARG_INT` equivalent (options.c:861) rejects as an overflow.
    #[test]
    fn negative_checksum_seed_forwarded_signed() {
        let config = ClientConfig::builder().checksum_seed(Some(-1)).build();
        assert!(
            args(&config, false)
                .iter()
                .any(|a| a == "--checksum-seed=-1"),
            "expected --checksum-seed=-1: {:?}",
            args(&config, false)
        );
    }

    // upstream: options.c:2886-2894 - --partial-dir and --delay-updates are
    // am_sender only; the remote receiver stages partial/updated files there.
    #[test]
    fn partial_dir_and_delay_updates_forwarded_on_push_only() {
        let config = ClientConfig::builder()
            .partial_directory(Some(".rsync-partial"))
            .delay_updates(true)
            .build();
        let push = args(&config, false);
        assert!(
            push.iter().any(|a| a == "--partial-dir=.rsync-partial"),
            "push must forward --partial-dir: {push:?}"
        );
        assert!(push.iter().any(|a| a == "--delay-updates"));
        let pull = args(&config, true);
        assert!(!pull.iter().any(|a| a.starts_with("--partial-dir")));
        assert!(!pull.iter().any(|a| a == "--delay-updates"));
    }

    // upstream: options.c:2925-2928 - --temp-dir is am_sender only; the remote
    // receiver writes temp files under the requested directory.
    #[test]
    fn temp_dir_forwarded_on_push_only() {
        let config = ClientConfig::builder()
            .temp_directory(Some("/var/tmp/rsync"))
            .build();
        assert!(
            args(&config, false)
                .iter()
                .any(|a| a == "--temp-dir=/var/tmp/rsync")
        );
        assert!(
            !args(&config, true)
                .iter()
                .any(|a| a.starts_with("--temp-dir"))
        );
    }

    // upstream: options.c:2976-2977 - --no-implied-dirs forwarded only for a
    // relative transfer with implied dirs disabled.
    #[test]
    fn no_implied_dirs_forwarded_when_relative_and_disabled() {
        let config = ClientConfig::builder()
            .relative_paths(true)
            .implied_dirs(false)
            .build();
        assert!(
            args(&config, false)
                .iter()
                .any(|a| a == "--no-implied-dirs")
        );
        // Not relative: never forwarded even with implied dirs off.
        let non_relative = ClientConfig::builder().implied_dirs(false).build();
        assert!(
            !args(&non_relative, false)
                .iter()
                .any(|a| a == "--no-implied-dirs")
        );
    }

    // upstream: options.c:3147 - the `(!am_sender || protocol_version >= 30)`
    // half of the guard. A daemon PUSH below protocol 30 (reachable once
    // `--protocol=N` caps the `@RSYNCD:` negotiation) must not forward the
    // option to a peer that predates it; a PULL forwards it at every version.
    #[test]
    fn no_implied_dirs_gated_on_protocol_for_daemon_push() {
        let config = ClientConfig::builder()
            .relative_paths(true)
            .implied_dirs(false)
            .build();
        // The 4th argument is the REMOTE's role, not ours: `is_sender: false`
        // means the daemon is the receiver, i.e. we are the sender (a push).
        // The polarity is inverted at the top of build_full_daemon_args
        // (`let we_are_sender = !is_sender;`) - do not "correct" it here.
        let push_28 = args_at(&config, ProtocolVersion::V28, false);
        assert!(
            !push_28.iter().any(|a| a == "--no-implied-dirs"),
            "daemon push below protocol 30 must not forward --no-implied-dirs: {push_28:?}"
        );
        let pull_28 = args_at(&config, ProtocolVersion::V28, true);
        assert!(
            pull_28.iter().any(|a| a == "--no-implied-dirs"),
            "daemon pull forwards --no-implied-dirs at every protocol: {pull_28:?}"
        );
    }

    // upstream: options.c:2990-2991 - --preallocate is am_sender only.
    #[test]
    fn preallocate_forwarded_on_push_only() {
        let config = ClientConfig::builder().preallocate(true).build();
        assert!(args(&config, false).iter().any(|a| a == "--preallocate"));
        assert!(!args(&config, true).iter().any(|a| a == "--preallocate"));
    }

    // upstream: options.c:2993-2994 - --open-noatime forwarded (role-agnostic).
    #[test]
    fn open_noatime_forwarded() {
        let config = ClientConfig::builder().open_noatime(true).build();
        assert!(args(&config, false).iter().any(|a| a == "--open-noatime"));
    }

    // upstream: options.c:2815-2816 - server_options() forwards the RAW
    // --checksum-choice string with BOTH comma components. WHY: a daemon
    // receiver parses the transfer AND file sums from this string
    // (checksum.c:178-189); dropping the second component - as the old
    // transfer-only override did - silently desyncs the file-sum algorithm
    // between client and daemon.
    #[test]
    fn daemon_forwards_both_checksum_choice_components() {
        let config = ClientConfig::builder()
            .checksum_choice(StrongChecksumChoice::parse("md5,xxh3").unwrap())
            .build();
        let pull = args(&config, true);
        assert!(
            pull.iter().any(|a| a == "--checksum-choice=md5,xxh3"),
            "daemon must forward both checksum-choice components: {pull:?}"
        );
    }

    // upstream: options.c:1997-2003 - "auto,md5" is NOT nulled (only bare
    // "auto"/"auto,auto" are), so options.c:2815 forwards the full string. WHY:
    // the transfer-only override returned None for a leading auto and forwarded
    // nothing, leaving the daemon to negotiate a checksum the client never
    // resolved.
    #[test]
    fn daemon_forwards_full_string_for_auto_md5() {
        let config = ClientConfig::builder()
            .checksum_choice(StrongChecksumChoice::parse("auto,md5").unwrap())
            .build();
        let pull = args(&config, true);
        assert!(
            pull.iter().any(|a| a == "--checksum-choice=auto,md5"),
            "daemon must forward the full auto,md5 string: {pull:?}"
        );
    }

    // upstream: options.c:1997-2003 + 2815 - the fully-auto forms null
    // checksum_choice, so nothing is forwarded and the daemon negotiates.
    #[test]
    fn daemon_omits_checksum_choice_when_fully_auto() {
        let config = ClientConfig::builder().build();
        let pull = args(&config, true);
        assert!(
            !pull.iter().any(|a| a.starts_with("--checksum-choice")),
            "fully-auto must not forward --checksum-choice: {pull:?}"
        );
    }
}

#[cfg(test)]
mod oc_flag_forwarding_tests {
    use std::num::{NonZeroU8, NonZeroUsize};
    use std::path::PathBuf;

    use super::build_full_daemon_args;
    use crate::client::ClientConfig;
    use crate::client::config::TcpFastOpenMode;
    use crate::client::remote::daemon_transfer::connection::DaemonTransferRequest;
    use protocol::ProtocolVersion;

    /// Long options recognized by upstream rsync 3.4.4 (options.c
    /// long_options[]) that this arg builder may emit. Upstream's `--server`
    /// aborts the whole transfer on the first unknown option, so every `--`
    /// token the builder produces MUST appear in this allowlist. oc-invented
    /// tuning flags (--io-uring-depth, --zero-copy, ...) are local resource
    /// knobs and must never reach the peer argv.
    const UPSTREAM_SERVER_LONG_OPTS: &[&str] = &[
        "--append",
        "--append-verify",
        "--backup-dir",
        "--block-size",
        "--bwlimit",
        "--checksum-choice",
        "--checksum-seed",
        "--compare-dest",
        "--compress-choice",
        "--compress-level",
        "--copy-dest",
        "--copy-devices",
        "--copy-unsafe-links",
        "--debug",
        "--delay-updates",
        "--delete",
        "--delete-after",
        "--delete-before",
        "--delete-delay",
        "--delete-during",
        "--delete-excluded",
        "--delete-missing-args",
        "--existing",
        "--files-from",
        "--force",
        "--from0",
        "--fsync",
        "--groupmap",
        "--iconv",
        "--ignore-errors",
        "--ignore-existing",
        "--ignore-missing-args",
        "--ignore-times",
        "--info",
        "--inplace",
        "--link-dest",
        "--list-only",
        "--log-format",
        "--max-alloc",
        "--max-delete",
        "--max-size",
        "--min-size",
        "--mkpath",
        "--modify-window",
        "--msgs2stderr",
        "--new-compress",
        "--no-implied-dirs",
        "--no-msgs2stderr",
        "--no-r",
        "--no-relative",
        "--no-specials",
        "--numeric-ids",
        "--old-compress",
        "--only-write-batch",
        "--open-noatime",
        "--partial",
        "--partial-dir",
        "--preallocate",
        "--read-batch",
        "--remove-sent-files",
        "--remove-source-files",
        "--safe-links",
        "--secluded-args",
        "--sender",
        "--server",
        "--size-only",
        "--skip-compress",
        "--specials",
        "--stop-at",
        "--suffix",
        "--temp-dir",
        "--timeout",
        // upstream: options.c:2908-2909 - server_options() spells the qsort
        // request as `--use-qsort` even though the popt table entry is
        // `qsort`; we mirror the emitted spelling.
        "--use-qsort",
        "--usermap",
        "--write-batch",
        "--write-devices",
    ];

    fn request() -> DaemonTransferRequest {
        DaemonTransferRequest::parse_rsync_url(std::ffi::OsStr::new("rsync://host/mod/path"), 873)
            .expect("valid rsync url")
    }

    /// Builds the daemon argv and renders it as `Vec<String>` for these
    /// upstream-recognition assertions. Every emitted arg is ASCII, so the
    /// lossy render of the `Vec<OsString>` the builder returns is exact.
    fn built_args(config: &ClientConfig, is_sender: bool) -> Vec<String> {
        build_full_daemon_args(config, &request(), ProtocolVersion::V31, is_sender)
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    /// Sets every oc-invented tuning knob to a non-default value. These are
    /// local resource knobs with no upstream counterpart; a real upstream
    /// 3.4.4 daemon rejects any of them with an unknown-option error.
    fn kitchen_sink_config() -> ClientConfig {
        ClientConfig::builder()
            .io_uring_policy(fast_io::IoUringPolicy::Enabled)
            .io_uring_depth(Some(128))
            .cow_policy(fast_io::CowPolicy::Required)
            .zero_copy_policy(fast_io::ZeroCopyPolicy::Enabled)
            .parallel_delta_scan(true)
            .compression_threads(NonZeroU8::new(4))
            .rayon_threads(NonZeroUsize::new(8))
            .tokio_threads(NonZeroUsize::new(4))
            .sparse_detect(engine::SparseDetectStrategy::Map)
            .tcp_fastopen(TcpFastOpenMode::On)
            .spill_dir(Some(PathBuf::from("/tmp/spill")))
            .spill_threshold_bytes(Some(1024))
            .build()
    }

    fn assert_upstream_recognized(args: &[String]) {
        for arg in args {
            if let Some(rest) = arg.strip_prefix("--") {
                let name = rest.split('=').next().unwrap_or(rest);
                let long = format!("--{name}");
                assert!(
                    UPSTREAM_SERVER_LONG_OPTS.contains(&long.as_str()),
                    "`{arg}` is not an upstream rsync 3.4.4 option; an \
                     upstream `--server` peer aborts the transfer on it \
                     (full argv: {args:?})"
                );
            }
        }
    }

    #[test]
    fn oc_tuning_flags_never_reach_daemon_sender_argv() {
        let baseline = built_args(&ClientConfig::builder().build(), true);
        let args = built_args(&kitchen_sink_config(), true);
        assert_upstream_recognized(&args);
        assert_eq!(
            args, baseline,
            "oc-invented tuning knobs must not alter the daemon peer argv"
        );
    }

    #[test]
    fn oc_tuning_flags_never_reach_daemon_receiver_argv() {
        let baseline = built_args(&ClientConfig::builder().build(), false);
        let args = built_args(&kitchen_sink_config(), false);
        assert_upstream_recognized(&args);
        assert_eq!(
            args, baseline,
            "oc-invented tuning knobs must not alter the daemon peer argv"
        );
    }
}
