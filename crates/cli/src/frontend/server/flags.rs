//! Server-mode long flag parsing.
//!
//! Extracts `--flag` and `--flag=value` arguments from the server argument
//! list into a structured representation.

use std::ffi::OsString;
use std::path::PathBuf;

use engine::{ReferenceDirectory, ReferenceDirectoryKind};

/// Detects whether secluded-args mode is requested in the server arguments.
///
/// In secluded-args mode, the client sends `-s` as a standalone argument
/// on the command line (not as part of a combined flag string). The server
/// then reads the full argument list from stdin before proceeding.
pub(crate) fn detect_secluded_args_flag(args: &[OsString]) -> bool {
    args.iter().skip(1).any(|a| a == "-s")
}

/// Long-form flags extracted from the server argument list.
///
/// These correspond to the `--flag` and `--flag=value` arguments that
/// upstream rsync's `server_options()` emits alongside the compact flag string.
/// upstream: options.c - `server_options()`.
pub(super) struct ServerLongFlags {
    pub(super) is_sender: bool,
    pub(super) is_receiver: bool,
    pub(super) ignore_errors: bool,
    pub(super) fsync: bool,
    pub(super) io_uring_policy: fast_io::IoUringPolicy,
    pub(super) write_devices: bool,
    pub(super) trust_sender: bool,
    pub(super) qsort: bool,
    pub(super) checksum_seed: Option<String>,
    pub(super) checksum_choice: Option<String>,
    pub(super) min_size: Option<String>,
    pub(super) max_size: Option<String>,
    pub(super) stop_at: Option<String>,
    pub(super) stop_after: Option<String>,
    pub(super) files_from: Option<String>,
    pub(super) from0: bool,
    pub(super) inplace: bool,
    pub(super) size_only: bool,
    /// Numeric IDs only (upstream: `--numeric-ids`, long-form only).
    pub(super) numeric_ids: bool,
    /// Delete extraneous files (upstream: `--delete-*` variants, long-form only).
    pub(super) delete: bool,
    /// Skip updating files that exist at destination (upstream: `--ignore-existing`).
    pub(super) ignore_existing: bool,
    /// Skip creating files not present at destination (upstream: `--existing`).
    pub(super) existing_only: bool,
    /// Maximum deletions allowed (upstream: `--max-delete=NUM`).
    pub(super) max_delete: Option<String>,
    /// Iconv specification forwarded by the client (upstream: `--iconv=CHARSET`).
    ///
    /// upstream: options.c:2716-2723 - client forwards the post-comma half of
    /// `--iconv=LOCAL,REMOTE` (or the whole spec if no comma) so the server
    /// opens its own iconv context against the wire's UTF-8 charset.
    pub(super) iconv: Option<String>,
    /// Reference directories for basis file lookup.
    /// upstream: options.c:2915-2923 - `--compare-dest`, `--copy-dest`, `--link-dest`
    pub(super) reference_directories: Vec<ReferenceDirectory>,
}

/// Parses all long-form flags from the server argument list.
///
/// Scans the argument list for `--flag` and `--flag=value` arguments,
/// extracting their values into a structured result. Unknown long flags
/// are ignored for forward compatibility.
pub(super) fn parse_server_long_flags(args: &[OsString]) -> ServerLongFlags {
    let mut flags = ServerLongFlags {
        is_sender: false,
        is_receiver: false,
        ignore_errors: false,
        fsync: false,
        io_uring_policy: fast_io::IoUringPolicy::Auto,
        write_devices: false,
        trust_sender: false,
        qsort: false,
        checksum_seed: None,
        checksum_choice: None,
        min_size: None,
        max_size: None,
        stop_at: None,
        stop_after: None,
        files_from: None,
        from0: false,
        inplace: false,
        size_only: false,
        numeric_ids: false,
        delete: false,
        ignore_existing: false,
        existing_only: false,
        max_delete: None,
        iconv: None,
        reference_directories: Vec::new(),
    };

    for arg in args {
        let s = arg.to_string_lossy();

        match s.as_ref() {
            "--sender" => flags.is_sender = true,
            "--receiver" => flags.is_receiver = true,
            "--ignore-errors" => flags.ignore_errors = true,
            "--fsync" => flags.fsync = true,
            "--io-uring" => flags.io_uring_policy = fast_io::IoUringPolicy::Enabled,
            "--no-io-uring" => flags.io_uring_policy = fast_io::IoUringPolicy::Disabled,
            "--write-devices" => flags.write_devices = true,
            "--trust-sender" => flags.trust_sender = true,
            "--qsort" => flags.qsort = true,
            "--from0" => flags.from0 = true,
            "--inplace" => flags.inplace = true,
            "--size-only" => flags.size_only = true,
            // upstream: --numeric-ids is long-form only (options.c:2887-2888)
            "--numeric-ids" => flags.numeric_ids = true,
            // upstream: --delete variants are long-form only (options.c:2818-2827)
            "--delete" | "--delete-before" | "--delete-during" | "--delete-after"
            | "--delete-delay" | "--delete-excluded" => flags.delete = true,
            // upstream: options.c:2831 - --ignore-existing sent as long-form arg
            "--ignore-existing" => flags.ignore_existing = true,
            // upstream: options.c:2833 - --existing (--ignore-non-existing) sent as long-form arg
            "--existing" | "--ignore-non-existing" => flags.existing_only = true,
            _ => {
                parse_value_bearing_flag(&s, &mut flags);
            }
        }
    }

    flags
}

/// Parses value-bearing `--flag=value` arguments into `ServerLongFlags`.
fn parse_value_bearing_flag(s: &str, flags: &mut ServerLongFlags) {
    if let Some(value) = s.strip_prefix("--checksum-seed=") {
        flags.checksum_seed = Some(value.to_owned());
    } else if let Some(value) = s.strip_prefix("--checksum-choice=") {
        flags.checksum_choice = Some(value.to_owned());
    } else if let Some(value) = s.strip_prefix("--min-size=") {
        flags.min_size = Some(value.to_owned());
    } else if let Some(value) = s.strip_prefix("--max-size=") {
        flags.max_size = Some(value.to_owned());
    } else if let Some(value) = s.strip_prefix("--stop-at=") {
        flags.stop_at = Some(value.to_owned());
    } else if let Some(value) = s.strip_prefix("--stop-after=") {
        flags.stop_after = Some(value.to_owned());
    } else if let Some(value) = s.strip_prefix("--files-from=") {
        flags.files_from = Some(value.to_owned());
    } else if let Some(value) = s.strip_prefix("--max-delete=") {
        flags.max_delete = Some(value.to_owned());
    } else if let Some(value) = s.strip_prefix("--iconv=") {
        // upstream: options.c:2716-2723 - server-side iconv forwarded by client
        flags.iconv = Some(value.to_owned());
    // upstream: options.c:2915-2923 - reference directory args
    } else if let Some(value) = s.strip_prefix("--compare-dest=") {
        flags.reference_directories.push(ReferenceDirectory::new(
            ReferenceDirectoryKind::Compare,
            PathBuf::from(value),
        ));
    } else if let Some(value) = s.strip_prefix("--copy-dest=") {
        flags.reference_directories.push(ReferenceDirectory::new(
            ReferenceDirectoryKind::Copy,
            PathBuf::from(value),
        ));
    } else if let Some(value) = s.strip_prefix("--link-dest=") {
        flags.reference_directories.push(ReferenceDirectory::new(
            ReferenceDirectoryKind::Link,
            PathBuf::from(value),
        ));
    }
}

/// Returns `true` when the argument is a known server-mode long flag.
///
/// Used by [`super::parse::parse_server_flag_string_and_args`] to skip long
/// flags when searching for the compact flag string.
pub(super) fn is_known_server_long_flag(arg: &str) -> bool {
    matches!(
        arg,
        "--server"
            | "--sender"
            | "--receiver"
            | "--ignore-errors"
            | "--fsync"
            | "--io-uring"
            | "--no-io-uring"
            | "--write-devices"
            | "--trust-sender"
            | "--qsort"
            | "--from0"
            | "--inplace"
            | "--size-only"
            | "--numeric-ids"
            | "--delete"
            | "--delete-before"
            | "--delete-during"
            | "--delete-after"
            | "--delete-delay"
            | "--delete-excluded"
            | "--ignore-existing"
            | "--existing"
            | "--ignore-non-existing"
    ) || arg == "-s"
        || arg.starts_with("--checksum-seed=")
        || arg.starts_with("--checksum-choice=")
        || arg.starts_with("--compare-dest=")
        || arg.starts_with("--copy-dest=")
        || arg.starts_with("--link-dest=")
        || arg.starts_with("--min-size=")
        || arg.starts_with("--max-size=")
        || arg.starts_with("--stop-at=")
        || arg.starts_with("--stop-after=")
        || arg.starts_with("--files-from=")
        || arg.starts_with("--max-delete=")
        || arg.starts_with("--iconv=")
}
