use std::ffi::OsString;
use std::path::PathBuf;

use core::client::{AddressMode, DeleteMode, HumanReadableMode, StrongChecksumChoice};

use super::bandwidth::BandwidthArgument;
use super::program_name::ProgramName;
use crate::frontend::progress::{NameOutputLevel, ProgressSetting};

/// Parsed command-line arguments for the rsync frontend.
///
/// Holds all recognized command-line options after parsing. Each field
/// corresponds to one or more rsync CLI options.
///
/// # Field conventions
///
/// - `bool` - enabled/disabled flag, default `false`.
/// - `Option<bool>` - tri-state: `None` (unset), `Some(true)` (explicit enable),
///   `Some(false)` (explicit disable via `--no-*`).
/// - `Option<T>` - optional value, `None` when unspecified.
/// - `Vec<T>` - accumulating option, empty when unspecified.
///
/// All fields are `pub` for integration test inspection via `cli::test_utils`.
/// Not part of the stable public API.
#[allow(clippy::struct_excessive_bools)]
#[allow(private_interfaces)] // ProgramName, BandwidthArgument are pub(crate) but exposed for tests
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParsedArgs {
    // ── General ──────────────────────────────────────────────────────
    /// Program name detected from `argv[0]`.
    pub program_name: ProgramName,

    /// `--help`, `-h`
    pub show_help: bool,

    /// `--version`, `-V`
    pub show_version: bool,

    /// `--human-readable`, `-h` (repeatable for higher levels).
    pub human_readable: Option<HumanReadableMode>,

    /// `--dry-run`, `-n` - trial run without changes.
    pub dry_run: bool,

    /// `--list-only` - list files instead of copying.
    pub list_only: bool,

    /// Remaining non-option arguments (sources and destination).
    pub remainder: Vec<OsString>,

    // ── Connection / Transport ───────────────────────────────────────
    /// `--rsh`, `-e` - remote shell command.
    pub remote_shell: Option<OsString>,

    /// `--connect-program` - program for direct daemon connection.
    pub connect_program: Option<OsString>,

    /// `--remote-option`, `-M` - extra options for the remote process.
    pub remote_options: Vec<OsString>,

    /// `--rsync-path` - path to rsync on the remote system.
    pub rsync_path: Option<OsString>,

    /// `--protect-args`, `-s` / `--no-protect-args` - prevent remote shell expansion.
    pub protect_args: Option<bool>,

    /// `--old-args` / `--no-old-args` - pre-3.0 argument passing.
    pub old_args: Option<bool>,

    /// `--ipv4`, `-4` / `--ipv6`, `-6` - address family preference.
    pub address_mode: AddressMode,

    /// `--address` - local bind address for outgoing connections.
    pub bind_address: Option<OsString>,

    /// `--sockopts` - comma-separated socket option settings.
    pub sockopts: Option<OsString>,

    /// `--blocking-io` / `--no-blocking-io` - blocking I/O for remote shell.
    pub blocking_io: Option<bool>,

    /// `--port` - TCP port for daemon connections (default 873).
    pub daemon_port: Option<u16>,

    /// `--protocol` - force a specific protocol version (28-32).
    pub protocol: Option<OsString>,

    /// `--timeout` - I/O timeout in seconds.
    pub timeout: Option<OsString>,

    /// `--contimeout` - connection establishment timeout in seconds.
    pub contimeout: Option<OsString>,

    /// `--stop-after` - stop transfer after the specified duration.
    pub stop_after: Option<OsString>,

    /// `--stop-at` - stop transfer at the specified wall-clock time.
    pub stop_at: Option<OsString>,

    // ── Server / Daemon ──────────────────────────────────────────────
    /// `--server` - run in server mode (internal, set by remote invocation).
    pub server_mode: bool,

    /// `--sender` - act as the sender in server mode (internal).
    pub sender_mode: bool,

    /// `--detach` / `--no-detach` - detach from controlling terminal.
    pub detach: Option<bool>,

    /// `--daemon` - run as a persistent rsync daemon.
    pub daemon_mode: bool,

    /// `--config` - path to daemon configuration file.
    pub config: Option<OsString>,

    /// `--dparam`, `-M` - daemon configuration overrides.
    pub dparam: Vec<OsString>,

    /// `--no-motd` - suppress daemon message of the day.
    pub no_motd: bool,

    /// `--password-file` - file containing daemon authentication password.
    pub password_file: Option<OsString>,

    // ── Recursion / Traversal ────────────────────────────────────────
    /// `--archive`, `-a` - shorthand for `-rlptgoD`.
    pub archive: bool,

    /// `--recursive`, `-r` - recurse into directories.
    pub recursive: bool,

    /// `--recursive` / `--no-recursive` - explicit override for recursion.
    pub recursive_override: Option<bool>,

    /// `--inc-recursive` / `--no-inc-recursive` - incremental recursion (scan-ahead).
    pub inc_recursive: Option<bool>,

    /// `--dirs`, `-d` / `--no-dirs` - transfer directories without recursing.
    pub dirs: Option<bool>,

    /// `--relative`, `-R` / `--no-relative` - preserve leading path components.
    pub relative: Option<bool>,

    /// `--one-file-system`, `-x` / `--no-one-file-system`.
    /// `None` = unset, `Some(0)` = disabled, `Some(1)` = skip cross-fs,
    /// `Some(2)` = also skip root-level mount points.
    pub one_file_system: Option<u8>,

    /// `--implied-dirs` / `--no-implied-dirs` - create parent dirs in relative mode.
    pub implied_dirs: Option<bool>,

    /// `--mkpath` - create missing destination path components.
    pub mkpath: bool,

    /// `--prune-empty-dirs`, `-m` / `--no-prune-empty-dirs`.
    pub prune_empty_dirs: Option<bool>,

    // ── Transfer Behavior ────────────────────────────────────────────
    /// `--checksum`, `-c` / `--no-checksum` - skip based on checksum, not mtime+size.
    pub checksum: Option<bool>,

    /// `--checksum-choice` - parsed checksum algorithm selection.
    pub checksum_choice: Option<StrongChecksumChoice>,

    /// `--checksum-choice` - raw argument preserved for remote forwarding.
    pub checksum_choice_arg: Option<OsString>,

    /// `--checksum-seed` - seed for reproducible checksums.
    pub checksum_seed: Option<u32>,

    /// `--size-only` - skip based on size only, ignore mtime.
    pub size_only: bool,

    /// `--ignore-times`, `-I` - always transfer regardless of timestamps.
    pub ignore_times: bool,

    /// `--ignore-existing` - skip files that exist on the destination.
    pub ignore_existing: bool,

    /// `--existing` - only update files that already exist on the destination.
    pub existing: bool,

    /// `--ignore-missing-args` - skip missing source arguments instead of erroring.
    pub ignore_missing_args: bool,

    /// `--update`, `-u` - skip files newer on the destination.
    pub update: bool,

    /// `--whole-file`, `-W` / `--no-whole-file` - disable delta-transfer algorithm.
    pub whole_file: Option<bool>,

    /// `--fuzzy`, `-y` / `--no-fuzzy` - find similar files for delta basis.
    /// `None` = unset, `Some(0)` = disabled, `Some(1)` = dest dir,
    /// `Some(2)` = also reference dirs.
    pub fuzzy: Option<u8>,

    /// `--inplace` / `--no-inplace` - write directly to destination files.
    pub inplace: Option<bool>,

    /// `--append` / `--no-append` - append data to shorter files.
    pub append: Option<bool>,

    /// `--append-verify` - append and verify existing content with checksums.
    pub append_verify: bool,

    /// `--remove-source-files` - delete source files after successful transfer.
    pub remove_source_files: bool,

    /// `--trust-sender` - trust the sending side's file list.
    pub trust_sender: bool,

    // ── Delete ───────────────────────────────────────────────────────
    /// `--delete`, `--delete-before`, `--delete-during`, `--delete-delay`,
    /// `--delete-after` - scheduling mode for extraneous file deletion.
    pub delete_mode: DeleteMode,

    /// `--delete-excluded` - also delete excluded files from the destination.
    pub delete_excluded: bool,

    /// `--delete-missing-args` - delete destination entries for missing source args.
    pub delete_missing_args: bool,

    /// `--ignore-errors` / `--no-ignore-errors` - continue deleting on I/O errors.
    pub ignore_errors: Option<bool>,

    /// `--max-delete` - limit on number of files to delete per run (-1 = report only).
    pub max_delete: Option<OsString>,

    /// `--force` / `--no-force` - force deletion of non-empty directories.
    pub force: Option<bool>,

    // ── Metadata / Permissions ───────────────────────────────────────
    /// `--owner`, `-o` / `--no-owner` - preserve file owner.
    pub owner: Option<bool>,

    /// `--group`, `-g` / `--no-group` - preserve file group.
    pub group: Option<bool>,

    /// `--chown` - set owner/group on destination (format: `USER:GROUP`).
    pub chown: Option<OsString>,

    /// `--copy-as` - run remote copy as a different user.
    pub copy_as: Option<OsString>,

    /// `--usermap` - map user names between source and destination.
    pub usermap: Option<OsString>,

    /// `--groupmap` - map group names between source and destination.
    pub groupmap: Option<OsString>,

    /// `--chmod` - permission modifications (symbolic or octal, repeatable).
    pub chmod: Vec<OsString>,

    /// `--perms`, `-p` / `--no-perms` - preserve file permissions.
    pub perms: Option<bool>,

    /// `--executability`, `-E` - preserve file executability.
    pub executability: Option<bool>,

    /// `--super` / `--no-super` - attempt privileged operations.
    pub super_mode: Option<bool>,

    /// `--fake-super` / `--no-fake-super` - store privileged attrs via xattrs.
    pub fake_super: Option<bool>,

    /// `--times`, `-t` / `--no-times` - preserve modification times.
    pub times: Option<bool>,

    /// `--omit-dir-times`, `-O` / `--no-omit-dir-times`.
    pub omit_dir_times: Option<bool>,

    /// `--omit-link-times`, `-J` / `--no-omit-link-times`.
    pub omit_link_times: Option<bool>,

    /// `--atimes`, `-U` / `--no-atimes` - preserve access times.
    pub atimes: Option<bool>,

    /// `--crtimes`, `-N` / `--no-crtimes` - preserve creation times (macOS/Windows).
    pub crtimes: Option<bool>,

    /// `--acls`, `-A` / `--no-acls` - preserve Access Control Lists.
    pub acls: Option<bool>,

    /// `--xattrs`, `-X` / `--no-xattrs` - preserve extended attributes.
    pub xattrs: Option<bool>,

    /// `--numeric-ids` / `--no-numeric-ids` - use numeric uid/gid instead of names.
    pub numeric_ids: Option<bool>,

    // ── Symlinks / Links ─────────────────────────────────────────────
    /// `--hard-links`, `-H` / `--no-hard-links` - preserve hard links.
    pub hard_links: Option<bool>,

    /// `--links`, `-l` / `--no-links` - copy symlinks as symlinks.
    pub links: Option<bool>,

    /// `--copy-links`, `-L` - follow and copy symlink referents.
    pub copy_links: Option<bool>,

    /// `--copy-dirlinks`, `-k` - follow and copy directory symlinks.
    pub copy_dirlinks: bool,

    /// `--copy-unsafe-links` - copy referents of unsafe (out-of-tree) symlinks.
    pub copy_unsafe_links: Option<bool>,

    /// `--keep-dirlinks`, `-K` - treat destination dir symlinks as directories.
    pub keep_dirlinks: Option<bool>,

    /// `--safe-links` - ignore symlinks pointing outside the source tree.
    pub safe_links: bool,

    /// `--munge-links` / `--no-munge-links` - munge symlinks for safety.
    pub munge_links: Option<bool>,

    // ── Devices / Specials ───────────────────────────────────────────
    /// `--devices` / `--no-devices` - preserve device files.
    pub devices: Option<bool>,

    /// `--specials` / `--no-specials` - preserve special files (sockets, FIFOs).
    pub specials: Option<bool>,

    /// `--copy-devices` - copy device file contents as regular files.
    pub copy_devices: bool,

    /// `--write-devices` / `--no-write-devices` - allow writing to device files.
    pub write_devices: Option<bool>,

    // ── Compression ──────────────────────────────────────────────────
    /// `--compress`, `-z` - enable transfer compression.
    pub compress: bool,

    /// `--no-compress` - explicitly disable compression.
    pub no_compress: bool,

    /// `--compress-level` - compression level (0-9).
    pub compress_level: Option<OsString>,

    /// `--compress-choice` - compression algorithm (e.g., `zlib`, `zstd`).
    pub compress_choice: Option<OsString>,

    /// `--old-compress` - force zlib compression.
    pub old_compress: bool,

    /// `--new-compress` - force newer compression methods.
    pub new_compress: bool,

    /// `--skip-compress` - file suffixes to skip compression for.
    pub skip_compress: Option<OsString>,

    // ── Backup ───────────────────────────────────────────────────────
    /// `--backup`, `-b` - make backups before overwriting.
    pub backup: bool,

    /// `--backup-dir` - directory to store backup files.
    pub backup_dir: Option<OsString>,

    /// `--suffix` - suffix for backup file names (default `~`).
    pub backup_suffix: Option<OsString>,

    // ── Filters / Patterns ───────────────────────────────────────────
    /// `--exclude` - patterns for excluding files.
    pub excludes: Vec<OsString>,

    /// `--include` - patterns for including files.
    pub includes: Vec<OsString>,

    /// `--exclude-from` - files containing exclude patterns.
    pub exclude_from: Vec<OsString>,

    /// `--include-from` - files containing include patterns.
    pub include_from: Vec<OsString>,

    /// `--filter`, `-f` - general filter rules.
    pub filters: Vec<OsString>,

    /// `--cvs-exclude`, `-C` - use CVS-style ignore patterns.
    pub cvs_exclude: bool,

    /// `--apple-double-skip` - exclude macOS AppleDouble (`._*`) sidecar files.
    pub apple_double_skip: bool,

    /// `-F` (repeatable) - rsync filter shortcut count.
    pub rsync_filter_shortcuts: usize,

    /// `--files-from` - files containing lists of source paths.
    pub files_from: Vec<OsString>,

    /// `--from0`, `-0` - use NUL as line separator in files-from.
    pub from0: bool,

    // ── Destination References ────────────────────────────────────────
    /// `--compare-dest` - directories to compare against when deciding transfers.
    pub compare_destinations: Vec<OsString>,

    /// `--copy-dest` - directories to copy from if file exists there.
    pub copy_destinations: Vec<OsString>,

    /// `--link-dest` - directories to hard-link from if file matches.
    pub link_destinations: Vec<OsString>,

    /// Resolved `--link-dest` paths for internal processing.
    pub link_dests: Vec<PathBuf>,

    // ── I/O / Performance ────────────────────────────────────────────
    /// `--bwlimit` - bandwidth limit (supports K, M, G suffixes).
    pub bwlimit: Option<BandwidthArgument>,

    /// `--min-size` - minimum file size to transfer.
    pub min_size: Option<OsString>,

    /// `--max-size` - maximum file size to transfer.
    pub max_size: Option<OsString>,

    /// `--block-size`, `-B` - block size for delta-transfer algorithm.
    pub block_size: Option<OsString>,

    /// `--modify-window` - timestamp comparison tolerance in seconds.
    pub modify_window: Option<OsString>,

    /// `--sparse`, `-S` / `--no-sparse` - handle sparse files efficiently.
    pub sparse: Option<bool>,

    /// `--open-noatime` - open files with `O_NOATIME`.
    pub open_noatime: bool,

    /// `--no-open-noatime` - explicitly disable `O_NOATIME`.
    pub no_open_noatime: bool,

    /// `--partial` - keep partially transferred files.
    pub partial: bool,

    /// `--partial-dir` - directory for partial files (implies `--partial`).
    pub partial_dir: Option<PathBuf>,

    /// `--preallocate` - pre-allocate disk space via `fallocate()`.
    pub preallocate: bool,

    /// `--fsync` - sync files to disk after writing.
    pub fsync: Option<bool>,

    /// `--io-uring` / `--no-io-uring` - io_uring policy for file I/O.
    pub io_uring_policy: fast_io::IoUringPolicy,

    /// `--delay-updates` - use temp files and rename after all transfers complete.
    pub delay_updates: bool,

    /// `--temp-dir`, `-T` - directory for temporary files during transfer.
    pub temp_dir: Option<PathBuf>,

    /// `--max-alloc` - maximum memory allocation limit.
    pub max_alloc: Option<OsString>,

    /// `--iconv` - character set conversion specification.
    pub iconv: Option<OsString>,

    /// `--no-iconv` - disable character set conversion.
    pub no_iconv: bool,

    /// `--qsort` - use qsort instead of merge-sort for file list sorting.
    pub qsort: bool,

    // ── Output / Formatting ──────────────────────────────────────────
    /// `--verbose`, `-v` / `--quiet`, `-q` - output verbosity level.
    pub verbosity: u8,

    /// `--progress`, `--info=progress2` / `--no-progress`.
    pub progress: ProgressSetting,

    /// File name output level during transfer.
    pub name_level: NameOutputLevel,

    /// Whether `name_level` was explicitly overridden by the user.
    pub name_overridden: bool,

    /// `--stats` - print transfer statistics at the end.
    pub stats: bool,

    /// `--8-bit-output`, `-8` - display high-bit characters as-is.
    pub eight_bit_output: bool,

    /// `--itemize-changes`, `-i` - output a change summary per file.
    pub itemize_changes: bool,

    /// `--out-format` - format string for file transfer messages.
    pub out_format: Option<OsString>,

    /// `--info` - fine-grained informational output control.
    pub info: Vec<OsString>,

    /// `--debug` - fine-grained debug output control.
    pub debug: Vec<OsString>,

    /// `--msgs2stderr` / `--no-msgs2stderr` - send info messages to stderr.
    pub msgs_to_stderr: Option<bool>,

    /// `--stderr` - stderr output mode (`errors`, `all`, `client`).
    pub stderr_mode: Option<OsString>,

    /// `--outbuf` - output buffering mode (`none`, `line`, `block`).
    pub outbuf: Option<OsString>,

    /// `--log-file` - path to write a log file.
    pub log_file: Option<OsString>,

    /// `--log-file-format` - format string for log file entries.
    pub log_file_format: Option<OsString>,

    // ── Batch ────────────────────────────────────────────────────────
    /// `--write-batch` - write a batch file for later replay.
    pub write_batch: Option<OsString>,

    /// `--only-write-batch` - write batch without performing the transfer.
    pub only_write_batch: Option<OsString>,

    /// `--read-batch` - read and apply a batch file.
    pub read_batch: Option<OsString>,

    /// `--early-input` - input file for early protocol stages.
    pub early_input: Option<OsString>,

    // ── SSH ──────────────────────────────────────────────────────────
    /// `--aes` - force AES-GCM cipher selection for SSH connections.
    /// `None` = use runtime hardware detection.
    pub prefer_aes_gcm: Option<bool>,

    // ── Embedded SSH ────────────────────────────────────────────────
    /// `--ssh-cipher` - comma-separated cipher preference list for embedded SSH.
    pub ssh_cipher: Vec<String>,

    /// `--ssh-connect-timeout` - connection timeout in seconds for embedded SSH.
    pub ssh_connect_timeout: Option<u64>,

    /// `--ssh-keepalive` - keepalive interval in seconds for embedded SSH (0 = disable).
    pub ssh_keepalive: Option<u64>,

    /// `--ssh-identity` - identity file paths for embedded SSH (repeatable).
    pub ssh_identity: Vec<PathBuf>,

    /// `--ssh-no-agent` - disable SSH agent authentication.
    pub ssh_no_agent: bool,

    /// `--ssh-strict-host-key-checking` - host key verification policy (`yes`, `no`, `ask`).
    pub ssh_strict_host_key_checking: Option<String>,

    /// `--ssh-ipv6` - prefer IPv6 for embedded SSH connections.
    pub ssh_ipv6: bool,

    /// `--ssh-port` - port override for embedded SSH connections.
    pub ssh_port: Option<u16>,
}
