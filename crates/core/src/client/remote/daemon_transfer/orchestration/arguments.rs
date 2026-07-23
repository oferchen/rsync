//! Daemon argument building for client-to-server communication.
//!
//! Builds the daemon argument list mirroring upstream `server_options()` in
//! `options.c`. Supports both single-phase (plain) and two-phase (protect-args)
//! argument exchange protocols.

use std::io::Write;

use protocol::ProtocolVersion;
use transfer::setup::build_capability_string_suffix;

use crate::client::config::{
    ClientConfig, DeleteMode, IconvSetting, ReferenceDirectoryKind, TransferTimeout,
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
    let phase1_args = if protect {
        build_minimal_daemon_args(config, is_sender)
    } else {
        // upstream: options.c:2608-3015 server_options() wraps every emitted
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
        // upstream: options.c:2734-2745 - `--iconv=...` is emitted before the
        // NULL cutoff, so it already travelled in phase 1
        // (`build_minimal_daemon_args`). Skip it here to avoid sending it
        // twice; `full_args` still carries it for the non-protect single-phase
        // send path above.
        secluded.extend(
            full_args
                .iter()
                .filter(|a| !a.starts_with("--iconv="))
                .map(String::as_str),
        );
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
) -> Vec<String> {
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

    // upstream: options.c:2815-2816
    let checksum_choice = config.checksum_choice();
    if let Some(override_algo) = checksum_choice.transfer_protocol_override() {
        args.push(format!("--checksum-choice={}", override_algo.as_str()));
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
        // options.c:3036 maybe_add_e_option() - 'i' is only advertised when
        // the local side actually honors INC_RECURSE on its receive path.
        // For daemon pull (`is_sender=true` means daemon is sender; we are
        // receiver) the receiver clears CF_INC_RECURSE in compat.rs after
        // reading it. If we still advertise 'i' the daemon writes the file
        // list in INC_RECURSE format (trailing NDX_FLIST_EOF), the receiver
        // skips `receive_extra_file_lists`, and the leftover 0xFF marker
        // trips `read_varint` overflow on the next decode.
        // upstream: io.c:1816 read_varint - rejects encodings with extra > 4.
        let we_are_receiver = is_sender;
        let advertise_inc_recurse = config.inc_recursive_send() && !we_are_receiver;
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

    // upstream: options.c:2768-2780 - `if (stdout_format && am_sender)` the
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

    // upstream: options.c:2787-2791 - `-B%u` (block_size). oc mirrors the SSH
    // builder's `--block-size=` spelling; both are accepted by the daemon's
    // option parser and carry the identical value, so the remote receiver's
    // generator sizes delta blocks exactly like the client requested.
    if let Some(bs) = config.block_size_override() {
        args.push(format!("--block-size={}", bs.get()));
    }

    // upstream: options.c:2793-2797 - --timeout=N so both peers enforce the
    // same idle deadline.
    if let TransferTimeout::Seconds(secs) = config.timeout() {
        args.push(format!("--timeout={}", secs.get()));
    }

    // upstream: options.c:2799 - `--bwlimit=%d` forwards the rate in whole KiB
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

    // oc-specific: forward `--zero-copy`/`--no-zero-copy` to the daemon-sender
    // (pull) so its socket write side can opt into io_uring SEND_ZC. This is a
    // non-upstream flag with no `server_options()` counterpart, forwarded only
    // when the daemon is the sender (`is_sender`) and the user set a non-Auto
    // policy - same opt-in precedent as `--io-uring-depth`. Auto is the default
    // and is never forwarded, so the daemon keeps its byte-identical writer.
    if is_sender {
        match config.zero_copy_policy() {
            fast_io::ZeroCopyPolicy::Enabled => args.push("--zero-copy".to_owned()),
            fast_io::ZeroCopyPolicy::Disabled => args.push("--no-zero-copy".to_owned()),
            fast_io::ZeroCopyPolicy::Auto => {}
        }
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
        if let Some(depth) = config.io_uring_depth() {
            args.push(format!("--io-uring-depth={depth}"));
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

    // upstream: options.c:2996-2997 - `if (mkpath_dest_arg && am_sender)`.
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
    // is always satisfied on the daemon path (proto >= 30 for rsync:// modules),
    // so gating on relative_paths alone matches upstream. Without the
    // relative_paths gate a non-relative transfer with implied_dirs=0
    // (options.c:2207) would wrongly forward the flag, which the remote sender
    // then stats as a source path.
    if config.relative_paths() && !config.implied_dirs() {
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

/// Converts a [`compress::zlib::CompressionLevel`] to its signed wire value.
///
/// upstream: options.c:2755-2756 - `--compress-level=%d` forwards the signed
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
        assert_eq!(safe_arg_for_daemon("--groupmap=*:42"), "--groupmap=\\*:42");

        // Reversing the escape with the daemon-side algorithm
        // (`\X -> X` for any X) must recover the original. This is the
        // round-trip parity asserted on both sides of the wire.
        let original = "--groupmap=*:42";
        let escaped = safe_arg_for_daemon(original);
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
    // emits for an option arg (`WILD_CHARS + SHELL_CHARS`) survives the
    // `safe_arg_for_daemon` -> daemon-side `unbackslash_arg` round trip.
    // Drift in either escape set would resurface upstream #829 for the
    // dropped character. Mirrors upstream `options.c:2541-2544`.
    #[test]
    fn every_safe_arg_escape_char_round_trips_through_unbackslash() {
        let escape_chars = [
            '*', '?', '[', ']', '!', '#', '$', '&', ';', '|', '<', '>', '(', ')', '{', '}', '"',
            '\'', '`', ' ', '\t', '\\',
        ];
        for &ch in &escape_chars {
            let original = format!("--groupmap=prefix{ch}suffix");
            let escaped = safe_arg_for_daemon(&original);
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
    use crate::client::remote::daemon_transfer::connection::DaemonTransferRequest;
    use protocol::ProtocolVersion;

    fn request() -> DaemonTransferRequest {
        DaemonTransferRequest::parse_rsync_url("rsync://host/mod/path").expect("valid rsync url")
    }

    fn args(config: &ClientConfig, is_sender: bool) -> Vec<String> {
        build_full_daemon_args(config, &request(), ProtocolVersion::V31, is_sender)
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

    // upstream: options.c:2979 - `if (write_devices && am_sender)`. am_sender is
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

    // upstream: options.c:2987 - `if (copy_devices && !am_sender)`. !am_sender is
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

    // upstream: options.c:2787-2791 - -B/block_size must reach the remote so its
    // generator sizes delta blocks identically. Role-agnostic (both directions).
    #[test]
    fn block_size_forwarded_both_directions() {
        let size = std::num::NonZeroU32::new(4096).unwrap();
        let config = ClientConfig::builder()
            .block_size_override(Some(size))
            .build();
        for is_sender in [true, false] {
            assert!(
                args(&config, is_sender)
                    .iter()
                    .any(|a| a == "--block-size=4096"),
                "block-size must forward (is_sender={is_sender})"
            );
        }
        let off = ClientConfig::builder().build();
        assert!(
            !args(&off, false)
                .iter()
                .any(|a| a.starts_with("--block-size")),
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

    // WHY: upstream options.c:2799 forwards `--bwlimit=%d` in whole KiB
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
}

#[cfg(test)]
mod zero_copy_forwarding_tests {
    use super::build_full_daemon_args;
    use crate::client::ClientConfig;
    use crate::client::remote::daemon_transfer::connection::DaemonTransferRequest;
    use protocol::ProtocolVersion;

    fn request() -> DaemonTransferRequest {
        DaemonTransferRequest::parse_rsync_url("rsync://host/mod/path").expect("valid rsync url")
    }

    fn args_for(policy: fast_io::ZeroCopyPolicy, is_sender: bool) -> Vec<String> {
        let config = ClientConfig::builder().zero_copy_policy(policy).build();
        build_full_daemon_args(&config, &request(), ProtocolVersion::V31, is_sender)
    }

    // The daemon-sender (pull, `is_sender = true`) socket write side is the
    // only place SEND_ZC helps, so `--zero-copy` is forwarded there when the
    // user opted in. Both ends must be oc-rsync for the daemon to parse it.
    #[test]
    fn enabled_forwards_zero_copy_on_daemon_sender() {
        let args = args_for(fast_io::ZeroCopyPolicy::Enabled, true);
        assert!(
            args.iter().any(|a| a == "--zero-copy"),
            "daemon-sender pull with --zero-copy must forward the flag: {args:?}"
        );
        assert!(!args.iter().any(|a| a == "--no-zero-copy"));
    }

    // Default (Auto) must never forward the flag: the daemon then keeps its
    // byte- and behavior-identical writer. This is the HARD default-path
    // invariant, asserted at the wire-arg boundary.
    #[test]
    fn auto_never_forwards_zero_copy() {
        let args = args_for(fast_io::ZeroCopyPolicy::Auto, true);
        assert!(
            !args
                .iter()
                .any(|a| a == "--zero-copy" || a == "--no-zero-copy"),
            "Auto policy must not forward any zero-copy flag: {args:?}"
        );
    }

    // `--no-zero-copy` pins the daemon-sender's policy to Disabled explicitly.
    #[test]
    fn disabled_forwards_no_zero_copy_on_daemon_sender() {
        let args = args_for(fast_io::ZeroCopyPolicy::Disabled, true);
        assert!(
            args.iter().any(|a| a == "--no-zero-copy"),
            "Disabled policy must forward --no-zero-copy: {args:?}"
        );
        assert!(!args.iter().any(|a| a == "--zero-copy"));
    }

    // On a push (`is_sender = false`, daemon is the receiver) the daemon's
    // socket write side carries no bulk data, so SEND_ZC is not forwarded even
    // when the user opted in - matching the sender-only benefit.
    #[test]
    fn enabled_does_not_forward_on_daemon_receiver() {
        let args = args_for(fast_io::ZeroCopyPolicy::Enabled, false);
        assert!(
            !args.iter().any(|a| a == "--zero-copy"),
            "daemon-receiver push must not forward --zero-copy: {args:?}"
        );
    }
}
