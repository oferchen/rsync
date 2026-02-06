use std::ffi::OsString;
use std::path::PathBuf;

use core::client::{AddressMode, DeleteMode, HumanReadableMode, StrongChecksumChoice};

use super::bandwidth::BandwidthArgument;
use super::program_name::ProgramName;
use crate::frontend::progress::{NameOutputLevel, ProgressSetting};

/// Parsed command-line arguments for the rsync frontend.
///
/// This structure holds all recognized command-line options after parsing,
/// representing the complete set of flags and values extracted from `argv`.
/// Each field corresponds to one or more rsync command-line options.
///
/// # Field Semantics
///
/// Fields use the following conventions to represent different option states:
///
/// - **`bool`**: Option is either enabled (`true`) or disabled (`false`).
///   Default is `false` unless the option is part of an implicit group like `--archive`.
///
/// - **`Option<bool>`**: Tri-state flag supporting explicit enable/disable.
///   - `None`: Option was not specified (use default behavior).
///   - `Some(true)`: Explicitly enabled (e.g., `--perms`).
///   - `Some(false)`: Explicitly disabled (e.g., `--no-perms`).
///
/// - **`Option<T>`**: Optional value that may or may not be provided.
///   - `None`: Option was not specified.
///   - `Some(value)`: Option was specified with the given value.
///
/// - **`Vec<T>`**: Accumulating options that can be specified multiple times.
///   Default is an empty vector.
///
/// # Visibility
///
/// All fields are `pub` to allow integration tests to inspect arguments.
///
/// **Warning**: This type is exposed via `cli::test_utils` for integration
/// tests only. It is not part of the stable public API.
#[allow(clippy::struct_excessive_bools)]
#[allow(private_interfaces)] // ProgramName, BandwidthArgument are pub(crate) but exposed for tests
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParsedArgs {
    /// Program name detected from `argv[0]`.
    ///
    /// Determines branding behavior: `rsync` uses upstream naming conventions,
    /// while `oc-rsync` uses the OpenrsyncCompat branding.
    ///
    /// Default: Detected from the executable name at runtime.
    pub program_name: ProgramName,

    /// Display help message and exit.
    ///
    /// Corresponds to: `--help`, `-h`
    ///
    /// Default: `false`
    pub show_help: bool,

    /// Display version information and exit.
    ///
    /// Corresponds to: `--version`, `-V`
    ///
    /// Default: `false`
    pub show_version: bool,

    /// Human-readable output formatting level.
    ///
    /// Controls how byte sizes are displayed in output:
    /// - `Disabled`: Show exact decimal values.
    /// - `Enabled`: Show suffixed values (e.g., `1.23K`, `4.56M`).
    /// - `Combined`: Show both human-readable and exact values.
    ///
    /// Corresponds to: `--human-readable`, `-h` (can be repeated for higher levels)
    ///
    /// Default: `None` (not specified; behavior depends on context).
    pub human_readable: Option<HumanReadableMode>,

    /// Perform a trial run without making changes.
    ///
    /// When enabled, rsync shows what would be transferred without actually
    /// copying any files or modifying the destination.
    ///
    /// Corresponds to: `--dry-run`, `-n`
    ///
    /// Default: `false`
    pub dry_run: bool,

    /// List files instead of copying them.
    ///
    /// When enabled, rsync lists the files that would be transferred
    /// without performing any actual transfers.
    ///
    /// Corresponds to: `--list-only`
    ///
    /// Default: `false`
    pub list_only: bool,

    /// Remote shell command to use for connections.
    ///
    /// Specifies the program and arguments used to establish remote connections.
    /// Common values include `ssh` or `ssh -p 2222`.
    ///
    /// Corresponds to: `--rsh`, `-e`
    ///
    /// Default: `None` (use the default remote shell, typically `ssh`).
    pub remote_shell: Option<OsString>,

    /// Program used to directly connect to an rsync daemon.
    ///
    /// Overrides the normal socket connection method for daemon connections.
    ///
    /// Corresponds to: `--connect-program`
    ///
    /// Default: `None`
    pub connect_program: Option<OsString>,

    /// Additional options to pass to the remote rsync process.
    ///
    /// These options are transmitted to the remote side after the standard
    /// option negotiation.
    ///
    /// Corresponds to: `--remote-option`, `-M`
    ///
    /// Default: Empty vector.
    pub remote_options: Vec<OsString>,

    /// Path to the rsync executable on the remote system.
    ///
    /// Allows specifying a non-standard rsync location on the remote host.
    ///
    /// Corresponds to: `--rsync-path`
    ///
    /// Default: `None` (use `rsync` in the remote `$PATH`).
    pub rsync_path: Option<OsString>,

    /// Protect arguments from shell expansion on the remote side.
    ///
    /// When enabled, arguments are sent to the remote rsync in a way that
    /// prevents the remote shell from interpreting special characters.
    ///
    /// Corresponds to: `--protect-args`, `-s` / `--no-protect-args`
    ///
    /// Default: `None` (enabled by default in modern rsync versions).
    pub protect_args: Option<bool>,

    /// Use old-style argument passing (pre-rsync 3.0).
    ///
    /// Disables the modern argument protection mechanism for compatibility
    /// with older rsync versions.
    ///
    /// Corresponds to: `--old-args` / `--no-old-args`
    ///
    /// Default: `None` (use modern argument passing).
    pub old_args: Option<bool>,

    /// IP address family preference for network connections.
    ///
    /// Controls whether to prefer IPv4 or IPv6 when connecting to remote hosts.
    ///
    /// Corresponds to: `--ipv4`, `-4` / `--ipv6`, `-6`
    ///
    /// Default: `AddressMode::Default` (let the OS choose).
    pub address_mode: AddressMode,

    /// Local address to bind outgoing connections to.
    ///
    /// Useful when the local machine has multiple network interfaces.
    ///
    /// Corresponds to: `--address`
    ///
    /// Default: `None` (let the OS choose).
    pub bind_address: Option<OsString>,

    /// Socket options to set on network connections.
    ///
    /// Accepts a comma-separated list of socket option settings.
    ///
    /// Corresponds to: `--sockopts`
    ///
    /// Default: `None`
    pub sockopts: Option<OsString>,

    /// Use blocking I/O for the remote shell.
    ///
    /// Some remote shells require blocking I/O for correct operation.
    ///
    /// Corresponds to: `--blocking-io` / `--no-blocking-io`
    ///
    /// Default: `None` (rsync chooses based on the remote shell type).
    pub blocking_io: Option<bool>,

    /// Archive mode: preserve permissions, times, owner, group, and more.
    ///
    /// This is a shorthand that enables: `-rlptgoD` (recursive, links,
    /// permissions, times, group, owner, devices/specials).
    ///
    /// Corresponds to: `--archive`, `-a`
    ///
    /// Default: `false`
    pub archive: bool,

    /// Recurse into directories.
    ///
    /// When enabled, rsync copies directories and their contents recursively.
    /// This is implied by `--archive`.
    ///
    /// Corresponds to: `--recursive`, `-r`
    ///
    /// Default: `false`
    pub recursive: bool,

    /// Explicit override for recursive mode.
    ///
    /// Used to explicitly enable or disable recursion, overriding implied settings.
    ///
    /// Corresponds to: `--recursive` / `--no-recursive`
    ///
    /// Default: `None` (use the value implied by other options).
    pub recursive_override: Option<bool>,

    /// Use incremental recursion (scan-ahead during transfer).
    ///
    /// Incremental recursion allows rsync to start transferring files
    /// before the entire file list is built, reducing memory usage.
    ///
    /// Corresponds to: `--inc-recursive` / `--no-inc-recursive`
    ///
    /// Default: `None` (enabled by default when recursing).
    pub inc_recursive: Option<bool>,

    /// Transfer directories without recursing into them.
    ///
    /// Copies directory entries but not their contents.
    ///
    /// Corresponds to: `--dirs`, `-d` / `--no-dirs`
    ///
    /// Default: `None`
    pub dirs: Option<bool>,

    /// Deletion scheduling mode for extraneous files.
    ///
    /// Controls when files that exist on the destination but not on the
    /// source are deleted:
    /// - `Disabled`: No deletion (default).
    /// - `Before`: Delete before transfer.
    /// - `During`: Delete while transferring (rsync default when `--delete` is used).
    /// - `Delay`: Collect deletions during transfer, apply after.
    /// - `After`: Delete after transfer completes.
    ///
    /// Corresponds to: `--delete`, `--delete-before`, `--delete-during`,
    /// `--delete-delay`, `--delete-after`
    ///
    /// Default: `DeleteMode::Disabled`
    pub delete_mode: DeleteMode,

    /// Also delete excluded files from the destination.
    ///
    /// Normally, excluded files are left untouched on the destination.
    /// This option removes them as well.
    ///
    /// Corresponds to: `--delete-excluded`
    ///
    /// Default: `false`
    pub delete_excluded: bool,

    /// Delete missing source arguments from the destination.
    ///
    /// If a source argument is missing, delete the corresponding
    /// destination entry.
    ///
    /// Corresponds to: `--delete-missing-args`
    ///
    /// Default: `false`
    pub delete_missing_args: bool,

    /// Continue deleting even if there are I/O errors.
    ///
    /// Normally, rsync skips deletion when errors occur to prevent data loss.
    ///
    /// Corresponds to: `--ignore-errors` / `--no-ignore-errors`
    ///
    /// Default: `None` (stop on errors).
    pub ignore_errors: Option<bool>,

    /// Make backups of files before overwriting.
    ///
    /// Creates backup copies with a suffix before modifying existing files.
    ///
    /// Corresponds to: `--backup`, `-b`
    ///
    /// Default: `false`
    pub backup: bool,

    /// Directory to store backup files.
    ///
    /// Specifies a location for backup files instead of placing them
    /// alongside the originals.
    ///
    /// Corresponds to: `--backup-dir`
    ///
    /// Default: `None` (backups placed alongside originals).
    pub backup_dir: Option<OsString>,

    /// Suffix to append to backup file names.
    ///
    /// Corresponds to: `--suffix`
    ///
    /// Default: `None` (uses `~` if not specified).
    pub backup_suffix: Option<OsString>,

    /// Skip files based on checksum rather than mod-time and size.
    ///
    /// Forces rsync to compare file contents using checksums instead of
    /// the default quick check (mod-time + size).
    ///
    /// Corresponds to: `--checksum`, `-c` / `--no-checksum`
    ///
    /// Default: `None` (use quick check).
    pub checksum: Option<bool>,

    /// Checksum algorithm selection for transfers.
    ///
    /// Specifies which checksum algorithm to use for file comparisons
    /// and transfer verification.
    ///
    /// Corresponds to: `--checksum-choice`
    ///
    /// Default: `None` (auto-negotiate).
    pub checksum_choice: Option<StrongChecksumChoice>,

    /// Raw checksum-choice argument as provided by the user.
    ///
    /// Preserved for forwarding to remote rsync processes.
    ///
    /// Corresponds to: `--checksum-choice`
    ///
    /// Default: `None`
    pub checksum_choice_arg: Option<OsString>,

    /// Seed value for checksum computations.
    ///
    /// Allows reproducible checksum results for testing purposes.
    ///
    /// Corresponds to: `--checksum-seed`
    ///
    /// Default: `None` (use random seed).
    pub checksum_seed: Option<u32>,

    /// Skip files based on file size only.
    ///
    /// Ignores modification times; files are only compared by size.
    /// Mutually exclusive with `--checksum`.
    ///
    /// Corresponds to: `--size-only`
    ///
    /// Default: `false`
    pub size_only: bool,

    /// Ignore modification times; always transfer files.
    ///
    /// Forces rsync to transfer all files regardless of their timestamps.
    ///
    /// Corresponds to: `--ignore-times`, `-I`
    ///
    /// Default: `false`
    pub ignore_times: bool,

    /// Skip files that already exist on the destination.
    ///
    /// Never update files that exist on the receiver.
    ///
    /// Corresponds to: `--ignore-existing`
    ///
    /// Default: `false`
    pub ignore_existing: bool,

    /// Only transfer files that already exist on the destination.
    ///
    /// Skip creating new files; only update existing ones.
    ///
    /// Corresponds to: `--existing`
    ///
    /// Default: `false`
    pub existing: bool,

    /// Ignore source arguments that do not exist.
    ///
    /// Instead of erroring, skip missing source files.
    ///
    /// Corresponds to: `--ignore-missing-args`
    ///
    /// Default: `false`
    pub ignore_missing_args: bool,

    /// Skip files that are newer on the destination.
    ///
    /// Only transfer files if the source is newer than the destination.
    ///
    /// Corresponds to: `--update`, `-u`
    ///
    /// Default: `false`
    pub update: bool,

    /// Remaining non-option arguments (sources and destination).
    ///
    /// After all options are parsed, the remaining arguments represent
    /// the source path(s) and destination path.
    ///
    /// Default: Empty vector.
    pub remainder: Vec<OsString>,

    /// Bandwidth limit for data transfer.
    ///
    /// Limits the transfer rate to the specified value (in KiB/s by default).
    /// Supports suffixes: K, M, G.
    ///
    /// Corresponds to: `--bwlimit`
    ///
    /// Default: `None` (unlimited).
    pub bwlimit: Option<BandwidthArgument>,

    /// Maximum number of files to delete.
    ///
    /// Limits how many extraneous files will be deleted in a single run.
    ///
    /// Corresponds to: `--max-delete`
    ///
    /// Default: `None` (no limit).
    pub max_delete: Option<OsString>,

    /// Minimum file size to transfer.
    ///
    /// Skip files smaller than the specified size. Supports suffixes: K, M, G.
    ///
    /// Corresponds to: `--min-size`
    ///
    /// Default: `None` (no minimum).
    pub min_size: Option<OsString>,

    /// Maximum file size to transfer.
    ///
    /// Skip files larger than the specified size. Supports suffixes: K, M, G.
    ///
    /// Corresponds to: `--max-size`
    ///
    /// Default: `None` (no maximum).
    pub max_size: Option<OsString>,

    /// Block size for the rsync algorithm.
    ///
    /// Controls the size of blocks used for delta-transfer calculations.
    /// Larger blocks reduce overhead but may miss small changes.
    ///
    /// Corresponds to: `--block-size`, `-B`
    ///
    /// Default: `None` (rsync chooses automatically based on file size).
    pub block_size: Option<OsString>,

    /// Timestamp comparison tolerance in seconds.
    ///
    /// Files with timestamps differing by less than this value are
    /// considered to have the same modification time. Useful for
    /// FAT filesystems where timestamps have 2-second resolution.
    ///
    /// Corresponds to: `--modify-window`
    ///
    /// Default: `None` (use 0, exact match required).
    ///
    /// Constraint: Non-negative integer.
    pub modify_window: Option<OsString>,

    /// Enable compression during transfer.
    ///
    /// Compresses file data during the transfer to reduce bandwidth usage.
    ///
    /// Corresponds to: `--compress`, `-z`
    ///
    /// Default: `false`
    pub compress: bool,

    /// Explicitly disable compression.
    ///
    /// Corresponds to: `--no-compress`
    ///
    /// Default: `false`
    pub no_compress: bool,

    /// Compression level (0-9).
    ///
    /// Controls the compression intensity. Level 0 means no compression;
    /// level 9 provides maximum compression at the cost of CPU time.
    ///
    /// Corresponds to: `--compress-level`
    ///
    /// Default: `None` (use default level, typically 6).
    ///
    /// Constraint: Integer between 0 and 9.
    pub compress_level: Option<OsString>,

    /// Compression algorithm selection.
    ///
    /// Specifies which compression algorithm to use (e.g., `zlib`, `lz4`, `zstd`).
    ///
    /// Corresponds to: `--compress-choice`
    ///
    /// Default: `None` (negotiate with remote).
    pub compress_choice: Option<OsString>,

    /// Use old compression algorithm (zlib).
    ///
    /// Forces the use of the older zlib-based compression for compatibility.
    ///
    /// Corresponds to: `--old-compress`
    ///
    /// Default: `false`
    pub old_compress: bool,

    /// Use new compression algorithm.
    ///
    /// Forces the use of newer compression methods when available.
    ///
    /// Corresponds to: `--new-compress`
    ///
    /// Default: `false`
    pub new_compress: bool,

    /// File suffixes to skip compression for.
    ///
    /// Files with these suffixes are transferred without compression
    /// (e.g., `.gz`, `.jpg`, `.mp3`).
    ///
    /// Corresponds to: `--skip-compress`
    ///
    /// Default: `None` (use built-in list of already-compressed formats).
    pub skip_compress: Option<OsString>,

    /// Open files with `O_NOATIME` to avoid updating access times.
    ///
    /// Prevents reading files from updating their access time, which can
    /// be useful for backup operations.
    ///
    /// Corresponds to: `--open-noatime`
    ///
    /// Default: `false`
    pub open_noatime: bool,

    /// Explicitly disable `O_NOATIME`.
    ///
    /// Corresponds to: `--no-open-noatime`
    ///
    /// Default: `false`
    pub no_open_noatime: bool,

    /// Character set conversion specification.
    ///
    /// Specifies how to convert file names between character encodings.
    /// Format: `LOCAL,REMOTE` or a single encoding name.
    ///
    /// Corresponds to: `--iconv`
    ///
    /// Default: `None` (no conversion).
    pub iconv: Option<OsString>,

    /// Preserve file owner.
    ///
    /// Attempts to set the owner of destination files to match the source.
    /// Requires appropriate privileges.
    ///
    /// Corresponds to: `--owner`, `-o` / `--no-owner`
    ///
    /// Default: `None` (enabled by `--archive` or when running as root).
    pub owner: Option<bool>,

    /// Preserve file group.
    ///
    /// Attempts to set the group of destination files to match the source.
    ///
    /// Corresponds to: `--group`, `-g` / `--no-group`
    ///
    /// Default: `None` (enabled by `--archive`).
    pub group: Option<bool>,

    /// Set owner and/or group on destination files.
    ///
    /// Format: `USER:GROUP`, `USER:`, `:GROUP`, or `USER`.
    ///
    /// Corresponds to: `--chown`
    ///
    /// Default: `None`
    pub chown: Option<OsString>,

    /// Run the remote copy as a different user.
    ///
    /// Format: `USER` or `USER:GROUP`.
    ///
    /// Corresponds to: `--copy-as`
    ///
    /// Default: `None`
    pub copy_as: Option<OsString>,

    /// Map user names between source and destination.
    ///
    /// Format: `SRCUSER:DESTUSER,...`
    ///
    /// Corresponds to: `--usermap`
    ///
    /// Default: `None`
    pub usermap: Option<OsString>,

    /// Map group names between source and destination.
    ///
    /// Format: `SRCGROUP:DESTGROUP,...`
    ///
    /// Corresponds to: `--groupmap`
    ///
    /// Default: `None`
    pub groupmap: Option<OsString>,

    /// Permission modifications to apply to destination files.
    ///
    /// Uses symbolic or octal permission specifications (e.g., `u+w`, `go-rwx`).
    /// Can be specified multiple times.
    ///
    /// Corresponds to: `--chmod`
    ///
    /// Default: Empty vector.
    pub chmod: Vec<OsString>,

    /// Preserve file permissions.
    ///
    /// Copies the source file permissions to the destination.
    ///
    /// Corresponds to: `--perms`, `-p` / `--no-perms`
    ///
    /// Default: `None` (enabled by `--archive`).
    pub perms: Option<bool>,

    /// Attempt to perform operations as root.
    ///
    /// Allows rsync to store information that normally requires root privileges.
    ///
    /// Corresponds to: `--super` / `--no-super`
    ///
    /// Default: `None`
    pub super_mode: Option<bool>,

    /// Store/restore privileged attributes using extended attributes.
    ///
    /// Allows non-root users to preserve root-only file attributes by
    /// storing them as extended attributes.
    ///
    /// Corresponds to: `--fake-super` / `--no-fake-super`
    ///
    /// Default: `None`
    pub fake_super: Option<bool>,

    /// Preserve modification times.
    ///
    /// Copies the source modification times to the destination.
    ///
    /// Corresponds to: `--times`, `-t` / `--no-times`
    ///
    /// Default: `None` (enabled by `--archive`).
    pub times: Option<bool>,

    /// Omit modification times from directories.
    ///
    /// Preserves times for files but not for directories.
    ///
    /// Corresponds to: `--omit-dir-times`, `-O` / `--no-omit-dir-times`
    ///
    /// Default: `None`
    pub omit_dir_times: Option<bool>,

    /// Omit modification times from symbolic links.
    ///
    /// Corresponds to: `--omit-link-times`, `-J` / `--no-omit-link-times`
    ///
    /// Default: `None`
    pub omit_link_times: Option<bool>,

    /// Preserve access times.
    ///
    /// Corresponds to: `--atimes`, `-U` / `--no-atimes`
    ///
    /// Default: `None`
    pub atimes: Option<bool>,

    /// Preserve creation times (macOS/Windows).
    ///
    /// Corresponds to: `--crtimes`, `-N` / `--no-crtimes`
    ///
    /// Default: `None`
    pub crtimes: Option<bool>,

    /// Preserve Access Control Lists.
    ///
    /// Corresponds to: `--acls`, `-A` / `--no-acls`
    ///
    /// Default: `None`
    pub acls: Option<bool>,

    /// Use numeric user and group IDs instead of names.
    ///
    /// Prevents rsync from mapping user/group names between systems.
    ///
    /// Corresponds to: `--numeric-ids` / `--no-numeric-ids`
    ///
    /// Default: `None`
    pub numeric_ids: Option<bool>,

    /// Preserve hard links between files.
    ///
    /// Detects and recreates hard links on the destination.
    ///
    /// Corresponds to: `--hard-links`, `-H` / `--no-hard-links`
    ///
    /// Default: `None`
    pub hard_links: Option<bool>,

    /// Copy symbolic links as symbolic links.
    ///
    /// Preserves symlinks instead of copying their targets.
    ///
    /// Corresponds to: `--links`, `-l` / `--no-links`
    ///
    /// Default: `None` (enabled by `--archive`).
    pub links: Option<bool>,

    /// Handle sparse files efficiently.
    ///
    /// Attempts to create sparse files on the destination when appropriate.
    ///
    /// Corresponds to: `--sparse`, `-S` / `--no-sparse`
    ///
    /// Default: `None`
    pub sparse: Option<bool>,

    /// Find similar files for basis (delta-transfer optimization).
    ///
    /// When a file is not found on the destination, look for similar files
    /// to use as a basis for delta transfer.
    ///
    /// Corresponds to: `--fuzzy`, `-y` / `--no-fuzzy`
    ///
    /// Default: `None`
    pub fuzzy: Option<bool>,

    /// Follow and copy the referent of symbolic links.
    ///
    /// Transforms symlinks into regular files containing the target content.
    ///
    /// Corresponds to: `--copy-links`, `-L` / `--no-copy-links`
    ///
    /// Default: `None`
    pub copy_links: Option<bool>,

    /// Follow and copy directory symlinks.
    ///
    /// Transforms symlinks pointing to directories into real directories.
    ///
    /// Corresponds to: `--copy-dirlinks`, `-k`
    ///
    /// Default: `false`
    pub copy_dirlinks: bool,

    /// Copy the referent of unsafe symlinks.
    ///
    /// Unsafe symlinks are those that point outside the transfer.
    ///
    /// Corresponds to: `--copy-unsafe-links` / `--no-copy-unsafe-links`
    ///
    /// Default: `None`
    pub copy_unsafe_links: Option<bool>,

    /// Treat destination symlinks to directories as directories.
    ///
    /// Allows rsync to transfer into symlinked directories on the receiver.
    ///
    /// Corresponds to: `--keep-dirlinks`, `-K` / `--no-keep-dirlinks`
    ///
    /// Default: `None`
    pub keep_dirlinks: Option<bool>,

    /// Ignore symlinks that point outside the source tree.
    ///
    /// Corresponds to: `--safe-links`
    ///
    /// Default: `false`
    pub safe_links: bool,

    /// Munge symlinks to make them safe.
    ///
    /// Transforms symlink targets to prevent them from escaping the transfer.
    ///
    /// Corresponds to: `--munge-links` / `--no-munge-links`
    ///
    /// Default: `None`
    pub munge_links: Option<bool>,

    /// Trust the sending side's file list.
    ///
    /// Reduces safety checks when the sender is trusted.
    ///
    /// Corresponds to: `--trust-sender`
    ///
    /// Default: `false`
    pub trust_sender: bool,

    /// Run in server mode (internal use).
    ///
    /// This flag is set automatically when rsync is invoked on the remote
    /// side of a transfer. Not intended for direct user invocation.
    ///
    /// Corresponds to: `--server`
    ///
    /// Default: `false`
    pub server_mode: bool,

    /// Act as the sender in server mode (internal use).
    ///
    /// Combined with `--server`, indicates this is the sending side.
    ///
    /// Corresponds to: `--sender`
    ///
    /// Default: `false`
    pub sender_mode: bool,

    /// Detach from the controlling terminal (daemon mode).
    ///
    /// Corresponds to: `--detach` / `--no-detach`
    ///
    /// Default: `None`
    pub detach: Option<bool>,

    /// Run as an rsync daemon.
    ///
    /// Starts rsync as a persistent daemon that accepts connections.
    ///
    /// Corresponds to: `--daemon`
    ///
    /// Default: `false`
    pub daemon_mode: bool,

    /// Path to the daemon configuration file.
    ///
    /// Corresponds to: `--config`
    ///
    /// Default: `None` (uses `/etc/rsyncd.conf`).
    pub config: Option<OsString>,

    /// Allow writing to device files.
    ///
    /// Corresponds to: `--write-devices` / `--no-write-devices`
    ///
    /// Default: `None`
    pub write_devices: Option<bool>,

    /// Preserve device files.
    ///
    /// Recreates device files (block and character special files) on the destination.
    ///
    /// Corresponds to: `--devices` / `--no-devices`
    ///
    /// Default: `None` (enabled by `--archive` when running as root).
    pub devices: Option<bool>,

    /// Copy device file contents rather than creating device files.
    ///
    /// Reads the device and writes its content to a regular file.
    ///
    /// Corresponds to: `--copy-devices`
    ///
    /// Default: `false`
    pub copy_devices: bool,

    /// Preserve special files (sockets, FIFOs).
    ///
    /// Corresponds to: `--specials` / `--no-specials`
    ///
    /// Default: `None` (enabled by `--archive` when running as root).
    pub specials: Option<bool>,

    /// Force deletion of non-empty directories.
    ///
    /// Allows rsync to delete directories that are not empty.
    ///
    /// Corresponds to: `--force` / `--no-force`
    ///
    /// Default: `None`
    pub force: Option<bool>,

    /// Use qsort instead of merge-sort for file list sorting.
    ///
    /// Internal option affecting file list ordering algorithm.
    ///
    /// Corresponds to: `--qsort`
    ///
    /// Default: `false`
    pub qsort: bool,

    /// Use relative paths in the transfer.
    ///
    /// Preserves leading path components when creating files on the destination.
    ///
    /// Corresponds to: `--relative`, `-R` / `--no-relative`
    ///
    /// Default: `None`
    pub relative: Option<bool>,

    /// Do not cross filesystem boundaries.
    ///
    /// When recursing, skip directories that are on different filesystems.
    /// Can be specified twice (`-xx`) to also skip root-level mount points.
    ///
    /// - `None`: not specified (default).
    /// - `Some(0)`: explicitly disabled via `--no-one-file-system`.
    /// - `Some(1)`: single `-x` -- skip cross-filesystem directories during recursion.
    /// - `Some(2)`: double `-xx` -- also skip root-level source mount points.
    ///
    /// Corresponds to: `--one-file-system`, `-x` / `--no-one-file-system`
    ///
    /// Default: `None`
    pub one_file_system: Option<u8>,

    /// Create implied directories in relative mode.
    ///
    /// When using `--relative`, create parent directories as needed.
    ///
    /// Corresponds to: `--implied-dirs` / `--no-implied-dirs`
    ///
    /// Default: `None` (enabled by default with `--relative`).
    pub implied_dirs: Option<bool>,

    /// Create missing path components of the destination.
    ///
    /// Corresponds to: `--mkpath`
    ///
    /// Default: `false`
    pub mkpath: bool,

    /// Remove empty directories from the file list.
    ///
    /// Prunes directories that would be empty after applying filter rules.
    ///
    /// Corresponds to: `--prune-empty-dirs`, `-m` / `--no-prune-empty-dirs`
    ///
    /// Default: `None`
    pub prune_empty_dirs: Option<bool>,

    /// Output verbosity level.
    ///
    /// Higher values produce more detailed output. Each `-v` flag increments
    /// the level by one.
    ///
    /// Corresponds to: `--verbose`, `-v` (can be repeated) / `--quiet`, `-q`
    ///
    /// Default: `0` (normal output).
    ///
    /// Constraint: Typically 0-4; higher values are accepted but have no additional effect.
    pub verbosity: u8,

    /// Progress reporting mode.
    ///
    /// Controls how transfer progress is displayed:
    /// - `Unspecified`: No explicit setting (behavior depends on context).
    /// - `Disabled`: Progress explicitly disabled.
    /// - `PerFile`: Show progress for each file.
    /// - `Overall`: Show overall transfer progress.
    ///
    /// Corresponds to: `--progress`, `--info=progress2` / `--no-progress`
    ///
    /// Default: `ProgressSetting::Unspecified`
    pub progress: ProgressSetting,

    /// File name output level during transfer.
    ///
    /// Controls which file names are printed:
    /// - `Disabled`: No file names.
    /// - `UpdatedOnly`: Only transferred files.
    /// - `UpdatedAndUnchanged`: All files including skipped ones.
    ///
    /// Default: `NameOutputLevel::UpdatedOnly` (when verbose).
    pub name_level: NameOutputLevel,

    /// Whether name output level was explicitly overridden.
    ///
    /// Tracks if the user explicitly set the name output level vs. using defaults.
    ///
    /// Default: `false`
    pub name_overridden: bool,

    /// Print file-transfer statistics at the end.
    ///
    /// Shows a summary of bytes transferred, speedup ratio, etc.
    ///
    /// Corresponds to: `--stats`
    ///
    /// Default: `false`
    pub stats: bool,

    /// Display high-bit characters in filenames as-is.
    ///
    /// Prevents escaping of non-ASCII characters in output.
    ///
    /// Corresponds to: `--8-bit-output`, `-8`
    ///
    /// Default: `false`
    pub eight_bit_output: bool,

    /// Keep partially transferred files.
    ///
    /// When a transfer is interrupted, keep the partial file for resumption.
    ///
    /// Corresponds to: `--partial`
    ///
    /// Default: `false`
    pub partial: bool,

    /// Pre-allocate disk space for destination files.
    ///
    /// Uses `fallocate()` or similar to reserve space before writing.
    ///
    /// Corresponds to: `--preallocate`
    ///
    /// Default: `false`
    pub preallocate: bool,

    /// Sync files to disk after writing.
    ///
    /// Calls `fsync()` to ensure data is written to stable storage.
    ///
    /// Corresponds to: `--fsync` / `--no-fsync`
    ///
    /// Default: `None`
    pub fsync: Option<bool>,

    /// Delay updates until the end of the transfer.
    ///
    /// Uses temporary files and renames them after all transfers complete.
    /// Provides atomic updates at the cost of additional disk space.
    ///
    /// Corresponds to: `--delay-updates`
    ///
    /// Default: `false`
    pub delay_updates: bool,

    /// Directory to store partial files.
    ///
    /// Specifies where to store partial files during transfer.
    /// Implies `--partial`.
    ///
    /// Corresponds to: `--partial-dir`
    ///
    /// Default: `None` (partial files stored in destination directory).
    pub partial_dir: Option<PathBuf>,

    /// Directory for temporary files during transfer.
    ///
    /// Specifies where to create temporary files before moving to destination.
    ///
    /// Corresponds to: `--temp-dir`, `-T`
    ///
    /// Default: `None` (temporary files in destination directory).
    pub temp_dir: Option<PathBuf>,

    /// Path to write a log file.
    ///
    /// Corresponds to: `--log-file`
    ///
    /// Default: `None`
    pub log_file: Option<OsString>,

    /// Format string for log file entries.
    ///
    /// Uses the same format codes as `--out-format`.
    ///
    /// Corresponds to: `--log-file-format`
    ///
    /// Default: `None` (uses default log format).
    pub log_file_format: Option<OsString>,

    /// Write a batch file for later replay.
    ///
    /// Creates a file that can be used to repeat the transfer without
    /// re-reading the source.
    ///
    /// Corresponds to: `--write-batch`
    ///
    /// Default: `None`
    pub write_batch: Option<OsString>,

    /// Write a batch file without performing the transfer.
    ///
    /// Like `--write-batch` but does not actually transfer files.
    ///
    /// Corresponds to: `--only-write-batch`
    ///
    /// Default: `None`
    pub only_write_batch: Option<OsString>,

    /// Read and apply a batch file.
    ///
    /// Replays a previously created batch file.
    ///
    /// Corresponds to: `--read-batch`
    ///
    /// Default: `None`
    pub read_batch: Option<OsString>,

    /// Input file for early protocol stages.
    ///
    /// Provides input to the remote rsync before the transfer begins.
    ///
    /// Corresponds to: `--early-input`
    ///
    /// Default: `None`
    pub early_input: Option<OsString>,

    /// Directories containing link destination files.
    ///
    /// Used internally for `--link-dest` processing.
    ///
    /// Default: Empty vector.
    pub link_dests: Vec<PathBuf>,

    /// Remove source files after successful transfer.
    ///
    /// Deletes files from the source after they are successfully transferred.
    /// Use with caution.
    ///
    /// Corresponds to: `--remove-source-files`
    ///
    /// Default: `false`
    pub remove_source_files: bool,

    /// Update destination files in-place.
    ///
    /// Writes directly to destination files instead of using temporaries.
    /// More efficient but less safe if the transfer is interrupted.
    ///
    /// Corresponds to: `--inplace` / `--no-inplace`
    ///
    /// Default: `None`
    pub inplace: Option<bool>,

    /// Append data to the end of shorter files.
    ///
    /// Assumes existing data in destination files is correct and only
    /// appends new data.
    ///
    /// Corresponds to: `--append` / `--no-append`
    ///
    /// Default: `None`
    pub append: Option<bool>,

    /// Append data and verify existing content.
    ///
    /// Like `--append` but verifies existing data with checksums.
    ///
    /// Corresponds to: `--append-verify`
    ///
    /// Default: `false`
    pub append_verify: bool,

    /// Send informational messages to stderr.
    ///
    /// Corresponds to: `--msgs2stderr` / `--no-msgs2stderr`
    ///
    /// Default: `None`
    pub msgs_to_stderr: Option<bool>,

    /// Stderr output mode selection.
    ///
    /// Controls how stderr output is handled: `errors`, `all`, or `client`.
    ///
    /// Corresponds to: `--stderr`
    ///
    /// Default: `None`
    pub stderr_mode: Option<OsString>,

    /// Output buffering mode.
    ///
    /// Controls output buffering: `none`, `line`, or `block`.
    ///
    /// Corresponds to: `--outbuf`
    ///
    /// Default: `None`
    pub outbuf: Option<OsString>,

    /// Maximum memory allocation limit.
    ///
    /// Limits how much memory rsync will allocate for file lists and buffers.
    /// Supports suffixes: K, M, G.
    ///
    /// Corresponds to: `--max-alloc`
    ///
    /// Default: `None` (use default limit).
    pub max_alloc: Option<OsString>,

    /// Output a change summary for each file.
    ///
    /// Displays itemized changes showing what was updated for each file.
    ///
    /// Corresponds to: `--itemize-changes`, `-i`
    ///
    /// Default: `false`
    pub itemize_changes: bool,

    /// Disable delta-transfer algorithm.
    ///
    /// Forces rsync to transfer whole files instead of using the
    /// delta-transfer algorithm. Useful for local transfers.
    ///
    /// Corresponds to: `--whole-file`, `-W` / `--no-whole-file`
    ///
    /// Default: `None` (rsync auto-detects based on transfer type).
    pub whole_file: Option<bool>,

    /// Patterns for excluding files from transfer.
    ///
    /// Files matching these patterns are not transferred.
    ///
    /// Corresponds to: `--exclude`
    ///
    /// Default: Empty vector.
    pub excludes: Vec<OsString>,

    /// Patterns for including files in transfer.
    ///
    /// Files matching these patterns are included even if they match
    /// an exclude pattern.
    ///
    /// Corresponds to: `--include`
    ///
    /// Default: Empty vector.
    pub includes: Vec<OsString>,

    /// Directories to compare against when deciding what to transfer.
    ///
    /// Files are skipped if they match a file in any compare-dest directory.
    ///
    /// Corresponds to: `--compare-dest`
    ///
    /// Default: Empty vector.
    pub compare_destinations: Vec<OsString>,

    /// Directories to copy from if file exists there.
    ///
    /// If a file exists in a copy-dest directory, copy it locally instead
    /// of transferring.
    ///
    /// Corresponds to: `--copy-dest`
    ///
    /// Default: Empty vector.
    pub copy_destinations: Vec<OsString>,

    /// Directories to hard-link from if file exists there.
    ///
    /// If a file exists in a link-dest directory and matches, create a
    /// hard link instead of copying.
    ///
    /// Corresponds to: `--link-dest`
    ///
    /// Default: Empty vector.
    pub link_destinations: Vec<OsString>,

    /// Files containing exclude patterns.
    ///
    /// Each file contains one pattern per line.
    ///
    /// Corresponds to: `--exclude-from`
    ///
    /// Default: Empty vector.
    pub exclude_from: Vec<OsString>,

    /// Files containing include patterns.
    ///
    /// Each file contains one pattern per line.
    ///
    /// Corresponds to: `--include-from`
    ///
    /// Default: Empty vector.
    pub include_from: Vec<OsString>,

    /// Filter rules for including/excluding files.
    ///
    /// General filter rules that can include, exclude, or modify file handling.
    ///
    /// Corresponds to: `--filter`, `-f`
    ///
    /// Default: Empty vector.
    pub filters: Vec<OsString>,

    /// Use CVS-style ignore patterns.
    ///
    /// Automatically excludes files that would be ignored by CVS.
    ///
    /// Corresponds to: `--cvs-exclude`, `-C`
    ///
    /// Default: `false`
    pub cvs_exclude: bool,

    /// Count of rsync filter shortcut options used.
    ///
    /// Tracks how many times `-F` was specified (each adds more filter rules).
    ///
    /// Corresponds to: `-F` (can be repeated)
    ///
    /// Default: `0`
    pub rsync_filter_shortcuts: usize,

    /// Files containing lists of source files to transfer.
    ///
    /// Each file contains paths to transfer, one per line.
    ///
    /// Corresponds to: `--files-from`
    ///
    /// Default: Empty vector.
    pub files_from: Vec<OsString>,

    /// Use NUL character as line separator in files-from.
    ///
    /// When reading files-from or other input files, use NUL instead of newline.
    ///
    /// Corresponds to: `--from0`, `-0`
    ///
    /// Default: `false`
    pub from0: bool,

    /// Fine-grained control over informational output.
    ///
    /// Allows enabling/disabling specific info messages by name.
    ///
    /// Corresponds to: `--info`
    ///
    /// Default: Empty vector.
    pub info: Vec<OsString>,

    /// Fine-grained control over debug output.
    ///
    /// Allows enabling/disabling specific debug messages by name.
    ///
    /// Corresponds to: `--debug`
    ///
    /// Default: Empty vector.
    pub debug: Vec<OsString>,

    /// Preserve extended attributes.
    ///
    /// Copies extended attributes (xattrs) from source to destination.
    ///
    /// Corresponds to: `--xattrs`, `-X` / `--no-xattrs`
    ///
    /// Default: `None`
    pub xattrs: Option<bool>,

    /// Suppress the daemon's message of the day.
    ///
    /// When connecting to a daemon, skip printing its MOTD.
    ///
    /// Corresponds to: `--no-motd`
    ///
    /// Default: `false`
    pub no_motd: bool,

    /// File containing the password for daemon authentication.
    ///
    /// Corresponds to: `--password-file`
    ///
    /// Default: `None`
    pub password_file: Option<OsString>,

    /// Force a specific protocol version.
    ///
    /// Override the normal protocol version negotiation.
    ///
    /// Corresponds to: `--protocol`
    ///
    /// Default: `None` (negotiate with remote).
    pub protocol: Option<OsString>,

    /// I/O timeout in seconds.
    ///
    /// Maximum time to wait for data during the transfer.
    ///
    /// Corresponds to: `--timeout`
    ///
    /// Default: `None` (no timeout).
    pub timeout: Option<OsString>,

    /// Connection timeout in seconds.
    ///
    /// Maximum time to wait when establishing the initial connection.
    ///
    /// Corresponds to: `--contimeout`
    ///
    /// Default: `None` (no timeout).
    pub contimeout: Option<OsString>,

    /// Stop transfer after the specified duration.
    ///
    /// Maximum time to run before stopping the transfer.
    ///
    /// Corresponds to: `--stop-after`
    ///
    /// Default: `None`
    pub stop_after: Option<OsString>,

    /// Stop transfer at the specified time.
    ///
    /// Wall-clock time at which to stop the transfer.
    ///
    /// Corresponds to: `--stop-at`
    ///
    /// Default: `None`
    pub stop_at: Option<OsString>,

    /// Output format string for file transfer messages.
    ///
    /// Controls how transferred files are displayed using format codes
    /// (e.g., `%n` for filename, `%l` for length).
    ///
    /// Corresponds to: `--out-format`
    ///
    /// Default: `None` (use default format).
    pub out_format: Option<OsString>,

    /// Port number for daemon connections.
    ///
    /// Specifies the TCP port to use when connecting to an rsync daemon.
    ///
    /// Corresponds to: `--port`
    ///
    /// Default: `None` (uses port 873).
    ///
    /// Constraint: Valid port number (1-65535).
    pub daemon_port: Option<u16>,

    /// Daemon configuration parameters.
    ///
    /// Override daemon configuration settings on the command line.
    ///
    /// Corresponds to: `--dparam`, `-M`
    ///
    /// Default: Empty vector.
    pub dparam: Vec<OsString>,

    /// Disable iconv character set conversion.
    ///
    /// Corresponds to: `--no-iconv`
    ///
    /// Default: `false`
    pub no_iconv: bool,

    /// Preserve file executability.
    ///
    /// Ensures executable permission is preserved when not preserving permissions.
    ///
    /// Corresponds to: `--executability`, `-E` / `--no-executability`
    ///
    /// Default: `None`
    pub executability: Option<bool>,
}
