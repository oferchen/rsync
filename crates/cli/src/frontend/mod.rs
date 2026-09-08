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
// The escaper lives in `logging-sink` so the log-file sink can apply it
// without depending on the CLI: upstream escapes inside `log.c:126 logit()`,
// the single writer to `logfile_fp`, which is what makes it unbypassable.
pub(crate) use logging_sink::escape;
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
/// Upstream rsync `--itemize-changes` (`-i`) output format.
pub mod itemize;
mod local_time;
mod lsm_status;
mod operator_file;
mod out_format;
mod partial_dir;
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
    filter_rules::load_filter_file_patterns(path, false, false)
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
    filter_rules::parse_merge_modifiers_from_argument(modifiers, directive, allow_extended)
}

#[cfg(test)]
pub(crate) fn process_merge_directive(
    directive: &str,
    options: &DirMergeOptions,
    base_dir: &Path,
    display: &str,
    source: filters::RuleSource<'_>,
    destination: &mut Vec<FilterRuleSpec>,
    visited: &mut Vec<PathBuf>,
) -> Result<(), Message> {
    filter_rules::process_merge_directive(
        directive,
        options,
        base_dir,
        display,
        source,
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
    // Resolve the process umask before any mode dispatch. The daemon installs a
    // seccomp filter whose worker allowlist has no `umask(2)`, and a
    // non-allowlisted syscall is answered with EPERM, so a first read taken
    // later - inside a sandboxed worker - would cache -1 and collapse
    // `dest_mode()`'s new-file result to mode 000.
    // upstream: main.c:1877 `umask(orig_umask = umask(0));` runs in main()
    // before any privilege drop or sandbox setup.
    #[cfg(unix)]
    metadata::init_orig_umask();

    let mut args: Vec<OsString> = arguments.into_iter().map(Into::into).collect();
    if args.is_empty() {
        args.push(OsString::from(ProgramName::OcRsync.as_str()));
    }

    let detected = detect_program_name(args.first().map(|arg| arg.as_os_str()));
    let brand = detected.brand();

    let daemon_alias_requested = daemon_invoked_via_program_name(&args, brand);

    // Check for --server --daemon (remote-shell daemon mode) BEFORE plain
    // --server. upstream: main.c:1843-1844 dispatches start_daemon() when both
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
    //
    // Each top-level client invocation opens a fresh latch epoch. Upstream
    // gets this for free because `_exit_cleanup`'s statics live and die with
    // the process (cleanup.c:105-108); `run` is a reentrant library entry
    // point, so the epoch must be reset explicitly.
    core::exit_code::process_latch().reset();
    install_client_signal_handling();

    let mut stderr_sink = MessageSink::with_brand(stderr, brand);
    // Raw command-line token count (including argv[0]); `== 2` means a single
    // option token was supplied, mirroring upstream's `argc == 2` test below.
    let raw_token_count = args.len();
    let exit_code = match parse_args(args) {
        Ok(parsed) => {
            // upstream: options.c:2005 - `human_readable > 1 && argc == 2 &&
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
    // partials when an interrupt signal was received. Restricted to
    // SIGINT/SIGTERM/SIGHUP so a broken output pipe (SIGPIPE) does not rewrite
    // an otherwise-successful exit code.
    let signal_code = match core::signal::shutdown_reason() {
        Some(
            reason @ (core::signal::ShutdownReason::Interrupted
            | core::signal::ShutdownReason::Terminated
            | core::signal::ShutdownReason::HangUp),
        ) => Some(i32::from(reason.exit_code())),
        _ => None,
    };
    latched_run_exit(core::exit_code::process_latch(), exit_code, signal_code)
}

/// Resolves the run's final exit code through the process latch and claims
/// the exit for the normal return path.
///
/// The signal's code is recorded before the transfer's code because the
/// signal fired earlier in time: upstream's signal path enters
/// `_exit_cleanup(RERR_SIGNAL)` before the interrupted transfer's own exit
/// call, so RERR_SIGNAL is the first writer (upstream: cleanup.c:113-117).
/// Claiming the exit tells the signal watcher to stand down - once the run
/// has produced its final code, an abort must not `process::exit` mid-way
/// through the remaining output (upstream: cleanup.c:105 - the cleanup body
/// is single-entrant).
fn latched_run_exit(
    latch: &core::exit_code::ExitCodeLatch,
    exit_code: i32,
    signal_code: Option<i32>,
) -> i32 {
    if let Some(code) = signal_code {
        latch.record(code);
    }
    let code = latch.resolve(exit_code);
    latch.claim_exit();
    code
}

/// Decides the abort path's fate against the process exit latch.
///
/// Returns `Some(code)` when the watcher wins the claim and must terminate
/// the process, `None` when the normal return path already claimed the exit -
/// the watcher's code is latched either way, so a deferring watcher loses
/// nothing but the right to truncate the winner's output.
/// upstream: cleanup.c:113-117 (first writer wins) + cleanup.c:105 (the
/// cleanup body runs once no matter how many entrants arrive).
fn abort_exit(latch: &core::exit_code::ExitCodeLatch, code: i32) -> Option<i32> {
    let code = latch.resolve(code);
    latch.claim_exit().then_some(code)
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

/// Interval at which the signal watcher re-checks the shutdown flags.
///
/// It also bounds how long a transfer parked in a blocking transport read can
/// stay parked after the first interrupt, so it doubles as the responsiveness
/// budget for Ctrl-C during a network transfer.
const SIGNAL_WATCH_INTERVAL: std::time::Duration = std::time::Duration::from_millis(50);

/// Installs client-side signal handlers and a watcher that reacts to them
/// outside signal context.
///
/// On the first interrupt the watcher fires every registered I/O waker
/// ([`core::signal::wake_blocked_io`]), which releases a transfer blocked on
/// the transport so it observes the shutdown flag and unwinds normally -
/// finalising each in-progress `--partial` temp file through its guard. On a
/// second (abort) interrupt it finalises the partial registry directly and
/// exits with the rsync signal code, because no unwinding is going to happen.
///
/// upstream: `rsync.c:684 sig_int()` only records the signal and lets the
/// normal I/O path shut the transfer down (`io.c:750 got_kill_signal` ->
/// `handle_kill_signal` -> `cleanup.c:_exit_cleanup(RERR_SIGNAL)`).
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
                let code = core::signal::shutdown_reason()
                    .map_or(i32::from(core::exit_code::ExitCode::Signal), |r| {
                        i32::from(r.exit_code())
                    });
                if let Some(code) = abort_exit(core::exit_code::process_latch(), code) {
                    engine::CleanupManager::global().finalize_partials();
                    std::process::exit(code);
                }
                // The normal return path already claimed the exit with a
                // final code (which now includes this abort's code if the
                // latch was empty). Exiting here would cut that thread's
                // remaining output off mid-write, so the watcher stands down.
                return;
            }
            if core::signal::is_shutdown_requested() {
                // Repeated deliberately: a transfer that registers its socket
                // after the first interrupt still gets woken on the next tick.
                core::signal::wake_blocked_io();
            }
            std::thread::sleep(SIGNAL_WATCH_INTERVAL);
        }
    });
}

/// Converts a numeric exit code into an [`std::process::ExitCode`].
#[must_use]
pub fn exit_code_from(status: i32) -> std::process::ExitCode {
    let clamped = status.clamp(0, MAX_EXIT_CODE);
    std::process::ExitCode::from(clamped as u8)
}

/// Deterministic race harness for the signal watcher vs the normal return
/// path. Both arms call the exact functions the live sites call
/// ([`abort_exit`] for the watcher, [`latched_run_exit`] for `run`'s tail),
/// and each ordering is forced with a channel handoff - no sleeps, no timing
/// luck. upstream: cleanup.c:113-117 + cleanup.c:105.
#[cfg(test)]
mod exit_latch_race_tests {
    use super::{abort_exit, latched_run_exit};
    use core::exit_code::ExitCodeLatch;
    use std::sync::Arc;
    use std::sync::mpsc;
    use std::thread;

    /// RERR_SIGNAL, the watcher's code for an interrupt-driven abort.
    const SIGNAL: i32 = 20;
    /// RERR_PARTIAL, standing in for a transfer that failed on its own.
    const PARTIAL: i32 = 23;

    /// Watcher first: it wins the claim and owns termination; the normal
    /// return then repeats the latched signal code instead of its own 0.
    #[test]
    fn watcher_first_normal_return_repeats_the_latched_code() {
        let latch = Arc::new(ExitCodeLatch::new());
        let (done_tx, done_rx) = mpsc::channel();
        let watcher = {
            let latch = Arc::clone(&latch);
            thread::spawn(move || {
                let decision = abort_exit(&latch, SIGNAL);
                done_tx.send(()).expect("main thread waits on the channel");
                decision
            })
        };
        // The channel handoff forces the watcher's step to complete first.
        done_rx.recv().expect("watcher completes its step");
        let main_code = latched_run_exit(&latch, 0, None);

        let decision = watcher.join().expect("watcher thread");
        assert_eq!(decision, Some(SIGNAL), "first claimant owns termination");
        assert_eq!(main_code, SIGNAL, "normal return repeats the latched code");
    }

    /// Normal return first with an error: the watcher must defer (it would
    /// otherwise cut the final output off mid-write) and the first writer's
    /// code survives the abort's competing code.
    #[test]
    fn normal_return_first_watcher_defers_and_first_code_survives() {
        let latch = Arc::new(ExitCodeLatch::new());
        let main_code = latched_run_exit(&latch, PARTIAL, None);
        assert_eq!(main_code, PARTIAL);

        let watcher = {
            let latch = Arc::clone(&latch);
            thread::spawn(move || abort_exit(&latch, SIGNAL))
        };
        let decision = watcher.join().expect("watcher thread");
        assert_eq!(decision, None, "the watcher must not exit the process");
        assert_eq!(
            latch.resolve(0),
            PARTIAL,
            "first-writer-wins: the abort's code cannot overwrite the error"
        );
    }

    /// Normal return first with success: an abort arriving afterwards defers,
    /// so a completed run's output (e.g. --stats) cannot be truncated by the
    /// watcher's `process::exit`.
    #[test]
    fn completed_successful_run_is_not_truncated_by_a_late_abort() {
        let latch = Arc::new(ExitCodeLatch::new());
        assert_eq!(latched_run_exit(&latch, 0, None), 0);

        let watcher = {
            let latch = Arc::clone(&latch);
            thread::spawn(move || abort_exit(&latch, SIGNAL))
        };
        assert_eq!(watcher.join().expect("watcher thread"), None);
    }

    /// A signal reason observed by the run's tail is recorded before the
    /// transfer's own code, so RERR_SIGNAL wins as the temporally-first
    /// writer (upstream's signal path enters `_exit_cleanup` first).
    #[test]
    fn signal_reason_is_recorded_before_the_transfer_code() {
        let latch = ExitCodeLatch::new();
        assert_eq!(latched_run_exit(&latch, PARTIAL, Some(SIGNAL)), SIGNAL);
    }

    /// No signal anywhere: the run's own code passes through unchanged.
    #[test]
    fn plain_run_exit_codes_pass_through() {
        assert_eq!(latched_run_exit(&ExitCodeLatch::new(), 0, None), 0);
        assert_eq!(
            latched_run_exit(&ExitCodeLatch::new(), PARTIAL, None),
            PARTIAL
        );
    }
}
