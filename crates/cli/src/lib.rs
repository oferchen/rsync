#![deny(unsafe_code)]
#![deny(missing_docs)]
#![deny(rustdoc::broken_intra_doc_links)]

//! # Overview
//!
//! `rsync_cli` implements the thin command-line front-end for the Rust `oc-rsync`
//! workspace. The crate is intentionally small: it recognises the subset of
//! command-line switches that are currently supported (`--help`/`-h`,
//! `--version`/`-V`, `--dry-run`/`-n`, `--delete`, and `--bwlimit`) and delegates local copy operations to
//! [`rsync_core::client::run_client`]. Higher layers will eventually extend the
//! parser to cover the full upstream surface (remote modules, incremental
//! recursion, filters, etc.), but providing these entry points today allows
//! downstream tooling to depend on a stable binary path (`oc-rsync`) while
//! development continues.
//!
//! # Design
//!
//! The crate exposes [`run`] as the primary entry point. The function accepts an
//! iterator of arguments together with handles for standard output and error,
//! mirroring the approach used by upstream rsync. Internally a
//! [`clap`](https://docs.rs/clap/) command definition performs a light-weight
//! parse that recognises `--help`, `--version`, `--dry-run`, `--delete`, and `--bwlimit` flags while treating all other
//! tokens as transfer arguments. When a transfer is requested, the function
//! delegates to [`rsync_core::client::run_client`], which currently implements a
//! deterministic local copy pipeline with optional bandwidth pacing.
//!
//! # Invariants
//!
//! - `run` never panics; unexpected I/O failures surface as non-zero exit codes.
//! - Version output is delegated to [`rsync_core::version::VersionInfoReport`]
//!   so the CLI remains byte-identical with the canonical banner used by other
//!   workspace components.
//! - Help output is rendered by [`render_help`] using a static snapshot that
//!   documents the currently supported subset. This keeps the wording stable
//!   while the full upstream-compatible renderer is implemented.
//! - Transfer attempts are forwarded to [`rsync_core::client::run_client`] so
//!   diagnostics and success cases remain centralised while higher-fidelity
//!   engines are developed.
//!
//! # Errors
//!
//! The parser returns a diagnostic message with exit code `1` when argument
//! processing fails. Transfer attempts surface their exit codes from
//! [`rsync_core::client::run_client`], preserving the structured diagnostics
//! emitted by the core crate.
//!
//! # Examples
//!
//! ```
//! use rsync_cli::run;
//!
//! let mut stdout = Vec::new();
//! let mut stderr = Vec::new();
//! let exit_code = run(["oc-rsync", "--version"], &mut stdout, &mut stderr);
//!
//! assert_eq!(exit_code, 0);
//! assert!(!stdout.is_empty());
//! assert!(stderr.is_empty());
//! ```
//!
//! # See also
//!
//! - [`rsync_core::version`] for the underlying banner rendering helpers.
//! - `bin/oc-rsync` for the binary crate that wires [`run`] into `main`.

use std::ffi::{OsStr, OsString};
use std::io::{self, Write};
use std::num::NonZeroU64;

use clap::{Arg, ArgAction, Command, builder::OsStringValueParser};
use rsync_core::{
    client::{
        BandwidthLimit, ClientConfig, ModuleListRequest, run_client as run_core_client,
        run_module_list,
    },
    message::{Message, Role},
    rsync_error,
    version::VersionInfoReport,
};
use rsync_logging::MessageSink;

/// Maximum exit code representable by a Unix process.
const MAX_EXIT_CODE: i32 = u8::MAX as i32;

/// Deterministic help text describing the CLI surface supported by this build.
const HELP_TEXT: &str = concat!(
    "oc-rsync 3.4.1-rust\n",
    "https://github.com/oferchen/rsync\n",
    "\n",
    "Usage: oc-rsync [-h] [-V] [-n] [-a] [--delete] [--bwlimit=RATE] SOURCE... DEST\n",
    "\n",
    "This development snapshot implements deterministic local filesystem\n",
    "copies for regular files, directories, and symbolic links. The\n",
    "following options are recognised:\n",
    "  -h, --help       Show this help message and exit.\n",
    "  -V, --version    Output version information and exit.\n",
    "  -n, --dry-run    Validate transfers without modifying the destination.\n",
    "  -a, --archive    Enable archive mode (implies --owner and --group).\n",
    "      --delete     Remove destination files that are absent from the source.\n",
    "      --bwlimit    Limit I/O bandwidth in KiB/s (0 disables the limit).\n",
    "      --owner      Preserve file ownership (requires super-user).\n",
    "      --group      Preserve file group (requires suitable privileges).\n",
    "\n",
    "All SOURCE operands must reside on the local filesystem. When multiple\n",
    "sources are supplied, DEST must name a directory. Metadata preservation\n",
    "covers permissions, timestamps, and optional ownership metadata.\n",
);

/// Human-readable list of the options recognised by this development build.
const SUPPORTED_OPTIONS_LIST: &str = "--help/-h, --version/-V, --dry-run/-n, --archive/-a, --delete, --bwlimit, --owner, and --group";

/// Parsed command produced by [`parse_args`].
#[derive(Debug, Default)]
struct ParsedArgs {
    show_help: bool,
    show_version: bool,
    dry_run: bool,
    archive: bool,
    delete: bool,
    remainder: Vec<OsString>,
    bwlimit: Option<OsString>,
    owner: bool,
    group: bool,
}

/// Builds the `clap` command used for parsing.
fn clap_command() -> Command {
    Command::new("oc-rsync")
        .disable_help_flag(true)
        .disable_version_flag(true)
        .arg_required_else_help(false)
        .arg(
            Arg::new("help")
                .long("help")
                .short('h')
                .help("Show this help message and exit.")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("version")
                .long("version")
                .short('V')
                .help("Output version information and exit.")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("dry-run")
                .long("dry-run")
                .short('n')
                .help("Validate transfers without modifying the destination.")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("archive")
                .long("archive")
                .short('a')
                .help("Enable archive mode (implies --owner and --group).")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("delete")
                .long("delete")
                .help("Remove destination files that are absent from the source.")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("owner")
                .long("owner")
                .short('o')
                .help("Preserve file ownership (requires super-user).")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("group")
                .long("group")
                .short('g')
                .help("Preserve file group (requires suitable privileges).")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("bwlimit")
                .long("bwlimit")
                .value_name("RATE")
                .help("Limit I/O bandwidth in KiB/s (0 disables the limit).")
                .num_args(1)
                .action(ArgAction::Set)
                .value_parser(OsStringValueParser::new()),
        )
        .arg(
            Arg::new("args")
                .action(ArgAction::Append)
                .num_args(0..)
                .allow_hyphen_values(true)
                .trailing_var_arg(true)
                .value_parser(OsStringValueParser::new()),
        )
}

/// Parses command-line arguments into a [`ParsedArgs`] structure.
fn parse_args<I, S>(arguments: I) -> Result<ParsedArgs, clap::Error>
where
    I: IntoIterator<Item = S>,
    S: Into<OsString>,
{
    let mut args: Vec<OsString> = arguments.into_iter().map(Into::into).collect();

    if args.is_empty() {
        args.push(OsString::from("oc-rsync"));
    }

    let mut matches = clap_command().try_get_matches_from(args)?;

    let show_help = matches.get_flag("help");
    let show_version = matches.get_flag("version");
    let dry_run = matches.get_flag("dry-run");
    let archive = matches.get_flag("archive");
    let delete = matches.get_flag("delete");
    let owner = matches.get_flag("owner");
    let group = matches.get_flag("group");
    let remainder = matches
        .remove_many::<OsString>("args")
        .map(|values| values.collect())
        .unwrap_or_default();
    let bwlimit = matches
        .remove_one::<OsString>("bwlimit")
        .map(|value| value.into());

    Ok(ParsedArgs {
        show_help,
        show_version,
        dry_run,
        archive,
        delete,
        remainder,
        bwlimit,
        owner,
        group,
    })
}

/// Renders the help text describing the currently supported options.
fn render_help() -> String {
    HELP_TEXT.to_string()
}

/// Writes a [`Message`] to the supplied sink, appending a newline.
fn write_message<W: Write>(message: &Message, sink: &mut MessageSink<W>) -> io::Result<()> {
    sink.write(message)
}

/// Runs the CLI using the provided argument iterator and output handles.
///
/// The function returns the process exit code that should be used by the caller.
/// On success, `0` is returned. All diagnostics are rendered using the central
/// [`rsync_core::message`] utilities to preserve formatting and trailers.
#[allow(clippy::module_name_repetitions)]
pub fn run<I, S, Out, Err>(arguments: I, stdout: &mut Out, stderr: &mut Err) -> i32
where
    I: IntoIterator<Item = S>,
    S: Into<OsString>,
    Out: Write,
    Err: Write,
{
    let mut stderr_sink = MessageSink::new(stderr);
    match parse_args(arguments) {
        Ok(parsed) => execute(parsed, stdout, &mut stderr_sink),
        Err(error) => {
            let mut message = rsync_error!(1, "{}", error);
            message = message.with_role(Role::Client);
            if write_message(&message, &mut stderr_sink).is_err() {
                let _ = writeln!(stderr_sink.writer_mut(), "{}", error);
            }
            1
        }
    }
}

fn execute<Out, Err>(parsed: ParsedArgs, stdout: &mut Out, stderr: &mut MessageSink<Err>) -> i32
where
    Out: Write,
    Err: Write,
{
    if parsed.show_help {
        let help = render_help();
        if stdout.write_all(help.as_bytes()).is_err() {
            let _ = writeln!(stdout, "{help}");
            return 1;
        }
        return 0;
    }

    if parsed.show_version && parsed.remainder.is_empty() {
        let report = VersionInfoReport::default();
        let banner = report.human_readable();
        if stdout.write_all(banner.as_bytes()).is_err() {
            return 1;
        }
        return 0;
    }

    let remainder = match extract_operands(parsed.remainder) {
        Ok(operands) => operands,
        Err(unsupported) => {
            let message = unsupported.to_message();
            let fallback = unsupported.fallback_text();
            if write_message(&message, stderr).is_err() {
                let _ = writeln!(stderr.writer_mut(), "{fallback}");
            }
            return 1;
        }
    };

    let bandwidth_limit = match parsed.bwlimit {
        Some(ref value) => match parse_bandwidth_limit(value) {
            Ok(limit) => limit,
            Err(message) => {
                if write_message(&message, stderr).is_err() {
                    let _ = writeln!(stderr.writer_mut(), "{}", message);
                }
                return 1;
            }
        },
        None => None,
    };

    match ModuleListRequest::from_operands(&remainder) {
        Ok(Some(request)) => {
            return match run_module_list(request) {
                Ok(list) => {
                    if render_module_list(stdout, &list).is_err() {
                        1
                    } else {
                        0
                    }
                }
                Err(error) => {
                    if write_message(error.message(), stderr).is_err() {
                        let _ = writeln!(
                            stderr.writer_mut(),
                            "rsync error: daemon functionality is unavailable in this build (code {})",
                            error.exit_code()
                        );
                    }
                    error.exit_code()
                }
            };
        }
        Ok(None) => {}
        Err(error) => {
            if write_message(error.message(), stderr).is_err() {
                let _ = writeln!(stderr.writer_mut(), "{}", error);
            }
            return error.exit_code();
        }
    }

    let config = ClientConfig::builder()
        .transfer_args(remainder)
        .dry_run(parsed.dry_run)
        .delete(parsed.delete)
        .bandwidth_limit(bandwidth_limit)
        .owner(parsed.owner || parsed.archive)
        .group(parsed.group || parsed.archive)
        .build();

    match run_core_client(config) {
        Ok(()) => 0,
        Err(error) => {
            if write_message(error.message(), stderr).is_err() {
                let _ = writeln!(
                    stderr.writer_mut(),
                    "rsync error: client functionality is unavailable in this build (code 1)",
                );
            }
            error.exit_code()
        }
    }
}

/// Converts a numeric exit code into an [`std::process::ExitCode`].
#[must_use]
pub fn exit_code_from(status: i32) -> std::process::ExitCode {
    let clamped = status.clamp(0, MAX_EXIT_CODE);
    std::process::ExitCode::from(clamped as u8)
}

#[derive(Debug)]
struct UnsupportedOption {
    option: OsString,
}

impl UnsupportedOption {
    fn new(option: OsString) -> Self {
        Self { option }
    }

    fn to_message(&self) -> Message {
        let option = self.option.to_string_lossy();
        let text = format!(
            "unsupported option '{}': this build currently supports only {}",
            option, SUPPORTED_OPTIONS_LIST
        );
        rsync_error!(1, text).with_role(Role::Client)
    }

    fn fallback_text(&self) -> String {
        format!(
            "unsupported option '{}': this build currently supports only {}",
            self.option.to_string_lossy(),
            SUPPORTED_OPTIONS_LIST
        )
    }
}

fn is_option(argument: &OsStr) -> bool {
    let text = argument.to_string_lossy();
    let mut chars = text.chars();
    matches!(chars.next(), Some('-')) && chars.next().is_some()
}

fn extract_operands(arguments: Vec<OsString>) -> Result<Vec<OsString>, UnsupportedOption> {
    let mut operands = Vec::new();
    let mut accept_everything = false;

    for argument in arguments {
        if !accept_everything {
            if argument == "--" {
                accept_everything = true;
                continue;
            }

            if is_option(argument.as_os_str()) {
                return Err(UnsupportedOption::new(argument));
            }
        }

        operands.push(argument);
    }

    Ok(operands)
}

fn parse_bandwidth_limit(argument: &OsStr) -> Result<Option<BandwidthLimit>, Message> {
    let text = argument.to_string_lossy();
    match parse_bwlimit_bytes(&text) {
        Ok(Some(bytes)) => Ok(Some(BandwidthLimit::from_bytes_per_second(
            NonZeroU64::new(bytes).expect("bandwidth limit must be non-zero"),
        ))),
        Ok(None) => Ok(None),
        Err(BandwidthParseError::Invalid) => {
            Err(rsync_error!(1, "--bwlimit={} is invalid", text).with_role(Role::Client))
        }
        Err(BandwidthParseError::TooSmall) => Err(rsync_error!(
            1,
            "--bwlimit={} is too small (min: 512 or 0 for unlimited)",
            text
        )
        .with_role(Role::Client)),
        Err(BandwidthParseError::TooLarge) => {
            Err(rsync_error!(1, "--bwlimit={} is too large", text).with_role(Role::Client))
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BandwidthParseError {
    Invalid,
    TooSmall,
    TooLarge,
}

fn parse_bwlimit_bytes(text: &str) -> Result<Option<u64>, BandwidthParseError> {
    if text.is_empty() {
        return Err(BandwidthParseError::Invalid);
    }

    let mut digits_seen = false;
    let mut decimal_seen = false;
    let mut numeric_end = text.len();

    for (index, ch) in text.char_indices() {
        if ch.is_ascii_digit() {
            digits_seen = true;
            continue;
        }

        if (ch == '.' || ch == ',') && !decimal_seen {
            decimal_seen = true;
            continue;
        }

        numeric_end = index;
        break;
    }

    let numeric_part = &text[..numeric_end];
    let remainder = &text[numeric_end..];

    if !digits_seen || numeric_part == "." || numeric_part == "," {
        return Err(BandwidthParseError::Invalid);
    }

    let normalized_numeric = numeric_part.replace(',', ".");
    let numeric_value: f64 = normalized_numeric
        .parse()
        .map_err(|_| BandwidthParseError::Invalid)?;

    let (suffix, mut remainder_after_suffix) =
        if remainder.is_empty() || remainder.starts_with('+') || remainder.starts_with('-') {
            ('K', remainder)
        } else {
            let mut chars = remainder.chars();
            let ch = chars.next().unwrap();
            (ch, chars.as_str())
        };

    let repetitions = match suffix.to_ascii_lowercase() {
        'b' => 0,
        'k' => 1,
        'm' => 2,
        'g' => 3,
        't' => 4,
        'p' => 5,
        _ => return Err(BandwidthParseError::Invalid),
    };

    let mut base: f64 = 1024.0;

    if !remainder_after_suffix.is_empty() {
        let bytes = remainder_after_suffix.as_bytes();
        match bytes[0] {
            b'b' | b'B' => {
                base = 1000.0;
                remainder_after_suffix = &remainder_after_suffix[1..];
            }
            b'i' | b'I' => {
                if bytes.len() < 2 {
                    return Err(BandwidthParseError::Invalid);
                }
                if matches!(bytes[1], b'b' | b'B') {
                    base = 1024.0;
                    remainder_after_suffix = &remainder_after_suffix[2..];
                } else {
                    return Err(BandwidthParseError::Invalid);
                }
            }
            b'+' | b'-' => {}
            _ => return Err(BandwidthParseError::Invalid),
        }
    }

    let mut adjust = 0.0f64;
    if !remainder_after_suffix.is_empty() {
        if remainder_after_suffix == "+1" && numeric_end > 0 {
            adjust = 1.0;
            remainder_after_suffix = "";
        } else if remainder_after_suffix == "-1" && numeric_end > 0 {
            adjust = -1.0;
            remainder_after_suffix = "";
        }
    }

    if !remainder_after_suffix.is_empty() {
        return Err(BandwidthParseError::Invalid);
    }

    let scale = match repetitions {
        0 => 1.0,
        reps => base.powi(reps as i32),
    };

    let mut size = numeric_value * scale;
    if !size.is_finite() {
        return Err(BandwidthParseError::TooLarge);
    }
    size += adjust;
    if !size.is_finite() {
        return Err(BandwidthParseError::TooLarge);
    }

    let truncated = size.trunc();
    if truncated < 0.0 || truncated > u128::MAX as f64 {
        return Err(BandwidthParseError::TooLarge);
    }

    let bytes = truncated as u128;

    if bytes == 0 {
        return Ok(None);
    }

    if bytes < 512 {
        return Err(BandwidthParseError::TooSmall);
    }

    let rounded = bytes
        .checked_add(512)
        .ok_or(BandwidthParseError::TooLarge)?
        / 1024;
    let rounded_bytes = rounded
        .checked_mul(1024)
        .ok_or(BandwidthParseError::TooLarge)?;

    let bytes_u64 = u64::try_from(rounded_bytes).map_err(|_| BandwidthParseError::TooLarge)?;
    Ok(Some(bytes_u64))
}

fn render_module_list<W: Write>(
    writer: &mut W,
    list: &rsync_core::client::ModuleList,
) -> io::Result<()> {
    for line in list.motd_lines() {
        writeln!(writer, "{}", line)?;
    }

    for entry in list.entries() {
        if let Some(comment) = entry.comment() {
            writeln!(writer, "{}\t{}", entry.name(), comment)?;
        } else {
            writeln!(writer, "{}", entry.name())?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;
    use std::io::{BufRead, BufReader, Write};
    use std::net::{TcpListener, TcpStream};
    use std::thread;
    use std::time::Duration;

    fn run_with_args<I, S>(args: I) -> (i32, Vec<u8>, Vec<u8>)
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let code = run(args, &mut stdout, &mut stderr);
        (code, stdout, stderr)
    }

    #[test]
    fn version_flag_renders_report() {
        let (code, stdout, stderr) =
            run_with_args([OsStr::new("oc-rsync"), OsStr::new("--version")]);

        assert_eq!(code, 0);
        assert!(stderr.is_empty());

        let expected = VersionInfoReport::default().human_readable();
        assert_eq!(stdout, expected.into_bytes());
    }

    #[test]
    fn short_version_flag_renders_report() {
        let (code, stdout, stderr) = run_with_args([OsStr::new("oc-rsync"), OsStr::new("-V")]);

        assert_eq!(code, 0);
        assert!(stderr.is_empty());

        let expected = VersionInfoReport::default().human_readable();
        assert_eq!(stdout, expected.into_bytes());
    }

    #[test]
    fn help_flag_renders_static_help_snapshot() {
        let (code, stdout, stderr) = run_with_args([OsStr::new("oc-rsync"), OsStr::new("--help")]);

        assert_eq!(code, 0);
        assert!(stderr.is_empty());

        let expected = render_help();
        assert_eq!(stdout, expected.into_bytes());
    }

    #[test]
    fn short_help_flag_renders_static_help_snapshot() {
        let (code, stdout, stderr) = run_with_args([OsStr::new("oc-rsync"), OsStr::new("-h")]);

        assert_eq!(code, 0);
        assert!(stderr.is_empty());

        let expected = render_help();
        assert_eq!(stdout, expected.into_bytes());
    }

    #[test]
    fn transfer_request_reports_missing_operands() {
        let (code, stdout, stderr) = run_with_args([OsString::from("oc-rsync")]);

        assert_eq!(code, 1);
        assert!(stdout.is_empty());

        let rendered = String::from_utf8(stderr).expect("diagnostic is valid UTF-8");
        assert!(rendered.contains("missing source operands"));
        assert!(rendered.contains("[client=3.4.1-rust]"));
    }

    #[test]
    fn transfer_request_copies_file() {
        use tempfile::tempdir;

        let tmp = tempdir().expect("tempdir");
        let source = tmp.path().join("source.txt");
        let destination = tmp.path().join("destination.txt");
        std::fs::write(&source, b"cli copy").expect("write source");

        let (code, stdout, stderr) = run_with_args([
            OsString::from("oc-rsync"),
            source.clone().into_os_string(),
            destination.clone().into_os_string(),
        ]);

        assert_eq!(code, 0);
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());
        assert_eq!(
            std::fs::read(destination).expect("read destination"),
            b"cli copy"
        );
    }

    #[test]
    fn transfer_request_with_archive_copies_file() {
        use tempfile::tempdir;

        let tmp = tempdir().expect("tempdir");
        let source = tmp.path().join("source.txt");
        let destination = tmp.path().join("destination.txt");
        std::fs::write(&source, b"archive").expect("write source");

        let (code, stdout, stderr) = run_with_args([
            OsString::from("oc-rsync"),
            OsString::from("-a"),
            source.clone().into_os_string(),
            destination.clone().into_os_string(),
        ]);

        assert_eq!(code, 0);
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());
        assert_eq!(
            std::fs::read(destination).expect("read destination"),
            b"archive"
        );
    }

    #[test]
    fn transfer_request_with_bwlimit_copies_file() {
        use tempfile::tempdir;

        let tmp = tempdir().expect("tempdir");
        let source = tmp.path().join("source.txt");
        let destination = tmp.path().join("destination.txt");
        std::fs::write(&source, b"limited").expect("write source");

        let (code, stdout, stderr) = run_with_args([
            OsString::from("oc-rsync"),
            OsString::from("--bwlimit=2048"),
            source.clone().into_os_string(),
            destination.clone().into_os_string(),
        ]);

        assert_eq!(code, 0);
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());
        assert_eq!(
            std::fs::read(destination).expect("read destination"),
            b"limited"
        );
    }

    #[cfg(unix)]
    #[test]
    fn transfer_request_with_owner_group_preserves_flags() {
        use tempfile::tempdir;

        let tmp = tempdir().expect("tempdir");
        let source = tmp.path().join("source.txt");
        let destination = tmp.path().join("destination.txt");
        std::fs::write(&source, b"metadata").expect("write source");

        let (code, stdout, stderr) = run_with_args([
            OsString::from("oc-rsync"),
            OsString::from("--owner"),
            OsString::from("--group"),
            source.clone().into_os_string(),
            destination.clone().into_os_string(),
        ]);

        assert_eq!(code, 0);
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());
        assert_eq!(
            std::fs::read(destination).expect("read destination"),
            b"metadata"
        );
    }

    #[test]
    fn bwlimit_invalid_value_reports_error() {
        let (code, stdout, stderr) =
            run_with_args([OsString::from("oc-rsync"), OsString::from("--bwlimit=oops")]);

        assert_eq!(code, 1);
        assert!(stdout.is_empty());
        let rendered = String::from_utf8(stderr).expect("diagnostic is valid UTF-8");
        assert!(rendered.contains("--bwlimit=oops is invalid"));
        assert!(rendered.contains("[client=3.4.1-rust]"));
    }

    #[test]
    fn bwlimit_rejects_small_fractional_values() {
        let (code, stdout, stderr) =
            run_with_args([OsString::from("oc-rsync"), OsString::from("--bwlimit=0.4")]);

        assert_eq!(code, 1);
        assert!(stdout.is_empty());
        let rendered = String::from_utf8(stderr).expect("diagnostic is valid UTF-8");
        assert!(rendered.contains("--bwlimit=0.4 is too small (min: 512 or 0 for unlimited)",));
    }

    #[test]
    fn bwlimit_accepts_decimal_suffixes() {
        let limit = parse_bandwidth_limit(OsStr::new("1.5M"))
            .expect("parse succeeds")
            .expect("limit available");
        assert_eq!(limit.bytes_per_second().get(), 1_572_864);
    }

    #[test]
    fn bwlimit_accepts_decimal_base_specifier() {
        let limit = parse_bandwidth_limit(OsStr::new("10KB"))
            .expect("parse succeeds")
            .expect("limit available");
        assert_eq!(limit.bytes_per_second().get(), 10_240);
    }

    #[test]
    fn bwlimit_zero_disables_limit() {
        let limit = parse_bandwidth_limit(OsStr::new("0")).expect("parse succeeds");
        assert!(limit.is_none());
    }

    #[test]
    fn remote_operand_reports_diagnostic() {
        let (code, stdout, stderr) = run_with_args([
            OsString::from("oc-rsync"),
            OsString::from("host::module"),
            OsString::from("dest"),
        ]);

        assert_eq!(code, 23);
        assert!(stdout.is_empty());

        let rendered = String::from_utf8(stderr).expect("diagnostic is valid UTF-8");
        assert!(rendered.contains("remote operands are not supported"));
        assert!(rendered.contains("[client=3.4.1-rust]"));
    }

    #[test]
    fn remote_daemon_listing_prints_modules() {
        let (addr, handle) = spawn_stub_daemon(vec![
            "@RSYNCD: MOTD Welcome to the test daemon\n",
            "@RSYNCD: OK\n",
            "first\tFirst module\n",
            "second\n",
            "@RSYNCD: EXIT\n",
        ]);

        let url = format!("rsync://{}:{}/", addr.ip(), addr.port());
        let (code, stdout, stderr) =
            run_with_args([OsString::from("oc-rsync"), OsString::from(url)]);

        assert_eq!(code, 0);
        assert!(stderr.is_empty());

        let rendered = String::from_utf8(stdout).expect("output is UTF-8");
        assert!(rendered.contains("Welcome to the test daemon"));
        assert!(rendered.contains("first\tFirst module"));
        assert!(rendered.contains("second"));

        handle.join().expect("server thread");
    }

    #[test]
    fn remote_daemon_error_is_reported() {
        let (addr, handle) = spawn_stub_daemon(vec!["@ERROR: unavailable\n", "@RSYNCD: EXIT\n"]);

        let url = format!("rsync://{}:{}/", addr.ip(), addr.port());
        let (code, stdout, stderr) =
            run_with_args([OsString::from("oc-rsync"), OsString::from(url)]);

        assert_eq!(code, 23);
        assert!(stdout.is_empty());

        let rendered = String::from_utf8(stderr).expect("diagnostic is valid UTF-8");
        assert!(rendered.contains("unavailable"));

        handle.join().expect("server thread");
    }

    #[test]
    fn module_list_username_prefix_is_rejected() {
        let (code, stdout, stderr) = run_with_args([
            OsString::from("oc-rsync"),
            OsString::from("rsync://user@localhost/"),
        ]);

        assert_eq!(code, 1);
        assert!(stdout.is_empty());

        let rendered = String::from_utf8(stderr).expect("diagnostic is valid UTF-8");
        assert!(rendered.contains("daemon usernames are not supported"));
    }

    #[test]
    fn module_list_username_prefix_legacy_syntax_is_rejected() {
        let (code, stdout, stderr) = run_with_args([
            OsString::from("oc-rsync"),
            OsString::from("user@localhost::"),
        ]);

        assert_eq!(code, 1);
        assert!(stdout.is_empty());

        let rendered = String::from_utf8(stderr).expect("diagnostic is valid UTF-8");
        assert!(rendered.contains("daemon usernames are not supported"));
    }

    #[test]
    fn clap_parse_error_is_reported_via_message() {
        let command = clap_command();
        let error = command
            .try_get_matches_from(vec!["oc-rsync", "--version=extra"])
            .unwrap_err();

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let status = run(
            [
                OsString::from("oc-rsync"),
                OsString::from("--version=extra"),
            ],
            &mut stdout,
            &mut stderr,
        );

        assert_eq!(status, 1);
        assert!(stdout.is_empty());

        let rendered = String::from_utf8(stderr).expect("diagnostic is valid UTF-8");
        assert!(rendered.contains(error.to_string().trim()));
    }

    #[test]
    fn delete_flag_is_parsed() {
        let parsed = super::parse_args([
            OsString::from("oc-rsync"),
            OsString::from("--delete"),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .expect("parse succeeds");

        assert!(parsed.delete);
    }

    #[test]
    fn archive_flag_is_parsed() {
        let parsed = super::parse_args([
            OsString::from("oc-rsync"),
            OsString::from("-a"),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .expect("parse succeeds");

        assert!(parsed.archive);
        assert!(!parsed.owner);
        assert!(!parsed.group);
    }

    #[test]
    fn long_archive_flag_is_parsed() {
        let parsed = super::parse_args([
            OsString::from("oc-rsync"),
            OsString::from("--archive"),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .expect("parse succeeds");

        assert!(parsed.archive);
    }

    #[test]
    fn unsupported_short_option_reports_error() {
        let (code, stdout, stderr) = run_with_args([
            OsString::from("oc-rsync"),
            OsString::from("-av"),
            OsString::from("source"),
            OsString::from("dest"),
        ]);

        assert_eq!(code, 1);
        assert!(stdout.is_empty());

        let rendered = String::from_utf8(stderr).expect("diagnostic is valid UTF-8");
        assert!(rendered.contains("unsupported option '-av'"));
        assert!(rendered.contains("[client=3.4.1-rust]"));
    }

    #[test]
    fn dry_run_flag_skips_destination_mutation() {
        use std::fs;
        use tempfile::tempdir;

        let tmp = tempdir().expect("tempdir");
        let source = tmp.path().join("source.txt");
        fs::write(&source, b"contents").expect("write source");
        let destination = tmp.path().join("dest.txt");

        let (code, stdout, stderr) = run_with_args([
            OsString::from("oc-rsync"),
            OsString::from("--dry-run"),
            source.clone().into_os_string(),
            destination.clone().into_os_string(),
        ]);

        assert_eq!(code, 0);
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());
        assert!(!destination.exists());
    }

    #[test]
    fn short_dry_run_flag_skips_destination_mutation() {
        use std::fs;
        use tempfile::tempdir;

        let tmp = tempdir().expect("tempdir");
        let source = tmp.path().join("source.txt");
        fs::write(&source, b"contents").expect("write source");
        let destination = tmp.path().join("dest.txt");

        let (code, stdout, stderr) = run_with_args([
            OsString::from("oc-rsync"),
            OsString::from("-n"),
            source.clone().into_os_string(),
            destination.clone().into_os_string(),
        ]);

        assert_eq!(code, 0);
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());
        assert!(!destination.exists());
    }

    #[test]
    fn operands_after_end_of_options_are_preserved() {
        use std::fs;
        use tempfile::tempdir;

        let tmp = tempdir().expect("tempdir");
        let source = tmp.path().join("-source");
        let destination = tmp.path().join("dest.txt");
        fs::write(&source, b"dash source").expect("write source");

        let (code, stdout, stderr) = run_with_args([
            OsString::from("oc-rsync"),
            OsString::from("--"),
            source.clone().into_os_string(),
            destination.clone().into_os_string(),
        ]);

        assert_eq!(code, 0);
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());
        assert_eq!(
            fs::read(destination).expect("read destination"),
            b"dash source"
        );
    }

    fn spawn_stub_daemon(
        responses: Vec<&'static str>,
    ) -> (std::net::SocketAddr, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind stub daemon");
        let addr = listener.local_addr().expect("local addr");

        let handle = thread::spawn(move || {
            if let Ok((stream, _)) = listener.accept() {
                handle_connection(stream, responses);
            }
        });

        (addr, handle)
    }

    fn handle_connection(mut stream: TcpStream, responses: Vec<&'static str>) {
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .expect("set read timeout");
        stream
            .set_write_timeout(Some(Duration::from_secs(5)))
            .expect("set write timeout");

        stream
            .write_all(b"@RSYNCD: 32.0\n")
            .expect("write greeting");
        stream.flush().expect("flush greeting");

        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader.read_line(&mut line).expect("read client greeting");
        assert_eq!(line, "@RSYNCD: 32.0\n");

        line.clear();
        reader.read_line(&mut line).expect("read request");
        assert_eq!(line, "#list\n");

        for response in responses {
            reader
                .get_mut()
                .write_all(response.as_bytes())
                .expect("write response");
        }
        reader.get_mut().flush().expect("flush response");
    }
}
