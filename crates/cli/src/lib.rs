#![deny(unsafe_code)]
#![deny(missing_docs)]
#![deny(rustdoc::broken_intra_doc_links)]

//! # Overview
//!
//! `rsync_cli` implements the thin command-line front-end for the Rust `oc-rsync`
//! workspace. The crate is intentionally small: it recognises the subset of
//! command-line switches that are currently supported (`--help`/`-h`,
//! `--version`/`-V`, `--dry-run`/`-n`, `--delete`, `--filter` (supporting
//! `+`/`-` actions and `merge FILE` directives), `--files-from`,
//! `--from0`, `--bwlimit`, and `--sparse`) and delegates local copy operations to
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
//! parse that recognises `--help`, `--version`, `--dry-run`, `--delete`,
//! `--filter`, `--files-from`, `--from0`, and `--bwlimit` flags while treating all other
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
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, Write};
use std::path::PathBuf;
use std::time::Duration;

#[cfg(unix)]
use std::os::unix::ffi::OsStringExt;

use clap::{Arg, ArgAction, Command, builder::OsStringValueParser};
use rsync_core::{
    bandwidth::BandwidthParseError,
    client::{
        BandwidthLimit, ClientConfig, ClientEvent, ClientEventKind, ClientSummary, FilterRuleKind,
        FilterRuleSpec, ModuleListRequest, run_client as run_core_client, run_module_list,
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
    "Usage: oc-rsync [-h] [-V] [-n] [-a] [-S] [--delete] [--bwlimit=RATE] SOURCE... DEST\n",
    "\n",
    "This development snapshot implements deterministic local filesystem\n",
    "copies for regular files, directories, and symbolic links. The\n",
    "following options are recognised:\n",
    "  -h, --help       Show this help message and exit.\n",
    "  -V, --version    Output version information and exit.\n",
    "  -n, --dry-run    Validate transfers without modifying the destination.\n",
    "  -a, --archive    Enable archive mode (implies --owner and --group).\n",
    "      --delete     Remove destination files that are absent from the source.\n",
    "  -c, --checksum   Skip updates for files that already match by checksum.\n",
    "      --exclude=PATTERN  Skip files matching PATTERN.\n",
    "      --exclude-from=FILE  Read exclude patterns from FILE.\n",
    "      --include=PATTERN  Re-include files matching PATTERN after exclusions.\n",
    "      --include-from=FILE  Read include patterns from FILE.\n",
    "      --filter=RULE  Apply filter RULE (supports '+' include, '-' exclude, 'include PATTERN', 'exclude PATTERN', 'protect PATTERN', and 'merge FILE').\n",
    "      --files-from=FILE  Read additional source operands from FILE.\n",
    "      --from0      Treat file list entries as NUL-terminated records.\n",
    "      --bwlimit    Limit I/O bandwidth in KiB/s (0 disables the limit).\n",
    "  -v, --verbose    Increase verbosity; repeat for more detail.\n",
    "      --progress   Show progress information during transfers.\n",
    "      --no-progress  Disable progress reporting.\n",
    "      --partial    Keep partially transferred files on errors.\n",
    "      --no-partial Discard partially transferred files on errors.\n",
    "      --inplace    Write updated data directly to destination files.\n",
    "      --no-inplace Use temporary files when updating regular files.\n",
    "  -P              Equivalent to --partial --progress.\n",
    "  -S, --sparse    Preserve sparse files by creating holes in the destination.\n",
    "      --no-sparse Disable sparse file handling.\n",
    "      --owner      Preserve file ownership (requires super-user).\n",
    "      --no-owner   Disable ownership preservation.\n",
    "      --group      Preserve file group (requires suitable privileges).\n",
    "      --no-group   Disable group preservation.\n",
    "  -p, --perms      Preserve file permissions.\n",
    "      --no-perms   Disable permission preservation.\n",
    "  -t, --times      Preserve modification times.\n",
    "      --no-times   Disable modification time preservation.\n",
    "  -X, --xattrs     Preserve extended attributes when supported.\n",
    "      --no-xattrs  Disable extended attribute preservation.\n",
    "      --numeric-ids      Preserve numeric UID/GID values.\n",
    "      --no-numeric-ids   Map UID/GID values to names when possible.\n",
    "\n",
    "All SOURCE operands must reside on the local filesystem. When multiple\n",
    "sources are supplied, DEST must name a directory. Metadata preservation\n",
    "covers permissions, timestamps, and optional ownership metadata.\n",
);

/// Human-readable list of the options recognised by this development build.
const SUPPORTED_OPTIONS_LIST: &str = "--help/-h, --version/-V, --dry-run/-n, --archive/-a, --delete, --checksum/-c, --exclude, --exclude-from, --include, --include-from, --filter, --files-from, --from0, --bwlimit, --verbose/-v, --progress, --no-progress, --partial, --no-partial, --inplace, --no-inplace, -P, --sparse/-S, --no-sparse, --owner, --no-owner, --group, --no-group, --perms/-p, --no-perms, --times/-t, --no-times, --xattrs/-X, --no-xattrs, --numeric-ids, and --no-numeric-ids";

/// Parsed command produced by [`parse_args`].
#[derive(Debug, Default)]
struct ParsedArgs {
    show_help: bool,
    show_version: bool,
    dry_run: bool,
    archive: bool,
    delete: bool,
    checksum: bool,
    remainder: Vec<OsString>,
    bwlimit: Option<OsString>,
    owner: Option<bool>,
    group: Option<bool>,
    perms: Option<bool>,
    times: Option<bool>,
    numeric_ids: Option<bool>,
    sparse: Option<bool>,
    verbosity: u8,
    progress: bool,
    partial: bool,
    inplace: Option<bool>,
    excludes: Vec<OsString>,
    includes: Vec<OsString>,
    exclude_from: Vec<OsString>,
    include_from: Vec<OsString>,
    filters: Vec<OsString>,
    files_from: Vec<OsString>,
    from0: bool,
    xattrs: Option<bool>,
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
            Arg::new("checksum")
                .long("checksum")
                .short('c')
                .help("Skip files whose contents already match by checksum.")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("sparse")
                .long("sparse")
                .short('S')
                .help("Preserve sparse files by creating holes in the destination.")
                .action(ArgAction::SetTrue)
                .conflicts_with("no-sparse"),
        )
        .arg(
            Arg::new("no-sparse")
                .long("no-sparse")
                .help("Disable sparse file handling.")
                .action(ArgAction::SetTrue)
                .conflicts_with("sparse"),
        )
        .arg(
            Arg::new("verbose")
                .long("verbose")
                .short('v')
                .help("Increase verbosity; may be supplied multiple times.")
                .action(ArgAction::Count),
        )
        .arg(
            Arg::new("progress")
                .long("progress")
                .help("Show progress information during transfers.")
                .action(ArgAction::SetTrue)
                .overrides_with("no-progress"),
        )
        .arg(
            Arg::new("no-progress")
                .long("no-progress")
                .help("Disable progress reporting.")
                .action(ArgAction::SetTrue)
                .overrides_with("progress"),
        )
        .arg(
            Arg::new("partial")
                .long("partial")
                .help("Keep partially transferred files on error.")
                .action(ArgAction::SetTrue)
                .overrides_with("no-partial"),
        )
        .arg(
            Arg::new("no-partial")
                .long("no-partial")
                .help("Discard partially transferred files on error.")
                .action(ArgAction::SetTrue)
                .overrides_with("partial"),
        )
        .arg(
            Arg::new("inplace")
                .long("inplace")
                .help("Write updated data directly to destination files.")
                .action(ArgAction::SetTrue)
                .overrides_with("no-inplace"),
        )
        .arg(
            Arg::new("no-inplace")
                .long("no-inplace")
                .help("Use temporary files when updating regular files.")
                .action(ArgAction::SetTrue)
                .overrides_with("inplace"),
        )
        .arg(
            Arg::new("partial-progress")
                .short('P')
                .help("Equivalent to --partial --progress.")
                .action(ArgAction::Count)
                .overrides_with("no-partial")
                .overrides_with("no-progress"),
        )
        .arg(
            Arg::new("delete")
                .long("delete")
                .help("Remove destination files that are absent from the source.")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("exclude")
                .long("exclude")
                .value_name("PATTERN")
                .help("Skip files matching PATTERN.")
                .value_parser(OsStringValueParser::new())
                .action(ArgAction::Append),
        )
        .arg(
            Arg::new("exclude-from")
                .long("exclude-from")
                .value_name("FILE")
                .help("Read exclude patterns from FILE.")
                .value_parser(OsStringValueParser::new())
                .action(ArgAction::Append),
        )
        .arg(
            Arg::new("include")
                .long("include")
                .value_name("PATTERN")
                .help("Re-include files matching PATTERN after exclusions.")
                .value_parser(OsStringValueParser::new())
                .action(ArgAction::Append),
        )
        .arg(
            Arg::new("include-from")
                .long("include-from")
                .value_name("FILE")
                .help("Read include patterns from FILE.")
                .value_parser(OsStringValueParser::new())
                .action(ArgAction::Append),
        )
        .arg(
            Arg::new("filter")
                .long("filter")
                .value_name("RULE")
                .help("Apply filter RULE (supports '+' include, '-' exclude, 'protect PATTERN', and 'merge FILE').")
                .value_parser(OsStringValueParser::new())
                .action(ArgAction::Append),
        )
        .arg(
            Arg::new("files-from")
                .long("files-from")
                .value_name("FILE")
                .help("Read additional source operands from FILE.")
                .value_parser(OsStringValueParser::new())
                .action(ArgAction::Append),
        )
        .arg(
            Arg::new("from0")
                .long("from0")
                .help("Treat file list entries as NUL-terminated records.")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("owner")
                .long("owner")
                .short('o')
                .help("Preserve file ownership (requires super-user).")
                .action(ArgAction::SetTrue)
                .overrides_with("no-owner"),
        )
        .arg(
            Arg::new("no-owner")
                .long("no-owner")
                .help("Disable ownership preservation.")
                .action(ArgAction::SetTrue)
                .overrides_with("owner"),
        )
        .arg(
            Arg::new("group")
                .long("group")
                .short('g')
                .help("Preserve file group (requires suitable privileges).")
                .action(ArgAction::SetTrue)
                .overrides_with("no-group"),
        )
        .arg(
            Arg::new("no-group")
                .long("no-group")
                .help("Disable group preservation.")
                .action(ArgAction::SetTrue)
                .overrides_with("group"),
        )
        .arg(
            Arg::new("perms")
                .long("perms")
                .short('p')
                .help("Preserve file permissions.")
                .action(ArgAction::SetTrue)
                .overrides_with("no-perms"),
        )
        .arg(
            Arg::new("no-perms")
                .long("no-perms")
                .help("Disable permission preservation.")
                .action(ArgAction::SetTrue)
                .overrides_with("perms"),
        )
        .arg(
            Arg::new("times")
                .long("times")
                .short('t')
                .help("Preserve modification times.")
                .action(ArgAction::SetTrue)
                .overrides_with("no-times"),
        )
        .arg(
            Arg::new("no-times")
                .long("no-times")
                .help("Disable modification time preservation.")
                .action(ArgAction::SetTrue)
                .overrides_with("times"),
        )
        .arg(
            Arg::new("xattrs")
                .long("xattrs")
                .short('X')
                .help("Preserve extended attributes when supported.")
                .action(ArgAction::SetTrue)
                .overrides_with("no-xattrs"),
        )
        .arg(
            Arg::new("no-xattrs")
                .long("no-xattrs")
                .help("Disable extended attribute preservation.")
                .action(ArgAction::SetTrue)
                .overrides_with("xattrs"),
        )
        .arg(
            Arg::new("numeric-ids")
                .long("numeric-ids")
                .help("Preserve numeric UID/GID values.")
                .action(ArgAction::SetTrue)
                .overrides_with("no-numeric-ids"),
        )
        .arg(
            Arg::new("no-numeric-ids")
                .long("no-numeric-ids")
                .help("Map UID/GID values to names when possible.")
                .action(ArgAction::SetTrue)
                .overrides_with("numeric-ids"),
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
    let owner = if matches.get_flag("owner") {
        Some(true)
    } else if matches.get_flag("no-owner") {
        Some(false)
    } else {
        None
    };
    let group = if matches.get_flag("group") {
        Some(true)
    } else if matches.get_flag("no-group") {
        Some(false)
    } else {
        None
    };
    let perms = if matches.get_flag("perms") {
        Some(true)
    } else if matches.get_flag("no-perms") {
        Some(false)
    } else {
        None
    };
    let times = if matches.get_flag("times") {
        Some(true)
    } else if matches.get_flag("no-times") {
        Some(false)
    } else {
        None
    };
    let xattrs = if matches.get_flag("xattrs") {
        Some(true)
    } else if matches.get_flag("no-xattrs") {
        Some(false)
    } else {
        None
    };
    let numeric_ids = if matches.get_flag("numeric-ids") {
        Some(true)
    } else if matches.get_flag("no-numeric-ids") {
        Some(false)
    } else {
        None
    };
    let sparse = if matches.get_flag("sparse") {
        Some(true)
    } else if matches.get_flag("no-sparse") {
        Some(false)
    } else {
        None
    };
    let verbosity = matches.get_count("verbose") as u8;
    let mut progress = matches.get_flag("progress");
    let mut partial = matches.get_flag("partial");
    if matches.get_flag("no-progress") {
        progress = false;
    }
    if matches.get_flag("no-partial") {
        partial = false;
    }
    if matches.get_count("partial-progress") > 0 {
        partial = true;
        if !matches.get_flag("no-progress") {
            progress = true;
        }
    }
    let inplace = if matches.get_flag("no-inplace") {
        Some(false)
    } else if matches.get_flag("inplace") {
        Some(true)
    } else {
        None
    };
    let remainder = matches
        .remove_many::<OsString>("args")
        .map(|values| values.collect())
        .unwrap_or_default();
    let checksum = matches.get_flag("checksum");

    let bwlimit = matches
        .remove_one::<OsString>("bwlimit")
        .map(|value| value.into());
    let excludes = matches
        .remove_many::<OsString>("exclude")
        .map(|values| values.collect())
        .unwrap_or_default();
    let includes = matches
        .remove_many::<OsString>("include")
        .map(|values| values.collect())
        .unwrap_or_default();
    let exclude_from = matches
        .remove_many::<OsString>("exclude-from")
        .map(|values| values.collect())
        .unwrap_or_default();
    let include_from = matches
        .remove_many::<OsString>("include-from")
        .map(|values| values.collect())
        .unwrap_or_default();
    let filters = matches
        .remove_many::<OsString>("filter")
        .map(|values| values.collect())
        .unwrap_or_default();
    let files_from = matches
        .remove_many::<OsString>("files-from")
        .map(|values| values.collect())
        .unwrap_or_default();
    let from0 = matches.get_flag("from0");

    Ok(ParsedArgs {
        show_help,
        show_version,
        dry_run,
        archive,
        delete,
        checksum,
        remainder,
        bwlimit,
        owner,
        group,
        perms,
        times,
        numeric_ids,
        sparse,
        verbosity,
        progress,
        partial,
        inplace,
        excludes,
        includes,
        exclude_from,
        include_from,
        filters,
        files_from,
        from0,
        xattrs,
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
    let ParsedArgs {
        show_help,
        show_version,
        dry_run,
        archive,
        delete,
        checksum,
        remainder: raw_remainder,
        bwlimit,
        owner,
        group,
        perms,
        times,
        excludes,
        includes,
        exclude_from,
        include_from,
        filters,
        files_from,
        from0,
        numeric_ids,
        sparse,
        verbosity,
        progress,
        partial,
        inplace,
        xattrs,
    } = parsed;

    if show_help {
        let help = render_help();
        if stdout.write_all(help.as_bytes()).is_err() {
            let _ = writeln!(stdout, "{help}");
            return 1;
        }
        return 0;
    }

    if show_version {
        let report = VersionInfoReport::default();
        let banner = report.human_readable();
        if stdout.write_all(banner.as_bytes()).is_err() {
            return 1;
        }
        return 0;
    }

    let remainder = match extract_operands(raw_remainder) {
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

    let bandwidth_limit = match bwlimit {
        Some(ref value) => match parse_bandwidth_limit(value.as_os_str()) {
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

    let numeric_ids = numeric_ids.unwrap_or(false);

    #[cfg(not(feature = "xattr"))]
    if xattrs.unwrap_or(false) {
        let message = rsync_error!(1, "extended attributes are not supported on this client")
            .with_role(Role::Client);
        if write_message(&message, stderr).is_err() {
            let _ = writeln!(
                stderr.writer_mut(),
                "extended attributes are not supported on this client"
            );
        }
        return 1;
    }

    let mut file_list_operands = match load_file_list_operands(&files_from, from0) {
        Ok(operands) => operands,
        Err(message) => {
            if write_message(&message, stderr).is_err() {
                let fallback = message.to_string();
                let _ = writeln!(stderr.writer_mut(), "{}", fallback);
            }
            return 1;
        }
    };

    if file_list_operands.is_empty() {
        match ModuleListRequest::from_operands(&remainder) {
            Ok(Some(request)) => {
                return match run_module_list(request) {
                    Ok(list) => {
                        if render_module_list(stdout, stderr.writer_mut(), &list).is_err() {
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
    }

    let mut transfer_operands = Vec::with_capacity(file_list_operands.len() + remainder.len());
    transfer_operands.append(&mut file_list_operands);
    transfer_operands.extend(remainder);

    let preserve_owner = owner.unwrap_or(archive);
    let preserve_group = group.unwrap_or(archive);
    let preserve_permissions = perms.unwrap_or(archive);
    let preserve_times = times.unwrap_or(archive);
    let sparse = sparse.unwrap_or(false);

    let mut builder = ClientConfig::builder()
        .transfer_args(transfer_operands)
        .dry_run(dry_run)
        .delete(delete)
        .bandwidth_limit(bandwidth_limit)
        .owner(preserve_owner)
        .group(preserve_group)
        .permissions(preserve_permissions)
        .times(preserve_times)
        .checksum(checksum)
        .numeric_ids(numeric_ids)
        .sparse(sparse)
        .verbosity(verbosity)
        .progress(progress)
        .partial(partial)
        .inplace(inplace.unwrap_or(false));
    #[cfg(feature = "xattr")]
    {
        builder = builder.xattrs(xattrs.unwrap_or(false));
    }

    let mut filter_rules = Vec::new();
    if let Err(message) =
        append_filter_rules_from_files(&mut filter_rules, &exclude_from, FilterRuleKind::Exclude)
    {
        if write_message(&message, stderr).is_err() {
            let fallback = message.to_string();
            let _ = writeln!(stderr.writer_mut(), "{}", fallback);
        }
        return 1;
    }
    filter_rules.extend(
        excludes
            .into_iter()
            .map(|pattern| FilterRuleSpec::exclude(os_string_to_pattern(pattern))),
    );
    if let Err(message) =
        append_filter_rules_from_files(&mut filter_rules, &include_from, FilterRuleKind::Include)
    {
        if write_message(&message, stderr).is_err() {
            let fallback = message.to_string();
            let _ = writeln!(stderr.writer_mut(), "{}", fallback);
        }
        return 1;
    }
    filter_rules.extend(
        includes
            .into_iter()
            .map(|pattern| FilterRuleSpec::include(os_string_to_pattern(pattern))),
    );
    let mut merge_stack = Vec::new();
    for filter in filters {
        match parse_filter_directive(filter.as_os_str()) {
            Ok(FilterDirective::Rule(spec)) => filter_rules.push(spec),
            Ok(FilterDirective::Merge(source)) => {
                if let Err(message) =
                    apply_merge_directive(source, &mut filter_rules, &mut merge_stack)
                {
                    if write_message(&message, stderr).is_err() {
                        let _ = writeln!(stderr.writer_mut(), "{}", message);
                    }
                    return 1;
                }
            }
            Err(message) => {
                if write_message(&message, stderr).is_err() {
                    let _ = writeln!(stderr.writer_mut(), "{}", message);
                }
                return 1;
            }
        }
    }
    if !filter_rules.is_empty() {
        builder = builder.extend_filter_rules(filter_rules);
    }

    let config = builder.build();

    match run_core_client(config) {
        Ok(summary) => {
            if let Err(error) = emit_transfer_summary(&summary, verbosity, progress, stdout) {
                let _ = writeln!(
                    stdout,
                    "warning: failed to render transfer summary: {error}"
                );
            }
            0
        }
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

/// Emits verbose and progress-oriented output derived from a [`ClientSummary`].
fn emit_transfer_summary<W: Write>(
    summary: &ClientSummary,
    verbosity: u8,
    progress: bool,
    stdout: &mut W,
) -> io::Result<()> {
    let events = summary.events();

    if progress && !events.is_empty() {
        emit_progress(events, stdout)?;
    }

    if verbosity > 0 && !events.is_empty() {
        emit_verbose(events, verbosity, stdout)?;
    }

    if verbosity > 0 {
        emit_totals(summary, stdout)?;
    }

    Ok(())
}

/// Renders progress lines for the provided transfer events.
fn emit_progress<W: Write>(events: &[ClientEvent], stdout: &mut W) -> io::Result<()> {
    let mut total_bytes = 0u64;
    let mut total_elapsed = Duration::default();
    let mut emitted = false;

    for event in events {
        let bytes = event.bytes_transferred();
        if bytes == 0 {
            continue;
        }

        emitted = true;
        total_bytes = total_bytes.saturating_add(bytes);
        total_elapsed += event.elapsed();

        if let Some(rate) = compute_rate(bytes, event.elapsed()) {
            writeln!(
                stdout,
                "{}: {} bytes ({rate:.1} B/s)",
                event.relative_path().display(),
                bytes
            )?;
        } else {
            writeln!(
                stdout,
                "{}: {} bytes",
                event.relative_path().display(),
                bytes
            )?;
        }
    }

    if !emitted {
        writeln!(stdout, "Total transferred: 0 bytes")?;
        return Ok(());
    }

    let seconds = total_elapsed.as_secs_f64();
    if seconds > 0.0 {
        let rate = total_bytes as f64 / seconds;
        writeln!(
            stdout,
            "Total transferred: {total_bytes} bytes in {seconds:.3}s ({rate:.1} B/s)"
        )
    } else {
        writeln!(stdout, "Total transferred: {total_bytes} bytes")
    }
}

/// Emits the summary lines reported by verbose transfers.
fn emit_totals<W: Write>(summary: &ClientSummary, stdout: &mut W) -> io::Result<()> {
    let sent = summary.bytes_copied();
    let received = 0u64;
    let total_size = summary.total_source_bytes();
    let elapsed = summary.total_elapsed();
    let seconds = elapsed.as_secs_f64();
    let rate = if seconds > 0.0 {
        (sent + received) as f64 / seconds
    } else {
        0.0
    };
    let transmitted = sent.saturating_add(received);
    let speedup = if transmitted > 0 {
        total_size as f64 / transmitted as f64
    } else {
        0.0
    };

    writeln!(
        stdout,
        "sent {sent} bytes  received {received} bytes  {rate:.2} bytes/sec"
    )?;
    writeln!(
        stdout,
        "total size is {total_size}  speedup is {speedup:.2}"
    )
}

/// Renders verbose listings for the provided transfer events.
fn emit_verbose<W: Write>(events: &[ClientEvent], verbosity: u8, stdout: &mut W) -> io::Result<()> {
    for event in events {
        if verbosity == 1 {
            writeln!(stdout, "{}", event.relative_path().display())?;
            continue;
        }

        let descriptor = describe_event_kind(event.kind());
        let bytes = event.bytes_transferred();
        if bytes > 0 {
            if let Some(rate) = compute_rate(bytes, event.elapsed()) {
                writeln!(
                    stdout,
                    "{descriptor}: {} ({} bytes, {rate:.1} B/s)",
                    event.relative_path().display(),
                    bytes
                )?;
            } else {
                writeln!(
                    stdout,
                    "{descriptor}: {} ({} bytes)",
                    event.relative_path().display(),
                    bytes
                )?;
            }
        } else {
            writeln!(stdout, "{descriptor}: {}", event.relative_path().display())?;
        }
    }
    Ok(())
}

/// Maps an event kind to a human-readable description.
fn describe_event_kind(kind: &ClientEventKind) -> &'static str {
    match kind {
        ClientEventKind::DataCopied => "copied",
        ClientEventKind::MetadataReused => "metadata reused",
        ClientEventKind::HardLink => "hard link",
        ClientEventKind::SymlinkCopied => "symlink",
        ClientEventKind::FifoCopied => "fifo",
        ClientEventKind::DeviceCopied => "device",
        ClientEventKind::DirectoryCreated => "directory",
        ClientEventKind::EntryDeleted => "deleted",
    }
}

/// Computes the throughput in bytes per second for the provided measurements.
fn compute_rate(bytes: u64, elapsed: Duration) -> Option<f64> {
    if elapsed.is_zero() {
        return None;
    }

    let seconds = elapsed.as_secs_f64();
    if seconds <= 0.0 {
        None
    } else {
        Some(bytes as f64 / seconds)
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

fn os_string_to_pattern(value: OsString) -> String {
    match value.into_string() {
        Ok(text) => text,
        Err(value) => value.to_string_lossy().into_owned(),
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum FilterDirective {
    Rule(FilterRuleSpec),
    Merge(OsString),
}

fn parse_filter_directive(argument: &OsStr) -> Result<FilterDirective, Message> {
    let text = argument.to_string_lossy();
    let trimmed_leading = text.trim_start();

    if let Some(rest) = trimmed_leading.strip_prefix("merge") {
        let remainder = rest.trim_start();

        if remainder.starts_with(',') {
            let message = rsync_error!(
                1,
                format!(
                    "filter merge directive '{trimmed_leading}' uses unsupported modifiers; this build accepts only 'merge FILE'"
                )
            )
            .with_role(Role::Client);
            return Err(message);
        }

        let path_text = remainder.trim();
        if path_text.is_empty() {
            let message = rsync_error!(
                1,
                format!("filter merge directive '{trimmed_leading}' is missing a file path")
            )
            .with_role(Role::Client);
            return Err(message);
        }

        return Ok(FilterDirective::Merge(OsString::from(path_text)));
    }

    let trimmed = trimmed_leading.trim_end();

    if trimmed.is_empty() {
        let message = rsync_error!(1, "filter rule is empty: supply '+', '-', or 'merge FILE'")
            .with_role(Role::Client);
        return Err(message);
    }

    if let Some(remainder) = trimmed.strip_prefix('+') {
        let pattern = remainder.trim_start();
        if pattern.is_empty() {
            let message = rsync_error!(1, "filter rule '{trimmed}' is missing a pattern after '+'")
                .with_role(Role::Client);
            return Err(message);
        }
        return Ok(FilterDirective::Rule(FilterRuleSpec::include(
            pattern.to_string(),
        )));
    }

    if let Some(remainder) = trimmed.strip_prefix('-') {
        let pattern = remainder.trim_start();
        if pattern.is_empty() {
            let message = rsync_error!(1, "filter rule '{trimmed}' is missing a pattern after '-'")
                .with_role(Role::Client);
            return Err(message);
        }
        return Ok(FilterDirective::Rule(FilterRuleSpec::exclude(
            pattern.to_string(),
        )));
    }

    let mut parts = trimmed.splitn(2, |ch: char| ch.is_ascii_whitespace());
    let keyword = parts.next().expect("split always yields at least one part");
    let remainder = parts.next().unwrap_or("");
    let pattern = remainder.trim_start();

    let handle_keyword = |action_label: &str, builder: fn(String) -> FilterRuleSpec| {
        if pattern.is_empty() {
            let text =
                format!("filter rule '{trimmed}' is missing a pattern after '{action_label}'");
            let message = rsync_error!(1, text).with_role(Role::Client);
            return Err(message);
        }

        Ok(FilterDirective::Rule(builder(pattern.to_string())))
    };

    if keyword.eq_ignore_ascii_case("include") {
        return handle_keyword("include", FilterRuleSpec::include);
    }

    if keyword.eq_ignore_ascii_case("exclude") {
        return handle_keyword("exclude", FilterRuleSpec::exclude);
    }

    if keyword.eq_ignore_ascii_case("protect") {
        return handle_keyword("protect", FilterRuleSpec::protect);
    }

    let message = rsync_error!(
        1,
        "unsupported filter rule '{trimmed}': this build currently supports only '+' (include), '-' (exclude), 'include PATTERN', 'exclude PATTERN', 'protect PATTERN', and 'merge FILE' directives"
    )
    .with_role(Role::Client);
    Err(message)
}

fn append_filter_rules_from_files(
    destination: &mut Vec<FilterRuleSpec>,
    files: &[OsString],
    kind: FilterRuleKind,
) -> Result<(), Message> {
    for path in files {
        let patterns = load_filter_file_patterns(path.as_os_str())?;
        destination.extend(patterns.into_iter().map(|pattern| match kind {
            FilterRuleKind::Include => FilterRuleSpec::include(pattern),
            FilterRuleKind::Exclude => FilterRuleSpec::exclude(pattern),
            FilterRuleKind::Protect => FilterRuleSpec::protect(pattern),
        }));
    }
    Ok(())
}

fn load_filter_file_patterns(path: &OsStr) -> Result<Vec<String>, Message> {
    if path == OsStr::new("-") {
        return read_filter_patterns_from_standard_input();
    }

    let path_buf = PathBuf::from(path);
    let path_display = path_buf.display().to_string();
    let file = File::open(&path_buf).map_err(|error| {
        let text = format!("failed to read filter file '{}': {}", path_display, error);
        rsync_error!(1, text).with_role(Role::Client)
    })?;

    let mut reader = BufReader::new(file);
    read_filter_patterns(&mut reader).map_err(|error| {
        let text = format!("failed to read filter file '{}': {}", path_display, error);
        rsync_error!(1, text).with_role(Role::Client)
    })
}

fn apply_merge_directive(
    source: OsString,
    destination: &mut Vec<FilterRuleSpec>,
    visited: &mut Vec<PathBuf>,
) -> Result<(), Message> {
    let (guard_key, display) = if source.as_os_str() == OsStr::new("-") {
        (PathBuf::from("-"), String::from("-"))
    } else {
        let path_buf = PathBuf::from(&source);
        let display = path_buf.display().to_string();
        let canonical = fs::canonicalize(&path_buf).unwrap_or(path_buf);
        (canonical, display)
    };

    if visited.contains(&guard_key) {
        let text = format!("recursive filter merge detected for '{display}'");
        return Err(rsync_error!(1, text).with_role(Role::Client));
    }

    visited.push(guard_key);
    let result = (|| -> Result<(), Message> {
        let entries = load_filter_file_patterns(source.as_os_str())?;
        for entry in entries {
            match parse_filter_directive(OsStr::new(entry.as_str())) {
                Ok(FilterDirective::Rule(rule)) => destination.push(rule),
                Ok(FilterDirective::Merge(nested)) => {
                    apply_merge_directive(nested, destination, visited).map_err(|error| {
                        let detail = error.to_string();
                        rsync_error!(
                            1,
                            format!("failed to process merge file '{display}': {detail}")
                        )
                        .with_role(Role::Client)
                    })?;
                }
                Err(error) => {
                    let detail = error.to_string();
                    return Err(
                        rsync_error!(
                            1,
                            format!(
                                "failed to parse filter rule '{entry}' from merge file '{display}': {detail}"
                            )
                        )
                        .with_role(Role::Client),
                    );
                }
            }
        }
        Ok(())
    })();
    visited.pop();
    result
}

fn read_filter_patterns_from_standard_input() -> Result<Vec<String>, Message> {
    #[cfg(test)]
    if let Some(data) = take_filter_stdin_input() {
        let mut cursor = io::Cursor::new(data);
        return read_filter_patterns(&mut cursor).map_err(|error| {
            let text = format!(
                "failed to read filter patterns from standard input: {}",
                error
            );
            rsync_error!(1, text).with_role(Role::Client)
        });
    }

    let stdin = io::stdin();
    let mut reader = stdin.lock();
    read_filter_patterns(&mut reader).map_err(|error| {
        let text = format!(
            "failed to read filter patterns from standard input: {}",
            error
        );
        rsync_error!(1, text).with_role(Role::Client)
    })
}

fn read_filter_patterns<R: BufRead>(reader: &mut R) -> io::Result<Vec<String>> {
    let mut buffer = Vec::new();
    let mut patterns = Vec::new();

    loop {
        buffer.clear();
        let bytes_read = reader.read_until(b'\n', &mut buffer)?;

        if bytes_read == 0 {
            break;
        }

        if buffer.last() == Some(&b'\n') {
            buffer.pop();
        }
        if buffer.last() == Some(&b'\r') {
            buffer.pop();
        }

        let line = String::from_utf8_lossy(&buffer);
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        patterns.push(line.into_owned());
    }

    Ok(patterns)
}

#[cfg(test)]
thread_local! {
    static FILTER_STDIN_INPUT: std::cell::RefCell<Option<Vec<u8>>> = const {
        std::cell::RefCell::new(None)
    };
}

#[cfg(test)]
fn take_filter_stdin_input() -> Option<Vec<u8>> {
    FILTER_STDIN_INPUT.with(|slot| slot.borrow_mut().take())
}

#[cfg(test)]
fn set_filter_stdin_input(data: Vec<u8>) {
    FILTER_STDIN_INPUT.with(|slot| *slot.borrow_mut() = Some(data));
}

fn load_file_list_operands(
    files: &[OsString],
    zero_terminated: bool,
) -> Result<Vec<OsString>, Message> {
    if files.is_empty() {
        return Ok(Vec::new());
    }

    let mut entries = Vec::new();
    let mut stdin_handle: Option<io::Stdin> = None;

    for path in files {
        if path.as_os_str() == OsStr::new("-") {
            let stdin = stdin_handle.get_or_insert_with(io::stdin);
            let mut reader = stdin.lock();
            read_file_list_from_reader(&mut reader, zero_terminated, &mut entries).map_err(
                |error| {
                    rsync_error!(
                        1,
                        format!("failed to read file list from standard input: {error}")
                    )
                    .with_role(Role::Client)
                },
            )?;
            continue;
        }

        let path_buf = PathBuf::from(path);
        let display = path_buf.display().to_string();
        let file = File::open(&path_buf).map_err(|error| {
            rsync_error!(
                1,
                format!("failed to read file list '{}': {}", display, error)
            )
            .with_role(Role::Client)
        })?;
        let mut reader = BufReader::new(file);
        read_file_list_from_reader(&mut reader, zero_terminated, &mut entries).map_err(
            |error| {
                rsync_error!(
                    1,
                    format!("failed to read file list '{}': {}", display, error)
                )
                .with_role(Role::Client)
            },
        )?;
    }

    Ok(entries)
}

fn read_file_list_from_reader<R: BufRead>(
    reader: &mut R,
    zero_terminated: bool,
    entries: &mut Vec<OsString>,
) -> io::Result<()> {
    if zero_terminated {
        let mut buffer = Vec::new();
        loop {
            buffer.clear();
            let read = reader.read_until(b'\0', &mut buffer)?;
            if read == 0 {
                break;
            }

            if buffer.last() == Some(&b'\0') {
                buffer.pop();
            }

            push_file_list_entry(&buffer, entries);
        }
        return Ok(());
    }

    let mut buffer = Vec::new();
    loop {
        buffer.clear();
        let bytes_read = reader.read_until(b'\n', &mut buffer)?;
        if bytes_read == 0 {
            break;
        }

        if buffer.last() == Some(&b'\n') {
            buffer.pop();
        }
        if buffer.last() == Some(&b'\r') {
            buffer.pop();
        }

        push_file_list_entry(&buffer, entries);
    }

    Ok(())
}

fn push_file_list_entry(bytes: &[u8], entries: &mut Vec<OsString>) {
    if bytes.is_empty() {
        return;
    }

    let mut end = bytes.len();
    while end > 0 && bytes[end - 1] == b'\r' {
        end -= 1;
    }

    if end == 0 {
        return;
    }

    let trimmed = &bytes[..end];

    #[cfg(unix)]
    {
        if !trimmed.is_empty() {
            entries.push(OsString::from_vec(trimmed.to_vec()));
        }
        return;
    }

    #[cfg(not(unix))]
    {
        let text = String::from_utf8_lossy(trimmed).into_owned();
        if !text.is_empty() {
            entries.push(OsString::from(text));
        }
    }
}

fn parse_bandwidth_limit(argument: &OsStr) -> Result<Option<BandwidthLimit>, Message> {
    let text = argument.to_string_lossy();
    match BandwidthLimit::parse(&text) {
        Ok(Some(limit)) => Ok(Some(limit)),
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

fn render_module_list<W: Write, E: Write>(
    stdout: &mut W,
    stderr: &mut E,
    list: &rsync_core::client::ModuleList,
) -> io::Result<()> {
    for warning in list.warnings() {
        writeln!(stderr, "@WARNING: {}", warning)?;
    }

    for line in list.motd_lines() {
        writeln!(stdout, "{}", line)?;
    }

    for entry in list.entries() {
        if let Some(comment) = entry.comment() {
            writeln!(stdout, "{}\t{}", entry.name(), comment)?;
        } else {
            writeln!(stdout, "{}", entry.name())?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsync_core::client::FilterRuleKind;
    use rsync_filters::{FilterRule as EngineFilterRule, FilterSet};
    use std::ffi::OsStr;
    use std::io::{BufRead, BufReader, Seek, SeekFrom, Write};
    use std::net::{TcpListener, TcpStream};
    use std::thread;
    use std::time::Duration;

    #[cfg(unix)]
    use std::os::unix::ffi::OsStrExt;

    #[cfg(feature = "xattr")]
    use xattr;

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
    fn version_flag_ignores_additional_operands() {
        let (code, stdout, stderr) = run_with_args([
            OsStr::new("oc-rsync"),
            OsStr::new("--version"),
            OsStr::new("source"),
        ]);

        assert_eq!(code, 0);
        assert!(stderr.is_empty());

        let expected = VersionInfoReport::default().human_readable();
        assert_eq!(stdout, expected.into_bytes());
    }

    #[test]
    fn short_version_flag_ignores_additional_operands() {
        let (code, stdout, stderr) = run_with_args([
            OsStr::new("oc-rsync"),
            OsStr::new("-V"),
            OsStr::new("source"),
            OsStr::new("dest"),
        ]);

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
    fn verbose_transfer_emits_event_lines() {
        use tempfile::tempdir;

        let tmp = tempdir().expect("tempdir");
        let source = tmp.path().join("file.txt");
        let destination = tmp.path().join("out.txt");
        std::fs::write(&source, b"verbose").expect("write source");

        let (code, stdout, stderr) = run_with_args([
            OsString::from("oc-rsync"),
            OsString::from("-v"),
            source.clone().into_os_string(),
            destination.clone().into_os_string(),
        ]);

        assert_eq!(code, 0);
        assert!(stderr.is_empty());

        let rendered = String::from_utf8(stdout).expect("verbose output is UTF-8");
        assert!(rendered.contains("file.txt"));
        assert!(!rendered.contains("Total transferred"));
        assert!(rendered.contains("sent 7 bytes  received 0 bytes"));
        assert!(rendered.contains("total size is 7  speedup is 1.00"));
        assert_eq!(
            std::fs::read(destination).expect("read destination"),
            b"verbose"
        );
    }

    #[test]
    fn progress_transfer_reports_totals() {
        use tempfile::tempdir;

        let tmp = tempdir().expect("tempdir");
        let source = tmp.path().join("progress.txt");
        let destination = tmp.path().join("progress.out");
        std::fs::write(&source, b"progress").expect("write source");

        let (code, stdout, stderr) = run_with_args([
            OsString::from("oc-rsync"),
            OsString::from("--progress"),
            source.clone().into_os_string(),
            destination.clone().into_os_string(),
        ]);

        assert_eq!(code, 0);
        assert!(stderr.is_empty());

        let rendered = String::from_utf8(stdout).expect("progress output is UTF-8");
        assert!(rendered.contains("progress.txt"));
        assert!(rendered.contains("Total transferred"));
        assert_eq!(
            std::fs::read(destination).expect("read destination"),
            b"progress"
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
    fn transfer_request_with_sparse_preserves_holes() {
        use std::os::unix::fs::MetadataExt;
        use tempfile::tempdir;

        let tmp = tempdir().expect("tempdir");
        let source = tmp.path().join("source.bin");
        let mut source_file = std::fs::File::create(&source).expect("create source");
        source_file.write_all(&[0x10]).expect("write leading byte");
        source_file
            .seek(SeekFrom::Start(1 * 1024 * 1024))
            .expect("seek to hole");
        source_file.write_all(&[0x20]).expect("write trailing byte");
        source_file.set_len(3 * 1024 * 1024).expect("extend source");

        let dense_dest = tmp.path().join("dense.bin");
        let sparse_dest = tmp.path().join("sparse.bin");

        let (code, stdout, stderr) = run_with_args([
            OsString::from("oc-rsync"),
            source.clone().into_os_string(),
            dense_dest.clone().into_os_string(),
        ]);
        assert_eq!(code, 0);
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());

        let (code, stdout, stderr) = run_with_args([
            OsString::from("oc-rsync"),
            OsString::from("--sparse"),
            source.into_os_string(),
            sparse_dest.clone().into_os_string(),
        ]);
        assert_eq!(code, 0);
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());

        let dense_meta = std::fs::metadata(&dense_dest).expect("dense metadata");
        let sparse_meta = std::fs::metadata(&sparse_dest).expect("sparse metadata");

        assert_eq!(dense_meta.len(), sparse_meta.len());
        assert!(sparse_meta.blocks() < dense_meta.blocks());
    }

    #[test]
    fn transfer_request_with_files_from_copies_listed_sources() {
        use tempfile::tempdir;

        let tmp = tempdir().expect("tempdir");
        let source_a = tmp.path().join("files-from-a.txt");
        let source_b = tmp.path().join("files-from-b.txt");
        std::fs::write(&source_a, b"files-from-a").expect("write source a");
        std::fs::write(&source_b, b"files-from-b").expect("write source b");

        let list_path = tmp.path().join("files-from.list");
        let list_contents = format!("{}\n{}\n", source_a.display(), source_b.display());
        std::fs::write(&list_path, list_contents).expect("write list");

        let dest_dir = tmp.path().join("files-from-dest");
        std::fs::create_dir(&dest_dir).expect("create dest");

        let (code, stdout, stderr) = run_with_args([
            OsString::from("oc-rsync"),
            OsString::from(format!("--files-from={}", list_path.display())),
            dest_dir.clone().into_os_string(),
        ]);

        assert_eq!(code, 0);
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());

        let copied_a = dest_dir.join(source_a.file_name().expect("file name a"));
        let copied_b = dest_dir.join(source_b.file_name().expect("file name b"));
        assert_eq!(
            std::fs::read(&copied_a).expect("read copied a"),
            b"files-from-a"
        );
        assert_eq!(
            std::fs::read(&copied_b).expect("read copied b"),
            b"files-from-b"
        );
    }

    #[test]
    fn transfer_request_with_from0_reads_null_separated_list() {
        use tempfile::tempdir;

        let tmp = tempdir().expect("tempdir");
        let source_a = tmp.path().join("from0-a.txt");
        let source_b = tmp.path().join("from0-b.txt");
        std::fs::write(&source_a, b"from0-a").expect("write source a");
        std::fs::write(&source_b, b"from0-b").expect("write source b");

        let mut bytes = Vec::new();
        bytes.extend_from_slice(source_a.display().to_string().as_bytes());
        bytes.push(0);
        bytes.extend_from_slice(source_b.display().to_string().as_bytes());
        bytes.push(0);
        let list_path = tmp.path().join("files-from0.list");
        std::fs::write(&list_path, bytes).expect("write list");

        let dest_dir = tmp.path().join("files-from0-dest");
        std::fs::create_dir(&dest_dir).expect("create dest");

        let (code, stdout, stderr) = run_with_args([
            OsString::from("oc-rsync"),
            OsString::from("--from0"),
            OsString::from(format!("--files-from={}", list_path.display())),
            dest_dir.clone().into_os_string(),
        ]);

        assert_eq!(code, 0);
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());

        let copied_a = dest_dir.join(source_a.file_name().expect("file name a"));
        let copied_b = dest_dir.join(source_b.file_name().expect("file name b"));
        assert_eq!(std::fs::read(&copied_a).expect("read copied a"), b"from0-a");
        assert_eq!(std::fs::read(&copied_b).expect("read copied b"), b"from0-b");
    }

    #[test]
    fn files_from_reports_read_failures() {
        use tempfile::tempdir;

        let tmp = tempdir().expect("tempdir");
        let missing = tmp.path().join("missing.list");
        let dest_dir = tmp.path().join("files-from-error-dest");
        std::fs::create_dir(&dest_dir).expect("create dest");

        let (code, stdout, stderr) = run_with_args([
            OsString::from("oc-rsync"),
            OsString::from(format!("--files-from={}", missing.display())),
            dest_dir.into_os_string(),
        ]);

        assert_eq!(code, 1);
        assert!(stdout.is_empty());
        let rendered = String::from_utf8(stderr).expect("utf8");
        assert!(rendered.contains("failed to read file list"));
    }

    #[cfg(unix)]
    #[test]
    fn files_from_preserves_non_utf8_entries() {
        use tempfile::tempdir;

        let tmp = tempdir().expect("tempdir");
        let list_path = tmp.path().join("binary.list");
        std::fs::write(&list_path, [b'f', b'o', 0x80, b'\n']).expect("write binary list");

        let entries =
            load_file_list_operands(&[list_path.into_os_string()], false).expect("load entries");

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].as_os_str().as_bytes(), b"fo\x80");
    }

    #[test]
    fn from0_reader_accepts_missing_trailing_separator() {
        let data = b"alpha\0beta\0gamma";
        let mut reader = BufReader::new(&data[..]);
        let mut entries = Vec::new();

        read_file_list_from_reader(&mut reader, true, &mut entries).expect("read list");

        assert_eq!(
            entries,
            vec![
                OsString::from("alpha"),
                OsString::from("beta"),
                OsString::from("gamma"),
            ]
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

    #[cfg(unix)]
    #[test]
    fn transfer_request_with_perms_preserves_mode() {
        use filetime::{FileTime, set_file_times};
        use std::os::unix::fs::PermissionsExt;
        use tempfile::tempdir;

        let tmp = tempdir().expect("tempdir");
        let source = tmp.path().join("source-perms.txt");
        let destination = tmp.path().join("dest-perms.txt");
        std::fs::write(&source, b"data").expect("write source");
        let atime = FileTime::from_unix_time(1_700_070_000, 0);
        let mtime = FileTime::from_unix_time(1_700_080_000, 0);
        set_file_times(&source, atime, mtime).expect("set times");
        std::fs::set_permissions(&source, PermissionsExt::from_mode(0o640)).expect("set perms");

        let (code, stdout, stderr) = run_with_args([
            OsString::from("oc-rsync"),
            OsString::from("--perms"),
            OsString::from("--times"),
            source.clone().into_os_string(),
            destination.clone().into_os_string(),
        ]);

        assert_eq!(code, 0);
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());

        let metadata = std::fs::metadata(&destination).expect("dest metadata");
        assert_eq!(metadata.permissions().mode() & 0o777, 0o640);
        let dest_mtime = FileTime::from_last_modification_time(&metadata);
        assert_eq!(dest_mtime, mtime);
    }

    #[cfg(unix)]
    #[test]
    fn transfer_request_with_no_perms_overrides_archive() {
        use std::os::unix::fs::PermissionsExt;
        use tempfile::tempdir;

        let tmp = tempdir().expect("tempdir");
        let source = tmp.path().join("source-no-perms.txt");
        let destination = tmp.path().join("dest-no-perms.txt");
        std::fs::write(&source, b"data").expect("write source");
        std::fs::set_permissions(&source, PermissionsExt::from_mode(0o600)).expect("set perms");

        let (code, stdout, stderr) = run_with_args([
            OsString::from("oc-rsync"),
            OsString::from("-a"),
            OsString::from("--no-perms"),
            source.clone().into_os_string(),
            destination.clone().into_os_string(),
        ]);

        assert_eq!(code, 0);
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());

        let metadata = std::fs::metadata(&destination).expect("dest metadata");
        assert_ne!(metadata.permissions().mode() & 0o777, 0o600);
    }

    #[test]
    fn parse_args_recognises_perms_and_times_flags() {
        let parsed = parse_args([
            OsString::from("oc-rsync"),
            OsString::from("--perms"),
            OsString::from("--times"),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .expect("parse");

        assert_eq!(parsed.perms, Some(true));
        assert_eq!(parsed.times, Some(true));

        let parsed = parse_args([
            OsString::from("oc-rsync"),
            OsString::from("-a"),
            OsString::from("--no-perms"),
            OsString::from("--no-times"),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .expect("parse");

        assert_eq!(parsed.perms, Some(false));
        assert_eq!(parsed.times, Some(false));
    }

    #[test]
    fn parse_args_recognises_owner_overrides() {
        let parsed = parse_args([
            OsString::from("oc-rsync"),
            OsString::from("--owner"),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .expect("parse");

        assert_eq!(parsed.owner, Some(true));
        assert_eq!(parsed.group, None);

        let parsed = parse_args([
            OsString::from("oc-rsync"),
            OsString::from("-a"),
            OsString::from("--no-owner"),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .expect("parse");

        assert_eq!(parsed.owner, Some(false));
        assert!(parsed.archive);
    }

    #[test]
    fn parse_args_recognises_group_overrides() {
        let parsed = parse_args([
            OsString::from("oc-rsync"),
            OsString::from("--group"),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .expect("parse");

        assert_eq!(parsed.group, Some(true));
        assert_eq!(parsed.owner, None);

        let parsed = parse_args([
            OsString::from("oc-rsync"),
            OsString::from("-a"),
            OsString::from("--no-group"),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .expect("parse");

        assert_eq!(parsed.group, Some(false));
        assert!(parsed.archive);
    }

    #[test]
    fn parse_args_recognises_numeric_ids_flags() {
        let parsed = parse_args([
            OsString::from("oc-rsync"),
            OsString::from("--numeric-ids"),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .expect("parse");

        assert_eq!(parsed.numeric_ids, Some(true));

        let parsed = parse_args([
            OsString::from("oc-rsync"),
            OsString::from("--no-numeric-ids"),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .expect("parse");

        assert_eq!(parsed.numeric_ids, Some(false));
    }

    #[test]
    fn parse_args_recognises_sparse_flags() {
        let parsed = parse_args([
            OsString::from("oc-rsync"),
            OsString::from("--sparse"),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .expect("parse");

        assert_eq!(parsed.sparse, Some(true));

        let parsed = parse_args([
            OsString::from("oc-rsync"),
            OsString::from("--no-sparse"),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .expect("parse");

        assert_eq!(parsed.sparse, Some(false));
    }

    #[test]
    fn parse_args_recognises_inplace_flags() {
        let parsed = parse_args([
            OsString::from("oc-rsync"),
            OsString::from("--inplace"),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .expect("parse");

        assert_eq!(parsed.inplace, Some(true));

        let parsed = parse_args([
            OsString::from("oc-rsync"),
            OsString::from("--no-inplace"),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .expect("parse");

        assert_eq!(parsed.inplace, Some(false));
    }

    #[test]
    fn parse_args_collects_filter_patterns() {
        let parsed = parse_args([
            OsString::from("oc-rsync"),
            OsString::from("--exclude"),
            OsString::from("*.tmp"),
            OsString::from("--include"),
            OsString::from("important/**"),
            OsString::from("--filter"),
            OsString::from("+ staging/**"),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .expect("parse");

        assert_eq!(parsed.excludes, vec![OsString::from("*.tmp")]);
        assert_eq!(parsed.includes, vec![OsString::from("important/**")]);
        assert_eq!(parsed.filters, vec![OsString::from("+ staging/**")]);
    }

    #[test]
    fn parse_args_collects_filter_files() {
        let parsed = parse_args([
            OsString::from("oc-rsync"),
            OsString::from("--exclude-from"),
            OsString::from("excludes.txt"),
            OsString::from("--include-from"),
            OsString::from("includes.txt"),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .expect("parse");

        assert_eq!(parsed.exclude_from, vec![OsString::from("excludes.txt")]);
        assert_eq!(parsed.include_from, vec![OsString::from("includes.txt")]);
    }

    #[test]
    fn parse_args_collects_files_from_paths() {
        let parsed = parse_args([
            OsString::from("oc-rsync"),
            OsString::from("--files-from"),
            OsString::from("list-a"),
            OsString::from("--files-from"),
            OsString::from("list-b"),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .expect("parse");

        assert_eq!(
            parsed.files_from,
            vec![OsString::from("list-a"), OsString::from("list-b")]
        );
    }

    #[test]
    fn parse_args_sets_from0_flag() {
        let parsed = parse_args([
            OsString::from("oc-rsync"),
            OsString::from("--from0"),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .expect("parse");

        assert!(parsed.from0);
    }

    #[test]
    fn parse_filter_directive_accepts_include_and_exclude() {
        let include =
            parse_filter_directive(OsStr::new("+ assets/**")).expect("include rule parses");
        assert_eq!(
            include,
            FilterDirective::Rule(FilterRuleSpec::include("assets/**".to_string()))
        );

        let exclude = parse_filter_directive(OsStr::new("- *.bak")).expect("exclude rule parses");
        assert_eq!(
            exclude,
            FilterDirective::Rule(FilterRuleSpec::exclude("*.bak".to_string()))
        );

        let include_keyword =
            parse_filter_directive(OsStr::new("include logs/**")).expect("keyword include parses");
        assert_eq!(
            include_keyword,
            FilterDirective::Rule(FilterRuleSpec::include("logs/**".to_string()))
        );

        let exclude_keyword =
            parse_filter_directive(OsStr::new("exclude *.tmp")).expect("keyword exclude parses");
        assert_eq!(
            exclude_keyword,
            FilterDirective::Rule(FilterRuleSpec::exclude("*.tmp".to_string()))
        );

        let protect_keyword = parse_filter_directive(OsStr::new("protect backups/**"))
            .expect("keyword protect parses");
        assert_eq!(
            protect_keyword,
            FilterDirective::Rule(FilterRuleSpec::protect("backups/**".to_string()))
        );
    }

    #[test]
    fn parse_filter_directive_rejects_missing_pattern() {
        let error =
            parse_filter_directive(OsStr::new("+   ")).expect_err("missing pattern should error");
        let rendered = error.to_string();
        assert!(rendered.contains("missing a pattern"));
    }

    #[test]
    fn parse_filter_directive_accepts_merge() {
        let directive =
            parse_filter_directive(OsStr::new("merge filters.txt")).expect("merge directive");
        assert_eq!(
            directive,
            FilterDirective::Merge(OsString::from("filters.txt"))
        );
    }

    #[test]
    fn parse_filter_directive_rejects_merge_without_path() {
        let error = parse_filter_directive(OsStr::new("merge "))
            .expect_err("missing merge path should error");
        let rendered = error.to_string();
        assert!(rendered.contains("missing a file path"));
    }

    #[test]
    fn parse_filter_directive_rejects_merge_with_modifiers() {
        let error = parse_filter_directive(OsStr::new("merge,- filters"))
            .expect_err("unsupported modifiers should error");
        let rendered = error.to_string();
        assert!(rendered.contains("unsupported modifiers"));
    }

    #[test]
    fn transfer_request_with_times_preserves_timestamp() {
        use filetime::{FileTime, set_file_times};
        use tempfile::tempdir;

        let tmp = tempdir().expect("tempdir");
        let source = tmp.path().join("source-times.txt");
        let destination = tmp.path().join("dest-times.txt");
        std::fs::write(&source, b"data").expect("write source");
        let mtime = FileTime::from_unix_time(1_700_090_000, 500_000_000);
        set_file_times(&source, mtime, mtime).expect("set times");

        let (code, stdout, stderr) = run_with_args([
            OsString::from("oc-rsync"),
            OsString::from("--times"),
            source.clone().into_os_string(),
            destination.clone().into_os_string(),
        ]);

        assert_eq!(code, 0);
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());

        let metadata = std::fs::metadata(&destination).expect("dest metadata");
        let dest_mtime = FileTime::from_last_modification_time(&metadata);
        assert_eq!(dest_mtime, mtime);
    }

    #[test]
    fn transfer_request_with_filter_excludes_patterns() {
        use tempfile::tempdir;

        let tmp = tempdir().expect("tempdir");
        let source_root = tmp.path().join("source");
        let dest_root = tmp.path().join("dest");
        std::fs::create_dir_all(&source_root).expect("create source root");
        std::fs::create_dir_all(&dest_root).expect("create dest root");
        std::fs::write(source_root.join("keep.txt"), b"keep").expect("write keep");
        std::fs::write(source_root.join("skip.tmp"), b"skip").expect("write skip");

        let (code, stdout, stderr) = run_with_args([
            OsString::from("oc-rsync"),
            OsString::from("--filter"),
            OsString::from("- *.tmp"),
            source_root.clone().into_os_string(),
            dest_root.clone().into_os_string(),
        ]);

        assert_eq!(code, 0);
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());

        let copied_root = dest_root.join("source");
        assert!(copied_root.join("keep.txt").exists());
        assert!(!copied_root.join("skip.tmp").exists());
    }

    #[test]
    fn transfer_request_with_filter_merge_applies_rules() {
        use tempfile::tempdir;

        let tmp = tempdir().expect("tempdir");
        let source_root = tmp.path().join("source");
        let dest_root = tmp.path().join("dest");
        std::fs::create_dir_all(&source_root).expect("create source root");
        std::fs::create_dir_all(&dest_root).expect("create dest root");
        std::fs::write(source_root.join("keep.txt"), b"keep").expect("write keep");
        std::fs::write(source_root.join("skip.tmp"), b"skip").expect("write skip");

        let filter_file = tmp.path().join("filters.txt");
        std::fs::write(&filter_file, "- *.tmp\n").expect("write filter file");

        let filter_arg = OsString::from(format!("merge {}", filter_file.display()));

        let (code, stdout, stderr) = run_with_args([
            OsString::from("oc-rsync"),
            OsString::from("--filter"),
            filter_arg,
            source_root.clone().into_os_string(),
            dest_root.clone().into_os_string(),
        ]);

        assert_eq!(code, 0);
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());

        let copied_root = dest_root.join("source");
        assert!(copied_root.join("keep.txt").exists());
        assert!(!copied_root.join("skip.tmp").exists());
    }

    #[test]
    fn transfer_request_with_filter_protect_preserves_destination_entry() {
        use tempfile::tempdir;

        let tmp = tempdir().expect("tempdir");
        let source_root = tmp.path().join("source");
        let dest_root = tmp.path().join("dest");
        std::fs::create_dir_all(&source_root).expect("create source root");
        std::fs::create_dir_all(&dest_root).expect("create dest root");

        let dest_subdir = dest_root.join("source");
        std::fs::create_dir_all(&dest_subdir).expect("create destination contents");
        std::fs::write(dest_subdir.join("keep.txt"), b"keep").expect("write dest keep");

        let (code, stdout, stderr) = run_with_args([
            OsString::from("oc-rsync"),
            OsString::from("--delete"),
            OsString::from("--filter"),
            OsString::from("protect keep.txt"),
            source_root.clone().into_os_string(),
            dest_root.clone().into_os_string(),
        ]);

        assert_eq!(code, 0);
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());

        let copied_root = dest_root.join("source");
        assert!(copied_root.join("keep.txt").exists());
    }

    #[test]
    fn transfer_request_with_filter_merge_detects_recursion() {
        use tempfile::tempdir;

        let tmp = tempdir().expect("tempdir");
        let source_root = tmp.path().join("source");
        let dest_root = tmp.path().join("dest");
        std::fs::create_dir_all(&source_root).expect("create source root");
        std::fs::create_dir_all(&dest_root).expect("create dest root");

        let filter_file = tmp.path().join("filters.txt");
        std::fs::write(&filter_file, format!("merge {}\n", filter_file.display()))
            .expect("write recursive filter");

        let filter_arg = OsString::from(format!("merge {}", filter_file.display()));

        let (code, stdout, stderr) = run_with_args([
            OsString::from("oc-rsync"),
            OsString::from("--filter"),
            filter_arg,
            source_root.into_os_string(),
            dest_root.into_os_string(),
        ]);

        assert_eq!(code, 1);
        assert!(stdout.is_empty());
        let rendered = String::from_utf8_lossy(&stderr);
        assert!(rendered.contains("recursive filter merge"));
    }

    #[test]
    fn transfer_request_with_exclude_from_skips_patterns() {
        use tempfile::tempdir;

        let tmp = tempdir().expect("tempdir");
        let source_root = tmp.path().join("source");
        let dest_root = tmp.path().join("dest");
        std::fs::create_dir_all(&source_root).expect("create source root");
        std::fs::create_dir_all(&dest_root).expect("create dest root");
        std::fs::write(source_root.join("keep.txt"), b"keep").expect("write keep");
        std::fs::write(source_root.join("skip.tmp"), b"skip").expect("write skip");

        let exclude_file = tmp.path().join("filters.txt");
        std::fs::write(&exclude_file, "# comment\n\n*.tmp\n").expect("write filters");

        let (code, stdout, stderr) = run_with_args([
            OsString::from("oc-rsync"),
            OsString::from("--exclude-from"),
            exclude_file.as_os_str().to_os_string(),
            source_root.clone().into_os_string(),
            dest_root.clone().into_os_string(),
        ]);

        assert_eq!(code, 0);
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());

        let copied_root = dest_root.join("source");
        assert!(copied_root.join("keep.txt").exists());
        assert!(!copied_root.join("skip.tmp").exists());
    }

    #[test]
    fn transfer_request_with_include_from_reinstate_patterns() {
        use tempfile::tempdir;

        let tmp = tempdir().expect("tempdir");
        let source_root = tmp.path().join("source");
        let dest_root = tmp.path().join("dest");
        let keep_dir = source_root.join("keep");
        std::fs::create_dir_all(&keep_dir).expect("create keep dir");
        std::fs::create_dir_all(&dest_root).expect("create dest root");
        std::fs::write(keep_dir.join("file.txt"), b"keep").expect("write keep");
        std::fs::write(source_root.join("skip.tmp"), b"skip").expect("write skip");

        let include_file = tmp.path().join("includes.txt");
        std::fs::write(&include_file, "keep/\nkeep/**\n").expect("write include file");

        let mut expected_rules = Vec::new();
        expected_rules.push(FilterRuleSpec::exclude("*".to_string()));
        append_filter_rules_from_files(
            &mut expected_rules,
            &[include_file.as_os_str().to_os_string()],
            FilterRuleKind::Include,
        )
        .expect("load include patterns");

        let engine_rules = expected_rules.iter().map(|rule| match rule.kind() {
            FilterRuleKind::Include => EngineFilterRule::include(rule.pattern()),
            FilterRuleKind::Exclude => EngineFilterRule::exclude(rule.pattern()),
            FilterRuleKind::Protect => EngineFilterRule::protect(rule.pattern()),
        });
        let filter_set = FilterSet::from_rules(engine_rules).expect("filters");
        assert!(filter_set.allows(std::path::Path::new("keep"), true));
        assert!(filter_set.allows(std::path::Path::new("keep/file.txt"), false));
        assert!(!filter_set.allows(std::path::Path::new("skip.tmp"), false));

        let mut source_operand = source_root.clone().into_os_string();
        source_operand.push(std::path::MAIN_SEPARATOR.to_string());

        let (code, stdout, stderr) = run_with_args([
            OsString::from("oc-rsync"),
            OsString::from("--exclude"),
            OsString::from("*"),
            OsString::from("--include-from"),
            include_file.as_os_str().to_os_string(),
            source_operand,
            dest_root.clone().into_os_string(),
        ]);

        assert_eq!(code, 0);
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());

        assert!(dest_root.join("keep/file.txt").exists());
        assert!(!dest_root.join("skip.tmp").exists());
    }

    #[test]
    fn transfer_request_reports_filter_file_errors() {
        let (code, stdout, stderr) = run_with_args([
            OsString::from("oc-rsync"),
            OsString::from("--exclude-from"),
            OsString::from("missing.txt"),
            OsString::from("src"),
            OsString::from("dst"),
        ]);

        assert_eq!(code, 1);
        assert!(stdout.is_empty());
        let rendered = String::from_utf8(stderr).expect("diagnostic utf8");
        assert!(rendered.contains("failed to read filter file 'missing.txt'"));
        assert!(rendered.contains("[client=3.4.1-rust]"));
    }

    #[test]
    fn load_filter_file_patterns_skips_comments_and_trims_crlf() {
        use tempfile::tempdir;

        let tmp = tempdir().expect("tempdir");
        let path = tmp.path().join("filters.txt");
        std::fs::write(&path, b"# comment\r\n\r\n include \r\npattern\r\n").expect("write filters");

        let patterns =
            load_filter_file_patterns(path.as_os_str()).expect("load filter patterns succeeds");

        assert_eq!(
            patterns,
            vec![" include ".to_string(), "pattern".to_string()]
        );
    }

    #[test]
    fn load_filter_file_patterns_handles_invalid_utf8() {
        use tempfile::tempdir;

        let tmp = tempdir().expect("tempdir");
        let path = tmp.path().join("filters.bin");
        std::fs::write(&path, [0xFFu8, b'\n']).expect("write invalid bytes");

        let patterns =
            load_filter_file_patterns(path.as_os_str()).expect("load filter patterns succeeds");

        assert_eq!(patterns, vec!["\u{fffd}".to_string()]);
    }

    #[test]
    fn load_filter_file_patterns_reads_from_stdin() {
        super::set_filter_stdin_input(b"keep\n# comment\n\ninclude\n".to_vec());
        let patterns =
            super::load_filter_file_patterns(OsStr::new("-")).expect("load stdin patterns");

        assert_eq!(patterns, vec!["keep".to_string(), "include".to_string()]);
    }

    #[test]
    fn transfer_request_with_no_times_overrides_archive() {
        use filetime::{FileTime, set_file_times};
        use tempfile::tempdir;

        let tmp = tempdir().expect("tempdir");
        let source = tmp.path().join("source-no-times.txt");
        let destination = tmp.path().join("dest-no-times.txt");
        std::fs::write(&source, b"data").expect("write source");
        let mtime = FileTime::from_unix_time(1_700_100_000, 0);
        set_file_times(&source, mtime, mtime).expect("set times");

        let (code, stdout, stderr) = run_with_args([
            OsString::from("oc-rsync"),
            OsString::from("-a"),
            OsString::from("--no-times"),
            source.clone().into_os_string(),
            destination.clone().into_os_string(),
        ]);

        assert_eq!(code, 0);
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());

        let metadata = std::fs::metadata(&destination).expect("dest metadata");
        let dest_mtime = FileTime::from_last_modification_time(&metadata);
        assert_ne!(dest_mtime, mtime);
    }

    #[test]
    fn checksum_with_no_times_preserves_existing_destination() {
        use filetime::{FileTime, set_file_mtime};
        use tempfile::tempdir;

        let tmp = tempdir().expect("tempdir");
        let source = tmp.path().join("source-checksum.txt");
        let destination = tmp.path().join("dest-checksum.txt");
        std::fs::write(&source, b"payload").expect("write source");

        let (code, stdout, stderr) = run_with_args([
            OsString::from("oc-rsync"),
            OsString::from("--no-times"),
            source.clone().into_os_string(),
            destination.clone().into_os_string(),
        ]);

        assert_eq!(code, 0);
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());

        let preserved = FileTime::from_unix_time(1_700_200_000, 0);
        set_file_mtime(&destination, preserved).expect("set destination mtime");

        let (code, stdout, stderr) = run_with_args([
            OsString::from("oc-rsync"),
            OsString::from("--checksum"),
            OsString::from("--no-times"),
            source.into_os_string(),
            destination.clone().into_os_string(),
        ]);

        assert_eq!(code, 0);
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());

        let metadata = std::fs::metadata(&destination).expect("dest metadata");
        let final_mtime = FileTime::from_last_modification_time(&metadata);
        assert_eq!(final_mtime, preserved);
        assert_eq!(
            std::fs::read(destination).expect("read destination"),
            b"payload"
        );
    }

    #[test]
    fn transfer_request_with_exclude_from_stdin_skips_patterns() {
        use tempfile::tempdir;

        let tmp = tempdir().expect("tempdir");
        let source_root = tmp.path().join("source");
        let dest_root = tmp.path().join("dest");
        std::fs::create_dir_all(&source_root).expect("create source root");
        std::fs::create_dir_all(&dest_root).expect("create dest root");
        std::fs::write(source_root.join("keep.txt"), b"keep").expect("write keep");
        std::fs::write(source_root.join("skip.tmp"), b"skip").expect("write skip");

        super::set_filter_stdin_input(b"*.tmp\n".to_vec());
        let (code, stdout, stderr) = run_with_args([
            OsString::from("oc-rsync"),
            OsString::from("--exclude-from"),
            OsString::from("-"),
            source_root.clone().into_os_string(),
            dest_root.clone().into_os_string(),
        ]);

        assert_eq!(code, 0);
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());

        let copied_root = dest_root.join("source");
        assert!(copied_root.join("keep.txt").exists());
        assert!(!copied_root.join("skip.tmp").exists());
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
    fn remote_daemon_listing_renders_warnings() {
        let (addr, handle) = spawn_stub_daemon(vec![
            "@WARNING: Maintenance\n",
            "@RSYNCD: OK\n",
            "module\n",
            "@RSYNCD: EXIT\n",
        ]);

        let url = format!("rsync://{}:{}/", addr.ip(), addr.port());
        let (code, stdout, stderr) =
            run_with_args([OsString::from("oc-rsync"), OsString::from(url)]);

        assert_eq!(code, 0);
        assert!(
            String::from_utf8(stdout)
                .expect("modules")
                .contains("module")
        );

        let rendered_err = String::from_utf8(stderr).expect("warnings are UTF-8");
        assert!(rendered_err.contains("@WARNING: Maintenance"));

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
        assert_eq!(parsed.owner, None);
        assert_eq!(parsed.group, None);
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
    fn checksum_flag_is_parsed() {
        let parsed = super::parse_args([
            OsString::from("oc-rsync"),
            OsString::from("--checksum"),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .expect("parse succeeds");

        assert!(parsed.checksum);
    }

    #[test]
    fn combined_archive_and_verbose_flags_are_supported() {
        use tempfile::tempdir;

        let tmp = tempdir().expect("tempdir");
        let source = tmp.path().join("combo.txt");
        let destination = tmp.path().join("combo.out");
        std::fs::write(&source, b"combo").expect("write source");

        let (code, stdout, stderr) = run_with_args([
            OsString::from("oc-rsync"),
            OsString::from("-av"),
            source.clone().into_os_string(),
            destination.clone().into_os_string(),
        ]);

        assert_eq!(code, 0);
        assert!(stderr.is_empty());

        let rendered = String::from_utf8(stdout).expect("verbose output is UTF-8");
        assert!(rendered.contains("combo.txt"));
        assert_eq!(
            std::fs::read(destination).expect("read destination"),
            b"combo"
        );
    }

    #[test]
    fn unsupported_short_option_reports_error() {
        let (code, stdout, stderr) = run_with_args([
            OsString::from("oc-rsync"),
            OsString::from("-z"),
            OsString::from("source"),
            OsString::from("dest"),
        ]);

        assert_eq!(code, 1);
        assert!(stdout.is_empty());

        let rendered = String::from_utf8(stderr).expect("diagnostic is valid UTF-8");
        assert!(
            rendered.contains("unsupported option '-z'"),
            "stderr: {rendered}"
        );
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

    #[cfg(not(feature = "xattr"))]
    #[test]
    fn xattrs_option_reports_unsupported_when_feature_disabled() {
        use tempfile::tempdir;

        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source.txt");
        let destination = temp.path().join("dest.txt");
        std::fs::write(&source, b"data").expect("write source");

        let (code, stdout, stderr) = run_with_args([
            OsString::from("oc-rsync"),
            OsString::from("--xattrs"),
            source.into_os_string(),
            destination.into_os_string(),
        ]);

        assert_eq!(code, 1);
        assert!(stdout.is_empty());
        let rendered = String::from_utf8(stderr).expect("UTF-8 error");
        assert!(rendered.contains("extended attributes are not supported on this client"));
        assert!(rendered.contains("[client=3.4.1-rust]"));
    }

    #[cfg(feature = "xattr")]
    #[test]
    fn xattrs_option_preserves_attributes() {
        use tempfile::tempdir;

        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source.txt");
        let destination = temp.path().join("dest.txt");
        std::fs::write(&source, b"attr data").expect("write source");
        xattr::set(&source, "user.test", b"value").expect("set xattr");

        let (code, stdout, stderr) = run_with_args([
            OsString::from("oc-rsync"),
            OsString::from("--xattrs"),
            source.clone().into_os_string(),
            destination.clone().into_os_string(),
        ]);

        assert_eq!(code, 0);
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());

        let copied = xattr::get(&destination, "user.test")
            .expect("read dest xattr")
            .expect("xattr present");
        assert_eq!(copied, b"value");
    }
}
