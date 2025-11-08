use std::ffi::OsString;
use std::io::Write;
use std::path::PathBuf;

use crate::client::{AddressMode, DeleteMode, HumanReadableMode, IconvSetting, TransferTimeout};
use oc_rsync_protocol::ProtocolVersion;

/// Arguments used to spawn the legacy `rsync` binary when remote operands are present.
///
/// The fallback path preserves the command-line semantics of upstream rsync while the
/// native protocol engine is completed. Higher level consumers such as the CLI build
/// this structure from parsed flags before handing control to
/// [`crate::client::fallback::run_remote_transfer_fallback`].
#[derive(Clone)]
pub struct RemoteFallbackArgs {
    /// Enables `--dry-run`.
    pub dry_run: bool,
    /// Enables `--list-only`.
    pub list_only: bool,
    /// Supplies the remote shell command forwarded via `-e`/`--rsh`.
    pub remote_shell: Option<OsString>,
    /// Additional options forwarded to the remote rsync invocation via `--remote-option`/`-M`.
    pub remote_options: Vec<OsString>,
    /// Optional command executed to reach rsync:// daemons.
    pub connect_program: Option<OsString>,
    /// Default daemon port forwarded via `--port` when contacting rsync:// daemons.
    #[doc(alias = "--port")]
    pub port: Option<u16>,
    /// Optional bind address forwarded via `--address`.
    pub bind_address: Option<OsString>,
    /// Optional socket options forwarded via `--sockopts`.
    pub sockopts: Option<OsString>,
    /// Optional `--blocking-io`/`--no-blocking-io` toggle.
    pub blocking_io: Option<bool>,
    /// Controls whether remote shell arguments are protected from expansion.
    ///
    /// When `Some(true)` the fallback command receives `--protect-args`,
    /// while `Some(false)` forwards `--no-protect-args`. A `None` value keeps
    /// rsync's default behaviour.
    pub protect_args: Option<bool>,
    /// Optional `--human-readable` level forwarded to the fallback binary.
    pub human_readable: Option<HumanReadableMode>,
    /// Enables archive mode (`-a`).
    pub archive: bool,
    /// Controls recursive traversal (`--recursive`/`--no-recursive`).
    pub recursive: Option<bool>,
    /// Controls directory handling when recursion is disabled (`--dirs`/`--no-dirs`).
    pub dirs: Option<bool>,
    /// Enables `--delete`.
    pub delete: bool,
    /// Selects the deletion timing to forward to the fallback binary.
    pub delete_mode: DeleteMode,
    /// Enables `--delete-excluded`.
    pub delete_excluded: bool,
    /// Limits deletions via `--max-delete`.
    pub max_delete: Option<u64>,
    /// Skips files smaller than the provided size via `--min-size`.
    pub min_size: Option<OsString>,
    /// Skips files larger than the provided size via `--max-size`.
    pub max_size: Option<OsString>,
    /// Overrides the delta-transfer block size via `--block-size`.
    #[doc(alias = "--block-size")]
    pub block_size: Option<OsString>,
    /// Enables `--checksum`.
    pub checksum: bool,
    /// Optional strong checksum selection forwarded via `--checksum-choice`.
    pub checksum_choice: Option<OsString>,
    /// Optional checksum seed forwarded via `--checksum-seed`.
    pub checksum_seed: Option<u32>,
    /// Enables `--size-only`.
    pub size_only: bool,
    /// Enables `--ignore-times`.
    pub ignore_times: bool,
    /// Enables `--ignore-existing`.
    pub ignore_existing: bool,
    /// Enables `--existing`.
    pub existing: bool,
    /// Enables `--ignore-missing-args`.
    pub ignore_missing_args: bool,
    /// Enables `--delete-missing-args`.
    pub delete_missing_args: bool,
    /// Enables `--update`.
    pub update: bool,
    /// Optional `--modify-window` tolerance forwarded to the fallback binary.
    pub modify_window: Option<u64>,
    /// Enables `--compress`.
    pub compress: bool,
    /// Enables `--no-compress` when `true` and compression is otherwise disabled.
    pub compress_disabled: bool,
    /// Optional compression level forwarded via `--compress-level`.
    pub compress_level: Option<OsString>,
    /// Optional compression algorithm forwarded via `--compress-choice`.
    pub compress_choice: Option<OsString>,
    /// Optional suffix list forwarded via `--skip-compress`.
    pub skip_compress: Option<OsString>,
    /// Optional `--open-noatime`/`--no-open-noatime` toggle.
    pub open_noatime: Option<bool>,
    /// Iconv charset conversion forwarded via `--iconv`/`--no-iconv`.
    pub iconv: IconvSetting,
    /// Optional `--stop-after` argument forwarded to the fallback binary.
    pub stop_after: Option<OsString>,
    /// Optional `--stop-at` argument forwarded to the fallback binary.
    pub stop_at: Option<OsString>,
    /// Optional ownership override forwarded via `--chown`.
    pub chown: Option<OsString>,
    /// Optional `--owner`/`--no-owner` toggle.
    pub owner: Option<bool>,
    /// Optional `--group`/`--no-group` toggle.
    pub group: Option<bool>,
    /// Optional `--usermap` mapping forwarded to the fallback binary.
    pub usermap: Option<OsString>,
    /// Optional `--groupmap` mapping forwarded to the fallback binary.
    pub groupmap: Option<OsString>,
    /// Repeated `--chmod` specifications forwarded to the fallback binary.
    pub chmod: Vec<OsString>,
    /// Optional `--perms`/`--no-perms` toggle.
    pub perms: Option<bool>,
    /// Optional `--super`/`--no-super` toggle.
    pub super_mode: Option<bool>,
    /// Optional `--times`/`--no-times` toggle.
    pub times: Option<bool>,
    /// Optional `--omit-dir-times`/`--no-omit-dir-times` toggle.
    pub omit_dir_times: Option<bool>,
    /// Optional `--omit-link-times`/`--no-omit-link-times` toggle.
    pub omit_link_times: Option<bool>,
    /// Optional `--numeric-ids`/`--no-numeric-ids` toggle.
    pub numeric_ids: Option<bool>,
    /// Optional `--hard-links`/`--no-hard-links` toggle.
    pub hard_links: Option<bool>,
    /// Optional `--copy-links`/`--no-copy-links` toggle.
    pub copy_links: Option<bool>,
    /// Enables `--copy-dirlinks` when `true`.
    pub copy_dirlinks: bool,
    /// Optional `--copy-unsafe-links`/`--no-copy-unsafe-links` toggle.
    pub copy_unsafe_links: Option<bool>,
    /// Optional `--keep-dirlinks`/`--no-keep-dirlinks` toggle.
    pub keep_dirlinks: Option<bool>,
    /// Enables `--safe-links` when `true`.
    pub safe_links: bool,
    /// Optional `--sparse`/`--no-sparse` toggle.
    pub sparse: Option<bool>,
    /// Optional `--devices`/`--no-devices` toggle.
    pub devices: Option<bool>,
    /// Enables `--copy-devices` when `true`.
    pub copy_devices: bool,
    /// Optional `--specials`/`--no-specials` toggle.
    pub specials: Option<bool>,
    /// Optional `--relative`/`--no-relative` toggle.
    pub relative: Option<bool>,
    /// Optional `--one-file-system`/`--no-one-file-system` toggle.
    pub one_file_system: Option<bool>,
    /// Optional `--implied-dirs`/`--no-implied-dirs` toggle.
    pub implied_dirs: Option<bool>,
    /// Enables `--mkpath`.
    pub mkpath: bool,
    /// Controls pruning of empty directories via `--prune-empty-dirs`.
    pub prune_empty_dirs: Option<bool>,
    /// Verbosity level translated into repeated `-v` flags.
    pub verbosity: u8,
    /// Enables `--progress`.
    pub progress: bool,
    /// Enables `--stats`.
    pub stats: bool,
    /// Enables `--itemize-changes` on the fallback command line.
    pub itemize_changes: bool,
    /// Enables `--partial`.
    pub partial: bool,
    /// Enables `--preallocate`.
    pub preallocate: bool,
    /// Controls `--fsync`/`--no-fsync` forwarding.
    pub fsync: Option<bool>,
    /// Enables `--delay-updates`.
    pub delay_updates: bool,
    /// Optional directory forwarded via `--partial-dir`.
    pub partial_dir: Option<PathBuf>,
    /// Optional directory forwarded via `--temp-dir`.
    pub temp_directory: Option<PathBuf>,
    /// Enables `--backup`.
    pub backup: bool,
    /// Optional directory forwarded via `--backup-dir`.
    pub backup_dir: Option<PathBuf>,
    /// Optional suffix forwarded via `--suffix`.
    pub backup_suffix: Option<OsString>,
    /// Directories forwarded via repeated `--link-dest` flags.
    pub link_dests: Vec<PathBuf>,
    /// Enables `--remove-source-files`.
    pub remove_source_files: bool,
    /// Optional `--append`/`--no-append` toggle.
    pub append: Option<bool>,
    /// Enables `--append-verify`.
    pub append_verify: bool,
    /// Optional `--inplace`/`--no-inplace` toggle.
    pub inplace: Option<bool>,
    /// Routes daemon messages to standard error via `--msgs2stderr`.
    pub msgs_to_stderr: bool,
    /// Optional `--outbuf` value forwarded to the fallback binary.
    pub outbuf: Option<OsString>,
    /// Optional `--whole-file`/`--no-whole-file` toggle.
    pub whole_file: Option<bool>,
    /// Optional bandwidth limit forwarded through `--bwlimit`.
    ///
    /// Values are normalised to their rounded byte-per-second representation so
    /// legacy fallback binaries that predate burst support continue accepting
    /// the argument. An unlimited transfer is represented using `"0"`.
    pub bwlimit: Option<OsString>,
    /// Patterns forwarded via repeated `--exclude` flags.
    pub excludes: Vec<OsString>,
    /// Patterns forwarded via repeated `--include` flags.
    pub includes: Vec<OsString>,
    /// File paths forwarded via repeated `--exclude-from` flags.
    pub exclude_from: Vec<OsString>,
    /// File paths forwarded via repeated `--include-from` flags.
    pub include_from: Vec<OsString>,
    /// Raw filter directives forwarded via repeated `--filter` flags.
    pub filters: Vec<OsString>,
    /// Number of times the `-F` shortcut was supplied.
    pub rsync_filter_shortcuts: usize,
    /// Reference directories forwarded via repeated `--compare-dest` flags.
    pub compare_destinations: Vec<OsString>,
    /// Reference directories forwarded via repeated `--copy-dest` flags.
    pub copy_destinations: Vec<OsString>,
    /// Reference directories forwarded via repeated `--link-dest` flags.
    pub link_destinations: Vec<OsString>,
    /// Enables `--cvs-exclude` on the fallback binary.
    pub cvs_exclude: bool,
    /// Values forwarded to the fallback binary via repeated `--info=FLAGS` occurrences.
    pub info_flags: Vec<OsString>,
    /// Values forwarded to the fallback binary via repeated `--debug=FLAGS` occurrences.
    pub debug_flags: Vec<OsString>,
    /// Whether the original invocation used `--files-from`.
    pub files_from_used: bool,
    /// Entries collected from `--files-from` operands.
    pub file_list_entries: Vec<OsString>,
    /// Indicates that `--from0` was supplied.
    pub from0: bool,
    /// Optional path provided via `--password-file`.
    pub password_file: Option<PathBuf>,
    /// Optional daemon password supplied via `--password-file=-`.
    ///
    /// When populated the helper writes the password to the fallback
    /// process' standard input so callers do not need to re-enter
    /// credentials after the CLI has already consumed them.
    pub daemon_password: Option<Vec<u8>>,
    /// Optional protocol override forwarded via `--protocol`.
    pub protocol: Option<ProtocolVersion>,
    /// Timeout applied to the spawned process via `--timeout`.
    pub timeout: TransferTimeout,
    /// Connection timeout forwarded via `--contimeout`.
    pub connect_timeout: TransferTimeout,
    /// Optional `--out-format` template.
    pub out_format: Option<OsString>,
    /// Optional log file path forwarded via `--log-file`.
    pub log_file: Option<PathBuf>,
    /// Optional log template forwarded via `--log-file-format`.
    pub log_file_format: Option<OsString>,
    /// Enables `--no-motd`.
    pub no_motd: bool,
    /// Preferred address family forwarded via `--ipv4`/`--ipv6`.
    pub address_mode: AddressMode,
    /// Optional override for the fallback executable path.
    ///
    /// When unspecified the helper consults the
    /// [`crate::fallback::CLIENT_FALLBACK_ENV`] environment variable
    /// (`OC_RSYNC_FALLBACK`) and
    /// defaults to `rsync` if the override is missing or empty.
    pub fallback_binary: Option<OsString>,
    /// Optional override for the remote rsync executable.
    ///
    /// When populated the helper forwards `--rsync-path` to the fallback command so upstream
    /// rsync executes the specified program on the remote system. The option is ignored when
    /// remote operands are absent because local transfers never invoke the fallback binary.
    pub rsync_path: Option<OsString>,
    /// Remaining operands to forward to the fallback binary.
    pub remainder: Vec<OsString>,
    /// Optional prefix forwarded via `--write-batch`.
    pub write_batch: Option<OsString>,
    /// Optional prefix forwarded via `--only-write-batch`.
    pub only_write_batch: Option<OsString>,
    /// Optional prefix forwarded via `--read-batch`.
    pub read_batch: Option<OsString>,
    /// Controls ACL forwarding (`--acls`/`--no-acls`).
    #[cfg(feature = "acl")]
    pub acls: Option<bool>,
    /// Controls xattr forwarding (`--xattrs`/`--no-xattrs`).
    #[cfg(feature = "xattr")]
    pub xattrs: Option<bool>,
}

/// Writer references and arguments required to invoke the fallback binary.
pub struct RemoteFallbackContext<'a, Out, Err>
where
    Out: Write + 'a,
    Err: Write + 'a,
{
    stdout: &'a mut Out,
    stderr: &'a mut Err,
    args: RemoteFallbackArgs,
}

impl<'a, Out, Err> RemoteFallbackContext<'a, Out, Err>
where
    Out: Write + 'a,
    Err: Write + 'a,
{
    /// Creates a new context that streams output into the supplied writers.
    #[must_use]
    pub fn new(stdout: &'a mut Out, stderr: &'a mut Err, args: RemoteFallbackArgs) -> Self {
        Self {
            stdout,
            stderr,
            args,
        }
    }

    pub(crate) fn split(self) -> (&'a mut Out, &'a mut Err, RemoteFallbackArgs) {
        let Self {
            stdout,
            stderr,
            args,
        } = self;
        (stdout, stderr, args)
    }
}
