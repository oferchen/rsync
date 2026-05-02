//! Remote invocation argument builder.
//!
//! Translates `ClientConfig` options into the compact flag string and long-form
//! arguments expected by `rsync --server`. The argument format mirrors upstream
//! `options.c:server_options()`.
//!
//! # Upstream Reference
//!
//! - `options.c:server_options()` - Server argument generation
//! - `options.c:parse_arguments()` - Server-side argument parsing

use std::ffi::OsString;

use super::super::super::config::{
    ClientConfig, DeleteMode, FilesFromSource, ReferenceDirectoryKind, StrongChecksumAlgorithm,
    TransferTimeout,
};
use super::{RemoteRole, SecludedInvocation};
use transfer::setup::build_capability_string;

/// Builder for constructing remote rsync `--server` invocation arguments.
///
/// This builder translates client configuration options into the compact flag
/// string format expected by `rsync --server`. The resulting argument vector
/// follows upstream rsync's `server_options()` format.
///
/// # Format
///
/// **Pull (local=receiver, remote=sender):**
/// ```text
/// rsync --server --sender -flags . /remote/path
/// ```
///
/// **Push (local=sender, remote=receiver):**
/// ```text
/// rsync --server -flags . /remote/path
/// ```
///
/// The `.` is a dummy argument required by upstream rsync for compatibility.
///
/// # Secluded Args
///
/// When `--protect-args` / `-s` is enabled, the builder produces a minimal
/// command line containing only `rsync --server [-s] [--sender]`,
/// and the full argument list is returned in `SecludedInvocation::stdin_args`
/// for transmission over stdin after SSH connection establishment.
pub struct RemoteInvocationBuilder<'a> {
    config: &'a ClientConfig,
    role: RemoteRole,
}

impl<'a> RemoteInvocationBuilder<'a> {
    /// Creates a new builder for the specified role and client configuration.
    #[must_use]
    pub const fn new(config: &'a ClientConfig, role: RemoteRole) -> Self {
        Self { config, role }
    }

    /// Builds the complete invocation argument vector.
    ///
    /// The first element is the rsync binary name (either from `--rsync-path`
    /// or "rsync" by default), followed by "--server", optional role flags,
    /// the compact flag string, ".", and the remote path(s).
    pub fn build(&self, remote_path: &str) -> Vec<OsString> {
        self.build_with_paths(&[remote_path])
    }

    /// Builds the complete invocation argument vector with multiple remote paths.
    ///
    /// This is used for pull operations with multiple remote sources from the same host.
    pub fn build_with_paths(&self, remote_paths: &[&str]) -> Vec<OsString> {
        let mut args = Vec::new();

        // Use custom rsync path if specified, otherwise default to "rsync"
        if let Some(rsync_path) = self.config.rsync_path() {
            args.push(OsString::from(rsync_path));
        } else {
            args.push(OsString::from("rsync"));
        }

        args.extend(self.build_args_without_program(remote_paths));
        args
    }

    /// Builds an invocation with secluded-args support.
    ///
    /// When secluded args is active, the SSH command line contains only the
    /// minimal server startup arguments (`rsync --server [-s] [--sender]`),
    /// and the full argument list is returned in `stdin_args` for transmission
    /// over stdin after the SSH connection is established.
    ///
    /// When secluded args is not active, this returns the same result as
    /// `build_with_paths` with an empty `stdin_args`.
    ///
    /// # Upstream Reference
    ///
    /// Mirrors `send_protected_args()` in upstream `main.c:1119`.
    pub fn build_secluded(self, remote_paths: &[&str]) -> SecludedInvocation {
        if !self.config.protect_args().unwrap_or(false) {
            return SecludedInvocation {
                command_line_args: self.build_with_paths(remote_paths),
                stdin_args: Vec::new(),
            };
        }

        // Build the full argument list as if secluded args were off -
        // these are what we will send over stdin.
        let full_args = self.build_full_args_for_stdin(remote_paths);

        // Build the minimal command line: rsync --server [-s] [--sender]
        let mut cmd_args = Vec::new();
        if let Some(rsync_path) = self.config.rsync_path() {
            cmd_args.push(OsString::from(rsync_path));
        } else {
            cmd_args.push(OsString::from("rsync"));
        }
        cmd_args.push(OsString::from("--server"));
        if self.role == RemoteRole::Receiver {
            cmd_args.push(OsString::from("--sender"));
        }
        // The `-s` flag tells the remote server to read args from stdin.
        // upstream: options.c - protect_args flag sent as `-s` in server mode
        cmd_args.push(OsString::from("-s"));
        // Dummy argument required by upstream
        cmd_args.push(OsString::from("."));

        SecludedInvocation {
            command_line_args: cmd_args,
            stdin_args: full_args,
        }
    }

    /// Builds the full argument list for stdin transmission in secluded-args mode.
    ///
    /// This produces the same arguments as `build_with_paths()` but as `String`
    /// values suitable for null-separated transmission over stdin.
    fn build_full_args_for_stdin(&self, remote_paths: &[&str]) -> Vec<String> {
        let os_args = self.build_args_without_program(remote_paths);
        os_args
            .into_iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    /// Builds the argument list without the rsync program name.
    ///
    /// This is shared between normal `build_with_paths` and secluded-args
    /// `build_full_args_for_stdin`. The result includes `--server`, optional
    /// `--sender`, flags, capability string, `.`, and remote paths.
    fn build_args_without_program(&self, remote_paths: &[&str]) -> Vec<OsString> {
        let mut args = Vec::new();

        args.push(OsString::from("--server"));

        if self.role == RemoteRole::Receiver {
            args.push(OsString::from("--sender"));
        }

        if self.config.ignore_errors() {
            args.push(OsString::from("--ignore-errors"));
        }

        if self.config.fsync() {
            args.push(OsString::from("--fsync"));
        }

        let flags = self.build_flag_string();
        if !flags.is_empty() {
            args.push(OsString::from(flags));
        }

        // Long-form options that cannot be expressed as single-char flags.
        // Order mirrors upstream options.c server_options().
        self.append_long_form_args(&mut args);

        // SSH transfers advertise the INC_RECURSE (`'i'`) capability in both
        // directions by default, mirroring upstream's `allow_inc_recurse = 1`
        // initialization. `--no-inc-recursive` clears the flag and suppresses
        // the bit, matching `set_allow_inc_recurse()`.
        // upstream: compat.c:720 set_allow_inc_recurse() - capability gate.
        // upstream: options.c:3003-3050 maybe_add_e_option() - capability string.
        args.push(OsString::from(build_capability_string(
            self.config.inc_recursive_send(),
        )));
        args.push(OsString::from("."));

        for path in remote_paths {
            args.push(OsString::from(path));
        }

        args
    }

    /// Appends long-form `--option=value` arguments to the argument vector.
    ///
    /// These are options that upstream rsync's `server_options()` emits as separate
    /// `--key=value` tokens rather than single-character flags. The order mirrors
    /// upstream for predictable interop testing.
    fn append_long_form_args(&self, args: &mut Vec<OsString>) {
        // --delete-* timing variants
        // upstream: options.c - delete_mode forwarded as --delete-before/during/after/delay
        match self.config.delete_mode() {
            DeleteMode::Disabled => {}
            DeleteMode::Before => args.push(OsString::from("--delete-before")),
            DeleteMode::During => args.push(OsString::from("--delete-during")),
            DeleteMode::After => args.push(OsString::from("--delete-after")),
            DeleteMode::Delay => args.push(OsString::from("--delete-delay")),
        }

        if self.config.delete_excluded() {
            args.push(OsString::from("--delete-excluded"));
        }

        if self.config.force_replacements() {
            args.push(OsString::from("--force"));
        }

        // --max-delete=N
        if let Some(max) = self.config.max_delete() {
            args.push(OsString::from(format!("--max-delete={max}")));
        }

        // --max-size / --min-size
        if let Some(max) = self.config.max_file_size() {
            args.push(OsString::from(format!("--max-size={max}")));
        }
        if let Some(min) = self.config.min_file_size() {
            args.push(OsString::from(format!("--min-size={min}")));
        }

        // --modify-window=N
        if let Some(window) = self.config.modify_window() {
            args.push(OsString::from(format!("--modify-window={window}")));
        }

        // --compress-level=N
        // upstream: options.c - compress_level sent to server when explicitly set
        if let Some(level) = self.config.compression_level() {
            let numeric = compression_level_to_numeric(level);
            args.push(OsString::from(format!("--compress-level={numeric}")));
        }

        // upstream: options.c:2800-2805 - compress choice forwarding.
        // Only sent when the user explicitly specified --compress-choice,
        // --new-compress, or --old-compress. The wire format depends on the
        // algorithm: zlibx uses --new-compress, explicit zlib uses
        // --old-compress, and other algorithms use --compress-choice=ALGO.
        if self.config.explicit_compress_choice() {
            let algo = self.config.compression_algorithm();
            let name = algo.name();
            match name {
                // upstream: compat.c:100 - "zlibx" is the new-compress alias
                "zlibx" => args.push(OsString::from("--new-compress")),
                // upstream: options.c:2802 - explicit zlib sent as --old-compress
                "zlib" => args.push(OsString::from("--old-compress")),
                // upstream: options.c:2804-2805 - other algorithms
                _ => args.push(OsString::from(format!("--compress-choice={name}"))),
            }
        }

        // --checksum-choice=ALGO
        // upstream: options.c - checksum_choice forwarded when not auto
        let checksum_choice = self.config.checksum_choice();
        if checksum_choice.transfer() != StrongChecksumAlgorithm::Auto
            || checksum_choice.file() != StrongChecksumAlgorithm::Auto
        {
            args.push(OsString::from(format!(
                "--checksum-choice={}",
                checksum_choice.to_argument()
            )));
        }

        // --block-size=N
        if let Some(bs) = self.config.block_size_override() {
            args.push(OsString::from(format!("--block-size={}", bs.get())));
        }

        // --timeout=N
        if let TransferTimeout::Seconds(secs) = self.config.timeout() {
            args.push(OsString::from(format!("--timeout={}", secs.get())));
        }

        // --bwlimit=N
        // upstream: options.c - bwlimit forwarded as bytes-per-second
        if let Some(bwlimit) = self.config.bandwidth_limit() {
            let mut arg = OsString::from("--bwlimit=");
            arg.push(bwlimit.fallback_argument());
            args.push(arg);
        }

        // --partial-dir=DIR
        if let Some(dir) = self.config.partial_directory() {
            let mut arg = OsString::from("--partial-dir=");
            arg.push(dir.as_os_str());
            args.push(arg);
        }

        // --temp-dir=DIR
        if let Some(dir) = self.config.temp_directory() {
            let mut arg = OsString::from("--temp-dir=");
            arg.push(dir.as_os_str());
            args.push(arg);
        }

        if self.config.inplace() {
            args.push(OsString::from("--inplace"));
        }

        if self.config.append() {
            args.push(OsString::from("--append"));
        } else if self.config.append_verify() {
            args.push(OsString::from("--append-verify"));
        }

        // --copy-unsafe-links, --safe-links, --munge-links
        if self.config.copy_unsafe_links() {
            args.push(OsString::from("--copy-unsafe-links"));
        }
        if self.config.safe_links() {
            args.push(OsString::from("--safe-links"));
        }
        if self.config.munge_links() {
            args.push(OsString::from("--munge-links"));
        }

        // --numeric-ids - upstream: options.c:2887-2888 (long-form only)
        if self.config.numeric_ids() {
            args.push(OsString::from("--numeric-ids"));
        }

        if self.config.size_only() {
            args.push(OsString::from("--size-only"));
        }
        if self.config.ignore_times() {
            args.push(OsString::from("--ignore-times"));
        }
        if self.config.ignore_existing() {
            args.push(OsString::from("--ignore-existing"));
        }
        if self.config.existing_only() {
            args.push(OsString::from("--existing"));
        }

        if self.config.remove_source_files() {
            args.push(OsString::from("--remove-source-files"));
        }

        if !self.config.implied_dirs() {
            args.push(OsString::from("--no-implied-dirs"));
        }

        if self.config.fake_super() {
            args.push(OsString::from("--fake-super"));
        }

        if self.config.omit_dir_times() {
            args.push(OsString::from("--omit-dir-times"));
        }
        if self.config.omit_link_times() {
            args.push(OsString::from("--omit-link-times"));
        }

        if self.config.delay_updates() {
            args.push(OsString::from("--delay-updates"));
        }

        // --backup, --backup-dir=DIR, --suffix=SUFFIX
        if self.config.backup() {
            args.push(OsString::from("--backup"));
            if let Some(dir) = self.config.backup_directory() {
                let mut arg = OsString::from("--backup-dir=");
                arg.push(dir.as_os_str());
                args.push(arg);
            }
            if let Some(suffix) = self.config.backup_suffix() {
                let mut arg = OsString::from("--suffix=");
                arg.push(suffix);
                args.push(arg);
            }
        }

        // --compare-dest, --copy-dest, --link-dest
        for ref_dir in self.config.reference_directories() {
            let flag = match ref_dir.kind() {
                ReferenceDirectoryKind::Compare => "--compare-dest=",
                ReferenceDirectoryKind::Copy => "--copy-dest=",
                ReferenceDirectoryKind::Link => "--link-dest=",
            };
            let mut arg = OsString::from(flag);
            arg.push(ref_dir.path().as_os_str());
            args.push(arg);
        }

        if self.config.copy_devices() {
            args.push(OsString::from("--copy-devices"));
        }
        if self.config.write_devices() {
            args.push(OsString::from("--write-devices"));
        }

        if self.config.open_noatime() {
            args.push(OsString::from("--open-noatime"));
        }

        if self.config.preallocate() {
            args.push(OsString::from("--preallocate"));
        }

        // upstream: options.c:2750-2762 - itemize-changes is forwarded as
        // --log-format=%i, not as a compact flag character. Upstream also
        // supports --log-format=%i%I when stdout_format_has_i > 1, but we
        // use the simpler single-%i form that covers the common case.
        if self.config.itemize_changes() {
            args.push(OsString::from("--log-format=%i"));
        }

        // --files-from forwarding.
        // upstream: options.c:2944-2956 - when the file is local (or stdin),
        // the client reads it and forwards content over the socket, so we tell
        // the server `--files-from=- --from0`. When the file is remote, we
        // send `--files-from=<path>` and optionally `--from0`.
        match self.config.files_from() {
            FilesFromSource::None => {}
            FilesFromSource::LocalFile(_) | FilesFromSource::Stdin => {
                args.push(OsString::from("--files-from=-"));
                args.push(OsString::from("--from0"));
            }
            FilesFromSource::RemoteFile(path) => {
                args.push(OsString::from(format!("--files-from={path}")));
                if self.config.from0() {
                    args.push(OsString::from("--from0"));
                }
            }
        }
    }

    /// Builds the compact flag string from client configuration.
    ///
    /// Format: `-logDtpre.LsfxC` where:
    /// - Transfer flags before `.` separator
    /// - Info/debug flags after `.` separator
    fn build_flag_string(&self) -> String {
        let mut flags = String::from("-");

        // upstream: options.c:2169-2188 — when --files-from is active, upstream
        // sets recurse=0, xfer_dirs=1, relative_paths=1. Suppress 'r' and imply
        // 'R' to match this behaviour.
        let files_from_active = self.config.files_from().is_active();
        let effective_recursive = self.config.recursive() && !files_from_active;
        let effective_relative = self.config.relative_paths() || files_from_active;

        // Transfer flags (order matches upstream server_options())
        if self.config.links() {
            flags.push('l');
        }
        if self.config.copy_links() {
            flags.push('L');
        }
        if self.config.copy_dirlinks() {
            flags.push('k');
        }
        if self.config.keep_dirlinks() {
            flags.push('K');
        }
        if self.config.preserve_owner() {
            flags.push('o');
        }
        if self.config.preserve_group() {
            flags.push('g');
        }
        if self.config.preserve_devices() || self.config.preserve_specials() {
            flags.push('D');
        }
        if self.config.preserve_times() {
            flags.push('t');
        }
        if self.config.preserve_atimes() {
            flags.push('U');
        }
        if self.config.preserve_permissions() {
            flags.push('p');
        } else if self.config.preserve_executability() {
            // upstream: options.c:2672-2675 - 'E' is only sent when
            // preserve_perms is false (else-if). When perms are preserved,
            // executability is implicitly included.
            flags.push('E');
        }
        if effective_recursive {
            flags.push('r');
        }
        if self.config.compress() {
            flags.push('z');
        }
        if self.config.checksum() {
            flags.push('c');
        }
        if self.config.preserve_hard_links() {
            flags.push('H');
        }
        #[cfg(all(any(unix, windows), feature = "acl"))]
        if self.config.preserve_acls() {
            flags.push('A');
        }
        #[cfg(all(unix, feature = "xattr"))]
        if self.config.preserve_xattrs() {
            flags.push('X');
        }
        // upstream: 'n' = dry_run (!do_xfers), NOT numeric_ids.
        // numeric_ids is always sent as long-form --numeric-ids (options.c:2887-2888).
        if self.config.dry_run() {
            flags.push('n');
        }
        // upstream: 'd' = --dirs (xfer_dirs without recursion), NOT delete.
        // delete variants are always sent as long-form --delete-* (options.c:2818-2827).
        // When --files-from is active, upstream sets xfer_dirs=1 and recurse=0,
        // so 'd' is emitted (options.c:2620).
        let effective_dirs = self.config.dirs() || files_from_active;
        if effective_dirs && !effective_recursive {
            flags.push('d');
        }
        // upstream: options.c:2644-2648 - only send 'W' when explicitly set
        // (whole_file > 0). The default for remote transfers is no-whole-file;
        // upstream never sends --no-whole-file because it's the default.
        if self.config.whole_file_raw() == Some(true) {
            flags.push('W');
        }
        if self.config.sparse() {
            flags.push('S');
        }
        // upstream: options.c:2613 - send 'y' for fuzzy, 'yy' for level 2
        for _ in 0..self.config.fuzzy_level() {
            flags.push('y');
        }
        for _ in 0..self.config.one_file_system_level() {
            flags.push('x');
        }
        if effective_relative {
            flags.push('R');
        }
        if self.config.partial() {
            flags.push('P');
        }
        if self.config.update() {
            flags.push('u');
        }
        if self.config.preserve_crtimes() {
            flags.push('N');
        }
        if self.config.prune_empty_dirs() {
            flags.push('m');
        }
        for _ in 0..self.config.verbosity() {
            flags.push('v');
        }

        // Note: itemize-changes is forwarded via --log-format=%i in
        // append_long_form_args(), not as a compact flag - upstream: options.c:2750-2762

        flags
    }
}

/// Converts a `CompressionLevel` into its numeric representation for the wire.
///
/// Upstream rsync sends the compression level as an integer in the range 0-9.
pub(super) fn compression_level_to_numeric(level: compress::zlib::CompressionLevel) -> u32 {
    use compress::zlib::CompressionLevel;
    match level {
        CompressionLevel::None => 0,
        CompressionLevel::Fast => 1,
        CompressionLevel::Default => 6,
        CompressionLevel::Best => 9,
        CompressionLevel::Precise(n) => u32::from(n.get()),
    }
}
