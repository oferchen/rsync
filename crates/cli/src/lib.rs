#![cfg_attr(docsrs, feature(doc_cfg))]
#![doc = include_str!("../README.md")]
#![deny(unsafe_code)]
#![deny(missing_docs)]
#![deny(rustdoc::broken_intra_doc_links)]

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
use std::fmt;
use std::fs::{self, File};
use std::io::ErrorKind;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{IpAddr, SocketAddr, ToSocketAddrs};
use std::num::{IntErrorKind, NonZeroU8, NonZeroU64};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::str::FromStr;
use std::sync::mpsc;
use std::thread;
use std::time::SystemTime;

#[cfg(unix)]
use std::os::unix::{ffi::OsStringExt, fs::PermissionsExt};

use clap::{Arg, ArgAction, Command as ClapCommand, builder::OsStringValueParser};
use rsync_checksums::strong::Md5;
use rsync_compress::zlib::CompressionLevel;
use rsync_core::{
    bandwidth::BandwidthParseError,
    branding::{self, Brand, manifest},
    client::{
        AddressMode, BandwidthLimit, BindAddress, ClientConfig, ClientEntryKind,
        ClientEntryMetadata, ClientEvent, ClientEventKind, ClientOutcome, ClientProgressObserver,
        CompressionSetting, DeleteMode, DirMergeEnforcedKind, DirMergeOptions, FilterRuleKind,
        FilterRuleSpec, HumanReadableMode, ModuleListOptions, ModuleListRequest,
        RemoteFallbackArgs, RemoteFallbackContext, StrongChecksumChoice, TransferTimeout,
        parse_skip_compress_list, run_client_or_fallback,
        run_module_list_with_password_and_options, skip_compress_from_env,
    },
    fallback::{CLIENT_FALLBACK_ENV, FallbackOverride, fallback_override},
    message::{Message, Role},
    rsync_error,
    version::VersionInfoReport,
};
use rsync_logging::MessageSink;
use rsync_meta::ChmodModifiers;
use rsync_protocol::{ParseProtocolVersionErrorKind, ProtocolVersion};
use time::{OffsetDateTime, format_description::FormatItem, macros::format_description};
use users::{get_group_by_gid, get_group_by_name, get_user_by_name, get_user_by_uid, gid_t, uid_t};

mod password;
mod progress;

pub(crate) use progress::*;

use password::{load_optional_password, load_password_file};

/// Maximum exit code representable by a Unix process.
const MAX_EXIT_CODE: i32 = u8::MAX as i32;

/// Renders deterministic help text describing the CLI surface supported by this build for `program_name`.
fn help_text(program_name: ProgramName) -> String {
    let manifest = manifest();
    let program = program_name.as_str();
    let daemon_profile = match program_name {
        ProgramName::Rsync => manifest.upstream(),
        ProgramName::OcRsync => manifest.oc(),
    };
    let daemon = daemon_profile.daemon_program_name();

    format!(
        concat!(
            "{program} {version}\n",
            "{website}\n",
            "\n",
            "Usage: {program} [-h] [-V] [--daemon] [-n] [-a] [-S] [-z] [-e COMMAND] [--delete] [--bwlimit=RATE[:BURST]] SOURCE... DEST\n",
            "\n",
            "This development snapshot implements deterministic local filesystem\n",
            "copies for regular files, directories, and symbolic links. The\n",
            "following options are recognised:\n",
            "      --help       Show this help message and exit.\n",
            "  -V, --version    Output version information and exit.\n",
            "  -e, --rsh=COMMAND  Use remote shell COMMAND for remote transfers.\n",
            "      --rsync-path=PROGRAM  Use PROGRAM as the remote rsync executable during remote transfers.\n",
            "      --connect-program=COMMAND  Execute COMMAND to reach rsync:// daemons (supports %H and %P placeholders).\n",
            "      --port=PORT  Connect to rsync:// daemons on TCP PORT when not specified by the source.\n",
            "  -M, --remote-option=OPTION  Forward OPTION to the remote rsync command.\n",
            "  -s, --protect-args  Protect remote shell arguments from expansion.\n",
            "      --no-protect-args  Allow the remote shell to expand wildcard arguments.\n",
            "      --secluded-args  Alias of --protect-args.\n",
            "      --no-secluded-args  Alias of --no-protect-args.\n",
            "      --ipv4          Prefer IPv4 when connecting to remote hosts.\n",
            "      --ipv6          Prefer IPv6 when connecting to remote hosts.\n",
            "      --daemon    Run as an {program} daemon (delegates to {daemon}).\n",
            "  -n, --dry-run    Validate transfers without modifying the destination.\n",
            "      --list-only  List files without performing a transfer.\n",
            "  -a, --archive    Enable archive mode (implies --owner, --group, --perms, --times, --devices, and --specials).\n",
            "      --delete, --del  Remove destination files that are absent from the source.\n",
            "      --delete-before  Remove destination files that are absent from the source before transfers start.\n",
            "      --delete-during  Remove destination files while processing directories.\n",
            "      --delete-delay  Defer deletions until after transfers while computing them during the run.\n",
            "      --delete-after  Remove destination files after transfers complete.\n",
            "      --delete-excluded  Remove excluded destination files during deletion sweeps.\n",
            "      --max-delete=NUM  Limit deletions to NUM entries per run.\n",
            "      --min-size=SIZE  Skip files smaller than SIZE.\n",
            "      --max-size=SIZE  Skip files larger than SIZE.\n",
            "  -b, --backup    Create backups before overwriting or deleting existing entries.\n",
            "      --backup-dir=DIR  Store backups inside DIR instead of alongside the destination.\n",
            "      --suffix=SUFFIX  Append SUFFIX to backup names (default '~').\n",
            "  -c, --checksum   Skip updates for files that already match by checksum.\n",
            "      --checksum-choice=ALGO  Select the strong checksum algorithm (auto, md4, md5, xxh64, xxh3, or xxh128).\n",
            "      --checksum-seed=NUM  Use NUM as the checksum seed for xxhash algorithms.\n",
            "      --size-only  Skip files whose size matches the destination, ignoring timestamps.\n",
            "      --ignore-existing  Skip updating files that already exist at the destination.\n",
            "      --ignore-missing-args  Skip missing source arguments without reporting an error.\n",
            "  -u, --update    Skip files that are newer on the destination.\n",
            "      --modify-window=SECS  Treat mtimes within SECS seconds as equal when comparing files.\n",
            "      --exclude=PATTERN  Skip files matching PATTERN.\n",
            "      --exclude-from=FILE  Read exclude patterns from FILE.\n",
            "      --include=PATTERN  Re-include files matching PATTERN after exclusions.\n",
            "      --include-from=FILE  Read include patterns from FILE.\n",
            "      --compare-dest=DIR  Skip creating files that already match DIR.\n",
            "      --copy-dest=DIR  Copy matching files from DIR instead of the source.\n",
            "      --link-dest=DIR  Hard-link matching files from DIR into DEST.\n",
            "  -H, --hard-links  Preserve hard links between files.\n",
            "      --no-hard-links  Disable hard link preservation.\n",
            "  -C, --cvs-exclude  Auto-ignore files using CVS-style ignore rules.\n",
            "      --filter=RULE  Apply filter RULE (supports '+' include, '-' exclude, '!' clear, 'include PATTERN', 'exclude PATTERN', 'show PATTERN'/'S PATTERN', 'hide PATTERN'/'H PATTERN', 'protect PATTERN'/'P PATTERN', 'risk PATTERN'/'R PATTERN', 'exclude-if-present=FILE', 'merge[,MODS] FILE' or '.[,MODS] FILE' with MODS drawn from '+', '-', 'C', 'e', 'n', 'w', 's', 'r', '/', and 'dir-merge[,MODS] FILE' or ':[,MODS] FILE' with MODS drawn from '+', '-', 'n', 'e', 'w', 's', 'r', '/', and 'C').\n",
            "  -F            Alias for per-directory .rsync-filter handling (repeat to also load receiver-side files).\n",
            "      --files-from=FILE  Read additional source operands from FILE.\n",
            "      --password-file=FILE  Read daemon passwords from FILE when contacting rsync:// daemons.\n",
            "      --no-motd    Suppress daemon MOTD lines when listing rsync:// modules.\n",
            "      --from0      Treat file list entries as NUL-terminated records.\n",
            "      --bwlimit=RATE[:BURST]  Limit I/O bandwidth (supports decimal, binary,\n",
            "                              and IEC units; optional :BURST caps the token\n",
            "                              bucket; 0 disables the limit).\n",
            "      --no-bwlimit    Remove any configured bandwidth limit.\n",
            "      --timeout=SECS  Abort when no progress is observed for SECS seconds (0 disables the timeout).\n",
            "      --contimeout=SECS  Abort connection attempts after SECS seconds (0 disables the limit).\n",
            "      --protocol=NUM  Force a specific protocol version (28 through 32).\n",
            "  -z, --compress  Compress file data during transfers.\n",
            "      --no-compress  Disable compression.\n",
            "      --compress-level=NUM  Override the compression level (0 disables compression).\n",
            "      --skip-compress=LIST  Skip compressing files with suffixes in LIST.\n",
            "      --info=FLAGS  Adjust informational messages; use --info=help for details.\n",
            "      --debug=FLAGS  Adjust diagnostic output; use --debug=help for details.\n",
            "  -v, --verbose    Increase verbosity; repeat for more detail.\n",
            "  -R, --relative   Preserve source path components relative to the current directory.\n",
            "      --no-relative  Disable preservation of source path components.\n",
            "  -x, --one-file-system  Don't cross filesystem boundaries during traversal.\n",
            "      --no-one-file-system  Allow traversal across filesystem boundaries.\n",
            "      --implied-dirs  Create parent directories implied by source paths.\n",
            "      --no-implied-dirs  Disable creation of parent directories implied by source paths.\n",
            "      --mkpath     Create destination's missing path components.\n",
            "  -m, --prune-empty-dirs  Skip creating directories that remain empty after filters.\n",
            "      --no-prune-empty-dirs  Disable pruning of empty directories.\n",
            "      --progress   Show progress information during transfers.\n",
            "      --no-progress  Disable progress reporting.\n",
            "      --msgs2stderr  Route informational messages to standard error.\n",
            "  -i, --itemize-changes  Output a change summary for each updated entry.\n",
            "      --out-format=FORMAT  Customise transfer output using FORMAT.\n",
            "      --stats      Output transfer statistics after completion.\n",
            "      --partial    Keep partially transferred files on errors.\n",
            "      --no-partial Discard partially transferred files on errors.\n",
            "      --partial-dir=DIR  Store partially transferred files in DIR.\n",
            "      --temp-dir=DIR  Store temporary files in DIR while transferring.\n",
            "      --delay-updates  Put completed updates in place after transfers finish.\n",
            "      --no-delay-updates  Disable delayed updates.\n",
            "      --link-dest=DIR  Create hard links to matching files in DIR when possible.\n",
            "  -W, --whole-file  Copy files without using the delta-transfer algorithm.\n",
            "      --no-whole-file  Enable the delta-transfer algorithm (disable whole-file copies).\n",
            "      --remove-source-files  Remove source files after a successful transfer.\n",
            "      --remove-sent-files   Alias of --remove-source-files.\n",
            "      --append    Append data to existing destination files without rewriting preserved bytes.\n",
            "      --no-append  Disable append mode for destination updates.\n",
            "      --append-verify  Append data while verifying that existing bytes match the sender.\n",
            "      --preallocate  Preallocate destination files before writing.\n",
            "      --inplace    Write updated data directly to destination files.\n",
            "      --no-inplace Use temporary files when updating regular files.\n",
            "  -h, --human-readable  Output numbers in a human-readable format.\n",
            "      --no-human-readable  Disable human-readable number formatting.\n",
            "  -P              Equivalent to --partial --progress.\n",
            "  -S, --sparse    Preserve sparse files by creating holes in the destination.\n",
            "      --no-sparse Disable sparse file handling.\n",
            "  -L, --copy-links     Transform symlinks into referent files/directories.\n",
            "      --no-copy-links  Preserve symlinks instead of following them.\n",
            "      --copy-unsafe-links  Transform unsafe symlinks into referent files/directories.\n",
            "      --no-copy-unsafe-links  Preserve unsafe symlinks instead of following them.\n",
            "      --safe-links     Skip symlinks that point outside the transfer root.\n",
            "  -k, --copy-dirlinks  Transform symlinked directories into referent directories.\n",
            "  -K, --keep-dirlinks  Treat destination symlinks to directories as directories.\n",
            "      --no-keep-dirlinks  Disable --keep-dirlinks semantics.\n",
            "  -D              Equivalent to --devices --specials.\n",
            "      --devices   Preserve device files.\n",
            "      --no-devices  Disable device file preservation.\n",
            "      --specials  Preserve special files such as FIFOs.\n",
            "      --no-specials  Disable preservation of special files.\n",
            "      --super      Receiver attempts super-user activities (implies --owner, --group, and --perms).\n",
            "      --no-super   Disable super-user handling even when running as root.\n",
            "      --owner      Preserve file ownership (requires super-user).\n",
            "      --no-owner   Disable ownership preservation.\n",
            "      --group      Preserve file group (requires suitable privileges).\n",
            "      --no-group   Disable group preservation.\n",
            "      --chown=USER:GROUP  Set destination ownership to USER and/or GROUP.\n",
            "      --chmod=SPEC  Apply chmod-style SPEC modifiers to received files.\n",
            "  -p, --perms      Preserve file permissions.\n",
            "      --no-perms   Disable permission preservation.\n",
            "  -t, --times      Preserve modification times.\n",
            "      --no-times   Disable modification time preservation.\n",
            "      --omit-dir-times  Skip preserving directory modification times.\n",
            "      --no-omit-dir-times  Preserve directory modification times.\n",
            "      --omit-link-times  Skip preserving symlink modification times.\n",
            "      --no-omit-link-times  Preserve symlink modification times.\n",
            "  -A, --acls      Preserve POSIX ACLs when supported.\n",
            "      --no-acls   Disable POSIX ACL preservation.\n",
            "  -X, --xattrs     Preserve extended attributes when supported.\n",
            "      --no-xattrs  Disable extended attribute preservation.\n",
            "      --numeric-ids      Preserve numeric UID/GID values.\n",
            "      --no-numeric-ids   Map UID/GID values to names when possible.\n",
            "\n",
            "All SOURCE operands must reside on the local filesystem. When multiple\n",
            "sources are supplied, DEST must name a directory. Metadata preservation\n",
            "covers permissions, timestamps, and optional ownership metadata.\n",
        ),
        program = program,
        version = manifest.rust_version(),
        website = manifest.source_url(),
        daemon = daemon,
    )
}

const SUPPORTED_OPTIONS_LIST: &str = "--help, --human-readable/-h, --no-human-readable, --version/-V, --daemon, --dry-run/-n, --list-only, --archive/-a, --delete/--del, --delete-before, --delete-during, --delete-delay, --delete-after, --max-delete, --min-size, --max-size, --checksum/-c, --checksum-choice, --checksum-seed, --size-only, --ignore-existing, --ignore-missing-args, --modify-window, --delay-updates, --exclude, --exclude-from, --include, --include-from, --compare-dest, --copy-dest, --link-dest, --filter (including exclude-if-present=FILE) and -F, --files-from, --password-file, --no-motd, --from0, --bwlimit, --no-bwlimit, --timeout, --contimeout, --protocol, --rsync-path, --port, --connect-program, --remote-option/-M, --ipv4, --ipv6, --compress/-z, --no-compress, --compress-level, --skip-compress, --info, --debug, --verbose/-v, --progress, --no-progress, --msgs2stderr, --itemize-changes/-i, --out-format, --stats, --partial, --partial-dir, --temp-dir, --no-partial, --remove-source-files, --remove-sent-files, --inplace, --no-inplace, --whole-file/-W, --no-whole-file, -P, --sparse/-S, --no-sparse, --copy-links/-L, --no-copy-links, --copy-unsafe-links, --no-copy-unsafe-links, --copy-dirlinks/-k, --keep-dirlinks/-K, --no-keep-dirlinks, -D, --devices, --no-devices, --specials, --no-specials, --super, --no-super, --owner, --no-owner, --group, --no-group, --chown, --chmod, --perms/-p, --no-perms, --times/-t, --no-times, --omit-dir-times, --no-omit-dir-times, --omit-link-times, --no-omit-link-times, --acls/-A, --no-acls, --xattrs/-X, --no-xattrs, --numeric-ids, --one-file-system/-x, --no-one-file-system, --mkpath, and --no-numeric-ids";

const ITEMIZE_CHANGES_FORMAT: &str = "%i %n%L";
/// Default patterns excluded by `--cvs-exclude`.
const CVS_EXCLUDE_PATTERNS: &[&str] = &[
    "RCS",
    "SCCS",
    "CVS",
    "CVS.adm",
    "RCSLOG",
    "cvslog.*",
    "tags",
    "TAGS",
    ".make.state",
    ".nse_depinfo",
    "*~",
    "#*",
    ".#*",
    ",*",
    "_$*",
    "*$",
    "*.old",
    "*.bak",
    "*.BAK",
    "*.orig",
    "*.rej",
    ".del-*",
    "*.a",
    "*.olb",
    "*.o",
    "*.obj",
    "*.so",
    "*.exe",
    "*.Z",
    "*.elc",
    "*.ln",
    "core",
    ".svn/",
    ".git/",
    ".hg/",
    ".bzr/",
];

/// Timestamp format used for `--list-only` output.
const LIST_TIMESTAMP_FORMAT: &[FormatItem<'static>] = format_description!(
    "[year]/[month padding:zero]/[day padding:zero] [hour padding:zero]:[minute padding:zero]:[second padding:zero]"
);

#[derive(Clone, Debug)]
struct OutFormat {
    tokens: Vec<OutFormatToken>,
}

#[derive(Clone, Debug)]
enum OutFormatToken {
    Literal(String),
    Placeholder(OutFormatPlaceholder),
}

#[derive(Clone, Copy, Debug)]
enum OutFormatPlaceholder {
    FileName,
    FileNameWithSymlinkTarget,
    FullPath,
    ItemizedChanges,
    FileLength,
    BytesTransferred,
    ChecksumBytes,
    Operation,
    ModifyTime,
    PermissionString,
    CurrentTime,
    SymlinkTarget,
    OwnerName,
    GroupName,
    OwnerUid,
    OwnerGid,
    ProcessId,
    RemoteHost,
    RemoteAddress,
    ModuleName,
    ModulePath,
    FullChecksum,
}

#[derive(Clone, Debug, Default)]
struct OutFormatContext {
    remote_host: Option<String>,
    remote_address: Option<String>,
    module_name: Option<String>,
    module_path: Option<String>,
}

fn parse_out_format(value: &OsStr) -> Result<OutFormat, Message> {
    let text = value.to_string_lossy();
    if text.is_empty() {
        return Err(rsync_error!(1, "--out-format value must not be empty").with_role(Role::Client));
    }

    let mut tokens = Vec::new();
    let mut literal = String::new();
    let mut chars = text.chars();
    while let Some(ch) = chars.next() {
        match ch {
            '%' => {
                let Some(next) = chars.next() else {
                    return Err(rsync_error!(1, "--out-format value may not end with '%'")
                        .with_role(Role::Client));
                };
                match next {
                    '%' => literal.push('%'),
                    'n' => {
                        if !literal.is_empty() {
                            tokens.push(OutFormatToken::Literal(std::mem::take(&mut literal)));
                        }
                        tokens.push(OutFormatToken::Placeholder(OutFormatPlaceholder::FileName));
                    }
                    'N' => {
                        if !literal.is_empty() {
                            tokens.push(OutFormatToken::Literal(std::mem::take(&mut literal)));
                        }
                        tokens.push(OutFormatToken::Placeholder(
                            OutFormatPlaceholder::FileNameWithSymlinkTarget,
                        ));
                    }
                    'f' => {
                        if !literal.is_empty() {
                            tokens.push(OutFormatToken::Literal(std::mem::take(&mut literal)));
                        }
                        tokens.push(OutFormatToken::Placeholder(OutFormatPlaceholder::FullPath));
                    }
                    'i' => {
                        if !literal.is_empty() {
                            tokens.push(OutFormatToken::Literal(std::mem::take(&mut literal)));
                        }
                        tokens.push(OutFormatToken::Placeholder(
                            OutFormatPlaceholder::ItemizedChanges,
                        ));
                    }
                    'l' => {
                        if !literal.is_empty() {
                            tokens.push(OutFormatToken::Literal(std::mem::take(&mut literal)));
                        }
                        tokens.push(OutFormatToken::Placeholder(
                            OutFormatPlaceholder::FileLength,
                        ));
                    }
                    'b' => {
                        if !literal.is_empty() {
                            tokens.push(OutFormatToken::Literal(std::mem::take(&mut literal)));
                        }
                        tokens.push(OutFormatToken::Placeholder(
                            OutFormatPlaceholder::BytesTransferred,
                        ));
                    }
                    'c' => {
                        if !literal.is_empty() {
                            tokens.push(OutFormatToken::Literal(std::mem::take(&mut literal)));
                        }
                        tokens.push(OutFormatToken::Placeholder(
                            OutFormatPlaceholder::ChecksumBytes,
                        ));
                    }
                    'o' => {
                        if !literal.is_empty() {
                            tokens.push(OutFormatToken::Literal(std::mem::take(&mut literal)));
                        }
                        tokens.push(OutFormatToken::Placeholder(OutFormatPlaceholder::Operation));
                    }
                    'M' => {
                        if !literal.is_empty() {
                            tokens.push(OutFormatToken::Literal(std::mem::take(&mut literal)));
                        }
                        tokens.push(OutFormatToken::Placeholder(
                            OutFormatPlaceholder::ModifyTime,
                        ));
                    }
                    'B' => {
                        if !literal.is_empty() {
                            tokens.push(OutFormatToken::Literal(std::mem::take(&mut literal)));
                        }
                        tokens.push(OutFormatToken::Placeholder(
                            OutFormatPlaceholder::PermissionString,
                        ));
                    }
                    'L' => {
                        if !literal.is_empty() {
                            tokens.push(OutFormatToken::Literal(std::mem::take(&mut literal)));
                        }
                        tokens.push(OutFormatToken::Placeholder(
                            OutFormatPlaceholder::SymlinkTarget,
                        ));
                    }
                    't' => {
                        if !literal.is_empty() {
                            tokens.push(OutFormatToken::Literal(std::mem::take(&mut literal)));
                        }
                        tokens.push(OutFormatToken::Placeholder(
                            OutFormatPlaceholder::CurrentTime,
                        ));
                    }
                    'u' => {
                        if !literal.is_empty() {
                            tokens.push(OutFormatToken::Literal(std::mem::take(&mut literal)));
                        }
                        tokens.push(OutFormatToken::Placeholder(OutFormatPlaceholder::OwnerName));
                    }
                    'g' => {
                        if !literal.is_empty() {
                            tokens.push(OutFormatToken::Literal(std::mem::take(&mut literal)));
                        }
                        tokens.push(OutFormatToken::Placeholder(OutFormatPlaceholder::GroupName));
                    }
                    'U' => {
                        if !literal.is_empty() {
                            tokens.push(OutFormatToken::Literal(std::mem::take(&mut literal)));
                        }
                        tokens.push(OutFormatToken::Placeholder(OutFormatPlaceholder::OwnerUid));
                    }
                    'G' => {
                        if !literal.is_empty() {
                            tokens.push(OutFormatToken::Literal(std::mem::take(&mut literal)));
                        }
                        tokens.push(OutFormatToken::Placeholder(OutFormatPlaceholder::OwnerGid));
                    }
                    'p' => {
                        if !literal.is_empty() {
                            tokens.push(OutFormatToken::Literal(std::mem::take(&mut literal)));
                        }
                        tokens.push(OutFormatToken::Placeholder(OutFormatPlaceholder::ProcessId));
                    }
                    'h' => {
                        if !literal.is_empty() {
                            tokens.push(OutFormatToken::Literal(std::mem::take(&mut literal)));
                        }
                        tokens.push(OutFormatToken::Placeholder(
                            OutFormatPlaceholder::RemoteHost,
                        ));
                    }
                    'a' => {
                        if !literal.is_empty() {
                            tokens.push(OutFormatToken::Literal(std::mem::take(&mut literal)));
                        }
                        tokens.push(OutFormatToken::Placeholder(
                            OutFormatPlaceholder::RemoteAddress,
                        ));
                    }
                    'm' => {
                        if !literal.is_empty() {
                            tokens.push(OutFormatToken::Literal(std::mem::take(&mut literal)));
                        }
                        tokens.push(OutFormatToken::Placeholder(
                            OutFormatPlaceholder::ModuleName,
                        ));
                    }
                    'P' => {
                        if !literal.is_empty() {
                            tokens.push(OutFormatToken::Literal(std::mem::take(&mut literal)));
                        }
                        tokens.push(OutFormatToken::Placeholder(
                            OutFormatPlaceholder::ModulePath,
                        ));
                    }
                    'C' => {
                        if !literal.is_empty() {
                            tokens.push(OutFormatToken::Literal(std::mem::take(&mut literal)));
                        }
                        tokens.push(OutFormatToken::Placeholder(
                            OutFormatPlaceholder::FullChecksum,
                        ));
                    }
                    other => {
                        return Err(rsync_error!(
                            1,
                            format!("unsupported --out-format placeholder '%{other}'"),
                        )
                        .with_role(Role::Client));
                    }
                }
            }
            '\\' => {
                let Some(next) = chars.next() else {
                    literal.push('\\');
                    break;
                };
                match next {
                    'n' => literal.push('\n'),
                    'r' => literal.push('\r'),
                    't' => literal.push('\t'),
                    '\\' => literal.push('\\'),
                    other => {
                        literal.push('\\');
                        literal.push(other);
                    }
                }
            }
            other => literal.push(other),
        }
    }

    if !literal.is_empty() {
        tokens.push(OutFormatToken::Literal(literal));
    }

    Ok(OutFormat { tokens })
}

impl OutFormat {
    fn render<W: Write + ?Sized>(
        &self,
        event: &ClientEvent,
        context: &OutFormatContext,
        writer: &mut W,
    ) -> io::Result<()> {
        use std::fmt::Write as _;
        let mut buffer = String::new();
        for token in &self.tokens {
            match token {
                OutFormatToken::Literal(text) => buffer.push_str(text),
                OutFormatToken::Placeholder(placeholder) => match placeholder {
                    OutFormatPlaceholder::FileName
                    | OutFormatPlaceholder::FileNameWithSymlinkTarget
                    | OutFormatPlaceholder::FullPath => {
                        append_rendered_path(
                            &mut buffer,
                            event,
                            matches!(
                                placeholder,
                                OutFormatPlaceholder::FileName
                                    | OutFormatPlaceholder::FileNameWithSymlinkTarget
                            ),
                        );
                        if matches!(placeholder, OutFormatPlaceholder::FileNameWithSymlinkTarget) {
                            if let Some(metadata) = event.metadata() {
                                if let Some(target) = metadata.symlink_target() {
                                    buffer.push_str(" -> ");
                                    buffer.push_str(&target.to_string_lossy());
                                }
                            }
                        }
                    }
                    OutFormatPlaceholder::ItemizedChanges => {
                        buffer.push_str(&format_itemized_changes(event));
                    }
                    OutFormatPlaceholder::FileLength => {
                        let length = event
                            .metadata()
                            .map(ClientEntryMetadata::length)
                            .unwrap_or(0);
                        let _ = write!(&mut buffer, "{length}");
                    }
                    OutFormatPlaceholder::BytesTransferred => {
                        let bytes = event.bytes_transferred();
                        let _ = write!(&mut buffer, "{bytes}");
                    }
                    OutFormatPlaceholder::ChecksumBytes => {
                        let checksum_bytes = match event.kind() {
                            ClientEventKind::DataCopied => event.bytes_transferred(),
                            _ => 0,
                        };
                        let _ = write!(&mut buffer, "{checksum_bytes}");
                    }
                    OutFormatPlaceholder::Operation => {
                        buffer.push_str(describe_event_kind(event.kind()));
                    }
                    OutFormatPlaceholder::ModifyTime => {
                        buffer.push_str(&format_out_format_mtime(event.metadata()));
                    }
                    OutFormatPlaceholder::PermissionString => {
                        buffer.push_str(&format_out_format_permissions(event.metadata()));
                    }
                    OutFormatPlaceholder::SymlinkTarget => {
                        if let Some(target) = event
                            .metadata()
                            .and_then(ClientEntryMetadata::symlink_target)
                        {
                            buffer.push_str(" -> ");
                            buffer.push_str(&target.to_string_lossy());
                        }
                    }
                    OutFormatPlaceholder::CurrentTime => {
                        buffer.push_str(&format_current_timestamp());
                    }
                    OutFormatPlaceholder::OwnerName => {
                        buffer.push_str(&format_owner_name(event.metadata()));
                    }
                    OutFormatPlaceholder::GroupName => {
                        buffer.push_str(&format_group_name(event.metadata()));
                    }
                    OutFormatPlaceholder::OwnerUid => {
                        let uid = event
                            .metadata()
                            .and_then(ClientEntryMetadata::uid)
                            .map(|value| value.to_string())
                            .unwrap_or_else(|| "0".to_string());
                        buffer.push_str(&uid);
                    }
                    OutFormatPlaceholder::OwnerGid => {
                        let gid = event
                            .metadata()
                            .and_then(ClientEntryMetadata::gid)
                            .map(|value| value.to_string())
                            .unwrap_or_else(|| "0".to_string());
                        buffer.push_str(&gid);
                    }
                    OutFormatPlaceholder::ProcessId => {
                        let pid = std::process::id();
                        let _ = write!(&mut buffer, "{pid}");
                    }
                    OutFormatPlaceholder::RemoteHost => {
                        append_remote_placeholder(&mut buffer, context.remote_host.as_deref(), 'h');
                    }
                    OutFormatPlaceholder::RemoteAddress => {
                        append_remote_placeholder(
                            &mut buffer,
                            context.remote_address.as_deref(),
                            'a',
                        );
                    }
                    OutFormatPlaceholder::ModuleName => {
                        append_remote_placeholder(&mut buffer, context.module_name.as_deref(), 'm');
                    }
                    OutFormatPlaceholder::ModulePath => {
                        append_remote_placeholder(&mut buffer, context.module_path.as_deref(), 'P');
                    }
                    OutFormatPlaceholder::FullChecksum => {
                        buffer.push_str(&format_full_checksum(event));
                    }
                },
            }
        }

        if buffer.ends_with('\n') {
            writer.write_all(buffer.as_bytes())
        } else {
            writer.write_all(buffer.as_bytes())?;
            writer.write_all(b"\n")
        }
    }
}

fn append_remote_placeholder(buffer: &mut String, value: Option<&str>, token: char) {
    if let Some(text) = value {
        buffer.push_str(text);
    } else {
        buffer.push('%');
        buffer.push(token);
    }
}

fn emit_out_format<W: Write + ?Sized>(
    events: &[ClientEvent],
    format: &OutFormat,
    context: &OutFormatContext,
    writer: &mut W,
) -> io::Result<()> {
    for event in events {
        format.render(event, context, writer)?;
    }
    Ok(())
}

fn append_rendered_path(buffer: &mut String, event: &ClientEvent, ensure_trailing_slash: bool) {
    let mut rendered = event.relative_path().to_string_lossy().into_owned();
    if ensure_trailing_slash
        && !rendered.ends_with('/')
        && event
            .metadata()
            .map(ClientEntryMetadata::kind)
            .map(ClientEntryKind::is_directory)
            .unwrap_or_else(|| matches!(event.kind(), ClientEventKind::DirectoryCreated))
    {
        rendered.push('/');
    }
    buffer.push_str(&rendered);
}

fn format_out_format_mtime(metadata: Option<&ClientEntryMetadata>) -> String {
    metadata
        .and_then(|meta| meta.modified())
        .and_then(|time| {
            OffsetDateTime::from(time)
                .format(LIST_TIMESTAMP_FORMAT)
                .ok()
        })
        .map(|formatted| formatted.replace(' ', "-"))
        .unwrap_or_else(|| "1970/01/01-00:00:00".to_string())
}

fn format_out_format_permissions(metadata: Option<&ClientEntryMetadata>) -> String {
    metadata
        .map(format_list_permissions)
        .map(|mut perms| {
            if !perms.is_empty() {
                perms.remove(0);
            }
            perms
        })
        .unwrap_or_else(|| "---------".to_string())
}

fn format_owner_name(metadata: Option<&ClientEntryMetadata>) -> String {
    metadata
        .and_then(ClientEntryMetadata::uid)
        .map(resolve_user_name)
        .unwrap_or_else(|| "0".to_string())
}

fn format_group_name(metadata: Option<&ClientEntryMetadata>) -> String {
    metadata
        .and_then(ClientEntryMetadata::gid)
        .map(resolve_group_name)
        .unwrap_or_else(|| "0".to_string())
}

fn resolve_user_name(uid: u32) -> String {
    get_user_by_uid(uid as uid_t)
        .map(|user| user.name().to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| uid.to_string())
}

fn resolve_group_name(gid: u32) -> String {
    get_group_by_gid(gid as gid_t)
        .map(|group| group.name().to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| gid.to_string())
}

fn format_current_timestamp() -> String {
    let now = OffsetDateTime::from(SystemTime::now());
    now.format(LIST_TIMESTAMP_FORMAT)
        .map(|text| text.replace(' ', "-"))
        .unwrap_or_else(|_| "1970/01/01-00:00:00".to_string())
}

fn format_itemized_changes(event: &ClientEvent) -> String {
    use ClientEventKind::*;

    if matches!(event.kind(), ClientEventKind::EntryDeleted) {
        return "*deleting".to_string();
    }

    let mut fields = ['.'; 11];

    fields[0] = match event.kind() {
        DataCopied => '>',
        MetadataReused
        | SkippedExisting
        | SkippedNewerDestination
        | SkippedNonRegular
        | SkippedUnsafeSymlink
        | SkippedMountPoint => '.',
        HardLink => 'h',
        DirectoryCreated | SymlinkCopied | FifoCopied | DeviceCopied | SourceRemoved => 'c',
        _ => '.',
    };

    fields[1] = match event
        .metadata()
        .map(ClientEntryMetadata::kind)
        .unwrap_or_else(|| match event.kind() {
            DirectoryCreated => ClientEntryKind::Directory,
            SymlinkCopied => ClientEntryKind::Symlink,
            FifoCopied => ClientEntryKind::Fifo,
            DeviceCopied => ClientEntryKind::CharDevice,
            HardLink | DataCopied | MetadataReused | SkippedExisting | SkippedNewerDestination => {
                ClientEntryKind::File
            }
            _ => ClientEntryKind::Other,
        }) {
        ClientEntryKind::File => 'f',
        ClientEntryKind::Directory => 'd',
        ClientEntryKind::Symlink => 'L',
        ClientEntryKind::Fifo | ClientEntryKind::Socket | ClientEntryKind::Other => 'S',
        ClientEntryKind::CharDevice | ClientEntryKind::BlockDevice => 'D',
    };

    if event.was_created() {
        for slot in fields.iter_mut().skip(2) {
            *slot = '+';
        }
        return fields.iter().collect();
    }

    let attr = &mut fields[2..];

    match event.kind() {
        DirectoryCreated | SymlinkCopied | FifoCopied | DeviceCopied | HardLink => {
            attr.fill('+');
        }
        DataCopied => {
            attr[0] = 'c';
            attr[1] = 's';
            attr[2] = 't';
        }
        SourceRemoved => {
            attr[0] = 'c';
        }
        _ => {}
    }

    fields.iter().collect()
}

fn format_full_checksum(event: &ClientEvent) -> String {
    const EMPTY_CHECKSUM: &str = "                                ";

    if !matches!(
        event.kind(),
        ClientEventKind::DataCopied | ClientEventKind::MetadataReused | ClientEventKind::HardLink
    ) {
        return EMPTY_CHECKSUM.to_string();
    }

    if let Some(metadata) = event.metadata() {
        if metadata.kind() != ClientEntryKind::File {
            return EMPTY_CHECKSUM.to_string();
        }
    }

    let path = event.destination_path();
    let mut file = match File::open(&path) {
        Ok(file) => file,
        Err(_) => return EMPTY_CHECKSUM.to_string(),
    };

    let mut hasher = Md5::new();
    let mut buffer = [0u8; 32 * 1024];
    loop {
        match file.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => hasher.update(&buffer[..read]),
            Err(error) if error.kind() == ErrorKind::Interrupted => continue,
            Err(_) => return EMPTY_CHECKSUM.to_string(),
        }
    }

    let digest = hasher.finalize();
    let mut rendered = String::with_capacity(32);
    for byte in digest {
        rendered.push_str(&format!("{byte:02x}"));
    }
    rendered
}

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

    if server_mode_requested(&args) {
        return run_server_mode(&args, stdout, stderr);
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

    let program_name = detect_program_name(args.first().map(OsString::as_os_str));
    let daemon_program = match program_name {
        ProgramName::Rsync => Brand::Upstream.daemon_program_name(),
        ProgramName::OcRsync => Brand::Oc.daemon_program_name(),
    };

    let mut daemon_args = Vec::with_capacity(args.len());
    daemon_args.push(OsString::from(daemon_program));

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

/// Returns `true` when the invocation requests server mode.
fn server_mode_requested(args: &[OsString]) -> bool {
    args.iter().skip(1).any(|arg| arg == "--server")
}

/// Delegates execution to the system rsync binary when `--server` is requested.
fn run_server_mode<Out, Err>(args: &[OsString], stdout: &mut Out, stderr: &mut Err) -> i32
where
    Out: Write,
    Err: Write,
{
    let _ = stdout.flush();
    let _ = stderr.flush();

    let upstream_program = Brand::Upstream.client_program_name();
    let upstream_program_os = OsStr::new(upstream_program);
    let fallback = match fallback_override(CLIENT_FALLBACK_ENV) {
        Some(FallbackOverride::Disabled) => {
            let text = format!(
                "remote server mode is unavailable because OC_RSYNC_FALLBACK is disabled; set OC_RSYNC_FALLBACK to point to an upstream {upstream_program} binary"
            );
            write_server_fallback_error(stderr, text);
            return 1;
        }
        Some(other) => other
            .resolve_or_default(upstream_program_os)
            .unwrap_or_else(|| OsString::from(upstream_program)),
        None => OsString::from(upstream_program),
    };

    let mut command = Command::new(&fallback);
    command.args(args.iter().skip(1));
    command.stdin(Stdio::inherit());
    command.stdout(Stdio::piped());
    command.stderr(Stdio::piped());

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            let text = format!(
                "failed to launch fallback {upstream_program} binary '{}': {error}",
                Path::new(&fallback).display()
            );
            write_server_fallback_error(stderr, text);
            return 1;
        }
    };

    let (sender, receiver) = mpsc::channel();
    let mut stdout_thread = child
        .stdout
        .take()
        .map(|handle| spawn_server_reader(handle, ServerStreamKind::Stdout, sender.clone()));
    let mut stderr_thread = child
        .stderr
        .take()
        .map(|handle| spawn_server_reader(handle, ServerStreamKind::Stderr, sender.clone()));
    drop(sender);

    let mut stdout_open = stdout_thread.is_some();
    let mut stderr_open = stderr_thread.is_some();

    while stdout_open || stderr_open {
        match receiver.recv() {
            Ok(ServerStreamMessage::Data(ServerStreamKind::Stdout, data)) => {
                if let Err(error) = stdout.write_all(&data) {
                    terminate_server_process(&mut child, &mut stdout_thread, &mut stderr_thread);
                    write_server_fallback_error(
                        stderr,
                        format!("failed to forward fallback stdout: {error}"),
                    );
                    return 1;
                }
            }
            Ok(ServerStreamMessage::Data(ServerStreamKind::Stderr, data)) => {
                if let Err(error) = stderr.write_all(&data) {
                    terminate_server_process(&mut child, &mut stdout_thread, &mut stderr_thread);
                    write_server_fallback_error(
                        stderr,
                        format!("failed to forward fallback stderr: {error}"),
                    );
                    return 1;
                }
            }
            Ok(ServerStreamMessage::Error(ServerStreamKind::Stdout, error)) => {
                terminate_server_process(&mut child, &mut stdout_thread, &mut stderr_thread);
                write_server_fallback_error(
                    stderr,
                    format!("failed to read stdout from fallback {upstream_program}: {error}"),
                );
                return 1;
            }
            Ok(ServerStreamMessage::Error(ServerStreamKind::Stderr, error)) => {
                terminate_server_process(&mut child, &mut stdout_thread, &mut stderr_thread);
                write_server_fallback_error(
                    stderr,
                    format!("failed to read stderr from fallback {upstream_program}: {error}"),
                );
                return 1;
            }
            Ok(ServerStreamMessage::Finished(kind)) => match kind {
                ServerStreamKind::Stdout => stdout_open = false,
                ServerStreamKind::Stderr => stderr_open = false,
            },
            Err(_) => break,
        }
    }

    join_server_thread(&mut stdout_thread);
    join_server_thread(&mut stderr_thread);

    match child.wait() {
        Ok(status) => status
            .code()
            .map(|code| code.clamp(0, MAX_EXIT_CODE))
            .unwrap_or(1),
        Err(error) => {
            write_server_fallback_error(
                stderr,
                format!("failed to wait for fallback {upstream_program} process: {error}"),
            );
            1
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum ServerStreamKind {
    Stdout,
    Stderr,
}

enum ServerStreamMessage {
    Data(ServerStreamKind, Vec<u8>),
    Error(ServerStreamKind, io::Error),
    Finished(ServerStreamKind),
}

fn spawn_server_reader<R>(
    mut reader: R,
    kind: ServerStreamKind,
    sender: mpsc::Sender<ServerStreamMessage>,
) -> thread::JoinHandle<()>
where
    R: Read + Send + 'static,
{
    thread::spawn(move || {
        let mut buffer = vec![0u8; 8192];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) => {
                    let _ = sender.send(ServerStreamMessage::Finished(kind));
                    break;
                }
                Ok(n) => {
                    if sender
                        .send(ServerStreamMessage::Data(kind, buffer[..n].to_vec()))
                        .is_err()
                    {
                        break;
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => {
                    let _ = sender.send(ServerStreamMessage::Error(kind, error));
                    break;
                }
            }
        }
    })
}

fn join_server_thread(handle: &mut Option<thread::JoinHandle<()>>) {
    if let Some(join) = handle.take() {
        let _ = join.join();
    }
}

fn terminate_server_process(
    child: &mut Child,
    stdout_thread: &mut Option<thread::JoinHandle<()>>,
    stderr_thread: &mut Option<thread::JoinHandle<()>>,
) {
    let _ = child.kill();
    let _ = child.wait();
    join_server_thread(stdout_thread);
    join_server_thread(stderr_thread);
}

fn write_server_fallback_error<Err: Write>(stderr: &mut Err, text: impl fmt::Display) {
    let mut sink = MessageSink::new(stderr);
    let mut message = rsync_error!(1, "{}", text);
    message = message.with_role(Role::Client);
    if write_message(&message, &mut sink).is_err() {
        let _ = writeln!(sink.writer_mut(), "{}", text);
    }
}

/// Delegates execution to the daemon front-end.
fn run_daemon_mode<Out, Err>(args: Vec<OsString>, stdout: &mut Out, stderr: &mut Err) -> i32
where
    Out: Write,
    Err: Write,
{
    rsync_daemon::run(args, stdout, stderr)
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
        let report = VersionInfoReport::default().with_client_brand(program_name.brand());
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

    if let Some(user) = get_user_by_name(input) {
        return Ok(user.uid());
    }

    Err(rsync_error!(1, "unknown user '{}' specified for --chown", input).with_role(Role::Client))
}

fn resolve_chown_group(input: &str) -> Result<gid_t, Message> {
    if let Ok(id) = input.parse::<gid_t>() {
        return Ok(id);
    }

    if let Some(group) = get_group_by_name(input) {
        return Ok(group.gid());
    }

    Err(rsync_error!(1, "unknown group '{}' specified for --chown", input).with_role(Role::Client))
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
    use crate::password::set_password_stdin_input;
    use rsync_checksums::strong::Md5;
    use rsync_core::{branding::manifest, client::FilterRuleKind};
    use rsync_daemon as daemon_cli;
    use rsync_filters::{FilterRule as EngineFilterRule, FilterSet};
    use std::collections::HashSet;
    use std::ffi::{OsStr, OsString};
    use std::io::{self, BufRead, BufReader, Seek, SeekFrom, Write};
    use std::net::{TcpListener, TcpStream};
    use std::path::Path;
    use std::sync::Mutex;
    use std::thread;
    use std::time::Duration;

    #[cfg(unix)]
    use std::os::unix::{ffi::OsStrExt, fs::PermissionsExt};

    const RSYNC: &str = branding::client_program_name();
    const OC_RSYNC: &str = branding::oc_client_program_name();
    const RSYNCD: &str = branding::daemon_program_name();
    const OC_RSYNC_D: &str = branding::oc_daemon_program_name();

    const LEGACY_DAEMON_GREETING: &str = "@RSYNCD: 32.0 sha512 sha256 sha1 md5 md4\n";

    #[cfg(all(
        unix,
        not(any(
            target_os = "ios",
            target_os = "macos",
            target_os = "tvos",
            target_os = "watchos"
        ))
    ))]
    fn mkfifo_for_tests(path: &Path, mode: u32) -> io::Result<()> {
        use rustix::fs::{CWD, FileType, Mode, makedev, mknodat};
        use std::convert::TryInto;

        let bits: u16 = (mode & 0o177_777)
            .try_into()
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "mode out of range"))?;
        let mode = Mode::from_bits_truncate(bits.into());
        mknodat(CWD, path, FileType::Fifo, mode, makedev(0, 0)).map_err(io::Error::from)
    }

    #[cfg(all(
        unix,
        any(
            target_os = "ios",
            target_os = "macos",
            target_os = "tvos",
            target_os = "watchos"
        )
    ))]
    fn mkfifo_for_tests(path: &Path, mode: u32) -> io::Result<()> {
        use std::convert::TryInto;
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;

        let bits: libc::mode_t = (mode & 0o177_777)
            .try_into()
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "mode out of range"))?;
        let path_c = CString::new(path.as_os_str().as_bytes()).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidInput, "path contains interior NUL")
        })?;
        let result = unsafe { libc::mkfifo(path_c.as_ptr(), bits) };
        if result == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    fn assert_contains_client_trailer(rendered: &str) {
        let expected = format!("[client={}]", manifest().rust_version());
        assert!(
            rendered.contains(&expected),
            "expected message to contain {expected:?}, got {rendered:?}"
        );
    }
    static ENV_LOCK: Mutex<()> = Mutex::new(());

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

    struct EnvGuard {
        key: &'static str,
        previous: Option<OsString>,
    }

    #[allow(unsafe_code)]
    impl EnvGuard {
        fn set(key: &'static str, value: &OsStr) -> Self {
            let previous = std::env::var_os(key);
            unsafe {
                std::env::set_var(key, value);
            }
            Self { key, previous }
        }
    }

    #[allow(unsafe_code)]
    impl Drop for EnvGuard {
        fn drop(&mut self) {
            if let Some(value) = self.previous.take() {
                unsafe {
                    std::env::set_var(self.key, value);
                }
            } else {
                unsafe {
                    std::env::remove_var(self.key);
                }
            }
        }
    }

    fn clear_rsync_rsh() -> EnvGuard {
        EnvGuard::set("RSYNC_RSH", OsStr::new(""))
    }

    #[cfg(unix)]
    fn write_executable_script(path: &Path, contents: &str) {
        std::fs::write(path, contents).expect("write script");
        let mut permissions = std::fs::metadata(path)
            .expect("script metadata")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(path, permissions).expect("set script permissions");
    }

    #[test]
    fn version_flag_renders_report() {
        let (code, stdout, stderr) = run_with_args([OsStr::new(RSYNC), OsStr::new("--version")]);

        assert_eq!(code, 0);
        assert!(stderr.is_empty());

        let expected = VersionInfoReport::default().human_readable();
        assert_eq!(stdout, expected.into_bytes());
    }

    #[test]
    fn oc_version_flag_renders_oc_banner() {
        let (code, stdout, stderr) = run_with_args([OsStr::new(OC_RSYNC), OsStr::new("--version")]);

        assert_eq!(code, 0);
        assert!(stderr.is_empty());

        let expected = VersionInfoReport::default()
            .with_client_brand(Brand::Oc)
            .human_readable();
        assert_eq!(stdout, expected.into_bytes());
    }

    #[test]
    fn short_version_flag_renders_report() {
        let (code, stdout, stderr) = run_with_args([OsStr::new(RSYNC), OsStr::new("-V")]);

        assert_eq!(code, 0);
        assert!(stderr.is_empty());

        let expected = VersionInfoReport::default().human_readable();
        assert_eq!(stdout, expected.into_bytes());
    }

    #[test]
    fn skip_compress_env_variable_enables_list() {
        use tempfile::tempdir;

        let _lock = ENV_LOCK.lock().expect("env mutex poisoned");
        let _guard = EnvGuard::set("RSYNC_SKIP_COMPRESS", OsStr::new("gz"));

        let tmp = tempdir().expect("tempdir");
        let source = tmp.path().join("archive.gz");
        let destination = tmp.path().join("dest.gz");
        std::fs::write(&source, b"payload").expect("write source");

        let (code, stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
            OsString::from("-z"),
            source.clone().into_os_string(),
            destination.clone().into_os_string(),
        ]);

        assert_eq!(code, 0);
        assert!(stderr.is_empty());
        assert!(stdout.is_empty());
        assert_eq!(std::fs::read(destination).expect("read dest"), b"payload");
    }

    #[test]
    fn skip_compress_invalid_env_reports_error() {
        use tempfile::tempdir;

        let _lock = ENV_LOCK.lock().expect("env mutex poisoned");
        let _guard = EnvGuard::set("RSYNC_SKIP_COMPRESS", OsStr::new("["));

        let tmp = tempdir().expect("tempdir");
        let source = tmp.path().join("file.txt");
        let destination = tmp.path().join("dest.txt");
        std::fs::write(&source, b"payload").expect("write source");

        let (code, stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
            source.clone().into_os_string(),
            destination.clone().into_os_string(),
        ]);

        assert_eq!(code, 1);
        assert!(stdout.is_empty());
        let rendered = String::from_utf8(stderr).expect("diagnostic is UTF-8");
        assert!(rendered.contains("RSYNC_SKIP_COMPRESS"));
        assert!(rendered.contains("invalid"));
    }

    #[test]
    fn oc_short_version_flag_renders_oc_banner() {
        let (code, stdout, stderr) = run_with_args([OsStr::new(OC_RSYNC), OsStr::new("-V")]);

        assert_eq!(code, 0);
        assert!(stderr.is_empty());

        let expected = VersionInfoReport::default()
            .with_client_brand(Brand::Oc)
            .human_readable();
        assert_eq!(stdout, expected.into_bytes());
    }

    #[test]
    fn version_flag_ignores_additional_operands() {
        let (code, stdout, stderr) = run_with_args([
            OsStr::new(RSYNC),
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
            OsStr::new(RSYNC),
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
            [OsStr::new(RSYNCD), OsStr::new("--help")],
            &mut expected_stdout,
            &mut expected_stderr,
        );

        assert_eq!(expected_code, 0);
        assert!(expected_stderr.is_empty());

        let (code, stdout, stderr) = run_with_args([
            OsStr::new(RSYNC),
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
            [OsStr::new(RSYNCD), OsStr::new("--version")],
            &mut expected_stdout,
            &mut expected_stderr,
        );

        assert_eq!(expected_code, 0);
        assert!(expected_stderr.is_empty());

        let (code, stdout, stderr) = run_with_args([
            OsStr::new(RSYNC),
            OsStr::new("--daemon"),
            OsStr::new("--version"),
        ]);

        assert_eq!(code, expected_code);
        assert_eq!(stdout, expected_stdout);
        assert_eq!(stderr, expected_stderr);
    }

    #[test]
    fn oc_daemon_flag_delegates_to_oc_daemon_version() {
        let mut expected_stdout = Vec::new();
        let mut expected_stderr = Vec::new();
        let expected_code = daemon_cli::run(
            [OsStr::new(OC_RSYNC_D), OsStr::new("--version")],
            &mut expected_stdout,
            &mut expected_stderr,
        );

        assert_eq!(expected_code, 0);
        assert!(expected_stderr.is_empty());

        let (code, stdout, stderr) = run_with_args([
            OsStr::new(OC_RSYNC),
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
            OsString::from(RSYNC),
            OsString::from("--"),
            OsString::from("--daemon"),
            OsString::from("dest"),
        ];

        assert!(daemon_mode_arguments(&args).is_none());
    }

    #[test]
    fn help_flag_renders_static_help_snapshot() {
        let (code, stdout, stderr) = run_with_args([OsStr::new(RSYNC), OsStr::new("--help")]);

        assert_eq!(code, 0);
        assert!(stderr.is_empty());

        let expected = render_help(ProgramName::Rsync);
        assert_eq!(stdout, expected.into_bytes());
    }

    #[test]
    fn oc_help_flag_uses_wrapped_program_name() {
        let (code, stdout, stderr) = run_with_args([OsStr::new(OC_RSYNC), OsStr::new("--help")]);

        assert_eq!(code, 0);
        assert!(stderr.is_empty());

        let expected = render_help(ProgramName::OcRsync);
        assert_eq!(stdout, expected.into_bytes());
    }

    #[test]
    fn oc_help_mentions_oc_rsyncd_delegation() {
        let (code, stdout, stderr) = run_with_args([OsStr::new(OC_RSYNC), OsStr::new("--help")]);

        assert_eq!(code, 0);
        assert!(stderr.is_empty());

        let rendered = String::from_utf8(stdout).expect("valid UTF-8");
        let upstream = format!("delegates to {}", branding::daemon_program_name());
        let branded = format!("delegates to {}", branding::oc_daemon_program_name());
        assert!(rendered.contains(&branded));
        assert!(!rendered.contains(&upstream));
    }

    #[test]
    fn oc_help_mentions_branded_daemon_phrase() {
        let (code, stdout, stderr) = run_with_args([OsStr::new(OC_RSYNC), OsStr::new("--help")]);

        assert_eq!(code, 0);
        assert!(stderr.is_empty());

        let rendered = String::from_utf8(stdout).expect("valid UTF-8");
        let upstream = format!("Run as an {} daemon", branding::client_program_name());
        let branded = format!("Run as an {} daemon", branding::oc_client_program_name());
        assert!(rendered.contains(&branded));
        assert!(!rendered.contains(&upstream));
    }

    #[test]
    fn short_h_flag_enables_human_readable_mode() {
        let parsed = parse_args([
            OsString::from(RSYNC),
            OsString::from("-h"),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .expect("parse");

        assert_eq!(parsed.human_readable, Some(HumanReadableMode::Enabled));
    }

    #[test]
    fn transfer_request_reports_missing_operands() {
        let (code, stdout, stderr) = run_with_args([OsString::from(RSYNC)]);

        assert_eq!(code, 1);
        assert!(stdout.is_empty());

        let rendered = String::from_utf8(stderr).expect("diagnostic is valid UTF-8");
        assert!(rendered.contains("missing source operands"));
        assert_contains_client_trailer(&rendered);
    }

    #[test]
    fn run_reports_invalid_chmod_specification() {
        use tempfile::tempdir;

        let tmp = tempdir().expect("tempdir");
        let source = tmp.path().join("source.txt");
        let destination = tmp.path().join("dest.txt");
        std::fs::write(&source, b"data").expect("write source");

        let (code, stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
            OsString::from("--chmod=a+q"),
            source.into_os_string(),
            destination.into_os_string(),
        ]);

        assert_eq!(code, 1);
        assert!(stdout.is_empty());
        let rendered = String::from_utf8(stderr).expect("diagnostic utf8");
        assert!(rendered.contains("failed to parse --chmod specification"));
    }

    #[test]
    fn transfer_request_copies_file() {
        use tempfile::tempdir;

        let tmp = tempdir().expect("tempdir");
        let source = tmp.path().join("source.txt");
        let destination = tmp.path().join("destination.txt");
        std::fs::write(&source, b"cli copy").expect("write source");

        let (code, stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
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
    fn backup_flag_creates_default_suffix_backups() {
        use tempfile::tempdir;

        let tmp = tempdir().expect("tempdir");
        let source_dir = tmp.path().join("source");
        let dest_dir = tmp.path().join("dest");
        std::fs::create_dir_all(&source_dir).expect("create source dir");
        std::fs::create_dir_all(&dest_dir).expect("create dest dir");

        let source_file = source_dir.join("file.txt");
        std::fs::write(&source_file, b"new data").expect("write source");

        let dest_root = dest_dir.join("source");
        std::fs::create_dir_all(&dest_root).expect("create dest root");
        std::fs::write(dest_root.join("file.txt"), b"old data").expect("seed dest");

        let (code, stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
            OsString::from("--backup"),
            source_dir.clone().into_os_string(),
            dest_dir.clone().into_os_string(),
        ]);

        assert_eq!(code, 0);
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());

        let target_root = dest_dir.join("source");
        assert_eq!(
            std::fs::read(target_root.join("file.txt")).expect("read dest"),
            b"new data"
        );
        assert_eq!(
            std::fs::read(target_root.join("file.txt~")).expect("read backup"),
            b"old data"
        );
    }

    #[test]
    fn backup_dir_flag_places_backups_in_relative_directory() {
        use tempfile::tempdir;

        let tmp = tempdir().expect("tempdir");
        let source_dir = tmp.path().join("source");
        let dest_dir = tmp.path().join("dest");
        std::fs::create_dir_all(source_dir.join("nested")).expect("create nested source");
        std::fs::create_dir_all(dest_dir.join("source/nested")).expect("create nested dest");

        let source_file = source_dir.join("nested/file.txt");
        std::fs::write(&source_file, b"updated").expect("write source");
        std::fs::write(dest_dir.join("source/nested/file.txt"), b"previous").expect("seed dest");

        let (code, stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
            OsString::from("--backup-dir"),
            OsString::from("backups"),
            source_dir.clone().into_os_string(),
            dest_dir.clone().into_os_string(),
        ]);

        assert_eq!(code, 0);
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());

        let backup_path = dest_dir.join("backups/source/nested/file.txt~");
        assert_eq!(
            std::fs::read(&backup_path).expect("read backup"),
            b"previous"
        );
        assert_eq!(
            std::fs::read(dest_dir.join("source/nested/file.txt")).expect("read dest"),
            b"updated"
        );
    }

    #[test]
    fn backup_suffix_flag_overrides_default_suffix() {
        use tempfile::tempdir;

        let tmp = tempdir().expect("tempdir");
        let source_dir = tmp.path().join("source");
        let dest_dir = tmp.path().join("dest");
        std::fs::create_dir_all(&source_dir).expect("create source dir");
        std::fs::create_dir_all(&dest_dir).expect("create dest dir");

        let source_file = source_dir.join("file.txt");
        std::fs::write(&source_file, b"fresh").expect("write source");
        let dest_root = dest_dir.join("source");
        std::fs::create_dir_all(&dest_root).expect("create dest root");
        std::fs::write(dest_root.join("file.txt"), b"stale").expect("seed dest");

        let (code, stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
            OsString::from("--suffix"),
            OsString::from(".bak"),
            source_dir.clone().into_os_string(),
            dest_dir.clone().into_os_string(),
        ]);

        assert_eq!(code, 0);
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());

        assert_eq!(
            std::fs::read(dest_root.join("file.txt")).expect("read dest"),
            b"fresh"
        );
        let backup_path = dest_root.join("file.txt.bak");
        assert_eq!(std::fs::read(&backup_path).expect("read backup"), b"stale");
    }

    #[test]
    fn verbose_transfer_emits_event_lines() {
        use tempfile::tempdir;

        let tmp = tempdir().expect("tempdir");
        let source = tmp.path().join("file.txt");
        let destination = tmp.path().join("out.txt");
        std::fs::write(&source, b"verbose").expect("write source");

        let (code, stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
            OsString::from("-v"),
            source.clone().into_os_string(),
            destination.clone().into_os_string(),
        ]);

        assert_eq!(code, 0);
        assert!(stderr.is_empty());

        let rendered = String::from_utf8(stdout).expect("verbose output is UTF-8");
        assert!(rendered.contains("file.txt"));
        assert!(!rendered.contains("Total transferred"));
        assert!(rendered.contains("sent 7 bytes  received 7 bytes"));
        assert!(rendered.contains("total size is 7  speedup is 0.50"));
        assert_eq!(
            std::fs::read(destination).expect("read destination"),
            b"verbose"
        );
    }

    #[test]
    fn stats_human_readable_formats_totals() {
        use tempfile::tempdir;

        let _env_lock = ENV_LOCK.lock().expect("env lock");
        let _rsh_guard = clear_rsync_rsh();

        let tmp = tempdir().expect("tempdir");
        let source = tmp.path().join("file.bin");
        std::fs::write(&source, vec![0u8; 1_536]).expect("write source");

        let dest_default = tmp.path().join("default");
        let (code, stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
            OsString::from("--stats"),
            source.clone().into_os_string(),
            dest_default.into_os_string(),
        ]);

        assert_eq!(code, 0);
        assert!(stderr.is_empty());
        let rendered = String::from_utf8(stdout).expect("stats output utf8");
        assert!(rendered.contains("Total file size: 1,536 bytes"));
        assert!(rendered.contains("Total bytes sent: 1,536"));

        let dest_human = tmp.path().join("human");
        let (code, stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
            OsString::from("--stats"),
            OsString::from("--human-readable"),
            source.into_os_string(),
            dest_human.into_os_string(),
        ]);

        assert_eq!(code, 0);
        assert!(stderr.is_empty());
        let rendered = String::from_utf8(stdout).expect("stats output utf8");
        assert!(rendered.contains("Total file size: 1.54K bytes"));
        assert!(rendered.contains("Total bytes sent: 1.54K"));
    }

    #[test]
    fn stats_human_readable_combined_formats_totals() {
        use tempfile::tempdir;

        let _env_lock = ENV_LOCK.lock().expect("env lock");
        let _rsh_guard = clear_rsync_rsh();

        let tmp = tempdir().expect("tempdir");
        let source = tmp.path().join("file.bin");
        std::fs::write(&source, vec![0u8; 1_536]).expect("write source");

        let dest_combined = tmp.path().join("combined");
        let (code, stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
            OsString::from("--stats"),
            OsString::from("--human-readable=2"),
            source.into_os_string(),
            dest_combined.into_os_string(),
        ]);

        assert_eq!(code, 0);
        assert!(stderr.is_empty());
        let rendered = String::from_utf8(stdout).expect("stats output utf8");
        assert!(rendered.contains("Total file size: 1.54K (1,536) bytes"));
        assert!(rendered.contains("Total bytes sent: 1.54K (1,536)"));
    }

    #[cfg(unix)]
    #[test]
    fn verbose_transfer_reports_skipped_specials() {
        use tempfile::tempdir;

        let tmp = tempdir().expect("tempdir");
        let source_fifo = tmp.path().join("skip.pipe");
        mkfifo_for_tests(&source_fifo, 0o600).expect("mkfifo");

        let destination = tmp.path().join("dest.pipe");
        let (code, stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
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
    fn verbose_human_readable_formats_sizes() {
        use tempfile::tempdir;

        let tmp = tempdir().expect("tempdir");
        let source = tmp.path().join("sizes.bin");
        std::fs::write(&source, vec![0u8; 1_536]).expect("write source");

        let dest_default = tmp.path().join("default.bin");
        let (code, stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
            OsString::from("-vv"),
            source.clone().into_os_string(),
            dest_default.into_os_string(),
        ]);

        assert_eq!(code, 0);
        assert!(stderr.is_empty());
        let rendered = String::from_utf8(stdout).expect("verbose output utf8");
        assert!(rendered.contains("1,536 bytes"));

        let dest_human = tmp.path().join("human.bin");
        let (code, stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
            OsString::from("-vv"),
            OsString::from("--human-readable"),
            source.into_os_string(),
            dest_human.into_os_string(),
        ]);

        assert_eq!(code, 0);
        assert!(stderr.is_empty());
        let rendered = String::from_utf8(stdout).expect("verbose output utf8");
        assert!(rendered.contains("1.54K bytes"));
    }

    #[test]
    fn verbose_human_readable_combined_formats_sizes() {
        use tempfile::tempdir;

        let tmp = tempdir().expect("tempdir");
        let source = tmp.path().join("sizes.bin");
        std::fs::write(&source, vec![0u8; 1_536]).expect("write source");

        let destination = tmp.path().join("combined.bin");
        let (code, stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
            OsString::from("-vv"),
            OsString::from("--human-readable=2"),
            source.into_os_string(),
            destination.into_os_string(),
        ]);

        assert_eq!(code, 0);
        assert!(stderr.is_empty());
        let rendered = String::from_utf8(stdout).expect("verbose output utf8");
        assert!(rendered.contains("1.54K (1,536) bytes"));
    }

    #[test]
    fn progress_transfer_renders_progress_lines() {
        use tempfile::tempdir;

        let tmp = tempdir().expect("tempdir");
        let source = tmp.path().join("progress.txt");
        let destination = tmp.path().join("progress.out");
        std::fs::write(&source, b"progress").expect("write source");

        let (code, stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
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
    fn progress_human_readable_formats_sizes() {
        use tempfile::tempdir;

        let tmp = tempdir().expect("tempdir");
        let source = tmp.path().join("human-progress.bin");
        std::fs::write(&source, vec![0u8; 1_536]).expect("write source");

        let destination_default = tmp.path().join("default-progress.bin");
        let (code, stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
            OsString::from("--progress"),
            source.clone().into_os_string(),
            destination_default.into_os_string(),
        ]);

        assert_eq!(code, 0);
        assert!(stderr.is_empty());
        let rendered = String::from_utf8(stdout).expect("progress output utf8");
        let normalized = rendered.replace('\r', "\n");
        assert!(normalized.contains("1,536"));

        let destination_human = tmp.path().join("human-progress.out");
        let (code, stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
            OsString::from("--progress"),
            OsString::from("--human-readable"),
            source.into_os_string(),
            destination_human.into_os_string(),
        ]);

        assert_eq!(code, 0);
        assert!(stderr.is_empty());
        let rendered = String::from_utf8(stdout).expect("progress output utf8");
        let normalized = rendered.replace('\r', "\n");
        assert!(normalized.contains("1.54K"));
    }

    #[test]
    fn progress_human_readable_combined_formats_sizes() {
        use tempfile::tempdir;

        let tmp = tempdir().expect("tempdir");
        let source = tmp.path().join("human-progress.bin");
        std::fs::write(&source, vec![0u8; 1_536]).expect("write source");

        let destination = tmp.path().join("combined-progress.out");
        let (code, stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
            OsString::from("--progress"),
            OsString::from("--human-readable=2"),
            source.into_os_string(),
            destination.into_os_string(),
        ]);

        assert_eq!(code, 0);
        assert!(stderr.is_empty());
        let rendered = String::from_utf8(stdout).expect("progress output utf8");
        let normalized = rendered.replace('\r', "\n");
        assert!(normalized.contains("1.54K (1,536)"));
    }

    #[test]
    fn progress_transfer_routes_messages_to_stderr_when_requested() {
        use tempfile::tempdir;

        let tmp = tempdir().expect("tempdir");
        let source = tmp.path().join("stderr-progress.txt");
        let destination = tmp.path().join("stderr-progress.out");
        std::fs::write(&source, b"stderr-progress").expect("write source");

        let (code, stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
            OsString::from("--progress"),
            OsString::from("--msgs2stderr"),
            source.clone().into_os_string(),
            destination.clone().into_os_string(),
        ]);

        assert_eq!(code, 0);
        let rendered_out = String::from_utf8(stdout).expect("stdout utf8");
        assert!(rendered_out.trim().is_empty());

        let rendered_err = String::from_utf8(stderr).expect("stderr utf8");
        assert!(rendered_err.contains("stderr-progress.txt"));
        assert!(rendered_err.contains("(xfr#1, to-chk=0/1)"));

        assert_eq!(
            std::fs::read(destination).expect("read destination"),
            b"stderr-progress"
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
            OsString::from(RSYNC),
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
        use std::os::unix::fs::FileTypeExt;
        use tempfile::tempdir;

        let tmp = tempdir().expect("tempdir");
        let source = tmp.path().join("fifo.in");
        mkfifo_for_tests(&source, 0o600).expect("mkfifo");

        let destination = tmp.path().join("fifo.out");
        let (code, stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
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

    #[cfg(unix)]
    #[test]
    fn info_progress2_enables_progress_output() {
        use std::os::unix::fs::FileTypeExt;
        use tempfile::tempdir;

        let tmp = tempdir().expect("tempdir");
        let source = tmp.path().join("info-fifo.in");
        mkfifo_for_tests(&source, 0o600).expect("mkfifo");

        let destination = tmp.path().join("info-fifo.out");
        let (code, stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
            OsString::from("--info=progress2"),
            OsString::from("--specials"),
            source.clone().into_os_string(),
            destination.clone().into_os_string(),
        ]);

        assert_eq!(code, 0);
        assert!(stderr.is_empty());
        let rendered = String::from_utf8(stdout).expect("progress output is UTF-8");
        assert!(!rendered.contains("info-fifo.in"));
        assert!(rendered.contains("to-chk=0/1"));
        assert!(rendered.contains("0.00kB/s"));

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
            OsString::from(RSYNC),
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
            OsString::from(RSYNC),
            OsString::from("--stats"),
            source.clone().into_os_string(),
            destination.clone().into_os_string(),
        ]);

        assert_eq!(code, 0);
        assert!(stderr.is_empty());

        let rendered = String::from_utf8(stdout).expect("stats output is UTF-8");
        let expected_size = payload.len();
        assert!(rendered.contains("Number of files: 1 (reg: 1)"));
        assert!(rendered.contains("Number of created files: 1 (reg: 1)"));
        assert!(rendered.contains("Number of regular files transferred: 1"));
        assert!(!rendered.contains("Number of regular files matched"));
        assert!(!rendered.contains("Number of hard links"));
        assert!(rendered.contains(&format!("Total file size: {expected_size} bytes")));
        assert!(rendered.contains(&format!("Literal data: {expected_size} bytes")));
        assert!(rendered.contains("Matched data: 0 bytes"));
        assert!(rendered.contains("File list size: 0"));
        assert!(rendered.contains("File list generation time:"));
        assert!(rendered.contains("File list transfer time:"));
        assert!(rendered.contains(&format!("Total bytes sent: {expected_size}")));
        assert!(rendered.contains(&format!("Total bytes received: {expected_size}")));
        assert!(rendered.contains("\n\nsent"));
        assert!(rendered.contains("total size is"));
        assert_eq!(
            std::fs::read(destination).expect("read destination"),
            payload
        );
    }

    #[test]
    fn info_stats_enables_summary_block() {
        use tempfile::tempdir;

        let tmp = tempdir().expect("tempdir");
        let source = tmp.path().join("info-stats.txt");
        let destination = tmp.path().join("info-stats.out");
        let payload = b"statistics";
        std::fs::write(&source, payload).expect("write source");

        let (code, stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
            OsString::from("--info=stats"),
            source.clone().into_os_string(),
            destination.clone().into_os_string(),
        ]);

        assert_eq!(code, 0);
        assert!(stderr.is_empty());

        let rendered = String::from_utf8(stdout).expect("stats output is UTF-8");
        let expected_size = payload.len();
        assert!(rendered.contains("Number of files: 1 (reg: 1)"));
        assert!(rendered.contains(&format!("Total file size: {expected_size} bytes")));
        assert!(rendered.contains("Literal data:"));
        assert!(rendered.contains("\n\nsent"));
        assert!(rendered.contains("total size is"));
        assert_eq!(
            std::fs::read(destination).expect("read destination"),
            payload
        );
    }

    #[test]
    fn info_none_disables_progress_output() {
        use tempfile::tempdir;

        let tmp = tempdir().expect("tempdir");
        let source = tmp.path().join("info-none.txt");
        let destination = tmp.path().join("info-none.out");
        std::fs::write(&source, b"payload").expect("write source");

        let (code, stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
            OsString::from("--progress"),
            OsString::from("--info=none"),
            source.clone().into_os_string(),
            destination.clone().into_os_string(),
        ]);

        assert_eq!(code, 0);
        assert!(stderr.is_empty());
        let rendered = String::from_utf8(stdout).expect("stdout utf8");
        assert!(!rendered.contains("to-chk"));
        assert!(rendered.trim().is_empty());
    }

    #[test]
    fn info_help_lists_supported_flags() {
        let (code, stdout, stderr) = run_with_args([OsStr::new(RSYNC), OsStr::new("--info=help")]);

        assert_eq!(code, 0);
        assert!(stderr.is_empty());
        assert_eq!(stdout, INFO_HELP_TEXT.as_bytes());
    }

    #[test]
    fn debug_help_lists_supported_flags() {
        let (code, stdout, stderr) = run_with_args([OsStr::new(RSYNC), OsStr::new("--debug=help")]);

        assert_eq!(code, 0);
        assert!(stderr.is_empty());
        assert_eq!(stdout, DEBUG_HELP_TEXT.as_bytes());
    }

    #[test]
    fn info_rejects_unknown_flag() {
        let (code, stdout, stderr) =
            run_with_args([OsStr::new(RSYNC), OsStr::new("--info=unknown")]);

        assert_eq!(code, 1);
        assert!(stdout.is_empty());
        let rendered = String::from_utf8(stderr).expect("stderr utf8");
        assert!(rendered.contains("invalid --info flag"));
    }

    #[test]
    fn info_accepts_comma_separated_tokens() {
        let flags = vec![OsString::from("progress,name2,stats")];
        let settings = parse_info_flags(&flags).expect("flags parse");
        assert!(matches!(settings.progress, ProgressSetting::PerFile));
        assert_eq!(settings.name, Some(NameOutputLevel::UpdatedAndUnchanged));
        assert_eq!(settings.stats, Some(true));
    }

    #[test]
    fn info_rejects_empty_segments() {
        let flags = vec![OsString::from("progress,,stats")];
        let error = parse_info_flags(&flags).err().expect("parse should fail");
        assert!(error.to_string().contains("--info flag must not be empty"));
    }

    #[test]
    fn debug_accepts_comma_separated_tokens() {
        let flags = vec![OsString::from("checksum,io")];
        let settings = parse_debug_flags(&flags).expect("flags parse");
        assert!(!settings.help_requested);
        assert_eq!(
            settings.flags,
            vec![OsString::from("checksum"), OsString::from("io")]
        );
    }

    #[test]
    fn debug_rejects_empty_segments() {
        let flags = vec![OsString::from("checksum,,io")];
        let error = parse_debug_flags(&flags).err().expect("parse should fail");
        assert!(error.to_string().contains("--debug flag must not be empty"));
    }

    #[test]
    fn info_name_emits_filenames_without_verbose() {
        use tempfile::tempdir;

        let tmp = tempdir().expect("tempdir");
        let source = tmp.path().join("name.txt");
        let destination = tmp.path().join("name.out");
        std::fs::write(&source, b"name-info").expect("write source");

        let (code, stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
            OsString::from("--info=name"),
            source.clone().into_os_string(),
            destination.clone().into_os_string(),
        ]);

        assert_eq!(code, 0);
        assert!(stderr.is_empty());
        let rendered = String::from_utf8(stdout).expect("stdout utf8");
        assert!(rendered.contains("name.txt"));
        assert!(rendered.contains("sent"));
        assert_eq!(
            std::fs::read(destination).expect("read destination"),
            b"name-info"
        );
    }

    #[test]
    fn info_name0_suppresses_verbose_output() {
        use tempfile::tempdir;

        let tmp = tempdir().expect("tempdir");
        let source = tmp.path().join("quiet.txt");
        let destination = tmp.path().join("quiet.out");
        std::fs::write(&source, b"quiet").expect("write source");

        let (code, stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
            OsString::from("-v"),
            OsString::from("--info=name0"),
            source.clone().into_os_string(),
            destination.clone().into_os_string(),
        ]);

        assert_eq!(code, 0);
        assert!(stderr.is_empty());
        let rendered = String::from_utf8(stdout).expect("stdout utf8");
        assert!(!rendered.contains("quiet.txt"));
        assert!(rendered.contains("sent"));
    }

    #[test]
    fn info_name2_reports_unchanged_entries() {
        use tempfile::tempdir;

        let tmp = tempdir().expect("tempdir");
        let source = tmp.path().join("unchanged.txt");
        let destination = tmp.path().join("unchanged.out");
        std::fs::write(&source, b"unchanged").expect("write source");

        let initial = run_with_args([
            OsString::from(RSYNC),
            source.clone().into_os_string(),
            destination.clone().into_os_string(),
        ]);
        assert_eq!(initial.0, 0);
        assert!(initial.1.is_empty());
        assert!(initial.2.is_empty());

        let (code, stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
            OsString::from("--info=name2"),
            source.into_os_string(),
            destination.into_os_string(),
        ]);

        assert_eq!(code, 0);
        assert!(stderr.is_empty());
        let rendered = String::from_utf8(stdout).expect("stdout utf8");
        assert!(rendered.contains("unchanged.txt"));
    }

    #[test]
    fn transfer_request_with_archive_copies_file() {
        use tempfile::tempdir;

        let tmp = tempdir().expect("tempdir");
        let source = tmp.path().join("source.txt");
        let destination = tmp.path().join("destination.txt");
        std::fs::write(&source, b"archive").expect("write source");

        let (code, stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
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
            OsString::from(RSYNC),
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
    fn transfer_request_with_remove_sent_files_alias_deletes_source() {
        use tempfile::tempdir;

        let tmp = tempdir().expect("tempdir");
        let source = tmp.path().join("source.txt");
        let destination = tmp.path().join("destination.txt");
        std::fs::write(&source, b"alias move").expect("write source");

        let (code, stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
            OsString::from("--remove-sent-files"),
            source.clone().into_os_string(),
            destination.clone().into_os_string(),
        ]);

        assert_eq!(code, 0);
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());
        assert!(!source.exists(), "source should be removed");
        assert_eq!(
            std::fs::read(destination).expect("read destination"),
            b"alias move"
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
            OsString::from(RSYNC),
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
    fn transfer_request_with_no_bwlimit_copies_file() {
        use tempfile::tempdir;

        let tmp = tempdir().expect("tempdir");
        let source = tmp.path().join("source.txt");
        let destination = tmp.path().join("destination.txt");
        std::fs::write(&source, b"unlimited").expect("write source");

        let (code, stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
            OsString::from("--no-bwlimit"),
            source.clone().into_os_string(),
            destination.clone().into_os_string(),
        ]);

        assert_eq!(code, 0);
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());
        assert_eq!(
            std::fs::read(destination).expect("read destination"),
            b"unlimited"
        );
    }

    #[test]
    fn transfer_request_with_out_format_renders_entries() {
        use tempfile::tempdir;

        let tmp = tempdir().expect("tempdir");
        let source = tmp.path().join("source.txt");
        let dest_dir = tmp.path().join("dest");
        std::fs::create_dir(&dest_dir).expect("create dest dir");
        std::fs::write(&source, b"format").expect("write source");

        let (code, stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
            OsString::from("--out-format=%f %b"),
            source.clone().into_os_string(),
            dest_dir.clone().into_os_string(),
        ]);

        assert_eq!(code, 0);
        assert!(stderr.is_empty());
        assert_eq!(String::from_utf8(stdout).expect("utf8"), "source.txt 6\n");

        let destination = dest_dir.join("source.txt");
        assert_eq!(
            std::fs::read(destination).expect("read destination"),
            b"format"
        );
    }

    #[test]
    fn transfer_request_with_itemize_changes_renders_itemized_output() {
        use tempfile::tempdir;

        let tmp = tempdir().expect("tempdir");
        let source = tmp.path().join("source.txt");
        let dest_dir = tmp.path().join("dest");
        std::fs::create_dir(&dest_dir).expect("create dest dir");
        std::fs::write(&source, b"itemized").expect("write source");

        let (code, stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
            OsString::from("--itemize-changes"),
            source.clone().into_os_string(),
            dest_dir.clone().into_os_string(),
        ]);

        assert_eq!(code, 0);
        assert!(stderr.is_empty());
        assert_eq!(
            String::from_utf8(stdout).expect("utf8"),
            ">f+++++++++ source.txt\n"
        );

        let destination = dest_dir.join("source.txt");
        assert_eq!(
            std::fs::read(destination).expect("read destination"),
            b"itemized"
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
            OsString::from(RSYNC),
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
    fn transfer_request_with_ignore_missing_args_skips_missing_sources() {
        use tempfile::tempdir;

        let tmp = tempdir().expect("tempdir");
        let missing = tmp.path().join("missing.txt");
        let destination = tmp.path().join("destination.txt");
        std::fs::write(&destination, b"existing").expect("write destination");

        let (code, stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
            OsString::from("--ignore-missing-args"),
            missing.clone().into_os_string(),
            destination.clone().into_os_string(),
        ]);

        assert_eq!(code, 0);
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());
        assert_eq!(
            std::fs::read(destination).expect("read destination"),
            b"existing"
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
            OsString::from(RSYNC),
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
            .seek(SeekFrom::Start(1024 * 1024))
            .expect("seek to hole");
        source_file.write_all(&[0x20]).expect("write trailing byte");
        source_file.set_len(3 * 1024 * 1024).expect("extend source");

        let dense_dest = tmp.path().join("dense.bin");
        let sparse_dest = tmp.path().join("sparse.bin");

        let (code, stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
            source.clone().into_os_string(),
            dense_dest.clone().into_os_string(),
        ]);
        assert_eq!(code, 0);
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());

        let (code, stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
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
            OsString::from(RSYNC),
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
            OsString::from(RSYNC),
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
            OsString::from(RSYNC),
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
            OsString::from(RSYNC),
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
            OsString::from(RSYNC),
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
            OsString::from(RSYNC),
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
            OsString::from(RSYNC),
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
            OsString::from(RSYNC),
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
            OsString::from(RSYNC),
            OsString::from("--perms"),
            OsString::from("--times"),
            OsString::from("--omit-dir-times"),
            OsString::from("--omit-link-times"),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .expect("parse");

        assert_eq!(parsed.perms, Some(true));
        assert_eq!(parsed.times, Some(true));
        assert_eq!(parsed.omit_dir_times, Some(true));
        assert_eq!(parsed.omit_link_times, Some(true));

        let parsed = parse_args([
            OsString::from(RSYNC),
            OsString::from("-a"),
            OsString::from("--no-perms"),
            OsString::from("--no-times"),
            OsString::from("--no-omit-dir-times"),
            OsString::from("--no-omit-link-times"),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .expect("parse");

        assert_eq!(parsed.perms, Some(false));
        assert_eq!(parsed.times, Some(false));
        assert_eq!(parsed.omit_dir_times, Some(false));
        assert_eq!(parsed.omit_link_times, Some(false));
    }

    #[test]
    fn parse_args_recognises_super_flag() {
        let parsed = parse_args([
            OsString::from(RSYNC),
            OsString::from("--super"),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .expect("parse");

        assert_eq!(parsed.super_mode, Some(true));
    }

    #[test]
    fn parse_args_recognises_no_super_flag() {
        let parsed = parse_args([
            OsString::from(RSYNC),
            OsString::from("--no-super"),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .expect("parse");

        assert_eq!(parsed.super_mode, Some(false));
    }

    #[test]
    fn parse_args_collects_chmod_values() {
        let parsed = parse_args([
            OsString::from(RSYNC),
            OsString::from("--chmod=Du+rwx"),
            OsString::from("--chmod"),
            OsString::from("Fgo-w"),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .expect("parse");

        assert_eq!(
            parsed.chmod,
            vec![OsString::from("Du+rwx"), OsString::from("Fgo-w")]
        );
    }

    #[test]
    fn parse_args_recognises_update_flag() {
        let parsed = parse_args([
            OsString::from(RSYNC),
            OsString::from("--update"),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .expect("parse");

        assert!(parsed.update);
    }

    #[test]
    fn parse_args_recognises_modify_window() {
        let parsed = parse_args([
            OsString::from(RSYNC),
            OsString::from("--modify-window=5"),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .expect("parse");

        assert_eq!(parsed.modify_window, Some(OsString::from("5")));
    }

    #[test]
    fn parse_args_recognises_checksum_choice() {
        let parsed = parse_args([
            OsString::from(RSYNC),
            OsString::from("--checksum-choice=XXH128"),
        ])
        .expect("parse");

        let expected = StrongChecksumChoice::parse("xxh128").expect("choice");
        assert_eq!(parsed.checksum_choice, Some(expected));
        assert_eq!(parsed.checksum_choice_arg, Some(OsString::from("xxh128")));
    }

    #[test]
    fn parse_args_rejects_invalid_checksum_choice() {
        let error = match parse_args([
            OsString::from(RSYNC),
            OsString::from("--checksum-choice=invalid"),
        ]) {
            Ok(_) => panic!("parse should fail"),
            Err(error) => error,
        };

        assert_eq!(error.kind(), clap::error::ErrorKind::ValueValidation);
        assert!(
            error
                .to_string()
                .contains("invalid --checksum-choice value 'invalid'")
        );
    }

    #[test]
    fn parse_args_recognises_owner_overrides() {
        let parsed = parse_args([
            OsString::from(RSYNC),
            OsString::from("--owner"),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .expect("parse");

        assert_eq!(parsed.owner, Some(true));
        assert_eq!(parsed.group, None);

        let parsed = parse_args([
            OsString::from(RSYNC),
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
    fn parse_args_recognises_chown_option() {
        let parsed = parse_args([
            OsString::from(RSYNC),
            OsString::from("--chown=user:group"),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .expect("parse");

        assert_eq!(parsed.chown, Some(OsString::from("user:group")));
    }

    #[test]
    fn parse_args_sets_protect_args_flag() {
        let parsed =
            parse_args([OsString::from(RSYNC), OsString::from("--protect-args")]).expect("parse");

        assert_eq!(parsed.protect_args, Some(true));
    }

    #[test]
    fn parse_args_sets_protect_args_alias() {
        let parsed =
            parse_args([OsString::from(RSYNC), OsString::from("--secluded-args")]).expect("parse");

        assert_eq!(parsed.protect_args, Some(true));
    }

    #[test]
    fn parse_args_sets_no_protect_args_flag() {
        let parsed = parse_args([OsString::from(RSYNC), OsString::from("--no-protect-args")])
            .expect("parse");

        assert_eq!(parsed.protect_args, Some(false));
    }

    #[test]
    fn parse_args_sets_no_protect_args_alias() {
        let parsed = parse_args([OsString::from(RSYNC), OsString::from("--no-secluded-args")])
            .expect("parse");

        assert_eq!(parsed.protect_args, Some(false));
    }

    #[test]
    fn parse_args_sets_ipv4_address_mode() {
        let parsed = parse_args([
            OsString::from(RSYNC),
            OsString::from("--ipv4"),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .expect("parse");

        assert_eq!(parsed.address_mode, AddressMode::Ipv4);
    }

    #[test]
    fn parse_args_sets_ipv6_address_mode() {
        let parsed = parse_args([
            OsString::from(RSYNC),
            OsString::from("--ipv6"),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .expect("parse");

        assert_eq!(parsed.address_mode, AddressMode::Ipv6);
    }

    #[test]
    fn parse_args_recognises_group_overrides() {
        let parsed = parse_args([
            OsString::from(RSYNC),
            OsString::from("--group"),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .expect("parse");

        assert_eq!(parsed.group, Some(true));
        assert_eq!(parsed.owner, None);

        let parsed = parse_args([
            OsString::from(RSYNC),
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
    fn parse_args_reads_env_protect_args_default() {
        let _env_lock = ENV_LOCK.lock().expect("env lock");
        let _guard = EnvGuard::set("RSYNC_PROTECT_ARGS", OsStr::new("1"));

        let parsed = parse_args([OsString::from(RSYNC)]).expect("parse");

        assert_eq!(parsed.protect_args, Some(true));
    }

    #[test]
    fn parse_args_respects_env_protect_args_disabled() {
        let _env_lock = ENV_LOCK.lock().expect("env lock");
        let _guard = EnvGuard::set("RSYNC_PROTECT_ARGS", OsStr::new("0"));

        let parsed = parse_args([OsString::from(RSYNC)]).expect("parse");

        assert_eq!(parsed.protect_args, Some(false));
    }

    #[test]
    fn parse_args_recognises_numeric_ids_flags() {
        let parsed = parse_args([
            OsString::from(RSYNC),
            OsString::from("--numeric-ids"),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .expect("parse");

        assert_eq!(parsed.numeric_ids, Some(true));

        let parsed = parse_args([
            OsString::from(RSYNC),
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
            OsString::from(RSYNC),
            OsString::from("--sparse"),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .expect("parse");

        assert_eq!(parsed.sparse, Some(true));

        let parsed = parse_args([
            OsString::from(RSYNC),
            OsString::from("--no-sparse"),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .expect("parse");

        assert_eq!(parsed.sparse, Some(false));
    }

    #[test]
    fn parse_args_recognises_copy_links_flags() {
        let parsed = parse_args([
            OsString::from(RSYNC),
            OsString::from("--copy-links"),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .expect("parse");

        assert_eq!(parsed.copy_links, Some(true));

        let parsed = parse_args([
            OsString::from(RSYNC),
            OsString::from("--no-copy-links"),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .expect("parse");

        assert_eq!(parsed.copy_links, Some(false));

        let parsed = parse_args([
            OsString::from(RSYNC),
            OsString::from("-L"),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .expect("parse");

        assert_eq!(parsed.copy_links, Some(true));
    }

    #[test]
    fn parse_args_recognises_copy_unsafe_links_flags() {
        let parsed = parse_args([
            OsString::from(RSYNC),
            OsString::from("--copy-unsafe-links"),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .expect("parse");

        assert_eq!(parsed.copy_unsafe_links, Some(true));
        assert!(parsed.safe_links);

        let parsed = parse_args([
            OsString::from(RSYNC),
            OsString::from("--no-copy-unsafe-links"),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .expect("parse");

        assert_eq!(parsed.copy_unsafe_links, Some(false));
    }

    #[test]
    fn parse_args_recognises_hard_links_flags() {
        let parsed = parse_args([
            OsString::from(RSYNC),
            OsString::from("--hard-links"),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .expect("parse");

        assert_eq!(parsed.hard_links, Some(true));

        let parsed = parse_args([
            OsString::from(RSYNC),
            OsString::from("--no-hard-links"),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .expect("parse");

        assert_eq!(parsed.hard_links, Some(false));

        let parsed = parse_args([
            OsString::from(RSYNC),
            OsString::from("-H"),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .expect("parse");

        assert_eq!(parsed.hard_links, Some(true));
    }

    #[test]
    fn parse_args_recognises_copy_dirlinks_flag() {
        let parsed = parse_args([
            OsString::from(RSYNC),
            OsString::from("--copy-dirlinks"),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .expect("parse");

        assert!(parsed.copy_dirlinks);

        let parsed = parse_args([
            OsString::from(RSYNC),
            OsString::from("-k"),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .expect("parse");

        assert!(parsed.copy_dirlinks);
    }

    #[test]
    fn parse_args_recognises_keep_dirlinks_flags() {
        let parsed = parse_args([
            OsString::from(RSYNC),
            OsString::from("--keep-dirlinks"),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .expect("parse");

        assert_eq!(parsed.keep_dirlinks, Some(true));

        let parsed = parse_args([
            OsString::from(RSYNC),
            OsString::from("--no-keep-dirlinks"),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .expect("parse");

        assert_eq!(parsed.keep_dirlinks, Some(false));

        let parsed = parse_args([
            OsString::from(RSYNC),
            OsString::from("-K"),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .expect("parse");

        assert_eq!(parsed.keep_dirlinks, Some(true));
    }

    #[test]
    fn parse_args_recognises_safe_links_flag() {
        let parsed = parse_args([
            OsString::from(RSYNC),
            OsString::from("--safe-links"),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .expect("parse");

        assert!(parsed.safe_links);
    }

    #[test]
    fn parse_args_recognises_cvs_exclude_flag() {
        let parsed = parse_args([
            OsString::from(RSYNC),
            OsString::from("--cvs-exclude"),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .expect("parse");

        assert!(parsed.cvs_exclude);

        let parsed = parse_args([
            OsString::from(RSYNC),
            OsString::from("-C"),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .expect("parse");

        assert!(parsed.cvs_exclude);
    }

    #[test]
    fn parse_args_recognises_partial_dir_and_enables_partial() {
        let parsed = parse_args([
            OsString::from(RSYNC),
            OsString::from("--partial-dir=.rsync-partial"),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .expect("parse");

        assert!(parsed.partial);
        assert_eq!(
            parsed.partial_dir.as_deref(),
            Some(Path::new(".rsync-partial"))
        );
    }

    #[test]
    fn parse_args_recognises_temp_dir_flag() {
        let parsed = parse_args([
            OsString::from(RSYNC),
            OsString::from("--temp-dir=.rsync-tmp"),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .expect("parse");

        assert_eq!(parsed.temp_dir.as_deref(), Some(Path::new(".rsync-tmp")));
    }

    #[test]
    fn parse_args_resets_delay_updates_with_no_delay_updates() {
        let parsed = parse_args([
            OsString::from(RSYNC),
            OsString::from("--delay-updates"),
            OsString::from("--no-delay-updates"),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .expect("parse");

        assert!(!parsed.delay_updates);
    }

    #[test]
    fn parse_args_allows_no_partial_to_clear_partial_dir() {
        let parsed = parse_args([
            OsString::from(RSYNC),
            OsString::from("--partial-dir=.rsync-partial"),
            OsString::from("--no-partial"),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .expect("parse");

        assert!(!parsed.partial);
        assert!(parsed.partial_dir.is_none());
    }

    #[test]
    fn parse_args_recognises_delay_updates_flag() {
        let parsed = parse_args([
            OsString::from(RSYNC),
            OsString::from("--delay-updates"),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .expect("parse");

        assert!(parsed.delay_updates);
    }

    #[test]
    fn parse_args_recognises_no_delay_updates_flag() {
        let parsed = parse_args([
            OsString::from(RSYNC),
            OsString::from("--delay-updates"),
            OsString::from("--no-delay-updates"),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .expect("parse");

        assert!(!parsed.delay_updates);
    }

    #[test]
    fn parse_args_collects_link_dest_paths() {
        let parsed = parse_args([
            OsString::from(RSYNC),
            OsString::from("--link-dest=baseline"),
            OsString::from("--link-dest=/var/cache"),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .expect("parse");

        assert_eq!(
            parsed.link_dests,
            vec![PathBuf::from("baseline"), PathBuf::from("/var/cache")]
        );
    }

    #[test]
    fn parse_args_recognises_devices_flags() {
        let parsed = parse_args([
            OsString::from(RSYNC),
            OsString::from("--devices"),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .expect("parse");

        assert_eq!(parsed.devices, Some(true));

        let parsed = parse_args([
            OsString::from(RSYNC),
            OsString::from("--no-devices"),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .expect("parse");

        assert_eq!(parsed.devices, Some(false));
    }

    #[test]
    fn parse_args_recognises_remove_sent_files_alias() {
        let parsed = parse_args([
            OsString::from(RSYNC),
            OsString::from("--remove-sent-files"),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .expect("parse");

        assert!(parsed.remove_source_files);
    }

    #[test]
    fn parse_args_recognises_specials_flags() {
        let parsed = parse_args([
            OsString::from(RSYNC),
            OsString::from("--specials"),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .expect("parse");

        assert_eq!(parsed.specials, Some(true));

        let parsed = parse_args([
            OsString::from(RSYNC),
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
            OsString::from(RSYNC),
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
            OsString::from(RSYNC),
            OsString::from("--relative"),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .expect("parse");

        assert_eq!(parsed.relative, Some(true));

        let parsed = parse_args([
            OsString::from(RSYNC),
            OsString::from("--no-relative"),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .expect("parse");

        assert_eq!(parsed.relative, Some(false));
    }

    #[test]
    fn parse_args_recognises_one_file_system_flags() {
        let parsed = parse_args([
            OsString::from(RSYNC),
            OsString::from("--one-file-system"),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .expect("parse");

        assert_eq!(parsed.one_file_system, Some(true));

        let parsed = parse_args([
            OsString::from(RSYNC),
            OsString::from("--no-one-file-system"),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .expect("parse");

        assert_eq!(parsed.one_file_system, Some(false));

        let parsed = parse_args([
            OsString::from(RSYNC),
            OsString::from("-x"),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .expect("parse");

        assert_eq!(parsed.one_file_system, Some(true));
    }

    #[test]
    fn parse_args_recognises_implied_dirs_flags() {
        let parsed = parse_args([
            OsString::from(RSYNC),
            OsString::from("--implied-dirs"),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .expect("parse");

        assert_eq!(parsed.implied_dirs, Some(true));

        let parsed = parse_args([
            OsString::from(RSYNC),
            OsString::from("--no-implied-dirs"),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .expect("parse");

        assert_eq!(parsed.implied_dirs, Some(false));
    }

    #[test]
    fn parse_args_recognises_prune_empty_dirs_flags() {
        let parsed = parse_args([
            OsString::from(RSYNC),
            OsString::from("--prune-empty-dirs"),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .expect("parse");

        assert_eq!(parsed.prune_empty_dirs, Some(true));

        let parsed = parse_args([
            OsString::from(RSYNC),
            OsString::from("--no-prune-empty-dirs"),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .expect("parse");

        assert_eq!(parsed.prune_empty_dirs, Some(false));
    }

    #[test]
    fn parse_args_recognises_inplace_flags() {
        let parsed = parse_args([
            OsString::from(RSYNC),
            OsString::from("--inplace"),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .expect("parse");

        assert_eq!(parsed.inplace, Some(true));

        let parsed = parse_args([
            OsString::from(RSYNC),
            OsString::from("--no-inplace"),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .expect("parse");

        assert_eq!(parsed.inplace, Some(false));
    }

    #[test]
    fn parse_args_recognises_append_flags() {
        let parsed = parse_args([
            OsString::from(RSYNC),
            OsString::from("--append"),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .expect("parse");

        assert_eq!(parsed.append, Some(true));
        assert!(!parsed.append_verify);

        let parsed = parse_args([
            OsString::from(RSYNC),
            OsString::from("--no-append"),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .expect("parse");

        assert_eq!(parsed.append, Some(false));
        assert!(!parsed.append_verify);
    }

    #[test]
    fn parse_args_captures_skip_compress_value() {
        let parsed = parse_args([
            OsString::from(RSYNC),
            OsString::from("--skip-compress=gz/mp3"),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .expect("parse");

        assert_eq!(parsed.skip_compress, Some(OsString::from("gz/mp3")));
    }

    #[test]
    fn parse_args_recognises_append_verify_flag() {
        let parsed = parse_args([
            OsString::from(RSYNC),
            OsString::from("--append-verify"),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .expect("parse");

        assert_eq!(parsed.append, Some(true));
        assert!(parsed.append_verify);
    }

    #[test]
    fn parse_args_recognises_preallocate_flag() {
        let parsed = parse_args([
            OsString::from(RSYNC),
            OsString::from("--preallocate"),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .expect("parse");

        assert!(parsed.preallocate);

        let parsed = parse_args([
            OsString::from(RSYNC),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .expect("parse");

        assert!(!parsed.preallocate);
    }

    #[test]
    fn parse_args_recognises_whole_file_flags() {
        let parsed = parse_args([
            OsString::from(RSYNC),
            OsString::from("-W"),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .expect("parse");

        assert_eq!(parsed.whole_file, Some(true));

        let parsed = parse_args([
            OsString::from(RSYNC),
            OsString::from("--no-whole-file"),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .expect("parse");

        assert_eq!(parsed.whole_file, Some(false));
    }

    #[test]
    fn parse_args_recognises_stats_flag() {
        let parsed = parse_args([
            OsString::from(RSYNC),
            OsString::from("--stats"),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .expect("parse");

        assert!(parsed.stats);
    }

    #[test]
    fn parse_args_recognises_human_readable_flag() {
        let parsed = parse_args([
            OsString::from(RSYNC),
            OsString::from("--human-readable"),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .expect("parse");

        assert_eq!(parsed.human_readable, Some(HumanReadableMode::Enabled));
    }

    #[test]
    fn parse_args_recognises_no_human_readable_flag() {
        let parsed = parse_args([
            OsString::from(RSYNC),
            OsString::from("--no-human-readable"),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .expect("parse");

        assert_eq!(parsed.human_readable, Some(HumanReadableMode::Disabled));
    }

    #[test]
    fn parse_args_recognises_human_readable_level_two() {
        let parsed = parse_args([
            OsString::from(RSYNC),
            OsString::from("--human-readable=2"),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .expect("parse");

        assert_eq!(parsed.human_readable, Some(HumanReadableMode::Combined));
    }

    #[test]
    fn parse_args_recognises_msgs2stderr_flag() {
        let parsed = parse_args([
            OsString::from(RSYNC),
            OsString::from("--msgs2stderr"),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .expect("parse");

        assert!(parsed.msgs_to_stderr);
    }

    #[test]
    fn parse_args_collects_out_format_value() {
        let parsed = parse_args([
            OsString::from(RSYNC),
            OsString::from("--out-format=%f %b"),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .expect("parse");

        assert_eq!(parsed.out_format, Some(OsString::from("%f %b")));
    }

    #[test]
    fn parse_args_recognises_itemize_changes_flag() {
        let parsed = parse_args([
            OsString::from(RSYNC),
            OsString::from("--itemize-changes"),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .expect("parse");

        assert!(parsed.itemize_changes);
    }

    #[test]
    fn parse_args_recognises_port_value() {
        let parsed = parse_args([
            OsString::from(RSYNC),
            OsString::from("--port=10873"),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .expect("parse");

        assert_eq!(parsed.daemon_port, Some(10873));
    }

    #[test]
    fn parse_args_recognises_list_only_flag() {
        let parsed = parse_args([
            OsString::from(RSYNC),
            OsString::from("--list-only"),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .expect("parse");

        assert!(parsed.list_only);
        assert!(parsed.dry_run);
    }

    #[test]
    fn parse_args_recognises_mkpath_flag() {
        let parsed = parse_args([
            OsString::from(RSYNC),
            OsString::from("--mkpath"),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .expect("parse");

        assert!(parsed.mkpath);
    }

    #[test]
    fn parse_args_recognises_no_bwlimit_flag() {
        let parsed = parse_args([
            OsString::from(RSYNC),
            OsString::from("--no-bwlimit"),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .expect("parse");

        assert!(matches!(parsed.bwlimit, Some(BandwidthArgument::Disabled)));
    }

    #[test]
    fn parse_args_no_bwlimit_overrides_bwlimit_value() {
        let parsed = parse_args([
            OsString::from(RSYNC),
            OsString::from("--bwlimit=2M"),
            OsString::from("--no-bwlimit"),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .expect("parse");

        assert!(matches!(parsed.bwlimit, Some(BandwidthArgument::Disabled)));
    }

    #[test]
    fn parse_args_collects_filter_patterns() {
        let parsed = parse_args([
            OsString::from(RSYNC),
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
            OsString::from(RSYNC),
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
    fn parse_args_collects_reference_destinations() {
        let parsed = parse_args([
            OsString::from(RSYNC),
            OsString::from("--compare-dest"),
            OsString::from("compare"),
            OsString::from("--copy-dest"),
            OsString::from("copy"),
            OsString::from("--link-dest"),
            OsString::from("link"),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .expect("parse");

        assert_eq!(parsed.compare_destinations, vec![OsString::from("compare")]);
        assert_eq!(parsed.copy_destinations, vec![OsString::from("copy")]);
        assert_eq!(parsed.link_destinations, vec![OsString::from("link")]);
    }

    #[test]
    fn parse_args_collects_files_from_paths() {
        let parsed = parse_args([
            OsString::from(RSYNC),
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
            OsString::from(RSYNC),
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
            OsString::from(RSYNC),
            OsString::from("--password-file"),
            OsString::from("secret.txt"),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .expect("parse");

        assert_eq!(parsed.password_file, Some(OsString::from("secret.txt")));

        let parsed = parse_args([
            OsString::from(RSYNC),
            OsString::from("--password-file=secrets.d"),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .expect("parse");

        assert_eq!(parsed.password_file, Some(OsString::from("secrets.d")));
    }

    #[test]
    fn chown_requires_non_empty_components() {
        let error =
            parse_chown_argument(OsStr::new("")).expect_err("empty --chown spec should fail");
        let rendered = error.to_string();
        assert!(
            rendered.contains("--chown requires a non-empty USER and/or GROUP"),
            "diagnostic missing non-empty message: {rendered}"
        );

        let colon_error =
            parse_chown_argument(OsStr::new(":")).expect_err("missing user and group should fail");
        let colon_rendered = colon_error.to_string();
        assert!(
            colon_rendered.contains("--chown requires a user and/or group"),
            "diagnostic missing user/group message: {colon_rendered}"
        );
    }

    #[test]
    fn parse_args_recognises_no_motd_flag() {
        let parsed = parse_args([
            OsString::from(RSYNC),
            OsString::from("--no-motd"),
            OsString::from("rsync://example/"),
        ])
        .expect("parse");

        assert!(parsed.no_motd);
    }

    #[test]
    fn parse_args_collects_protocol_value() {
        let parsed = parse_args([
            OsString::from(RSYNC),
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
            OsString::from(RSYNC),
            OsString::from("--timeout=90"),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .expect("parse");

        assert_eq!(parsed.timeout, Some(OsString::from("90")));
    }

    #[test]
    fn parse_args_collects_contimeout_value() {
        let parsed = parse_args([
            OsString::from(RSYNC),
            OsString::from("--contimeout=45"),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .expect("parse");

        assert_eq!(parsed.contimeout, Some(OsString::from("45")));
    }

    #[test]
    fn parse_args_collects_max_delete_value() {
        let parsed = parse_args([
            OsString::from(RSYNC),
            OsString::from("--max-delete=12"),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .expect("parse");

        assert_eq!(parsed.max_delete, Some(OsString::from("12")));
    }

    #[test]
    fn parse_args_collects_checksum_seed_value() {
        let parsed = parse_args([
            OsString::from(RSYNC),
            OsString::from("--checksum-seed=42"),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .expect("parse");

        assert_eq!(parsed.checksum_seed, Some(42));
    }

    #[test]
    fn parse_args_collects_min_max_size_values() {
        let parsed = parse_args([
            OsString::from(RSYNC),
            OsString::from("--min-size=1.5K"),
            OsString::from("--max-size=2M"),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .expect("parse");

        assert_eq!(parsed.min_size, Some(OsString::from("1.5K")));
        assert_eq!(parsed.max_size, Some(OsString::from("2M")));
    }

    #[test]
    fn parse_max_delete_argument_accepts_zero() {
        let limit = parse_max_delete_argument(OsStr::new("0")).expect("parse max-delete");
        assert_eq!(limit, 0);
    }

    #[test]
    fn parse_size_limit_argument_accepts_fractional_units() {
        let value =
            parse_size_limit_argument(OsStr::new("1.5K"), "--min-size").expect("parse size limit");
        assert_eq!(value, 1536);
    }

    #[test]
    fn parse_size_limit_argument_rejects_negative() {
        let error = parse_size_limit_argument(OsStr::new("-2"), "--min-size")
            .expect_err("negative rejected");
        let rendered = error.to_string();
        assert!(
            rendered.contains("size must be non-negative"),
            "missing detail: {rendered}"
        );
    }

    #[test]
    fn parse_size_limit_argument_rejects_invalid_suffix() {
        let error = parse_size_limit_argument(OsStr::new("10QB"), "--max-size")
            .expect_err("invalid suffix rejected");
        let rendered = error.to_string();
        assert!(
            rendered.contains("expected a size with an optional"),
            "missing message: {rendered}"
        );
    }

    #[test]
    fn pow_u128_for_size_accepts_zero_exponent() {
        let result = pow_u128_for_size(1024, 0).expect("pow for zero exponent");
        assert_eq!(result, 1);
    }

    #[test]
    fn pow_u128_for_size_reports_overflow() {
        let result = pow_u128_for_size(u32::MAX, 5);
        assert!(matches!(result, Err(SizeParseError::TooLarge)));
    }

    #[test]
    fn parse_max_delete_argument_rejects_negative() {
        let error =
            parse_max_delete_argument(OsStr::new("-4")).expect_err("negative limit should fail");
        let rendered = error.to_string();
        assert!(
            rendered.contains("deletion limit must be non-negative"),
            "diagnostic missing detail: {rendered}"
        );
    }

    #[test]
    fn parse_max_delete_argument_rejects_non_numeric() {
        let error = parse_max_delete_argument(OsStr::new("abc"))
            .expect_err("non-numeric limit should fail");
        let rendered = error.to_string();
        assert!(
            rendered.contains("deletion limit must be an unsigned integer"),
            "diagnostic missing unsigned message: {rendered}"
        );
    }

    #[test]
    fn parse_checksum_seed_argument_accepts_zero() {
        let seed = parse_checksum_seed_argument(OsStr::new("0")).expect("parse checksum seed");
        assert_eq!(seed, 0);
    }

    #[test]
    fn parse_checksum_seed_argument_accepts_max_u32() {
        let seed =
            parse_checksum_seed_argument(OsStr::new("4294967295")).expect("parse checksum seed");
        assert_eq!(seed, u32::MAX);
    }

    #[test]
    fn parse_checksum_seed_argument_rejects_negative() {
        let error =
            parse_checksum_seed_argument(OsStr::new("-1")).expect_err("negative seed should fail");
        let rendered = error.to_string();
        assert!(
            rendered.contains("must be non-negative"),
            "diagnostic missing negativity detail: {rendered}"
        );
    }

    #[test]
    fn parse_checksum_seed_argument_rejects_non_numeric() {
        let error = parse_checksum_seed_argument(OsStr::new("seed"))
            .expect_err("non-numeric seed should fail");
        let rendered = error.to_string();
        assert!(
            rendered.contains("invalid --checksum-seed value"),
            "diagnostic missing invalid message: {rendered}"
        );
    }

    #[test]
    fn parse_modify_window_argument_accepts_positive_values() {
        let value = parse_modify_window_argument(OsStr::new("  42 ")).expect("parse modify-window");
        assert_eq!(value, 42);
    }

    #[test]
    fn parse_modify_window_argument_rejects_negative_values() {
        let error = parse_modify_window_argument(OsStr::new("-1"))
            .expect_err("negative modify-window should fail");
        let rendered = error.to_string();
        assert!(
            rendered.contains("window must be non-negative"),
            "diagnostic missing negativity detail: {rendered}"
        );
    }

    #[test]
    fn parse_modify_window_argument_rejects_invalid_values() {
        let error = parse_modify_window_argument(OsStr::new("abc"))
            .expect_err("non-numeric modify-window should fail");
        let rendered = error.to_string();
        assert!(
            rendered.contains("window must be an unsigned integer"),
            "diagnostic missing numeric detail: {rendered}"
        );
    }

    #[test]
    fn parse_modify_window_argument_rejects_empty_values() {
        let error = parse_modify_window_argument(OsStr::new("   "))
            .expect_err("empty modify-window should fail");
        let rendered = error.to_string();
        assert!(
            rendered.contains("value must not be empty"),
            "diagnostic missing emptiness detail: {rendered}"
        );
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
    fn bind_address_argument_accepts_ipv4_literal() {
        let parsed =
            parse_bind_address_argument(OsStr::new("192.0.2.1")).expect("parse bind address");
        let expected = "192.0.2.1".parse::<IpAddr>().expect("ip literal");
        assert_eq!(parsed.socket().ip(), expected);
        assert_eq!(parsed.raw(), OsStr::new("192.0.2.1"));
    }

    #[test]
    fn bind_address_argument_rejects_empty_value() {
        let error = parse_bind_address_argument(OsStr::new(" ")).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("--address requires a non-empty value")
        );
    }

    #[test]
    fn out_format_argument_accepts_supported_placeholders() {
        let format = parse_out_format(OsStr::new(
            "%f %b %c %l %o %M %B %L %N %p %u %g %U %G %t %i %h %a %m %P %C %%",
        ))
        .expect("parse out-format");
        assert!(!format.tokens.is_empty());
    }

    #[test]
    fn out_format_argument_rejects_unknown_placeholders() {
        let error = parse_out_format(OsStr::new("%z")).unwrap_err();
        assert!(error.to_string().contains("unsupported --out-format"));
    }

    #[test]
    fn out_format_remote_placeholders_preserve_literals_without_context() {
        let temp = tempfile::tempdir().expect("tempdir");
        let src_dir = temp.path().join("src");
        let dst_dir = temp.path().join("dst");
        std::fs::create_dir(&src_dir).expect("create src");
        std::fs::create_dir(&dst_dir).expect("create dst");

        let source = src_dir.join("file.txt");
        std::fs::write(&source, b"payload").expect("write source");
        let destination = dst_dir.join("file.txt");

        let config = ClientConfig::builder()
            .transfer_args([
                source.as_os_str().to_os_string(),
                destination.as_os_str().to_os_string(),
            ])
            .force_event_collection(true)
            .build();

        let outcome =
            run_client_or_fallback::<io::Sink, io::Sink>(config, None, None).expect("run client");
        let summary = match outcome {
            ClientOutcome::Local(summary) => *summary,
            ClientOutcome::Fallback(_) => panic!("unexpected fallback outcome"),
        };

        let event = summary
            .events()
            .iter()
            .find(|event| matches!(event.kind(), ClientEventKind::DataCopied))
            .expect("data event present");

        let mut output = Vec::new();
        parse_out_format(OsStr::new("%h %a %m %P"))
            .expect("parse placeholders")
            .render(event, &OutFormatContext::default(), &mut output)
            .expect("render placeholders");

        let rendered = String::from_utf8(output).expect("utf8");
        assert_eq!(rendered, "%h %a %m %P\n");
    }

    #[test]
    fn out_format_renders_full_checksum_for_files() {
        let temp = tempfile::tempdir().expect("tempdir");
        let src_dir = temp.path().join("src");
        let dst_dir = temp.path().join("dst");
        std::fs::create_dir(&src_dir).expect("create src dir");
        std::fs::create_dir(&dst_dir).expect("create dst dir");

        let source = src_dir.join("file.bin");
        let contents = b"checksum payload";
        std::fs::write(&source, contents).expect("write source");
        let destination = dst_dir.join("file.bin");

        let config = ClientConfig::builder()
            .transfer_args([
                source.as_os_str().to_os_string(),
                destination.as_os_str().to_os_string(),
            ])
            .force_event_collection(true)
            .build();

        let outcome =
            run_client_or_fallback::<io::Sink, io::Sink>(config, None, None).expect("run client");
        let summary = match outcome {
            ClientOutcome::Local(summary) => *summary,
            ClientOutcome::Fallback(_) => panic!("unexpected fallback outcome"),
        };

        let event = summary
            .events()
            .iter()
            .find(|event| event.relative_path().to_string_lossy() == "file.bin")
            .expect("file event present");

        let mut output = Vec::new();
        parse_out_format(OsStr::new("%C"))
            .expect("parse %C")
            .render(event, &OutFormatContext::default(), &mut output)
            .expect("render %C");

        let rendered = String::from_utf8(output).expect("utf8");
        let mut hasher = Md5::new();
        hasher.update(contents);
        let digest = hasher.finalize();
        let expected: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
        assert_eq!(rendered, format!("{expected}\n"));
    }

    #[test]
    fn out_format_renders_full_checksum_for_non_file_entries_as_spaces() {
        let temp = tempfile::tempdir().expect("tempdir");
        let src_dir = temp.path().join("src");
        let dst_dir = temp.path().join("dst");
        std::fs::create_dir_all(src_dir.join("nested")).expect("create source tree");
        std::fs::create_dir(&dst_dir).expect("create destination root");
        std::fs::write(src_dir.join("nested").join("file.txt"), b"contents").expect("write file");

        let source_operand = OsString::from(format!("{}/", src_dir.display()));
        let dest_operand = OsString::from(format!("{}/", dst_dir.display()));

        let config = ClientConfig::builder()
            .transfer_args([source_operand, dest_operand])
            .force_event_collection(true)
            .build();

        let outcome =
            run_client_or_fallback::<io::Sink, io::Sink>(config, None, None).expect("run client");
        let summary = match outcome {
            ClientOutcome::Local(summary) => *summary,
            ClientOutcome::Fallback(_) => panic!("unexpected fallback outcome"),
        };

        let dir_event = summary
            .events()
            .iter()
            .find(|event| matches!(event.kind(), ClientEventKind::DirectoryCreated))
            .expect("directory event present");

        let mut output = Vec::new();
        parse_out_format(OsStr::new("%C"))
            .expect("parse %C")
            .render(dir_event, &OutFormatContext::default(), &mut output)
            .expect("render %C");

        let rendered = String::from_utf8(output).expect("utf8");
        assert_eq!(rendered.len(), 33);
        assert!(rendered[..32].chars().all(|ch| ch == ' '));
        assert_eq!(rendered.as_bytes()[32], b'\n');
    }

    #[test]
    fn out_format_renders_checksum_bytes_for_data_copy_events() {
        let temp = tempfile::tempdir().expect("tempdir");
        let src_dir = temp.path().join("src");
        let dst_dir = temp.path().join("dst");
        std::fs::create_dir(&src_dir).expect("create src");
        std::fs::create_dir(&dst_dir).expect("create dst");

        let source = src_dir.join("payload.bin");
        let contents = b"checksum-bytes";
        std::fs::write(&source, contents).expect("write source");
        let destination = dst_dir.join("payload.bin");

        let config = ClientConfig::builder()
            .transfer_args([
                source.as_os_str().to_os_string(),
                destination.as_os_str().to_os_string(),
            ])
            .times(true)
            .force_event_collection(true)
            .build();

        let outcome =
            run_client_or_fallback::<io::Sink, io::Sink>(config, None, None).expect("run client");
        let summary = match outcome {
            ClientOutcome::Local(summary) => *summary,
            ClientOutcome::Fallback(_) => panic!("unexpected fallback outcome"),
        };

        let event = summary
            .events()
            .iter()
            .find(|event| matches!(event.kind(), ClientEventKind::DataCopied))
            .expect("data copy event present");

        let mut output = Vec::new();
        parse_out_format(OsStr::new("%c"))
            .expect("parse %c")
            .render(event, &OutFormatContext::default(), &mut output)
            .expect("render %c");

        let rendered = String::from_utf8(output).expect("utf8");
        assert_eq!(rendered.trim_end(), contents.len().to_string());
    }

    #[test]
    fn out_format_renders_checksum_bytes_as_zero_when_metadata_reused() {
        let temp = tempfile::tempdir().expect("tempdir");
        let src_dir = temp.path().join("src");
        let dst_dir = temp.path().join("dst");
        std::fs::create_dir(&src_dir).expect("create src");
        std::fs::create_dir(&dst_dir).expect("create dst");

        let source = src_dir.join("unchanged.txt");
        std::fs::write(&source, b"same contents").expect("write source");
        let destination = dst_dir.join("unchanged.txt");

        let build_config = || {
            ClientConfig::builder()
                .transfer_args([
                    source.as_os_str().to_os_string(),
                    destination.as_os_str().to_os_string(),
                ])
                .times(true)
                .force_event_collection(true)
                .build()
        };

        // First run populates the destination.
        let outcome = run_client_or_fallback::<io::Sink, io::Sink>(build_config(), None, None)
            .expect("initial copy");
        if let ClientOutcome::Fallback(_) = outcome {
            panic!("unexpected fallback outcome during initial copy");
        }

        // Second run should reuse metadata and avoid copying data bytes.
        let outcome = run_client_or_fallback::<io::Sink, io::Sink>(build_config(), None, None)
            .expect("re-run");
        let summary = match outcome {
            ClientOutcome::Local(summary) => *summary,
            ClientOutcome::Fallback(_) => panic!("unexpected fallback outcome"),
        };

        let event = summary
            .events()
            .iter()
            .find(|event| matches!(event.kind(), ClientEventKind::MetadataReused))
            .expect("metadata reuse event present");

        let mut output = Vec::new();
        parse_out_format(OsStr::new("%c"))
            .expect("parse %c")
            .render(event, &OutFormatContext::default(), &mut output)
            .expect("render %c");

        let rendered = String::from_utf8(output).expect("utf8");
        assert_eq!(rendered.trim_end(), "0");
    }

    #[cfg(unix)]
    #[test]
    fn out_format_renders_permission_and_identity_placeholders() {
        use std::fs;
        use std::os::unix::fs::MetadataExt;
        use std::os::unix::fs::PermissionsExt;
        use tempfile::tempdir;
        use users::{get_group_by_gid, get_user_by_uid, gid_t, uid_t};

        let temp = tempdir().expect("tempdir");
        let src_dir = temp.path().join("src");
        let dst_dir = temp.path().join("dst");
        fs::create_dir(&src_dir).expect("create src");
        fs::create_dir(&dst_dir).expect("create dst");
        let source = src_dir.join("script.sh");
        fs::write(&source, b"echo ok\n").expect("write source");

        let expected_uid = fs::metadata(&source).expect("source metadata").uid();
        let expected_gid = fs::metadata(&source).expect("source metadata").gid();
        let expected_user = get_user_by_uid(expected_uid as uid_t)
            .map(|user| user.name().to_string_lossy().into_owned())
            .unwrap_or_else(|| expected_uid.to_string());
        let expected_group = get_group_by_gid(expected_gid as gid_t)
            .map(|group| group.name().to_string_lossy().into_owned())
            .unwrap_or_else(|| expected_gid.to_string());

        let mut permissions = fs::metadata(&source)
            .expect("source metadata")
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&source, permissions).expect("set permissions");

        let config = ClientConfig::builder()
            .transfer_args([
                source.as_os_str().to_os_string(),
                dst_dir.as_os_str().to_os_string(),
            ])
            .permissions(true)
            .force_event_collection(true)
            .build();

        let outcome =
            run_client_or_fallback::<io::Sink, io::Sink>(config, None, None).expect("run client");
        let summary = match outcome {
            ClientOutcome::Local(summary) => *summary,
            ClientOutcome::Fallback(_) => panic!("unexpected fallback outcome"),
        };
        let event = summary
            .events()
            .iter()
            .find(|event| {
                event
                    .relative_path()
                    .to_string_lossy()
                    .contains("script.sh")
            })
            .expect("event present");

        let mut output = Vec::new();
        parse_out_format(OsStr::new("%B"))
            .expect("parse out-format")
            .render(event, &OutFormatContext::default(), &mut output)
            .expect("render %B");
        assert_eq!(output, b"rwxr-xr-x\n");

        output.clear();
        parse_out_format(OsStr::new("%p"))
            .expect("parse %p")
            .render(event, &OutFormatContext::default(), &mut output)
            .expect("render %p");
        let expected_pid = format!("{}\n", std::process::id());
        assert_eq!(output, expected_pid.as_bytes());

        output.clear();
        parse_out_format(OsStr::new("%U"))
            .expect("parse %U")
            .render(event, &OutFormatContext::default(), &mut output)
            .expect("render %U");
        assert_eq!(output, format!("{expected_uid}\n").as_bytes());

        output.clear();
        parse_out_format(OsStr::new("%G"))
            .expect("parse %G")
            .render(event, &OutFormatContext::default(), &mut output)
            .expect("render %G");
        assert_eq!(output, format!("{expected_gid}\n").as_bytes());

        output.clear();
        parse_out_format(OsStr::new("%u"))
            .expect("parse %u")
            .render(event, &OutFormatContext::default(), &mut output)
            .expect("render %u");
        assert_eq!(output, format!("{expected_user}\n").as_bytes());

        output.clear();
        parse_out_format(OsStr::new("%g"))
            .expect("parse %g")
            .render(event, &OutFormatContext::default(), &mut output)
            .expect("render %g");
        assert_eq!(output, format!("{expected_group}\n").as_bytes());
    }

    #[test]
    fn out_format_renders_modify_time_placeholder() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("file.txt");
        std::fs::write(&source, b"data").expect("write source");
        let destination = temp.path().join("dest");

        let config = ClientConfig::builder()
            .transfer_args([
                source.as_os_str().to_os_string(),
                destination.as_os_str().to_os_string(),
            ])
            .force_event_collection(true)
            .build();

        let outcome =
            run_client_or_fallback::<io::Sink, io::Sink>(config, None, None).expect("run client");
        let summary = match outcome {
            ClientOutcome::Local(summary) => *summary,
            ClientOutcome::Fallback(_) => panic!("unexpected fallback outcome"),
        };
        let event = summary
            .events()
            .iter()
            .find(|event| event.relative_path().to_string_lossy().contains("file.txt"))
            .expect("event present");

        let format = parse_out_format(OsStr::new("%M")).expect("parse out-format");
        let mut output = Vec::new();
        format
            .render(event, &OutFormatContext::default(), &mut output)
            .expect("render out-format");

        assert!(String::from_utf8_lossy(&output).trim().contains('-'));
    }

    #[test]
    fn out_format_renders_itemized_placeholder_for_new_file() {
        let temp = tempfile::tempdir().expect("tempdir");
        let src_dir = temp.path().join("src");
        let dst_dir = temp.path().join("dst");
        std::fs::create_dir(&src_dir).expect("create src dir");
        std::fs::create_dir(&dst_dir).expect("create dst dir");

        let source = src_dir.join("file.txt");
        std::fs::write(&source, b"content").expect("write source");
        let destination = dst_dir.join("file.txt");

        let config = ClientConfig::builder()
            .transfer_args([
                source.as_os_str().to_os_string(),
                destination.as_os_str().to_os_string(),
            ])
            .force_event_collection(true)
            .build();

        let outcome =
            run_client_or_fallback::<io::Sink, io::Sink>(config, None, None).expect("run client");
        let summary = match outcome {
            ClientOutcome::Local(summary) => *summary,
            ClientOutcome::Fallback(_) => panic!("unexpected fallback outcome"),
        };

        let event = summary
            .events()
            .iter()
            .find(|event| event.relative_path().to_string_lossy() == "file.txt")
            .expect("event present");

        let mut output = Vec::new();
        parse_out_format(OsStr::new("%i"))
            .expect("parse %i")
            .render(event, &OutFormatContext::default(), &mut output)
            .expect("render %i");

        assert_eq!(output, b">f+++++++++\n");
    }

    #[test]
    fn out_format_itemized_placeholder_reports_deletion() {
        let temp = tempfile::tempdir().expect("tempdir");
        let src_dir = temp.path().join("src");
        let dst_dir = temp.path().join("dst");
        std::fs::create_dir(&src_dir).expect("create src dir");
        std::fs::create_dir(&dst_dir).expect("create dst dir");

        let destination_file = dst_dir.join("obsolete.txt");
        std::fs::write(&destination_file, b"old").expect("write obsolete");

        let source_operand = OsString::from(format!("{}/", src_dir.display()));
        let dest_operand = OsString::from(format!("{}/", dst_dir.display()));

        let config = ClientConfig::builder()
            .transfer_args([source_operand, dest_operand])
            .delete(true)
            .force_event_collection(true)
            .build();

        let outcome =
            run_client_or_fallback::<io::Sink, io::Sink>(config, None, None).expect("run client");
        let summary = match outcome {
            ClientOutcome::Local(summary) => *summary,
            ClientOutcome::Fallback(_) => panic!("unexpected fallback outcome"),
        };

        let mut events = summary.events().iter();
        let event = events
            .find(|event| event.relative_path().to_string_lossy() == "obsolete.txt")
            .unwrap_or_else(|| {
                let recorded: Vec<_> = summary
                    .events()
                    .iter()
                    .map(|event| event.relative_path().to_string_lossy().into_owned())
                    .collect();
                panic!("deletion event missing, recorded events: {recorded:?}");
            });

        let mut output = Vec::new();
        parse_out_format(OsStr::new("%i"))
            .expect("parse %i")
            .render(event, &OutFormatContext::default(), &mut output)
            .expect("render %i");

        assert_eq!(output, b"*deleting\n");
    }

    #[test]
    fn parse_args_sets_compress_flag() {
        let parsed = parse_args([
            OsString::from(RSYNC),
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
            OsString::from(RSYNC),
            OsString::from("-z"),
            OsString::from("--no-compress"),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .expect("parse");

        assert!(!parsed.compress);
        assert!(parsed.no_compress);
    }

    #[test]
    fn parse_args_records_compress_level_value() {
        let parsed = parse_args([
            OsString::from(RSYNC),
            OsString::from("--compress-level=5"),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .expect("parse");

        assert_eq!(parsed.compress_level, Some(OsString::from("5")));
    }

    #[test]
    fn parse_args_compress_level_zero_records_disable() {
        let parsed = parse_args([
            OsString::from(RSYNC),
            OsString::from("--compress-level=0"),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .expect("parse");

        assert_eq!(parsed.compress_level, Some(OsString::from("0")));
    }

    #[test]
    fn parse_args_recognises_compress_level_flag() {
        let parsed = parse_args([
            OsString::from(RSYNC),
            OsString::from("--compress-level=5"),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .expect("parse");

        assert!(parsed.compress);
        assert_eq!(parsed.compress_level, Some(OsString::from("5")));
    }

    #[test]
    fn parse_args_compress_level_zero_disables_compress() {
        let parsed = parse_args([
            OsString::from(RSYNC),
            OsString::from("--compress-level=0"),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .expect("parse");

        assert!(!parsed.compress);
        assert_eq!(parsed.compress_level, Some(OsString::from("0")));
    }

    #[test]
    fn parse_compress_level_argument_rejects_invalid_value() {
        let error = parse_compress_level_argument(OsStr::new("fast")).unwrap_err();
        let rendered = error.to_string();
        assert!(rendered.contains("invalid compression level"));
        assert!(rendered.contains("integer"));

        let range_error = parse_compress_level_argument(OsStr::new("12")).unwrap_err();
        let rendered_range = range_error.to_string();
        assert!(rendered_range.contains("outside the supported range"));
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
    fn parse_filter_directive_accepts_risk_keyword_and_shorthand() {
        let risk_keyword =
            parse_filter_directive(OsStr::new("risk backups/**")).expect("keyword risk parses");
        assert_eq!(
            risk_keyword,
            FilterDirective::Rule(FilterRuleSpec::risk("backups/**".to_string()))
        );

        let risk_shorthand =
            parse_filter_directive(OsStr::new("R logs/**")).expect("shorthand risk parses");
        assert_eq!(
            risk_shorthand,
            FilterDirective::Rule(FilterRuleSpec::risk("logs/**".to_string()))
        );
    }

    #[test]
    fn parse_filter_directive_accepts_shorthand_hide_show_and_protect() {
        let protect =
            parse_filter_directive(OsStr::new("P backups/**")).expect("shorthand protect parses");
        assert_eq!(
            protect,
            FilterDirective::Rule(FilterRuleSpec::protect("backups/**".to_string()))
        );

        let hide = parse_filter_directive(OsStr::new("H *.tmp")).expect("shorthand hide parses");
        assert_eq!(
            hide,
            FilterDirective::Rule(FilterRuleSpec::hide("*.tmp".to_string()))
        );

        let show =
            parse_filter_directive(OsStr::new("S public/**")).expect("shorthand show parses");
        assert_eq!(
            show,
            FilterDirective::Rule(FilterRuleSpec::show("public/**".to_string()))
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
    fn parse_filter_directive_accepts_clear_keyword() {
        let keyword = parse_filter_directive(OsStr::new("clear")).expect("keyword parses");
        assert_eq!(keyword, FilterDirective::Clear);

        let uppercase = parse_filter_directive(OsStr::new("  CLEAR  ")).expect("uppercase parses");
        assert_eq!(uppercase, FilterDirective::Clear);
    }

    #[test]
    fn parse_filter_directive_rejects_clear_with_trailing_characters() {
        let error = parse_filter_directive(OsStr::new("! comment"))
            .expect_err("trailing text should error");
        let rendered = error.to_string();
        assert!(rendered.contains("'!' rule has trailing characters: ! comment"));

        let error = parse_filter_directive(OsStr::new("!extra")).expect_err("suffix should error");
        let rendered = error.to_string();
        assert!(rendered.contains("'!' rule has trailing characters: !extra"));
    }

    #[test]
    fn parse_filter_directive_rejects_missing_pattern() {
        let error =
            parse_filter_directive(OsStr::new("+   ")).expect_err("missing pattern should error");
        let rendered = error.to_string();
        assert!(rendered.contains("missing a pattern"));

        let shorthand_error = parse_filter_directive(OsStr::new("P   "))
            .expect_err("shorthand protect requires pattern");
        let rendered = shorthand_error.to_string();
        assert!(rendered.contains("missing a pattern"));
    }

    #[test]
    fn parse_filter_directive_accepts_merge() {
        let directive =
            parse_filter_directive(OsStr::new("merge filters.txt")).expect("merge directive");
        let (options, _) =
            parse_merge_modifiers("", "merge filters.txt", false).expect("modifiers");
        let expected =
            MergeDirective::new(OsString::from("filters.txt"), None).with_options(options);
        assert_eq!(directive, FilterDirective::Merge(expected));
    }

    #[test]
    fn parse_filter_directive_rejects_merge_without_path() {
        let error = parse_filter_directive(OsStr::new("merge "))
            .expect_err("missing merge path should error");
        let rendered = error.to_string();
        assert!(rendered.contains("missing a file path"));
    }

    #[test]
    fn parse_filter_directive_accepts_merge_with_forced_include() {
        let directive =
            parse_filter_directive(OsStr::new("merge,+ rules")).expect("merge,+ should parse");
        let (options, _) = parse_merge_modifiers("+", "merge,+ rules", false).expect("modifiers");
        let expected = MergeDirective::new(OsString::from("rules"), Some(FilterRuleKind::Include))
            .with_options(options);
        assert_eq!(directive, FilterDirective::Merge(expected));
    }

    #[test]
    fn parse_filter_directive_accepts_merge_with_forced_exclude() {
        let directive =
            parse_filter_directive(OsStr::new("merge,- rules")).expect("merge,- should parse");
        let (options, _) = parse_merge_modifiers("-", "merge,- rules", false).expect("modifiers");
        let expected = MergeDirective::new(OsString::from("rules"), Some(FilterRuleKind::Exclude))
            .with_options(options);
        assert_eq!(directive, FilterDirective::Merge(expected));
    }

    #[test]
    fn parse_filter_directive_accepts_merge_with_cvs_alias() {
        let directive =
            parse_filter_directive(OsStr::new("merge,C")).expect("merge,C should parse");
        let (options, _) = parse_merge_modifiers("C", "merge,C", false).expect("modifiers");
        let expected =
            MergeDirective::new(OsString::from(".cvsignore"), Some(FilterRuleKind::Exclude))
                .with_options(options);
        assert_eq!(directive, FilterDirective::Merge(expected));
    }

    #[test]
    fn parse_filter_directive_accepts_short_merge() {
        let directive =
            parse_filter_directive(OsStr::new(". per-dir")).expect("short merge directive parses");
        let (options, _) = parse_merge_modifiers("", ". per-dir", false).expect("modifiers");
        let expected = MergeDirective::new(OsString::from("per-dir"), None).with_options(options);
        assert_eq!(directive, FilterDirective::Merge(expected));
    }

    #[test]
    fn parse_filter_directive_accepts_short_merge_with_cvs_alias() {
        let directive = parse_filter_directive(OsStr::new(".C"))
            .expect("short merge directive with 'C' parses");
        let (options, _) = parse_merge_modifiers("C", ".C", false).expect("modifiers");
        let expected =
            MergeDirective::new(OsString::from(".cvsignore"), Some(FilterRuleKind::Exclude))
                .with_options(options);
        assert_eq!(directive, FilterDirective::Merge(expected));
    }

    #[test]
    fn parse_filter_directive_accepts_merge_sender_modifier() {
        let directive = parse_filter_directive(OsStr::new("merge,s rules"))
            .expect("merge directive with 's' parses");
        let expected_options = DirMergeOptions::default()
            .allow_list_clearing(true)
            .sender_modifier();
        let expected =
            MergeDirective::new(OsString::from("rules"), None).with_options(expected_options);
        assert_eq!(directive, FilterDirective::Merge(expected));
    }

    #[test]
    fn parse_filter_directive_accepts_merge_anchor_and_whitespace_modifiers() {
        let directive = parse_filter_directive(OsStr::new("merge,/w patterns"))
            .expect("merge directive with '/' and 'w' parses");
        let expected_options = DirMergeOptions::default()
            .allow_list_clearing(true)
            .anchor_root(true)
            .use_whitespace()
            .allow_comments(false);
        let expected =
            MergeDirective::new(OsString::from("patterns"), None).with_options(expected_options);
        assert_eq!(directive, FilterDirective::Merge(expected));
    }

    #[test]
    fn parse_filter_directive_rejects_merge_with_unknown_modifier() {
        let error = parse_filter_directive(OsStr::new("merge,x rules"))
            .expect_err("merge with unsupported modifier should error");
        let rendered = error.to_string();
        assert!(rendered.contains("uses unsupported modifier"));
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
    fn parse_filter_directive_accepts_short_dir_merge() {
        let directive = parse_filter_directive(OsStr::new(": rules"))
            .expect("short dir-merge directive parses");

        let FilterDirective::Rule(rule) = directive else {
            panic!("expected dir-merge rule");
        };

        assert_eq!(rule.pattern(), "rules");
        let options = rule
            .dir_merge_options()
            .expect("dir-merge rule returns options");
        assert!(options.inherit_rules());
        assert!(!options.excludes_self());
    }

    #[test]
    fn parse_filter_directive_accepts_short_dir_merge_with_exclude_modifier() {
        let directive = parse_filter_directive(OsStr::new(":- per-dir"))
            .expect("short dir-merge with '-' modifier parses");

        let FilterDirective::Rule(rule) = directive else {
            panic!("expected dir-merge rule");
        };

        assert_eq!(rule.pattern(), "per-dir");
        let options = rule
            .dir_merge_options()
            .expect("dir-merge rule returns options");
        assert_eq!(options.enforced_kind(), Some(DirMergeEnforcedKind::Exclude));
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
    fn collect_filter_arguments_merges_shortcuts_with_filters() {
        use std::ffi::OsString;

        let filters = vec![OsString::from("+ foo"), OsString::from("- bar")];
        let filter_indices = vec![5_usize, 9_usize];
        let rsync_indices = vec![1_usize, 7_usize];

        let merged = collect_filter_arguments(&filters, &filter_indices, &rsync_indices);

        assert_eq!(
            merged,
            vec![
                OsString::from("dir-merge /.rsync-filter"),
                OsString::from("exclude .rsync-filter"),
                OsString::from("+ foo"),
                OsString::from("dir-merge .rsync-filter"),
                OsString::from("- bar"),
            ]
        );
    }

    #[test]
    fn collect_filter_arguments_handles_shortcuts_without_filters() {
        use std::ffi::OsString;

        let filters: Vec<OsString> = Vec::new();
        let filter_indices: Vec<usize> = Vec::new();
        let merged = collect_filter_arguments(&filters, &filter_indices, &[2_usize, 4_usize]);

        assert_eq!(
            merged,
            vec![
                OsString::from("dir-merge /.rsync-filter"),
                OsString::from("exclude .rsync-filter"),
                OsString::from("dir-merge .rsync-filter"),
            ]
        );
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
            OsString::from(RSYNC),
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
            OsString::from(RSYNC),
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
            OsString::from(RSYNC),
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
            OsString::from(RSYNC),
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
            OsString::from(RSYNC),
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
            OsString::from(RSYNC),
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
            OsString::from(RSYNC),
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
            OsString::from(RSYNC),
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
            OsString::from(RSYNC),
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
    fn transfer_request_with_cvs_exclude_skips_default_patterns() {
        use tempfile::tempdir;

        let _env_lock = ENV_LOCK.lock().expect("env lock");
        let _home_guard = EnvGuard::set("HOME", OsStr::new(""));
        let _cvs_guard = EnvGuard::set("CVSIGNORE", OsStr::new(""));

        let tmp = tempdir().expect("tempdir");
        let source_root = tmp.path().join("source");
        let dest_root = tmp.path().join("dest");
        std::fs::create_dir_all(&source_root).expect("create source root");
        std::fs::create_dir_all(&dest_root).expect("create dest root");
        std::fs::write(source_root.join("keep.txt"), b"keep").expect("write keep");
        std::fs::write(source_root.join("core"), b"core").expect("write core");
        let git_dir = source_root.join(".git");
        std::fs::create_dir_all(&git_dir).expect("create git dir");
        std::fs::write(git_dir.join("config"), b"git").expect("write git config");

        let (code, stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
            OsString::from("--cvs-exclude"),
            source_root.clone().into_os_string(),
            dest_root.clone().into_os_string(),
        ]);

        assert_eq!(code, 0);
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());

        let copied_root = dest_root.join("source");
        assert!(copied_root.join("keep.txt").exists());
        assert!(!copied_root.join("core").exists());
        assert!(!copied_root.join(".git").exists());
    }

    #[test]
    fn transfer_request_with_cvs_exclude_respects_cvsignore_files() {
        use tempfile::tempdir;

        let _env_lock = ENV_LOCK.lock().expect("env lock");
        let _home_guard = EnvGuard::set("HOME", OsStr::new(""));
        let _cvs_guard = EnvGuard::set("CVSIGNORE", OsStr::new(""));

        let tmp = tempdir().expect("tempdir");
        let source_root = tmp.path().join("source");
        let dest_root = tmp.path().join("dest");
        std::fs::create_dir_all(&source_root).expect("create source root");
        std::fs::create_dir_all(&dest_root).expect("create dest root");
        std::fs::write(source_root.join("keep.txt"), b"keep").expect("write keep");
        std::fs::write(source_root.join("skip.log"), b"skip").expect("write skip");
        std::fs::write(source_root.join(".cvsignore"), b"skip.log\n").expect("write cvsignore");

        let (code, stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
            OsString::from("--cvs-exclude"),
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
    fn transfer_request_with_cvs_exclude_respects_cvsignore_env() {
        use tempfile::tempdir;

        let _env_lock = ENV_LOCK.lock().expect("env lock");
        let _home_guard = EnvGuard::set("HOME", OsStr::new(""));
        let _cvs_guard = EnvGuard::set("CVSIGNORE", OsStr::new("*.tmp"));

        let tmp = tempdir().expect("tempdir");
        let source_root = tmp.path().join("source");
        let dest_root = tmp.path().join("dest");
        std::fs::create_dir_all(&source_root).expect("create source root");
        std::fs::create_dir_all(&dest_root).expect("create dest root");
        std::fs::write(source_root.join("keep.txt"), b"keep").expect("write keep");
        std::fs::write(source_root.join("skip.tmp"), b"skip").expect("write skip");

        let (code, stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
            OsString::from("--cvs-exclude"),
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
            FilterRuleKind::Clear => None,
            FilterRuleKind::Protect => Some(EngineFilterRule::protect(rule.pattern())),
            FilterRuleKind::Risk => Some(EngineFilterRule::risk(rule.pattern())),
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
            OsString::from(RSYNC),
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
            OsString::from(RSYNC),
            OsString::from("--exclude-from"),
            OsString::from("missing.txt"),
            OsString::from("src"),
            OsString::from("dst"),
        ]);

        assert_eq!(code, 1);
        assert!(stdout.is_empty());
        let rendered = String::from_utf8(stderr).expect("diagnostic utf8");
        assert!(rendered.contains("failed to read filter file 'missing.txt'"));
        assert_contains_client_trailer(&rendered);
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
        let mut visited = HashSet::new();
        let directive = MergeDirective::new(OsString::from("outer.rules"), None)
            .with_options(DirMergeOptions::default().allow_list_clearing(true));
        super::apply_merge_directive(directive, temp.path(), &mut rules, &mut visited)
            .expect("merge succeeds");

        assert!(visited.is_empty());
        let patterns: Vec<_> = rules
            .iter()
            .map(|rule| rule.pattern().to_string())
            .collect();
        assert_eq!(patterns, vec!["outer", "child", "grand"]);
    }

    #[test]
    fn apply_merge_directive_respects_forced_include() {
        use tempfile::tempdir;

        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("filters.rules");
        std::fs::write(&path, b"alpha\n!\nbeta\n").expect("write filters");

        let mut rules = vec![FilterRuleSpec::exclude("existing".to_string())];
        let mut visited = HashSet::new();
        let directive = MergeDirective::new(path.into_os_string(), Some(FilterRuleKind::Include))
            .with_options(
                DirMergeOptions::default()
                    .with_enforced_kind(Some(DirMergeEnforcedKind::Include))
                    .allow_list_clearing(true),
            );
        super::apply_merge_directive(directive, temp.path(), &mut rules, &mut visited)
            .expect("merge succeeds");

        assert!(visited.is_empty());
        let patterns: Vec<_> = rules
            .iter()
            .map(|rule| rule.pattern().to_string())
            .collect();
        assert_eq!(patterns, vec!["beta"]);
    }

    #[test]
    fn merge_directive_options_inherit_parent_configuration() {
        let base = DirMergeOptions::default()
            .inherit(false)
            .exclude_filter_file(true)
            .allow_list_clearing(false)
            .anchor_root(true)
            .allow_comments(false)
            .with_side_overrides(Some(true), Some(false));

        let directive = MergeDirective::new(OsString::from("nested.rules"), None);
        let merged = super::merge_directive_options(&base, &directive);

        assert!(!merged.inherit_rules());
        assert!(merged.excludes_self());
        assert!(!merged.list_clear_allowed());
        assert!(merged.anchor_root_enabled());
        assert!(!merged.allows_comments());
        assert_eq!(merged.sender_side_override(), Some(true));
        assert_eq!(merged.receiver_side_override(), Some(false));
    }

    #[test]
    fn merge_directive_options_respect_child_overrides() {
        let base = DirMergeOptions::default()
            .inherit(false)
            .with_side_overrides(Some(true), Some(false));

        let child_options = DirMergeOptions::default()
            .inherit(true)
            .allow_list_clearing(true)
            .with_enforced_kind(Some(DirMergeEnforcedKind::Include))
            .use_whitespace()
            .with_side_overrides(Some(false), Some(true));
        let directive =
            MergeDirective::new(OsString::from("nested.rules"), None).with_options(child_options);

        let merged = super::merge_directive_options(&base, &directive);

        assert_eq!(merged.enforced_kind(), Some(DirMergeEnforcedKind::Include));
        assert!(merged.uses_whitespace());
        assert_eq!(merged.sender_side_override(), Some(false));
        assert_eq!(merged.receiver_side_override(), Some(true));
    }

    #[test]
    fn process_merge_directive_applies_parent_overrides_to_nested_merges() {
        use tempfile::tempdir;

        let temp = tempdir().expect("tempdir");
        let nested = temp.path().join("nested.rules");
        std::fs::write(&nested, b"+ file\n").expect("write nested");

        let options = DirMergeOptions::default()
            .sender_modifier()
            .inherit(false)
            .exclude_filter_file(true)
            .allow_list_clearing(false);

        let mut rules = Vec::new();
        let mut visited = HashSet::new();
        super::process_merge_directive(
            "merge nested.rules",
            &options,
            temp.path(),
            "parent.rules",
            &mut rules,
            &mut visited,
        )
        .expect("merge succeeds");

        assert!(visited.is_empty());
        let include_rule = rules
            .iter()
            .find(|rule| rule.pattern() == "file")
            .expect("include rule present");
        assert!(include_rule.applies_to_sender());
        assert!(!include_rule.applies_to_receiver());
        assert!(rules.iter().any(|rule| rule.pattern() == "nested.rules"));
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
            OsString::from(RSYNC),
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
    fn transfer_request_with_omit_dir_times_skips_directory_timestamp() {
        use filetime::{FileTime, set_file_times};
        use tempfile::tempdir;

        let tmp = tempdir().expect("tempdir");
        let source_root = tmp.path().join("source");
        let dest_root = tmp.path().join("dest");
        let source_dir = source_root.join("nested");
        let source_file = source_dir.join("file.txt");

        std::fs::create_dir_all(&source_dir).expect("create source dir");
        std::fs::write(&source_file, b"payload").expect("write file");

        let dir_mtime = FileTime::from_unix_time(1_700_200_000, 0);
        set_file_times(&source_dir, dir_mtime, dir_mtime).expect("set dir times");
        set_file_times(&source_file, dir_mtime, dir_mtime).expect("set file times");

        let (code, stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
            OsString::from("-a"),
            OsString::from("--omit-dir-times"),
            source_root.clone().into_os_string(),
            dest_root.clone().into_os_string(),
        ]);

        assert_eq!(code, 0);
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());

        let dest_dir = dest_root.join("nested");
        let dest_file = dest_dir.join("file.txt");

        let dir_metadata = std::fs::metadata(&dest_dir).expect("dest dir metadata");
        let file_metadata = std::fs::metadata(&dest_file).expect("dest file metadata");
        let dest_dir_mtime = FileTime::from_last_modification_time(&dir_metadata);
        let dest_file_mtime = FileTime::from_last_modification_time(&file_metadata);

        assert_ne!(dest_dir_mtime, dir_mtime);
        assert_eq!(dest_file_mtime, dir_mtime);
    }

    #[cfg(unix)]
    #[test]
    fn transfer_request_with_omit_link_times_skips_symlink_timestamp() {
        use filetime::{FileTime, set_file_times, set_symlink_file_times};
        use std::fs;
        use std::os::unix::fs::symlink;
        use tempfile::tempdir;

        let tmp = tempdir().expect("tempdir");
        let source_root = tmp.path().join("source");
        let dest_root = tmp.path().join("dest");
        fs::create_dir_all(&source_root).expect("create source dir");

        let source_target = source_root.join("target.txt");
        let source_link = source_root.join("link.txt");
        fs::write(&source_target, b"payload").expect("write source target");
        symlink("target.txt", &source_link).expect("create symlink");

        let timestamp = FileTime::from_unix_time(1_700_300_000, 0);
        set_file_times(&source_target, timestamp, timestamp).expect("set file times");
        set_symlink_file_times(&source_link, timestamp, timestamp).expect("set symlink times");

        let (code, stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
            OsString::from("-a"),
            OsString::from("--omit-link-times"),
            source_root.clone().into_os_string(),
            dest_root.clone().into_os_string(),
        ]);

        assert_eq!(code, 0);
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());

        let dest_target = dest_root.join("target.txt");
        let dest_link = dest_root.join("link.txt");

        let dest_target_metadata = fs::metadata(&dest_target).expect("dest target metadata");
        let dest_link_metadata = fs::symlink_metadata(&dest_link).expect("dest link metadata");
        let dest_target_mtime = FileTime::from_last_modification_time(&dest_target_metadata);
        let dest_link_mtime = FileTime::from_last_modification_time(&dest_link_metadata);

        assert_eq!(dest_target_mtime, timestamp);
        assert_ne!(dest_link_mtime, timestamp);
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
            OsString::from(RSYNC),
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
            OsString::from(RSYNC),
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
            OsString::from(RSYNC),
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
            run_with_args([OsString::from(RSYNC), OsString::from("--bwlimit=oops")]);

        assert_eq!(code, 1);
        assert!(stdout.is_empty());
        let rendered = String::from_utf8(stderr).expect("diagnostic is valid UTF-8");
        assert!(rendered.contains("--bwlimit=oops is invalid"));
        assert_contains_client_trailer(&rendered);
    }

    #[test]
    fn bwlimit_rejects_small_fractional_values() {
        let (code, stdout, stderr) =
            run_with_args([OsString::from(RSYNC), OsString::from("--bwlimit=0.4")]);

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
        assert_eq!(limit.bytes_per_second().get(), 10_000);
    }

    #[test]
    fn bwlimit_accepts_burst_component() {
        let limit = parse_bandwidth_limit(OsStr::new("4M:32K"))
            .expect("parse succeeds")
            .expect("limit available");
        assert_eq!(limit.bytes_per_second().get(), 4_194_304);
        assert_eq!(
            limit.burst_bytes().map(std::num::NonZeroU64::get),
            Some(32 * 1024)
        );
    }

    #[test]
    fn bwlimit_zero_disables_limit() {
        let limit = parse_bandwidth_limit(OsStr::new("0")).expect("parse succeeds");
        assert!(limit.is_none());
    }

    #[test]
    fn bwlimit_rejects_whitespace_wrapped_argument() {
        let error = parse_bandwidth_limit(OsStr::new(" 1M \t"))
            .expect_err("whitespace-wrapped bwlimit should fail");
        let rendered = format!("{error}");
        assert!(rendered.contains("--bwlimit= 1M \t is invalid"));
    }

    #[test]
    fn bwlimit_accepts_leading_plus_sign() {
        let limit = parse_bandwidth_limit(OsStr::new("+2M"))
            .expect("parse succeeds")
            .expect("limit available");
        assert_eq!(limit.bytes_per_second().get(), 2_097_152);
    }

    #[test]
    fn bwlimit_rejects_negative_values() {
        let (code, stdout, stderr) =
            run_with_args([OsString::from(RSYNC), OsString::from("--bwlimit=-1")]);

        assert_eq!(code, 1);
        assert!(stdout.is_empty());
        let rendered = String::from_utf8(stderr).expect("diagnostic is valid UTF-8");
        assert!(rendered.contains("--bwlimit=-1 is invalid"));
    }

    #[test]
    fn compress_level_invalid_value_reports_error() {
        let (code, stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
            OsString::from("--compress-level=fast"),
        ]);

        assert_eq!(code, 1);
        assert!(stdout.is_empty());
        let rendered = String::from_utf8(stderr).expect("diagnostic is valid UTF-8");
        assert!(rendered.contains("--compress-level=fast is invalid"));
    }

    #[test]
    fn compress_level_out_of_range_reports_error() {
        let (code, stdout, stderr) =
            run_with_args([OsString::from(RSYNC), OsString::from("--compress-level=12")]);

        assert_eq!(code, 1);
        assert!(stdout.is_empty());
        let rendered = String::from_utf8(stderr).expect("diagnostic is valid UTF-8");
        assert!(rendered.contains("--compress-level=12 must be between 0 and 9"));
    }

    #[test]
    fn skip_compress_invalid_reports_error() {
        let (code, stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
            OsString::from("--skip-compress=mp[]"),
        ]);

        assert_eq!(code, 1);
        assert!(stdout.is_empty());
        let rendered = String::from_utf8(stderr).expect("diagnostic is valid UTF-8");
        assert!(rendered.contains("invalid --skip-compress specification"));
        assert!(rendered.contains("empty character class"));
    }

    #[cfg(windows)]
    #[test]
    fn operand_detection_ignores_windows_drive_and_device_prefixes() {
        use std::ffi::OsStr;

        assert!(!operand_is_remote(OsStr::new("C:\\temp\\file.txt")));
        assert!(!operand_is_remote(OsStr::new("\\\\?\\C:\\temp\\file.txt")));
        assert!(!operand_is_remote(OsStr::new("\\\\.\\C:\\pipe\\name")));
    }

    #[test]
    fn remote_operand_reports_launch_failure_when_fallback_missing() {
        let _env_lock = ENV_LOCK.lock().expect("env lock");
        let _rsh_guard = clear_rsync_rsh();
        let missing = OsString::from("rsync-missing-binary");
        let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, missing.as_os_str());

        let (code, stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
            OsString::from("remote::module"),
            OsString::from("dest"),
        ]);

        assert_eq!(code, 1);
        assert!(stdout.is_empty());

        let rendered = String::from_utf8(stderr).expect("diagnostic is valid UTF-8");
        assert!(rendered.contains("failed to launch fallback rsync binary"));
        assert_contains_client_trailer(&rendered);
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
        let (code, stdout, stderr) = run_with_args([OsString::from(RSYNC), OsString::from(url)]);

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
            OsString::from(RSYNC),
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
    fn remote_daemon_listing_with_rsync_path_does_not_spawn_fallback() {
        use tempfile::tempdir;

        let (addr, handle) =
            spawn_stub_daemon(vec!["@RSYNCD: OK\n", "module\n", "@RSYNCD: EXIT\n"]);

        let url = format!("rsync://{}:{}/", addr.ip(), addr.port());

        let _env_lock = ENV_LOCK.lock().expect("env lock");
        let temp = tempdir().expect("tempdir");
        let script_path = temp.path().join("fallback.sh");
        let marker_path = temp.path().join("marker.txt");

        let script = r#"#!/bin/sh
printf "%s\n" "invoked" > "$MARKER_FILE"
exit 99
"#;
        write_executable_script(&script_path, script);

        let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
        let _marker_guard = EnvGuard::set("MARKER_FILE", marker_path.as_os_str());

        let (code, stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
            OsString::from("--rsync-path=/opt/custom/rsync"),
            OsString::from(url.clone()),
        ]);

        assert_eq!(code, 0);
        assert!(stderr.is_empty());
        let rendered = String::from_utf8(stdout).expect("output is UTF-8");
        assert!(rendered.contains("module"));
        assert!(!marker_path.exists());

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
            OsString::from(RSYNC),
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
        let (code, stdout, stderr) = run_with_args([OsString::from(RSYNC), OsString::from(url)]);

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
        let (code, stdout, stderr) = run_with_args([OsString::from(RSYNC), OsString::from(url)]);

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
        let (code, stdout, stderr) = run_with_args([OsString::from(RSYNC), OsString::from(url)]);

        assert_eq!(code, 0);
        assert!(stderr.is_empty());

        let rendered = String::from_utf8(stdout).expect("output is UTF-8");
        assert!(rendered.contains("module\tWith comment"));

        handle.join().expect("server thread");
    }

    #[test]
    fn module_list_uses_connect_program_option() {
        let _env_lock = ENV_LOCK.lock().expect("env lock");

        let temp = tempfile::tempdir().expect("tempdir");
        let script_path = temp.path().join("connect-program.sh");
        let script = r#"#!/bin/sh
set -eu
printf "@RSYNCD: 31.0\n"
read _greeting
printf "@RSYNCD: OK\n"
read _request
printf "example\tvia connect program\n@RSYNCD: EXIT\n"
"#;
        write_executable_script(&script_path, script);

        let (code, stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
            OsString::from(format!("--connect-program={}", script_path.display())),
            OsString::from("rsync://example/"),
        ]);

        assert_eq!(code, 0);
        assert!(stderr.is_empty());
        let rendered = String::from_utf8(stdout).expect("module listing is UTF-8");
        assert!(rendered.contains("example\tvia connect program"));
    }

    #[test]
    fn module_list_username_prefix_legacy_syntax_is_accepted() {
        let (addr, handle) =
            spawn_stub_daemon(vec!["@RSYNCD: OK\n", "module\n", "@RSYNCD: EXIT\n"]);

        let url = format!("user@[{}]:{}::", addr.ip(), addr.port());
        let (code, stdout, stderr) = run_with_args([OsString::from(RSYNC), OsString::from(url)]);

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
            OsString::from(RSYNC),
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
            OsString::from(RSYNC),
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
            OsString::from(RSYNC),
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
            OsString::from(RSYNC),
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
    fn password_file_dash_conflicts_with_files_from_dash() {
        let (code, stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
            OsString::from("--files-from=-"),
            OsString::from("--password-file=-"),
            OsString::from("/tmp/dest"),
        ]);

        assert_eq!(code, 1);
        assert!(stdout.is_empty());
        let rendered = String::from_utf8(stderr).expect("diagnostic is UTF-8");
        assert!(rendered.contains("--password-file=- cannot be combined with --files-from=-"));
    }

    #[test]
    fn protocol_option_requires_daemon_operands() {
        use tempfile::tempdir;

        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source.txt");
        let destination = temp.path().join("dest.txt");
        std::fs::write(&source, b"data").expect("write source");

        let (code, stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
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
            OsString::from(RSYNC),
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
            OsString::from(RSYNC),
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
        let command = clap_command(rsync_core::version::PROGRAM_NAME);
        let error = command
            .try_get_matches_from(vec!["rsync", "--version=extra"])
            .unwrap_err();

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let status = run(
            [OsString::from(RSYNC), OsString::from("--version=extra")],
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
            OsString::from(RSYNC),
            OsString::from("--delete"),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .expect("parse succeeds");

        assert_eq!(parsed.delete_mode, DeleteMode::During);
        assert!(!parsed.delete_excluded);
    }

    #[test]
    fn delete_alias_del_is_parsed() {
        let parsed = super::parse_args([
            OsString::from(RSYNC),
            OsString::from("--del"),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .expect("parse succeeds");

        assert_eq!(parsed.delete_mode, DeleteMode::During);
        assert!(!parsed.delete_excluded);
    }

    #[test]
    fn delete_after_flag_is_parsed() {
        let parsed = super::parse_args([
            OsString::from(RSYNC),
            OsString::from("--delete-after"),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .expect("parse succeeds");

        assert_eq!(parsed.delete_mode, DeleteMode::After);
        assert!(!parsed.delete_excluded);
    }

    #[test]
    fn delete_before_flag_is_parsed() {
        let parsed = super::parse_args([
            OsString::from(RSYNC),
            OsString::from("--delete-before"),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .expect("parse succeeds");

        assert_eq!(parsed.delete_mode, DeleteMode::Before);
        assert!(!parsed.delete_excluded);
    }

    #[test]
    fn delete_during_flag_is_parsed() {
        let parsed = super::parse_args([
            OsString::from(RSYNC),
            OsString::from("--delete-during"),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .expect("parse succeeds");

        assert_eq!(parsed.delete_mode, DeleteMode::During);
        assert!(!parsed.delete_excluded);
    }

    #[test]
    fn delete_delay_flag_is_parsed() {
        let parsed = super::parse_args([
            OsString::from(RSYNC),
            OsString::from("--delete-delay"),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .expect("parse succeeds");

        assert_eq!(parsed.delete_mode, DeleteMode::Delay);
        assert!(!parsed.delete_excluded);
    }

    #[test]
    fn delete_excluded_flag_implies_delete() {
        let parsed = super::parse_args([
            OsString::from(RSYNC),
            OsString::from("--delete-excluded"),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .expect("parse succeeds");

        assert!(parsed.delete_mode.is_enabled());
        assert!(parsed.delete_excluded);
    }

    #[test]
    fn archive_flag_is_parsed() {
        let parsed = super::parse_args([
            OsString::from(RSYNC),
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
            OsString::from(RSYNC),
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
            OsString::from(RSYNC),
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
            OsString::from(RSYNC),
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
            OsString::from(RSYNC),
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
            OsString::from(RSYNC),
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
    fn compress_level_flag_is_accepted_for_local_copies() {
        use tempfile::tempdir;

        let tmp = tempdir().expect("tempdir");
        let source = tmp.path().join("compress.txt");
        let destination = tmp.path().join("compress.out");
        std::fs::write(&source, b"payload").expect("write source");

        let (code, stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
            OsString::from("--compress-level=6"),
            source.clone().into_os_string(),
            destination.clone().into_os_string(),
        ]);

        assert_eq!(code, 0);
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());
        assert_eq!(
            std::fs::read(destination).expect("read destination"),
            b"payload"
        );
    }

    #[test]
    fn compress_level_zero_disables_local_compression() {
        use tempfile::tempdir;

        let tmp = tempdir().expect("tempdir");
        let source = tmp.path().join("compress.txt");
        let destination = tmp.path().join("compress.out");
        std::fs::write(&source, b"payload").expect("write source");

        let (code, stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
            OsString::from("--compress-level=0"),
            OsString::from("-z"),
            source.clone().into_os_string(),
            destination.clone().into_os_string(),
        ]);

        assert_eq!(code, 0);
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());
        assert_eq!(
            std::fs::read(destination).expect("read destination"),
            b"payload"
        );
    }

    #[cfg(unix)]
    #[test]
    fn server_mode_invokes_fallback_binary() {
        use std::fs;
        use std::io;
        use std::os::unix::fs::PermissionsExt;
        use tempfile::tempdir;

        let _env_lock = ENV_LOCK.lock().expect("env lock");

        let temp = tempdir().expect("tempdir");
        let script_path = temp.path().join("server.sh");
        let marker_path = temp.path().join("marker.txt");

        fs::write(
            &script_path,
            r#"#!/bin/sh
set -eu
: "${SERVER_MARKER:?}"
printf 'invoked' > "$SERVER_MARKER"
exit 37
"#,
        )
        .expect("write script");

        let mut perms = fs::metadata(&script_path)
            .expect("script metadata")
            .permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&script_path, perms).expect("set script perms");

        let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
        let _marker_guard = EnvGuard::set("SERVER_MARKER", marker_path.as_os_str());

        let mut stdout = io::sink();
        let mut stderr = io::sink();
        let exit_code = run(
            [
                OsString::from(RSYNC),
                OsString::from("--server"),
                OsString::from("--sender"),
                OsString::from("."),
                OsString::from("dest"),
            ],
            &mut stdout,
            &mut stderr,
        );

        assert_eq!(exit_code, 37);
        assert_eq!(fs::read(&marker_path).expect("read marker"), b"invoked");
    }

    #[cfg(unix)]
    #[test]
    fn server_mode_forwards_output_to_provided_handles() {
        use tempfile::tempdir;

        let _env_lock = ENV_LOCK.lock().expect("env lock");

        let temp = tempdir().expect("tempdir");
        let script_path = temp.path().join("server_output.sh");

        let script = r#"#!/bin/sh
set -eu
printf 'fallback stdout line\n'
printf 'fallback stderr line\n' >&2
exit 0
"#;
        write_executable_script(&script_path, script);

        let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let exit_code = run(
            [
                OsString::from(RSYNC),
                OsString::from("--server"),
                OsString::from("--sender"),
                OsString::from("."),
                OsString::from("dest"),
            ],
            &mut stdout,
            &mut stderr,
        );

        assert_eq!(exit_code, 0);
        assert!(stdout.ends_with(b"fallback stdout line\n"));
        assert!(stderr.ends_with(b"fallback stderr line\n"));
    }

    #[cfg(unix)]
    #[test]
    fn server_mode_reports_disabled_fallback_override() {
        use std::io;

        let _env_lock = ENV_LOCK.lock().expect("env lock");
        let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("no"));

        let mut stdout = io::sink();
        let mut stderr = Vec::new();
        let exit_code = run(
            [
                OsString::from(RSYNC),
                OsString::from("--server"),
                OsString::from("--sender"),
                OsString::from("."),
                OsString::from("dest"),
            ],
            &mut stdout,
            &mut stderr,
        );

        assert_eq!(exit_code, 1);
        let stderr_text = String::from_utf8(stderr).expect("stderr utf8");
        assert!(stderr_text.contains(&format!(
            "remote server mode is unavailable because {env} is disabled",
            env = CLIENT_FALLBACK_ENV,
        )));
    }

    #[cfg(unix)]
    #[test]
    fn remote_operands_invoke_fallback_binary() {
        use tempfile::tempdir;

        let _env_lock = ENV_LOCK.lock().expect("env lock");
        let _rsh_guard = clear_rsync_rsh();
        let temp = tempdir().expect("tempdir");
        let script_path = temp.path().join("fallback.sh");
        let args_path = temp.path().join("args.txt");
        std::fs::File::create(&args_path).expect("create args file");

        let script = r#"#!/bin/sh
printf "%s\n" "$@" > "$ARGS_FILE"
echo fallback-stdout
echo fallback-stderr >&2
exit 7
"#;
        write_executable_script(&script_path, script);

        let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
        let _args_guard = EnvGuard::set("ARGS_FILE", args_path.as_os_str());

        let (code, stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
            OsString::from("--dry-run"),
            OsString::from("remote::module/path"),
            OsString::from("dest"),
        ]);

        assert_eq!(code, 7);
        assert_eq!(
            String::from_utf8(stdout).expect("stdout UTF-8"),
            "fallback-stdout\n"
        );
        assert_eq!(
            String::from_utf8(stderr).expect("stderr UTF-8"),
            "fallback-stderr\n"
        );

        let recorded = std::fs::read_to_string(&args_path).expect("read args file");
        assert!(recorded.contains("--dry-run"));
        assert!(recorded.contains("remote::module/path"));
        assert!(recorded.contains("dest"));
    }

    #[cfg(unix)]
    #[test]
    fn remote_fallback_includes_ipv4_flag() {
        use tempfile::tempdir;

        let _env_lock = ENV_LOCK.lock().expect("env lock");
        let _rsh_guard = clear_rsync_rsh();
        let temp = tempdir().expect("tempdir");
        let script_path = temp.path().join("fallback.sh");
        let args_path = temp.path().join("args.txt");
        std::fs::File::create(&args_path).expect("create args file");

        let script = r#"#!/bin/sh
printf "%s\n" "$@" > "$ARGS_FILE"
exit 0
"#;
        write_executable_script(&script_path, script);

        let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
        let _args_guard = EnvGuard::set("ARGS_FILE", args_path.as_os_str());

        let (code, stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
            OsString::from("--ipv4"),
            OsString::from("remote::module"),
            OsString::from("dest"),
        ]);

        assert_eq!(code, 0);
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());

        let recorded = std::fs::read_to_string(&args_path).expect("read args file");
        assert!(recorded.contains("--ipv4"));
        assert!(!recorded.contains("--ipv6"));
    }

    #[cfg(unix)]
    #[test]
    fn remote_fallback_includes_ipv6_flag() {
        use tempfile::tempdir;

        let _env_lock = ENV_LOCK.lock().expect("env lock");
        let _rsh_guard = clear_rsync_rsh();
        let temp = tempdir().expect("tempdir");
        let script_path = temp.path().join("fallback.sh");
        let args_path = temp.path().join("args.txt");
        std::fs::File::create(&args_path).expect("create args file");

        let script = r#"#!/bin/sh
printf "%s\n" "$@" > "$ARGS_FILE"
exit 0
"#;
        write_executable_script(&script_path, script);

        let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
        let _args_guard = EnvGuard::set("ARGS_FILE", args_path.as_os_str());

        let (code, stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
            OsString::from("--ipv6"),
            OsString::from("remote::module"),
            OsString::from("dest"),
        ]);

        assert_eq!(code, 0);
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());

        let recorded = std::fs::read_to_string(&args_path).expect("read args file");
        assert!(recorded.contains("--ipv6"));
        assert!(!recorded.contains("--ipv4"));
    }

    #[cfg(unix)]
    #[test]
    fn remote_fallback_forwards_filter_shortcut() {
        use tempfile::tempdir;

        let _env_lock = ENV_LOCK.lock().expect("env lock");
        let _rsh_guard = clear_rsync_rsh();
        let temp = tempdir().expect("tempdir");
        let script_path = temp.path().join("fallback.sh");
        let args_path = temp.path().join("args.txt");
        std::fs::File::create(&args_path).expect("create args file");

        let script = r#"#!/bin/sh
printf "%s\n" "$@" > "$ARGS_FILE"
exit 0
"#;
        write_executable_script(&script_path, script);

        let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
        let _args_guard = EnvGuard::set("ARGS_FILE", args_path.as_os_str());

        let (code, stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
            OsString::from("-F"),
            OsString::from("remote::module"),
            OsString::from("dest"),
        ]);

        assert_eq!(code, 0);
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());

        let recorded = std::fs::read_to_string(&args_path).expect("read args file");
        let occurrences = recorded.lines().filter(|line| *line == "-F").count();
        assert_eq!(occurrences, 1);
    }

    #[cfg(unix)]
    #[test]
    fn remote_fallback_forwards_double_filter_shortcut() {
        use tempfile::tempdir;

        let _env_lock = ENV_LOCK.lock().expect("env lock");
        let _rsh_guard = clear_rsync_rsh();
        let temp = tempdir().expect("tempdir");
        let script_path = temp.path().join("fallback.sh");
        let args_path = temp.path().join("args.txt");
        std::fs::File::create(&args_path).expect("create args file");

        let script = r#"#!/bin/sh
printf "%s\n" "$@" > "$ARGS_FILE"
exit 0
"#;
        write_executable_script(&script_path, script);

        let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
        let _args_guard = EnvGuard::set("ARGS_FILE", args_path.as_os_str());

        let (code, stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
            OsString::from("-FF"),
            OsString::from("remote::module"),
            OsString::from("dest"),
        ]);

        assert_eq!(code, 0);
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());

        let recorded = std::fs::read_to_string(&args_path).expect("read args file");
        let occurrences = recorded.lines().filter(|line| *line == "-F").count();
        assert_eq!(occurrences, 2);
    }

    #[cfg(unix)]
    #[test]
    fn remote_rsync_url_invokes_fallback_binary() {
        use tempfile::tempdir;

        let _env_lock = ENV_LOCK.lock().expect("env lock");
        let _rsh_guard = clear_rsync_rsh();
        let temp = tempdir().expect("tempdir");
        let script_path = temp.path().join("fallback.sh");
        let args_path = temp.path().join("args.txt");
        std::fs::File::create(&args_path).expect("create args file");

        let script = r#"#!/bin/sh
printf "%s\n" "$@" > "$ARGS_FILE"
exit 0
"#;
        write_executable_script(&script_path, script);

        let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
        let _args_guard = EnvGuard::set("ARGS_FILE", args_path.as_os_str());

        let (code, stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
            OsString::from("--list-only"),
            OsString::from("rsync://example.com/module"),
        ]);

        assert_eq!(code, 0);
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());

        let recorded = std::fs::read_to_string(&args_path).expect("read args file");
        assert!(recorded.contains("--list-only"));
        assert!(recorded.contains("rsync://example.com/module"));
    }

    #[cfg(unix)]
    #[test]
    fn remote_fallback_forwards_files_from_entries() {
        use tempfile::tempdir;

        let _env_lock = ENV_LOCK.lock().expect("env lock");
        let _rsh_guard = clear_rsync_rsh();
        let temp = tempdir().expect("tempdir");
        let list_path = temp.path().join("file-list.txt");
        std::fs::write(&list_path, b"alpha\nbeta\n").expect("write list");

        let script_path = temp.path().join("fallback.sh");
        let args_path = temp.path().join("args.txt");
        let files_copy_path = temp.path().join("files.bin");
        let dest_path = temp.path().join("dest");

        let script = r#"#!/bin/sh
printf "%s\n" "$@" > "$ARGS_FILE"
files_from=""
while [ "$#" -gt 0 ]; do
  if [ "$1" = "--files-from" ]; then
    files_from="$2"
    break
  fi
  shift
done
if [ -n "$files_from" ]; then
  cat "$files_from" > "$FILES_COPY"
fi
exit 0
"#;
        write_executable_script(&script_path, script);

        let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
        let _args_guard = EnvGuard::set("ARGS_FILE", args_path.as_os_str());
        let _files_guard = EnvGuard::set("FILES_COPY", files_copy_path.as_os_str());

        let (code, stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
            OsString::from(format!("--files-from={}", list_path.display())),
            OsString::from("remote::module"),
            dest_path.clone().into_os_string(),
        ]);

        assert_eq!(code, 0);
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());

        let dest_display = dest_path.display().to_string();
        let recorded = std::fs::read_to_string(&args_path).expect("read args file");
        assert!(recorded.contains("--files-from"));
        assert!(recorded.contains("remote::module"));
        assert!(recorded.contains(&dest_display));

        let copied = std::fs::read(&files_copy_path).expect("read copied file list");
        assert_eq!(copied, b"alpha\nbeta\n");
    }

    #[cfg(unix)]
    #[test]
    fn remote_fallback_forwards_partial_dir_argument() {
        use tempfile::tempdir;

        let _env_lock = ENV_LOCK.lock().expect("env lock");
        let _rsh_guard = clear_rsync_rsh();
        let temp = tempdir().expect("tempdir");
        let script_path = temp.path().join("fallback.sh");
        let args_path = temp.path().join("args.txt");

        let script = r#"#!/bin/sh
printf "%s\n" "$@" > "$ARGS_FILE"
exit 0
"#;
        write_executable_script(&script_path, script);

        let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
        let _args_guard = EnvGuard::set("ARGS_FILE", args_path.as_os_str());

        let partial_dir = temp.path().join("partials");
        let dest_path = temp.path().join("dest");

        let (code, stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
            OsString::from(format!("--partial-dir={}", partial_dir.display())),
            OsString::from("remote::module"),
            dest_path.clone().into_os_string(),
        ]);

        assert_eq!(code, 0);
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());

        let recorded = std::fs::read_to_string(&args_path).expect("read args file");
        assert!(recorded.lines().any(|line| line == "--partial"));
        assert!(recorded.lines().any(|line| line == "--partial-dir"));
        assert!(
            recorded
                .lines()
                .any(|line| line == partial_dir.display().to_string())
        );

        // Ensure destination operand still forwarded correctly alongside partial dir args.
        assert!(
            recorded
                .lines()
                .any(|line| line == dest_path.display().to_string())
        );
    }

    #[cfg(unix)]
    #[test]
    fn remote_fallback_forwards_temp_dir_argument() {
        use tempfile::tempdir;

        let _env_lock = ENV_LOCK.lock().expect("env lock");
        let _rsh_guard = clear_rsync_rsh();
        let temp = tempdir().expect("tempdir");
        let script_path = temp.path().join("fallback.sh");
        let args_path = temp.path().join("args.txt");

        let script = r#"#!/bin/sh
printf "%s\n" "$@" > "$ARGS_FILE"
exit 0
"#;
        write_executable_script(&script_path, script);

        let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
        let _args_guard = EnvGuard::set("ARGS_FILE", args_path.as_os_str());

        let temp_dir = temp.path().join("temp-stage");
        let dest_path = temp.path().join("dest");

        let (code, stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
            OsString::from(format!("--temp-dir={}", temp_dir.display())),
            OsString::from("remote::module"),
            dest_path.clone().into_os_string(),
        ]);

        assert_eq!(code, 0);
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());

        let recorded = std::fs::read_to_string(&args_path).expect("read args file");
        assert!(recorded.lines().any(|line| line == "--temp-dir"));
        assert!(
            recorded
                .lines()
                .any(|line| line == temp_dir.display().to_string())
        );
        assert!(
            recorded
                .lines()
                .any(|line| line == dest_path.display().to_string())
        );
    }

    #[cfg(unix)]
    #[test]
    fn remote_fallback_forwards_backup_arguments() {
        use tempfile::tempdir;

        let _env_lock = ENV_LOCK.lock().expect("env lock");
        let _rsh_guard = clear_rsync_rsh();
        let temp = tempdir().expect("tempdir");
        let script_path = temp.path().join("fallback.sh");
        let args_path = temp.path().join("args.txt");

        let script = r#"#!/bin/sh
printf "%s\n" "$@" > "$ARGS_FILE"
exit 0
"#;
        write_executable_script(&script_path, script);

        let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
        let _args_guard = EnvGuard::set("ARGS_FILE", args_path.as_os_str());

        let dest_path = temp.path().join("dest");

        let (code, stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
            OsString::from("--backup"),
            OsString::from("remote::module"),
            dest_path.clone().into_os_string(),
        ]);

        assert_eq!(code, 0);
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());

        let recorded = std::fs::read_to_string(&args_path).expect("read args file");
        let args: Vec<&str> = recorded.lines().collect();
        assert!(args.contains(&"--backup"));
        assert!(!args.contains(&"--backup-dir"));
        assert!(!args.contains(&"--suffix"));

        std::fs::write(&args_path, b"").expect("truncate args file");

        let (code, stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
            OsString::from("--backup-dir"),
            OsString::from("backups"),
            OsString::from("remote::module"),
            dest_path.clone().into_os_string(),
        ]);

        assert_eq!(code, 0);
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());

        let recorded = std::fs::read_to_string(&args_path).expect("read args file");
        let args: Vec<&str> = recorded.lines().collect();
        assert!(args.contains(&"--backup"));
        assert!(args.contains(&"--backup-dir"));
        assert!(args.contains(&"backups"));

        std::fs::write(&args_path, b"").expect("truncate args file");

        let (code, stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
            OsString::from("--suffix"),
            OsString::from(".bak"),
            OsString::from("remote::module"),
            dest_path.into_os_string(),
        ]);

        assert_eq!(code, 0);
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());

        let recorded = std::fs::read_to_string(&args_path).expect("read args file");
        let args: Vec<&str> = recorded.lines().collect();
        assert!(args.contains(&"--backup"));
        assert!(args.contains(&"--suffix"));
        assert!(args.contains(&".bak"));
    }

    #[cfg(unix)]
    #[test]
    fn remote_fallback_forwards_link_dest_arguments() {
        use tempfile::tempdir;

        let _env_lock = ENV_LOCK.lock().expect("env lock");
        let _rsh_guard = clear_rsync_rsh();
        let temp = tempdir().expect("tempdir");
        let script_path = temp.path().join("fallback.sh");
        let args_path = temp.path().join("args.txt");

        let script = r#"#!/bin/sh
printf "%s\n" "$@" > "$ARGS_FILE"
exit 0
"#;
        write_executable_script(&script_path, script);

        let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
        let _args_guard = EnvGuard::set("ARGS_FILE", args_path.as_os_str());

        let dest_path = temp.path().join("dest");

        let (code, stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
            OsString::from("--link-dest"),
            OsString::from("baseline"),
            OsString::from("--link-dest"),
            OsString::from("/var/cache"),
            OsString::from("--link-dest=link"),
            OsString::from("remote::module"),
            dest_path.clone().into_os_string(),
        ]);

        assert_eq!(code, 0);
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());

        let recorded = std::fs::read_to_string(&args_path).expect("read args file");
        assert!(
            recorded
                .lines()
                .filter(|line| *line == "--link-dest")
                .count()
                >= 2
        );
        assert!(recorded.lines().any(|line| line == "baseline"));
        assert!(recorded.lines().any(|line| line == "/var/cache"));
        assert!(recorded.lines().any(|line| line == "--link-dest=link"));
        assert!(recorded.lines().any(|line| line == "--link-dest"));
        assert!(recorded.lines().any(|line| line == "link"));
        assert!(
            recorded
                .lines()
                .any(|line| line == dest_path.display().to_string())
        );
    }

    #[cfg(unix)]
    #[test]
    fn remote_fallback_forwards_reference_destinations() {
        use tempfile::tempdir;

        let _env_lock = ENV_LOCK.lock().expect("env lock");
        let _rsh_guard = clear_rsync_rsh();
        let temp = tempdir().expect("tempdir");
        let script_path = temp.path().join("fallback.sh");
        let args_path = temp.path().join("args.txt");

        let script = r#"#!/bin/sh
printf "%s\n" "$@" > "$ARGS_FILE"
exit 0
"#;
        write_executable_script(&script_path, script);

        let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
        let _args_guard = EnvGuard::set("ARGS_FILE", args_path.as_os_str());

        let dest_path = temp.path().join("dest");

        let (code, stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
            OsString::from("--link-dest=baseline"),
            OsString::from("--link-dest=/var/cache"),
            OsString::from("--compare-dest=compare"),
            OsString::from("--copy-dest=copy"),
            OsString::from("--link-dest=link"),
            OsString::from("remote::module"),
            dest_path.clone().into_os_string(),
        ]);

        assert_eq!(code, 0);
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());

        let recorded = std::fs::read_to_string(&args_path).expect("read args file");
        assert!(recorded.lines().any(|line| line == "--link-dest=baseline"));
        assert!(
            recorded
                .lines()
                .any(|line| line == "--link-dest=/var/cache")
        );
        assert!(recorded.lines().any(|line| line == "--compare-dest"));
        assert!(recorded.lines().any(|line| line == "compare"));
        assert!(recorded.lines().any(|line| line == "--copy-dest"));
        assert!(recorded.lines().any(|line| line == "copy"));
        assert!(recorded.lines().any(|line| line == "--link-dest"));
        assert!(recorded.lines().any(|line| line == "link"));
    }

    #[cfg(unix)]
    #[test]
    fn remote_fallback_forwards_compress_level() {
        use tempfile::tempdir;

        let _env_lock = ENV_LOCK.lock().expect("env lock");
        let _rsh_guard = clear_rsync_rsh();
        let temp = tempdir().expect("tempdir");
        let script_path = temp.path().join("fallback.sh");
        let args_path = temp.path().join("args.txt");

        let script = r#"#!/bin/sh
printf "%s\n" "$@" > "$ARGS_FILE"
exit 0
"#;
        write_executable_script(&script_path, script);

        let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
        let _args_guard = EnvGuard::set("ARGS_FILE", args_path.as_os_str());

        let (code, stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
            OsString::from("--compress-level=7"),
            OsString::from("remote::module"),
            OsString::from("dest"),
        ]);

        assert_eq!(code, 0);
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());

        let recorded = std::fs::read_to_string(&args_path).expect("read args file");
        assert!(recorded.contains("--compress"));
        assert!(recorded.contains("--compress-level"));
        assert!(recorded.contains("7"));
    }

    #[cfg(unix)]
    #[test]
    fn remote_fallback_forwards_skip_compress_arguments() {
        use tempfile::tempdir;

        let _env_lock = ENV_LOCK.lock().expect("env lock");
        let _rsh_guard = clear_rsync_rsh();
        let temp = tempdir().expect("tempdir");
        let script_path = temp.path().join("fallback.sh");
        let args_path = temp.path().join("args.txt");

        let script = r#"#!/bin/sh
printf "%s\n" "$@" > "$ARGS_FILE"
exit 0
"#;
        write_executable_script(&script_path, script);

        let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
        let _args_guard = EnvGuard::set("ARGS_FILE", args_path.as_os_str());

        let (code, stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
            OsString::from("--skip-compress=gz/mp3"),
            OsString::from("remote::module"),
            OsString::from("dest"),
        ]);

        assert_eq!(code, 0);
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());

        let recorded = std::fs::read_to_string(&args_path).expect("read args file");
        assert!(
            recorded
                .lines()
                .any(|line| line == "--skip-compress=gz/mp3")
        );

        let (code, stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
            OsString::from("--skip-compress="),
            OsString::from("remote::module"),
            OsString::from("dest"),
        ]);

        assert_eq!(code, 0);
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());

        let recorded = std::fs::read_to_string(&args_path).expect("read args file");
        assert!(recorded.lines().any(|line| line == "--skip-compress="));
    }

    #[cfg(unix)]
    #[test]
    fn remote_fallback_forwards_no_compress_when_level_zero() {
        use tempfile::tempdir;

        let _env_lock = ENV_LOCK.lock().expect("env lock");
        let _rsh_guard = clear_rsync_rsh();
        let temp = tempdir().expect("tempdir");
        let script_path = temp.path().join("fallback.sh");
        let args_path = temp.path().join("args.txt");

        let script = r#"#!/bin/sh
printf "%s\n" "$@" > "$ARGS_FILE"
exit 0
"#;
        write_executable_script(&script_path, script);

        let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
        let _args_guard = EnvGuard::set("ARGS_FILE", args_path.as_os_str());

        let (code, stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
            OsString::from("--compress-level=0"),
            OsString::from("remote::module"),
            OsString::from("dest"),
        ]);

        assert_eq!(code, 0);
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());

        let recorded = std::fs::read_to_string(&args_path).expect("read args file");
        assert!(recorded.contains("--no-compress"));
        assert!(recorded.contains("--compress-level"));
        assert!(recorded.contains("0"));
    }

    #[cfg(unix)]
    #[test]
    fn remote_fallback_streams_process_output() {
        use tempfile::tempdir;

        let _env_lock = ENV_LOCK.lock().expect("env lock");
        let _rsh_guard = clear_rsync_rsh();
        let temp = tempdir().expect("tempdir");
        let script_path = temp.path().join("fallback.sh");

        let script = r#"#!/bin/sh
echo "fallback stdout"
echo "fallback stderr" 1>&2
exit 0
"#;
        write_executable_script(&script_path, script);

        let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());

        let (code, stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
            OsString::from("remote::module"),
            OsString::from("dest"),
        ]);

        assert_eq!(code, 0);
        assert_eq!(stdout, b"fallback stdout\n");
        assert_eq!(stderr, b"fallback stderr\n");
    }

    #[cfg(unix)]
    #[test]
    fn remote_fallback_preserves_from0_entries() {
        use tempfile::tempdir;

        let _env_lock = ENV_LOCK.lock().expect("env lock");
        let _rsh_guard = clear_rsync_rsh();
        let temp = tempdir().expect("tempdir");
        let list_path = temp.path().join("file-list.bin");
        std::fs::write(&list_path, b"first\0second\0").expect("write list");

        let script_path = temp.path().join("fallback.sh");
        let args_path = temp.path().join("args.txt");
        let files_copy_path = temp.path().join("files.bin");
        let dest_path = temp.path().join("dest");

        let script = r#"#!/bin/sh
printf "%s\n" "$@" > "$ARGS_FILE"
files_from=""
while [ "$#" -gt 0 ]; do
  if [ "$1" = "--files-from" ]; then
    files_from="$2"
    break
  fi
  shift
done
if [ -n "$files_from" ]; then
  cat "$files_from" > "$FILES_COPY"
fi
exit 0
"#;
        write_executable_script(&script_path, script);

        let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
        let _args_guard = EnvGuard::set("ARGS_FILE", args_path.as_os_str());
        let _files_guard = EnvGuard::set("FILES_COPY", files_copy_path.as_os_str());

        let (code, stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
            OsString::from("--from0"),
            OsString::from(format!("--files-from={}", list_path.display())),
            OsString::from("remote::module"),
            dest_path.clone().into_os_string(),
        ]);

        assert_eq!(code, 0);
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());

        let dest_display = dest_path.display().to_string();
        let recorded = std::fs::read_to_string(&args_path).expect("read args file");
        assert!(recorded.contains("--files-from"));
        assert!(recorded.contains("--from0"));
        assert!(recorded.contains(&dest_display));

        let copied = std::fs::read(&files_copy_path).expect("read copied file list");
        assert_eq!(copied, b"first\0second\0");
    }

    #[cfg(unix)]
    #[test]
    fn remote_fallback_forwards_whole_file_flags() {
        use tempfile::tempdir;

        let _env_lock = ENV_LOCK.lock().expect("env lock");
        let _rsh_guard = clear_rsync_rsh();
        let temp = tempdir().expect("tempdir");
        let script_path = temp.path().join("fallback.sh");
        let args_path = temp.path().join("args.txt");

        let script = r#"#!/bin/sh
printf "%s\n" "$@" > "$ARGS_FILE"
exit 0
"#;
        write_executable_script(&script_path, script);

        let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
        let _args_guard = EnvGuard::set("ARGS_FILE", args_path.as_os_str());

        let dest_path = temp.path().join("dest");

        let (code, stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
            OsString::from("-W"),
            OsString::from("remote::module"),
            dest_path.clone().into_os_string(),
        ]);

        assert_eq!(code, 0);
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());

        let recorded = std::fs::read_to_string(&args_path).expect("read args file");
        let args: Vec<&str> = recorded.lines().collect();
        assert!(args.contains(&"--whole-file"));
        assert!(!args.contains(&"--no-whole-file"));
        let dest_string = dest_path.display().to_string();
        assert!(args.contains(&"--whole-file"));
        assert!(!args.contains(&"--no-whole-file"));
        assert!(args.contains(&dest_string.as_str()));

        std::fs::write(&args_path, b"").expect("truncate args file");

        let (code, stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
            OsString::from("--no-whole-file"),
            OsString::from("remote::module"),
            dest_path.into_os_string(),
        ]);

        assert_eq!(code, 0);
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());

        let recorded = std::fs::read_to_string(&args_path).expect("read args file");
        let args: Vec<&str> = recorded.lines().collect();
        assert!(args.contains(&"--no-whole-file"));
        assert!(!args.contains(&"--whole-file"));
    }

    #[cfg(unix)]
    #[test]
    fn remote_fallback_forwards_hard_links_flags() {
        use tempfile::tempdir;

        let _env_lock = ENV_LOCK.lock().expect("env lock");
        let _rsh_guard = clear_rsync_rsh();
        let temp = tempdir().expect("tempdir");
        let script_path = temp.path().join("fallback.sh");
        let args_path = temp.path().join("args.txt");

        let script = r#"#!/bin/sh
printf "%s\n" "$@" > "$ARGS_FILE"
exit 0
"#;
        write_executable_script(&script_path, script);

        let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
        let _args_guard = EnvGuard::set("ARGS_FILE", args_path.as_os_str());

        let dest_path = temp.path().join("dest");

        let (code, stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
            OsString::from("--hard-links"),
            OsString::from("remote::module"),
            dest_path.clone().into_os_string(),
        ]);

        assert_eq!(code, 0);
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());

        let recorded = std::fs::read_to_string(&args_path).expect("read args file");
        let args: Vec<&str> = recorded.lines().collect();
        assert!(args.contains(&"--hard-links"));
        assert!(!args.contains(&"--no-hard-links"));

        std::fs::write(&args_path, b"").expect("truncate args file");

        let (code, stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
            OsString::from("--no-hard-links"),
            OsString::from("remote::module"),
            dest_path.into_os_string(),
        ]);

        assert_eq!(code, 0);
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());

        let recorded = std::fs::read_to_string(&args_path).expect("read args file");
        let args: Vec<&str> = recorded.lines().collect();
        assert!(args.contains(&"--no-hard-links"));
        assert!(!args.contains(&"--hard-links"));
    }

    #[cfg(unix)]
    #[test]
    fn remote_fallback_forwards_append_flags() {
        use tempfile::tempdir;

        let _env_lock = ENV_LOCK.lock().expect("env lock");
        let _rsh_guard = clear_rsync_rsh();
        let temp = tempdir().expect("tempdir");
        let script_path = temp.path().join("fallback.sh");
        let args_path = temp.path().join("args.txt");

        let script = r#"#!/bin/sh
printf "%s\n" "$@" > "$ARGS_FILE"
exit 0
"#;
        write_executable_script(&script_path, script);

        let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
        let _args_guard = EnvGuard::set("ARGS_FILE", args_path.as_os_str());

        let dest_path = temp.path().join("dest");

        let (code, stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
            OsString::from("--append"),
            OsString::from("remote::module"),
            dest_path.clone().into_os_string(),
        ]);

        assert_eq!(code, 0);
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());

        let recorded = std::fs::read_to_string(&args_path).expect("read args file");
        let args: Vec<&str> = recorded.lines().collect();
        assert!(args.contains(&"--append"));
        assert!(!args.contains(&"--append-verify"));
        assert!(!args.contains(&"--no-append"));

        std::fs::write(&args_path, b"").expect("truncate args file");

        let (code, stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
            OsString::from("--no-append"),
            OsString::from("remote::module"),
            dest_path.clone().into_os_string(),
        ]);

        assert_eq!(code, 0);
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());

        let recorded = std::fs::read_to_string(&args_path).expect("read args file");
        let args: Vec<&str> = recorded.lines().collect();
        assert!(args.contains(&"--no-append"));
        assert!(!args.contains(&"--append"));
        assert!(!args.contains(&"--append-verify"));

        std::fs::write(&args_path, b"").expect("truncate args file");

        let (code, stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
            OsString::from("--append-verify"),
            OsString::from("remote::module"),
            dest_path.into_os_string(),
        ]);

        assert_eq!(code, 0);
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());

        let recorded = std::fs::read_to_string(&args_path).expect("read args file");
        let args: Vec<&str> = recorded.lines().collect();
        assert!(args.contains(&"--append-verify"));
        assert!(!args.contains(&"--append"));
        assert!(!args.contains(&"--no-append"));
    }

    #[cfg(unix)]
    #[test]
    fn remote_fallback_forwards_preallocate_flag() {
        use tempfile::tempdir;

        let _env_lock = ENV_LOCK.lock().expect("env lock");
        let _rsh_guard = clear_rsync_rsh();
        let temp = tempdir().expect("tempdir");
        let script_path = temp.path().join("fallback.sh");
        let args_path = temp.path().join("args.txt");

        let script = r#"#!/bin/sh
printf "%s\n" "$@" > "$ARGS_FILE"
exit 0
"#;
        write_executable_script(&script_path, script);

        let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
        let _args_guard = EnvGuard::set("ARGS_FILE", args_path.as_os_str());

        let dest_path = temp.path().join("dest");

        let (code, stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
            OsString::from("--preallocate"),
            OsString::from("remote::module"),
            dest_path.clone().into_os_string(),
        ]);

        assert_eq!(code, 0);
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());

        let recorded = std::fs::read_to_string(&args_path).expect("read args file");
        let args: Vec<&str> = recorded.lines().collect();
        assert!(args.contains(&"--preallocate"));

        std::fs::write(&args_path, b"").expect("truncate args file");

        let (code, stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
            OsString::from("remote::module"),
            dest_path.into_os_string(),
        ]);

        assert_eq!(code, 0);
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());

        let recorded = std::fs::read_to_string(&args_path).expect("read args file");
        assert!(!recorded.lines().any(|line| line == "--preallocate"));
    }

    #[cfg(unix)]
    #[test]
    fn remote_fallback_sanitises_bwlimit_argument() {
        use tempfile::tempdir;

        let _env_lock = ENV_LOCK.lock().expect("env lock");
        let _rsh_guard = clear_rsync_rsh();
        let temp = tempdir().expect("tempdir");
        let script_path = temp.path().join("fallback.sh");
        let args_path = temp.path().join("args.txt");
        std::fs::File::create(&args_path).expect("create args file");

        let script = r#"#!/bin/sh
printf "%s\n" "$@" > "$ARGS_FILE"
exit 0
"#;
        write_executable_script(&script_path, script);

        let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
        let _args_guard = EnvGuard::set("ARGS_FILE", args_path.as_os_str());

        let (code, stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
            OsString::from("--bwlimit=1M:64K"),
            OsString::from("remote::module"),
            OsString::from("dest"),
        ]);

        assert_eq!(code, 0);
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());

        let recorded = std::fs::read_to_string(&args_path).expect("read args file");
        assert!(recorded.contains("--bwlimit"));
        assert!(!recorded.contains("1M:64K"));

        let mut lines = recorded.lines();
        while let Some(line) = lines.next() {
            if line == "--bwlimit" {
                let value = lines.next().expect("bwlimit value recorded");
                assert_eq!(value, "1048576:65536");
                return;
            }
        }

        panic!("--bwlimit argument not forwarded to fallback");
    }

    #[cfg(unix)]
    #[test]
    fn remote_fallback_forwards_no_bwlimit_argument() {
        use tempfile::tempdir;

        let _env_lock = ENV_LOCK.lock().expect("env lock");
        let _rsh_guard = clear_rsync_rsh();
        let temp = tempdir().expect("tempdir");
        let script_path = temp.path().join("fallback.sh");
        let args_path = temp.path().join("args.txt");
        std::fs::File::create(&args_path).expect("create args file");

        let script = r#"#!/bin/sh
printf "%s\n" "$@" > "$ARGS_FILE"
exit 0
"#;
        write_executable_script(&script_path, script);

        let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
        let _args_guard = EnvGuard::set("ARGS_FILE", args_path.as_os_str());

        let (code, stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
            OsString::from("--no-bwlimit"),
            OsString::from("remote::module"),
            OsString::from("dest"),
        ]);

        assert_eq!(code, 0);
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());

        let recorded = std::fs::read_to_string(&args_path).expect("read args file");
        let mut lines = recorded.lines();
        while let Some(line) = lines.next() {
            if line == "--bwlimit" {
                let value = lines.next().expect("bwlimit value recorded");
                assert_eq!(value, "0");
                return;
            }
        }

        panic!("--bwlimit argument not forwarded to fallback");
    }

    #[cfg(unix)]
    #[test]
    fn remote_fallback_forwards_safe_links_flag() {
        use tempfile::tempdir;

        let _env_lock = ENV_LOCK.lock().expect("env lock");
        let _rsh_guard = clear_rsync_rsh();
        let temp = tempdir().expect("tempdir");
        let script_path = temp.path().join("fallback.sh");
        let args_path = temp.path().join("args.txt");

        let script = r#"#!/bin/sh
printf "%s\n" "$@" > "$ARGS_FILE"
exit 0
"#;
        write_executable_script(&script_path, script);

        let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
        let _args_guard = EnvGuard::set("ARGS_FILE", args_path.as_os_str());

        let dest_path = temp.path().join("dest");

        let (code, stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
            OsString::from("--safe-links"),
            OsString::from("remote::module"),
            dest_path.clone().into_os_string(),
        ]);

        assert_eq!(code, 0);
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());

        let recorded = std::fs::read_to_string(&args_path).expect("read args file");
        let args: Vec<&str> = recorded.lines().collect();
        assert!(args.contains(&"--safe-links"));

        std::fs::write(&args_path, b"").expect("truncate args file");

        let (code, stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
            OsString::from("remote::module"),
            dest_path.into_os_string(),
        ]);

        assert_eq!(code, 0);
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());

        let recorded = std::fs::read_to_string(&args_path).expect("read args file");
        assert!(!recorded.lines().any(|line| line == "--safe-links"));
    }

    #[cfg(unix)]
    #[test]
    fn remote_fallback_forwards_implied_dirs_flags() {
        use tempfile::tempdir;

        let _env_lock = ENV_LOCK.lock().expect("env lock");
        let _rsh_guard = clear_rsync_rsh();
        let temp = tempdir().expect("tempdir");
        let script_path = temp.path().join("fallback.sh");
        let args_path = temp.path().join("args.txt");

        let script = r#"#!/bin/sh
printf "%s\n" "$@" > "$ARGS_FILE"
exit 0
"#;
        write_executable_script(&script_path, script);

        let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
        let _args_guard = EnvGuard::set("ARGS_FILE", args_path.as_os_str());

        let destination = temp.path().join("dest");
        let (code, stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
            OsString::from("--no-implied-dirs"),
            OsString::from("remote::module"),
            destination.clone().into_os_string(),
        ]);

        assert_eq!(code, 0);
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());

        let recorded = std::fs::read_to_string(&args_path).expect("read args file");
        let args: Vec<&str> = recorded.lines().collect();
        assert!(args.contains(&"--no-implied-dirs"));
        assert!(!args.contains(&"--implied-dirs"));

        std::fs::write(&args_path, b"").expect("truncate args file");

        let (code, stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
            OsString::from("--implied-dirs"),
            OsString::from("remote::module"),
            destination.into_os_string(),
        ]);

        assert_eq!(code, 0);
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());

        let recorded = std::fs::read_to_string(&args_path).expect("read args file");
        let args: Vec<&str> = recorded.lines().collect();
        assert!(args.contains(&"--implied-dirs"));
        assert!(!args.contains(&"--no-implied-dirs"));
    }

    #[cfg(unix)]
    #[test]
    fn remote_fallback_forwards_prune_empty_dirs_flags() {
        use tempfile::tempdir;

        let _env_lock = ENV_LOCK.lock().expect("env lock");
        let _rsh_guard = clear_rsync_rsh();
        let temp = tempdir().expect("tempdir");
        let script_path = temp.path().join("fallback.sh");
        let args_path = temp.path().join("args.txt");

        let script = r#"#!/bin/sh
printf "%s\n" "$@" > "$ARGS_FILE"
exit 0
"#;
        write_executable_script(&script_path, script);

        let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
        let _args_guard = EnvGuard::set("ARGS_FILE", args_path.as_os_str());

        let destination = temp.path().join("dest");

        let (code, stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
            OsString::from("--prune-empty-dirs"),
            OsString::from("remote::module"),
            destination.clone().into_os_string(),
        ]);

        assert_eq!(code, 0);
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());

        let recorded = std::fs::read_to_string(&args_path).expect("read args file");
        let args: Vec<&str> = recorded.lines().collect();
        assert!(args.contains(&"--prune-empty-dirs"));
        assert!(!args.contains(&"--no-prune-empty-dirs"));

        std::fs::write(&args_path, b"").expect("truncate args file");

        let (code, stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
            OsString::from("--no-prune-empty-dirs"),
            OsString::from("remote::module"),
            destination.into_os_string(),
        ]);

        assert_eq!(code, 0);
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());

        let recorded = std::fs::read_to_string(&args_path).expect("read args file");
        let args: Vec<&str> = recorded.lines().collect();
        assert!(args.contains(&"--no-prune-empty-dirs"));
        assert!(!args.contains(&"--prune-empty-dirs"));
    }

    #[cfg(unix)]
    #[test]
    fn remote_fallback_forwards_delete_after_flag() {
        use tempfile::tempdir;

        let _env_lock = ENV_LOCK.lock().expect("env lock");
        let _rsh_guard = clear_rsync_rsh();
        let temp = tempdir().expect("tempdir");
        let script_path = temp.path().join("fallback.sh");
        let args_path = temp.path().join("args.txt");

        let script = r#"#!/bin/sh
printf "%s\n" "$@" > "$ARGS_FILE"
exit 0
"#;
        write_executable_script(&script_path, script);

        let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
        let _args_guard = EnvGuard::set("ARGS_FILE", args_path.as_os_str());

        let destination = temp.path().join("dest");
        let (code, stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
            OsString::from("--delete-after"),
            OsString::from("remote::module"),
            destination.into_os_string(),
        ]);

        assert_eq!(code, 0);
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());

        let recorded = std::fs::read_to_string(&args_path).expect("read args file");
        let args: Vec<&str> = recorded.lines().collect();
        assert!(args.contains(&"--delete"));
        assert!(args.contains(&"--delete-after"));
        assert!(!args.contains(&"--delete-delay"));
    }

    #[cfg(unix)]
    #[test]
    fn remote_fallback_forwards_delete_before_flag() {
        use tempfile::tempdir;

        let _env_lock = ENV_LOCK.lock().expect("env lock");
        let _rsh_guard = clear_rsync_rsh();
        let temp = tempdir().expect("tempdir");
        let script_path = temp.path().join("fallback.sh");
        let args_path = temp.path().join("args.txt");

        let script = r#"#!/bin/sh
printf "%s\n" "$@" > "$ARGS_FILE"
exit 0
"#;
        write_executable_script(&script_path, script);

        let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
        let _args_guard = EnvGuard::set("ARGS_FILE", args_path.as_os_str());

        let destination = temp.path().join("dest");
        let (code, stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
            OsString::from("--delete-before"),
            OsString::from("remote::module"),
            destination.into_os_string(),
        ]);

        assert_eq!(code, 0);
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());

        let recorded = std::fs::read_to_string(&args_path).expect("read args file");
        let args: Vec<&str> = recorded.lines().collect();
        assert!(args.contains(&"--delete"));
        assert!(args.contains(&"--delete-before"));
        assert!(!args.contains(&"--delete-after"));
    }

    #[cfg(unix)]
    #[test]
    fn remote_fallback_forwards_delete_during_flag() {
        use tempfile::tempdir;

        let _env_lock = ENV_LOCK.lock().expect("env lock");
        let _rsh_guard = clear_rsync_rsh();
        let temp = tempdir().expect("tempdir");
        let script_path = temp.path().join("fallback.sh");
        let args_path = temp.path().join("args.txt");

        let script = r#"#!/bin/sh
printf "%s\n" "$@" > "$ARGS_FILE"
exit 0
"#;
        write_executable_script(&script_path, script);

        let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
        let _args_guard = EnvGuard::set("ARGS_FILE", args_path.as_os_str());

        let destination = temp.path().join("dest");
        let (code, stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
            OsString::from("--delete-during"),
            OsString::from("remote::module"),
            destination.into_os_string(),
        ]);

        assert_eq!(code, 0);
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());

        let recorded = std::fs::read_to_string(&args_path).expect("read args file");
        let args: Vec<&str> = recorded.lines().collect();
        assert!(args.contains(&"--delete"));
        assert!(args.contains(&"--delete-during"));
        assert!(!args.contains(&"--delete-delay"));
        assert!(!args.contains(&"--delete-before"));
        assert!(!args.contains(&"--delete-after"));
    }

    #[cfg(unix)]
    #[test]
    fn remote_fallback_forwards_delete_delay_flag() {
        use tempfile::tempdir;

        let _env_lock = ENV_LOCK.lock().expect("env lock");
        let _rsh_guard = clear_rsync_rsh();
        let temp = tempdir().expect("tempdir");
        let script_path = temp.path().join("fallback.sh");
        let args_path = temp.path().join("args.txt");

        let script = r#"#!/bin/sh
printf "%s\n" "$@" > "$ARGS_FILE"
exit 0
"#;
        write_executable_script(&script_path, script);

        let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
        let _args_guard = EnvGuard::set("ARGS_FILE", args_path.as_os_str());

        let destination = temp.path().join("dest");
        let (code, stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
            OsString::from("--delete-delay"),
            OsString::from("remote::module"),
            destination.into_os_string(),
        ]);

        assert_eq!(code, 0);
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());

        let recorded = std::fs::read_to_string(&args_path).expect("read args file");
        let args: Vec<&str> = recorded.lines().collect();
        assert!(args.contains(&"--delete"));
        assert!(args.contains(&"--delete-delay"));
        assert!(!args.contains(&"--delete-before"));
        assert!(!args.contains(&"--delete-after"));
        assert!(!args.contains(&"--delete-during"));
    }

    #[cfg(unix)]
    #[test]
    fn remote_fallback_forwards_max_delete_limit() {
        use tempfile::tempdir;

        let _env_lock = ENV_LOCK.lock().expect("env lock");
        let _rsh_guard = clear_rsync_rsh();
        let temp = tempdir().expect("tempdir");
        let script_path = temp.path().join("fallback.sh");
        let args_path = temp.path().join("args.txt");

        let script = r#"#!/bin/sh
printf "%s\n" "$@" > "$ARGS_FILE"
exit 0
"#;
        write_executable_script(&script_path, script);

        let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
        let _args_guard = EnvGuard::set("ARGS_FILE", args_path.as_os_str());

        let destination = temp.path().join("dest");
        let (code, stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
            OsString::from("--max-delete=5"),
            OsString::from("remote::module"),
            destination.into_os_string(),
        ]);

        assert_eq!(code, 0);
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());

        let recorded = std::fs::read_to_string(&args_path).expect("read args file");
        let args: Vec<&str> = recorded.lines().collect();
        assert!(args.contains(&"--delete"));
        assert!(args.contains(&"--max-delete=5"));
    }

    #[cfg(unix)]
    #[test]
    fn remote_fallback_forwards_modify_window() {
        use tempfile::tempdir;

        let _env_lock = ENV_LOCK.lock().expect("env lock");
        let _rsh_guard = clear_rsync_rsh();
        let temp = tempdir().expect("tempdir");
        let script_path = temp.path().join("fallback.sh");
        let args_path = temp.path().join("args.txt");

        let script = r#"#!/bin/sh
printf "%s\n" "$@" > "$ARGS_FILE"
exit 0
"#;
        write_executable_script(&script_path, script);

        let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
        let _args_guard = EnvGuard::set("ARGS_FILE", args_path.as_os_str());

        let destination = temp.path().join("dest");
        let (code, stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
            OsString::from("--modify-window=5"),
            OsString::from("remote::module"),
            destination.into_os_string(),
        ]);

        assert_eq!(code, 0);
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());

        let recorded = std::fs::read_to_string(&args_path).expect("read args file");
        let args: Vec<&str> = recorded.lines().collect();
        assert!(args.contains(&"--modify-window=5"));
    }

    #[cfg(unix)]
    #[test]
    fn remote_fallback_forwards_min_max_size_arguments() {
        use tempfile::tempdir;

        let _env_lock = ENV_LOCK.lock().expect("env lock");
        let _rsh_guard = clear_rsync_rsh();
        let temp = tempdir().expect("tempdir");
        let script_path = temp.path().join("fallback.sh");
        let args_path = temp.path().join("args.txt");

        let script = r#"#!/bin/sh
printf "%s\n" "$@" > "$ARGS_FILE"
exit 0
"#;
        write_executable_script(&script_path, script);

        let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
        let _args_guard = EnvGuard::set("ARGS_FILE", args_path.as_os_str());

        let destination = temp.path().join("dest");
        let (code, stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
            OsString::from("--min-size=1.5K"),
            OsString::from("--max-size=2M"),
            OsString::from("remote::module"),
            destination.into_os_string(),
        ]);

        assert_eq!(code, 0);
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());

        let recorded = std::fs::read_to_string(&args_path).expect("read args file");
        let args: Vec<&str> = recorded.lines().collect();
        assert!(args.contains(&"--min-size=1.5K"));
        assert!(args.contains(&"--max-size=2M"));
    }

    #[cfg(unix)]
    #[test]
    fn remote_fallback_forwards_update_flag() {
        use tempfile::tempdir;

        let _env_lock = ENV_LOCK.lock().expect("env lock");
        let _rsh_guard = clear_rsync_rsh();
        let temp = tempdir().expect("tempdir");
        let script_path = temp.path().join("fallback.sh");
        let args_path = temp.path().join("args.txt");

        let script = r#"#!/bin/sh
printf "%s\n" "$@" > "$ARGS_FILE"
exit 0
"#;
        write_executable_script(&script_path, script);

        let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
        let _args_guard = EnvGuard::set("ARGS_FILE", args_path.as_os_str());

        let destination = temp.path().join("dest");
        let (code, stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
            OsString::from("--update"),
            OsString::from("remote::module"),
            destination.into_os_string(),
        ]);

        assert_eq!(code, 0);
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());

        let recorded = std::fs::read_to_string(&args_path).expect("read args file");
        let args: Vec<&str> = recorded.lines().collect();
        assert!(args.contains(&"--update"));
    }

    #[cfg(unix)]
    #[test]
    fn remote_fallback_forwards_debug_flags() {
        use tempfile::tempdir;

        let _env_lock = ENV_LOCK.lock().expect("env lock");
        let _rsh_guard = clear_rsync_rsh();
        let temp = tempdir().expect("tempdir");
        let script_path = temp.path().join("fallback.sh");
        let args_path = temp.path().join("args.txt");

        let script = r#"#!/bin/sh
printf "%s\n" "$@" > "$ARGS_FILE"
exit 0
"#;
        write_executable_script(&script_path, script);

        let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
        let _args_guard = EnvGuard::set("ARGS_FILE", args_path.as_os_str());

        let destination = temp.path().join("dest");
        let (code, stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
            OsString::from("--debug=io,events"),
            OsString::from("remote::module"),
            destination.into_os_string(),
        ]);

        assert_eq!(code, 0);
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());

        let recorded = std::fs::read_to_string(&args_path).expect("read args file");
        let args: Vec<&str> = recorded.lines().collect();
        assert!(args.contains(&"--debug=io"));
        assert!(args.contains(&"--debug=events"));
    }

    #[cfg(unix)]
    #[test]
    fn remote_fallback_forwards_protect_args_flag() {
        use tempfile::tempdir;

        let _env_lock = ENV_LOCK.lock().expect("env lock");
        let _rsh_guard = clear_rsync_rsh();
        let temp = tempdir().expect("tempdir");
        let script_path = temp.path().join("fallback.sh");
        let args_path = temp.path().join("args.txt");

        let script = r#"#!/bin/sh
printf "%s\n" "$@" > "$ARGS_FILE"
exit 0
"#;
        write_executable_script(&script_path, script);

        let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
        let _args_guard = EnvGuard::set("ARGS_FILE", args_path.as_os_str());

        let destination = temp.path().join("dest");
        let (code, stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
            OsString::from("--protect-args"),
            OsString::from("remote::module"),
            destination.into_os_string(),
        ]);

        assert_eq!(code, 0);
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());

        let recorded = std::fs::read_to_string(&args_path).expect("read args file");
        let args: Vec<&str> = recorded.lines().collect();
        assert!(args.contains(&"--protect-args"));
        assert!(!args.contains(&"--no-protect-args"));
    }

    #[cfg(unix)]
    #[test]
    fn remote_fallback_forwards_human_readable_flag() {
        use tempfile::tempdir;

        let _env_lock = ENV_LOCK.lock().expect("env lock");
        let _rsh_guard = clear_rsync_rsh();
        let temp = tempdir().expect("tempdir");
        let script_path = temp.path().join("fallback.sh");
        let args_path = temp.path().join("args.txt");

        let script = r#"#!/bin/sh
printf "%s\n" "$@" > "$ARGS_FILE"
exit 0
"#;
        write_executable_script(&script_path, script);

        let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
        let _args_guard = EnvGuard::set("ARGS_FILE", args_path.as_os_str());

        let destination = temp.path().join("dest");
        let (code, stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
            OsString::from("--human-readable"),
            OsString::from("remote::module"),
            destination.into_os_string(),
        ]);

        assert_eq!(code, 0);
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());

        let recorded = std::fs::read_to_string(&args_path).expect("read args file");
        let args: Vec<&str> = recorded.lines().collect();
        assert!(args.contains(&"--human-readable"));
        assert!(!args.contains(&"--no-human-readable"));
        assert!(!args.contains(&"--human-readable=2"));
    }

    #[cfg(unix)]
    #[test]
    fn remote_fallback_forwards_no_human_readable_flag() {
        use tempfile::tempdir;

        let _env_lock = ENV_LOCK.lock().expect("env lock");
        let _rsh_guard = clear_rsync_rsh();
        let temp = tempdir().expect("tempdir");
        let script_path = temp.path().join("fallback.sh");
        let args_path = temp.path().join("args.txt");

        let script = r#"#!/bin/sh
printf "%s\n" "$@" > "$ARGS_FILE"
exit 0
"#;
        write_executable_script(&script_path, script);

        let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
        let _args_guard = EnvGuard::set("ARGS_FILE", args_path.as_os_str());

        let destination = temp.path().join("dest");
        let (code, stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
            OsString::from("--no-human-readable"),
            OsString::from("remote::module"),
            destination.into_os_string(),
        ]);

        assert_eq!(code, 0);
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());

        let recorded = std::fs::read_to_string(&args_path).expect("read args file");
        let args: Vec<&str> = recorded.lines().collect();
        assert!(args.contains(&"--no-human-readable"));
        assert!(!args.contains(&"--human-readable"));
        assert!(!args.contains(&"--human-readable=2"));
    }

    #[cfg(unix)]
    #[test]
    fn remote_fallback_forwards_human_readable_level_two() {
        use tempfile::tempdir;

        let _env_lock = ENV_LOCK.lock().expect("env lock");
        let _rsh_guard = clear_rsync_rsh();
        let temp = tempdir().expect("tempdir");
        let script_path = temp.path().join("fallback.sh");
        let args_path = temp.path().join("args.txt");

        let script = r#"#!/bin/sh
printf "%s\n" "$@" > "$ARGS_FILE"
exit 0
"#;
        write_executable_script(&script_path, script);

        let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
        let _args_guard = EnvGuard::set("ARGS_FILE", args_path.as_os_str());

        let destination = temp.path().join("dest");
        let (code, stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
            OsString::from("--human-readable=2"),
            OsString::from("remote::module"),
            destination.into_os_string(),
        ]);

        assert_eq!(code, 0);
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());

        let recorded = std::fs::read_to_string(&args_path).expect("read args file");
        let args: Vec<&str> = recorded.lines().collect();
        assert!(args.contains(&"--human-readable=2"));
        assert!(!args.contains(&"--human-readable"));
        assert!(!args.contains(&"--no-human-readable"));
    }

    #[cfg(unix)]
    #[test]
    fn remote_fallback_forwards_super_flag() {
        use tempfile::tempdir;

        let _env_lock = ENV_LOCK.lock().expect("env lock");
        let _rsh_guard = clear_rsync_rsh();
        let temp = tempdir().expect("tempdir");
        let script_path = temp.path().join("fallback.sh");
        let args_path = temp.path().join("args.txt");

        let script = r#"#!/bin/sh
printf "%s\n" "$@" > "$ARGS_FILE"
exit 0
"#;
        write_executable_script(&script_path, script);

        let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
        let _args_guard = EnvGuard::set("ARGS_FILE", args_path.as_os_str());

        let destination = temp.path().join("dest");
        let (code, stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
            OsString::from("--super"),
            OsString::from("remote::module"),
            destination.into_os_string(),
        ]);

        assert_eq!(code, 0);
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());

        let recorded = std::fs::read_to_string(&args_path).expect("read args file");
        let args: Vec<&str> = recorded.lines().collect();
        assert!(args.contains(&"--super"));
    }

    #[cfg(unix)]
    #[test]
    fn remote_fallback_forwards_no_super_flag() {
        use tempfile::tempdir;

        let _env_lock = ENV_LOCK.lock().expect("env lock");
        let _rsh_guard = clear_rsync_rsh();
        let temp = tempdir().expect("tempdir");
        let script_path = temp.path().join("fallback.sh");
        let args_path = temp.path().join("args.txt");

        let script = r#"#!/bin/sh
printf "%s\n" "$@" > "$ARGS_FILE"
exit 0
"#;
        write_executable_script(&script_path, script);

        let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
        let _args_guard = EnvGuard::set("ARGS_FILE", args_path.as_os_str());

        let destination = temp.path().join("dest");
        let (code, stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
            OsString::from("--no-super"),
            OsString::from("remote::module"),
            destination.into_os_string(),
        ]);

        assert_eq!(code, 0);
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());

        let recorded = std::fs::read_to_string(&args_path).expect("read args file");
        let args: Vec<&str> = recorded.lines().collect();
        assert!(args.contains(&"--no-super"));
        assert!(!args.contains(&"--super"));
    }

    #[cfg(unix)]
    #[test]
    fn remote_fallback_forwards_info_flags() {
        use tempfile::tempdir;

        let _env_lock = ENV_LOCK.lock().expect("env lock");
        let _rsh_guard = clear_rsync_rsh();
        let temp = tempdir().expect("tempdir");
        let script_path = temp.path().join("fallback.sh");
        let args_path = temp.path().join("args.txt");

        let script = r#"#!/bin/sh
printf "%s\n" "$@" > "$ARGS_FILE"
exit 0
"#;
        write_executable_script(&script_path, script);

        let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
        let _args_guard = EnvGuard::set("ARGS_FILE", args_path.as_os_str());

        let destination = temp.path().join("dest");
        let (code, stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
            OsString::from("--info=progress2"),
            OsString::from("remote::module"),
            destination.into_os_string(),
        ]);

        assert_eq!(code, 0);
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());

        let recorded = std::fs::read_to_string(&args_path).expect("read args file");
        let args: Vec<&str> = recorded.lines().collect();
        assert!(args.contains(&"--info=progress2"));
    }

    #[cfg(unix)]
    #[test]
    fn remote_fallback_forwards_no_protect_args_alias() {
        use tempfile::tempdir;

        let _env_lock = ENV_LOCK.lock().expect("env lock");
        let _rsh_guard = clear_rsync_rsh();
        let temp = tempdir().expect("tempdir");
        let script_path = temp.path().join("fallback.sh");
        let args_path = temp.path().join("args.txt");

        let script = r#"#!/bin/sh
printf "%s\n" "$@" > "$ARGS_FILE"
exit 0
"#;
        write_executable_script(&script_path, script);

        let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
        let _args_guard = EnvGuard::set("ARGS_FILE", args_path.as_os_str());

        let destination = temp.path().join("dest");
        let (code, stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
            OsString::from("--no-secluded-args"),
            OsString::from("remote::module"),
            destination.into_os_string(),
        ]);

        assert_eq!(code, 0);
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());

        let recorded = std::fs::read_to_string(&args_path).expect("read args file");
        let args: Vec<&str> = recorded.lines().collect();
        assert!(args.contains(&"--no-protect-args"));
        assert!(!args.contains(&"--protect-args"));
    }

    #[cfg(unix)]
    #[test]
    fn remote_fallback_respects_env_protect_args() {
        use tempfile::tempdir;

        let _env_lock = ENV_LOCK.lock().expect("env lock");
        let _rsh_guard = clear_rsync_rsh();
        let temp = tempdir().expect("tempdir");
        let script_path = temp.path().join("fallback.sh");
        let args_path = temp.path().join("args.txt");

        let script = r#"#!/bin/sh
printf "%s\n" "$@" > "$ARGS_FILE"
exit 0
"#;
        write_executable_script(&script_path, script);

        let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
        let _args_guard = EnvGuard::set("ARGS_FILE", args_path.as_os_str());
        let _protect_guard = EnvGuard::set("RSYNC_PROTECT_ARGS", OsStr::new("1"));

        let destination = temp.path().join("dest");
        let (code, stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
            OsString::from("remote::module"),
            destination.into_os_string(),
        ]);

        assert_eq!(code, 0);
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());

        let recorded = std::fs::read_to_string(&args_path).expect("read args file");
        let args: Vec<&str> = recorded.lines().collect();
        assert!(args.contains(&"--protect-args"));
        assert!(args.contains(&"--info=progress2"));
    }

    #[cfg(unix)]
    #[test]
    fn remote_fallback_respects_zero_compress_level() {
        use tempfile::tempdir;

        let _env_lock = ENV_LOCK.lock().expect("env lock");
        let _rsh_guard = clear_rsync_rsh();
        let temp = tempdir().expect("tempdir");
        let script_path = temp.path().join("fallback.sh");
        let args_path = temp.path().join("args.txt");

        let script = r#"#!/bin/sh
printf "%s\n" "$@" > "$ARGS_FILE"
exit 0
"#;
        write_executable_script(&script_path, script);

        let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
        let _args_guard = EnvGuard::set("ARGS_FILE", args_path.as_os_str());

        let (code, stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
            OsString::from("--compress-level=0"),
            OsString::from("remote::module"),
            OsString::from("dest"),
        ]);

        assert_eq!(code, 0);
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());

        let recorded = std::fs::read_to_string(&args_path).expect("read args file");
        let args: Vec<&str> = recorded.lines().collect();
        assert!(!args.contains(&"--compress"));
        assert!(args.contains(&"--compress-level"));
        assert!(args.contains(&"0"));
        assert!(!args.contains(&"--whole-file"));
        assert!(args.contains(&"--no-whole-file"));
    }

    #[cfg(unix)]
    #[test]
    fn local_delta_transfer_executes_locally() {
        use tempfile::tempdir;

        let _env_lock = ENV_LOCK.lock().expect("env lock");
        let _rsh_guard = clear_rsync_rsh();
        let temp = tempdir().expect("tempdir");
        let script_path = temp.path().join("fallback.sh");
        let args_path = temp.path().join("args.txt");

        let script = r#"#!/bin/sh
printf "%s\n" "$@" > "$ARGS_FILE"
exit 0
"#;
        write_executable_script(&script_path, script);

        let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
        let _args_guard = EnvGuard::set("ARGS_FILE", args_path.as_os_str());

        std::fs::write(&args_path, b"untouched").expect("seed args file");

        let source = temp.path().join("source.txt");
        let destination = temp.path().join("dest.txt");
        std::fs::write(&source, b"contents").expect("write source");

        let (code, stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
            OsString::from("--no-whole-file"),
            source.clone().into_os_string(),
            destination.clone().into_os_string(),
        ]);

        assert_eq!(code, 0);
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());
        assert_eq!(
            std::fs::read(&destination).expect("read destination"),
            b"contents"
        );

        let recorded = std::fs::read_to_string(&args_path).expect("read args file");
        assert_eq!(recorded, "untouched");
    }

    #[cfg(unix)]
    #[test]
    fn remote_fallback_forwards_connection_options() {
        use tempfile::tempdir;

        let _env_lock = ENV_LOCK.lock().expect("env lock");
        let _rsh_guard = clear_rsync_rsh();
        let temp = tempdir().expect("tempdir");
        let script_path = temp.path().join("fallback.sh");
        let args_path = temp.path().join("args.txt");
        let password_path = temp.path().join("password.txt");
        std::fs::write(&password_path, b"secret\n").expect("write password");
        let mut permissions = std::fs::metadata(&password_path)
            .expect("password metadata")
            .permissions();
        permissions.set_mode(0o600);
        std::fs::set_permissions(&password_path, permissions).expect("set password perms");

        let script = r#"#!/bin/sh
printf "%s\n" "$@" > "$ARGS_FILE"
exit 0
"#;
        write_executable_script(&script_path, script);

        let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
        let _args_guard = EnvGuard::set("ARGS_FILE", args_path.as_os_str());

        let dest_path = temp.path().join("dest");
        let (code, stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
            OsString::from(format!("--password-file={}", password_path.display())),
            OsString::from("--protocol=30"),
            OsString::from("--timeout=120"),
            OsString::from("--contimeout=75"),
            OsString::from("rsync://remote/module"),
            dest_path.clone().into_os_string(),
        ]);

        assert_eq!(code, 0);
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());

        let password_display = password_path.display().to_string();
        let dest_display = dest_path.display().to_string();
        let recorded = std::fs::read_to_string(&args_path).expect("read args file");
        assert!(recorded.contains("--password-file"));
        assert!(recorded.contains(&password_display));
        assert!(recorded.contains("--protocol"));
        assert!(recorded.contains("30"));
        assert!(recorded.contains("--timeout"));
        assert!(recorded.contains("120"));
        assert!(recorded.contains("--contimeout"));
        assert!(recorded.contains("75"));
        assert!(recorded.contains("rsync://remote/module"));
        assert!(recorded.contains(&dest_display));
    }

    #[test]
    fn remote_fallback_forwards_rsh_option() {
        use tempfile::tempdir;

        let _env_lock = ENV_LOCK.lock().expect("env lock");
        let _rsh_guard = clear_rsync_rsh();
        let temp = tempdir().expect("tempdir");
        let script_path = temp.path().join("fallback.sh");
        let args_path = temp.path().join("args.txt");

        let script = r#"#!/bin/sh
printf "%s\n" "$@" > "$ARGS_FILE"
exit 0
"#;
        write_executable_script(&script_path, script);

        let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
        let _args_guard = EnvGuard::set("ARGS_FILE", args_path.as_os_str());

        let dest_path = temp.path().join("dest");
        let (code, stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
            OsString::from("-e"),
            OsString::from("ssh -p 2222"),
            OsString::from("remote::module"),
            dest_path.clone().into_os_string(),
        ]);

        assert_eq!(code, 0);
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());

        let recorded = std::fs::read_to_string(&args_path).expect("read args file");
        assert!(recorded.lines().any(|line| line == "-e"));
        assert!(recorded.lines().any(|line| line == "ssh -p 2222"));
    }

    #[test]
    fn remote_fallback_forwards_rsync_path_option() {
        use tempfile::tempdir;

        let _env_lock = ENV_LOCK.lock().expect("env lock");
        let _rsh_guard = clear_rsync_rsh();
        let temp = tempdir().expect("tempdir");
        let script_path = temp.path().join("fallback.sh");
        let args_path = temp.path().join("args.txt");

        let script = r#"#!/bin/sh
printf "%s\n" "$@" > "$ARGS_FILE"
exit 0
"#;
        write_executable_script(&script_path, script);

        let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
        let _args_guard = EnvGuard::set("ARGS_FILE", args_path.as_os_str());

        let dest_path = temp.path().join("dest");
        let (code, stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
            OsString::from("--rsync-path=/opt/custom/rsync"),
            OsString::from("remote::module"),
            dest_path.clone().into_os_string(),
        ]);

        assert_eq!(code, 0);
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());

        let recorded = std::fs::read_to_string(&args_path).expect("read args file");
        assert!(recorded.lines().any(|line| line == "--rsync-path"));
        assert!(recorded.lines().any(|line| line == "/opt/custom/rsync"));
    }

    #[test]
    fn remote_fallback_forwards_connect_program_option() {
        use tempfile::tempdir;

        let _env_lock = ENV_LOCK.lock().expect("env lock");
        let _rsh_guard = clear_rsync_rsh();
        let temp = tempdir().expect("tempdir");
        let script_path = temp.path().join("fallback.sh");
        let args_path = temp.path().join("args.txt");

        let script = r#"#!/bin/sh
printf "%s\n" "$@" > "$ARGS_FILE"
exit 0
"#;
        write_executable_script(&script_path, script);

        let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
        let _args_guard = EnvGuard::set("ARGS_FILE", args_path.as_os_str());

        let dest_path = temp.path().join("dest");
        let (code, stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
            OsString::from("--connect-program=nc %H %P"),
            OsString::from("remote::module"),
            dest_path.clone().into_os_string(),
        ]);

        assert_eq!(code, 0);
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());

        let recorded = std::fs::read_to_string(&args_path).expect("read args file");
        assert!(recorded.lines().any(|line| line == "--connect-program"));
        assert!(recorded.lines().any(|line| line == "nc %H %P"));
    }

    #[test]
    fn remote_fallback_forwards_port_option() {
        use tempfile::tempdir;

        let _env_lock = ENV_LOCK.lock().expect("env lock");
        let _rsh_guard = clear_rsync_rsh();
        let temp = tempdir().expect("tempdir");
        let script_path = temp.path().join("fallback.sh");
        let args_path = temp.path().join("args.txt");

        let script = r#"#!/bin/sh
printf "%s\n" "$@" > "$ARGS_FILE"
exit 0
"#;
        write_executable_script(&script_path, script);

        let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
        let _args_guard = EnvGuard::set("ARGS_FILE", args_path.as_os_str());

        let dest_path = temp.path().join("dest");
        let (code, stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
            OsString::from("--port=10873"),
            OsString::from("remote::module"),
            dest_path.clone().into_os_string(),
        ]);

        assert_eq!(code, 0);
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());

        let recorded = std::fs::read_to_string(&args_path).expect("read args file");
        assert!(recorded.lines().any(|line| line == "--port=10873"));
    }

    #[test]
    fn remote_fallback_forwards_remote_option() {
        use tempfile::tempdir;

        let _env_lock = ENV_LOCK.lock().expect("env lock");
        let _rsh_guard = clear_rsync_rsh();
        let temp = tempdir().expect("tempdir");
        let script_path = temp.path().join("fallback.sh");
        let args_path = temp.path().join("args.txt");

        let script = r#"#!/bin/sh
printf "%s\n" "$@" > "$ARGS_FILE"
exit 0
"#;
        write_executable_script(&script_path, script);

        let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
        let _args_guard = EnvGuard::set("ARGS_FILE", args_path.as_os_str());

        let dest_path = temp.path().join("dest");
        let (code, stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
            OsString::from("--remote-option=--log-file=/tmp/rsync.log"),
            OsString::from("remote::module"),
            dest_path.clone().into_os_string(),
        ]);

        assert_eq!(code, 0);
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());

        let recorded = std::fs::read_to_string(&args_path).expect("read args file");
        assert!(recorded.lines().any(|line| line == "--remote-option"));
        assert!(
            recorded
                .lines()
                .any(|line| line == "--log-file=/tmp/rsync.log")
        );
    }

    #[test]
    fn remote_fallback_forwards_remote_option_short_flag() {
        use tempfile::tempdir;

        let _env_lock = ENV_LOCK.lock().expect("env lock");
        let _rsh_guard = clear_rsync_rsh();
        let temp = tempdir().expect("tempdir");
        let script_path = temp.path().join("fallback.sh");
        let args_path = temp.path().join("args.txt");

        let script = r#"#!/bin/sh
printf "%s\n" "$@" > "$ARGS_FILE"
exit 0
"#;
        write_executable_script(&script_path, script);

        let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
        let _args_guard = EnvGuard::set("ARGS_FILE", args_path.as_os_str());

        let dest_path = temp.path().join("dest");
        let (code, stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
            OsString::from("-M"),
            OsString::from("--log-format=%n"),
            OsString::from("remote::module"),
            dest_path.clone().into_os_string(),
        ]);

        assert_eq!(code, 0);
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());

        let recorded = std::fs::read_to_string(&args_path).expect("read args file");
        assert!(recorded.lines().any(|line| line == "--remote-option"));
        assert!(recorded.lines().any(|line| line == "--log-format=%n"));
    }

    #[test]
    fn remote_fallback_reads_rsync_rsh_env() {
        use tempfile::tempdir;

        let _env_lock = ENV_LOCK.lock().expect("env lock");
        let _clear_guard = clear_rsync_rsh();
        let _env_guard = EnvGuard::set("RSYNC_RSH", OsStr::new("ssh -p 2200"));
        let temp = tempdir().expect("tempdir");
        let script_path = temp.path().join("fallback.sh");
        let args_path = temp.path().join("args.txt");

        let script = r#"#!/bin/sh
printf "%s\n" "$@" > "$ARGS_FILE"
exit 0
"#;
        write_executable_script(&script_path, script);

        let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
        let _args_guard = EnvGuard::set("ARGS_FILE", args_path.as_os_str());

        let dest_path = temp.path().join("dest");
        let (code, stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
            OsString::from("remote::module"),
            dest_path.clone().into_os_string(),
        ]);

        assert_eq!(code, 0);
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());

        let recorded = std::fs::read_to_string(&args_path).expect("read args file");
        assert!(recorded.lines().any(|line| line == "-e"));
        assert!(recorded.lines().any(|line| line == "ssh -p 2200"));
    }

    #[test]
    fn remote_fallback_cli_rsh_overrides_env() {
        use tempfile::tempdir;

        let _env_lock = ENV_LOCK.lock().expect("env lock");
        let _clear_guard = clear_rsync_rsh();
        let _env_guard = EnvGuard::set("RSYNC_RSH", OsStr::new("ssh -p 2200"));
        let temp = tempdir().expect("tempdir");
        let script_path = temp.path().join("fallback.sh");
        let args_path = temp.path().join("args.txt");

        let script = r#"#!/bin/sh
printf "%s\n" "$@" > "$ARGS_FILE"
exit 0
"#;
        write_executable_script(&script_path, script);

        let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
        let _args_guard = EnvGuard::set("ARGS_FILE", args_path.as_os_str());

        let dest_path = temp.path().join("dest");
        let (code, stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
            OsString::from("-e"),
            OsString::from("ssh -p 2222"),
            OsString::from("remote::module"),
            dest_path.clone().into_os_string(),
        ]);

        assert_eq!(code, 0);
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());

        let recorded = std::fs::read_to_string(&args_path).expect("read args file");
        assert!(recorded.lines().any(|line| line == "ssh -p 2222"));
        assert!(!recorded.lines().any(|line| line == "ssh -p 2200"));
    }

    #[test]
    fn rsync_path_requires_remote_operands() {
        use tempfile::tempdir;

        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source.txt");
        let dest = temp.path().join("dest.txt");
        std::fs::write(&source, b"content").expect("write source");

        let (code, stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
            OsString::from("--rsync-path=/opt/custom/rsync"),
            source.clone().into_os_string(),
            dest.clone().into_os_string(),
        ]);

        assert_eq!(code, 1);
        assert!(stdout.is_empty());
        let message = String::from_utf8(stderr).expect("stderr utf8");
        assert!(
            message.contains("the --rsync-path option may only be used with remote connections")
        );
        assert!(!dest.exists());
    }

    #[test]
    fn connect_program_requires_remote_operands() {
        use tempfile::tempdir;

        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source.txt");
        let dest = temp.path().join("dest.txt");
        std::fs::write(&source, b"content").expect("write source");

        let (code, stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
            OsString::from("--connect-program=/usr/bin/nc %H %P"),
            source.clone().into_os_string(),
            dest.clone().into_os_string(),
        ]);

        assert_eq!(code, 1);
        assert!(stdout.is_empty());
        let message = String::from_utf8(stderr).expect("stderr utf8");
        assert!(message.contains(
            "the --connect-program option may only be used when accessing an rsync daemon"
        ));
        assert!(!dest.exists());
    }

    #[test]
    fn remote_option_requires_remote_operands() {
        use tempfile::tempdir;

        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source.txt");
        let dest = temp.path().join("dest.txt");
        std::fs::write(&source, b"content").expect("write source");

        let (code, stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
            OsString::from("--remote-option=--log-file=/tmp/rsync.log"),
            source.clone().into_os_string(),
            dest.clone().into_os_string(),
        ]);

        assert_eq!(code, 1);
        assert!(stdout.is_empty());
        let message = String::from_utf8(stderr).expect("stderr utf8");
        assert!(
            message.contains("the --remote-option option may only be used with remote connections")
        );
        assert!(!dest.exists());
    }

    #[cfg(all(unix, feature = "acl"))]
    #[test]
    fn remote_fallback_forwards_acls_toggle() {
        use tempfile::tempdir;

        let _env_lock = ENV_LOCK.lock().expect("env lock");
        let _rsh_guard = clear_rsync_rsh();
        let temp = tempdir().expect("tempdir");
        let script_path = temp.path().join("fallback.sh");
        let args_path = temp.path().join("args.txt");

        let script = r#"#!/bin/sh
printf "%s\n" "$@" > "$ARGS_FILE"
exit 0
"#;
        write_executable_script(&script_path, script);

        let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
        let _args_guard = EnvGuard::set("ARGS_FILE", args_path.as_os_str());

        let dest_path = temp.path().join("dest");
        let (code, stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
            OsString::from("--acls"),
            OsString::from("remote::module"),
            dest_path.clone().into_os_string(),
        ]);

        assert_eq!(code, 0);
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());

        let recorded = std::fs::read_to_string(&args_path).expect("read args file");
        assert!(recorded.contains("--acls"));
        assert!(!recorded.contains("--no-acls"));
    }

    #[test]
    fn remote_fallback_forwards_omit_dir_times_toggle() {
        use tempfile::tempdir;

        let _env_lock = ENV_LOCK.lock().expect("env lock");
        let _rsh_guard = clear_rsync_rsh();
        let temp = tempdir().expect("tempdir");
        let script_path = temp.path().join("fallback.sh");
        let args_path = temp.path().join("args.txt");

        let script = r#"#!/bin/sh
printf "%s\n" "$@" > "$ARGS_FILE"
exit 0
"#;
        write_executable_script(&script_path, script);

        let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
        let _args_guard = EnvGuard::set("ARGS_FILE", args_path.as_os_str());

        let dest_path = temp.path().join("dest");
        let (code, stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
            OsString::from("--omit-dir-times"),
            OsString::from("remote::module"),
            dest_path.clone().into_os_string(),
        ]);

        assert_eq!(code, 0);
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());

        let recorded = std::fs::read_to_string(&args_path).expect("read args file");
        assert!(recorded.contains("--omit-dir-times"));
        assert!(!recorded.contains("--no-omit-dir-times"));
    }

    #[test]
    fn remote_fallback_forwards_omit_link_times_toggle() {
        use tempfile::tempdir;

        let _env_lock = ENV_LOCK.lock().expect("env lock");
        let _rsh_guard = clear_rsync_rsh();
        let temp = tempdir().expect("tempdir");
        let script_path = temp.path().join("fallback.sh");
        let args_path = temp.path().join("args.txt");

        let script = r#"#!/bin/sh
printf "%s\n" "$@" > "$ARGS_FILE"
exit 0
"#;
        write_executable_script(&script_path, script);

        let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
        let _args_guard = EnvGuard::set("ARGS_FILE", args_path.as_os_str());

        let dest_path = temp.path().join("dest");
        let (code, stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
            OsString::from("--omit-link-times"),
            OsString::from("remote::module"),
            dest_path.clone().into_os_string(),
        ]);

        assert_eq!(code, 0);
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());

        let recorded = std::fs::read_to_string(&args_path).expect("read args file");
        assert!(recorded.contains("--omit-link-times"));
        assert!(!recorded.contains("--no-omit-link-times"));
    }

    #[cfg(all(unix, feature = "acl"))]
    #[test]
    fn remote_fallback_forwards_no_acls_toggle() {
        use tempfile::tempdir;

        let _env_lock = ENV_LOCK.lock().expect("env lock");
        let _rsh_guard = clear_rsync_rsh();
        let temp = tempdir().expect("tempdir");
        let script_path = temp.path().join("fallback.sh");
        let args_path = temp.path().join("args.txt");

        let script = r#"#!/bin/sh
printf "%s\n" "$@" > "$ARGS_FILE"
exit 0
"#;
        write_executable_script(&script_path, script);

        let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
        let _args_guard = EnvGuard::set("ARGS_FILE", args_path.as_os_str());

        let dest_path = temp.path().join("dest");
        let (code, stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
            OsString::from("--no-acls"),
            OsString::from("remote::module"),
            dest_path.clone().into_os_string(),
        ]);

        assert_eq!(code, 0);
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());

        let recorded = std::fs::read_to_string(&args_path).expect("read args file");
        assert!(recorded.contains("--no-acls"));
    }

    #[test]
    fn remote_fallback_forwards_no_omit_dir_times_toggle() {
        use tempfile::tempdir;

        let _env_lock = ENV_LOCK.lock().expect("env lock");
        let _rsh_guard = clear_rsync_rsh();
        let temp = tempdir().expect("tempdir");
        let script_path = temp.path().join("fallback.sh");
        let args_path = temp.path().join("args.txt");

        let script = r#"#!/bin/sh
printf "%s\n" "$@" > "$ARGS_FILE"
exit 0
"#;
        write_executable_script(&script_path, script);

        let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
        let _args_guard = EnvGuard::set("ARGS_FILE", args_path.as_os_str());

        let dest_path = temp.path().join("dest");
        let (code, stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
            OsString::from("--no-omit-dir-times"),
            OsString::from("remote::module"),
            dest_path.clone().into_os_string(),
        ]);

        assert_eq!(code, 0);
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());

        let recorded = std::fs::read_to_string(&args_path).expect("read args file");
        assert!(recorded.contains("--no-omit-dir-times"));
    }

    #[test]
    fn remote_fallback_forwards_no_omit_link_times_toggle() {
        use tempfile::tempdir;

        let _env_lock = ENV_LOCK.lock().expect("env lock");
        let _rsh_guard = clear_rsync_rsh();
        let temp = tempdir().expect("tempdir");
        let script_path = temp.path().join("fallback.sh");
        let args_path = temp.path().join("args.txt");

        let script = r#"#!/bin/sh
printf "%s\n" "$@" > "$ARGS_FILE"
exit 0
"#;
        write_executable_script(&script_path, script);

        let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
        let _args_guard = EnvGuard::set("ARGS_FILE", args_path.as_os_str());

        let dest_path = temp.path().join("dest");
        let (code, stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
            OsString::from("--no-omit-link-times"),
            OsString::from("remote::module"),
            dest_path.clone().into_os_string(),
        ]);

        assert_eq!(code, 0);
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());

        let recorded = std::fs::read_to_string(&args_path).expect("read args file");
        assert!(recorded.contains("--no-omit-link-times"));
    }

    #[test]
    fn remote_fallback_forwards_one_file_system_toggles() {
        use tempfile::tempdir;

        let _env_lock = ENV_LOCK.lock().expect("env lock");
        let _rsh_guard = clear_rsync_rsh();
        let temp = tempdir().expect("tempdir");
        let script_path = temp.path().join("fallback.sh");
        let args_path = temp.path().join("args.txt");

        let script = r#"#!/bin/sh
printf "%s\n" "$@" > "$ARGS_FILE"
exit 0
"#;
        write_executable_script(&script_path, script);

        let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
        let _args_guard = EnvGuard::set("ARGS_FILE", args_path.as_os_str());

        let dest_path = temp.path().join("dest");
        let (code, stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
            OsString::from("--one-file-system"),
            OsString::from("remote::module"),
            dest_path.clone().into_os_string(),
        ]);

        assert_eq!(code, 0);
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());

        let recorded = std::fs::read_to_string(&args_path).expect("read args file");
        assert!(recorded.contains("--one-file-system"));

        let (code, stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
            OsString::from("--no-one-file-system"),
            OsString::from("remote::module"),
            dest_path.into_os_string(),
        ]);

        assert_eq!(code, 0);
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());

        let recorded = std::fs::read_to_string(&args_path).expect("read args file");
        assert!(recorded.contains("--no-one-file-system"));
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
            OsString::from(RSYNC),
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
        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;

            let link_path = source_dir.join("link.txt");
            symlink("file.txt", &link_path).expect("create symlink");
        }
        let destination_dir = tmp.path().join("dest");
        fs::create_dir(&destination_dir).expect("create dest dir");

        let (code, stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
            OsString::from("--list-only"),
            source_dir.clone().into_os_string(),
            destination_dir.clone().into_os_string(),
        ]);

        assert_eq!(code, 0);
        assert!(stderr.is_empty());
        let rendered = String::from_utf8(stdout).expect("utf8 stdout");
        assert!(rendered.contains("file.txt"));
        #[cfg(unix)]
        {
            assert!(rendered.contains("link.txt -> file.txt"));
        }
        assert!(!destination_dir.join("file.txt").exists());
    }

    #[test]
    fn list_only_formats_directory_without_trailing_slash() {
        use std::fs;
        use tempfile::tempdir;

        let tmp = tempdir().expect("tempdir");
        let source_dir = tmp.path().join("src");
        let dest_dir = tmp.path().join("dst");
        fs::create_dir(&source_dir).expect("create src dir");
        fs::create_dir(&dest_dir).expect("create dest dir");

        let (code, stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
            OsString::from("--list-only"),
            source_dir.clone().into_os_string(),
            dest_dir.into_os_string(),
        ]);

        assert_eq!(code, 0);
        assert!(stderr.is_empty());
        let rendered = String::from_utf8(stdout).expect("utf8 stdout");

        let mut directory_line = None;
        for line in rendered.lines() {
            if line.ends_with("src") {
                directory_line = Some(line.to_string());
                break;
            }
        }

        let directory_line = directory_line.expect("directory entry present");
        assert!(directory_line.starts_with('d'));
        assert!(!directory_line.ends_with('/'));
    }

    #[test]
    fn list_only_matches_rsync_format_for_regular_file() {
        use filetime::{FileTime, set_file_times};
        use std::fs;
        use std::os::unix::fs::PermissionsExt;
        use tempfile::tempdir;

        let tmp = tempdir().expect("tempdir");
        let source_dir = tmp.path().join("src");
        let dest_dir = tmp.path().join("dst");
        fs::create_dir(&source_dir).expect("create src dir");
        fs::create_dir(&dest_dir).expect("create dest dir");

        let file_path = source_dir.join("data.bin");
        fs::write(&file_path, vec![0u8; 1_234]).expect("write source file");
        fs::set_permissions(&file_path, fs::Permissions::from_mode(0o644))
            .expect("set file permissions");

        let timestamp = FileTime::from_unix_time(1_700_000_000, 0);
        set_file_times(&file_path, timestamp, timestamp).expect("set file times");

        let mut source_arg = source_dir.clone().into_os_string();
        source_arg.push(std::path::MAIN_SEPARATOR.to_string());

        let (code, stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
            OsString::from("--list-only"),
            source_arg,
            dest_dir.clone().into_os_string(),
        ]);

        assert_eq!(code, 0);
        assert!(stderr.is_empty());

        let rendered = String::from_utf8(stdout).expect("utf8 stdout");
        let file_line = rendered
            .lines()
            .find(|line| line.ends_with("data.bin"))
            .expect("file entry present");

        let expected_permissions = "-rw-r--r--";
        let expected_size = format_list_size(1_234, HumanReadableMode::Disabled);
        let system_time = SystemTime::UNIX_EPOCH
            + Duration::from_secs(
                u64::try_from(timestamp.unix_seconds()).expect("positive timestamp"),
            )
            + Duration::from_nanos(u64::from(timestamp.nanoseconds()));
        let expected_timestamp = format_list_timestamp(Some(system_time));
        let expected =
            format!("{expected_permissions} {expected_size} {expected_timestamp} data.bin");

        assert_eq!(file_line, expected);

        let mut source_arg = source_dir.into_os_string();
        source_arg.push(std::path::MAIN_SEPARATOR.to_string());

        let (code, stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
            OsString::from("--list-only"),
            OsString::from("--human-readable"),
            source_arg,
            dest_dir.into_os_string(),
        ]);

        assert_eq!(code, 0);
        assert!(stderr.is_empty());

        let rendered = String::from_utf8(stdout).expect("utf8 stdout");
        let human_line = rendered
            .lines()
            .find(|line| line.ends_with("data.bin"))
            .expect("file entry present");
        let expected_human_size = format_list_size(1_234, HumanReadableMode::Enabled);
        let expected_human =
            format!("{expected_permissions} {expected_human_size} {expected_timestamp} data.bin");
        assert_eq!(human_line, expected_human);
    }

    #[test]
    fn list_only_formats_special_permission_bits_like_rsync() {
        use filetime::{FileTime, set_file_times};
        use std::fs;
        use std::os::unix::fs::PermissionsExt;
        use tempfile::tempdir;

        let tmp = tempdir().expect("tempdir");
        let source_dir = tmp.path().join("src");
        let dest_dir = tmp.path().join("dst");
        fs::create_dir(&source_dir).expect("create src dir");
        fs::create_dir(&dest_dir).expect("create dest dir");

        let sticky_exec = source_dir.join("exec-special");
        let sticky_plain = source_dir.join("plain-special");

        fs::write(&sticky_exec, b"exec").expect("write exec file");
        fs::write(&sticky_plain, b"plain").expect("write plain file");

        fs::set_permissions(&sticky_exec, fs::Permissions::from_mode(0o7777))
            .expect("set permissions with execute bits");
        fs::set_permissions(&sticky_plain, fs::Permissions::from_mode(0o7666))
            .expect("set permissions without execute bits");

        let timestamp = FileTime::from_unix_time(1_700_000_000, 0);
        set_file_times(&sticky_exec, timestamp, timestamp).expect("set exec times");
        set_file_times(&sticky_plain, timestamp, timestamp).expect("set plain times");

        let mut source_arg = source_dir.clone().into_os_string();
        source_arg.push(std::path::MAIN_SEPARATOR.to_string());

        let (code, stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
            OsString::from("--list-only"),
            source_arg,
            dest_dir.clone().into_os_string(),
        ]);

        assert_eq!(code, 0);
        assert!(stderr.is_empty());

        let rendered = String::from_utf8(stdout).expect("utf8 stdout");
        let system_time = SystemTime::UNIX_EPOCH
            + Duration::from_secs(
                u64::try_from(timestamp.unix_seconds()).expect("positive timestamp"),
            )
            + Duration::from_nanos(u64::from(timestamp.nanoseconds()));
        let expected_timestamp = format_list_timestamp(Some(system_time));

        let expected_exec = format!(
            "-rwsrwsrwt {} {expected_timestamp} exec-special",
            format_list_size(4, HumanReadableMode::Disabled)
        );
        let expected_plain = format!(
            "-rwSrwSrwT {} {expected_timestamp} plain-special",
            format_list_size(5, HumanReadableMode::Disabled)
        );

        let mut exec_line = None;
        let mut plain_line = None;
        for line in rendered.lines() {
            if line.ends_with("exec-special") {
                exec_line = Some(line.to_string());
            } else if line.ends_with("plain-special") {
                plain_line = Some(line.to_string());
            }
        }

        assert_eq!(exec_line.expect("exec entry"), expected_exec);
        assert_eq!(plain_line.expect("plain entry"), expected_plain);
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
            OsString::from(RSYNC),
            OsString::from("-n"),
            source.clone().into_os_string(),
            destination.clone().into_os_string(),
        ]);

        assert_eq!(code, 0);
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());
        assert!(!destination.exists());
    }

    #[cfg(unix)]
    #[test]
    fn verbose_output_includes_symlink_target() {
        use std::fs;
        use std::os::unix::fs::symlink;
        use tempfile::tempdir;

        let tmp = tempdir().expect("tempdir");
        let source_dir = tmp.path().join("src");
        fs::create_dir(&source_dir).expect("create src dir");
        let source_file = source_dir.join("file.txt");
        fs::write(&source_file, b"contents").expect("write source file");
        let link_path = source_dir.join("link.txt");
        symlink("file.txt", &link_path).expect("create symlink");

        let destination_dir = tmp.path().join("dest");
        fs::create_dir(&destination_dir).expect("create dest dir");

        let (code, stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
            OsString::from("-av"),
            source_dir.clone().into_os_string(),
            destination_dir.clone().into_os_string(),
        ]);

        assert_eq!(code, 0);
        assert!(stderr.is_empty());
        let rendered = String::from_utf8(stdout).expect("utf8 stdout");
        assert!(rendered.contains("link.txt -> file.txt"));
    }

    #[cfg(unix)]
    #[test]
    fn out_format_renders_symlink_target_placeholder() {
        use std::fs;
        use std::os::unix::fs::symlink;
        use tempfile::tempdir;

        let tmp = tempdir().expect("tempdir");
        let source_dir = tmp.path().join("src");
        fs::create_dir(&source_dir).expect("create src dir");
        let file = source_dir.join("file.txt");
        fs::write(&file, b"data").expect("write file");
        let link_path = source_dir.join("link.txt");
        symlink("file.txt", &link_path).expect("create symlink");

        let dest_dir = tmp.path().join("dst");
        fs::create_dir(&dest_dir).expect("create dst dir");

        let config = ClientConfig::builder()
            .transfer_args([
                source_dir.as_os_str().to_os_string(),
                dest_dir.as_os_str().to_os_string(),
            ])
            .force_event_collection(true)
            .build();

        let outcome =
            run_client_or_fallback::<io::Sink, io::Sink>(config, None, None).expect("run client");
        let summary = match outcome {
            ClientOutcome::Local(summary) => *summary,
            ClientOutcome::Fallback(_) => panic!("unexpected fallback outcome"),
        };
        let event = summary
            .events()
            .iter()
            .find(|event| event.relative_path().to_string_lossy().contains("link.txt"))
            .expect("symlink event present");

        let mut output = Vec::new();
        parse_out_format(OsStr::new("%L"))
            .expect("parse %L")
            .render(event, &OutFormatContext::default(), &mut output)
            .expect("render %L");

        assert_eq!(output, b" -> file.txt\n");
    }

    #[cfg(unix)]
    #[test]
    fn out_format_renders_combined_name_and_target_placeholder() {
        use std::fs;
        use std::os::unix::fs::symlink;
        use tempfile::tempdir;

        let tmp = tempdir().expect("tempdir");
        let source_dir = tmp.path().join("src");
        fs::create_dir(&source_dir).expect("create src dir");
        let file = source_dir.join("file.txt");
        fs::write(&file, b"data").expect("write file");
        let link_path = source_dir.join("link.txt");
        symlink("file.txt", &link_path).expect("create symlink");

        let dest_dir = tmp.path().join("dst");
        fs::create_dir(&dest_dir).expect("create dst dir");

        let config = ClientConfig::builder()
            .transfer_args([
                source_dir.as_os_str().to_os_string(),
                dest_dir.as_os_str().to_os_string(),
            ])
            .force_event_collection(true)
            .build();

        let outcome =
            run_client_or_fallback::<io::Sink, io::Sink>(config, None, None).expect("run client");
        let summary = match outcome {
            ClientOutcome::Local(summary) => *summary,
            ClientOutcome::Fallback(_) => panic!("unexpected fallback outcome"),
        };
        let event = summary
            .events()
            .iter()
            .find(|event| event.relative_path().to_string_lossy().contains("link.txt"))
            .expect("symlink event present");

        let mut output = Vec::new();
        parse_out_format(OsStr::new("%N"))
            .expect("parse %N")
            .render(event, &OutFormatContext::default(), &mut output)
            .expect("render %N");

        assert_eq!(output, b"src/link.txt -> file.txt\n");
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
            OsString::from(RSYNC),
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

    #[cfg(not(feature = "acl"))]
    #[test]
    fn acls_option_reports_unsupported_when_feature_disabled() {
        use tempfile::tempdir;

        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source.txt");
        let destination = temp.path().join("dest.txt");
        std::fs::write(&source, b"data").expect("write source");

        let (code, stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
            OsString::from("--acls"),
            source.into_os_string(),
            destination.into_os_string(),
        ]);

        assert_eq!(code, 1);
        assert!(stdout.is_empty());
        let rendered = String::from_utf8(stderr).expect("UTF-8 error");
        assert!(rendered.contains("POSIX ACLs are not supported on this client"));
        assert_contains_client_trailer(&rendered);
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
            OsString::from(RSYNC),
            OsString::from("--xattrs"),
            source.into_os_string(),
            destination.into_os_string(),
        ]);

        assert_eq!(code, 1);
        assert!(stdout.is_empty());
        let rendered = String::from_utf8(stderr).expect("UTF-8 error");
        assert!(rendered.contains("extended attributes are not supported on this client"));
        assert_contains_client_trailer(&rendered);
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
            OsString::from(RSYNC),
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

    #[test]
    fn format_size_combined_includes_exact_component() {
        assert_eq!(
            format_size(1_536, HumanReadableMode::Combined),
            "1.54K (1,536)"
        );
    }

    #[test]
    fn format_progress_rate_zero_bytes_matches_mode() {
        assert_eq!(
            format_progress_rate(0, Duration::from_secs(1), HumanReadableMode::Disabled),
            "0.00kB/s"
        );
        assert_eq!(
            format_progress_rate(0, Duration::from_secs(1), HumanReadableMode::Enabled),
            "0.00B/s"
        );
        assert_eq!(
            format_progress_rate(0, Duration::from_secs(1), HumanReadableMode::Combined),
            "0.00B/s"
        );
    }

    #[test]
    fn format_progress_rate_combined_includes_decimal_component() {
        let rendered = format_progress_rate(
            1_048_576,
            Duration::from_secs(1),
            HumanReadableMode::Combined,
        );
        assert_eq!(rendered, "1.05MB/s (1.00MB/s)");
    }
}
