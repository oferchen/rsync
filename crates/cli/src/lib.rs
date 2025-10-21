#![deny(unsafe_code)]
#![deny(missing_docs)]
#![deny(rustdoc::broken_intra_doc_links)]

//! # Overview
//!
//! `rsync_cli` implements the thin command-line front-end for the Rust `oc-rsync`
//! workspace. The crate is intentionally small: it recognises the subset of
//! command-line switches that are currently supported (`--help`/`-h`,
//! `--version`/`-V`, `--dry-run`/`-n`, `--delete`, `--filter`, `--files-from`,
//! `--from0`, and `--bwlimit`) and delegates local copy operations to
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
use std::fs::File;
use std::io::{self, BufRead, BufReader, Write};
use std::num::NonZeroU64;
use std::path::PathBuf;

use clap::{Arg, ArgAction, Command, builder::OsStringValueParser};
use rsync_core::{
    client::{
        BandwidthLimit, ClientConfig, FilterRuleKind, FilterRuleSpec, ModuleListRequest,
        run_client as run_core_client, run_module_list,
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
    "      --exclude=PATTERN  Skip files matching PATTERN.\n",
    "      --exclude-from=FILE  Read exclude patterns from FILE.\n",
    "      --include=PATTERN  Re-include files matching PATTERN after exclusions.\n",
    "      --include-from=FILE  Read include patterns from FILE.\n",
    "      --filter=RULE  Apply filter RULE (supports '+' include and '-' exclude).\n",
    "      --files-from=FILE  Read additional source operands from FILE.\n",
    "      --from0      Treat file list entries as NUL-terminated records.\n",
    "      --bwlimit    Limit I/O bandwidth in KiB/s (0 disables the limit).\n",
    "      --owner      Preserve file ownership (requires super-user).\n",
    "      --no-owner   Disable ownership preservation.\n",
    "      --group      Preserve file group (requires suitable privileges).\n",
    "      --no-group   Disable group preservation.\n",
    "  -p, --perms      Preserve file permissions.\n",
    "      --no-perms   Disable permission preservation.\n",
    "  -t, --times      Preserve modification times.\n",
    "      --no-times   Disable modification time preservation.\n",
    "      --numeric-ids      Preserve numeric UID/GID values.\n",
    "      --no-numeric-ids   Map UID/GID values to names when possible.\n",
    "\n",
    "All SOURCE operands must reside on the local filesystem. When multiple\n",
    "sources are supplied, DEST must name a directory. Metadata preservation\n",
    "covers permissions, timestamps, and optional ownership metadata.\n",
);

/// Human-readable list of the options recognised by this development build.
const SUPPORTED_OPTIONS_LIST: &str = "--help/-h, --version/-V, --dry-run/-n, --archive/-a, --delete, --exclude, --exclude-from, --include, --include-from, --filter, --files-from, --from0, --bwlimit, --owner, --no-owner, --group, --no-group, --perms/-p, --no-perms, --times/-t, --no-times, --numeric-ids, and --no-numeric-ids";

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
    owner: Option<bool>,
    group: Option<bool>,
    perms: Option<bool>,
    times: Option<bool>,
    numeric_ids: Option<bool>,
    excludes: Vec<OsString>,
    includes: Vec<OsString>,
    exclude_from: Vec<OsString>,
    include_from: Vec<OsString>,
    filters: Vec<OsString>,
    files_from: Vec<OsString>,
    from0: bool,
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
                .help("Apply filter RULE (supports '+' include and '-' exclude).")
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
    let numeric_ids = if matches.get_flag("numeric-ids") {
        Some(true)
    } else if matches.get_flag("no-numeric-ids") {
        Some(false)
    } else {
        None
    };
    let remainder = matches
        .remove_many::<OsString>("args")
        .map(|values| values.collect())
        .unwrap_or_default();
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
        remainder,
        bwlimit,
        owner,
        group,
        perms,
        times,
        excludes,
        includes,
        exclude_from,
        include_from,
        numeric_ids,
        filters,
        files_from,
        from0,
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
    } = parsed;

    if show_help {
        let help = render_help();
        if stdout.write_all(help.as_bytes()).is_err() {
            let _ = writeln!(stdout, "{help}");
            return 1;
        }
        return 0;
    }

    if show_version && raw_remainder.is_empty() {
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
    }

    let mut transfer_operands = Vec::with_capacity(file_list_operands.len() + remainder.len());
    transfer_operands.append(&mut file_list_operands);
    transfer_operands.extend(remainder);

    let preserve_owner = owner.unwrap_or(archive);
    let preserve_group = group.unwrap_or(archive);
    let preserve_permissions = perms.unwrap_or(archive);
    let preserve_times = times.unwrap_or(archive);

    let mut builder = ClientConfig::builder()
        .transfer_args(transfer_operands)
        .dry_run(dry_run)
        .delete(delete)
        .bandwidth_limit(bandwidth_limit)
        .owner(preserve_owner)
        .group(preserve_group)
        .permissions(preserve_permissions)
        .times(preserve_times)
        .numeric_ids(numeric_ids);

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
    for rule in filters {
        match parse_filter_rule(&rule) {
            Ok(spec) => filter_rules.push(spec),
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
            let _ = summary;
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

fn parse_filter_rule(argument: &OsStr) -> Result<FilterRuleSpec, Message> {
    let text = argument.to_string_lossy();
    let trimmed = text.trim();

    if trimmed.is_empty() {
        let message = rsync_error!(
            1,
            "filter rule is empty: supply '+' or '-' followed by a pattern"
        )
        .with_role(Role::Client);
        return Err(message);
    }

    let mut chars = trimmed.chars();
    let action = chars.next().expect("non-empty after trim");
    let remainder = chars
        .as_str()
        .trim_start_matches(|ch: char| ch.is_ascii_whitespace());

    if remainder.is_empty() {
        let message = rsync_error!(
            1,
            "filter rule '{trimmed}' is missing a pattern after '{action}'"
        )
        .with_role(Role::Client);
        return Err(message);
    }

    let pattern = remainder.to_string();
    match action {
        '+' => Ok(FilterRuleSpec::include(pattern)),
        '-' => Ok(FilterRuleSpec::exclude(pattern)),
        _ => {
            let message = rsync_error!(
                1,
                "unsupported filter rule '{trimmed}': this build currently supports only '+' (include) and '-' (exclude) actions"
            )
            .with_role(Role::Client);
            Err(message)
        }
    }
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
        reader.read_to_end(&mut buffer)?;
        let mut start = 0;
        while start < buffer.len() {
            let end = match buffer[start..].iter().position(|&byte| byte == b'\0') {
                Some(offset) => start + offset,
                None => buffer.len(),
            };
            push_file_list_entry(&buffer[start..end], entries);
            if end == buffer.len() {
                break;
            }
            start = end + 1;
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

    let text = String::from_utf8_lossy(&bytes[..end]).into_owned();
    if !text.is_empty() {
        entries.push(OsString::from(text));
    }
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
    use rsync_core::client::FilterRuleKind;
    use rsync_filters::{FilterRule as EngineFilterRule, FilterSet};
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
            OsString::from("--numeric-ids"),
            OsString::from("--no-numeric-ids"),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .expect("parse");

        assert_eq!(parsed.numeric_ids, Some(false));
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
    fn parse_filter_rule_accepts_include_and_exclude() {
        let include = parse_filter_rule(OsStr::new("+ assets/**")).expect("include rule parses");
        assert_eq!(include.kind(), FilterRuleKind::Include);
        assert_eq!(include.pattern(), "assets/**");

        let exclude = parse_filter_rule(OsStr::new("- *.bak")).expect("exclude rule parses");
        assert_eq!(exclude.kind(), FilterRuleKind::Exclude);
        assert_eq!(exclude.pattern(), "*.bak");
    }

    #[test]
    fn parse_filter_rule_rejects_missing_pattern() {
        let error =
            parse_filter_rule(OsStr::new("+   ")).expect_err("missing pattern should error");
        let rendered = error.to_string();
        assert!(rendered.contains("missing a pattern"));
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
