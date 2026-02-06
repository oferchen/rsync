use std::ffi::OsString;
use std::io::Write;
use std::path::PathBuf;

use crate::client::{AddressMode, DeleteMode, HumanReadableMode, IconvSetting, TransferTimeout};
use protocol::ProtocolVersion;

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
    /// Enables `--8-bit-output` for raw multibyte output.
    pub eight_bit_output: bool,
    /// Enables archive mode (`-a`).
    pub archive: bool,
    /// Controls recursive traversal (`--recursive`/`--no-recursive`).
    pub recursive: Option<bool>,
    /// Controls incremental recursion (`--inc-recursive`/`--no-inc-recursive`).
    pub inc_recursive: Option<bool>,
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
    /// Optional `--checksum`/`--no-checksum` toggle.
    pub checksum: Option<bool>,
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
    /// Optional `--executability`/`--no-executability` toggle.
    pub executability: Option<bool>,
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
    /// Optional `--links`/`--no-links` toggle.
    pub links: Option<bool>,
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
    /// Optional `--fuzzy`/`--no-fuzzy` toggle.
    pub fuzzy: Option<bool>,
    /// Optional `--devices`/`--no-devices` toggle.
    pub devices: Option<bool>,
    /// Enables `--copy-devices` when `true`.
    pub copy_devices: bool,
    /// Enables `--write-devices` when `true`.
    pub write_devices: bool,
    /// Optional `--specials`/`--no-specials` toggle.
    pub specials: Option<bool>,
    /// Optional `--relative`/`--no-relative` toggle.
    pub relative: Option<bool>,
    /// Filesystem boundary traversal level (0=off, 1=single -x, 2=double -xx).
    pub one_file_system: Option<u8>,
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
    /// Routes daemon messages via `--msgs2stderr` / `--no-msgs2stderr` when specified.
    pub msgs_to_stderr: Option<bool>,
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
    #[cfg(all(unix, feature = "acl"))]
    pub acls: Option<bool>,
    /// Controls xattr forwarding (`--xattrs`/`--no-xattrs`).
    #[cfg(all(unix, feature = "xattr"))]
    pub xattrs: Option<bool>,
}

/// Writer references and arguments required to invoke the fallback binary.
#[allow(dead_code)] // Reserved for future fallback implementation
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
    pub const fn new(stdout: &'a mut Out, stderr: &'a mut Err, args: RemoteFallbackArgs) -> Self {
        Self {
            stdout,
            stderr,
            args,
        }
    }

    /// Splits the context into its component parts.
    #[allow(dead_code)] // Reserved for future fallback implementation
    pub(crate) fn split(self) -> (&'a mut Out, &'a mut Err, RemoteFallbackArgs) {
        let Self {
            stdout,
            stderr,
            args,
        } = self;
        (stdout, stderr, args)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    fn default_args() -> RemoteFallbackArgs {
        RemoteFallbackArgs {
            dry_run: false,
            list_only: false,
            remote_shell: None,
            remote_options: Vec::new(),
            connect_program: None,
            port: None,
            bind_address: None,
            sockopts: None,
            blocking_io: None,
            protect_args: None,
            human_readable: None,
            eight_bit_output: false,
            archive: false,
            recursive: None,
            inc_recursive: None,
            dirs: None,
            delete: false,
            delete_mode: DeleteMode::Disabled,
            delete_excluded: false,
            max_delete: None,
            min_size: None,
            max_size: None,
            block_size: None,
            checksum: None,
            checksum_choice: None,
            checksum_seed: None,
            size_only: false,
            ignore_times: false,
            ignore_existing: false,
            existing: false,
            ignore_missing_args: false,
            delete_missing_args: false,
            update: false,
            modify_window: None,
            compress: false,
            compress_disabled: false,
            compress_level: None,
            compress_choice: None,
            skip_compress: None,
            open_noatime: None,
            iconv: IconvSetting::Unspecified,
            stop_after: None,
            stop_at: None,
            chown: None,
            owner: None,
            group: None,
            usermap: None,
            groupmap: None,
            chmod: Vec::new(),
            executability: None,
            perms: None,
            super_mode: None,
            times: None,
            omit_dir_times: None,
            omit_link_times: None,
            numeric_ids: None,
            hard_links: None,
            links: None,
            copy_links: None,
            copy_dirlinks: false,
            copy_unsafe_links: None,
            keep_dirlinks: None,
            safe_links: false,
            sparse: None,
            fuzzy: None,
            devices: None,
            copy_devices: false,
            write_devices: false,
            specials: None,
            relative: None,
            one_file_system: None,
            implied_dirs: None,
            mkpath: false,
            prune_empty_dirs: None,
            verbosity: 0,
            progress: false,
            stats: false,
            itemize_changes: false,
            partial: false,
            preallocate: false,
            fsync: None,
            delay_updates: false,
            partial_dir: None,
            temp_directory: None,
            backup: false,
            backup_dir: None,
            backup_suffix: None,
            link_dests: Vec::new(),
            remove_source_files: false,
            append: None,
            append_verify: false,
            inplace: None,
            msgs_to_stderr: None,
            outbuf: None,
            whole_file: None,
            bwlimit: None,
            excludes: Vec::new(),
            includes: Vec::new(),
            exclude_from: Vec::new(),
            include_from: Vec::new(),
            filters: Vec::new(),
            rsync_filter_shortcuts: 0,
            compare_destinations: Vec::new(),
            copy_destinations: Vec::new(),
            link_destinations: Vec::new(),
            cvs_exclude: false,
            info_flags: Vec::new(),
            debug_flags: Vec::new(),
            files_from_used: false,
            file_list_entries: Vec::new(),
            from0: false,
            password_file: None,
            daemon_password: None,
            protocol: None,
            timeout: TransferTimeout::Default,
            connect_timeout: TransferTimeout::Default,
            out_format: None,
            log_file: None,
            log_file_format: None,
            no_motd: false,
            address_mode: AddressMode::Default,
            fallback_binary: None,
            rsync_path: None,
            remainder: Vec::new(),
            write_batch: None,
            only_write_batch: None,
            read_batch: None,
            #[cfg(all(unix, feature = "acl"))]
            acls: None,
            #[cfg(all(unix, feature = "xattr"))]
            xattrs: None,
        }
    }

    mod remote_fallback_args_tests {
        use super::*;

        #[test]
        fn clone() {
            let mut args = default_args();
            args.dry_run = true;
            args.verbosity = 3;
            args.archive = true;

            let cloned = args;
            assert!(cloned.dry_run);
            assert_eq!(cloned.verbosity, 3);
            assert!(cloned.archive);
        }

        #[test]
        fn with_remote_shell() {
            let mut args = default_args();
            args.remote_shell = Some(OsString::from("ssh -i ~/.ssh/id_rsa"));

            let cloned = args;
            assert_eq!(
                cloned.remote_shell,
                Some(OsString::from("ssh -i ~/.ssh/id_rsa"))
            );
        }

        #[test]
        fn with_remote_options() {
            let mut args = default_args();
            args.remote_options = vec![
                OsString::from("--bwlimit=1000"),
                OsString::from("--compress"),
            ];

            let cloned = args;
            assert_eq!(cloned.remote_options.len(), 2);
        }

        #[test]
        fn with_filters() {
            let mut args = default_args();
            args.excludes = vec![OsString::from("*.tmp")];
            args.includes = vec![OsString::from("*.rs")];
            args.filters = vec![OsString::from("- .git/")];

            let cloned = args;
            assert_eq!(cloned.excludes.len(), 1);
            assert_eq!(cloned.includes.len(), 1);
            assert_eq!(cloned.filters.len(), 1);
        }

        #[test]
        fn with_port() {
            let mut args = default_args();
            args.port = Some(8873);

            assert_eq!(args.port, Some(8873));
        }

        #[test]
        fn with_delete_mode() {
            let mut args = default_args();
            args.delete = true;
            args.delete_mode = DeleteMode::After;

            assert!(args.delete);
            assert_eq!(args.delete_mode, DeleteMode::After);
        }

        #[test]
        fn with_checksum_settings() {
            let mut args = default_args();
            args.checksum = Some(true);
            args.checksum_choice = Some(OsString::from("xxh3"));
            args.checksum_seed = Some(12345);

            assert_eq!(args.checksum, Some(true));
            assert_eq!(args.checksum_choice, Some(OsString::from("xxh3")));
            assert_eq!(args.checksum_seed, Some(12345));
        }

        #[test]
        fn with_compression_settings() {
            let mut args = default_args();
            args.compress = true;
            args.compress_level = Some(OsString::from("9"));
            args.compress_choice = Some(OsString::from("zstd"));

            assert!(args.compress);
            assert_eq!(args.compress_level, Some(OsString::from("9")));
            assert_eq!(args.compress_choice, Some(OsString::from("zstd")));
        }

        #[test]
        fn with_backup_settings() {
            let mut args = default_args();
            args.backup = true;
            args.backup_dir = Some(PathBuf::from("/backup"));
            args.backup_suffix = Some(OsString::from(".bak"));

            assert!(args.backup);
            assert_eq!(args.backup_dir, Some(PathBuf::from("/backup")));
            assert_eq!(args.backup_suffix, Some(OsString::from(".bak")));
        }

        #[test]
        fn with_partial_settings() {
            let mut args = default_args();
            args.partial = true;
            args.partial_dir = Some(PathBuf::from(".rsync-partial"));

            assert!(args.partial);
            assert_eq!(args.partial_dir, Some(PathBuf::from(".rsync-partial")));
        }

        #[test]
        fn with_batch_settings() {
            let mut args = default_args();
            args.write_batch = Some(OsString::from("batch-out"));
            args.read_batch = Some(OsString::from("batch-in"));

            assert_eq!(args.write_batch, Some(OsString::from("batch-out")));
            assert_eq!(args.read_batch, Some(OsString::from("batch-in")));
        }
    }

    mod remote_fallback_context_tests {
        use super::*;

        #[test]
        fn new_creates_context() {
            let mut stdout = Vec::new();
            let mut stderr = Vec::new();
            let args = default_args();

            let context = RemoteFallbackContext::new(&mut stdout, &mut stderr, args);
            assert!(context.stdout.is_empty());
            assert!(context.stderr.is_empty());
        }

        #[test]
        fn split_returns_components() {
            let mut stdout = Vec::new();
            let mut stderr = Vec::new();
            let mut args = default_args();
            args.dry_run = true;

            let context = RemoteFallbackContext::new(&mut stdout, &mut stderr, args);
            let (out, err, returned_args) = context.split();

            assert!(out.is_empty());
            assert!(err.is_empty());
            assert!(returned_args.dry_run);
        }

        #[test]
        fn context_can_write_to_stdout() {
            let mut stdout = Vec::new();
            let mut stderr = Vec::new();
            let args = default_args();

            let context = RemoteFallbackContext::new(&mut stdout, &mut stderr, args);
            let (out, _, _) = context.split();
            out.write_all(b"hello").unwrap();

            assert_eq!(stdout, b"hello");
        }

        #[test]
        fn context_can_write_to_stderr() {
            let mut stdout = Vec::new();
            let mut stderr = Vec::new();
            let args = default_args();

            let context = RemoteFallbackContext::new(&mut stdout, &mut stderr, args);
            let (_, err, _) = context.split();
            err.write_all(b"error message").unwrap();

            assert_eq!(stderr, b"error message");
        }
    }
}
