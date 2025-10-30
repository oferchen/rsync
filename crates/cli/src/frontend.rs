//! # Overview
//!
//! `rsync_cli` implements the thin command-line front-end for the Rust `rsync`
//! workspace. The crate is intentionally small: it recognises the subset of
//! command-line switches that are currently supported (`--help`/`-h`,
//! `--version`/`-V`, `--daemon`, `--server`, `--dry-run`/`-n`, `--list-only`,
//! `--delete`/`--delete-excluded`, `--filter` (supporting `+`/`-` actions, the
//! `!` clear directive, and `merge FILE` directives), `--files-from`, `--from0`,
//! `--compare-dest`, `--copy-dest`, `--link-dest`, `--bwlimit`,
//! `--append`/`--append-verify`, `--remote-option`, `--connect-program`, and `--sparse`) and delegates local copy operations to
//! [`rsync_core::client::run_client`]. Daemon invocations are forwarded to
//! [`rsync_daemon::run`], while `--server` sessions immediately spawn the
//! system `rsync` binary (controlled by the `OC_RSYNC_FALLBACK` environment
//! variable) so remote-shell transports keep functioning while the native
//! server implementation is completed. Higher layers will eventually extend the
//! parser to cover the full upstream surface (remote modules, incremental
//! recursion, filters, etc.), but providing these entry points today allows
//! downstream tooling to depend on a stable binary path (`oc-rsync`, or `rsync`
//! via symlink) while development continues.
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
//! delegates to [`rsync_core::client::run_client`], which currently implements a
//! deterministic local copy pipeline with optional bandwidth pacing.
//!
//! # Invariants
//!
//! - `run` never panics; unexpected I/O failures surface as non-zero exit codes.
//! - Version output is delegated to [`rsync_core::version::VersionInfoReport`]
//!   so the CLI remains byte-identical with the canonical banner used by other
//!   workspace components.
//! - Help output is rendered by a dedicated helper using a static snapshot that
//!   documents the currently supported subset. The helper substitutes the
//!   invoked program name so wrappers like `oc-rsync` display branded banners
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
//! let exit_code = run(
//!     [
//!         rsync_core::branding::client_program_name(),
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
//! - [`rsync_core::version`] for the underlying banner rendering helpers.
//! - `bin/oc-rsync` for the binary crate that wires [`run`] into `main`.

use std::collections::{HashSet, VecDeque};
use std::env;
use std::ffi::{OsStr, OsString};
use std::fs::{self, File};
use std::io::ErrorKind;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{IpAddr, SocketAddr, ToSocketAddrs};
use std::num::{IntErrorKind, NonZeroU8, NonZeroU64};
#[cfg(unix)]
use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use crate::platform::{gid_t, uid_t};
use clap::{Arg, ArgAction, Command as ClapCommand, builder::OsStringValueParser};
use rsync_compress::zlib::CompressionLevel;
use rsync_core::{
    bandwidth::BandwidthParseError,
    branding::{self, Brand},
    client::{
        AddressMode, BandwidthLimit, BindAddress, ClientConfig, ClientOutcome,
        ClientProgressObserver, CompressionSetting, DeleteMode, DirMergeEnforcedKind,
        DirMergeOptions, FilterRuleKind, FilterRuleSpec, HumanReadableMode, ModuleListOptions,
        ModuleListRequest, RemoteFallbackArgs, RemoteFallbackContext, StrongChecksumChoice,
        TransferTimeout, parse_skip_compress_list, run_client_or_fallback,
        run_module_list_with_password_and_options, skip_compress_from_env,
    },
    message::{Message, Role},
    rsync_error,
    version::VersionInfoReport,
};
use rsync_logging::MessageSink;
use rsync_meta::ChmodModifiers;
use rsync_protocol::{ParseProtocolVersionErrorKind, ProtocolVersion};
#[path = "defaults.rs"]
mod defaults;
#[path = "help.rs"]
mod help;
#[path = "out_format/mod.rs"]
mod out_format;
#[path = "password.rs"]
pub(crate) mod password;
#[path = "progress/mod.rs"]
mod progress;
#[path = "server.rs"]
mod server;

#[cfg(test)]
#[path = "tests/mod.rs"]
mod tests;

pub(crate) use defaults::LIST_TIMESTAMP_FORMAT;
use defaults::{CVS_EXCLUDE_PATTERNS, ITEMIZE_CHANGES_FORMAT, SUPPORTED_OPTIONS_LIST};
use help::help_text;
pub(crate) use out_format::{OutFormat, OutFormatContext, emit_out_format, parse_out_format};
use password::{load_optional_password, load_password_file};
pub(crate) use progress::*;

/// Maximum exit code representable by a Unix process.
const MAX_EXIT_CODE: i32 = u8::MAX as i32;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ProgramName {
    Rsync,
    OcRsync,
}

impl ProgramName {
    #[inline]
    const fn as_str(self) -> &'static str {
        match self {
            Self::Rsync => Brand::Upstream.client_program_name(),
            Self::OcRsync => Brand::Oc.client_program_name(),
        }
    }

    #[inline]
    const fn brand(self) -> Brand {
        match self {
            Self::Rsync => Brand::Upstream,
            Self::OcRsync => Brand::Oc,
        }
    }
}

fn detect_program_name(program: Option<&OsStr>) -> ProgramName {
    match branding::detect_brand(program) {
        Brand::Oc => ProgramName::OcRsync,
        Brand::Upstream => ProgramName::Rsync,
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum BandwidthArgument {
    Limit(OsString),
    Disabled,
}

struct ParsedArgs {
    program_name: ProgramName,
    show_help: bool,
    show_version: bool,
    human_readable: Option<HumanReadableMode>,
    dry_run: bool,
    list_only: bool,
    remote_shell: Option<OsString>,
    connect_program: Option<OsString>,
    remote_options: Vec<OsString>,
    rsync_path: Option<OsString>,
    protect_args: Option<bool>,
    address_mode: AddressMode,
    bind_address: Option<OsString>,
    archive: bool,
    delete_mode: DeleteMode,
    delete_excluded: bool,
    backup: bool,
    backup_dir: Option<OsString>,
    backup_suffix: Option<OsString>,
    checksum: bool,
    checksum_choice: Option<StrongChecksumChoice>,
    checksum_choice_arg: Option<OsString>,
    checksum_seed: Option<u32>,
    size_only: bool,
    ignore_existing: bool,
    ignore_missing_args: bool,
    update: bool,
    remainder: Vec<OsString>,
    bwlimit: Option<BandwidthArgument>,
    max_delete: Option<OsString>,
    min_size: Option<OsString>,
    max_size: Option<OsString>,
    modify_window: Option<OsString>,
    compress: bool,
    no_compress: bool,
    compress_level: Option<OsString>,
    skip_compress: Option<OsString>,
    owner: Option<bool>,
    group: Option<bool>,
    chown: Option<OsString>,
    chmod: Vec<OsString>,
    perms: Option<bool>,
    super_mode: Option<bool>,
    times: Option<bool>,
    omit_dir_times: Option<bool>,
    omit_link_times: Option<bool>,
    acls: Option<bool>,
    numeric_ids: Option<bool>,
    hard_links: Option<bool>,
    sparse: Option<bool>,
    copy_links: Option<bool>,
    copy_dirlinks: bool,
    copy_unsafe_links: Option<bool>,
    keep_dirlinks: Option<bool>,
    safe_links: bool,
    devices: Option<bool>,
    specials: Option<bool>,
    relative: Option<bool>,
    one_file_system: Option<bool>,
    implied_dirs: Option<bool>,
    mkpath: bool,
    prune_empty_dirs: Option<bool>,
    verbosity: u8,
    progress: ProgressSetting,
    name_level: NameOutputLevel,
    name_overridden: bool,
    stats: bool,
    partial: bool,
    preallocate: bool,
    delay_updates: bool,
    partial_dir: Option<PathBuf>,
    temp_dir: Option<PathBuf>,
    link_dests: Vec<PathBuf>,
    remove_source_files: bool,
    inplace: Option<bool>,
    append: Option<bool>,
    append_verify: bool,
    msgs_to_stderr: bool,
    itemize_changes: bool,
    whole_file: Option<bool>,
    excludes: Vec<OsString>,
    includes: Vec<OsString>,
    exclude_from: Vec<OsString>,
    include_from: Vec<OsString>,
    filters: Vec<OsString>,
    compare_destinations: Vec<OsString>,
    copy_destinations: Vec<OsString>,
    link_destinations: Vec<OsString>,
    cvs_exclude: bool,
    rsync_filter_shortcuts: u8,
    files_from: Vec<OsString>,
    from0: bool,
    info: Vec<OsString>,
    debug: Vec<OsString>,
    xattrs: Option<bool>,
    no_motd: bool,
    password_file: Option<OsString>,
    protocol: Option<OsString>,
    timeout: Option<OsString>,
    contimeout: Option<OsString>,
    out_format: Option<OsString>,
    daemon_port: Option<u16>,
}

fn env_protect_args_default() -> Option<bool> {
    let value = env::var_os("RSYNC_PROTECT_ARGS")?;
    if value.is_empty() {
        return Some(true);
    }

    let normalized = value.to_string_lossy();
    let trimmed = normalized.trim();

    if trimmed.is_empty() {
        Some(true)
    } else if trimmed.eq_ignore_ascii_case("0")
        || trimmed.eq_ignore_ascii_case("no")
        || trimmed.eq_ignore_ascii_case("false")
        || trimmed.eq_ignore_ascii_case("off")
    {
        Some(false)
    } else {
        Some(true)
    }
}

/// Builds the `clap` command used for parsing.
fn clap_command(program_name: &'static str) -> ClapCommand {
    ClapCommand::new(program_name)
        .disable_help_flag(true)
        .disable_version_flag(true)
        .arg_required_else_help(false)
        .arg(
            Arg::new("help")
                .long("help")
                .help("Show this help message and exit.")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("human-readable")
                .short('h')
                .long("human-readable")
                .value_name("LEVEL")
                .help(
                    "Output numbers in a human-readable format; optional LEVEL selects 0, 1, or 2.",
                )
                .num_args(0..=1)
                .default_missing_value("1")
                .require_equals(true)
                .value_parser(OsStringValueParser::new())
                .action(ArgAction::Set)
                .overrides_with("no-human-readable"),
        )
        .arg(
            Arg::new("no-human-readable")
                .long("no-human-readable")
                .help("Disable human-readable number formatting.")
                .action(ArgAction::SetTrue)
                .overrides_with("human-readable"),
        )
        .arg(
            Arg::new("msgs2stderr")
                .long("msgs2stderr")
                .help("Route informational messages to standard error.")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("itemize-changes")
                .long("itemize-changes")
                .short('i')
                .help("Output a change summary for each updated entry.")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("out-format")
                .long("out-format")
                .value_name("FORMAT")
                .help("Customise transfer output using FORMAT for each processed entry.")
                .num_args(1)
                .value_parser(OsStringValueParser::new()),
        )
        .arg(
            Arg::new("version")
                .long("version")
                .short('V')
                .help("Output version information and exit.")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("rsh")
                .long("rsh")
                .short('e')
                .value_name("COMMAND")
                .help("Use remote shell COMMAND for remote transfers.")
                .num_args(1)
                .action(ArgAction::Set)
                .value_parser(OsStringValueParser::new()),
        )
        .arg(
            Arg::new("rsync-path")
                .long("rsync-path")
                .value_name("PROGRAM")
                .help("Use PROGRAM as the remote rsync executable during remote transfers.")
                .num_args(1)
                .action(ArgAction::Set)
                .value_parser(OsStringValueParser::new()),
        )
        .arg(
            Arg::new("connect-program")
                .long("connect-program")
                .value_name("COMMAND")
                .help(
                    "Execute COMMAND to reach rsync:// daemons (supports %H and %P placeholders).",
                )
                .num_args(1)
                .action(ArgAction::Set)
                .allow_hyphen_values(true)
                .value_parser(OsStringValueParser::new()),
        )
        .arg(
            Arg::new("port")
                .long("port")
                .value_name("PORT")
                .help("Use PORT as the default rsync:// daemon TCP port when none is specified.")
                .num_args(1)
                .action(ArgAction::Set)
                .value_parser(clap::value_parser!(u16)),
        )
        .arg(
            Arg::new("remote-option")
                .long("remote-option")
                .short('M')
                .value_name("OPTION")
                .help("Forward OPTION to the remote rsync command.")
                .action(ArgAction::Append)
                .num_args(1)
                .allow_hyphen_values(true)
                .value_parser(OsStringValueParser::new()),
        )
        .arg(
            Arg::new("protect-args")
                .long("protect-args")
                .short('s')
                .alias("secluded-args")
                .help("Protect remote shell arguments from expansion.")
                .action(ArgAction::SetTrue)
                .overrides_with("no-protect-args"),
        )
        .arg(
            Arg::new("no-protect-args")
                .long("no-protect-args")
                .alias("no-secluded-args")
                .help("Allow the remote shell to expand wildcard arguments.")
                .action(ArgAction::SetTrue)
                .overrides_with("protect-args"),
        )
        .arg(
            Arg::new("ipv4")
                .long("ipv4")
                .help("Prefer IPv4 when contacting remote hosts.")
                .action(ArgAction::SetTrue)
                .conflicts_with("ipv6"),
        )
        .arg(
            Arg::new("ipv6")
                .long("ipv6")
                .help("Prefer IPv6 when contacting remote hosts.")
                .action(ArgAction::SetTrue)
                .conflicts_with("ipv4"),
        )
        .arg(
            Arg::new("address")
                .long("address")
                .value_name("ADDRESS")
                .help("Bind outgoing connections to ADDRESS when contacting remotes.")
                .value_parser(OsStringValueParser::new()),
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
            Arg::new("mkpath")
                .long("mkpath")
                .help("Create destination's missing path components.")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("prune-empty-dirs")
                .long("prune-empty-dirs")
                .short('m')
                .help("Skip creating directories that remain empty after filters.")
                .action(ArgAction::SetTrue)
                .overrides_with("no-prune-empty-dirs"),
        )
        .arg(
            Arg::new("no-prune-empty-dirs")
                .long("no-prune-empty-dirs")
                .help("Disable pruning of empty directories.")
                .action(ArgAction::SetTrue)
                .overrides_with("prune-empty-dirs"),
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
            Arg::new("checksum-choice")
                .long("checksum-choice")
                .alias("cc")
                .value_name("ALGO")
                .help(
                    "Select the strong checksum algorithm (auto, md4, md5, xxh64, xxh3, or xxh128).",
                )
                .num_args(1)
                .action(ArgAction::Set)
                .value_parser(OsStringValueParser::new()),
        )
        .arg(
            Arg::new("checksum-seed")
                .long("checksum-seed")
                .value_name("NUM")
                .help("Set the checksum seed used by xxhash-based algorithms.")
                .num_args(1)
                .value_parser(OsStringValueParser::new()),
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
            Arg::new("modify-window")
                .long("modify-window")
                .value_name("SECS")
                .help("Treat mtimes within SECS seconds as equal when comparing files.")
                .num_args(1)
                .action(ArgAction::Set)
                .value_parser(OsStringValueParser::new()),
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
            Arg::new("copy-links")
                .long("copy-links")
                .short('L')
                .help("Transform symlinks into referent files/directories.")
                .action(ArgAction::SetTrue)
                .conflicts_with("no-copy-links"),
        )
        .arg(
            Arg::new("copy-unsafe-links")
                .long("copy-unsafe-links")
                .help("Transform unsafe symlinks into referent files/directories.")
                .action(ArgAction::SetTrue)
                .conflicts_with("no-copy-unsafe-links"),
        )
        .arg(
            Arg::new("hard-links")
                .long("hard-links")
                .short('H')
                .help("Preserve hard links between files.")
                .action(ArgAction::SetTrue)
                .conflicts_with("no-hard-links"),
        )
        .arg(
            Arg::new("copy-dirlinks")
                .long("copy-dirlinks")
                .short('k')
                .help("Transform symlinked directories into referent directories.")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("keep-dirlinks")
                .long("keep-dirlinks")
                .short('K')
                .help("Treat existing destination symlinks to directories as directories.")
                .action(ArgAction::SetTrue)
                .conflicts_with("no-keep-dirlinks"),
        )
        .arg(
            Arg::new("no-copy-links")
                .long("no-copy-links")
                .help("Preserve symlinks instead of following them.")
                .action(ArgAction::SetTrue)
                .conflicts_with("copy-links"),
        )
        .arg(
            Arg::new("no-copy-unsafe-links")
                .long("no-copy-unsafe-links")
                .help("Preserve unsafe symlinks instead of following them.")
                .action(ArgAction::SetTrue)
                .conflicts_with("copy-unsafe-links"),
        )
        .arg(
            Arg::new("no-hard-links")
                .long("no-hard-links")
                .help("Disable hard link preservation.")
                .action(ArgAction::SetTrue)
                .conflicts_with("hard-links"),
        )
        .arg(
            Arg::new("safe-links")
                .long("safe-links")
                .help("Skip symlinks that point outside the transfer root.")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("no-keep-dirlinks")
                .long("no-keep-dirlinks")
                .help("Disable treating destination symlinks to directories as directories.")
                .action(ArgAction::SetTrue)
                .conflicts_with("keep-dirlinks"),
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
            Arg::new("super")
                .long("super")
                .help(
                    "Receiver attempts super-user activities (implies --owner, --group, and --perms).",
                )
                .action(ArgAction::SetTrue)
                .overrides_with("no-super"),
        )
        .arg(
            Arg::new("no-super")
                .long("no-super")
                .help("Disable super-user handling even when running as root.")
                .action(ArgAction::SetTrue)
                .overrides_with("super"),
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
            Arg::new("one-file-system")
                .long("one-file-system")
                .short('x')
                .help("Do not cross filesystem boundaries during traversal.")
                .action(ArgAction::SetTrue)
                .overrides_with("no-one-file-system"),
        )
        .arg(
            Arg::new("no-one-file-system")
                .long("no-one-file-system")
                .help("Allow traversal across filesystem boundaries.")
                .action(ArgAction::SetTrue)
                .overrides_with("one-file-system"),
        )
        .arg(
            Arg::new("implied-dirs")
                .long("implied-dirs")
                .help("Create parent directories implied by source paths.")
                .action(ArgAction::SetTrue)
                .overrides_with("no-implied-dirs"),
        )
        .arg(
            Arg::new("no-implied-dirs")
                .long("no-implied-dirs")
                .help("Disable creation of parent directories implied by source paths.")
                .action(ArgAction::SetTrue)
                .overrides_with("implied-dirs"),
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
            Arg::new("delay-updates")
                .long("delay-updates")
                .help("Put all updated files into place at end of transfer.")
                .action(ArgAction::SetTrue)
                .overrides_with("no-delay-updates"),
        )
        .arg(
            Arg::new("no-delay-updates")
                .long("no-delay-updates")
                .help("Write updated files immediately during the transfer.")
                .action(ArgAction::SetTrue)
                .overrides_with("delay-updates"),
        )
        .arg(
            Arg::new("partial-dir")
                .long("partial-dir")
                .value_name("DIR")
                .help("Store partially transferred files in DIR.")
                .value_parser(OsStringValueParser::new())
                .overrides_with("no-partial"),
        )
        .arg(
            Arg::new("temp-dir")
                .long("temp-dir")
                .visible_alias("tmp-dir")
                .value_name("DIR")
                .help("Store temporary files in DIR while transferring.")
                .value_parser(OsStringValueParser::new()),
        )
        .arg(
            Arg::new("whole-file")
                .long("whole-file")
                .short('W')
                .help("Copy files without using the delta-transfer algorithm.")
                .action(ArgAction::SetTrue)
                .overrides_with("no-whole-file"),
        )
        .arg(
            Arg::new("no-whole-file")
                .long("no-whole-file")
                .help("Enable the delta-transfer algorithm (disable whole-file copies).")
                .action(ArgAction::SetTrue)
                .overrides_with("whole-file"),
        )
        .arg(
            Arg::new("remove-source-files")
                .long("remove-source-files")
                .help("Remove source files after a successful transfer.")
                .action(ArgAction::SetTrue)
                .overrides_with("remove-sent-files"),
        )
        .arg(
            Arg::new("remove-sent-files")
                .long("remove-sent-files")
                .help("Alias of --remove-source-files.")
                .action(ArgAction::SetTrue)
                .overrides_with("remove-source-files"),
        )
        .arg(
            Arg::new("append")
                .long("append")
                .help(
                    "Append data to existing destination files without rewriting preserved bytes.",
                )
                .action(ArgAction::SetTrue)
                .overrides_with("no-append")
                .overrides_with("append-verify"),
        )
        .arg(
            Arg::new("no-append")
                .long("no-append")
                .help("Disable append mode for destination updates.")
                .action(ArgAction::SetTrue)
                .overrides_with("append")
                .overrides_with("append-verify"),
        )
        .arg(
            Arg::new("append-verify")
                .long("append-verify")
                .help("Append data while verifying that existing bytes match the sender.")
                .action(ArgAction::SetTrue)
                .overrides_with("append")
                .overrides_with("no-append"),
        )
        .arg(
            Arg::new("preallocate")
                .long("preallocate")
                .help("Preallocate destination files before writing.")
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
                .visible_alias("del")
                .help("Remove destination files that are absent from the source.")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("delete-before")
                .long("delete-before")
                .help("Remove destination files that are absent from the source before transfers start.")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("delete-during")
                .long("delete-during")
                .help("Remove destination files that are absent from the source during directory traversal.")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("delete-delay")
                .long("delete-delay")
                .help("Compute deletions during the transfer and prune them once the run completes.")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("delete-after")
                .long("delete-after")
                .help("Remove destination files after transfers complete.")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("ignore-missing-args")
                .long("ignore-missing-args")
                .help("Skip missing source arguments without reporting an error.")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("delete-excluded")
                .long("delete-excluded")
                .help("Remove excluded destination files during deletion sweeps.")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("max-delete")
                .long("max-delete")
                .value_name("NUM")
                .help("Limit the number of deletions that may occur.")
                .num_args(1)
                .value_parser(OsStringValueParser::new()),
        )
        .arg(
            Arg::new("min-size")
                .long("min-size")
                .value_name("SIZE")
                .help("Skip files smaller than the specified size.")
                .num_args(1)
                .value_parser(OsStringValueParser::new()),
        )
        .arg(
            Arg::new("max-size")
                .long("max-size")
                .value_name("SIZE")
                .help("Skip files larger than the specified size.")
                .num_args(1)
                .value_parser(OsStringValueParser::new()),
        )
        .arg(
            Arg::new("backup")
                .long("backup")
                .short('b')
                .help("Create backups before overwriting or deleting existing entries.")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("backup-dir")
                .long("backup-dir")
                .value_name("DIR")
                .help("Store backups inside DIR instead of alongside the destination.")
                .num_args(1)
                .action(ArgAction::Set)
                .allow_hyphen_values(true)
                .value_parser(OsStringValueParser::new()),
        )
        .arg(
            Arg::new("suffix")
                .long("suffix")
                .value_name("SUFFIX")
                .help("Append SUFFIX to backup names (default '~').")
                .num_args(1)
                .action(ArgAction::Set)
                .allow_hyphen_values(true)
                .value_parser(OsStringValueParser::new()),
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
            Arg::new("compare-dest")
                .long("compare-dest")
                .value_name("DIR")
                .help("Skip creating destination files that match DIR.")
                .value_parser(OsStringValueParser::new())
                .action(ArgAction::Append),
        )
        .arg(
            Arg::new("copy-dest")
                .long("copy-dest")
                .value_name("DIR")
                .help("Copy matching files from DIR instead of the source.")
                .value_parser(OsStringValueParser::new())
                .action(ArgAction::Append),
        )
        .arg(
            Arg::new("link-dest")
                .long("link-dest")
                .value_name("DIR")
                .help("Hard-link matching files from DIR into the destination.")
                .value_parser(OsStringValueParser::new())
                .action(ArgAction::Append),
        )
        .arg(
            Arg::new("cvs-exclude")
                .long("cvs-exclude")
                .short('C')
                .help("Auto-ignore files using CVS-style ignore rules.")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("filter")
                .long("filter")
                .value_name("RULE")
                .help("Apply filter RULE (supports '+' include, '-' exclude, '!' clear, 'protect PATTERN', 'risk PATTERN', 'merge[,MODS] FILE' or '.[,MODS] FILE', and 'dir-merge[,MODS] FILE' or ':[,MODS] FILE').")
                .value_parser(OsStringValueParser::new())
                .action(ArgAction::Append),
        )
        .arg(
            Arg::new("rsync-filter")
                .short('F')
                .help("Shortcut for per-directory .rsync-filter handling (repeat to also load receiver-side files).")
                .action(ArgAction::Count),
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
            Arg::new("chown")
                .long("chown")
                .value_name("USER:GROUP")
                .help("Set destination ownership to USER and/or GROUP.")
                .value_parser(OsStringValueParser::new())
                .num_args(1),
        )
        .arg(
            Arg::new("chmod")
                .long("chmod")
                .value_name("SPEC")
                .help("Apply chmod-style SPEC modifiers to received files.")
                .action(ArgAction::Append)
                .value_parser(OsStringValueParser::new()),
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
            Arg::new("omit-dir-times")
                .long("omit-dir-times")
                .short('O')
                .help("Skip preserving directory modification times.")
                .action(ArgAction::SetTrue)
                .overrides_with("no-omit-dir-times"),
        )
        .arg(
            Arg::new("no-omit-dir-times")
                .long("no-omit-dir-times")
                .help("Preserve directory modification times.")
                .action(ArgAction::SetTrue)
                .overrides_with("omit-dir-times"),
        )
        .arg(
            Arg::new("omit-link-times")
                .long("omit-link-times")
                .help("Skip preserving symlink modification times.")
                .action(ArgAction::SetTrue)
                .overrides_with("no-omit-link-times"),
        )
        .arg(
            Arg::new("no-omit-link-times")
                .long("no-omit-link-times")
                .help("Preserve symlink modification times.")
                .action(ArgAction::SetTrue)
                .overrides_with("omit-link-times"),
        )
        .arg(
            Arg::new("acls")
                .long("acls")
                .short('A')
                .help("Preserve POSIX ACLs when supported.")
                .action(ArgAction::SetTrue)
                .overrides_with("no-acls"),
        )
        .arg(
            Arg::new("no-acls")
                .long("no-acls")
                .help("Disable POSIX ACL preservation.")
                .action(ArgAction::SetTrue)
                .overrides_with("acls"),
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
                .overrides_with("no-bwlimit")
                .value_parser(OsStringValueParser::new()),
        )
        .arg(
            Arg::new("no-bwlimit")
                .long("no-bwlimit")
                .help("Disable any configured bandwidth limit.")
                .action(ArgAction::SetTrue)
                .overrides_with("bwlimit"),
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
            Arg::new("contimeout")
                .long("contimeout")
                .value_name("SECS")
                .help("Set connection timeout in seconds (0 disables the limit).")
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
            Arg::new("compress-level")
                .long("compress-level")
                .value_name("LEVEL")
                .help("Set compression level (0 disables compression).")
                .help("Set compression level (0-9). 0 disables compression.")
                .value_parser(OsStringValueParser::new()),
        )
        .arg(
            Arg::new("skip-compress")
                .long("skip-compress")
                .value_name("LIST")
                .help("Skip compressing files with suffixes in LIST.")
                .num_args(1)
                .action(ArgAction::Set)
                .value_parser(OsStringValueParser::new()),
        )
        .arg(
            Arg::new("info")
                .long("info")
                .value_name("FLAGS")
                .help("Adjust informational messages; use --info=help for details.")
                .action(ArgAction::Append)
                .value_parser(OsStringValueParser::new())
                .value_delimiter(','),
        )
        .arg(
            Arg::new("debug")
                .long("debug")
                .value_name("FLAGS")
                .help("Adjust diagnostic output; use --debug=help for details.")
                .action(ArgAction::Append)
                .value_parser(OsStringValueParser::new())
                .value_delimiter(','),
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

    let program_name = detect_program_name(args.first().map(OsString::as_os_str));

    if args.is_empty() {
        args.push(OsString::from(program_name.as_str()));
    }

    let raw_args = args.clone();
    let (filter_indices, rsync_filter_indices) = locate_filter_arguments(&raw_args);
    let mut matches = clap_command(program_name.as_str()).try_get_matches_from(args)?;

    let show_help = matches.get_flag("help");
    let show_version = matches.get_flag("version");
    let mut human_readable = matches
        .remove_one::<OsString>("human-readable")
        .map(|value| parse_human_readable_level(value.as_os_str()))
        .transpose()?;
    if matches.get_flag("no-human-readable") {
        human_readable = Some(HumanReadableMode::Disabled);
    }
    let mut dry_run = matches.get_flag("dry-run");
    let list_only = matches.get_flag("list-only");
    let mkpath = matches.get_flag("mkpath");
    let prune_empty_dirs = if matches.get_flag("no-prune-empty-dirs") {
        Some(false)
    } else if matches.get_flag("prune-empty-dirs") {
        Some(true)
    } else {
        None
    };
    let omit_link_times = if matches.get_flag("no-omit-link-times") {
        Some(false)
    } else if matches.get_flag("omit-link-times") {
        Some(true)
    } else {
        None
    };
    if list_only {
        dry_run = true;
    }
    let remote_shell = matches
        .remove_one::<OsString>("rsh")
        .filter(|value| !value.is_empty())
        .or_else(|| env::var_os("RSYNC_RSH").filter(|value| !value.is_empty()));
    let rsync_path = matches
        .remove_one::<OsString>("rsync-path")
        .filter(|value| !value.is_empty());
    let connect_program = matches
        .remove_one::<OsString>("connect-program")
        .filter(|value| !value.is_empty());
    let daemon_port = matches.remove_one::<u16>("port");
    let remote_options = matches
        .remove_many::<OsString>("remote-option")
        .map(|values| values.collect())
        .unwrap_or_default();
    let protect_args = if matches.get_flag("no-protect-args") {
        Some(false)
    } else if matches.get_flag("protect-args") {
        Some(true)
    } else {
        env_protect_args_default()
    };
    let address_mode = if matches.get_flag("ipv4") {
        AddressMode::Ipv4
    } else if matches.get_flag("ipv6") {
        AddressMode::Ipv6
    } else {
        AddressMode::Default
    };
    let bind_address_raw = matches.remove_one::<OsString>("address");
    let archive = matches.get_flag("archive");
    let delete_flag = matches.get_flag("delete");
    let delete_before_flag = matches.get_flag("delete-before");
    let delete_during_flag = matches.get_flag("delete-during");
    let delete_delay_flag = matches.get_flag("delete-delay");
    let delete_after_flag = matches.get_flag("delete-after");
    let ignore_missing_args = matches.get_flag("ignore-missing-args");
    let delete_excluded = matches.get_flag("delete-excluded");
    let max_delete = matches.remove_one::<OsString>("max-delete");
    let min_size = matches.remove_one::<OsString>("min-size");
    let max_size = matches.remove_one::<OsString>("max-size");
    let modify_window = matches.remove_one::<OsString>("modify-window");

    let delete_mode_conflicts = [
        delete_before_flag,
        delete_during_flag,
        delete_delay_flag,
        delete_after_flag,
    ]
    .into_iter()
    .filter(|flag| *flag)
    .count();

    if delete_mode_conflicts > 1 {
        return Err(clap::Error::raw(
            clap::error::ErrorKind::ArgumentConflict,
            "--delete-before, --delete-during, --delete-delay, and --delete-after are mutually exclusive",
        ));
    }

    let mut delete_mode = if delete_before_flag {
        DeleteMode::Before
    } else if delete_delay_flag {
        DeleteMode::Delay
    } else if delete_after_flag {
        DeleteMode::After
    } else if delete_during_flag || delete_flag {
        DeleteMode::During
    } else {
        DeleteMode::Disabled
    };

    if delete_excluded && !delete_mode.is_enabled() {
        delete_mode = DeleteMode::During;
    }
    if max_delete.is_some() && !delete_mode.is_enabled() {
        delete_mode = DeleteMode::During;
    }
    let mut backup = matches.get_flag("backup");
    let backup_dir = matches.remove_one::<OsString>("backup-dir");
    let backup_suffix = matches.remove_one::<OsString>("suffix");
    if backup_dir.is_some() || backup_suffix.is_some() {
        backup = true;
    }
    let compress_flag = matches.get_flag("compress");
    let no_compress = matches.get_flag("no-compress");
    let mut compress = if no_compress { false } else { compress_flag };
    let compress_level_opt = matches.get_one::<OsString>("compress-level").cloned();
    if let Some(ref value) = compress_level_opt {
        if let Ok(setting) = parse_compress_level_argument(value.as_os_str()) {
            compress = !setting.is_disabled();
        }
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
    let chown = matches.remove_one::<OsString>("chown");
    let chmod = matches
        .remove_many::<OsString>("chmod")
        .map(|values| values.collect())
        .unwrap_or_default();
    let perms = if matches.get_flag("perms") {
        Some(true)
    } else if matches.get_flag("no-perms") {
        Some(false)
    } else {
        None
    };
    let super_mode = if matches.get_flag("super") {
        Some(true)
    } else if matches.get_flag("no-super") {
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
    let omit_dir_times = if matches.get_flag("omit-dir-times") {
        Some(true)
    } else if matches.get_flag("no-omit-dir-times") {
        Some(false)
    } else {
        None
    };
    let acls = if matches.get_flag("acls") {
        Some(true)
    } else if matches.get_flag("no-acls") {
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
    let hard_links = if matches.get_flag("no-hard-links") {
        Some(false)
    } else if matches.get_flag("hard-links") {
        Some(true)
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
    let copy_links = if matches.get_flag("no-copy-links") {
        Some(false)
    } else if matches.get_flag("copy-links") {
        Some(true)
    } else {
        None
    };
    let copy_unsafe_links_option = if matches.get_flag("no-copy-unsafe-links") {
        Some(false)
    } else if matches.get_flag("copy-unsafe-links") {
        Some(true)
    } else {
        None
    };
    let copy_dirlinks = matches.get_flag("copy-dirlinks");
    let keep_dirlinks = if matches.get_flag("no-keep-dirlinks") {
        Some(false)
    } else if matches.get_flag("keep-dirlinks") {
        Some(true)
    } else {
        None
    };
    let mut safe_links = matches.get_flag("safe-links");
    if copy_unsafe_links_option == Some(true) {
        safe_links = true;
    }
    let archive_devices = matches.get_flag("archive-devices");
    let devices = if matches.get_flag("no-devices") {
        Some(false)
    } else if matches.get_flag("devices") || archive_devices {
        Some(true)
    } else {
        None
    };
    let specials = if matches.get_flag("no-specials") {
        Some(false)
    } else if matches.get_flag("specials") || archive_devices {
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
    let one_file_system = if matches.get_flag("no-one-file-system") {
        Some(false)
    } else if matches.get_flag("one-file-system") {
        Some(true)
    } else {
        None
    };
    let implied_dirs = if matches.get_flag("implied-dirs") {
        Some(true)
    } else if matches.get_flag("no-implied-dirs") {
        Some(false)
    } else {
        None
    };
    let verbosity = matches.get_count("verbose") as u8;
    let name_level = match verbosity {
        0 => NameOutputLevel::Disabled,
        1 => NameOutputLevel::UpdatedOnly,
        _ => NameOutputLevel::UpdatedAndUnchanged,
    };
    let name_overridden = false;
    let progress_flag = matches.get_flag("progress");
    let no_progress_flag = matches.get_flag("no-progress");
    let mut progress_setting = if progress_flag {
        ProgressSetting::PerFile
    } else {
        ProgressSetting::Unspecified
    };
    let stats = matches.get_flag("stats");
    let mut partial = matches.get_flag("partial");
    let preallocate = matches.get_flag("preallocate");
    let mut delay_updates = matches.get_flag("delay-updates");
    let mut partial_dir = matches
        .remove_one::<OsString>("partial-dir")
        .map(PathBuf::from);
    let temp_dir = matches
        .remove_one::<OsString>("temp-dir")
        .map(PathBuf::from);
    if partial_dir.is_some() {
        partial = true;
    }
    if matches.get_flag("no-delay-updates") {
        delay_updates = false;
    }
    if no_progress_flag {
        progress_setting = ProgressSetting::Disabled;
    }
    if matches.get_flag("no-partial") {
        partial = false;
        partial_dir = None;
        delay_updates = false;
    }
    if matches.get_count("partial-progress") > 0 {
        partial = true;
        if !no_progress_flag {
            progress_setting = ProgressSetting::PerFile;
        }
    }
    let link_dests: Vec<PathBuf> = matches
        .remove_many::<OsString>("link-dest")
        .map(|values| {
            values
                .filter(|value| !value.is_empty())
                .map(PathBuf::from)
                .collect()
        })
        .unwrap_or_default();
    let link_destinations: Vec<OsString> = link_dests
        .iter()
        .map(|path| path.clone().into_os_string())
        .collect();
    let remove_source_files =
        matches.get_flag("remove-source-files") || matches.get_flag("remove-sent-files");
    let append_verify_flag = matches.get_flag("append-verify");
    let append_flag = matches.get_flag("append");
    let no_append_flag = matches.get_flag("no-append");
    let append = if append_verify_flag || append_flag {
        Some(true)
    } else if no_append_flag {
        Some(false)
    } else {
        None
    };
    let inplace = if matches.get_flag("no-inplace") {
        Some(false)
    } else if matches.get_flag("inplace") {
        Some(true)
    } else {
        None
    };
    let msgs_to_stderr = matches.get_flag("msgs2stderr");
    let whole_file = if matches.get_flag("whole-file") {
        Some(true)
    } else if matches.get_flag("no-whole-file") {
        Some(false)
    } else {
        None
    };
    let remainder = matches
        .remove_many::<OsString>("args")
        .map(|values| values.collect())
        .unwrap_or_default();
    let checksum = matches.get_flag("checksum");
    let size_only = matches.get_flag("size-only");
    let (checksum_choice, checksum_choice_arg) =
        match matches.remove_one::<OsString>("checksum-choice") {
            Some(value) => {
                let text = value.to_string_lossy().into_owned();
                match StrongChecksumChoice::parse(&text) {
                    Ok(choice) => {
                        let normalized = OsString::from(choice.to_argument());
                        (Some(choice), Some(normalized))
                    }
                    Err(message) => {
                        return Err(clap::Error::raw(
                            clap::error::ErrorKind::ValueValidation,
                            message.text().to_string(),
                        ));
                    }
                }
            }
            None => (None, None),
        };

    let checksum_seed = match matches.remove_one::<OsString>("checksum-seed") {
        Some(value) => match parse_checksum_seed_argument(value.as_os_str()) {
            Ok(seed) => Some(seed),
            Err(message) => {
                return Err(clap::Error::raw(
                    clap::error::ErrorKind::ValueValidation,
                    message.text().to_string(),
                ));
            }
        },
        None => None,
    };

    let compress_level = matches.remove_one::<OsString>("compress-level");
    let skip_compress = matches.remove_one::<OsString>("skip-compress");
    let no_bwlimit = matches.get_flag("no-bwlimit");
    let bwlimit = if no_bwlimit {
        Some(BandwidthArgument::Disabled)
    } else {
        matches
            .remove_one::<OsString>("bwlimit")
            .map(BandwidthArgument::Limit)
    };
    let excludes = matches
        .remove_many::<OsString>("exclude")
        .map(|values| values.collect())
        .unwrap_or_default();
    let includes = matches
        .remove_many::<OsString>("include")
        .map(|values| values.collect())
        .unwrap_or_default();
    let compare_destinations = matches
        .remove_many::<OsString>("compare-dest")
        .map(|values| values.collect())
        .unwrap_or_default();
    let copy_destinations = matches
        .remove_many::<OsString>("copy-dest")
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
    let filters: Vec<OsString> = matches
        .remove_many::<OsString>("filter")
        .map(|values| values.collect())
        .unwrap_or_default();
    let rsync_filter_shortcuts = rsync_filter_indices.len() as u8;
    let filter_args = collect_filter_arguments(&filters, &filter_indices, &rsync_filter_indices);
    let cvs_exclude = matches.get_flag("cvs-exclude");
    let files_from = matches
        .remove_many::<OsString>("files-from")
        .map(|values| values.collect())
        .unwrap_or_default();
    let from0 = matches.get_flag("from0");
    let info = matches
        .remove_many::<OsString>("info")
        .map(|values| values.collect())
        .unwrap_or_default();
    let debug = matches
        .remove_many::<OsString>("debug")
        .map(|values| values.collect())
        .unwrap_or_default();
    let ignore_existing = matches.get_flag("ignore-existing");
    let update = matches.get_flag("update");
    let password_file = matches.remove_one::<OsString>("password-file");
    let protocol = matches.remove_one::<OsString>("protocol");
    let timeout = matches.remove_one::<OsString>("timeout");
    let contimeout = matches.remove_one::<OsString>("contimeout");
    let out_format = matches.remove_one::<OsString>("out-format");
    let itemize_changes = matches.get_flag("itemize-changes");
    let no_motd = matches.get_flag("no-motd");

    Ok(ParsedArgs {
        program_name,
        show_help,
        show_version,
        human_readable,
        dry_run,
        list_only,
        remote_shell,
        connect_program,
        daemon_port,
        remote_options,
        rsync_path,
        protect_args,
        address_mode,
        bind_address: bind_address_raw,
        archive,
        delete_mode,
        delete_excluded,
        backup,
        backup_dir,
        backup_suffix,
        checksum,
        checksum_choice,
        checksum_choice_arg,
        checksum_seed,
        size_only,
        ignore_existing,
        ignore_missing_args,
        update,
        remainder,
        bwlimit,
        max_delete,
        min_size,
        max_size,
        modify_window,
        compress,
        no_compress,
        compress_level,
        skip_compress,
        owner,
        group,
        chown,
        chmod,
        perms,
        super_mode,
        times,
        omit_dir_times,
        omit_link_times,
        numeric_ids,
        hard_links,
        sparse,
        copy_links,
        copy_dirlinks,
        copy_unsafe_links: copy_unsafe_links_option,
        keep_dirlinks,
        safe_links,
        devices,
        specials,
        relative,
        one_file_system,
        implied_dirs,
        mkpath,
        prune_empty_dirs,
        verbosity,
        progress: progress_setting,
        name_level,
        name_overridden,
        stats,
        partial,
        preallocate,
        delay_updates,
        partial_dir,
        temp_dir,
        link_dests,
        remove_source_files,
        inplace,
        append,
        append_verify: append_verify_flag,
        msgs_to_stderr,
        itemize_changes,
        whole_file,
        excludes,
        includes,
        compare_destinations,
        copy_destinations,
        link_destinations,
        exclude_from,
        include_from,
        filters: filter_args.clone(),
        cvs_exclude,
        rsync_filter_shortcuts,
        files_from,
        from0,
        info,
        debug,
        acls,
        xattrs,
        no_motd,
        password_file,
        protocol,
        timeout,
        contimeout,
        out_format,
    })
}

/// Renders the help text describing the currently supported options.
fn render_help(program_name: ProgramName) -> String {
    help_text(program_name)
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
        args.push(OsString::from(Brand::Upstream.client_program_name()));
    }

    if server::server_mode_requested(&args) {
        return server::run_server_mode(&args, stdout, stderr);
    }

    if let Some(daemon_args) = server::daemon_mode_arguments(&args) {
        return server::run_daemon_mode(daemon_args, stdout, stderr);
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

fn with_output_writer<'a, Out, Err, R>(
    stdout: &'a mut Out,
    stderr: &'a mut MessageSink<Err>,
    use_stderr: bool,
    f: impl FnOnce(&'a mut dyn Write) -> R,
) -> R
where
    Out: Write + 'a,
    Err: Write + 'a,
{
    if use_stderr {
        let writer: &mut Err = stderr.writer_mut();
        f(writer)
    } else {
        f(stdout)
    }
}

fn execute<Out, Err>(parsed: ParsedArgs, stdout: &mut Out, stderr: &mut MessageSink<Err>) -> i32
where
    Out: Write,
    Err: Write,
{
    let ParsedArgs {
        program_name,
        show_help,
        show_version,
        human_readable,
        dry_run,
        list_only,
        remote_shell,
        connect_program,
        daemon_port,
        remote_options,
        rsync_path,
        protect_args,
        address_mode,
        bind_address: bind_address_raw,
        archive,
        delete_mode,
        delete_excluded,
        backup,
        backup_dir,
        backup_suffix,
        checksum,
        checksum_choice,
        checksum_choice_arg,
        checksum_seed,
        size_only,
        ignore_existing,
        ignore_missing_args,
        update,
        remainder: raw_remainder,
        bwlimit,
        max_delete,
        min_size,
        max_size,
        modify_window,
        compress: compress_flag,
        no_compress,
        compress_level,
        skip_compress,
        owner,
        group,
        chown,
        chmod,
        perms,
        super_mode,
        times,
        omit_dir_times,
        omit_link_times,
        acls,
        excludes,
        includes,
        compare_destinations,
        copy_destinations,
        link_destinations,
        exclude_from,
        include_from,
        filters,
        cvs_exclude,
        rsync_filter_shortcuts,
        files_from,
        from0,
        info,
        debug,
        numeric_ids,
        hard_links,
        sparse,
        copy_links,
        copy_dirlinks,
        copy_unsafe_links,
        keep_dirlinks,
        safe_links,
        devices,
        specials,
        relative,
        one_file_system,
        implied_dirs,
        mkpath,
        prune_empty_dirs,
        verbosity,
        progress: initial_progress,
        name_level: initial_name_level,
        name_overridden: initial_name_overridden,
        stats,
        partial,
        preallocate,
        delay_updates,
        partial_dir,
        temp_dir,
        link_dests,
        remove_source_files,
        inplace,
        append,
        append_verify,
        msgs_to_stderr,
        itemize_changes,
        whole_file,
        xattrs,
        no_motd,
        password_file,
        protocol,
        timeout,
        contimeout,
        out_format,
    } = parsed;

    let password_file = password_file.map(PathBuf::from);
    let human_readable_setting = human_readable;
    let human_readable_mode = human_readable_setting.unwrap_or(HumanReadableMode::Disabled);
    let human_readable_enabled = human_readable_mode.is_enabled();

    if password_file
        .as_deref()
        .is_some_and(|path| path == Path::new("-"))
        && files_from
            .iter()
            .any(|entry| entry.as_os_str() == OsStr::new("-"))
    {
        let message = rsync_error!(
            1,
            "--password-file=- cannot be combined with --files-from=- because both read from standard input"
        )
        .with_role(Role::Client);
        if write_message(&message, stderr).is_err() {
            let fallback = message.to_string();
            let _ = writeln!(stderr.writer_mut(), "{fallback}");
        }
        return 1;
    }
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

    let connect_timeout_setting = match contimeout {
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

    let mut out_format_template = match out_format.as_ref() {
        Some(value) => match parse_out_format(value.as_os_str()) {
            Ok(template) => Some(template),
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

    let mut fallback_out_format = out_format.clone();

    if itemize_changes {
        if fallback_out_format.is_none() {
            fallback_out_format = Some(OsString::from(ITEMIZE_CHANGES_FORMAT));
        }
        if out_format_template.is_none() {
            out_format_template = Some(
                parse_out_format(OsStr::new(ITEMIZE_CHANGES_FORMAT))
                    .expect("default itemize-changes format parses"),
            );
        }
    }

    if show_help {
        let help = render_help(program_name);
        if stdout.write_all(help.as_bytes()).is_err() {
            let _ = writeln!(stdout, "{help}");
            return 1;
        }
        return 0;
    }

    if show_version {
        let report = VersionInfoReport::for_client_brand(program_name.brand());
        let banner = report.human_readable();
        if stdout.write_all(banner.as_bytes()).is_err() {
            return 1;
        }
        return 0;
    }

    let bind_address = match bind_address_raw {
        Some(value) => match parse_bind_address_argument(&value) {
            Ok(parsed) => Some(parsed),
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

    let mut compress = compress_flag;
    let mut progress_setting = initial_progress;
    let mut stats = stats;
    let mut name_level = initial_name_level;
    let mut name_overridden = initial_name_overridden;

    let mut debug_flags_list = Vec::new();

    if !info.is_empty() {
        match parse_info_flags(&info) {
            Ok(settings) => {
                if settings.help_requested {
                    if stdout.write_all(INFO_HELP_TEXT.as_bytes()).is_err() {
                        let _ = write!(stderr.writer_mut(), "{INFO_HELP_TEXT}");
                        return 1;
                    }
                    return 0;
                }

                match settings.progress {
                    ProgressSetting::Unspecified => {}
                    value => progress_setting = value,
                }
                if let Some(value) = settings.stats {
                    stats = value;
                }
                if let Some(level) = settings.name {
                    name_level = level;
                    name_overridden = true;
                }
            }
            Err(message) => {
                if write_message(&message, stderr).is_err() {
                    let fallback = message.to_string();
                    let _ = writeln!(stderr.writer_mut(), "{fallback}");
                }
                return 1;
            }
        }
    }

    if !debug.is_empty() {
        match parse_debug_flags(&debug) {
            Ok(settings) => {
                if settings.help_requested {
                    if stdout.write_all(DEBUG_HELP_TEXT.as_bytes()).is_err() {
                        let _ = write!(stderr.writer_mut(), "{DEBUG_HELP_TEXT}");
                        return 1;
                    }
                    return 0;
                }

                debug_flags_list = settings.flags;
            }
            Err(message) => {
                if write_message(&message, stderr).is_err() {
                    let fallback = message.to_string();
                    let _ = writeln!(stderr.writer_mut(), "{fallback}");
                }
                return 1;
            }
        }
    }

    let progress_mode = progress_setting.resolved();

    let bandwidth_limit = match bwlimit.as_ref() {
        Some(BandwidthArgument::Limit(value)) => match parse_bandwidth_limit(value.as_os_str()) {
            Ok(limit) => limit,
            Err(message) => {
                if write_message(&message, stderr).is_err() {
                    let _ = writeln!(stderr.writer_mut(), "{}", message);
                }
                return 1;
            }
        },
        Some(BandwidthArgument::Disabled) | None => None,
    };

    let fallback_bwlimit = match (bandwidth_limit.as_ref(), bwlimit.as_ref()) {
        (Some(limit), _) => Some(limit.fallback_argument()),
        (None, Some(BandwidthArgument::Limit(_))) => {
            Some(BandwidthLimit::fallback_unlimited_argument())
        }
        (None, Some(BandwidthArgument::Disabled)) => {
            Some(BandwidthLimit::fallback_unlimited_argument())
        }
        (None, None) => None,
    };

    let max_delete_limit = match max_delete {
        Some(ref value) => match parse_max_delete_argument(value.as_os_str()) {
            Ok(limit) => Some(limit),
            Err(message) => {
                if write_message(&message, stderr).is_err() {
                    let _ = writeln!(stderr.writer_mut(), "{}", message);
                }
                return 1;
            }
        },
        None => None,
    };

    let min_size_limit = match min_size.as_ref() {
        Some(value) => match parse_size_limit_argument(value.as_os_str(), "--min-size") {
            Ok(limit) => Some(limit),
            Err(message) => {
                if write_message(&message, stderr).is_err() {
                    let _ = writeln!(stderr.writer_mut(), "{}", message);
                }
                return 1;
            }
        },
        None => None,
    };

    let max_size_limit = match max_size.as_ref() {
        Some(value) => match parse_size_limit_argument(value.as_os_str(), "--max-size") {
            Ok(limit) => Some(limit),
            Err(message) => {
                if write_message(&message, stderr).is_err() {
                    let _ = writeln!(stderr.writer_mut(), "{}", message);
                }
                return 1;
            }
        },
        None => None,
    };

    let modify_window_setting = match modify_window.as_ref() {
        Some(value) => match parse_modify_window_argument(value.as_os_str()) {
            Ok(window) => Some(window),
            Err(message) => {
                if write_message(&message, stderr).is_err() {
                    let _ = writeln!(stderr.writer_mut(), "{}", message);
                }
                return 1;
            }
        },
        None => None,
    };

    let compress_level_setting = match compress_level {
        Some(ref value) => match parse_compress_level(value.as_os_str()) {
            Ok(setting) => Some(setting),
            Err(message) => {
                if write_message(&message, stderr).is_err() {
                    let _ = writeln!(stderr.writer_mut(), "{}", message);
                }
                return 1;
            }
        },
        None => None,
    };

    let mut compression_level_override = None;
    if let Some(setting) = compress_level_setting {
        match setting {
            CompressLevelArg::Disable => {
                compress = false;
            }
            CompressLevelArg::Level(level) => {
                if !no_compress {
                    compress = true;
                    compression_level_override = Some(CompressionLevel::precise(level));
                }
            }
        }
    }

    let compress_disabled =
        no_compress || matches!(compress_level_setting, Some(CompressLevelArg::Disable));
    let compress_level_cli = match (compress_level_setting, compress_disabled) {
        (Some(CompressLevelArg::Level(level)), false) => {
            Some(OsString::from(level.get().to_string()))
        }
        (Some(CompressLevelArg::Disable), _) => Some(OsString::from("0")),
        _ => None,
    };

    let skip_compress_list = if let Some(value) = skip_compress.as_ref() {
        match parse_skip_compress_list(value.as_os_str()) {
            Ok(list) => Some(list),
            Err(message) => {
                if write_message(&message, stderr).is_err() {
                    let fallback = message.to_string();
                    let _ = writeln!(stderr.writer_mut(), "{fallback}");
                }
                return 1;
            }
        }
    } else {
        match skip_compress_from_env("RSYNC_SKIP_COMPRESS") {
            Ok(value) => value,
            Err(message) => {
                if write_message(&message, stderr).is_err() {
                    let fallback = message.to_string();
                    let _ = writeln!(stderr.writer_mut(), "{fallback}");
                }
                return 1;
            }
        }
    };

    let mut compression_setting = CompressionSetting::default();
    if let Some(ref value) = compress_level {
        match parse_compress_level_argument(value.as_os_str()) {
            Ok(setting) => {
                compression_setting = setting;
                compress = !setting.is_disabled();
            }
            Err(message) => {
                if write_message(&message, stderr).is_err() {
                    let fallback = message.to_string();
                    let _ = writeln!(stderr.writer_mut(), "{}", fallback);
                }
                return 1;
            }
        }
    }

    let numeric_ids_option = numeric_ids;
    let whole_file_option = whole_file;

    #[allow(unused_variables)]
    let preserve_acls = acls.unwrap_or(false);

    #[cfg(not(feature = "acl"))]
    if preserve_acls {
        let message =
            rsync_error!(1, "POSIX ACLs are not supported on this client").with_role(Role::Client);
        if write_message(&message, stderr).is_err() {
            let _ = writeln!(
                stderr.writer_mut(),
                "POSIX ACLs are not supported on this client"
            );
        }
        return 1;
    }

    let parsed_chown = match chown.as_ref() {
        Some(value) => match parse_chown_argument(value.as_os_str()) {
            Ok(parsed) => Some(parsed),
            Err(message) => {
                if write_message(&message, stderr).is_err() {
                    let fallback = message.to_string();
                    let _ = writeln!(stderr.writer_mut(), "{}", fallback);
                }
                return 1;
            }
        },
        None => None,
    };

    let owner_override_value = parsed_chown.as_ref().and_then(|value| value.owner);
    let group_override_value = parsed_chown.as_ref().and_then(|value| value.group);
    let chown_spec = parsed_chown.as_ref().map(|value| value.spec.clone());

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
        let module_list_port = daemon_port.unwrap_or(ModuleListRequest::DEFAULT_PORT);
        match ModuleListRequest::from_operands_with_port(&remainder, module_list_port) {
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

                let list_options = ModuleListOptions::default()
                    .suppress_motd(no_motd)
                    .with_address_mode(address_mode)
                    .with_bind_address(bind_address.as_ref().map(|addr| addr.socket()))
                    .with_connect_program(connect_program.clone());
                return match run_module_list_with_password_and_options(
                    request,
                    list_options,
                    password_override,
                    timeout_setting,
                    connect_timeout_setting,
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

    let files_from_used = !files_from.is_empty();
    let implied_dirs_option = implied_dirs;
    let implied_dirs = implied_dirs_option.unwrap_or(true);
    let requires_remote_fallback = transfer_requires_remote(&remainder, &file_list_operands);
    let fallback_required = requires_remote_fallback;

    let append_for_fallback = if append_verify { Some(true) } else { append };
    let fallback_one_file_system = one_file_system;

    let fallback_args = if fallback_required {
        let mut fallback_info_flags = info.clone();
        let fallback_debug_flags = debug_flags_list.clone();
        if protect_args.unwrap_or(false)
            && matches!(progress_setting, ProgressSetting::Unspecified)
            && !info_flags_include_progress(&fallback_info_flags)
        {
            fallback_info_flags.push(OsString::from("progress2"));
        }
        let delete_for_fallback =
            delete_mode.is_enabled() || delete_excluded || max_delete_limit.is_some();
        let daemon_password = match password_file.as_deref() {
            Some(path) if path == Path::new("-") => match load_password_file(path) {
                Ok(bytes) => Some(bytes),
                Err(message) => {
                    if write_message(&message, stderr).is_err() {
                        let fallback = message.to_string();
                        let _ = writeln!(stderr.writer_mut(), "{fallback}");
                    }
                    return 1;
                }
            },
            _ => None,
        };
        Some(RemoteFallbackArgs {
            dry_run,
            list_only,
            remote_shell: remote_shell.clone(),
            remote_options: remote_options.clone(),
            connect_program: connect_program.clone(),
            port: daemon_port,
            bind_address: bind_address
                .as_ref()
                .map(|address| address.raw().to_os_string()),
            protect_args,
            human_readable: human_readable_setting,
            archive,
            delete: delete_for_fallback,
            delete_mode,
            delete_excluded,
            max_delete: max_delete_limit,
            min_size: min_size.clone(),
            max_size: max_size.clone(),
            checksum,
            checksum_choice: checksum_choice_arg.clone(),
            checksum_seed,
            size_only,
            ignore_existing,
            ignore_missing_args,
            update,
            modify_window: modify_window_setting,
            compress,
            compress_disabled,
            compress_level: compress_level_cli.clone(),
            skip_compress: skip_compress.clone(),
            chown: chown_spec.clone(),
            owner,
            group,
            chmod: chmod.clone(),
            perms,
            super_mode,
            times,
            omit_dir_times,
            omit_link_times,
            numeric_ids: numeric_ids_option,
            hard_links,
            copy_links,
            copy_dirlinks,
            copy_unsafe_links,
            keep_dirlinks,
            safe_links,
            sparse,
            devices,
            specials,
            relative,
            one_file_system: fallback_one_file_system,
            implied_dirs: implied_dirs_option,
            mkpath,
            prune_empty_dirs,
            verbosity,
            progress: progress_mode.is_some(),
            stats,
            partial,
            preallocate,
            delay_updates,
            partial_dir: partial_dir.clone(),
            temp_directory: temp_dir.clone(),
            backup,
            backup_dir: backup_dir.clone().map(PathBuf::from),
            backup_suffix: backup_suffix.clone(),
            link_dests: link_dests.clone(),
            remove_source_files,
            append: append_for_fallback,
            append_verify,
            inplace,
            msgs_to_stderr,
            whole_file: whole_file_option,
            bwlimit: fallback_bwlimit.clone(),
            excludes: excludes.clone(),
            includes: includes.clone(),
            exclude_from: exclude_from.clone(),
            include_from: include_from.clone(),
            filters: filters.clone(),
            rsync_filter_shortcuts,
            compare_destinations: compare_destinations.clone(),
            copy_destinations: copy_destinations.clone(),
            link_destinations: link_destinations.clone(),
            cvs_exclude,
            info_flags: fallback_info_flags,
            debug_flags: fallback_debug_flags,
            files_from_used,
            file_list_entries: file_list_operands.clone(),
            from0,
            password_file: password_file.clone(),
            daemon_password,
            protocol: desired_protocol,
            timeout: timeout_setting,
            connect_timeout: connect_timeout_setting,
            out_format: fallback_out_format.clone(),
            no_motd,
            address_mode,
            fallback_binary: None,
            rsync_path: rsync_path.clone(),
            remainder: remainder.clone(),
            #[cfg(feature = "acl")]
            acls,
            #[cfg(feature = "xattr")]
            xattrs,
            itemize_changes,
        })
    } else {
        None
    };

    let numeric_ids = numeric_ids_option.unwrap_or(false);

    if !fallback_required {
        if rsync_path.is_some() {
            let message = rsync_error!(
                1,
                "the --rsync-path option may only be used with remote connections"
            )
            .with_role(Role::Client);
            if write_message(&message, stderr).is_err() {
                let _ = writeln!(
                    stderr.writer_mut(),
                    "the --rsync-path option may only be used with remote connections"
                );
            }
            return 1;
        }

        if !remote_options.is_empty() {
            let message = rsync_error!(
                1,
                "the --remote-option option may only be used with remote connections"
            )
            .with_role(Role::Client);
            if write_message(&message, stderr).is_err() {
                let _ = writeln!(
                    stderr.writer_mut(),
                    "the --remote-option option may only be used with remote connections"
                );
            }
            return 1;
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

        if connect_program.is_some() {
            let message = rsync_error!(
                1,
                "the --connect-program option may only be used when accessing an rsync daemon"
            )
            .with_role(Role::Client);
            if write_message(&message, stderr).is_err() {
                let _ = writeln!(
                    stderr.writer_mut(),
                    "the --connect-program option may only be used when accessing an rsync daemon"
                );
            }
            return 1;
        }
    }

    let mut transfer_operands = Vec::with_capacity(file_list_operands.len() + remainder.len());
    transfer_operands.append(&mut file_list_operands);
    transfer_operands.extend(remainder);

    let preserve_owner = if parsed_chown
        .as_ref()
        .and_then(|value| value.owner)
        .is_some()
    {
        true
    } else if let Some(value) = owner {
        value
    } else if super_mode == Some(true) {
        true
    } else {
        archive
    };
    let preserve_group = if parsed_chown
        .as_ref()
        .and_then(|value| value.group)
        .is_some()
    {
        true
    } else if let Some(value) = group {
        value
    } else if super_mode == Some(true) {
        true
    } else {
        archive
    };
    let preserve_permissions = if let Some(value) = perms {
        value
    } else if super_mode == Some(true) {
        true
    } else {
        archive
    };
    let preserve_times = times.unwrap_or(archive);
    let omit_dir_times_setting = omit_dir_times.unwrap_or(false);
    let omit_link_times_setting = omit_link_times.unwrap_or(false);
    let preserve_devices = devices.unwrap_or(archive);
    let preserve_specials = specials.unwrap_or(archive);
    let preserve_hard_links = hard_links.unwrap_or(false);
    let sparse = sparse.unwrap_or(false);
    let copy_links = copy_links.unwrap_or(false);
    let copy_unsafe_links = copy_unsafe_links.unwrap_or(false);
    let keep_dirlinks_flag = keep_dirlinks.unwrap_or(false);
    let relative = relative.unwrap_or(false);
    let one_file_system = fallback_one_file_system.unwrap_or(false);

    let mut chmod_modifiers: Option<ChmodModifiers> = None;
    for spec in &chmod {
        let spec_text = spec.to_string_lossy();
        let trimmed = spec_text.trim();
        match ChmodModifiers::parse(trimmed) {
            Ok(parsed) => {
                if let Some(existing) = &mut chmod_modifiers {
                    existing.extend(parsed);
                } else {
                    chmod_modifiers = Some(parsed);
                }
            }
            Err(error) => {
                let formatted = format!(
                    "failed to parse --chmod specification '{}': {}",
                    spec_text, error
                );
                let message = rsync_error!(1, formatted).with_role(Role::Client);
                if write_message(&message, stderr).is_err() {
                    let fallback = message.to_string();
                    let _ = writeln!(stderr.writer_mut(), "{fallback}");
                }
                return 1;
            }
        }
    }

    let mut builder = ClientConfig::builder()
        .transfer_args(transfer_operands)
        .address_mode(address_mode)
        .connect_program(connect_program.clone())
        .bind_address(bind_address.clone())
        .dry_run(dry_run)
        .list_only(list_only)
        .delete(delete_mode.is_enabled() || delete_excluded || max_delete_limit.is_some())
        .delete_excluded(delete_excluded)
        .max_delete(max_delete_limit)
        .min_file_size(min_size_limit)
        .max_file_size(max_size_limit)
        .backup(backup)
        .backup_directory(backup_dir.clone().map(PathBuf::from))
        .backup_suffix(backup_suffix.clone())
        .bandwidth_limit(bandwidth_limit)
        .compression_setting(compression_setting)
        .compress(compress)
        .compression_level(compression_level_override)
        .owner(preserve_owner)
        .owner_override(owner_override_value)
        .group(preserve_group)
        .group_override(group_override_value)
        .chmod(chmod_modifiers.clone())
        .permissions(preserve_permissions)
        .times(preserve_times)
        .modify_window(modify_window_setting)
        .omit_dir_times(omit_dir_times_setting)
        .omit_link_times(omit_link_times_setting)
        .devices(preserve_devices)
        .specials(preserve_specials)
        .checksum(checksum)
        .checksum_seed(checksum_seed)
        .size_only(size_only)
        .ignore_existing(ignore_existing)
        .ignore_missing_args(ignore_missing_args)
        .update(update)
        .numeric_ids(numeric_ids)
        .hard_links(preserve_hard_links)
        .sparse(sparse)
        .copy_links(copy_links)
        .copy_dirlinks(copy_dirlinks)
        .copy_unsafe_links(copy_unsafe_links)
        .keep_dirlinks(keep_dirlinks_flag)
        .safe_links(safe_links)
        .relative_paths(relative)
        .one_file_system(one_file_system)
        .implied_dirs(implied_dirs)
        .human_readable(human_readable_enabled)
        .mkpath(mkpath)
        .prune_empty_dirs(prune_empty_dirs.unwrap_or(false))
        .verbosity(verbosity)
        .progress(progress_mode.is_some())
        .stats(stats)
        .debug_flags(debug_flags_list.clone())
        .partial(partial)
        .preallocate(preallocate)
        .delay_updates(delay_updates)
        .partial_directory(partial_dir.clone())
        .temp_directory(temp_dir.clone())
        .delay_updates(delay_updates)
        .extend_link_dests(link_dests.clone())
        .remove_source_files(remove_source_files)
        .inplace(inplace.unwrap_or(false))
        .append(append.unwrap_or(false))
        .append_verify(append_verify)
        .whole_file(whole_file_option.unwrap_or(true))
        .timeout(timeout_setting)
        .connect_timeout(connect_timeout_setting);

    if let Some(choice) = checksum_choice {
        builder = builder.checksum_choice(choice);
    }

    for path in &compare_destinations {
        builder = builder.compare_destination(PathBuf::from(path));
    }

    for path in &copy_destinations {
        builder = builder.copy_destination(PathBuf::from(path));
    }

    for path in &link_destinations {
        builder = builder.link_destination(PathBuf::from(path));
    }
    #[cfg(feature = "acl")]
    {
        builder = builder.acls(preserve_acls);
    }
    #[cfg(feature = "xattr")]
    {
        builder = builder.xattrs(xattrs.unwrap_or(false));
    }

    if let Some(list) = skip_compress_list {
        builder = builder.skip_compress(list);
    }

    builder = match delete_mode {
        DeleteMode::Before => builder.delete_before(true),
        DeleteMode::After => builder.delete_after(true),
        DeleteMode::Delay => builder.delete_delay(true),
        DeleteMode::During | DeleteMode::Disabled => builder,
    };

    let force_event_collection = itemize_changes
        || out_format_template.is_some()
        || !matches!(name_level, NameOutputLevel::Disabled);
    builder = builder.force_event_collection(force_event_collection);

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
    if cvs_exclude {
        if let Err(message) = append_cvs_exclude_rules(&mut filter_rules) {
            if write_message(&message, stderr).is_err() {
                let fallback = message.to_string();
                let _ = writeln!(stderr.writer_mut(), "{}", fallback);
            }
            return 1;
        }
    }

    let mut merge_stack = HashSet::new();
    let merge_base = env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    for filter in &filters {
        match parse_filter_directive(filter.as_os_str()) {
            Ok(FilterDirective::Rule(spec)) => filter_rules.push(spec),
            Ok(FilterDirective::Merge(directive)) => {
                let effective_options =
                    merge_directive_options(&DirMergeOptions::default(), &directive);
                let directive = directive.with_options(effective_options);
                if let Err(message) = apply_merge_directive(
                    directive,
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

    if let Some(args) = fallback_args {
        let outcome = {
            let mut stderr_writer = stderr.writer_mut();
            run_client_or_fallback(
                config,
                None,
                Some(RemoteFallbackContext::new(stdout, &mut stderr_writer, args)),
            )
        };

        return match outcome {
            Ok(ClientOutcome::Fallback(summary)) => summary.exit_code(),
            Ok(ClientOutcome::Local(_)) => {
                unreachable!("local outcome returned without fallback context")
            }
            Err(error) => {
                if write_message(error.message(), stderr).is_err() {
                    let fallback = error.message().to_string();
                    let _ = writeln!(stderr.writer_mut(), "{}", fallback);
                }
                error.exit_code()
            }
        };
    }

    let mut live_progress = if let Some(mode) = progress_mode {
        Some(with_output_writer(
            stdout,
            stderr,
            msgs_to_stderr,
            |writer| LiveProgress::new(writer, mode, human_readable_mode),
        ))
    } else {
        None
    };

    let result = {
        let observer = live_progress
            .as_mut()
            .map(|observer| observer as &mut dyn ClientProgressObserver);
        run_client_or_fallback::<io::Sink, io::Sink>(config, observer, None)
    };

    match result {
        Ok(ClientOutcome::Local(summary)) => {
            let summary = *summary;
            let progress_rendered_live = live_progress.as_ref().is_some_and(LiveProgress::rendered);

            if let Some(observer) = live_progress {
                if let Err(error) = observer.finish() {
                    let _ = with_output_writer(stdout, stderr, msgs_to_stderr, |writer| {
                        writeln!(writer, "warning: failed to render progress output: {error}")
                    });
                }
            }

            let out_format_context = OutFormatContext::default();
            if let Err(error) = with_output_writer(stdout, stderr, msgs_to_stderr, |writer| {
                emit_transfer_summary(
                    &summary,
                    verbosity,
                    progress_mode,
                    stats,
                    progress_rendered_live,
                    list_only,
                    out_format_template.as_ref(),
                    &out_format_context,
                    name_level,
                    name_overridden,
                    human_readable_mode,
                    writer,
                )
            }) {
                let _ = with_output_writer(stdout, stderr, msgs_to_stderr, |writer| {
                    writeln!(
                        writer,
                        "warning: failed to render transfer summary: {error}"
                    )
                });
            }
            0
        }
        Ok(ClientOutcome::Fallback(_)) => {
            unreachable!("fallback outcome returned without fallback args")
        }
        Err(error) => {
            if let Some(observer) = live_progress {
                if let Err(err) = observer.finish() {
                    let _ = with_output_writer(stdout, stderr, msgs_to_stderr, |writer| {
                        writeln!(writer, "warning: failed to render progress output: {err}")
                    });
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

fn supported_protocols_list() -> &'static str {
    ProtocolVersion::supported_protocol_numbers_display()
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

fn parse_human_readable_level(value: &OsStr) -> Result<HumanReadableMode, clap::Error> {
    let text = value.to_string_lossy();
    let trimmed = text.trim_matches(|ch: char| ch.is_ascii_whitespace());
    let display = if trimmed.is_empty() {
        text.as_ref()
    } else {
        trimmed
    };

    if trimmed.is_empty() {
        return Err(clap::Error::raw(
            clap::error::ErrorKind::InvalidValue,
            "human-readable level must not be empty",
        ));
    }

    match trimmed {
        "0" => Ok(HumanReadableMode::Disabled),
        "1" => Ok(HumanReadableMode::Enabled),
        "2" => Ok(HumanReadableMode::Combined),
        _ => Err(clap::Error::raw(
            clap::error::ErrorKind::InvalidValue,
            format!(
                "invalid human-readable level '{}': expected 0, 1, or 2",
                display
            ),
        )),
    }
}

fn parse_max_delete_argument(value: &OsStr) -> Result<u64, Message> {
    let text = value.to_string_lossy();
    let trimmed = text.trim_matches(|ch: char| ch.is_ascii_whitespace());
    let display = if trimmed.is_empty() {
        text.as_ref()
    } else {
        trimmed
    };

    if trimmed.is_empty() {
        return Err(rsync_error!(1, "--max-delete value must not be empty").with_role(Role::Client));
    }

    if trimmed.starts_with('-') {
        return Err(rsync_error!(
            1,
            format!(
                "invalid --max-delete '{}': deletion limit must be non-negative",
                display
            )
        )
        .with_role(Role::Client));
    }

    let normalized = trimmed.strip_prefix('+').unwrap_or(trimmed);

    match normalized.parse::<u64>() {
        Ok(value) => Ok(value),
        Err(error) => {
            let detail = match error.kind() {
                IntErrorKind::InvalidDigit => "deletion limit must be an unsigned integer",
                IntErrorKind::PosOverflow | IntErrorKind::NegOverflow => {
                    "deletion limit exceeds the supported range"
                }
                IntErrorKind::Empty => "--max-delete value must not be empty",
                _ => "deletion limit is invalid",
            };
            Err(
                rsync_error!(1, format!("invalid --max-delete '{}': {}", display, detail))
                    .with_role(Role::Client),
            )
        }
    }
}

fn parse_checksum_seed_argument(value: &OsStr) -> Result<u32, Message> {
    let text = value.to_string_lossy();
    let trimmed = text.trim_matches(|ch: char| ch.is_ascii_whitespace());
    let display = if trimmed.is_empty() {
        text.as_ref()
    } else {
        trimmed
    };

    if trimmed.is_empty() {
        return Err(
            rsync_error!(1, "--checksum-seed value must not be empty").with_role(Role::Client)
        );
    }

    if trimmed.starts_with('-') {
        return Err(rsync_error!(
            1,
            format!(
                "invalid --checksum-seed value '{}': must be non-negative",
                display
            )
        )
        .with_role(Role::Client));
    }

    let normalized = trimmed.strip_prefix('+').unwrap_or(trimmed);

    normalized.parse::<u32>().map_err(|_| {
        rsync_error!(
            1,
            format!(
                "invalid --checksum-seed value '{}': must be between 0 and {}",
                display,
                u32::MAX
            )
        )
        .with_role(Role::Client)
    })
}

fn parse_modify_window_argument(value: &OsStr) -> Result<u64, Message> {
    let text = value.to_string_lossy();
    let trimmed = text.trim_matches(|ch: char| ch.is_ascii_whitespace());
    let display = if trimmed.is_empty() {
        text.as_ref()
    } else {
        trimmed
    };

    if trimmed.is_empty() {
        return Err(
            rsync_error!(1, "--modify-window value must not be empty").with_role(Role::Client)
        );
    }

    if trimmed.starts_with('-') {
        return Err(rsync_error!(
            1,
            format!(
                "invalid --modify-window '{}': window must be non-negative",
                display
            )
        )
        .with_role(Role::Client));
    }

    let normalized = trimmed.strip_prefix('+').unwrap_or(trimmed);

    match normalized.parse::<u64>() {
        Ok(value) => Ok(value),
        Err(error) => {
            let detail = match error.kind() {
                IntErrorKind::InvalidDigit => "window must be an unsigned integer",
                IntErrorKind::PosOverflow | IntErrorKind::NegOverflow => {
                    "window exceeds the supported range"
                }
                IntErrorKind::Empty => "--modify-window value must not be empty",
                _ => "window is invalid",
            };
            Err(rsync_error!(
                1,
                format!("invalid --modify-window '{}': {}", display, detail)
            )
            .with_role(Role::Client))
        }
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum SizeParseError {
    Empty,
    Negative,
    Invalid,
    TooLarge,
}

fn parse_size_limit_argument(value: &OsStr, flag: &str) -> Result<u64, Message> {
    let text = value.to_string_lossy();
    let trimmed = text.trim_matches(|ch: char| ch.is_ascii_whitespace());
    let display = if trimmed.is_empty() {
        text.as_ref()
    } else {
        trimmed
    };

    match parse_size_spec(trimmed) {
        Ok(limit) => Ok(limit),
        Err(SizeParseError::Empty) => {
            Err(rsync_error!(1, format!("{flag} value must not be empty")).with_role(Role::Client))
        }
        Err(SizeParseError::Negative) => Err(rsync_error!(
            1,
            format!("invalid {flag} '{display}': size must be non-negative")
        )
        .with_role(Role::Client)),
        Err(SizeParseError::Invalid) => Err(rsync_error!(
            1,
            format!(
                "invalid {flag} '{display}': expected a size with an optional K/M/G/T/P/E suffix"
            )
        )
        .with_role(Role::Client)),
        Err(SizeParseError::TooLarge) => Err(rsync_error!(
            1,
            format!("invalid {flag} '{display}': size exceeds the supported range")
        )
        .with_role(Role::Client)),
    }
}

fn parse_size_spec(text: &str) -> Result<u64, SizeParseError> {
    if text.is_empty() {
        return Err(SizeParseError::Empty);
    }

    let mut unsigned = text;
    let mut negative = false;

    if let Some(first) = unsigned.chars().next() {
        match first {
            '+' => {
                unsigned = &unsigned[first.len_utf8()..];
            }
            '-' => {
                negative = true;
                unsigned = &unsigned[first.len_utf8()..];
            }
            _ => {}
        }
    }

    if unsigned.is_empty() {
        return Err(SizeParseError::Empty);
    }

    if negative {
        return Err(SizeParseError::Negative);
    }

    let mut digits_seen = false;
    let mut decimal_seen = false;
    let mut numeric_end = unsigned.len();

    for (index, ch) in unsigned.char_indices() {
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

    let numeric_part = &unsigned[..numeric_end];
    let remainder = &unsigned[numeric_end..];

    if !digits_seen || numeric_part == "." || numeric_part == "," {
        return Err(SizeParseError::Invalid);
    }

    let (integer_part, fractional_part, denominator) =
        parse_decimal_components_for_size(numeric_part)?;

    let (exponent, mut remainder_after_suffix) = if remainder.is_empty() {
        (0u32, remainder)
    } else {
        let mut chars = remainder.chars();
        let ch = chars.next().unwrap();
        (
            match ch.to_ascii_lowercase() {
                'b' => 0,
                'k' => 1,
                'm' => 2,
                'g' => 3,
                't' => 4,
                'p' => 5,
                'e' => 6,
                _ => return Err(SizeParseError::Invalid),
            },
            chars.as_str(),
        )
    };

    let mut base = 1024u32;

    if !remainder_after_suffix.is_empty() {
        let bytes = remainder_after_suffix.as_bytes();
        match bytes[0] {
            b'b' | b'B' => {
                base = 1000;
                remainder_after_suffix = &remainder_after_suffix[1..];
            }
            b'i' | b'I' => {
                if bytes.len() < 2 {
                    return Err(SizeParseError::Invalid);
                }
                if matches!(bytes[1], b'b' | b'B') {
                    remainder_after_suffix = &remainder_after_suffix[2..];
                } else {
                    return Err(SizeParseError::Invalid);
                }
            }
            _ => {}
        }
    }

    if !remainder_after_suffix.is_empty() {
        return Err(SizeParseError::Invalid);
    }

    let scale = pow_u128_for_size(base, exponent)?;

    let numerator = integer_part
        .checked_mul(denominator)
        .and_then(|value| value.checked_add(fractional_part))
        .ok_or(SizeParseError::TooLarge)?;
    let product = numerator
        .checked_mul(scale)
        .ok_or(SizeParseError::TooLarge)?;

    let value = product / denominator;
    if value > u64::MAX as u128 {
        return Err(SizeParseError::TooLarge);
    }

    Ok(value as u64)
}

fn parse_decimal_components_for_size(text: &str) -> Result<(u128, u128, u128), SizeParseError> {
    let mut integer = 0u128;
    let mut fraction = 0u128;
    let mut denominator = 1u128;
    let mut saw_decimal = false;

    for ch in text.chars() {
        match ch {
            '0'..='9' => {
                let digit = u128::from(ch as u8 - b'0');
                if saw_decimal {
                    denominator = denominator
                        .checked_mul(10)
                        .ok_or(SizeParseError::TooLarge)?;
                    fraction = fraction
                        .checked_mul(10)
                        .and_then(|value| value.checked_add(digit))
                        .ok_or(SizeParseError::TooLarge)?;
                } else {
                    integer = integer
                        .checked_mul(10)
                        .and_then(|value| value.checked_add(digit))
                        .ok_or(SizeParseError::TooLarge)?;
                }
            }
            '.' | ',' => {
                if saw_decimal {
                    return Err(SizeParseError::Invalid);
                }
                saw_decimal = true;
            }
            _ => return Err(SizeParseError::Invalid),
        }
    }

    Ok((integer, fraction, denominator))
}

fn pow_u128_for_size(base: u32, exponent: u32) -> Result<u128, SizeParseError> {
    u128::from(base)
        .checked_pow(exponent)
        .ok_or(SizeParseError::TooLarge)
}

impl Default for ProgressSetting {
    fn default() -> Self {
        Self::Unspecified
    }
}

#[derive(Default)]
struct InfoFlagSettings {
    progress: ProgressSetting,
    stats: Option<bool>,
    name: Option<NameOutputLevel>,
    help_requested: bool,
}

impl InfoFlagSettings {
    fn enable_all(&mut self) {
        self.progress = ProgressSetting::PerFile;
        self.stats = Some(true);
        self.name = Some(NameOutputLevel::UpdatedAndUnchanged);
    }

    fn disable_all(&mut self) {
        self.progress = ProgressSetting::Disabled;
        self.stats = Some(false);
        self.name = Some(NameOutputLevel::Disabled);
    }

    fn apply(&mut self, token: &str, display: &str) -> Result<(), Message> {
        let lower = token.to_ascii_lowercase();
        match lower.as_str() {
            "help" => {
                self.help_requested = true;
                Ok(())
            }
            "all" | "1" => {
                self.enable_all();
                Ok(())
            }
            "none" | "0" => {
                self.disable_all();
                Ok(())
            }
            "progress" | "progress1" => {
                self.progress = ProgressSetting::PerFile;
                Ok(())
            }
            "progress2" => {
                self.progress = ProgressSetting::Overall;
                Ok(())
            }
            "progress0" | "noprogress" | "-progress" => {
                self.progress = ProgressSetting::Disabled;
                Ok(())
            }
            "stats" | "stats1" => {
                self.stats = Some(true);
                Ok(())
            }
            "stats0" | "nostats" | "-stats" => {
                self.stats = Some(false);
                Ok(())
            }
            _ if lower.starts_with("name") => {
                let level = &lower[4..];
                let parsed = if level.is_empty() || level == "1" {
                    Some(NameOutputLevel::UpdatedOnly)
                } else if level == "0" {
                    Some(NameOutputLevel::Disabled)
                } else if level.chars().all(|ch| ch.is_ascii_digit()) {
                    Some(NameOutputLevel::UpdatedAndUnchanged)
                } else {
                    None
                };

                match parsed {
                    Some(level) => {
                        self.name = Some(level);
                        Ok(())
                    }
                    None => Err(info_flag_error(display)),
                }
            }
            _ => Err(info_flag_error(display)),
        }
    }
}

fn info_flag_error(display: &str) -> Message {
    rsync_error!(
        1,
        format!(
            "invalid --info flag '{display}': supported flags are help, all, none, 0, 1, name, name0, name1, name2, progress, progress0, progress1, progress2, stats, stats0, and stats1"
        )
    )
    .with_role(Role::Client)
}

fn parse_info_flags(values: &[OsString]) -> Result<InfoFlagSettings, Message> {
    let mut settings = InfoFlagSettings::default();
    for value in values {
        let text = value.to_string_lossy();
        let trimmed = text.trim_matches(|ch: char| ch.is_ascii_whitespace());

        if trimmed.is_empty() {
            return Err(rsync_error!(1, "--info flag must not be empty").with_role(Role::Client));
        }

        for token in trimmed.split(',') {
            let token = token.trim_matches(|ch: char| ch.is_ascii_whitespace());
            if token.is_empty() {
                return Err(
                    rsync_error!(1, "--info flag must not be empty").with_role(Role::Client)
                );
            }

            settings.apply(token, token)?;
        }
    }

    Ok(settings)
}

struct DebugFlagSettings {
    flags: Vec<OsString>,
    help_requested: bool,
}

impl DebugFlagSettings {
    fn push_flag(&mut self, flag: &str) {
        self.flags.push(OsString::from(flag));
    }
}

fn parse_debug_flags(values: &[OsString]) -> Result<DebugFlagSettings, Message> {
    let mut settings = DebugFlagSettings {
        flags: Vec::new(),
        help_requested: false,
    };

    for value in values {
        let text = value.to_string_lossy();
        let trimmed = text.trim_matches(|ch: char| ch.is_ascii_whitespace());

        if trimmed.is_empty() {
            return Err(debug_flag_empty_error());
        }

        for token in trimmed.split(',') {
            let token = token.trim_matches(|ch: char| ch.is_ascii_whitespace());
            if token.is_empty() {
                return Err(debug_flag_empty_error());
            }

            if token.eq_ignore_ascii_case("help") {
                settings.help_requested = true;
            } else {
                settings.push_flag(token);
            }
        }
    }

    Ok(settings)
}

fn debug_flag_empty_error() -> Message {
    rsync_error!(1, "--debug flag must not be empty").with_role(Role::Client)
}

fn info_flags_include_progress(flags: &[OsString]) -> bool {
    flags.iter().any(|value| {
        value
            .to_string_lossy()
            .split(',')
            .map(|token| token.trim())
            .filter(|token| !token.is_empty())
            .any(|token| {
                let normalized = token.to_ascii_lowercase();
                let without_dash = normalized.strip_prefix('-').unwrap_or(&normalized);
                let stripped = without_dash
                    .strip_prefix("no-")
                    .or_else(|| without_dash.strip_prefix("no"))
                    .unwrap_or(without_dash);
                stripped.starts_with("progress")
            })
    })
}

const INFO_HELP_TEXT: &str = "The following --info flags are supported:\n\
    all         Enable all informational output currently implemented.\n\
    none        Disable all informational output handled by this build.\n\
    name        Mention updated file and directory names.\n\
    name2       Mention updated and unchanged file and directory names.\n\
    name0       Disable file and directory name output.\n\
    progress    Enable per-file progress updates.\n\
    progress2   Enable overall transfer progress.\n\
    progress0   Disable progress reporting.\n\
    stats       Enable transfer statistics.\n\
    stats0      Disable transfer statistics.\n\
Flags may also be written with 'no' prefixes (for example, --info=noprogress).\n";

const DEBUG_HELP_TEXT: &str = "The following --debug flags are supported:\n\
    all         Enable all diagnostic categories currently implemented.\n\
    none        Disable diagnostic output.\n\
    checksum    Trace checksum calculations and verification.\n\
    deltas      Trace delta-transfer generation and token handling.\n\
    events      Trace file-list discovery and generator events.\n\
    fs          Trace filesystem metadata operations.\n\
    io          Trace I/O buffering and transport exchanges.\n\
    socket      Trace socket setup, negotiation, and pacing decisions.\n\
Flags may be prefixed with 'no' or '-' to disable a category. Multiple flags\n\
may be combined by separating them with commas.\n";

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
    Merge(MergeDirective),
    Clear,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct MergeDirective {
    source: OsString,
    options: DirMergeOptions,
}

impl MergeDirective {
    fn new(source: OsString, enforced_kind: Option<FilterRuleKind>) -> Self {
        let mut options = DirMergeOptions::default();
        options = match enforced_kind {
            Some(FilterRuleKind::Include) => {
                options.with_enforced_kind(Some(DirMergeEnforcedKind::Include))
            }
            Some(FilterRuleKind::Exclude) => {
                options.with_enforced_kind(Some(DirMergeEnforcedKind::Exclude))
            }
            _ => options,
        };

        Self { source, options }
    }

    fn with_options(mut self, options: DirMergeOptions) -> Self {
        self.options = options;
        self
    }

    fn source(&self) -> &OsStr {
        self.source.as_os_str()
    }

    fn options(&self) -> &DirMergeOptions {
        &self.options
    }
}

fn merge_directive_options(base: &DirMergeOptions, directive: &MergeDirective) -> DirMergeOptions {
    let defaults = DirMergeOptions::default();
    let current = directive.options();

    let inherit = if current.inherit_rules() != defaults.inherit_rules() {
        current.inherit_rules()
    } else {
        base.inherit_rules()
    };

    let exclude_self = if current.excludes_self() != defaults.excludes_self() {
        current.excludes_self()
    } else {
        base.excludes_self()
    };

    let allow_list_clear = if current.list_clear_allowed() != defaults.list_clear_allowed() {
        current.list_clear_allowed()
    } else {
        base.list_clear_allowed()
    };

    let uses_whitespace = if current.uses_whitespace() != defaults.uses_whitespace() {
        current.uses_whitespace()
    } else {
        base.uses_whitespace()
    };

    let allows_comments = if current.allows_comments() != defaults.allows_comments() {
        current.allows_comments()
    } else {
        base.allows_comments()
    };

    let enforced_kind = if current.enforced_kind() != defaults.enforced_kind() {
        current.enforced_kind()
    } else {
        base.enforced_kind()
    };

    let sender_override = current
        .sender_side_override()
        .or_else(|| base.sender_side_override());
    let receiver_override = current
        .receiver_side_override()
        .or_else(|| base.receiver_side_override());

    let anchor_root = if current.anchor_root_enabled() != defaults.anchor_root_enabled() {
        current.anchor_root_enabled()
    } else {
        base.anchor_root_enabled()
    };

    let mut merged = DirMergeOptions::default()
        .inherit(inherit)
        .exclude_filter_file(exclude_self)
        .allow_list_clearing(allow_list_clear)
        .anchor_root(anchor_root)
        .with_side_overrides(sender_override, receiver_override)
        .with_enforced_kind(enforced_kind);

    if uses_whitespace {
        merged = merged.use_whitespace();
    }

    if !allows_comments {
        merged = merged.allow_comments(false);
    }

    merged
}

fn split_short_rule_modifiers(text: &str) -> (&str, &str) {
    if text.is_empty() {
        return ("", "");
    }

    if let Some(rest) = text.strip_prefix(',') {
        let mut parts = rest.splitn(2, |ch: char| ch.is_ascii_whitespace() || ch == '_');
        let modifiers = parts.next().unwrap_or("");
        let remainder = parts.next().unwrap_or("");
        let remainder =
            remainder.trim_start_matches(|ch: char| ch == '_' || ch.is_ascii_whitespace());
        return (modifiers, remainder);
    }

    let mut chars = text.chars();
    match chars.next() {
        None => ("", ""),
        Some(first) if first.is_ascii_whitespace() || first == '_' => {
            let remainder =
                text.trim_start_matches(|ch: char| ch == '_' || ch.is_ascii_whitespace());
            ("", remainder)
        }
        Some(_) => {
            let mut len = 0;
            for ch in text.chars() {
                if ch.is_ascii_whitespace() || ch == '_' {
                    break;
                }
                len += ch.len_utf8();
            }
            let modifiers = &text[..len];
            let remainder =
                text[len..].trim_start_matches(|ch: char| ch == '_' || ch.is_ascii_whitespace());
            (modifiers, remainder)
        }
    }
}

fn parse_short_merge_directive(text: &str) -> Option<Result<FilterDirective, Message>> {
    let mut chars = text.chars();
    let first = chars.next()?;
    let (allow_extended, label) = match first {
        '.' => (false, "merge"),
        ':' => (true, "dir-merge"),
        _ => return None,
    };

    let remainder = chars.as_str();
    let (modifiers, rest) = split_short_rule_modifiers(remainder);
    let (options, assume_cvsignore) = match parse_merge_modifiers(modifiers, text, allow_extended) {
        Ok(result) => result,
        Err(error) => return Some(Err(error)),
    };

    let pattern = rest.trim();
    let pattern = if pattern.is_empty() {
        if assume_cvsignore {
            ".cvsignore"
        } else if allow_extended {
            let message = rsync_error!(
                1,
                format!("filter rule '{text}' is missing a file name after '{label}'")
            )
            .with_role(Role::Client);
            return Some(Err(message));
        } else {
            let message = rsync_error!(
                1,
                format!("filter merge directive '{text}' is missing a file path")
            )
            .with_role(Role::Client);
            return Some(Err(message));
        }
    } else {
        pattern
    };

    if allow_extended {
        let rule = FilterRuleSpec::dir_merge(pattern.to_string(), options.clone());
        return Some(Ok(FilterDirective::Rule(rule)));
    }

    let enforced_kind = match options.enforced_kind() {
        Some(DirMergeEnforcedKind::Include) => Some(FilterRuleKind::Include),
        Some(DirMergeEnforcedKind::Exclude) => Some(FilterRuleKind::Exclude),
        None => None,
    };

    let directive =
        MergeDirective::new(OsString::from(pattern), enforced_kind).with_options(options);
    Some(Ok(FilterDirective::Merge(directive)))
}

fn parse_filter_shorthand(
    trimmed: &str,
    short: char,
    label: &str,
    builder: fn(String) -> FilterRuleSpec,
) -> Option<Result<FilterDirective, Message>> {
    let mut chars = trimmed.chars();
    let first = chars.next()?;
    if !first.eq_ignore_ascii_case(&short) {
        return None;
    }

    let remainder = chars.as_str();
    if remainder.is_empty() {
        let text = format!("filter rule '{trimmed}' is missing a pattern after '{label}'");
        let message = rsync_error!(1, text).with_role(Role::Client);
        return Some(Err(message));
    }

    if !remainder
        .chars()
        .next()
        .is_some_and(|ch| ch.is_ascii_whitespace() || ch == '_')
    {
        return None;
    }

    let pattern = remainder.trim_start_matches(|ch: char| ch == '_' || ch.is_ascii_whitespace());
    if pattern.is_empty() {
        let text = format!("filter rule '{trimmed}' is missing a pattern after '{label}'");
        let message = rsync_error!(1, text).with_role(Role::Client);
        return Some(Err(message));
    }

    Some(Ok(FilterDirective::Rule(builder(pattern.to_string()))))
}

fn parse_merge_modifiers(
    modifiers: &str,
    directive: &str,
    allow_extended: bool,
) -> Result<(DirMergeOptions, bool), Message> {
    let mut options = if allow_extended {
        DirMergeOptions::default()
    } else {
        DirMergeOptions::default().allow_list_clearing(true)
    };
    let mut enforced: Option<DirMergeEnforcedKind> = None;
    let mut saw_include = false;
    let mut saw_exclude = false;
    let mut assume_cvsignore = false;

    for modifier in modifiers.chars() {
        let lower = modifier.to_ascii_lowercase();
        match lower {
            '-' => {
                if saw_include {
                    let message = rsync_error!(
                        1,
                        format!("filter rule '{directive}' cannot combine '+' and '-' modifiers")
                    )
                    .with_role(Role::Client);
                    return Err(message);
                }
                saw_exclude = true;
                enforced = Some(DirMergeEnforcedKind::Exclude);
            }
            '+' => {
                if saw_exclude {
                    let message = rsync_error!(
                        1,
                        format!("filter rule '{directive}' cannot combine '+' and '-' modifiers")
                    )
                    .with_role(Role::Client);
                    return Err(message);
                }
                saw_include = true;
                enforced = Some(DirMergeEnforcedKind::Include);
            }
            'c' => {
                if saw_include {
                    let message = rsync_error!(
                        1,
                        format!(
                            "filter merge directive '{directive}' cannot combine 'C' with '+' or '-'"
                        )
                    )
                    .with_role(Role::Client);
                    return Err(message);
                }
                saw_exclude = true;
                enforced = Some(DirMergeEnforcedKind::Exclude);
                options = options
                    .use_whitespace()
                    .allow_comments(false)
                    .allow_list_clearing(true)
                    .inherit(false);
                assume_cvsignore = true;
            }
            'e' => {
                if allow_extended {
                    options = options.exclude_filter_file(true);
                } else {
                    let message = rsync_error!(
                        1,
                        format!(
                            "filter merge directive '{directive}' uses unsupported modifier '{}'",
                            modifier
                        )
                    )
                    .with_role(Role::Client);
                    return Err(message);
                }
            }
            'n' => {
                if allow_extended {
                    options = options.inherit(false);
                } else {
                    let message = rsync_error!(
                        1,
                        format!(
                            "filter merge directive '{directive}' uses unsupported modifier '{}'",
                            modifier
                        )
                    )
                    .with_role(Role::Client);
                    return Err(message);
                }
            }
            'w' => {
                options = options.use_whitespace().allow_comments(false);
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
            _ => {
                let message = rsync_error!(
                    1,
                    format!(
                        "filter merge directive '{directive}' uses unsupported modifier '{}'",
                        modifier
                    )
                )
                .with_role(Role::Client);
                return Err(message);
            }
        }
    }

    options = options.with_enforced_kind(enforced);
    if !allow_extended && !options.list_clear_allowed() {
        options = options.allow_list_clearing(true);
    }
    Ok((options, assume_cvsignore))
}

fn parse_filter_directive(argument: &OsStr) -> Result<FilterDirective, Message> {
    let text = argument.to_string_lossy();
    let trimmed_leading = text.trim_start();

    if let Some(result) = parse_short_merge_directive(trimmed_leading) {
        return result;
    }

    if let Some(rest) = trimmed_leading.strip_prefix("merge") {
        let mut remainder =
            rest.trim_start_matches(|ch: char| ch == '_' || ch.is_ascii_whitespace());
        let mut modifiers = "";
        if let Some(next) = remainder.strip_prefix(',') {
            let mut split = next.splitn(2, |ch: char| ch.is_ascii_whitespace() || ch == '_');
            modifiers = split.next().unwrap_or("");
            remainder = split
                .next()
                .unwrap_or("")
                .trim_start_matches(|ch: char| ch == '_' || ch.is_ascii_whitespace());
        }
        let (options, assume_cvsignore) = parse_merge_modifiers(modifiers, trimmed_leading, false)?;

        let mut path_text = remainder.trim_end();
        if path_text.is_empty() {
            if assume_cvsignore {
                path_text = ".cvsignore";
            } else {
                let message = rsync_error!(
                    1,
                    format!("filter merge directive '{trimmed_leading}' is missing a file path")
                )
                .with_role(Role::Client);
                return Err(message);
            }
        }

        let enforced_kind = match options.enforced_kind() {
            Some(DirMergeEnforcedKind::Include) => Some(FilterRuleKind::Include),
            Some(DirMergeEnforcedKind::Exclude) => Some(FilterRuleKind::Exclude),
            None => None,
        };

        let directive =
            MergeDirective::new(OsString::from(path_text), enforced_kind).with_options(options);
        return Ok(FilterDirective::Merge(directive));
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

    if let Some(remainder) = trimmed.strip_prefix('!') {
        if remainder.trim().is_empty() {
            return Ok(FilterDirective::Clear);
        }

        let message = rsync_error!(1, "'!' rule has trailing characters: {}", trimmed)
            .with_role(Role::Client);
        return Err(message);
    }

    if trimmed.eq_ignore_ascii_case("clear") {
        return Ok(FilterDirective::Clear);
    }

    const EXCLUDE_IF_PRESENT_PREFIX: &str = "exclude-if-present";

    if let Some(result) = parse_filter_shorthand(trimmed, 'P', "P", FilterRuleSpec::protect) {
        return result;
    }

    if let Some(result) = parse_filter_shorthand(trimmed, 'H', "H", FilterRuleSpec::hide) {
        return result;
    }

    if let Some(result) = parse_filter_shorthand(trimmed, 'S', "S", FilterRuleSpec::show) {
        return result;
    }

    if let Some(result) = parse_filter_shorthand(trimmed, 'R', "R", FilterRuleSpec::risk) {
        return result;
    }

    if trimmed.len() >= EXCLUDE_IF_PRESENT_PREFIX.len()
        && trimmed[..EXCLUDE_IF_PRESENT_PREFIX.len()]
            .eq_ignore_ascii_case(EXCLUDE_IF_PRESENT_PREFIX)
    {
        let mut remainder = trimmed[EXCLUDE_IF_PRESENT_PREFIX.len()..]
            .trim_start_matches(|ch: char| ch == '_' || ch.is_ascii_whitespace());
        if let Some(rest) = remainder.strip_prefix('=') {
            remainder = rest.trim_start_matches(|ch: char| ch == '_' || ch.is_ascii_whitespace());
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
        let pattern =
            remainder.trim_start_matches(|ch: char| ch == '_' || ch.is_ascii_whitespace());
        if pattern.is_empty() {
            let message = rsync_error!(
                1,
                "filter rule '{}' is missing a pattern after '+'",
                trimmed
            )
            .with_role(Role::Client);
            return Err(message);
        }
        return Ok(FilterDirective::Rule(FilterRuleSpec::include(
            pattern.to_string(),
        )));
    }

    if let Some(remainder) = trimmed.strip_prefix('-') {
        let pattern =
            remainder.trim_start_matches(|ch: char| ch == '_' || ch.is_ascii_whitespace());
        if pattern.is_empty() {
            let message = rsync_error!(
                1,
                "filter rule '{}' is missing a pattern after '-'",
                trimmed
            )
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
        let mut remainder = trimmed[DIR_MERGE_PREFIX.len()..]
            .trim_start_matches(|ch: char| ch == '_' || ch.is_ascii_whitespace());
        let mut modifiers = "";
        if let Some(rest) = remainder.strip_prefix(',') {
            let mut split = rest.splitn(2, |ch: char| ch.is_ascii_whitespace() || ch == '_');
            modifiers = split.next().unwrap_or("");
            remainder = split
                .next()
                .unwrap_or("")
                .trim_start_matches(|ch: char| ch == '_' || ch.is_ascii_whitespace());
        }

        let (options, assume_cvsignore) = parse_merge_modifiers(modifiers, trimmed, true)?;

        let mut path_text = remainder.trim_end();
        if path_text.is_empty() {
            if assume_cvsignore {
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
    let pattern = remainder.trim_start_matches(|ch: char| ch == '_' || ch.is_ascii_whitespace());

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

    if keyword.eq_ignore_ascii_case("risk") {
        return handle_keyword("risk", FilterRuleSpec::risk);
    }

    let message = rsync_error!(
        1,
        "unsupported filter rule '{}': this build currently supports only '+' (include), '-' (exclude), '!' (clear), 'include PATTERN', 'exclude PATTERN', 'show PATTERN', 'hide PATTERN', 'protect PATTERN', 'risk PATTERN', 'merge[,MODS] FILE' or '.[,MODS] FILE', and 'dir-merge[,MODS] FILE' or ':[,MODS] FILE' directives",
        trimmed
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
            FilterRuleKind::Clear => FilterRuleSpec::clear(),
            FilterRuleKind::ExcludeIfPresent => FilterRuleSpec::exclude_if_present(pattern),
            FilterRuleKind::Protect => FilterRuleSpec::protect(pattern),
            FilterRuleKind::Risk => FilterRuleSpec::risk(pattern),
            FilterRuleKind::DirMerge => unreachable!("dir-merge handled above"),
        }));
    }
    Ok(())
}

fn locate_filter_arguments(args: &[OsString]) -> (Vec<usize>, Vec<usize>) {
    let mut filter_indices = Vec::new();
    let mut rsync_filter_indices = Vec::new();
    let mut after_double_dash = false;
    let mut expect_filter_value = false;

    for (index, arg) in args.iter().enumerate().skip(1) {
        if after_double_dash {
            continue;
        }

        if expect_filter_value {
            expect_filter_value = false;
            continue;
        }

        if arg == "--" {
            after_double_dash = true;
            continue;
        }

        if arg == "--filter" {
            filter_indices.push(index);
            expect_filter_value = true;
            continue;
        }

        let value = arg.to_string_lossy();

        if value.starts_with("--filter=") {
            filter_indices.push(index);
            continue;
        }

        if value.starts_with('-') && !value.starts_with("--") && value.len() > 1 {
            for ch in value[1..].chars() {
                if ch == 'F' {
                    rsync_filter_indices.push(index);
                }
            }
        }
    }

    (filter_indices, rsync_filter_indices)
}

fn collect_filter_arguments(
    filters: &[OsString],
    filter_indices: &[usize],
    rsync_filter_indices: &[usize],
) -> Vec<OsString> {
    if rsync_filter_indices.is_empty() {
        return filters.to_vec();
    }

    let mut raw_queue: VecDeque<(usize, &OsString)> =
        filter_indices.iter().copied().zip(filters.iter()).collect();
    let mut alias_queue: VecDeque<(usize, usize)> = rsync_filter_indices
        .iter()
        .copied()
        .enumerate()
        .map(|(occurrence, position)| (position, occurrence))
        .collect();
    let mut merged = Vec::with_capacity(raw_queue.len() + alias_queue.len() * 2);

    while !raw_queue.is_empty() || !alias_queue.is_empty() {
        match (raw_queue.front(), alias_queue.front()) {
            (Some((raw_index, _)), Some((alias_index, _))) => {
                if alias_index <= raw_index {
                    let (_, occurrence) = alias_queue.pop_front().unwrap();
                    push_rsync_filter_shortcut(&mut merged, occurrence);
                } else {
                    let (_, value) = raw_queue.pop_front().unwrap();
                    merged.push(value.clone());
                }
            }
            (Some(_), None) => {
                let (_, value) = raw_queue.pop_front().unwrap();
                merged.push(value.clone());
            }
            (None, Some(_)) => {
                let (_, occurrence) = alias_queue.pop_front().unwrap();
                push_rsync_filter_shortcut(&mut merged, occurrence);
            }
            (None, None) => break,
        }
    }

    merged
}

fn push_rsync_filter_shortcut(target: &mut Vec<OsString>, occurrence: usize) {
    if occurrence == 0 {
        target.push(OsString::from("dir-merge /.rsync-filter"));
        target.push(OsString::from("exclude .rsync-filter"));
    } else {
        target.push(OsString::from("dir-merge .rsync-filter"));
    }
}

fn append_cvs_exclude_rules(destination: &mut Vec<FilterRuleSpec>) -> Result<(), Message> {
    let mut cvs_rules: Vec<FilterRuleSpec> = CVS_EXCLUDE_PATTERNS
        .iter()
        .map(|pattern| FilterRuleSpec::exclude((*pattern).to_string()))
        .collect();

    if let Some(home) = env::var_os("HOME").filter(|value| !value.is_empty()) {
        let path = Path::new(&home).join(".cvsignore");
        match fs::read(&path) {
            Ok(contents) => {
                let owned = String::from_utf8_lossy(&contents).into_owned();
                append_cvsignore_tokens(&mut cvs_rules, owned.split_whitespace());
            }
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => {
                let text = format!(
                    "failed to read '{}' for --cvs-exclude: {error}",
                    path.display()
                );
                return Err(rsync_error!(1, text).with_role(Role::Client));
            }
        }
    }

    if let Some(value) = env::var_os("CVSIGNORE").filter(|value| !value.is_empty()) {
        let owned = value.to_string_lossy().into_owned();
        append_cvsignore_tokens(&mut cvs_rules, owned.split_whitespace());
    }

    let options = DirMergeOptions::default()
        .with_enforced_kind(Some(DirMergeEnforcedKind::Exclude))
        .use_whitespace()
        .allow_comments(false)
        .inherit(false)
        .allow_list_clearing(true);
    cvs_rules.push(FilterRuleSpec::dir_merge(".cvsignore".to_string(), options));

    destination.extend(cvs_rules);
    Ok(())
}

fn append_cvsignore_tokens<'a, I>(destination: &mut Vec<FilterRuleSpec>, tokens: I)
where
    I: IntoIterator<Item = &'a str>,
{
    for token in tokens {
        let trimmed = token.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        if trimmed == "!" {
            destination.clear();
            continue;
        }

        if let Some(remainder) = trimmed.strip_prefix('!') {
            if remainder.is_empty() {
                continue;
            }
            remove_cvs_pattern(destination, remainder);
            continue;
        }

        destination.push(FilterRuleSpec::exclude(trimmed.to_string()));
    }
}

fn remove_cvs_pattern(rules: &mut Vec<FilterRuleSpec>, pattern: &str) {
    rules.retain(|rule| {
        !(matches!(rule.kind(), FilterRuleKind::Exclude) && rule.pattern() == pattern)
    });
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
    directive: MergeDirective,
    base_dir: &Path,
    destination: &mut Vec<FilterRuleSpec>,
    visited: &mut HashSet<PathBuf>,
) -> Result<(), Message> {
    let options = directive.options().clone();
    let original_source_text = os_string_to_pattern(directive.source().to_os_string());
    let is_stdin = directive.source() == OsStr::new("-");
    let (resolved_path, display, canonical_path) = if is_stdin {
        (PathBuf::from("-"), String::from("-"), None)
    } else {
        let raw_path = PathBuf::from(directive.source());
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

    if !visited.insert(guard_key.clone()) {
        let text = format!("recursive filter merge detected for '{display}'");
        return Err(rsync_error!(1, text).with_role(Role::Client));
    }

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
        let contents = if is_stdin {
            read_merge_from_standard_input()?
        } else {
            read_merge_file(&resolved_path)?
        };

        parse_merge_contents(
            &contents,
            &options,
            next_base,
            &display,
            destination,
            visited,
        )
    })();
    visited.remove(&guard_key);
    if result.is_ok() && options.excludes_self() && !is_stdin {
        let mut rule = FilterRuleSpec::exclude(original_source_text);
        rule.apply_dir_merge_overrides(&options);
        destination.push(rule);
    }
    result
}

fn read_merge_file(path: &Path) -> Result<String, Message> {
    fs::read_to_string(path).map_err(|error| {
        let text = format!("failed to read filter file '{}': {}", path.display(), error);
        rsync_error!(1, text).with_role(Role::Client)
    })
}

fn read_merge_from_standard_input() -> Result<String, Message> {
    #[cfg(test)]
    if let Some(data) = take_filter_stdin_input() {
        return String::from_utf8(data).map_err(|error| {
            let text = format!(
                "failed to read filter patterns from standard input: {}",
                error
            );
            rsync_error!(1, text).with_role(Role::Client)
        });
    }

    let mut buffer = String::new();
    io::stdin().read_to_string(&mut buffer).map_err(|error| {
        let text = format!(
            "failed to read filter patterns from standard input: {}",
            error
        );
        rsync_error!(1, text).with_role(Role::Client)
    })?;
    Ok(buffer)
}

fn parse_merge_contents(
    contents: &str,
    options: &DirMergeOptions,
    base_dir: &Path,
    display: &str,
    destination: &mut Vec<FilterRuleSpec>,
    visited: &mut HashSet<PathBuf>,
) -> Result<(), Message> {
    if options.uses_whitespace() {
        let mut tokens = contents.split_whitespace();
        while let Some(token) = tokens.next() {
            if token.is_empty() {
                continue;
            }

            if token == "!" {
                if options.list_clear_allowed() {
                    destination.clear();
                    continue;
                }
                let message = rsync_error!(
                    1,
                    format!("list-clearing '!' is not permitted in merge file '{display}'")
                )
                .with_role(Role::Client);
                return Err(message);
            }

            if let Some(kind) = options.enforced_kind() {
                let mut rule = match kind {
                    DirMergeEnforcedKind::Include => FilterRuleSpec::include(token.to_string()),
                    DirMergeEnforcedKind::Exclude => FilterRuleSpec::exclude(token.to_string()),
                };
                rule.apply_dir_merge_overrides(options);
                destination.push(rule);
                continue;
            }

            let lower = token.to_ascii_lowercase();
            let directive = if merge_directive_requires_argument(&lower) {
                let Some(arg) = tokens.next() else {
                    let message = rsync_error!(
                        1,
                        format!(
                            "filter merge directive '{}' in '{}' is missing a pattern",
                            token, display
                        )
                    )
                    .with_role(Role::Client);
                    return Err(message);
                };
                format!("{token} {arg}")
            } else {
                token.to_string()
            };

            process_merge_directive(&directive, options, base_dir, display, destination, visited)?;
        }
        return Ok(());
    }

    for line in contents.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if options.allows_comments() && trimmed.starts_with('#') {
            continue;
        }
        if trimmed.starts_with(';') && options.allows_comments() {
            continue;
        }

        if trimmed == "!" {
            if options.list_clear_allowed() {
                destination.clear();
                continue;
            }
            let message = rsync_error!(
                1,
                format!("list-clearing '!' is not permitted in merge file '{display}'")
            )
            .with_role(Role::Client);
            return Err(message);
        }

        if let Some(kind) = options.enforced_kind() {
            let mut rule = match kind {
                DirMergeEnforcedKind::Include => FilterRuleSpec::include(trimmed.to_string()),
                DirMergeEnforcedKind::Exclude => FilterRuleSpec::exclude(trimmed.to_string()),
            };
            rule.apply_dir_merge_overrides(options);
            destination.push(rule);
            continue;
        }

        process_merge_directive(trimmed, options, base_dir, display, destination, visited)?;
    }

    Ok(())
}

fn process_merge_directive(
    directive: &str,
    options: &DirMergeOptions,
    base_dir: &Path,
    display: &str,
    destination: &mut Vec<FilterRuleSpec>,
    visited: &mut HashSet<PathBuf>,
) -> Result<(), Message> {
    match parse_filter_directive(OsStr::new(directive)) {
        Ok(FilterDirective::Rule(mut rule)) => {
            rule.apply_dir_merge_overrides(options);
            destination.push(rule);
        }
        Ok(FilterDirective::Merge(nested)) => {
            let effective_options = merge_directive_options(options, &nested);
            let nested = nested.with_options(effective_options);
            apply_merge_directive(nested, base_dir, destination, visited).map_err(|error| {
                let detail = error.to_string();
                rsync_error!(
                    1,
                    format!("failed to process merge file '{display}': {detail}")
                )
                .with_role(Role::Client)
            })?;
        }
        Ok(FilterDirective::Clear) => destination.clear(),
        Err(error) => {
            let detail = error.to_string();
            let message = rsync_error!(
                1,
                format!(
                    "failed to parse filter rule '{}' from merge file '{}': {}",
                    directive, display, detail
                )
            )
            .with_role(Role::Client);
            return Err(message);
        }
    }

    Ok(())
}

fn merge_directive_requires_argument(keyword: &str) -> bool {
    matches!(
        keyword,
        "merge" | "include" | "exclude" | "show" | "hide" | "protect"
    ) || keyword.starts_with("dir-merge")
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

fn parse_bind_address_argument(value: &OsStr) -> Result<BindAddress, Message> {
    if value.is_empty() {
        return Err(rsync_error!(1, "--address requires a non-empty value").with_role(Role::Client));
    }

    let text = value.to_string_lossy();
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err(rsync_error!(1, "--address requires a non-empty value").with_role(Role::Client));
    }

    match resolve_bind_address(trimmed) {
        Ok(socket) => Ok(BindAddress::new(value.to_os_string(), socket)),
        Err(error) => {
            let formatted = format!("failed to resolve --address value '{}': {}", trimmed, error);
            Err(rsync_error!(1, formatted).with_role(Role::Client))
        }
    }
}

fn resolve_bind_address(text: &str) -> io::Result<SocketAddr> {
    if let Ok(ip) = text.parse::<IpAddr>() {
        return Ok(SocketAddr::new(ip, 0));
    }

    let candidate = if text.starts_with('[') {
        format!("{text}:0")
    } else if text.contains(':') {
        format!("[{text}]:0")
    } else {
        format!("{text}:0")
    };

    let mut resolved = candidate.to_socket_addrs()?;
    resolved
        .next()
        .map(|addr| SocketAddr::new(addr.ip(), 0))
        .ok_or_else(|| {
            io::Error::new(
                ErrorKind::AddrNotAvailable,
                "address resolution returned no results",
            )
        })
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

fn transfer_requires_remote(remainder: &[OsString], file_list_operands: &[OsString]) -> bool {
    remainder
        .iter()
        .chain(file_list_operands.iter())
        .any(|operand| operand_is_remote(operand.as_os_str()))
}

#[cfg(windows)]
fn operand_has_windows_prefix(path: &OsStr) -> bool {
    use std::os::windows::ffi::OsStrExt;

    const COLON: u16 = b':' as u16;
    const QUESTION: u16 = b'?' as u16;
    const DOT: u16 = b'.' as u16;
    const SLASH: u16 = b'/' as u16;
    const BACKSLASH: u16 = b'\\' as u16;

    fn is_ascii_alpha(unit: u16) -> bool {
        (unit >= b'a' as u16 && unit <= b'z' as u16) || (unit >= b'A' as u16 && unit <= b'Z' as u16)
    }

    fn is_separator(unit: u16) -> bool {
        unit == SLASH || unit == BACKSLASH
    }

    let units: Vec<u16> = path.encode_wide().collect();
    if units.is_empty() {
        return false;
    }

    if units.len() >= 4
        && is_separator(units[0])
        && is_separator(units[1])
        && (units[2] == QUESTION || units[2] == DOT)
        && is_separator(units[3])
    {
        return true;
    }

    if units.len() >= 2 && is_separator(units[0]) && is_separator(units[1]) {
        return true;
    }

    if units.len() >= 2 && is_ascii_alpha(units[0]) && units[1] == COLON {
        return true;
    }

    false
}

fn operand_is_remote(path: &OsStr) -> bool {
    let text = path.to_string_lossy();

    if text.starts_with("rsync://") {
        return true;
    }

    if text.contains("::") {
        return true;
    }

    if let Some(colon_index) = text.find(':') {
        #[cfg(windows)]
        if operand_has_windows_prefix(path) {
            return false;
        }

        let after = &text[colon_index + 1..];
        if after.starts_with(':') {
            return true;
        }

        #[cfg(windows)]
        {
            use std::path::{Component, Path};

            if Path::new(path)
                .components()
                .next()
                .is_some_and(|component| matches!(component, Component::Prefix(_)))
            {
                return false;
            }
        }

        let before = &text[..colon_index];
        if before.contains('/') || before.contains('\\') {
            return false;
        }

        if colon_index == 1 && before.chars().all(|ch| ch.is_ascii_alphabetic()) {
            return false;
        }

        return true;
    }

    false
}

#[cfg(all(test, windows))]
mod windows_operand_detection {
    use super::operand_is_remote;
    use std::ffi::OsStr;

    #[test]
    fn drive_letter_paths_are_local() {
        assert!(!operand_is_remote(OsStr::new(r"C:\\tmp\\file.txt")));
        assert!(!operand_is_remote(OsStr::new(r"c:relative\\path")));
    }

    #[test]
    fn extended_prefixes_are_local() {
        assert!(!operand_is_remote(OsStr::new(r"\\\\?\\C:\\tmp\\file.txt")));
        assert!(!operand_is_remote(OsStr::new(
            r"\\\\?\\UNC\\server\\share\\file.txt"
        )));
        assert!(!operand_is_remote(OsStr::new(r"\\\\.\\pipe\\rsync")));
    }

    #[test]
    fn unc_and_forward_slash_paths_are_local() {
        assert!(!operand_is_remote(OsStr::new(
            r"\\\\server\\share\\file.txt"
        )));
        assert!(!operand_is_remote(OsStr::new("//server/share/file.txt")));
    }

    #[test]
    fn remote_operands_remain_remote() {
        assert!(operand_is_remote(OsStr::new("host:path")));
        assert!(operand_is_remote(OsStr::new("user@host:path")));
        assert!(operand_is_remote(OsStr::new("host::module")));
        assert!(operand_is_remote(OsStr::new("rsync://example.com/module")));
    }
}

fn push_file_list_entry(bytes: &[u8], entries: &mut Vec<OsString>) {
    if bytes.is_empty() {
        return;
    }

    let mut end = bytes.len();
    while end > 0 && bytes[end - 1] == b'\r' {
        end -= 1;
    }

    if end > 0 {
        let trimmed = &bytes[..end];

        #[cfg(unix)]
        {
            if !trimmed.is_empty() {
                entries.push(OsString::from_vec(trimmed.to_vec()));
            }
        }

        #[cfg(not(unix))]
        {
            let text = String::from_utf8_lossy(trimmed).into_owned();
            if !text.is_empty() {
                entries.push(OsString::from(text));
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CompressLevelArg {
    Disable,
    Level(NonZeroU8),
}

fn parse_compress_level(argument: &OsStr) -> Result<CompressLevelArg, Message> {
    let text = argument.to_string_lossy();
    let trimmed = text.trim();

    if trimmed.is_empty() {
        return Err(rsync_error!(1, "--compress-level={} is invalid", text).with_role(Role::Client));
    }

    match trimmed.parse::<i32>() {
        Ok(0) => Ok(CompressLevelArg::Disable),
        Ok(value @ 1..=9) => Ok(CompressLevelArg::Level(
            NonZeroU8::new(value as u8).expect("range guarantees non-zero"),
        )),
        Ok(_) => Err(
            rsync_error!(1, "--compress-level={} must be between 0 and 9", trimmed)
                .with_role(Role::Client),
        ),
        Err(_) => {
            Err(rsync_error!(1, "--compress-level={} is invalid", text).with_role(Role::Client))
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

fn parse_compress_level_argument(value: &OsStr) -> Result<CompressionSetting, Message> {
    let text = value.to_string_lossy();
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err(
            rsync_error!(1, "compression level value must not be empty").with_role(Role::Client)
        );
    }

    if !trimmed.chars().all(|ch| ch.is_ascii_digit()) {
        return Err(
            rsync_error!(
                1,
                format!(
                    "invalid compression level '{trimmed}': compression level must be an integer between 0 and 9"
                )
            )
            .with_role(Role::Client),
        );
    }

    let level: u32 = trimmed.parse().map_err(|_| {
        rsync_error!(
            1,
            format!(
                "invalid compression level '{trimmed}': compression level must be an integer between 0 and 9"
            )
        )
        .with_role(Role::Client)
    })?;

    CompressionSetting::try_from_numeric(level).map_err(|error| {
        rsync_error!(
            1,
            format!(
                "invalid compression level '{trimmed}': compression level {} is outside the supported range 0-9",
                error.level()
            )
        )
        .with_role(Role::Client)
    })
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ParsedChown {
    spec: OsString,
    owner: Option<uid_t>,
    group: Option<gid_t>,
}

fn parse_chown_argument(value: &OsStr) -> Result<ParsedChown, Message> {
    let text = value.to_string_lossy();
    let trimmed = text.trim();

    if trimmed.is_empty() {
        return Err(
            rsync_error!(1, "--chown requires a non-empty USER and/or GROUP")
                .with_role(Role::Client),
        );
    }

    let (user_part, group_part) = match trimmed.split_once(':') {
        Some((user, group)) => (user, group),
        None => (trimmed, ""),
    };

    let owner = if user_part.is_empty() {
        None
    } else {
        Some(resolve_chown_user(user_part)?)
    };
    let group = if group_part.is_empty() {
        None
    } else {
        Some(resolve_chown_group(group_part)?)
    };

    if owner.is_none() && group.is_none() {
        return Err(rsync_error!(1, "--chown requires a user and/or group").with_role(Role::Client));
    }

    Ok(ParsedChown {
        spec: OsString::from(trimmed),
        owner,
        group,
    })
}

fn resolve_chown_user(input: &str) -> Result<uid_t, Message> {
    if let Ok(id) = input.parse::<uid_t>() {
        return Ok(id);
    }

    if let Some(uid) = crate::platform::lookup_user_by_name(input) {
        return Ok(uid);
    }

    if crate::platform::supports_user_name_lookup() {
        Err(
            rsync_error!(1, "unknown user '{}' specified for --chown", input)
                .with_role(Role::Client),
        )
    } else {
        Err(rsync_error!(
            1,
            "user name '{}' specified for --chown requires a numeric ID on this platform",
            input
        )
        .with_role(Role::Client))
    }
}

fn resolve_chown_group(input: &str) -> Result<gid_t, Message> {
    if let Ok(id) = input.parse::<gid_t>() {
        return Ok(id);
    }

    if let Some(gid) = crate::platform::lookup_group_by_name(input) {
        return Ok(gid);
    }

    if crate::platform::supports_group_name_lookup() {
        Err(
            rsync_error!(1, "unknown group '{}' specified for --chown", input)
                .with_role(Role::Client),
        )
    } else {
        Err(rsync_error!(
            1,
            "group name '{}' specified for --chown requires a numeric ID on this platform",
            input
        )
        .with_role(Role::Client))
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
