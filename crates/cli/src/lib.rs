#![deny(unsafe_code)]
#![deny(missing_docs)]
#![deny(rustdoc::broken_intra_doc_links)]

//! # Overview
//!
//! `rsync_cli` implements the thin command-line front-end for the Rust `oc-rsync`
//! workspace. The crate is intentionally small: it recognises the subset of
//! command-line switches that are currently supported (`--help`/`-h`,
//! `--version`/`-V`, `--daemon`, `--dry-run`/`-n`, `--list-only`, `--delete`/`--delete-excluded`,
//! `--filter` (supporting `+`/`-` actions, the `!` clear directive, and
//! `merge FILE` directives), `--files-from`, `--from0`, `--bwlimit`, and
//! `--sparse`) and delegates local copy operations to
//! [`rsync_core::client::run_client`] or forwards daemon invocations to
//! [`rsync_daemon::run`]. Higher layers will eventually extend the
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
//! `--delete-excluded`,
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

use std::env;
use std::ffi::{OsStr, OsString};
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::num::{IntErrorKind, NonZeroU64};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::{Duration, SystemTime};

#[cfg(unix)]
use std::os::unix::{ffi::OsStringExt, fs::PermissionsExt};

use clap::{Arg, ArgAction, Command, builder::OsStringValueParser};
use rsync_core::{
    bandwidth::BandwidthParseError,
    client::{
        BandwidthLimit, ClientConfig, ClientEntryKind, ClientEntryMetadata, ClientEvent,
        ClientEventKind, ClientProgressObserver, ClientProgressUpdate, ClientSummary,
        DirMergeEnforcedKind, DirMergeOptions, FilterRuleKind, FilterRuleSpec, ModuleListRequest,
        TransferTimeout, run_client_with_observer as run_core_client_with_observer,
        run_module_list_with_password,
    },
    message::{Message, Role},
    rsync_error,
    version::VersionInfoReport,
};
use rsync_logging::MessageSink;
use rsync_protocol::{ParseProtocolVersionErrorKind, ProtocolVersion};
use time::{OffsetDateTime, format_description::FormatItem, macros::format_description};

/// Maximum exit code representable by a Unix process.
const MAX_EXIT_CODE: i32 = u8::MAX as i32;

/// Deterministic help text describing the CLI surface supported by this build.
const HELP_TEXT: &str = concat!(
    "oc-rsync 3.4.1-rust\n",
    "https://github.com/oferchen/rsync\n",
    "\n",
    "Usage: oc-rsync [-h] [-V] [--daemon] [-n] [-a] [-S] [-z] [--delete] [--bwlimit=RATE] SOURCE... DEST\n",
    "\n",
    "This development snapshot implements deterministic local filesystem\n",
    "copies for regular files, directories, and symbolic links. The\n",
    "following options are recognised:\n",
    "  -h, --help       Show this help message and exit.\n",
    "  -V, --version    Output version information and exit.\n",
    "      --daemon    Run as an rsync daemon (delegates to oc-rsyncd).\n",
    "  -n, --dry-run    Validate transfers without modifying the destination.\n",
    "      --list-only  List files without performing a transfer.\n",
    "  -a, --archive    Enable archive mode (implies --owner, --group, --perms, --times, --devices, and --specials).\n",
    "      --delete     Remove destination files that are absent from the source.\n",
    "      --delete-excluded  Remove excluded destination files during deletion sweeps.\n",
    "  -c, --checksum   Skip updates for files that already match by checksum.\n",
    "      --size-only  Skip files whose size matches the destination, ignoring timestamps.\n",
    "      --ignore-existing  Skip updating files that already exist at the destination.\n",
    "  -u, --update    Skip files that are newer on the destination.\n",
    "      --exclude=PATTERN  Skip files matching PATTERN.\n",
    "      --exclude-from=FILE  Read exclude patterns from FILE.\n",
    "      --include=PATTERN  Re-include files matching PATTERN after exclusions.\n",
    "      --include-from=FILE  Read include patterns from FILE.\n",
    "      --filter=RULE  Apply filter RULE (supports '+' include, '-' exclude, '!' clear, 'include PATTERN', 'exclude PATTERN', 'show PATTERN', 'hide PATTERN', 'protect PATTERN', 'exclude-if-present=FILE', 'merge FILE', and 'dir-merge[,MODS] FILE' with MODS drawn from '+', '-', 'n', 'e', 'w', 's', 'r', '/', and 'C').\n",
    "      --files-from=FILE  Read additional source operands from FILE.\n",
    "      --password-file=FILE  Read daemon passwords from FILE when contacting rsync:// daemons.\n",
    "      --no-motd    Suppress daemon MOTD lines when listing rsync:// modules.\n",
    "      --from0      Treat file list entries as NUL-terminated records.\n",
    "      --bwlimit    Limit I/O bandwidth in KiB/s (0 disables the limit).\n",
    "      --timeout=SECS  Set I/O timeout to SECS seconds (0 disables the timeout).\n",
    "      --protocol=NUM  Force protocol version NUM when accessing rsync daemons.\n",
    "  -z, --compress  Enable compression during transfers (no effect for local copies).\n",
    "      --no-compress  Disable compression.\n",
    "  -v, --verbose    Increase verbosity; repeat for more detail.\n",
    "  -R, --relative   Preserve source path components relative to the current directory.\n",
    "      --no-relative  Disable preservation of source path components.\n",
    "      --progress   Show progress information during transfers.\n",
    "      --no-progress  Disable progress reporting.\n",
    "      --stats      Output transfer statistics after completion.\n",
    "      --partial    Keep partially transferred files on errors.\n",
    "      --no-partial Discard partially transferred files on errors.\n",
    "      --remove-source-files  Remove source files after a successful transfer.\n",
    "      --remove-sent-files   Alias of --remove-source-files.\n",
    "      --inplace    Write updated data directly to destination files.\n",
    "      --no-inplace Use temporary files when updating regular files.\n",
    "  -P              Equivalent to --partial --progress.\n",
    "  -S, --sparse    Preserve sparse files by creating holes in the destination.\n",
    "      --no-sparse Disable sparse file handling.\n",
    "  -D              Equivalent to --devices --specials.\n",
    "      --devices   Preserve device files.\n",
    "      --no-devices  Disable device file preservation.\n",
    "      --specials  Preserve special files such as FIFOs.\n",
    "      --no-specials  Disable preservation of special files.\n",
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
const SUPPORTED_OPTIONS_LIST: &str = "--help/-h, --version/-V, --daemon, --dry-run/-n, --list-only, --archive/-a, --delete, --checksum/-c, --size-only, --ignore-existing, --update/-u, --exclude, --exclude-from, --include, --include-from, --filter (including exclude-if-present=FILE), --files-from, --password-file, --no-motd, --from0, --bwlimit, --timeout, --protocol, --compress/-z, --no-compress, --verbose/-v, --progress, --no-progress, --stats, --partial, --no-partial, --remove-source-files, --remove-sent-files, --inplace, --no-inplace, -P, --sparse/-S, --no-sparse, -D, --devices, --no-devices, --specials, --no-specials, --owner, --no-owner, --group, --no-group, --perms/-p, --no-perms, --times/-t, --no-times, --xattrs/-X, --no-xattrs, --numeric-ids, and --no-numeric-ids";

/// Timestamp format used for `--list-only` output.
const LIST_TIMESTAMP_FORMAT: &[FormatItem<'static>] = format_description!(
    "[year]/[month padding:zero]/[day padding:zero] [hour padding:zero]:[minute padding:zero]:[second padding:zero]"
);

/// Parsed command produced by [`parse_args`].
#[derive(Debug, Default)]
struct ParsedArgs {
    show_help: bool,
    show_version: bool,
    dry_run: bool,
    list_only: bool,
    archive: bool,
    delete: bool,
    delete_excluded: bool,
    checksum: bool,
    size_only: bool,
    ignore_existing: bool,
    update: bool,
    remainder: Vec<OsString>,
    bwlimit: Option<OsString>,
    compress: bool,
    owner: Option<bool>,
    group: Option<bool>,
    perms: Option<bool>,
    times: Option<bool>,
    numeric_ids: Option<bool>,
    sparse: Option<bool>,
    devices: Option<bool>,
    specials: Option<bool>,
    relative: Option<bool>,
    verbosity: u8,
    progress: bool,
    stats: bool,
    partial: bool,
    remove_source_files: bool,
    inplace: Option<bool>,
    excludes: Vec<OsString>,
    includes: Vec<OsString>,
    exclude_from: Vec<OsString>,
    include_from: Vec<OsString>,
    filters: Vec<OsString>,
    files_from: Vec<OsString>,
    from0: bool,
    xattrs: Option<bool>,
    no_motd: bool,
    password_file: Option<OsString>,
    protocol: Option<OsString>,
    timeout: Option<OsString>,
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
            Arg::new("list-only")
                .long("list-only")
                .help("List files without performing a transfer.")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("archive")
                .long("archive")
                .short('a')
                .help("Enable archive mode (implies --owner, --group, --perms, and --times).")
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
            Arg::new("size-only")
                .long("size-only")
                .help("Skip files whose size already matches the destination.")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("ignore-existing")
                .long("ignore-existing")
                .help("Skip updating files that already exist at the destination.")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("update")
                .long("update")
                .short('u')
                .help("Skip files that are newer on the destination.")
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
            Arg::new("archive-devices")
                .short('D')
                .help("Preserve device and special files (equivalent to --devices --specials).")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("devices")
                .long("devices")
                .help("Preserve device files.")
                .action(ArgAction::SetTrue)
                .conflicts_with("no-devices"),
        )
        .arg(
            Arg::new("no-devices")
                .long("no-devices")
                .help("Disable device file preservation.")
                .action(ArgAction::SetTrue)
                .conflicts_with("devices"),
        )
        .arg(
            Arg::new("specials")
                .long("specials")
                .help("Preserve special files such as FIFOs.")
                .action(ArgAction::SetTrue)
                .conflicts_with("no-specials"),
        )
        .arg(
            Arg::new("no-specials")
                .long("no-specials")
                .help("Disable preservation of special files such as FIFOs.")
                .action(ArgAction::SetTrue)
                .conflicts_with("specials"),
        )
        .arg(
            Arg::new("verbose")
                .long("verbose")
                .short('v')
                .help("Increase verbosity; may be supplied multiple times.")
                .action(ArgAction::Count),
        )
        .arg(
            Arg::new("relative")
                .long("relative")
                .short('R')
                .help("Preserve source path components relative to the current directory.")
                .action(ArgAction::SetTrue)
                .overrides_with("no-relative"),
        )
        .arg(
            Arg::new("no-relative")
                .long("no-relative")
                .help("Disable preservation of source path components.")
                .action(ArgAction::SetTrue)
                .overrides_with("relative"),
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
            Arg::new("stats")
                .long("stats")
                .help("Output transfer statistics after completion.")
                .action(ArgAction::SetTrue),
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
            Arg::new("remove-source-files")
                .long("remove-source-files")
                .alias("remove-sent-files")
                .help("Remove source files after a successful transfer.")
                .action(ArgAction::SetTrue),
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
            Arg::new("delete-excluded")
                .long("delete-excluded")
                .help("Remove excluded destination files during deletion sweeps.")
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
                .help("Apply filter RULE (supports '+' include, '-' exclude, '!' clear, 'protect PATTERN', 'merge FILE', and 'dir-merge[,MODS] FILE').")
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
            Arg::new("password-file")
                .long("password-file")
                .value_name("FILE")
                .help("Read daemon passwords from FILE when contacting rsync:// daemons.")
                .value_parser(OsStringValueParser::new())
                .action(ArgAction::Set),
        )
        .arg(
            Arg::new("no-motd")
                .long("no-motd")
                .help("Suppress daemon MOTD lines when listing rsync:// modules.")
                .action(ArgAction::SetTrue),
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
            Arg::new("timeout")
                .long("timeout")
                .value_name("SECS")
                .help("Set I/O timeout in seconds (0 disables the timeout).")
                .num_args(1)
                .action(ArgAction::Set)
                .value_parser(OsStringValueParser::new()),
        )
        .arg(
            Arg::new("protocol")
                .long("protocol")
                .value_name("NUM")
                .help("Force protocol version NUM when accessing rsync daemons.")
                .num_args(1)
                .action(ArgAction::Set)
                .value_parser(OsStringValueParser::new()),
        )
        .arg(
            Arg::new("compress")
                .long("compress")
                .short('z')
                .help("Enable compression during transfers.")
                .action(ArgAction::SetTrue)
                .overrides_with("no-compress"),
        )
        .arg(
            Arg::new("no-compress")
                .long("no-compress")
                .help("Disable compression.")
                .action(ArgAction::SetTrue)
                .overrides_with("compress"),
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
    let mut dry_run = matches.get_flag("dry-run");
    let list_only = matches.get_flag("list-only");
    if list_only {
        dry_run = true;
    }
    let archive = matches.get_flag("archive");
    let mut delete = matches.get_flag("delete");
    let delete_excluded = matches.get_flag("delete-excluded");
    if delete_excluded {
        delete = true;
    }
    let mut compress = matches.get_flag("compress");
    if matches.get_flag("no-compress") {
        compress = false;
    }
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
    let archive_devices = matches.get_flag("archive-devices");
    let devices = if matches.get_flag("no-devices") {
        Some(false)
    } else if matches.get_flag("devices") {
        Some(true)
    } else if archive_devices {
        Some(true)
    } else {
        None
    };
    let specials = if matches.get_flag("no-specials") {
        Some(false)
    } else if matches.get_flag("specials") {
        Some(true)
    } else if archive_devices {
        Some(true)
    } else {
        None
    };
    let relative = if matches.get_flag("relative") {
        Some(true)
    } else if matches.get_flag("no-relative") {
        Some(false)
    } else {
        None
    };
    let verbosity = matches.get_count("verbose") as u8;
    let mut progress = matches.get_flag("progress");
    let stats = matches.get_flag("stats");
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
    let remove_source_files = matches.get_flag("remove-source-files");
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
    let size_only = matches.get_flag("size-only");

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
    let ignore_existing = matches.get_flag("ignore-existing");
    let update = matches.get_flag("update");
    let password_file = matches.remove_one::<OsString>("password-file");
    let protocol = matches.remove_one::<OsString>("protocol");
    let timeout = matches.remove_one::<OsString>("timeout");
    let no_motd = matches.get_flag("no-motd");

    Ok(ParsedArgs {
        show_help,
        show_version,
        dry_run,
        list_only,
        archive,
        delete,
        delete_excluded,
        checksum,
        size_only,
        ignore_existing,
        update,
        remainder,
        bwlimit,
        compress,
        owner,
        group,
        perms,
        times,
        numeric_ids,
        sparse,
        devices,
        specials,
        relative,
        verbosity,
        progress,
        stats,
        partial,
        remove_source_files,
        inplace,
        excludes,
        includes,
        exclude_from,
        include_from,
        filters,
        files_from,
        from0,
        xattrs,
        no_motd,
        password_file,
        protocol,
        timeout,
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
    let mut args: Vec<OsString> = arguments.into_iter().map(Into::into).collect();
    if args.is_empty() {
        args.push(OsString::from("oc-rsync"));
    }

    if let Some(daemon_args) = daemon_mode_arguments(&args) {
        return run_daemon_mode(daemon_args, stdout, stderr);
    }

    let mut stderr_sink = MessageSink::new(stderr);
    match parse_args(args) {
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

/// Returns the daemon argument vector when `--daemon` is present.
fn daemon_mode_arguments(args: &[OsString]) -> Option<Vec<OsString>> {
    if args.is_empty() {
        return None;
    }

    let mut daemon_args = Vec::with_capacity(args.len());
    daemon_args.push(OsString::from("oc-rsyncd"));

    let mut found = false;
    let mut reached_double_dash = false;

    for arg in args.iter().skip(1) {
        if !reached_double_dash && arg == "--" {
            reached_double_dash = true;
            daemon_args.push(arg.clone());
            continue;
        }

        if !reached_double_dash && arg == "--daemon" {
            found = true;
            continue;
        }

        daemon_args.push(arg.clone());
    }

    if found { Some(daemon_args) } else { None }
}

/// Delegates execution to the daemon front-end.
fn run_daemon_mode<Out, Err>(args: Vec<OsString>, stdout: &mut Out, stderr: &mut Err) -> i32
where
    Out: Write,
    Err: Write,
{
    rsync_daemon::run(args, stdout, stderr)
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
        list_only,
        archive,
        delete,
        delete_excluded,
        checksum,
        size_only,
        ignore_existing,
        update,
        remainder: raw_remainder,
        bwlimit,
        compress,
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
        devices,
        specials,
        relative,
        verbosity,
        progress,
        stats,
        partial,
        remove_source_files,
        inplace,
        xattrs,
        no_motd,
        password_file,
        protocol,
        timeout,
    } = parsed;

    let password_file = password_file.map(PathBuf::from);
    let desired_protocol = match protocol {
        Some(value) => match parse_protocol_version_arg(value.as_os_str()) {
            Ok(version) => Some(version),
            Err(message) => {
                if write_message(&message, stderr).is_err() {
                    let fallback = message.to_string();
                    let _ = writeln!(stderr.writer_mut(), "{fallback}");
                }
                return 1;
            }
        },
        None => None,
    };

    let timeout_setting = match timeout {
        Some(value) => match parse_timeout_argument(value.as_os_str()) {
            Ok(setting) => setting,
            Err(message) => {
                if write_message(&message, stderr).is_err() {
                    let fallback = message.to_string();
                    let _ = writeln!(stderr.writer_mut(), "{fallback}");
                }
                return 1;
            }
        },
        None => TransferTimeout::Default,
    };

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
                let request = if let Some(protocol) = desired_protocol {
                    request.with_protocol(protocol)
                } else {
                    request
                };
                let password_override = match load_optional_password(password_file.as_deref()) {
                    Ok(secret) => secret,
                    Err(message) => {
                        if write_message(&message, stderr).is_err() {
                            let fallback = message.to_string();
                            let _ = writeln!(stderr.writer_mut(), "{}", fallback);
                        }
                        return 1;
                    }
                };

                return match run_module_list_with_password(
                    request,
                    password_override,
                    timeout_setting,
                ) {
                    Ok(list) => {
                        if render_module_list(stdout, stderr.writer_mut(), &list, no_motd).is_err()
                        {
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

    if desired_protocol.is_some() {
        let message = rsync_error!(
            1,
            "the --protocol option may only be used when accessing an rsync daemon"
        )
        .with_role(Role::Client);
        if write_message(&message, stderr).is_err() {
            let _ = writeln!(
                stderr.writer_mut(),
                "the --protocol option may only be used when accessing an rsync daemon"
            );
        }
        return 1;
    }

    if password_file.is_some() {
        let message = rsync_error!(
            1,
            "the --password-file option may only be used when accessing an rsync daemon"
        )
        .with_role(Role::Client);
        if write_message(&message, stderr).is_err() {
            let _ = writeln!(
                stderr.writer_mut(),
                "the --password-file option may only be used when accessing an rsync daemon"
            );
        }
        return 1;
    }

    let mut transfer_operands = Vec::with_capacity(file_list_operands.len() + remainder.len());
    transfer_operands.append(&mut file_list_operands);
    transfer_operands.extend(remainder);

    let preserve_owner = owner.unwrap_or(archive);
    let preserve_group = group.unwrap_or(archive);
    let preserve_permissions = perms.unwrap_or(archive);
    let preserve_times = times.unwrap_or(archive);
    let preserve_devices = devices.unwrap_or(archive);
    let preserve_specials = specials.unwrap_or(archive);
    let sparse = sparse.unwrap_or(false);
    let relative = relative.unwrap_or(false);

    let mut builder = ClientConfig::builder()
        .transfer_args(transfer_operands)
        .dry_run(dry_run)
        .list_only(list_only)
        .delete(delete)
        .delete_excluded(delete_excluded)
        .bandwidth_limit(bandwidth_limit)
        .compress(compress)
        .owner(preserve_owner)
        .group(preserve_group)
        .permissions(preserve_permissions)
        .times(preserve_times)
        .devices(preserve_devices)
        .specials(preserve_specials)
        .checksum(checksum)
        .size_only(size_only)
        .ignore_existing(ignore_existing)
        .update(update)
        .numeric_ids(numeric_ids)
        .sparse(sparse)
        .relative_paths(relative)
        .verbosity(verbosity)
        .progress(progress)
        .stats(stats)
        .partial(partial)
        .remove_source_files(remove_source_files)
        .inplace(inplace.unwrap_or(false))
        .timeout(timeout_setting);
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
    let merge_base = env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    for filter in filters {
        match parse_filter_directive(filter.as_os_str()) {
            Ok(FilterDirective::Rule(spec)) => filter_rules.push(spec),
            Ok(FilterDirective::Merge(source)) => {
                if let Err(message) = apply_merge_directive(
                    source,
                    merge_base.as_path(),
                    &mut filter_rules,
                    &mut merge_stack,
                ) {
                    if write_message(&message, stderr).is_err() {
                        let _ = writeln!(stderr.writer_mut(), "{}", message);
                    }
                    return 1;
                }
            }
            Ok(FilterDirective::Clear) => filter_rules.clear(),
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

    let mut live_progress = if progress {
        Some(LiveProgress::new(stdout))
    } else {
        None
    };

    let result = {
        let observer = live_progress
            .as_mut()
            .map(|observer| observer as &mut dyn ClientProgressObserver);
        run_core_client_with_observer(config, observer)
    };

    match result {
        Ok(summary) => {
            let progress_rendered_live =
                live_progress.as_ref().map_or(false, LiveProgress::rendered);

            if let Some(observer) = live_progress {
                if let Err(error) = observer.finish() {
                    let _ = writeln!(stdout, "warning: failed to render progress output: {error}");
                }
            }

            if let Err(error) = emit_transfer_summary(
                &summary,
                verbosity,
                progress,
                stats,
                progress_rendered_live,
                list_only,
                stdout,
            ) {
                let _ = writeln!(
                    stdout,
                    "warning: failed to render transfer summary: {error}"
                );
            }
            0
        }
        Err(error) => {
            if let Some(observer) = live_progress {
                if let Err(err) = observer.finish() {
                    let _ = writeln!(stdout, "warning: failed to render progress output: {err}");
                }
            }

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

/// Emits verbose, statistics, and progress-oriented output derived from a [`ClientSummary`].
struct LiveProgress<'a, W: Write> {
    writer: &'a mut W,
    rendered: bool,
    error: Option<io::Error>,
    active_path: Option<PathBuf>,
    line_active: bool,
}

impl<'a, W: Write> LiveProgress<'a, W> {
    fn new(writer: &'a mut W) -> Self {
        Self {
            writer,
            rendered: false,
            error: None,
            active_path: None,
            line_active: false,
        }
    }

    fn rendered(&self) -> bool {
        self.rendered
    }

    fn record_error(&mut self, error: io::Error) {
        if self.error.is_none() {
            self.error = Some(error);
        }
    }

    fn finish(self) -> io::Result<()> {
        if let Some(error) = self.error {
            return Err(error);
        }

        if self.line_active {
            writeln!(self.writer)?;
        }

        Ok(())
    }
}

impl<'a, W: Write> ClientProgressObserver for LiveProgress<'a, W> {
    fn on_progress(&mut self, update: &ClientProgressUpdate) {
        if self.error.is_some() {
            return;
        }

        let event = update.event();
        let total = update.total().max(update.index());
        let remaining = total.saturating_sub(update.index());

        let write_result = (|| -> io::Result<()> {
            let relative = event.relative_path();
            let path_changed = self
                .active_path
                .as_deref()
                .map_or(true, |path| path != relative);

            if path_changed {
                if self.line_active {
                    writeln!(self.writer)?;
                    self.line_active = false;
                }
                writeln!(self.writer, "{}", relative.display())?;
                self.active_path = Some(relative.to_path_buf());
            }

            let bytes = event.bytes_transferred();
            let size_field = format!("{:>15}", format_progress_bytes(bytes));
            let percent = format_progress_percent(bytes, update.total_bytes());
            let percent_field = format!("{:>4}", percent);
            let rate_field = format!("{:>12}", format_progress_rate(bytes, event.elapsed()));
            let elapsed_field = format!("{:>11}", format_progress_elapsed(event.elapsed()));
            let xfr_index = update.index();

            if self.line_active {
                write!(self.writer, "\r")?;
            }

            write!(
                self.writer,
                "{size_field} {percent_field} {rate_field} {elapsed_field} (xfr#{xfr_index}, to-chk={remaining}/{total})"
            )?;

            if update.is_final() {
                writeln!(self.writer)?;
                self.line_active = false;
                self.active_path = None;
            } else {
                self.line_active = true;
            }
            Ok(())
        })();

        match write_result {
            Ok(()) => {
                self.rendered = true;
            }
            Err(error) => self.record_error(error),
        }
    }
}

fn emit_transfer_summary<W: Write>(
    summary: &ClientSummary,
    verbosity: u8,
    progress: bool,
    stats: bool,
    progress_already_rendered: bool,
    list_only: bool,
    stdout: &mut W,
) -> io::Result<()> {
    let events = summary.events();

    if list_only {
        let mut wrote_listing = false;
        if !events.is_empty() {
            emit_list_only(events, stdout)?;
            wrote_listing = true;
        }

        if stats {
            if wrote_listing {
                writeln!(stdout)?;
            }
            emit_stats(summary, stdout)?;
        } else if verbosity > 0 {
            if wrote_listing {
                writeln!(stdout)?;
            }
            emit_totals(summary, stdout)?;
        }

        return Ok(());
    }

    let progress_rendered = if progress_already_rendered {
        true
    } else if progress && !events.is_empty() {
        emit_progress(events, stdout)?
    } else {
        false
    };

    let emit_verbose_listing =
        verbosity > 0 && !events.is_empty() && (!progress_rendered || verbosity > 1);

    if progress_rendered && (emit_verbose_listing || stats || verbosity > 0) {
        writeln!(stdout)?;
    }

    if emit_verbose_listing {
        emit_verbose(events, verbosity, stdout)?;
        if stats {
            writeln!(stdout)?;
        }
    }

    if stats {
        emit_stats(summary, stdout)?;
    } else if verbosity > 0 {
        emit_totals(summary, stdout)?;
    }

    Ok(())
}

fn emit_list_only<W: Write>(events: &[ClientEvent], stdout: &mut W) -> io::Result<()> {
    for event in events {
        if !list_only_event(event.kind()) {
            continue;
        }

        if let Some(metadata) = event.metadata() {
            let permissions = format_list_permissions(metadata);
            let links = metadata.nlink().unwrap_or(1);
            let owner = format_numeric_identifier(metadata.uid());
            let group = format_numeric_identifier(metadata.gid());
            let size = metadata.length();
            let timestamp = format_list_timestamp(metadata.modified());
            let mut rendered = event.relative_path().to_string_lossy().into_owned();
            if metadata.kind().is_directory() && !rendered.ends_with('/') {
                rendered.push('/');
            }

            writeln!(
                stdout,
                "{permissions} {links:>3} {owner:>8} {group:>8} {size:>12} {timestamp} {rendered}",
            )?;
        } else {
            let mut rendered = event.relative_path().to_string_lossy().into_owned();
            if matches!(event.kind(), ClientEventKind::DirectoryCreated) && !rendered.ends_with('/')
            {
                rendered.push('/');
            }
            writeln!(stdout, "{rendered}")?;
        }
    }

    Ok(())
}

fn list_only_event(kind: &ClientEventKind) -> bool {
    matches!(
        kind,
        ClientEventKind::DataCopied
            | ClientEventKind::MetadataReused
            | ClientEventKind::HardLink
            | ClientEventKind::SymlinkCopied
            | ClientEventKind::FifoCopied
            | ClientEventKind::DeviceCopied
            | ClientEventKind::DirectoryCreated
    )
}

fn format_numeric_identifier(value: Option<u32>) -> String {
    value
        .map(|id| id.to_string())
        .unwrap_or_else(|| "-".to_string())
}

fn format_list_permissions(metadata: &ClientEntryMetadata) -> String {
    let type_char = match metadata.kind() {
        ClientEntryKind::File => '-',
        ClientEntryKind::Directory => 'd',
        ClientEntryKind::Symlink => 'l',
        ClientEntryKind::Fifo => 'p',
        ClientEntryKind::CharDevice => 'c',
        ClientEntryKind::BlockDevice => 'b',
        ClientEntryKind::Socket => 's',
        ClientEntryKind::Other => '?',
    };

    let mut output = String::with_capacity(10);
    output.push(type_char);

    if let Some(mode) = metadata.mode() {
        const MASKS: [u32; 9] = [
            0o400, 0o200, 0o100, 0o040, 0o020, 0o010, 0o004, 0o002, 0o001,
        ];
        const SYMBOLS: [char; 3] = ['r', 'w', 'x'];
        for (index, mask) in MASKS.iter().enumerate() {
            let bit = if mode & mask != 0 {
                SYMBOLS[index % 3]
            } else {
                '-'
            };
            output.push(bit);
        }
    } else {
        output.push_str("---------");
    }

    output
}

fn format_list_timestamp(modified: Option<SystemTime>) -> String {
    if let Some(time) = modified {
        if let Ok(datetime) = OffsetDateTime::from(time).format(LIST_TIMESTAMP_FORMAT) {
            return datetime;
        }
    }
    "1970/01/01 00:00:00".to_string()
}

/// Returns whether the provided event kind should be reflected in progress output.
fn is_progress_event(kind: &ClientEventKind) -> bool {
    kind.is_progress()
}

/// Formats a byte count using thousands separators, mirroring upstream rsync progress lines.
fn format_progress_bytes(bytes: u64) -> String {
    if bytes == 0 {
        return "0".to_string();
    }

    let mut value = bytes;
    let mut parts = Vec::new();
    while value > 0 {
        let chunk = value % 1_000;
        value /= 1_000;
        if value == 0 {
            parts.push(chunk.to_string());
        } else {
            parts.push(format!("{chunk:03}"));
        }
    }
    parts.reverse();
    parts.join(",")
}

/// Formats a progress percentage, producing the upstream `??%` placeholder when totals are
/// unavailable.
fn format_progress_percent(bytes: u64, total: Option<u64>) -> String {
    match total {
        Some(total_bytes) if total_bytes > 0 => {
            let capped = bytes.min(total_bytes);
            let percent = (capped.saturating_mul(100)) / total_bytes;
            format!("{percent}%")
        }
        Some(_) => "100%".to_string(),
        None => "??%".to_string(),
    }
}

/// Formats a transfer rate in the `kB/s`, `MB/s`, or `GB/s` ranges.
fn format_progress_rate(bytes: u64, elapsed: Duration) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = KIB * 1024.0;
    const GIB: f64 = MIB * 1024.0;

    if bytes == 0 || elapsed.is_zero() {
        return "0.00kB/s".to_string();
    }

    let seconds = elapsed.as_secs_f64();
    if seconds <= 0.0 {
        return "0.00kB/s".to_string();
    }

    let bytes_per_second = bytes as f64 / seconds;
    let (value, unit) = if bytes_per_second >= GIB {
        (bytes_per_second / GIB, "GB/s")
    } else if bytes_per_second >= MIB {
        (bytes_per_second / MIB, "MB/s")
    } else {
        (bytes_per_second / KIB, "kB/s")
    };

    format!("{value:.2}{unit}")
}

/// Formats an elapsed duration as `H:MM:SS`, matching rsync's progress output.
fn format_progress_elapsed(elapsed: Duration) -> String {
    let total_seconds = elapsed.as_secs();
    let hours = total_seconds / 3_600;
    let minutes = (total_seconds % 3_600) / 60;
    let seconds = total_seconds % 60;
    format!("{hours}:{minutes:02}:{seconds:02}")
}

/// Renders progress lines for the provided transfer events.
fn emit_progress<W: Write>(events: &[ClientEvent], stdout: &mut W) -> io::Result<bool> {
    let progress_events: Vec<_> = events
        .iter()
        .filter(|event| is_progress_event(event.kind()))
        .collect();

    if progress_events.is_empty() {
        return Ok(false);
    }

    let total = progress_events.len();

    for (index, event) in progress_events.into_iter().enumerate() {
        writeln!(stdout, "{}", event.relative_path().display())?;

        let bytes = event.bytes_transferred();
        let size_field = format!("{:>15}", format_progress_bytes(bytes));
        let percent_hint = matches!(event.kind(), ClientEventKind::DataCopied).then_some(bytes);
        let percent_field = format!("{:>4}", format_progress_percent(bytes, percent_hint));
        let rate_field = format!("{:>12}", format_progress_rate(bytes, event.elapsed()));
        let elapsed_field = format!("{:>11}", format_progress_elapsed(event.elapsed()));
        let remaining = total - index - 1;
        let xfr_index = index + 1;

        writeln!(
            stdout,
            "{size_field} {percent_field} {rate_field} {elapsed_field} (xfr#{xfr_index}, to-chk={remaining}/{total})"
        )?;
    }

    Ok(true)
}

/// Emits a statistics summary mirroring the subset of counters supported by the local engine.
fn emit_stats<W: Write>(summary: &ClientSummary, stdout: &mut W) -> io::Result<()> {
    let files = summary.files_copied();
    let files_total = summary.regular_files_total();
    let matched = summary.regular_files_matched();
    let directories = summary.directories_created();
    let directories_total = summary.directories_total();
    let symlinks = summary.symlinks_copied();
    let symlinks_total = summary.symlinks_total();
    let hard_links = summary.hard_links_created();
    let devices = summary.devices_created();
    let devices_total = summary.devices_total();
    let fifos = summary.fifos_created();
    let fifos_total = summary.fifos_total();
    let deleted = summary.items_deleted();
    let transferred = summary.bytes_copied();
    let compressed = summary.compressed_bytes().unwrap_or(transferred);
    let total_size = summary.total_source_bytes();
    let matched_bytes = total_size.saturating_sub(transferred);

    let total_entries = files_total
        .saturating_add(directories_total)
        .saturating_add(symlinks_total)
        .saturating_add(devices_total)
        .saturating_add(fifos_total);
    let created_total = files
        .saturating_add(directories)
        .saturating_add(symlinks)
        .saturating_add(devices)
        .saturating_add(fifos);

    writeln!(
        stdout,
        "Number of files: {total_entries} (reg: {files_total}, dir: {directories_total}, link: {symlinks_total}, dev: {devices_total}, fifo: {fifos_total})"
    )?;
    writeln!(
        stdout,
        "Number of created files: {created_total} (reg: {files}, dir: {directories}, link: {symlinks}, dev: {devices}, fifo: {fifos})"
    )?;
    writeln!(stdout, "Number of deleted files: {deleted}")?;
    writeln!(stdout, "Number of regular files transferred: {files}")?;
    writeln!(stdout, "Number of regular files matched: {matched}")?;
    writeln!(stdout, "Number of hard links created: {hard_links}")?;
    writeln!(stdout, "Total file size: {total_size} bytes")?;
    writeln!(stdout, "Total transferred file size: {transferred} bytes")?;
    writeln!(stdout, "Literal data: {transferred} bytes")?;
    writeln!(stdout, "Matched data: {matched_bytes} bytes")?;
    writeln!(stdout, "Total bytes sent: {compressed}")?;
    writeln!(stdout, "Total bytes received: 0")?;
    writeln!(stdout)?;

    emit_totals(summary, stdout)
}

/// Emits the summary lines reported by verbose transfers.
fn emit_totals<W: Write>(summary: &ClientSummary, stdout: &mut W) -> io::Result<()> {
    let sent = summary
        .compressed_bytes()
        .unwrap_or_else(|| summary.bytes_copied());
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
        match event.kind() {
            ClientEventKind::SkippedExisting => {
                writeln!(
                    stdout,
                    "skipping existing file \"{}\"",
                    event.relative_path().display()
                )?;
                continue;
            }
            ClientEventKind::SkippedNewerDestination => {
                writeln!(
                    stdout,
                    "skipping newer destination file \"{}\"",
                    event.relative_path().display()
                )?;
                continue;
            }
            ClientEventKind::SkippedNonRegular => {
                writeln!(
                    stdout,
                    "skipping non-regular file \"{}\"",
                    event.relative_path().display()
                )?;
                continue;
            }
            _ => {}
        }

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
        ClientEventKind::SkippedExisting => "skipped existing file",
        ClientEventKind::SkippedNonRegular => "skipped non-regular file",
        ClientEventKind::SkippedNewerDestination => "skipped newer destination file",
        ClientEventKind::EntryDeleted => "deleted",
        ClientEventKind::SourceRemoved => "source removed",
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

fn parse_protocol_version_arg(value: &OsStr) -> Result<ProtocolVersion, Message> {
    let text = value.to_string_lossy();
    let trimmed = text.trim_matches(|ch: char| ch.is_ascii_whitespace());
    let display = if trimmed.is_empty() {
        text.as_ref()
    } else {
        trimmed
    };

    match ProtocolVersion::from_str(text.as_ref()) {
        Ok(version) => Ok(version),
        Err(error) => {
            let supported = supported_protocols_list();
            let mut detail = match error.kind() {
                ParseProtocolVersionErrorKind::Empty => {
                    "protocol value must not be empty".to_string()
                }
                ParseProtocolVersionErrorKind::InvalidDigit => {
                    "protocol version must be an unsigned integer".to_string()
                }
                ParseProtocolVersionErrorKind::Negative => {
                    "protocol version cannot be negative".to_string()
                }
                ParseProtocolVersionErrorKind::Overflow => {
                    "protocol version value exceeds 255".to_string()
                }
                ParseProtocolVersionErrorKind::UnsupportedRange(value) => {
                    let (oldest, newest) = ProtocolVersion::supported_range_bounds();
                    format!(
                        "protocol version {} is outside the supported range {}-{}",
                        value, oldest, newest
                    )
                }
            };
            if !detail.is_empty() {
                detail.push_str("; ");
            }
            detail.push_str(&format!("supported protocols are {}", supported));

            Err(rsync_error!(
                1,
                format!("invalid protocol version '{}': {}", display, detail)
            )
            .with_role(Role::Client))
        }
    }
}

fn supported_protocols_list() -> String {
    let values: Vec<String> = ProtocolVersion::supported_protocol_numbers()
        .iter()
        .map(|value| value.to_string())
        .collect();
    values.join(", ")
}

fn parse_timeout_argument(value: &OsStr) -> Result<TransferTimeout, Message> {
    let text = value.to_string_lossy();
    let trimmed = text.trim_matches(|ch: char| ch.is_ascii_whitespace());
    let display = if trimmed.is_empty() {
        text.as_ref()
    } else {
        trimmed
    };

    if trimmed.is_empty() {
        return Err(rsync_error!(1, "timeout value must not be empty").with_role(Role::Client));
    }

    if trimmed.starts_with('-') {
        return Err(rsync_error!(
            1,
            format!(
                "invalid timeout '{}': timeout must be non-negative",
                display
            )
        )
        .with_role(Role::Client));
    }

    let normalized = trimmed.strip_prefix('+').unwrap_or(trimmed);

    match normalized.parse::<u64>() {
        Ok(0) => Ok(TransferTimeout::Disabled),
        Ok(value) => Ok(TransferTimeout::Seconds(
            NonZeroU64::new(value).expect("non-zero ensured"),
        )),
        Err(error) => {
            let detail = match error.kind() {
                IntErrorKind::InvalidDigit => "timeout must be an unsigned integer",
                IntErrorKind::PosOverflow | IntErrorKind::NegOverflow => {
                    "timeout value exceeds the supported range"
                }
                IntErrorKind::Empty => "timeout value must not be empty",
                _ => "timeout value is invalid",
            };
            Err(
                rsync_error!(1, format!("invalid timeout '{}': {}", display, detail))
                    .with_role(Role::Client),
            )
        }
    }
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
    Clear,
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
        let message = rsync_error!(
            1,
            "filter rule is empty: supply '+', '-', '!', or 'merge FILE'"
        )
        .with_role(Role::Client);
        return Err(message);
    }

    if trimmed == "!" {
        return Ok(FilterDirective::Clear);
    }

    const EXCLUDE_IF_PRESENT_PREFIX: &str = "exclude-if-present";

    if trimmed.len() >= EXCLUDE_IF_PRESENT_PREFIX.len()
        && trimmed[..EXCLUDE_IF_PRESENT_PREFIX.len()]
            .eq_ignore_ascii_case(EXCLUDE_IF_PRESENT_PREFIX)
    {
        let mut remainder = trimmed[EXCLUDE_IF_PRESENT_PREFIX.len()..].trim_start();
        if let Some(rest) = remainder.strip_prefix('=') {
            remainder = rest.trim_start();
        }

        let pattern_text = remainder.trim();
        if pattern_text.is_empty() {
            let message = rsync_error!(
                1,
                format!(
                    "filter rule '{trimmed}' is missing a marker file after 'exclude-if-present'"
                )
            )
            .with_role(Role::Client);
            return Err(message);
        }

        return Ok(FilterDirective::Rule(FilterRuleSpec::exclude_if_present(
            pattern_text.to_string(),
        )));
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

    const DIR_MERGE_PREFIX: &str = "dir-merge";

    if trimmed.len() >= DIR_MERGE_PREFIX.len()
        && trimmed[..DIR_MERGE_PREFIX.len()].eq_ignore_ascii_case(DIR_MERGE_PREFIX)
    {
        let mut remainder = trimmed[DIR_MERGE_PREFIX.len()..].trim_start();
        let mut modifiers = "";
        if let Some(rest) = remainder.strip_prefix(',') {
            let mut split = rest.splitn(2, char::is_whitespace);
            modifiers = split.next().unwrap_or("");
            remainder = split.next().unwrap_or("").trim_start();
        }

        let mut options = DirMergeOptions::default();
        let mut saw_plus = false;
        let mut saw_minus = false;
        let mut used_cvs_default = false;

        for modifier in modifiers.chars() {
            let lower = modifier.to_ascii_lowercase();
            match lower {
                '-' => {
                    if saw_plus {
                        let text =
                            format!("filter rule '{trimmed}' cannot combine '+' and '-' modifiers");
                        return Err(rsync_error!(1, text).with_role(Role::Client));
                    }
                    saw_minus = true;
                    options = options.with_enforced_kind(Some(DirMergeEnforcedKind::Exclude));
                }
                '+' => {
                    if saw_minus {
                        let text =
                            format!("filter rule '{trimmed}' cannot combine '+' and '-' modifiers");
                        return Err(rsync_error!(1, text).with_role(Role::Client));
                    }
                    saw_plus = true;
                    options = options.with_enforced_kind(Some(DirMergeEnforcedKind::Include));
                }
                'n' => {
                    options = options.inherit(false);
                }
                'e' => {
                    options = options.exclude_filter_file(true);
                }
                'w' => {
                    options = options.use_whitespace();
                    options = options.allow_comments(false);
                }
                's' => {
                    options = options.sender_modifier();
                }
                'r' => {
                    options = options.receiver_modifier();
                }
                '/' => {
                    options = options.anchor_root(true);
                }
                'c' => {
                    used_cvs_default = true;
                    options = options.with_enforced_kind(Some(DirMergeEnforcedKind::Exclude));
                    options = options.use_whitespace();
                    options = options.allow_comments(false);
                    options = options.inherit(false);
                    options = options.allow_list_clearing(true);
                }
                _ => {
                    let text = format!(
                        "filter rule '{trimmed}' uses unsupported dir-merge modifier '{modifier}'"
                    );
                    return Err(rsync_error!(1, text).with_role(Role::Client));
                }
            }
        }

        let mut path_text = remainder.trim();
        if path_text.is_empty() {
            if used_cvs_default {
                path_text = ".cvsignore";
            } else {
                let text =
                    format!("filter rule '{trimmed}' is missing a file name after 'dir-merge'");
                return Err(rsync_error!(1, text).with_role(Role::Client));
            }
        }

        return Ok(FilterDirective::Rule(FilterRuleSpec::dir_merge(
            path_text.to_string(),
            options,
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

    if keyword.eq_ignore_ascii_case("show") {
        return handle_keyword("show", FilterRuleSpec::show);
    }

    if keyword.eq_ignore_ascii_case("hide") {
        return handle_keyword("hide", FilterRuleSpec::hide);
    }

    if keyword.eq_ignore_ascii_case("protect") {
        return handle_keyword("protect", FilterRuleSpec::protect);
    }

    let message = rsync_error!(
        1,
        "unsupported filter rule '{trimmed}': this build currently supports only '+' (include), '-' (exclude), '!' (clear), 'include PATTERN', 'exclude PATTERN', 'show PATTERN', 'hide PATTERN', 'protect PATTERN', 'merge FILE', and 'dir-merge[,MODS] FILE' directives"
    )
    .with_role(Role::Client);
    Err(message)
}

fn append_filter_rules_from_files(
    destination: &mut Vec<FilterRuleSpec>,
    files: &[OsString],
    kind: FilterRuleKind,
) -> Result<(), Message> {
    if matches!(kind, FilterRuleKind::DirMerge) {
        let message = rsync_error!(
            1,
            "dir-merge directives cannot be loaded via --include-from/--exclude-from in this build"
        )
        .with_role(Role::Client);
        return Err(message);
    }

    for path in files {
        let patterns = load_filter_file_patterns(Path::new(path.as_os_str()))?;
        destination.extend(patterns.into_iter().map(|pattern| match kind {
            FilterRuleKind::Include => FilterRuleSpec::include(pattern),
            FilterRuleKind::Exclude => FilterRuleSpec::exclude(pattern),
            FilterRuleKind::ExcludeIfPresent => FilterRuleSpec::exclude_if_present(pattern),
            FilterRuleKind::Protect => FilterRuleSpec::protect(pattern),
            FilterRuleKind::DirMerge => unreachable!("dir-merge handled above"),
        }));
    }
    Ok(())
}

fn load_filter_file_patterns(path: &Path) -> Result<Vec<String>, Message> {
    if path == Path::new("-") {
        return read_filter_patterns_from_standard_input();
    }

    let path_display = path.display().to_string();
    let file = File::open(path).map_err(|error| {
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
    base_dir: &Path,
    destination: &mut Vec<FilterRuleSpec>,
    visited: &mut Vec<PathBuf>,
) -> Result<(), Message> {
    let is_stdin = source.as_os_str() == OsStr::new("-");
    let (resolved_path, display, canonical_path) = if is_stdin {
        (PathBuf::from("-"), String::from("-"), None)
    } else {
        let raw_path = PathBuf::from(&source);
        let resolved = if raw_path.is_absolute() {
            raw_path
        } else {
            base_dir.join(raw_path)
        };
        let display = resolved.display().to_string();
        let canonical = fs::canonicalize(&resolved).ok();
        (resolved, display, canonical)
    };

    let guard_key = if is_stdin {
        PathBuf::from("-")
    } else if let Some(canonical) = &canonical_path {
        canonical.clone()
    } else {
        resolved_path.clone()
    };

    if visited.contains(&guard_key) {
        let text = format!("recursive filter merge detected for '{display}'");
        return Err(rsync_error!(1, text).with_role(Role::Client));
    }

    visited.push(guard_key);
    let next_base_storage = if is_stdin {
        None
    } else {
        let resolved_for_base = canonical_path.as_ref().unwrap_or(&resolved_path);
        Some(
            resolved_for_base
                .parent()
                .map(|parent| parent.to_path_buf())
                .unwrap_or_else(|| base_dir.to_path_buf()),
        )
    };
    let next_base = next_base_storage.as_deref().unwrap_or(base_dir);

    let result = (|| -> Result<(), Message> {
        let entries = load_filter_file_patterns(&resolved_path)?;
        for entry in entries {
            match parse_filter_directive(OsStr::new(entry.as_str())) {
                Ok(FilterDirective::Rule(rule)) => destination.push(rule),
                Ok(FilterDirective::Merge(nested)) => {
                    apply_merge_directive(nested, next_base, destination, visited).map_err(
                        |error| {
                            let detail = error.to_string();
                            rsync_error!(
                                1,
                                format!("failed to process merge file '{display}': {detail}")
                            )
                            .with_role(Role::Client)
                        },
                    )?;
                }
                Ok(FilterDirective::Clear) => destination.clear(),
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
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with(';') {
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

#[cfg(test)]
thread_local! {
    static PASSWORD_STDIN_INPUT: std::cell::RefCell<Option<Vec<u8>>> = const {
        std::cell::RefCell::new(None)
    };
}

#[cfg(test)]
fn take_password_stdin_input() -> Option<Vec<u8>> {
    PASSWORD_STDIN_INPUT.with(|slot| slot.borrow_mut().take())
}

#[cfg(test)]
fn set_password_stdin_input(data: Vec<u8>) {
    PASSWORD_STDIN_INPUT.with(|slot| *slot.borrow_mut() = Some(data));
}

fn load_optional_password(path: Option<&Path>) -> Result<Option<Vec<u8>>, Message> {
    match path {
        Some(path) => load_password_file(path).map(Some),
        None => Ok(None),
    }
}

fn load_password_file(path: &Path) -> Result<Vec<u8>, Message> {
    if path == Path::new("-") {
        return read_password_from_stdin().map_err(|error| {
            rsync_error!(
                1,
                format!("failed to read password from standard input: {}", error)
            )
            .with_role(Role::Client)
        });
    }

    let display = path.display();
    let metadata = fs::metadata(path).map_err(|error| {
        rsync_error!(
            1,
            format!("failed to access password file '{}': {}", display, error)
        )
        .with_role(Role::Client)
    })?;

    if !metadata.is_file() {
        return Err(rsync_error!(
            1,
            format!("password file '{}' must be a regular file", display)
        )
        .with_role(Role::Client));
    }

    #[cfg(unix)]
    {
        let mode = metadata.permissions().mode();
        if mode & 0o077 != 0 {
            return Err(
                rsync_error!(
                    1,
                    format!(
                        "password file '{}' must not be accessible to group or others (expected permissions 0600)",
                        display
                    )
                )
                .with_role(Role::Client),
            );
        }
    }

    let mut bytes = fs::read(path).map_err(|error| {
        rsync_error!(
            1,
            format!("failed to read password file '{}': {}", display, error)
        )
        .with_role(Role::Client)
    })?;

    trim_trailing_newlines(&mut bytes);

    Ok(bytes)
}

/// Loads operands referenced by `--files-from` arguments.
///
/// When `zero_terminated` is `false`, the reader treats lines beginning with `#`
/// or `;` as comments, matching upstream rsync. Supplying `--from0` disables the
/// comment semantics so entries can legitimately start with those bytes.
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

        if buffer
            .first()
            .is_some_and(|byte| matches!(byte, b'#' | b';'))
        {
            continue;
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

fn read_password_from_stdin() -> io::Result<Vec<u8>> {
    #[cfg(test)]
    if let Some(bytes) = take_password_stdin_input() {
        let mut cursor = std::io::Cursor::new(bytes);
        return read_password_from_reader(&mut cursor);
    }

    let mut stdin = io::stdin().lock();
    read_password_from_reader(&mut stdin)
}

fn read_password_from_reader<R: Read>(reader: &mut R) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader.read_to_end(&mut bytes)?;
    trim_trailing_newlines(&mut bytes);
    Ok(bytes)
}

fn trim_trailing_newlines(bytes: &mut Vec<u8>) {
    while matches!(bytes.last(), Some(b'\n' | b'\r')) {
        bytes.pop();
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
    suppress_motd: bool,
) -> io::Result<()> {
    for warning in list.warnings() {
        writeln!(stderr, "@WARNING: {}", warning)?;
    }

    if !suppress_motd {
        for line in list.motd_lines() {
            writeln!(stdout, "{}", line)?;
        }
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
    use rsync_daemon as daemon_cli;
    use rsync_filters::{FilterRule as EngineFilterRule, FilterSet};
    use std::ffi::{OsStr, OsString};
    use std::io::{BufRead, BufReader, Seek, SeekFrom, Write};
    use std::net::{TcpListener, TcpStream};
    use std::path::Path;
    use std::thread;
    use std::time::Duration;

    #[cfg(unix)]
    use std::os::unix::ffi::OsStrExt;

    #[cfg(feature = "xattr")]
    use xattr;

    const LEGACY_DAEMON_GREETING: &str = "@RSYNCD: 32.0 sha512 sha256 sha1 md5 md4\n";

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
    fn daemon_flag_delegates_to_daemon_help() {
        let mut expected_stdout = Vec::new();
        let mut expected_stderr = Vec::new();
        let expected_code = daemon_cli::run(
            [OsStr::new("oc-rsyncd"), OsStr::new("--help")],
            &mut expected_stdout,
            &mut expected_stderr,
        );

        assert_eq!(expected_code, 0);
        assert!(expected_stderr.is_empty());

        let (code, stdout, stderr) = run_with_args([
            OsStr::new("oc-rsync"),
            OsStr::new("--daemon"),
            OsStr::new("--help"),
        ]);

        assert_eq!(code, expected_code);
        assert_eq!(stdout, expected_stdout);
        assert_eq!(stderr, expected_stderr);
    }

    #[test]
    fn daemon_flag_delegates_to_daemon_version() {
        let mut expected_stdout = Vec::new();
        let mut expected_stderr = Vec::new();
        let expected_code = daemon_cli::run(
            [OsStr::new("oc-rsyncd"), OsStr::new("--version")],
            &mut expected_stdout,
            &mut expected_stderr,
        );

        assert_eq!(expected_code, 0);
        assert!(expected_stderr.is_empty());

        let (code, stdout, stderr) = run_with_args([
            OsStr::new("oc-rsync"),
            OsStr::new("--daemon"),
            OsStr::new("--version"),
        ]);

        assert_eq!(code, expected_code);
        assert_eq!(stdout, expected_stdout);
        assert_eq!(stderr, expected_stderr);
    }

    #[test]
    fn daemon_mode_arguments_ignore_operands_after_double_dash() {
        let args = vec![
            OsString::from("oc-rsync"),
            OsString::from("--"),
            OsString::from("--daemon"),
            OsString::from("dest"),
        ];

        assert!(daemon_mode_arguments(&args).is_none());
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

    #[cfg(unix)]
    #[test]
    fn verbose_transfer_reports_skipped_specials() {
        use rustix::fs::{CWD, FileType, Mode, makedev, mknodat};
        use tempfile::tempdir;

        let tmp = tempdir().expect("tempdir");
        let source_fifo = tmp.path().join("skip.pipe");
        mknodat(
            CWD,
            &source_fifo,
            FileType::Fifo,
            Mode::from_bits_truncate(0o600),
            makedev(0, 0),
        )
        .expect("mkfifo");

        let destination = tmp.path().join("dest.pipe");
        let (code, stdout, stderr) = run_with_args([
            OsString::from("oc-rsync"),
            OsString::from("-v"),
            source_fifo.clone().into_os_string(),
            destination.clone().into_os_string(),
        ]);

        assert_eq!(code, 0);
        assert!(stderr.is_empty());
        assert!(std::fs::symlink_metadata(&destination).is_err());

        let rendered = String::from_utf8(stdout).expect("verbose output is UTF-8");
        assert!(rendered.contains("skipping non-regular file \"skip.pipe\""));
    }

    #[test]
    fn progress_transfer_renders_progress_lines() {
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
        assert!(rendered.contains("(xfr#1, to-chk=0/1)"));
        assert!(!rendered.contains("Total transferred"));
        assert_eq!(
            std::fs::read(destination).expect("read destination"),
            b"progress"
        );
    }

    #[test]
    fn progress_percent_placeholder_used_for_unknown_totals() {
        assert_eq!(format_progress_percent(42, None), "??%");
        assert_eq!(format_progress_percent(0, Some(0)), "100%");
        assert_eq!(format_progress_percent(50, Some(200)), "25%");
    }

    #[test]
    fn progress_reports_intermediate_updates() {
        use tempfile::tempdir;

        let tmp = tempdir().expect("tempdir");
        let source = tmp.path().join("large.bin");
        let destination = tmp.path().join("large.out");
        let payload = vec![0xA5u8; 256 * 1024];
        std::fs::write(&source, &payload).expect("write large source");

        let (code, stdout, stderr) = run_with_args([
            OsString::from("oc-rsync"),
            OsString::from("--progress"),
            source.clone().into_os_string(),
            destination.clone().into_os_string(),
        ]);

        assert_eq!(code, 0);
        assert!(stderr.is_empty());

        let rendered = String::from_utf8(stdout).expect("progress output is UTF-8");
        assert!(rendered.contains("large.bin"));
        assert!(rendered.contains("(xfr#1, to-chk=0/1)"));
        assert!(rendered.contains("\r"));
        assert!(rendered.contains(" 50%"));
        assert!(rendered.contains("100%"));
        assert_eq!(
            std::fs::read(destination).expect("read destination"),
            payload
        );
    }

    #[cfg(unix)]
    #[test]
    fn progress_reports_unknown_totals_with_placeholder() {
        use rustix::fs::{CWD, FileType, Mode, makedev, mknodat};
        use std::os::unix::fs::FileTypeExt;
        use tempfile::tempdir;

        let tmp = tempdir().expect("tempdir");
        let source = tmp.path().join("fifo.in");
        mknodat(
            CWD,
            &source,
            FileType::Fifo,
            Mode::from_bits_truncate(0o600),
            makedev(0, 0),
        )
        .expect("mkfifo");

        let destination = tmp.path().join("fifo.out");
        let (code, stdout, stderr) = run_with_args([
            OsString::from("oc-rsync"),
            OsString::from("--progress"),
            OsString::from("--specials"),
            source.clone().into_os_string(),
            destination.clone().into_os_string(),
        ]);

        assert_eq!(code, 0);
        assert!(stderr.is_empty());
        let rendered = String::from_utf8(stdout).expect("progress output is UTF-8");
        assert!(rendered.contains("fifo.in"));
        assert!(rendered.contains("??%"));
        assert!(rendered.contains("to-chk=0/1"));

        let metadata = std::fs::symlink_metadata(&destination).expect("stat destination");
        assert!(metadata.file_type().is_fifo());
    }

    #[test]
    fn progress_with_verbose_inserts_separator_before_totals() {
        use tempfile::tempdir;

        let tmp = tempdir().expect("tempdir");
        let source = tmp.path().join("progress.txt");
        let destination = tmp.path().join("progress.out");
        std::fs::write(&source, b"progress").expect("write source");

        let (code, stdout, stderr) = run_with_args([
            OsString::from("oc-rsync"),
            OsString::from("--progress"),
            OsString::from("-v"),
            source.clone().into_os_string(),
            destination.clone().into_os_string(),
        ]);

        assert_eq!(code, 0);
        assert!(stderr.is_empty());

        let rendered = String::from_utf8(stdout).expect("progress output is UTF-8");
        assert!(rendered.contains("(xfr#1, to-chk=0/1)"));
        assert!(rendered.contains("\n\nsent"));
        assert!(rendered.contains("sent"));
        assert!(rendered.contains("total size is"));
    }

    #[test]
    fn stats_transfer_renders_summary_block() {
        use tempfile::tempdir;

        let tmp = tempdir().expect("tempdir");
        let source = tmp.path().join("stats.txt");
        let destination = tmp.path().join("stats.out");
        let payload = b"statistics";
        std::fs::write(&source, payload).expect("write source");

        let (code, stdout, stderr) = run_with_args([
            OsString::from("oc-rsync"),
            OsString::from("--stats"),
            source.clone().into_os_string(),
            destination.clone().into_os_string(),
        ]);

        assert_eq!(code, 0);
        assert!(stderr.is_empty());

        let rendered = String::from_utf8(stdout).expect("stats output is UTF-8");
        let expected_size = payload.len();
        assert!(rendered.contains("Number of files: 1 (reg: 1, dir: 0, link: 0, dev: 0, fifo: 0)"));
        assert!(
            rendered
                .contains("Number of created files: 1 (reg: 1, dir: 0, link: 0, dev: 0, fifo: 0)")
        );
        assert!(rendered.contains("Number of regular files transferred: 1"));
        assert!(rendered.contains("Number of regular files matched: 0"));
        assert!(rendered.contains("Number of hard links created: 0"));
        assert!(rendered.contains(&format!("Total file size: {expected_size} bytes")));
        assert!(rendered.contains(&format!("Literal data: {expected_size} bytes")));
        assert!(rendered.contains("Matched data: 0 bytes"));
        assert!(rendered.contains("Total bytes received: 0"));
        assert!(rendered.contains("\n\nsent"));
        assert!(rendered.contains("total size is"));
        assert_eq!(
            std::fs::read(destination).expect("read destination"),
            payload
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
    fn transfer_request_with_remove_source_files_deletes_source() {
        use tempfile::tempdir;

        let tmp = tempdir().expect("tempdir");
        let source = tmp.path().join("source.txt");
        let destination = tmp.path().join("destination.txt");
        std::fs::write(&source, b"move me").expect("write source");

        let (code, stdout, stderr) = run_with_args([
            OsString::from("oc-rsync"),
            OsString::from("--remove-source-files"),
            source.clone().into_os_string(),
            destination.clone().into_os_string(),
        ]);

        assert_eq!(code, 0);
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());
        assert!(!source.exists(), "source should be removed");
        assert_eq!(
            std::fs::read(destination).expect("read destination"),
            b"move me"
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
    fn transfer_request_with_ignore_existing_leaves_destination_unchanged() {
        use tempfile::tempdir;

        let tmp = tempdir().expect("tempdir");
        let source = tmp.path().join("source.txt");
        let destination = tmp.path().join("destination.txt");
        std::fs::write(&source, b"updated").expect("write source");
        std::fs::write(&destination, b"original").expect("write destination");

        let (code, stdout, stderr) = run_with_args([
            OsString::from("oc-rsync"),
            OsString::from("--ignore-existing"),
            source.clone().into_os_string(),
            destination.clone().into_os_string(),
        ]);

        assert_eq!(code, 0);
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());
        assert_eq!(
            std::fs::read(destination).expect("read destination"),
            b"original"
        );
    }

    #[test]
    fn transfer_request_with_relative_preserves_parent_directories() {
        use tempfile::tempdir;

        let tmp = tempdir().expect("tempdir");
        let source_root = tmp.path().join("src");
        let destination_root = tmp.path().join("dest");
        std::fs::create_dir_all(source_root.join("foo/bar")).expect("create source tree");
        std::fs::create_dir_all(&destination_root).expect("create destination");
        let source_file = source_root.join("foo").join("bar").join("relative.txt");
        std::fs::write(&source_file, b"relative").expect("write source");

        let operand = source_root
            .join(".")
            .join("foo")
            .join("bar")
            .join("relative.txt");

        let (code, stdout, stderr) = run_with_args([
            OsString::from("oc-rsync"),
            OsString::from("--relative"),
            operand.into_os_string(),
            destination_root.clone().into_os_string(),
        ]);

        assert_eq!(code, 0);
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());
        let copied = destination_root
            .join("foo")
            .join("bar")
            .join("relative.txt");
        assert_eq!(
            std::fs::read(copied).expect("read copied file"),
            b"relative"
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
    fn transfer_request_with_files_from_skips_comment_lines() {
        use tempfile::tempdir;

        let tmp = tempdir().expect("tempdir");
        let source_a = tmp.path().join("comment-a.txt");
        let source_b = tmp.path().join("comment-b.txt");
        std::fs::write(&source_a, b"comment-a").expect("write source a");
        std::fs::write(&source_b, b"comment-b").expect("write source b");

        let list_path = tmp.path().join("files-from.list");
        let contents = format!(
            "# leading comment\n; alt comment\n{}\n{}\n",
            source_a.display(),
            source_b.display()
        );
        std::fs::write(&list_path, contents).expect("write list");

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
        assert_eq!(std::fs::read(&copied_a).expect("read a"), b"comment-a");
        assert_eq!(std::fs::read(&copied_b).expect("read b"), b"comment-b");
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
    fn transfer_request_with_from0_preserves_comment_prefix_entries() {
        use tempfile::tempdir;

        let tmp = tempdir().expect("tempdir");
        let comment_named = tmp.path().join("#commented.txt");
        std::fs::write(&comment_named, b"from0-comment").expect("write comment source");

        let mut bytes = Vec::new();
        bytes.extend_from_slice(comment_named.display().to_string().as_bytes());
        bytes.push(0);
        let list_path = tmp.path().join("files-from0-comments.list");
        std::fs::write(&list_path, bytes).expect("write list");

        let dest_dir = tmp.path().join("files-from0-comments-dest");
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

        let copied = dest_dir.join(comment_named.file_name().expect("file name"));
        assert_eq!(
            std::fs::read(&copied).expect("read copied"),
            b"from0-comment"
        );
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
    fn parse_args_recognises_update_flag() {
        let parsed = parse_args([
            OsString::from("oc-rsync"),
            OsString::from("--update"),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .expect("parse");

        assert!(parsed.update);
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
    fn parse_args_recognises_devices_flags() {
        let parsed = parse_args([
            OsString::from("oc-rsync"),
            OsString::from("--devices"),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .expect("parse");

        assert_eq!(parsed.devices, Some(true));

        let parsed = parse_args([
            OsString::from("oc-rsync"),
            OsString::from("--no-devices"),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .expect("parse");

        assert_eq!(parsed.devices, Some(false));
    }

    #[test]
    fn parse_args_recognises_specials_flags() {
        let parsed = parse_args([
            OsString::from("oc-rsync"),
            OsString::from("--specials"),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .expect("parse");

        assert_eq!(parsed.specials, Some(true));

        let parsed = parse_args([
            OsString::from("oc-rsync"),
            OsString::from("--no-specials"),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .expect("parse");

        assert_eq!(parsed.specials, Some(false));
    }

    #[test]
    fn parse_args_recognises_archive_devices_combo() {
        let parsed = parse_args([
            OsString::from("oc-rsync"),
            OsString::from("-D"),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .expect("parse");

        assert_eq!(parsed.devices, Some(true));
        assert_eq!(parsed.specials, Some(true));
    }

    #[test]
    fn parse_args_recognises_relative_flags() {
        let parsed = parse_args([
            OsString::from("oc-rsync"),
            OsString::from("--relative"),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .expect("parse");

        assert_eq!(parsed.relative, Some(true));

        let parsed = parse_args([
            OsString::from("oc-rsync"),
            OsString::from("--no-relative"),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .expect("parse");

        assert_eq!(parsed.relative, Some(false));
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
    fn parse_args_recognises_stats_flag() {
        let parsed = parse_args([
            OsString::from("oc-rsync"),
            OsString::from("--stats"),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .expect("parse");

        assert!(parsed.stats);
    }

    #[test]
    fn parse_args_recognises_list_only_flag() {
        let parsed = parse_args([
            OsString::from("oc-rsync"),
            OsString::from("--list-only"),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .expect("parse");

        assert!(parsed.list_only);
        assert!(parsed.dry_run);
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
    fn parse_args_recognises_password_file() {
        let parsed = parse_args([
            OsString::from("oc-rsync"),
            OsString::from("--password-file"),
            OsString::from("secret.txt"),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .expect("parse");

        assert_eq!(parsed.password_file, Some(OsString::from("secret.txt")));

        let parsed = parse_args([
            OsString::from("oc-rsync"),
            OsString::from("--password-file=secrets.d"),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .expect("parse");

        assert_eq!(parsed.password_file, Some(OsString::from("secrets.d")));
    }

    #[test]
    fn parse_args_recognises_no_motd_flag() {
        let parsed = parse_args([
            OsString::from("oc-rsync"),
            OsString::from("--no-motd"),
            OsString::from("rsync://example/"),
        ])
        .expect("parse");

        assert!(parsed.no_motd);
    }

    #[test]
    fn parse_args_collects_protocol_value() {
        let parsed = parse_args([
            OsString::from("oc-rsync"),
            OsString::from("--protocol=30"),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .expect("parse");

        assert_eq!(parsed.protocol, Some(OsString::from("30")));
    }

    #[test]
    fn parse_args_collects_timeout_value() {
        let parsed = parse_args([
            OsString::from("oc-rsync"),
            OsString::from("--timeout=90"),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .expect("parse");

        assert_eq!(parsed.timeout, Some(OsString::from("90")));
    }

    #[test]
    fn timeout_argument_zero_disables_timeout() {
        let timeout = parse_timeout_argument(OsStr::new("0")).expect("parse timeout");
        assert_eq!(timeout, TransferTimeout::Disabled);
    }

    #[test]
    fn timeout_argument_positive_sets_seconds() {
        let timeout = parse_timeout_argument(OsStr::new("15")).expect("parse timeout");
        assert_eq!(timeout.as_seconds(), NonZeroU64::new(15));
    }

    #[test]
    fn timeout_argument_negative_reports_error() {
        let error = parse_timeout_argument(OsStr::new("-1")).unwrap_err();
        assert!(error.to_string().contains("timeout must be non-negative"));
    }

    #[test]
    fn parse_args_sets_compress_flag() {
        let parsed = parse_args([
            OsString::from("oc-rsync"),
            OsString::from("-z"),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .expect("parse");

        assert!(parsed.compress);
    }

    #[test]
    fn parse_args_no_compress_overrides_compress_flag() {
        let parsed = parse_args([
            OsString::from("oc-rsync"),
            OsString::from("-z"),
            OsString::from("--no-compress"),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .expect("parse");

        assert!(!parsed.compress);
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
    fn parse_filter_directive_accepts_hide_and_show_keywords() {
        let show_keyword =
            parse_filter_directive(OsStr::new("show images/**")).expect("keyword show parses");
        assert_eq!(
            show_keyword,
            FilterDirective::Rule(FilterRuleSpec::show("images/**".to_string()))
        );

        let hide_keyword =
            parse_filter_directive(OsStr::new("hide *.swp")).expect("keyword hide parses");
        assert_eq!(
            hide_keyword,
            FilterDirective::Rule(FilterRuleSpec::hide("*.swp".to_string()))
        );
    }

    #[test]
    fn parse_filter_directive_accepts_exclude_if_present() {
        let directive = parse_filter_directive(OsStr::new("exclude-if-present marker"))
            .expect("exclude-if-present with whitespace parses");
        assert_eq!(
            directive,
            FilterDirective::Rule(FilterRuleSpec::exclude_if_present("marker".to_string()))
        );

        let equals_variant = parse_filter_directive(OsStr::new("exclude-if-present=.skip"))
            .expect("exclude-if-present with equals parses");
        assert_eq!(
            equals_variant,
            FilterDirective::Rule(FilterRuleSpec::exclude_if_present(".skip".to_string()))
        );
    }

    #[test]
    fn parse_filter_directive_rejects_exclude_if_present_without_marker() {
        let error = parse_filter_directive(OsStr::new("exclude-if-present   "))
            .expect_err("missing marker should error");
        let rendered = error.to_string();
        assert!(rendered.contains("missing a marker file"));
    }

    #[test]
    fn parse_filter_directive_accepts_clear_directive() {
        let clear = parse_filter_directive(OsStr::new("!")).expect("clear directive parses");
        assert_eq!(clear, FilterDirective::Clear);

        let clear_with_whitespace =
            parse_filter_directive(OsStr::new("  !   ")).expect("clear with whitespace parses");
        assert_eq!(clear_with_whitespace, FilterDirective::Clear);
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
    fn parse_filter_directive_accepts_dir_merge_without_modifiers() {
        let directive = parse_filter_directive(OsStr::new("dir-merge .rsync-filter"))
            .expect("dir-merge without modifiers parses");
        assert_eq!(
            directive,
            FilterDirective::Rule(FilterRuleSpec::dir_merge(
                ".rsync-filter".to_string(),
                DirMergeOptions::default(),
            )),
        );
    }

    #[test]
    fn parse_filter_directive_accepts_dir_merge_with_remove_modifier() {
        let directive = parse_filter_directive(OsStr::new("dir-merge,- .rsync-filter"))
            .expect("dir-merge with '-' modifier parses");
        assert_eq!(
            directive,
            FilterDirective::Rule(FilterRuleSpec::dir_merge(
                ".rsync-filter".to_string(),
                DirMergeOptions::default().with_enforced_kind(Some(DirMergeEnforcedKind::Exclude)),
            ))
        );
    }

    #[test]
    fn parse_filter_directive_accepts_dir_merge_with_include_modifier() {
        let directive = parse_filter_directive(OsStr::new("dir-merge,+ .rsync-filter"))
            .expect("dir-merge with '+' modifier parses");

        let FilterDirective::Rule(rule) = directive else {
            panic!("expected dir-merge rule");
        };

        assert_eq!(rule.pattern(), ".rsync-filter");
        let options = rule
            .dir_merge_options()
            .expect("dir-merge rule returns options");
        assert_eq!(options.enforced_kind(), Some(DirMergeEnforcedKind::Include));
        assert!(options.inherit_rules());
        assert!(!options.excludes_self());
    }

    #[test]
    fn parse_filter_directive_accepts_dir_merge_with_no_inherit_modifier() {
        let directive = parse_filter_directive(OsStr::new("dir-merge,n per-dir"))
            .expect("dir-merge with 'n' modifier parses");

        let FilterDirective::Rule(rule) = directive else {
            panic!("expected dir-merge rule");
        };

        assert_eq!(rule.pattern(), "per-dir");
        let options = rule
            .dir_merge_options()
            .expect("dir-merge rule returns options");
        assert!(!options.inherit_rules());
        assert!(options.allows_comments());
        assert!(!options.uses_whitespace());
    }

    #[test]
    fn parse_filter_directive_accepts_dir_merge_with_exclude_self_modifier() {
        let directive = parse_filter_directive(OsStr::new("dir-merge,e per-dir"))
            .expect("dir-merge with 'e' modifier parses");

        let FilterDirective::Rule(rule) = directive else {
            panic!("expected dir-merge rule");
        };

        let options = rule
            .dir_merge_options()
            .expect("dir-merge rule returns options");
        assert!(options.excludes_self());
        assert!(options.inherit_rules());
        assert!(!options.uses_whitespace());
    }

    #[test]
    fn parse_filter_directive_accepts_dir_merge_with_whitespace_modifier() {
        let directive = parse_filter_directive(OsStr::new("dir-merge,w per-dir"))
            .expect("dir-merge with 'w' modifier parses");

        let FilterDirective::Rule(rule) = directive else {
            panic!("expected dir-merge rule");
        };

        let options = rule
            .dir_merge_options()
            .expect("dir-merge rule returns options");
        assert!(options.uses_whitespace());
        assert!(!options.allows_comments());
    }

    #[test]
    fn parse_filter_directive_accepts_dir_merge_with_cvs_modifier() {
        let directive = parse_filter_directive(OsStr::new("dir-merge,C"))
            .expect("dir-merge with 'C' modifier parses");

        let FilterDirective::Rule(rule) = directive else {
            panic!("expected dir-merge rule");
        };

        assert_eq!(rule.pattern(), ".cvsignore");
        let options = rule
            .dir_merge_options()
            .expect("dir-merge rule returns options");
        assert_eq!(options.enforced_kind(), Some(DirMergeEnforcedKind::Exclude));
        assert!(options.uses_whitespace());
        assert!(!options.allows_comments());
        assert!(!options.inherit_rules());
        assert!(options.list_clear_allowed());
    }

    #[test]
    fn parse_filter_directive_rejects_dir_merge_with_conflicting_modifiers() {
        let error = parse_filter_directive(OsStr::new("dir-merge,+- per-dir"))
            .expect_err("conflicting modifiers should error");
        let rendered = error.to_string();
        assert!(rendered.contains("cannot combine '+' and '-'"));
    }

    #[test]
    fn parse_filter_directive_accepts_dir_merge_with_sender_modifier() {
        let directive = parse_filter_directive(OsStr::new("dir-merge,s per-dir"))
            .expect("dir-merge with 's' modifier parses");
        let FilterDirective::Rule(rule) = directive else {
            panic!("expected dir-merge rule");
        };
        let options = rule
            .dir_merge_options()
            .expect("dir-merge rule returns options");
        assert!(options.applies_to_sender());
        assert!(!options.applies_to_receiver());
    }

    #[test]
    fn parse_filter_directive_accepts_dir_merge_with_receiver_modifier() {
        let directive = parse_filter_directive(OsStr::new("dir-merge,r per-dir"))
            .expect("dir-merge with 'r' modifier parses");
        let FilterDirective::Rule(rule) = directive else {
            panic!("expected dir-merge rule");
        };
        let options = rule
            .dir_merge_options()
            .expect("dir-merge rule returns options");
        assert!(!options.applies_to_sender());
        assert!(options.applies_to_receiver());
    }

    #[test]
    fn parse_filter_directive_accepts_dir_merge_with_anchor_modifier() {
        let directive = parse_filter_directive(OsStr::new("dir-merge,/ .rules"))
            .expect("dir-merge with '/' modifier parses");
        let FilterDirective::Rule(rule) = directive else {
            panic!("expected dir-merge rule");
        };
        let options = rule
            .dir_merge_options()
            .expect("dir-merge rule returns options");
        assert!(options.anchor_root_enabled());
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
    fn transfer_request_with_filter_clear_resets_rules() {
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
            OsString::from("--filter"),
            OsString::from("!"),
            source_root.clone().into_os_string(),
            dest_root.clone().into_os_string(),
        ]);

        assert_eq!(code, 0);
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());

        let copied_root = dest_root.join("source");
        assert!(copied_root.join("keep.txt").exists());
        assert!(copied_root.join("skip.tmp").exists());
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
    fn transfer_request_with_filter_merge_clear_resets_rules() {
        use tempfile::tempdir;

        let tmp = tempdir().expect("tempdir");
        let source_root = tmp.path().join("source");
        let dest_root = tmp.path().join("dest");
        std::fs::create_dir_all(&source_root).expect("create source root");
        std::fs::create_dir_all(&dest_root).expect("create dest root");
        std::fs::write(source_root.join("keep.txt"), b"keep").expect("write keep");
        std::fs::write(source_root.join("skip.tmp"), b"skip").expect("write tmp");
        std::fs::write(source_root.join("skip.log"), b"log").expect("write log");

        let filter_file = tmp.path().join("filters.txt");
        std::fs::write(&filter_file, "- *.tmp\n!\n- *.log\n").expect("write filter file");

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
        assert!(copied_root.join("skip.tmp").exists());
        assert!(!copied_root.join("skip.log").exists());
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
    fn transfer_request_with_delete_excluded_prunes_filtered_entries() {
        use tempfile::tempdir;

        let tmp = tempdir().expect("tempdir");
        let source_root = tmp.path().join("source");
        let dest_root = tmp.path().join("dest");
        std::fs::create_dir_all(&source_root).expect("create source root");
        std::fs::create_dir_all(&dest_root).expect("create dest root");
        std::fs::write(source_root.join("keep.txt"), b"keep").expect("write keep");

        let dest_subdir = dest_root.join("source");
        std::fs::create_dir_all(&dest_subdir).expect("create destination contents");
        std::fs::write(dest_subdir.join("skip.log"), b"skip").expect("write excluded file");

        let (code, stdout, stderr) = run_with_args([
            OsString::from("oc-rsync"),
            OsString::from("--delete-excluded"),
            OsString::from("--exclude=*.log"),
            source_root.clone().into_os_string(),
            dest_root.clone().into_os_string(),
        ]);

        assert_eq!(code, 0);
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());

        let copied_root = dest_root.join("source");
        assert!(copied_root.join("keep.txt").exists());
        assert!(!copied_root.join("skip.log").exists());
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

        let engine_rules = expected_rules.iter().filter_map(|rule| match rule.kind() {
            FilterRuleKind::Include => Some(EngineFilterRule::include(rule.pattern())),
            FilterRuleKind::Exclude => Some(EngineFilterRule::exclude(rule.pattern())),
            FilterRuleKind::Protect => Some(EngineFilterRule::protect(rule.pattern())),
            FilterRuleKind::ExcludeIfPresent => None,
            FilterRuleKind::DirMerge => None,
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
            load_filter_file_patterns(path.as_path()).expect("load filter patterns succeeds");

        assert_eq!(
            patterns,
            vec![" include ".to_string(), "pattern".to_string()]
        );
    }

    #[test]
    fn load_filter_file_patterns_skip_semicolon_comments() {
        use tempfile::tempdir;

        let tmp = tempdir().expect("tempdir");
        let path = tmp.path().join("filters-semicolon.txt");
        std::fs::write(&path, b"; leading comment\n  ; spaced comment\nkeep\n")
            .expect("write filters");

        let patterns =
            load_filter_file_patterns(path.as_path()).expect("load filter patterns succeeds");

        assert_eq!(patterns, vec!["keep".to_string()]);
    }

    #[test]
    fn load_filter_file_patterns_handles_invalid_utf8() {
        use tempfile::tempdir;

        let tmp = tempdir().expect("tempdir");
        let path = tmp.path().join("filters.bin");
        std::fs::write(&path, [0xFFu8, b'\n']).expect("write invalid bytes");

        let patterns =
            load_filter_file_patterns(path.as_path()).expect("load filter patterns succeeds");

        assert_eq!(patterns, vec!["\u{fffd}".to_string()]);
    }

    #[test]
    fn load_filter_file_patterns_reads_from_stdin() {
        super::set_filter_stdin_input(b"keep\n# comment\n\ninclude\n".to_vec());
        let patterns =
            super::load_filter_file_patterns(Path::new("-")).expect("load stdin patterns");

        assert_eq!(patterns, vec!["keep".to_string(), "include".to_string()]);
    }

    #[test]
    fn apply_merge_directive_resolves_relative_paths() {
        use tempfile::tempdir;

        let temp = tempdir().expect("tempdir");
        let outer = temp.path().join("outer.rules");
        let subdir = temp.path().join("nested");
        std::fs::create_dir(&subdir).expect("create nested dir");
        let child = subdir.join("child.rules");
        let grand = subdir.join("grand.rules");

        std::fs::write(&outer, b"+ outer\nmerge nested/child.rules\n").expect("write outer");
        std::fs::write(&child, b"+ child\nmerge grand.rules\n").expect("write child");
        std::fs::write(&grand, b"+ grand\n").expect("write grand");

        let mut rules = Vec::new();
        let mut visited = Vec::new();
        super::apply_merge_directive(
            OsString::from("outer.rules"),
            temp.path(),
            &mut rules,
            &mut visited,
        )
        .expect("merge succeeds");

        assert!(visited.is_empty());
        let patterns: Vec<_> = rules
            .iter()
            .map(|rule| rule.pattern().to_string())
            .collect();
        assert_eq!(patterns, vec!["outer", "child", "grand"]);
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
    fn remote_daemon_listing_suppresses_motd_with_flag() {
        let (addr, handle) = spawn_stub_daemon(vec![
            "@RSYNCD: MOTD Welcome to the test daemon\n",
            "@RSYNCD: OK\n",
            "module\n",
            "@RSYNCD: EXIT\n",
        ]);

        let url = format!("rsync://{}:{}/", addr.ip(), addr.port());
        let (code, stdout, stderr) = run_with_args([
            OsString::from("oc-rsync"),
            OsString::from("--no-motd"),
            OsString::from(url),
        ]);

        assert_eq!(code, 0);
        assert!(stderr.is_empty());

        let rendered = String::from_utf8(stdout).expect("output is UTF-8");
        assert!(!rendered.contains("Welcome to the test daemon"));
        assert!(rendered.contains("module"));

        handle.join().expect("server thread");
    }

    #[test]
    fn remote_daemon_listing_respects_protocol_cap() {
        let (addr, handle) = spawn_stub_daemon_with_protocol(
            vec!["@RSYNCD: OK\n", "module\n", "@RSYNCD: EXIT\n"],
            "29.0",
        );

        let url = format!("rsync://{}:{}/", addr.ip(), addr.port());
        let (code, stdout, stderr) = run_with_args([
            OsString::from("oc-rsync"),
            OsString::from("--protocol=29"),
            OsString::from(url),
        ]);

        assert_eq!(code, 0);
        assert!(stderr.is_empty());

        let rendered = String::from_utf8(stdout).expect("output is UTF-8");
        assert!(rendered.contains("module"));

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
    fn module_list_username_prefix_is_accepted() {
        let (addr, handle) = spawn_stub_daemon(vec![
            "@RSYNCD: OK\n",
            "module\tWith comment\n",
            "@RSYNCD: EXIT\n",
        ]);

        let url = format!("rsync://user@{}:{}/", addr.ip(), addr.port());
        let (code, stdout, stderr) =
            run_with_args([OsString::from("oc-rsync"), OsString::from(url)]);

        assert_eq!(code, 0);
        assert!(stderr.is_empty());

        let rendered = String::from_utf8(stdout).expect("output is UTF-8");
        assert!(rendered.contains("module\tWith comment"));

        handle.join().expect("server thread");
    }

    #[test]
    fn module_list_username_prefix_legacy_syntax_is_accepted() {
        let (addr, handle) =
            spawn_stub_daemon(vec!["@RSYNCD: OK\n", "module\n", "@RSYNCD: EXIT\n"]);

        let url = format!("user@[{}]:{}::", addr.ip(), addr.port());
        let (code, stdout, stderr) =
            run_with_args([OsString::from("oc-rsync"), OsString::from(url)]);

        assert_eq!(code, 0);
        assert!(stderr.is_empty());

        let rendered = String::from_utf8(stdout).expect("output is UTF-8");
        assert!(rendered.contains("module"));

        handle.join().expect("server thread");
    }

    #[test]
    fn module_list_uses_password_file_for_authentication() {
        use base64::Engine as _;
        use base64::engine::general_purpose::STANDARD_NO_PAD;
        use rsync_checksums::strong::Md5;
        use tempfile::tempdir;

        let challenge = "pw-test";
        let secret = b"cli-secret";
        let expected_digest = {
            let mut hasher = Md5::new();
            hasher.update(secret);
            hasher.update(challenge.as_bytes());
            let digest = hasher.finalize();
            STANDARD_NO_PAD.encode(digest)
        };

        let expected_credentials = format!("user {expected_digest}");
        let (addr, handle) = spawn_auth_stub_daemon(
            challenge,
            expected_credentials,
            vec!["@RSYNCD: OK\n", "secure\n", "@RSYNCD: EXIT\n"],
        );

        let temp = tempdir().expect("tempdir");
        let password_path = temp.path().join("daemon.pw");
        std::fs::write(&password_path, b"cli-secret\n").expect("write password");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            let mut permissions = std::fs::metadata(&password_path)
                .expect("metadata")
                .permissions();
            permissions.set_mode(0o600);
            std::fs::set_permissions(&password_path, permissions).expect("set permissions");
        }

        let url = format!("rsync://user@{}:{}/", addr.ip(), addr.port());
        let (code, stdout, stderr) = run_with_args([
            OsString::from("oc-rsync"),
            OsString::from(format!("--password-file={}", password_path.display())),
            OsString::from(url),
        ]);

        assert_eq!(code, 0);
        assert!(stderr.is_empty());
        let rendered = String::from_utf8(stdout).expect("module listing is UTF-8");
        assert!(rendered.contains("secure"));

        handle.join().expect("server thread");
    }

    #[test]
    fn module_list_reads_password_from_stdin() {
        use base64::Engine as _;
        use base64::engine::general_purpose::STANDARD_NO_PAD;
        use rsync_checksums::strong::Md5;

        let challenge = "stdin-test";
        let secret = b"stdin-secret";
        let expected_digest = {
            let mut hasher = Md5::new();
            hasher.update(secret);
            hasher.update(challenge.as_bytes());
            let digest = hasher.finalize();
            STANDARD_NO_PAD.encode(digest)
        };

        let expected_credentials = format!("user {expected_digest}");
        let (addr, handle) = spawn_auth_stub_daemon(
            challenge,
            expected_credentials,
            vec!["@RSYNCD: OK\n", "secure\n", "@RSYNCD: EXIT\n"],
        );

        set_password_stdin_input(b"stdin-secret\n".to_vec());

        let url = format!("rsync://user@{}:{}/", addr.ip(), addr.port());
        let (code, stdout, stderr) = run_with_args([
            OsString::from("oc-rsync"),
            OsString::from("--password-file=-"),
            OsString::from(url),
        ]);

        assert_eq!(code, 0);
        assert!(stderr.is_empty());
        let rendered = String::from_utf8(stdout).expect("module listing is UTF-8");
        assert!(rendered.contains("secure"));

        handle.join().expect("server thread");
    }

    #[cfg(unix)]
    #[test]
    fn module_list_rejects_world_readable_password_file() {
        use tempfile::tempdir;

        let temp = tempdir().expect("tempdir");
        let password_path = temp.path().join("insecure.pw");
        std::fs::write(&password_path, b"secret\n").expect("write password");
        {
            use std::os::unix::fs::PermissionsExt;

            let mut permissions = std::fs::metadata(&password_path)
                .expect("metadata")
                .permissions();
            permissions.set_mode(0o644);
            std::fs::set_permissions(&password_path, permissions).expect("set perms");
        }

        let url = String::from("rsync://user@127.0.0.1:873/");
        let (code, stdout, stderr) = run_with_args([
            OsString::from("oc-rsync"),
            OsString::from(format!("--password-file={}", password_path.display())),
            OsString::from(url),
        ]);

        assert_eq!(code, 1);
        assert!(stdout.is_empty());
        let rendered = String::from_utf8(stderr).expect("diagnostic is UTF-8");
        assert!(rendered.contains("password file"));
        assert!(rendered.contains("0600"));

        // No server was contacted: the permission error occurs before negotiation.
    }

    #[test]
    fn password_file_requires_daemon_operands() {
        use tempfile::tempdir;

        let temp = tempdir().expect("tempdir");
        let password_path = temp.path().join("local.pw");
        std::fs::write(&password_path, b"secret\n").expect("write password");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            let mut permissions = std::fs::metadata(&password_path)
                .expect("metadata")
                .permissions();
            permissions.set_mode(0o600);
            std::fs::set_permissions(&password_path, permissions).expect("set permissions");
        }

        let source = temp.path().join("source.txt");
        let destination = temp.path().join("dest.txt");
        std::fs::write(&source, b"data").expect("write source");

        let (code, stdout, stderr) = run_with_args([
            OsString::from("oc-rsync"),
            OsString::from(format!("--password-file={}", password_path.display())),
            source.clone().into_os_string(),
            destination.clone().into_os_string(),
        ]);

        assert_eq!(code, 1);
        assert!(stdout.is_empty());
        let rendered = String::from_utf8(stderr).expect("diagnostic is UTF-8");
        assert!(rendered.contains("--password-file"));
        assert!(rendered.contains("rsync daemon"));
        assert!(!destination.exists());
    }

    #[test]
    fn protocol_option_requires_daemon_operands() {
        use tempfile::tempdir;

        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source.txt");
        let destination = temp.path().join("dest.txt");
        std::fs::write(&source, b"data").expect("write source");

        let (code, stdout, stderr) = run_with_args([
            OsString::from("oc-rsync"),
            OsString::from("--protocol=30"),
            source.clone().into_os_string(),
            destination.clone().into_os_string(),
        ]);

        assert_eq!(code, 1);
        assert!(stdout.is_empty());
        let rendered = String::from_utf8(stderr).expect("diagnostic is UTF-8");
        assert!(rendered.contains("--protocol"));
        assert!(rendered.contains("rsync daemon"));
        assert!(!destination.exists());
    }

    #[test]
    fn invalid_protocol_value_reports_error() {
        let (code, stdout, stderr) = run_with_args([
            OsString::from("oc-rsync"),
            OsString::from("--protocol=27"),
            OsString::from("source"),
            OsString::from("dest"),
        ]);

        assert_eq!(code, 1);
        assert!(stdout.is_empty());
        let rendered = String::from_utf8(stderr).expect("diagnostic is UTF-8");
        assert!(rendered.contains("invalid protocol version '27'"));
        assert!(rendered.contains("outside the supported range"));
    }

    #[test]
    fn non_numeric_protocol_value_reports_error() {
        let (code, stdout, stderr) = run_with_args([
            OsString::from("oc-rsync"),
            OsString::from("--protocol=abc"),
            OsString::from("source"),
            OsString::from("dest"),
        ]);

        assert_eq!(code, 1);
        assert!(stdout.is_empty());
        let rendered = String::from_utf8(stderr).expect("diagnostic is UTF-8");
        assert!(rendered.contains("invalid protocol version 'abc'"));
        assert!(rendered.contains("unsigned integer"));
    }

    #[test]
    fn protocol_value_with_whitespace_and_plus_is_accepted() {
        let version = parse_protocol_version_arg(OsStr::new(" +31 \n"))
            .expect("whitespace-wrapped value should parse");
        assert_eq!(version.as_u8(), 31);
    }

    #[test]
    fn protocol_value_negative_reports_specific_diagnostic() {
        let message = parse_protocol_version_arg(OsStr::new("-30"))
            .expect_err("negative protocol should be rejected");
        let rendered = message.to_string();
        assert!(rendered.contains("invalid protocol version '-30'"));
        assert!(rendered.contains("cannot be negative"));
    }

    #[test]
    fn protocol_value_empty_reports_specific_diagnostic() {
        let message = parse_protocol_version_arg(OsStr::new("   "))
            .expect_err("empty protocol value should be rejected");
        let rendered = message.to_string();
        assert!(rendered.contains("invalid protocol version '   '"));
        assert!(rendered.contains("must not be empty"));
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
        assert!(!parsed.delete_excluded);
    }

    #[test]
    fn delete_excluded_flag_implies_delete() {
        let parsed = super::parse_args([
            OsString::from("oc-rsync"),
            OsString::from("--delete-excluded"),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .expect("parse succeeds");

        assert!(parsed.delete);
        assert!(parsed.delete_excluded);
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
    fn size_only_flag_is_parsed() {
        let parsed = super::parse_args([
            OsString::from("oc-rsync"),
            OsString::from("--size-only"),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .expect("parse succeeds");

        assert!(parsed.size_only);
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
    fn compress_flag_is_accepted_for_local_copies() {
        use tempfile::tempdir;

        let tmp = tempdir().expect("tempdir");
        let source = tmp.path().join("compress.txt");
        let destination = tmp.path().join("compress.out");
        std::fs::write(&source, b"compressed").expect("write source");

        let (code, stdout, stderr) = run_with_args([
            OsString::from("oc-rsync"),
            OsString::from("-z"),
            source.clone().into_os_string(),
            destination.clone().into_os_string(),
        ]);

        assert_eq!(code, 0);
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());
        assert_eq!(
            std::fs::read(destination).expect("read destination"),
            b"compressed"
        );
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
    fn list_only_lists_entries_without_copying() {
        use std::fs;
        use tempfile::tempdir;

        let tmp = tempdir().expect("tempdir");
        let source_dir = tmp.path().join("src");
        fs::create_dir(&source_dir).expect("create src dir");
        let source_file = source_dir.join("file.txt");
        fs::write(&source_file, b"contents").expect("write source file");
        let destination_dir = tmp.path().join("dest");
        fs::create_dir(&destination_dir).expect("create dest dir");

        let (code, stdout, stderr) = run_with_args([
            OsString::from("oc-rsync"),
            OsString::from("--list-only"),
            source_dir.clone().into_os_string(),
            destination_dir.clone().into_os_string(),
        ]);

        assert_eq!(code, 0);
        assert!(stderr.is_empty());
        let rendered = String::from_utf8(stdout).expect("utf8 stdout");
        assert!(rendered.contains("file.txt"));
        assert!(!destination_dir.join("file.txt").exists());
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
        spawn_stub_daemon_with_protocol(responses, "32.0")
    }

    fn spawn_stub_daemon_with_protocol(
        responses: Vec<&'static str>,
        expected_protocol: &'static str,
    ) -> (std::net::SocketAddr, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind stub daemon");
        let addr = listener.local_addr().expect("local addr");

        let handle = thread::spawn(move || {
            if let Ok((stream, _)) = listener.accept() {
                handle_connection(stream, responses, expected_protocol);
            }
        });

        (addr, handle)
    }

    fn spawn_auth_stub_daemon(
        challenge: &'static str,
        expected_credentials: String,
        responses: Vec<&'static str>,
    ) -> (std::net::SocketAddr, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind auth daemon");
        let addr = listener.local_addr().expect("local addr");

        let handle = thread::spawn(move || {
            if let Ok((stream, _)) = listener.accept() {
                handle_auth_connection(
                    stream,
                    challenge,
                    &expected_credentials,
                    &responses,
                    "32.0",
                );
            }
        });

        (addr, handle)
    }

    fn handle_connection(
        mut stream: TcpStream,
        responses: Vec<&'static str>,
        expected_protocol: &str,
    ) {
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .expect("set read timeout");
        stream
            .set_write_timeout(Some(Duration::from_secs(5)))
            .expect("set write timeout");

        stream
            .write_all(LEGACY_DAEMON_GREETING.as_bytes())
            .expect("write greeting");
        stream.flush().expect("flush greeting");

        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader.read_line(&mut line).expect("read client greeting");
        let trimmed = line.trim_end_matches(['\r', '\n']);
        let expected_prefix = ["@RSYNCD: ", expected_protocol].concat();
        assert!(
            trimmed.starts_with(&expected_prefix),
            "client greeting {trimmed:?} did not begin with {expected_prefix:?}"
        );

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

    fn handle_auth_connection(
        mut stream: TcpStream,
        challenge: &'static str,
        expected_credentials: &str,
        responses: &[&'static str],
        expected_protocol: &str,
    ) {
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .expect("set read timeout");
        stream
            .set_write_timeout(Some(Duration::from_secs(5)))
            .expect("set write timeout");

        stream
            .write_all(LEGACY_DAEMON_GREETING.as_bytes())
            .expect("write greeting");
        stream.flush().expect("flush greeting");

        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader.read_line(&mut line).expect("read client greeting");
        let trimmed = line.trim_end_matches(['\r', '\n']);
        let expected_prefix = ["@RSYNCD: ", expected_protocol].concat();
        assert!(
            trimmed.starts_with(&expected_prefix),
            "client greeting {trimmed:?} did not begin with {expected_prefix:?}"
        );

        line.clear();
        reader.read_line(&mut line).expect("read request");
        assert_eq!(line, "#list\n");

        reader
            .get_mut()
            .write_all(format!("@RSYNCD: AUTHREQD {challenge}\n").as_bytes())
            .expect("write challenge");
        reader.get_mut().flush().expect("flush challenge");

        line.clear();
        reader.read_line(&mut line).expect("read credentials");
        let received = line.trim_end_matches(['\n', '\r']);
        assert_eq!(received, expected_credentials);

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
