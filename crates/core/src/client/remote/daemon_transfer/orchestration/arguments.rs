//! Daemon argument building for client-to-server communication.
//!
//! Builds the daemon argument list mirroring upstream `server_options()` in
//! `options.c`. Supports both single-phase (plain) and two-phase (protect-args)
//! argument exchange protocols.

use std::io::Write;
use std::net::TcpStream;

use protocol::ProtocolVersion;
use transfer::setup::build_capability_string;

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
pub(crate) fn send_daemon_arguments(
    stream: &mut TcpStream,
    config: &ClientConfig,
    request: &DaemonTransferRequest,
    protocol: ProtocolVersion,
    is_sender: bool,
) -> Result<(), ClientError> {
    let protect = config.protect_args().unwrap_or(false);

    let full_args = build_full_daemon_args(config, request, protocol, is_sender);

    // Phase 1: send args over the daemon text protocol.
    // With protect-args, only send the minimal set so the daemon detects `-s`
    // and expects a phase-2 secluded-args payload.
    // upstream: clientserver.c:393-405 - stops at the NULL marker in sargs
    let phase1_args = if protect {
        build_minimal_daemon_args(is_sender)
    } else {
        full_args.clone()
    };

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

    // Empty string signals end of phase-1 argument list.
    stream.write_all(&[terminator]).map_err(|e| {
        socket_error(
            "send final terminator to",
            request.address.socket_addr_display(),
            e,
        )
    })?;

    // Phase 2: when protect-args is active, send the real arguments via
    // the secluded-args wire format (null-separated with empty terminator).
    // upstream: clientserver.c:407-408 - send_protected_args(f_out, sargs)
    // upstream: rsync.c:283-320 - send_protected_args() applies
    // iconvbufs(ic_send, ...) per arg when --iconv is configured.
    if protect {
        let mut secluded = vec!["rsync"];
        secluded.extend(full_args.iter().map(String::as_str));
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

    // Single-character flag string (e.g., "-logDtprzc").
    // upstream: options.c:2594-2713
    let flag_string = flags::build_server_flag_string(config);
    if !flag_string.is_empty() {
        args.push(flag_string);
    }

    // Capability flags for protocol 30+.
    // upstream: options.c:2707-2713 (via maybe_add_e_option appended to argstr)
    // upstream: compat.c:177-178 - daemon checks client_info for 'i' to set allow_inc_recurse
    // INC_RECURSE ('i') is advertised in both directions by default, mirroring
    // upstream's `allow_inc_recurse = 1` initialization. `--no-inc-recursive`
    // clears the gate and suppresses the bit.
    // upstream: compat.c:720 set_allow_inc_recurse() - capability gate.
    if protocol.as_u8() >= 30 {
        args.push(build_capability_string(config.inc_recursive_send()));
    }

    let we_are_sender = !is_sender;

    // upstream: options.c:2750-2762 - server needs to know about log-format
    // so it can generate itemize output via MSG_INFO frames.
    // Only sent when client is sender (push) - matches upstream am_sender guard.
    if we_are_sender && config.itemize_changes() {
        args.push("--log-format=%i".to_owned());
    }

    // upstream: options.c:2800-2805 - compress choice forwarding.
    // Only sent when the user explicitly specified --compress-choice,
    // --new-compress, or --old-compress.
    if config.explicit_compress_choice() {
        let algo = config.compression_algorithm();
        let name = algo.name();
        match name {
            "zlibx" => args.push("--new-compress".to_owned()),
            "zlib" => args.push("--old-compress".to_owned()),
            _ => args.push(format!("--compress-choice={name}")),
        }
    }

    // --compress-level=N
    // upstream: options.c:2737-2740
    if let Some(level) = config.compression_level() {
        args.push(format!(
            "--compress-level={}",
            compression_level_numeric(level)
        ));
    }

    // Sender-specific args
    // upstream: options.c:2807-2839
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

    // Sender-only long-form args
    // upstream: options.c:2893-2925
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

        // --compare-dest=DIR, --copy-dest=DIR, --link-dest=DIR
        // upstream: options.c:2915-2923 - sent only when client is sender (push).
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

    // --files-from
    // upstream: options.c:2944-2956
    {
        use crate::client::config::FilesFromSource;
        let client_is_sender = !is_sender;
        match config.files_from() {
            FilesFromSource::None => {}
            FilesFromSource::Stdin | FilesFromSource::LocalFile(_) => {
                if !client_is_sender {
                    // Pull: daemon is sender and needs the file list from us.
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

    // --iconv forwarding to the remote daemon.
    // upstream: options.c:2716-2723 - when iconv_opt contains a comma, only the
    // post-comma half (the daemon's local charset) is forwarded; otherwise the
    // whole string is forwarded as-is. `--iconv=-` (Disabled) and the default
    // (Unspecified) forward nothing because upstream nulls iconv_opt at
    // options.c:2052-2054 before this branch runs. Without this the daemon
    // never enables `ic_recv` and writes wire UTF-8 bytes verbatim instead of
    // transcoding to its local charset.
    match config.iconv() {
        IconvSetting::Unspecified | IconvSetting::Disabled => {}
        IconvSetting::LocaleDefault => args.push("--iconv=.".to_owned()),
        IconvSetting::Explicit { local, remote } => {
            let forwarded = remote.as_deref().unwrap_or(local);
            args.push(format!("--iconv={forwarded}"));
        }
    }

    // Dummy argument (upstream requirement - represents CWD)
    args.push(".".to_owned());

    // Module path
    let module_path = format!("{}/{}", request.module, request.path);
    args.push(module_path);

    args
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
