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
use std::time::SystemTime;

use super::super::super::config::{
    ClientConfig, DeleteMode, IconvSetting, ReferenceDirectoryKind, StrongChecksumAlgorithm,
    TransferTimeout,
};
use super::{RemoteRole, SecludedInvocation};
use transfer::setup::build_capability_string_suffix;

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
    /// Filename arguments are shell-escaped for safe eval by the remote shell.
    pub fn build_with_paths(&self, remote_paths: &[&str]) -> Vec<OsString> {
        let mut args = Vec::new();

        if let Some(rsync_path) = self.config.rsync_path() {
            args.push(OsString::from(rsync_path));
        } else {
            args.push(OsString::from("rsync"));
        }

        args.extend(self.build_args_without_program(remote_paths, true));
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

        // Build the full argument list as if secluded args were off; these
        // are what we will send over stdin.
        let full_args = self.build_full_args_for_stdin(remote_paths);

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
        // upstream: options.c - protect_args flag sent as `-s` in server
        // mode tells the remote server to read args from stdin.
        cmd_args.push(OsString::from("-s"));
        // upstream: dummy argument required after the flag string.
        cmd_args.push(OsString::from("."));

        SecludedInvocation {
            command_line_args: cmd_args,
            stdin_args: full_args,
        }
    }

    /// Builds the full argument list for stdin transmission in secluded-args mode.
    ///
    /// This produces the same arguments as `build_with_paths()` but as `String`
    /// values suitable for null-separated transmission over stdin. No shell
    /// escaping is applied because stdin args are null-separated, not eval'd.
    fn build_full_args_for_stdin(&self, remote_paths: &[&str]) -> Vec<String> {
        let os_args = self.build_args_without_program(remote_paths, false);
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
    ///
    /// When `escape_for_shell` is true, filename arguments (remote paths) are
    /// backslash-escaped for safe evaluation by the remote shell. This mirrors
    /// upstream `options.c:safe_arg(NULL, path)` which escapes shell
    /// metacharacters so that `eval "$@"` in the remote shell wrapper (e.g.,
    /// `lsh.sh`) does not misinterpret special characters in filenames.
    fn build_args_without_program(
        &self,
        remote_paths: &[&str],
        escape_for_shell: bool,
    ) -> Vec<OsString> {
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

        if let Some(depth) = self.config.io_uring_depth() {
            args.push(OsString::from(format!("--io-uring-depth={depth}")));
        }

        let mut flags = self.build_flag_string();

        // upstream: options.c:2710 - maybe_add_e_option() appends the `e.xxx`
        // capability string directly onto the compact flag string, producing a
        // single argument like `-logDtpre.iLsfxCIvu`. Emitting it as a separate
        // `-e.xxx` argument would confuse the server-side parser which treats
        // only the first short-flag argument as the compact flag string.
        // upstream: compat.c:162-181 set_allow_inc_recurse(),
        // options.c:3036 maybe_add_e_option() - 'i' is only advertised when
        // the local side honors INC_RECURSE on its receive path. The local
        // Receiver role strips CF_INC_RECURSE from compat_flags after reading
        // (compat.c:723) but receive_extra_file_lists then skips the
        // NDX_FLIST_EOF the remote still emits, leaving its trailing bytes
        // to trip read_varint overflow on the next decode.
        // upstream: io.c:1816 read_varint - rejects encodings with extra > 4.
        let advertise_inc_recurse =
            self.config.inc_recursive_send() && self.role != RemoteRole::Receiver;
        let capability_suffix = build_capability_string_suffix(advertise_inc_recurse);
        flags.push_str(&capability_suffix);

        if !flags.is_empty() {
            args.push(OsString::from(flags));
        }

        // Long-form options that cannot be expressed as single-char flags.
        // Order mirrors upstream options.c server_options().
        self.append_long_form_args(&mut args);

        args.push(OsString::from("."));

        // upstream: options.c:2533 safe_arg() - when old_style_args >= 1,
        // filename arguments (is_filename_arg=true) skip shell escaping so
        // the remote shell's eval naturally splits space-separated paths.
        let old_args_active = self.config.old_args().unwrap_or(false);

        // upstream: options.c:2553-2558 escape_leading_tilde is set only when
        // local is NOT the sender (a pull, so the remote paths are the source)
        // and the sender's args are not trusted. The per-path shape test is
        // applied below.
        let am_sender = self.role == RemoteRole::Sender;
        let escape_tilde_role = !am_sender && !self.config.trust_sender();

        for path in remote_paths {
            if escape_for_shell && !old_args_active {
                // upstream: main.c:613 safe_arg(NULL, *remote_argv++)
                // upstream: options.c:2555-2557 - escape a leading ~ for a
                // relative path without a `/./` pivot, or a path with no `/`.
                let escape_tilde = escape_tilde_role
                    && ((self.config.relative_paths() && !path.contains("/./"))
                        || !path.contains('/'));
                let escaped = if escape_tilde {
                    shell_safe_filename_arg_with_tilde(path, true)
                } else {
                    shell_safe_filename_arg(path)
                };
                args.push(OsString::from(escaped));
            } else {
                args.push(OsString::from(*path));
            }
        }

        args
    }

    /// Appends long-form `--option=value` arguments to the argument vector.
    ///
    /// These are options that upstream rsync's `server_options()` emits as separate
    /// `--key=value` tokens rather than single-character flags. The order mirrors
    /// upstream for predictable interop testing.
    fn append_long_form_args(&self, args: &mut Vec<OsString>) {
        // upstream: options.c:2818-2829 - delete timing variants. Explicit
        // --delete-before/during/after/delay are always sent. Bare --delete
        // (DuringDefault) is suppressed when --delete-excluded is active,
        // matching upstream: `else if (delete_mode && !delete_excluded)`.
        match self.config.delete_mode() {
            DeleteMode::Disabled => {}
            DeleteMode::Before => args.push(OsString::from("--delete-before")),
            DeleteMode::During => args.push(OsString::from("--delete-during")),
            DeleteMode::DuringDefault => {
                if !self.config.delete_excluded() {
                    args.push(OsString::from("--delete"));
                }
            }
            DeleteMode::After => args.push(OsString::from("--delete-after")),
            DeleteMode::Delay => args.push(OsString::from("--delete-delay")),
        }

        if self.config.delete_excluded() {
            args.push(OsString::from("--delete-excluded"));
        }

        if self.config.force_replacements() {
            args.push(OsString::from("--force"));
        }

        if let Some(max) = self.config.max_delete() {
            args.push(OsString::from(format!("--max-delete={max}")));
        }

        if let Some(max) = self.config.max_file_size() {
            args.push(OsString::from(format!("--max-size={max}")));
        }
        if let Some(min) = self.config.min_file_size() {
            args.push(OsString::from(format!("--min-size={min}")));
        }

        // upstream: options.c:2845-2846 - `--max-alloc=arg` is forwarded to
        // the server when the user supplied a non-default value. Each side
        // owns its own cap, so forwarding lets the remote enforce the same
        // budget the client requested.
        if let Some(limit) = self.config.max_alloc() {
            args.push(OsString::from(format!("--max-alloc={limit}")));
        }

        if let Some(window) = self.config.modify_window() {
            args.push(OsString::from(format!("--modify-window={window}")));
        }

        // upstream: options.c - compress_level sent to server when
        // explicitly set.
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

        // upstream: options.c - checksum_choice forwarded as
        // --checksum-choice=ALGO when not auto.
        let checksum_choice = self.config.checksum_choice();
        if checksum_choice.transfer() != StrongChecksumAlgorithm::Auto
            || checksum_choice.file() != StrongChecksumAlgorithm::Auto
        {
            args.push(OsString::from(format!(
                "--checksum-choice={}",
                checksum_choice.to_argument()
            )));
        }

        if let Some(bs) = self.config.block_size_override() {
            args.push(OsString::from(format!("--block-size={}", bs.get())));
        }

        if let TransferTimeout::Seconds(secs) = self.config.timeout() {
            args.push(OsString::from(format!("--timeout={}", secs.get())));
        }

        // upstream: options.c:server_options() - stop_at_utime is forwarded
        // as --stop-at=YYYY/MM/DDTHH:MM so the remote side enforces the
        // same deadline. Both --stop-after (duration) and --stop-at (absolute)
        // are converted to an absolute SystemTime at parse time, so only the
        // absolute form is forwarded.
        if let Some(deadline) = self.config.stop_at() {
            if let Some(formatted) = format_system_time_for_stop_at(deadline) {
                args.push(OsString::from(format!("--stop-at={formatted}")));
            }
        }

        // upstream: options.c - bwlimit forwarded as bytes-per-second.
        if let Some(bwlimit) = self.config.bandwidth_limit() {
            let mut arg = OsString::from("--bwlimit=");
            arg.push(bwlimit.fallback_argument());
            args.push(arg);
        }

        if let Some(dir) = self.config.partial_directory() {
            let mut arg = OsString::from("--partial-dir=");
            arg.push(dir.as_os_str());
            args.push(arg);
        }

        if let Some(dir) = self.config.temp_directory() {
            let mut arg = OsString::from("--temp-dir=");
            arg.push(dir.as_os_str());
            args.push(arg);
        }

        if self.config.inplace() {
            args.push(OsString::from("--inplace"));
        }

        // upstream: options.c:2951-2954 server_options() - append_mode is sent
        // as one or two bare `--append` flags, never `--append-verify`. A
        // second `--append` is what tells the server-side receiver to run in
        // verify mode (append_mode == 2, OPT_APPEND increments it on am_server).
        if self.config.append() {
            args.push(OsString::from("--append"));
            if self.config.append_verify() {
                args.push(OsString::from("--append"));
            }
        }

        if self.config.copy_unsafe_links() {
            args.push(OsString::from("--copy-unsafe-links"));
        }
        if self.config.safe_links() {
            args.push(OsString::from("--safe-links"));
        }
        if self.config.munge_links() {
            args.push(OsString::from("--munge-links"));
        }

        // upstream: options.c:2887-2888 - --numeric-ids is long-form only.
        if self.config.numeric_ids() {
            args.push(OsString::from("--numeric-ids"));
        }

        // upstream: options.c:2889-2890 - --trust-sender forwarded as long-form.
        if self.config.trust_sender() {
            args.push(OsString::from("--trust-sender"));
        }

        // upstream: options.c:2892-2894 - --checksum-seed=N forwarded so the
        // server uses the same seed for rolling and strong checksum generation.
        if let Some(seed) = self.config.checksum_seed() {
            args.push(OsString::from(format!("--checksum-seed={seed}")));
        }

        if self.config.size_only() {
            args.push(OsString::from("--size-only"));
        }
        // upstream: options.c:2711-2712 - --ignore-times is emitted as the
        // compact `I` letter in build_flag_string(), not as a long-form arg.
        if self.config.ignore_existing() {
            args.push(OsString::from("--ignore-existing"));
        }
        if self.config.existing_only() {
            args.push(OsString::from("--existing"));
        }

        // upstream: options.c:817-818 - missing_args forwarded as long-form.
        if self.config.ignore_missing_args() {
            args.push(OsString::from("--ignore-missing-args"));
        }
        if self.config.delete_missing_args() {
            args.push(OsString::from("--delete-missing-args"));
        }

        if self.config.remove_source_files() {
            args.push(OsString::from("--remove-source-files"));
        }

        // upstream: options.c:2976-2977 - `if (relative_paths && !implied_dirs
        // && (!am_sender || protocol_version >= 30)) args[ac++] =
        // "--no-implied-dirs";`. The flag is forwarded to the peer only when
        // relative paths are active; implied dirs exist solely for
        // relative-rooted transfers, so a non-relative transfer never sends it.
        // The `(!am_sender || protocol_version >= 30)` guard is always satisfied
        // for oc's modern protocol (>= 30), so gating on `relative_paths` alone
        // matches upstream. Without the `relative_paths` gate a non-relative
        // transfer with `implied_dirs = 0` (options.c:2207 forces this) would
        // wrongly forward `--no-implied-dirs`, which the remote sender then
        // link_stat()s as a source path (exit 23).
        if self.config.relative_paths() && !self.config.implied_dirs() {
            args.push(OsString::from("--no-implied-dirs"));
        }

        // upstream: options.c:2825,2852-2853 server_options() - the super flag
        // is forwarded only inside the `if (am_sender)` block (so to a remote
        // receiver), never to a remote sender. `RemoteRole::Receiver` means the
        // local process is the sender (PUSH), matching upstream's `am_sender`
        // branch. Forwarding `--fake-super` to a remote sender (PULL) would make
        // the remote stat source paths under fake-super semantics, which
        // upstream never does.
        if self.config.fake_super() && self.role == RemoteRole::Receiver {
            args.push(OsString::from("--fake-super"));
        }

        // upstream: options.c:2646-2649 - --omit-dir-times ('O') and
        // --omit-link-times ('J') are encoded as sender-only compact letters in
        // the transfer-flag string (see the `am_sender` block above), not as
        // standalone long options. Emitting them here would forward
        // `--omit-dir-times` to a remote sender (pull), which then stats it as a
        // source path.

        // upstream: options.c:2996-2997 - `if (mkpath_dest_arg && am_sender)
        // args[ac++] = "--mkpath"`. The dest-arg path creation is a
        // receiver-side concern, so the option is forwarded only when the
        // local process is the sender (a PUSH to a remote receiver).
        // `RemoteRole::Sender` here means the remote peer acts as the
        // receiver (this builder pushes `--sender` for the opposite role),
        // matching upstream's `am_sender` branch (see `am_sender` above).
        if self.config.mkpath() && self.role == RemoteRole::Sender {
            args.push(OsString::from("--mkpath"));
        }

        if self.config.delay_updates() {
            args.push(OsString::from("--delay-updates"));
        }

        // upstream: options.c:2630-2631 - bare `make_backups` is emitted as the
        // `b` character in the compact flag string by `build_flag_string`. Only
        // `--backup-dir` and `--suffix` are forwarded as long-form arguments
        // (`options.c:2807,2813`).
        if self.config.backup() {
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

        // upstream: options.c:164-175 server_options - itemize-changes is
        // forwarded as --log-format, not as a compact flag character. `%i%I` is
        // the `-ii` form (stdout_format_has_i > 1) that itemizes unchanged
        // entries too; `%i` alone is the `-i` form.
        if self.config.itemize_changes() {
            if self.config.itemize_unchanged() {
                args.push(OsString::from("--log-format=%i%I"));
            } else {
                args.push(OsString::from("--log-format=%i"));
            }
        }

        // upstream: options.c:2962-2974 - `if (files_from && (!am_sender ||
        // filesfrom_host))`. The server-side `--files-from` arg is only added
        // when we are NOT the sender (i.e. local is receiver, doing pull) OR
        // the files-from spec was a hostspec (remote filelist).
        //
        // - PUSH with local filelist: local sender reads the list locally and
        //   walks accordingly; the remote receiver gets no `--files-from`.
        // - PUSH with remote filelist: remote receiver opens the file and
        //   forwards its bytes back to the local sender (main.c:1191-1198).
        // - PULL with local filelist: local receiver forwards the file bytes
        //   on the wire; remote sender reads from `--files-from=-`.
        // - PULL with remote filelist: remote sender opens the file via
        //   `--files-from=<path>`.
        //
        // `RemoteRole::Sender` here means the local process is the sender
        // (PUSH); `RemoteRole::Receiver` means the local process is the
        // receiver (PULL).
        // upstream: options.c:2962 server_options() - the files-from arg is
        // forwarded only when the remote peer reads the list (`!am_sender ||
        // filesfrom_host`). The direction-aware resolver collapses the single
        // files-from fd so a localhost:path hostspec is treated as a plain
        // local file in either direction and never double-sourced.
        let local_is_push = self.role == RemoteRole::Sender;
        let plan = self
            .config
            .files_from()
            .resolve_for(local_is_push, self.config.from0());
        if let Some(arg) = plan.remote_arg {
            args.push(OsString::from(format!("--files-from={arg}")));
            if plan.remote_from0 {
                args.push(OsString::from("--from0"));
            }
        }

        // upstream: options.c:2894-2898 - --usermap / --groupmap are
        // forwarded as `--key=value` arguments. With `protect_args` the value
        // is shipped verbatim; without `protect_args`, upstream wraps it in
        // `safe_arg("--usermap", value)` which escapes shell + wildcard
        // characters so a downstream `eval "$@"` does not glob-expand them.
        // We rely on `protect_args` being the default for SSH transports
        // (matching upstream's `old_style_args = -1` default at options.c:325),
        // so the verbatim form is correct and the wildcard `*` survives.
        if let Some(mapping) = self.config.user_mapping() {
            args.push(OsString::from(format!("--usermap={}", mapping.spec())));
        }
        if let Some(mapping) = self.config.group_mapping() {
            args.push(OsString::from(format!("--groupmap={}", mapping.spec())));
        }

        // upstream: options.c:2716-2723 - --iconv forwarding. When iconv_opt
        // contains a comma, only the post-comma half (the remote charset) is
        // forwarded; otherwise the whole string is forwarded as-is.
        // `--iconv=-` (Disabled) and the default (Unspecified) forward
        // nothing because upstream nulls iconv_opt at options.c:2052-2054
        // before this branch runs.
        match self.config.iconv() {
            IconvSetting::Unspecified | IconvSetting::Disabled => {}
            IconvSetting::LocaleDefault => {
                args.push(OsString::from("--iconv=."));
            }
            IconvSetting::Explicit { local, remote } => {
                let forwarded = remote.as_deref().unwrap_or(local);
                args.push(OsString::from(format!("--iconv={forwarded}")));
            }
        }

        // upstream: options.c:2986-2993 - remote_options[] are appended after
        // all other server arguments. Each -M value is forwarded verbatim.
        for opt in self.config.remote_options() {
            args.push(opt.clone());
        }
    }

    /// Builds the compact flag string from client configuration.
    ///
    /// Format: `-logDtpre.LsfxC` where:
    /// - Transfer flags before `.` separator
    /// - Info/debug flags after `.` separator
    fn build_flag_string(&self) -> String {
        let mut flags = String::from("-");

        // upstream: options.c:2169-2188 - when --files-from is active, upstream
        // sets recurse=0, xfer_dirs=1, relative_paths=1. Suppress 'r' and imply
        // 'R' to match this behaviour.
        let files_from_active = self.config.files_from().is_active();
        let effective_recursive = self.config.recursive() && !files_from_active;
        let effective_relative = self.config.relative_paths() || files_from_active;
        let effective_dirs = self.config.dirs() || files_from_active;
        // upstream: options.c:2641 / :2655 - several compact letters live in a
        // direction-specific branch. `am_sender` is true when the local process
        // is the sender (push); false on a pull. RemoteRole::Sender == push.
        let am_sender = self.role == RemoteRole::Sender;

        // The compact letter ORDER mirrors upstream `server_options()`
        // (options.c:2619-2723) byte-for-byte so the server arg string matches
        // upstream rsync exactly. Letters are appended in upstream's source
        // order, NOT alphabetical or grouped-by-concern order.

        // upstream: options.c:2625-2626 - one 'v' per verbosity level, first.
        for _ in 0..self.config.verbosity() {
            flags.push('v');
        }
        // upstream: options.c:2630-2631 - `make_backups` rides as `b`. Emitting
        // `--backup` as a long arg lands as a positional path on upstream
        // server parsers, so `b` is mandatory here.
        if self.config.backup() {
            flags.push('b');
        }
        // upstream: options.c:2632-2633 - update_only.
        if self.config.update() {
            flags.push('u');
        }
        // upstream: options.c:2634-2635 - 'n' = dry_run (!do_xfers), NOT
        // numeric_ids (which is long-form --numeric-ids).
        if self.config.dry_run() {
            flags.push('n');
        }
        // upstream: options.c:2636-2637 - preserve_links.
        if self.config.links() {
            flags.push('l');
        }
        // upstream: options.c:2638-2640 - 'd' = --dirs (xfer_dirs without
        // recursion). When --files-from is active, upstream sets xfer_dirs=1 and
        // recurse=0, so 'd' is emitted.
        if effective_dirs && !effective_recursive {
            flags.push('d');
        }
        if am_sender {
            // upstream: options.c:2642-2654 - sender-only compact letters.
            if self.config.keep_dirlinks() {
                flags.push('K');
            }
            if self.config.prune_empty_dirs() {
                flags.push('m');
            }
            // upstream: options.c:2646-2649 - 'O' = --omit-dir-times, 'J' =
            // --omit-link-times. These are sender-only compact letters (the
            // `if (am_sender)` block), placed after 'm' and before the fuzzy
            // 'y' letters. For a pull (remote is the sender) they are NOT sent;
            // the local receiver applies omit-dir/link-times itself. Sending
            // `--omit-dir-times` as a separate long option to the remote sender
            // makes it stat the flag as a source path.
            if self.config.omit_dir_times() {
                flags.push('O');
            }
            if self.config.omit_link_times() {
                flags.push('J');
            }
            // upstream: options.c:2650-2654 - 'y' for fuzzy, 'yy' for level 2.
            for _ in 0..self.config.fuzzy_level() {
                flags.push('y');
            }
        } else {
            // upstream: options.c:2655-2660 - receiver-only compact letters.
            // copy_links/copy_dirlinks dereference on the sender, so they are
            // forwarded to the remote only when the remote is the sender (pull).
            if self.config.copy_links() {
                flags.push('L');
            }
            if self.config.copy_dirlinks() {
                flags.push('k');
            }
        }
        // upstream: options.c:2662-2663 - only send 'W' when explicitly set
        // (whole_file > 0). The default for remote transfers is no-whole-file;
        // upstream never sends --no-whole-file because it's the default.
        if self.config.whole_file_raw() == Some(true) {
            flags.push('W');
        }
        // upstream: options.c:2668-2672 - preserve_hard_links.
        if self.config.preserve_hard_links() {
            flags.push('H');
        }
        // upstream: options.c:2673-2674 - preserve_uid.
        if self.config.preserve_owner() {
            flags.push('o');
        }
        // upstream: options.c:2675-2676 - preserve_gid.
        if self.config.preserve_group() {
            flags.push('g');
        }
        // upstream: options.c:2677-2678 - preserve_devices (specials handled
        // via long-form --specials/--no-specials).
        if self.config.preserve_devices() || self.config.preserve_specials() {
            flags.push('D');
        }
        // upstream: options.c:2679-2680 - preserve_mtimes.
        if self.config.preserve_times() {
            flags.push('t');
        }
        // upstream: options.c:2681-2685 - preserve_atimes.
        if self.config.preserve_atimes() {
            flags.push('U');
        }
        // upstream: options.c:2686-2689 - preserve_crtimes.
        if self.config.preserve_crtimes() {
            flags.push('N');
        }
        if self.config.preserve_permissions() {
            // upstream: options.c:2690-2691 - preserve_perms.
            flags.push('p');
        } else if self.config.preserve_executability() && am_sender {
            // upstream: options.c:2692-2693 - 'E' only when preserve_perms is
            // false AND we are the sender.
            flags.push('E');
        }
        #[cfg(all(any(unix, windows), feature = "acl"))]
        if self.config.preserve_acls() {
            // upstream: options.c:2694-2697 - preserve_acls (after p/E).
            flags.push('A');
        }
        #[cfg(all(unix, feature = "xattr"))]
        if self.config.preserve_xattrs() {
            // upstream: options.c:2698-2704 - preserve_xattrs (before r).
            flags.push('X');
        }
        // upstream: options.c:2705-2706 - recurse.
        if effective_recursive {
            flags.push('r');
        }
        // upstream: options.c:2707-2708 - always_checksum.
        if self.config.checksum() {
            flags.push('c');
        }
        // upstream: options.c:2711-2712 - ignore_times rides in the compact
        // flag string as `I`, NOT as a long-form `--ignore-times`. Emitting the
        // long form lands `--ignore-times` as a positional path on the remote
        // server's arg parser (link_stat "--ignore-times" failed), so the
        // compact letter is mandatory here.
        if self.config.ignore_times() {
            flags.push('I');
        }
        // upstream: options.c:2713-2714 - relative_paths.
        if effective_relative {
            flags.push('R');
        }
        // upstream: options.c:2715-2719 - one_file_system ('x', 'xx').
        for _ in 0..self.config.one_file_system_level() {
            flags.push('x');
        }
        // upstream: options.c:2720-2721 - sparse_files.
        if self.config.sparse() {
            flags.push('S');
        }
        // upstream: options.c:2722-2723 - 'z' only for zlib compression.
        if self.config.compress() {
            flags.push('z');
        }
        if self.config.partial() {
            flags.push('P');
        }

        flags
    }
}

/// Shell metacharacters that upstream rsync escapes in filename arguments.
///
/// upstream: options.c `SHELL_CHARS` - characters requiring backslash escaping
/// when passing filename arguments through a remote shell that evaluates them
/// via `eval "$@"`.
const SHELL_CHARS: &str = "!#$&;|<>(){}\"'` \t\\";

/// Wildcard characters recognized by upstream rsync.
///
/// upstream: options.c `WILD_CHARS` - when a backslash precedes one of these
/// characters in a filename argument, the backslash is kept as-is (it already
/// serves as a wildcard escape), so we do not double-escape it.
const WILD_CHARS: &str = "*?[]";

/// Backslash-escapes shell metacharacters in a filename argument.
///
/// Mirrors upstream `options.c:safe_arg(NULL, arg)` which prepends a backslash
/// before every character in `SHELL_CHARS`, with special handling:
///
/// - Backslash itself is only escaped when it does NOT precede a wildcard
///   character (`*`, `?`, `[`, `]`), preserving intentional wildcard escapes.
/// - A leading `-` is prefixed with `./` to prevent the remote server from
///   interpreting the path as an option.
///
/// This escaping is applied when `protect_args` is not active, matching the
/// upstream condition `!protect_args && old_style_args < 2`.
pub(super) fn shell_safe_filename_arg(arg: &str) -> String {
    shell_safe_filename_arg_with_tilde(arg, false)
}

/// Backslash-escapes shell metacharacters in a filename argument, optionally
/// escaping a leading `~`.
///
/// Behaves like [`shell_safe_filename_arg`]; when `escape_leading_tilde` is
/// set and `arg` begins with `~`, a single backslash is prepended (`~foo` ->
/// `\~foo`) so the remote shell does not tilde-expand a path literally named
/// `~foo`. Mirrors upstream `options.c:2553-2558` / `:2581`, where the
/// `escape_leading_tilde` flag is set only on a pull (`!am_sender`) for an
/// untrusted sender and a relative/no-slash path; the caller computes that
/// gate and passes the result here.
pub(super) fn shell_safe_filename_arg_with_tilde(arg: &str, escape_leading_tilde: bool) -> String {
    let leading_dash = arg.starts_with('-');
    let leading_tilde = escape_leading_tilde && arg.starts_with('~');
    let needs_escaping =
        leading_dash || leading_tilde || arg.chars().any(|c| SHELL_CHARS.contains(c));
    if !needs_escaping {
        return arg.to_owned();
    }

    let mut out = String::with_capacity(arg.len() + 16);

    if leading_dash {
        out.push_str("./");
    } else if leading_tilde {
        // upstream: options.c:2581 - a single backslash before a leading ~.
        out.push('\\');
    }

    let chars: Vec<char> = arg.chars().collect();
    for (i, &ch) in chars.iter().enumerate() {
        if ch == '\\' {
            // upstream: backslash is only escaped when the next character is
            // NOT a wildcard (preserving intentional wildcard escapes).
            let next = chars.get(i + 1).copied().unwrap_or('\0');
            if !WILD_CHARS.contains(next) {
                out.push('\\');
            }
        } else if SHELL_CHARS.contains(ch) {
            out.push('\\');
        }
        out.push(ch);
    }

    out
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

/// Formats a `SystemTime` deadline as `YYYY/MM/DDTHH:MM` for the `--stop-at`
/// server argument.
///
/// Uses UTC to avoid timezone ambiguity between local and remote hosts.
/// Returns `None` if the timestamp predates the UNIX epoch (cannot be
/// represented).
///
/// The format matches upstream rsync's `stopat_format()` in `options.c`, which
/// produces `YYYY/MM/DDTHH:MM:SS`. We drop seconds (always `:00`) because the
/// server-side `parse_time()` only parses `HH:MM`.
///
/// # Algorithm
///
/// Calendar conversion uses Howard Hinnant's `civil_from_days` algorithm for
/// correct Gregorian date computation without external crate dependencies.
pub(super) fn format_system_time_for_stop_at(time: SystemTime) -> Option<String> {
    let secs = time.duration_since(SystemTime::UNIX_EPOCH).ok()?.as_secs();
    let (year, month, day, hour, minute) = unix_secs_to_utc_components(secs);
    Some(format!(
        "{year:04}/{month:02}/{day:02}T{hour:02}:{minute:02}"
    ))
}

/// Converts UNIX seconds to UTC calendar components (year, month, day, hour, minute).
///
/// Uses Howard Hinnant's `civil_from_days` algorithm (public domain) which
/// converts a day count since the epoch into Gregorian year/month/day
/// components in O(1) with no branching on month lengths or leap years.
pub(super) fn unix_secs_to_utc_components(secs: u64) -> (i32, u8, u8, u8, u8) {
    let day_secs = secs % 86_400;
    let hour = (day_secs / 3_600) as u8;
    let minute = ((day_secs % 3_600) / 60) as u8;

    // civil_from_days: convert days since 1970-01-01 to (year, month, day).
    // Shift epoch to 0000-03-01 so Feb is the last month of a "year",
    // simplifying leap-year handling.
    let z = (secs / 86_400) as i64 + 719_468; // days since 0000-03-01
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u32; // day of era [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // day of year [0, 365]
    let mp = (5 * doy + 2) / 153; // month prime [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // day [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // month [1, 12]
    let y = if m <= 2 { y + 1 } else { y }; // adjust year for Jan/Feb

    (y as i32, m as u8, d as u8, hour, minute)
}
