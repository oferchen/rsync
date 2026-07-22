//! # Overview
//!
//! `cli` implements the thin command-line front-end for the Rust `rsync`
//! workspace. The crate is intentionally small: it recognises the subset of
//! command-line switches that are currently supported (`--help`/`-h`,
//! `--version`/`-V`, `--daemon`, `--server`, `--dry-run`/`-n`, `--list-only`,
//! `--delete`/`--delete-excluded`, `--filter` (supporting `+`/`-` actions, the
//! `!` clear directive, and `merge FILE` directives), `--files-from`, `--from0`,
//! `--compare-dest`, `--copy-dest`, `--link-dest`, `--bwlimit`,
//! `--append`/`--append-verify`, `--remote-option`, `--connect-program`, and
//! `--sparse`) and delegates transfer operations to
//! [`core::client::run_client`]. Daemon invocations are forwarded to
//! [`daemon::run`], while `--server` sessions surface a branded diagnostic until
//! the native server implementation is fully wired. Higher layers will
//! eventually extend the parser to cover the full upstream surface (remote
//! modules, incremental recursion, filters, etc.), but providing these entry
//! points today allows downstream tooling to depend on a stable binary path
//! (`oc-rsync`, or `rsync` via symlink) while development continues.
//!
//! # Design
//!
//! The crate exposes [`run`] as the primary entry point. The function accepts an
//! iterator of arguments together with handles for standard output and error,
//! mirroring the approach used by upstream rsync. Internally a
//! [`clap`](https://docs.rs/clap/) command definition performs a light-weight
//! parse that recognises `--help`, `--version`, `--dry-run`, `--delete`,
//! `--delete-excluded`, `--compare-dest`, `--copy-dest`, `--link-dest`,
//! `--filter`, `--files-from`, `--from0`, and `--bwlimit` flags while treating all other
//! tokens as transfer arguments. When a transfer is requested, the function
//! delegates to [`core::client::run_client`], which currently implements a
//! deterministic local copy pipeline with optional bandwidth pacing.
//!
//! # Invariants
//!
//! - `run` never panics; unexpected I/O failures surface as non-zero exit codes.
//! - Version output is delegated to [`core::version::VersionInfoReport`]
//!   so the CLI remains byte-identical with the canonical banner used by other
//!   workspace components.
//! - Help output is rendered by a dedicated helper using a static snapshot that
//!   documents the currently supported subset. The helper substitutes the
//!   invoked program name so wrappers like `oc-rsync` display branded banners
//!   while the full upstream-compatible renderer is implemented.
//! - Transfer attempts are forwarded to [`core::client::run_client`] so
//!   diagnostics and success cases remain centralised while higher-fidelity
//!   engines are developed.
//!
//! # Errors
//!
//! The parser returns a diagnostic message with exit code `1` when argument
//! processing fails. Transfer attempts surface their exit codes from
//! [`core::client::run_client`], preserving the structured diagnostics
//! emitted by the core crate.
//!
//! # Examples
//!
//! ```
//! use cli::run;
//!
//! let mut stdout = Vec::new();
//! let mut stderr = Vec::new();
//! let exit_code = run(
//!     [
//!         core::branding::client_program_name(),
//!         "--version",
//!     ],
//!     &mut stdout,
//!     &mut stderr,
//! );
//!
//! assert_eq!(exit_code, 0);
//! assert!(!stdout.is_empty());
//! assert!(stderr.is_empty());
//! ```
//!
//! # See also
//!
//! - [`core::version`] for the underlying banner rendering helpers.
//! - `src/bin/oc-rsync.rs` for the binary that wires [`run`] into `main`.

use std::ffi::OsString;
use std::io::{self, Write};
/// CLI argument parsing for the rsync frontend.
pub mod arguments;
mod command_builder;
pub(crate) mod escape;
mod execution;
pub(crate) mod outbuf;

#[cfg(test)]
pub(crate) use command_builder::clap_command;
#[cfg(test)]
pub(crate) use core::client::*;
#[cfg(test)]
pub(crate) use core::version::VersionInfoReport;
use core::{
    branding::Brand,
    message::{Message, Role, strings},
    rsync_error,
};
use execution::execute;
use logging_sink::MessageSink;
use outbuf::{OutbufAdapter, parse_outbuf_mode};
use progress::diagnostic::flush_diagnostics;
#[cfg(test)]
use std::collections::HashSet;
#[cfg(test)]
use std::env;
#[cfg(test)]
use std::net::IpAddr;
#[cfg(test)]
use std::path::{Path, PathBuf};
mod defaults;
/// Upstream rsync `--dry-run` (`-n`) output simulation.
pub mod dry_run;
mod filter_rules;
mod help;
/// Info output flags controlling informational message display.
pub mod info_output;
/// Upstream rsync `--itemize-changes` (`-i`) output format.
pub mod itemize;
mod local_time;
mod lsm_status;
mod out_format;
pub(crate) mod password;
/// Progress and verbose output helpers extracted from the CLI front-end.
pub mod progress;
/// Progress formatting for upstream rsync's `--progress` output.
pub mod progress_format;
mod server;
/// Statistics formatting for upstream rsync's `--stats` output.
pub mod stats_format;

#[cfg(test)]
mod tests;

#[cfg(test)]
pub(crate) use arguments::BandwidthArgument;
pub(crate) use arguments::{ProgramName, detect_program_name, parse_args};
#[cfg(test)]
pub(crate) use core::branding::{self as branding};
#[cfg(test)]
pub(crate) use core::client::{AddressMode, StrongChecksumChoice, TransferTimeout};
pub(crate) use defaults::LIST_TIMESTAMP_FORMAT;
#[cfg(test)]
pub(crate) use execution::*;
#[cfg(test)]
pub(crate) use filter_rules::MergeDirective;
#[cfg(test)]
pub(crate) use filter_rules::{
    FilterDirective, append_filter_rules_from_files, apply_merge_directive,
    merge_directive_options, parse_filter_directive,
};
use help::help_text;
use lsm_status::render_lsm_status;
pub(crate) use out_format::{
    OutFormat, OutFormatContext, emit_out_format, log_format_has, parse_out_format,
};
pub(crate) use progress::*;
#[cfg(test)]
pub(crate) use std::num::NonZeroU64;

#[cfg(test)]
pub(crate) fn load_filter_file_patterns(path: &Path) -> Result<Vec<String>, Message> {
    filter_rules::load_filter_file_patterns(path, false)
}

#[cfg(test)]
pub(crate) fn set_filter_stdin_input(data: Vec<u8>) {
    filter_rules::set_filter_stdin_input(data);
}

#[cfg(test)]
pub(crate) fn parse_merge_modifiers(
    modifiers: &str,
    directive: &str,
    allow_extended: bool,
) -> Result<(DirMergeOptions, bool), Message> {
    filter_rules::parse_merge_modifiers(modifiers, directive, allow_extended)
}

#[cfg(test)]
pub(crate) fn process_merge_directive(
    directive: &str,
    options: &DirMergeOptions,
    base_dir: &Path,
    display: &str,
    destination: &mut Vec<FilterRuleSpec>,
    visited: &mut HashSet<PathBuf>,
) -> Result<(), Message> {
    filter_rules::process_merge_directive(
        directive,
        options,
        base_dir,
        display,
        destination,
        visited,
    )
}

/// Maximum exit code representable by a Unix process.
const MAX_EXIT_CODE: i32 = u8::MAX as i32;

fn render_help(program_name: ProgramName) -> String {
    help_text(program_name)
}

/// Renders the `--lsm-status` diagnostic text for the invoked program.
///
/// Returns a newline-terminated multi-line summary describing the active
/// LSMs, Landlock support, seccomp state, and io_uring SQPOLL policy of
/// the current process. The leading banner uses `program_name`'s string
/// form so wrappers symlinked as `rsync` render the matching label.
fn render_lsm_status_text(program_name: ProgramName) -> String {
    render_lsm_status(program_name.as_str())
}

fn write_message<W: Write>(message: &Message, sink: &mut MessageSink<W>) -> io::Result<()> {
    sink.write(message)
}

fn daemon_invoked_via_program_name(args: &[OsString], brand: Brand) -> bool {
    let Some(program) = args.first() else {
        return false;
    };
    let profile = brand.profile();

    profile.daemon_program_name() != profile.client_program_name()
        && profile.matches_daemon_program_alias(program.as_os_str())
}

fn daemon_mode_arguments_for_alias(args: &[OsString], brand: Brand) -> Option<Vec<OsString>> {
    if args.is_empty() {
        return None;
    }

    if !daemon_invoked_via_program_name(args, brand) {
        return None;
    }

    let mut synthetic = Vec::with_capacity(args.len() + 1);
    synthetic.push(args[0].clone());
    synthetic.push(OsString::from("--daemon"));
    synthetic.extend(args.iter().skip(1).cloned());

    server::daemon_mode_arguments(&synthetic)
}

/// Runs the CLI front-end against the supplied arguments and writers.
///
/// Returns the process exit code that should be used by the caller. On success
/// `0` is returned. All diagnostics are rendered using the central
/// [`core::message`] utilities to preserve formatting and trailers.
#[allow(clippy::module_name_repetitions)]
pub fn run<I, S, Out, Err>(arguments: I, stdout: &mut Out, stderr: &mut Err) -> i32
where
    I: IntoIterator<Item = S>,
    S: Into<OsString>,
    Out: Write,
    Err: Write,
{
    let mut args: Vec<OsString> = arguments.into_iter().map(Into::into).collect();
    if args.is_empty() {
        args.push(OsString::from(ProgramName::OcRsync.as_str()));
    }

    let detected = detect_program_name(args.first().map(|arg| arg.as_os_str()));
    let brand = detected.brand();

    let daemon_alias_requested = daemon_invoked_via_program_name(&args, brand);

    // Check for --server --daemon (remote-shell daemon mode) BEFORE plain
    // --server. upstream: main.c:1867-1868 dispatches start_daemon() when both
    // am_server and am_daemon are set, before the normal server path.
    if server::server_daemon_mode_requested(&args) {
        return server::run_server_daemon_mode(&args, stderr);
    }

    if server::server_mode_requested(&args) {
        return server::run_server_mode(&args, stdout, stderr);
    }

    if let Some(daemon_args) = server::daemon_mode_arguments(&args) {
        return server::run_daemon_mode(daemon_args, stdout, stderr);
    }

    if daemon_alias_requested
        && let Some(daemon_args) = daemon_mode_arguments_for_alias(&args, brand)
    {
        return server::run_daemon_mode(daemon_args, stdout, stderr);
    }

    // Install signal handlers for the client transfer path so an interrupt
    // (SIGINT/SIGTERM/SIGHUP) finalises any in-progress --partial file and
    // exits with the rsync signal code instead of terminating abruptly.
    // upstream: main.c installs sig handlers; cleanup.c:exit_cleanup finalises
    // the partial and exits with RERR_SIGNAL.
    install_client_signal_handling();

    let mut stderr_sink = MessageSink::with_brand(stderr, brand);
    // Raw command-line token count (including argv[0]); `== 2` means a single
    // option token was supplied, mirroring upstream's `argc == 2` test below.
    let raw_token_count = args.len();
    let exit_code = match parse_args(args) {
        Ok(parsed) => {
            // upstream: options.c:2021 - `human_readable > 1 && argc == 2 &&
            // !am_server` preserves the historic meaning of a lone `-h` as
            // `--help`. When the only command-line token increments the
            // human-readable counter (`-h`, `-hh`, `-avh`, `--human-readable`),
            // rsync prints usage to stdout and exits 0 instead of treating it as
            // a number-formatting request. The server/daemon paths returned
            // above, so `!am_server` holds here.
            if raw_token_count == 2
                && matches!(
                    parsed.human_readable,
                    Some(
                        core::client::HumanReadableMode::DecimalUnits
                            | core::client::HumanReadableMode::BinaryUnits
                    )
                )
            {
                let help = render_help(parsed.program_name);
                if stdout.write_all(help.as_bytes()).is_err() {
                    let _ = writeln!(stdout, "{help}");
                }
                return 0;
            }

            let outbuf_mode = match parsed.outbuf.as_ref() {
                Some(value) => match parse_outbuf_mode(value.as_os_str()) {
                    Ok(mode) => Some(mode),
                    Err(message) => {
                        if write_message(&message, &mut stderr_sink).is_err() {
                            let _ = writeln!(stderr_sink.writer_mut(), "{message}");
                        }
                        return 1;
                    }
                },
                None => None,
            };

            match outbuf_mode {
                Some(mode) => {
                    let mut adapter = OutbufAdapter::new(stdout, mode);
                    let exit_code = execute(parsed, &mut adapter, &mut stderr_sink);
                    // Honour the workflow's resolved --msgs-to-stderr setting
                    // for any leftover Info events. Hardcoding `true` here
                    // routed every FINFO message (e.g. the --info=backup
                    // notice from the local-copy executor) to stderr even
                    // when the CLI default expected stdout, breaking
                    // upstream tests that grep `$outfile` (stdout only).
                    let msgs_to_stderr = progress::diagnostic::msgs_to_stderr();
                    let _ =
                        flush_diagnostics(&mut adapter, stderr_sink.writer_mut(), msgs_to_stderr);
                    if let Err(error) = adapter.flush() {
                        let message =
                            rsync_error!(1, "failed to flush stdout: {error}", error = error)
                                .with_role(Role::Client);
                        if write_message(&message, &mut stderr_sink).is_err() {
                            let _ = writeln!(stderr_sink.writer_mut(), "{message}");
                        }
                        1
                    } else {
                        exit_code
                    }
                }
                None => {
                    let exit_code = execute(parsed, stdout, &mut stderr_sink);
                    let msgs_to_stderr = progress::diagnostic::msgs_to_stderr();
                    let _ = flush_diagnostics(stdout, stderr_sink.writer_mut(), msgs_to_stderr);
                    exit_code
                }
            }
        }
        Err(error) => {
            let code = clap_parse_error_exit_code(&error);
            let detail = clap_error_detail(&error);
            let mut message = strings::exit_code_message_with_detail(code, detail.as_str())
                .unwrap_or_else(|| rsync_error!(code, "{}", detail));
            message = message.with_role(Role::Client);
            if write_message(&message, &mut stderr_sink).is_err() {
                let _ = writeln!(stderr_sink.writer_mut(), "{detail}");
            }
            code
        }
    };

    // upstream: cleanup.c:exit_cleanup exits with RERR_SIGNAL after finalising
    // partials when an interrupt signal was received. Override whatever the
    // interrupted transfer returned with the signal's exit code. Restricted to
    // SIGINT/SIGTERM/SIGHUP so a broken output pipe (SIGPIPE) does not rewrite
    // an otherwise-successful exit code.
    match core::signal::shutdown_reason() {
        Some(
            reason @ (core::signal::ShutdownReason::Interrupted
            | core::signal::ShutdownReason::Terminated
            | core::signal::ShutdownReason::HangUp),
        ) => i32::from(reason.exit_code()),
        _ => exit_code,
    }
}

/// Renders a `clap` parse failure into the detail text used to compose an
/// rsync-style diagnostic, stripping clap's own leading `error: ` header.
///
/// `clap::Error`'s `Display` prepends a stock `error: ` prefix to every
/// message. oc then wraps the detail with the canonical `rerr_names` category
/// (e.g. `syntax or usage error: `), so leaving clap's prefix in place doubles
/// the wording into `syntax or usage error: error: ...`. Upstream rsync emits
/// the category exactly once, so the redundant clap header is removed here at
/// the single rendering site. The match is intentionally exact and
/// case-sensitive: clap is built without the `color` feature, so the header is
/// always the literal `error: ` with no ANSI styling.
fn clap_error_detail(error: &clap::Error) -> String {
    let rendered = error.to_string();
    match rendered.strip_prefix("error: ") {
        Some(stripped) => stripped.to_owned(),
        None => rendered,
    }
}

/// Maps a `clap` argument-parse failure to an rsync exit code.
///
/// Most usage errors map to `RERR_SYNTAX` (1). An unusable `--checksum-choice`
/// name is the exception: upstream `checksum.c:139 parse_checksum_choice()`
/// exits with `RERR_UNSUPPORTED` (4, errcode.h:28). `--checksum-choice` is
/// validated inside the `clap` value flow (unlike `--compress-choice`, which is
/// validated later in the pipeline where the message code survives), so its
/// intended exit code is reconstructed here from the diagnostic text we emit.
fn clap_parse_error_exit_code(error: &clap::Error) -> i32 {
    if error.kind() == clap::error::ErrorKind::ValueValidation
        && error.to_string().contains("--checksum-choice")
    {
        4
    } else {
        1
    }
}

/// Installs client-side signal handlers and a watcher that, on a second
/// (abort) interrupt, finalises in-progress `--partial` temp files and exits
/// with the rsync signal code. A single interrupt is handled gracefully by the
/// copy loop, which stops mid-file and lets each transfer's guard finalise its
/// partial during normal unwinding.
fn install_client_signal_handling() {
    use std::sync::atomic::{AtomicBool, Ordering};
    static INSTALLED: AtomicBool = AtomicBool::new(false);
    if INSTALLED.swap(true, Ordering::SeqCst) {
        return;
    }
    // A failure here is non-fatal: the transfer still runs, just without
    // graceful partial finalisation on signal.
    let _ = core::signal::install_signal_handlers();
    std::thread::spawn(|| {
        loop {
            if core::signal::is_abort_requested() {
                engine::CleanupManager::global().finalize_partials();
                let code = core::signal::shutdown_reason()
                    .map_or(i32::from(core::exit_code::ExitCode::Signal), |r| {
                        i32::from(r.exit_code())
                    });
                std::process::exit(code);
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    });
}

/// Converts a numeric exit code into an [`std::process::ExitCode`].
#[must_use]
pub fn exit_code_from(status: i32) -> std::process::ExitCode {
    let clamped = status.clamp(0, MAX_EXIT_CODE);
    std::process::ExitCode::from(clamped as u8)
}
