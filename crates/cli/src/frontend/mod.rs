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
//! [`daemon::run`], and remote `--server` sessions are expected to execute the
//! native Rust server path rather than spawning a system `rsync` fallback. The
//! parser will continue to expand until it mirrors the full upstream surface
//! (remote modules, incremental recursion, filters, etc.), but providing these
//! entry points today allows downstream tooling to depend on a stable binary
//! path (`oc-rsync`, or `rsync` via symlink) while the native transport reaches
//! parity.
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
mod arguments;
mod command_builder;
mod execution;
mod outbuf;

#[cfg(test)]
pub(crate) use command_builder::clap_command;
#[cfg(test)]
#[allow(unused_imports)]
pub(crate) use core::client::*;
#[cfg(test)]
#[allow(unused_imports)]
pub(crate) use core::version::VersionInfoReport;
use core::{
    branding::Brand,
    message::{Message, Role},
    rsync_error,
};
use execution::execute;
use logging::MessageSink;
use outbuf::{OutbufAdapter, parse_outbuf_mode};
#[cfg(test)]
use std::collections::HashSet;
#[cfg(test)]
use std::env;
#[cfg(test)]
use std::net::IpAddr;
#[cfg(test)]
use std::path::{Path, PathBuf};
mod defaults;
mod filter_rules;
mod help;
mod out_format;
pub(crate) mod password;
mod progress;
mod server;

#[cfg(test)]
mod tests;

#[cfg(test)]
#[allow(unused_imports)]
pub(crate) use arguments::env_protect_args_default;
#[allow(unused_imports)]
pub(crate) use arguments::{
    BandwidthArgument, ParsedArgs, ProgramName, detect_program_name, parse_args,
};
#[cfg(test)]
#[allow(unused_imports)]
pub(crate) use core::branding::{self as branding};
#[cfg(test)]
#[allow(unused_imports)]
pub(crate) use core::client::{AddressMode, StrongChecksumChoice, TransferTimeout};
pub(crate) use defaults::LIST_TIMESTAMP_FORMAT;
#[cfg(test)]
pub(crate) use execution::*;
#[allow(unused_imports)]
pub(crate) use execution::{
    parse_checksum_seed_argument, parse_compress_level_argument, parse_human_readable_level,
};
#[cfg(test)]
pub(crate) use filter_rules::MergeDirective;
#[cfg(test)]
#[allow(unused_imports)]
pub(crate) use filter_rules::{
    FilterDirective, append_cvs_exclude_rules, append_filter_rules_from_files,
    apply_merge_directive, collect_filter_arguments, locate_filter_arguments,
    merge_directive_options, os_string_to_pattern, parse_filter_directive,
};
use help::help_text;
pub(crate) use out_format::{OutFormat, OutFormatContext, emit_out_format, parse_out_format};
pub(crate) use progress::*;
#[cfg(test)]
#[allow(unused_imports)]
pub(crate) use std::num::NonZeroU64;

#[cfg(test)]
pub(crate) fn load_filter_file_patterns(path: &Path) -> Result<Vec<String>, Message> {
    filter_rules::load_filter_file_patterns(path)
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

/// The function returns the process exit code that should be used by the caller.
/// On success, `0` is returned. All diagnostics are rendered using the central
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

    if server::server_mode_requested(&args) {
        return server::run_server_mode(&args, stdout, stderr);
    }

    if let Some(daemon_args) = server::daemon_mode_arguments(&args) {
        return server::run_daemon_mode(daemon_args, stdout, stderr);
    }

    if daemon_alias_requested {
        if let Some(daemon_args) = daemon_mode_arguments_for_alias(&args, brand) {
            return server::run_daemon_mode(daemon_args, stdout, stderr);
        }
    }

    let mut stderr_sink = MessageSink::with_brand(stderr, brand);
    match parse_args(args) {
        Ok(parsed) => {
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
                None => execute(parsed, stdout, &mut stderr_sink),
            }
        }
        Err(error) => {
            let mut message = rsync_error!(1, "{}", error);
            message = message.with_role(Role::Client);
            if write_message(&message, &mut stderr_sink).is_err() {
                let _ = writeln!(stderr_sink.writer_mut(), "{error}");
            }
            1
        }
    }
}

/// Converts a numeric exit code into an [`std::process::ExitCode`].
#[must_use]
pub fn exit_code_from(status: i32) -> std::process::ExitCode {
    let clamped = status.clamp(0, MAX_EXIT_CODE);
    std::process::ExitCode::from(clamped as u8)
}
