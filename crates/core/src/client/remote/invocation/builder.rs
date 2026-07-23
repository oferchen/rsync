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
use super::super::flags;
use super::super::output_option::{OutputWordKind, make_output_option};
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
/// When `--protect-args` / `-s` is enabled, the builder mirrors upstream
/// `server_options()`: the protected head - `rsync --server [--sender]
/// -<flags>e.<caps> [--iconv=...]` - stays on the spawned command line, and
/// only the remainder (long-form options, `.`, and the path arguments) is
/// returned in `SecludedInvocation::stdin_args` for transmission over stdin
/// after SSH connection establishment (upstream: `options.c:2745-2746`
/// NULL cutoff, `rsync.c:283-320 send_protected_args()`).
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
    /// protected head - `rsync --server [--sender] -<flags>e.<caps>
    /// [--iconv=...]` - and the remainder (the long-form options, `.`, and
    /// the path arguments) is returned in `stdin_args` for transmission over
    /// stdin after the SSH connection is established.
    ///
    /// When secluded args is not active, this returns the same result as
    /// `build_with_paths` with an empty `stdin_args`.
    ///
    /// # Upstream Reference
    ///
    /// Mirrors upstream `options.c:2604-2745 server_options()`, which keeps
    /// `--server`, `--sender`, the compact flag string (with `s` and the
    /// capability suffix), and `--iconv` on the actual spawned command line
    /// even under `--secluded-args`; only the arguments emitted after the
    /// `protect_args && !local_server` NULL cutoff (`options.c:2745-2746`)
    /// are deferred to `send_protected_args()` (`rsync.c:283-320`).
    pub fn build_secluded(self, remote_paths: &[&str]) -> SecludedInvocation {
        if !self.config.protect_args().unwrap_or(false) {
            return SecludedInvocation {
                command_line_args: self.build_with_paths(remote_paths),
                stdin_args: Vec::new(),
            };
        }

        let mut cmd_args = Vec::new();
        if let Some(rsync_path) = self.config.rsync_path() {
            cmd_args.push(OsString::from(rsync_path));
        } else {
            cmd_args.push(OsString::from("rsync"));
        }
        cmd_args.extend(self.build_head_args());

        let stdin_args = self.build_tail_args_for_stdin(remote_paths);

        SecludedInvocation {
            command_line_args: cmd_args,
            stdin_args,
        }
    }

    /// Builds the tail portion for stdin transmission in secluded-args mode:
    /// the long-form options, `.`, and the remote paths as `String` values
    /// suitable for null-separated transmission. No shell escaping is
    /// applied because stdin args are null-separated, not eval'd.
    ///
    /// # Upstream Reference
    ///
    /// This is exactly the portion upstream emits after the `protect_args`
    /// NULL cutoff and ships via `send_protected_args()` (`rsync.c:283-320`).
    /// The leading `"rsync"` element mirrors `rsync.c:293 args[i] =
    /// "rsync";`, which overwrites the NULL-cutoff slot with a synthetic
    /// arg0 so the receiver's `read_args()`/`parse_arguments()` can skip
    /// argv[0] the same way it would for a real command line; the
    /// server-side reader (`frontend/server/run.rs`) discards this element.
    fn build_tail_args_for_stdin(&self, remote_paths: &[&str]) -> Vec<String> {
        let mut args = vec![OsString::from("rsync")];
        self.append_long_form_args(&mut args);
        args.push(OsString::from("."));
        for path in remote_paths {
            args.push(OsString::from(*path));
        }
        args.into_iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    /// Builds the protected head of the server invocation: `--server`,
    /// optional `--sender`, the compact flag string (with the `s` letter and
    /// capability suffix embedded), and `--iconv=...` when configured.
    ///
    /// # Upstream Reference
    ///
    /// `options.c:2604-2745 server_options()` builds exactly this sequence
    /// before inserting the `protect_args && !local_server` NULL cutoff
    /// (`options.c:2745-2746`) that splits the spawned command line from the
    /// arguments `send_protected_args()` defers to stdin. This method
    /// returns the pre-cutoff portion, so it belongs on the actual process
    /// command line even when secluded-args defers the remainder
    /// (`append_long_form_args` + `.` + paths) to stdin - see
    /// [`Self::build_secluded`].
    fn build_head_args(&self) -> Vec<OsString> {
        let mut args = Vec::new();

        args.push(OsString::from("--server"));
        if self.role == RemoteRole::Receiver {
            args.push(OsString::from("--sender"));
        }

        let mut flags = self.build_flag_string();

        // upstream: options.c:2604 - `if (protect_args) argstr[x++] = 's';`
        // packs 's' as the FIRST transfer-flag letter, immediately after the
        // leading '-' and before verbosity. Embedding it here (rather than
        // emitting a standalone `-s` token) keeps the compact flag string a
        // single argv slot, matching upstream's argstr byte-for-byte.
        if self.config.protect_args().unwrap_or(false) {
            flags.insert(1, 's');
        }

        // upstream: options.c:2728 - maybe_add_e_option() appends the `e.xxx`
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
        flags.push_str(&build_capability_string_suffix(advertise_inc_recurse));
        args.push(OsString::from(flags));

        if let Some(arg) = self.iconv_arg() {
            args.push(arg);
        }

        args
    }

    /// Builds the `--iconv=...` argument when configured, or `None`.
    ///
    /// upstream: `options.c:2734-2741` - forwarded immediately before the
    /// `protect_args` NULL cutoff, so `--iconv` stays on the command line
    /// even under secluded-args.
    fn iconv_arg(&self) -> Option<OsString> {
        match self.config.iconv() {
            IconvSetting::Unspecified | IconvSetting::Disabled => None,
            IconvSetting::LocaleDefault => Some(OsString::from("--iconv=.")),
            IconvSetting::Explicit { local, remote } => {
                let forwarded = remote.as_deref().unwrap_or(local);
                Some(OsString::from(format!("--iconv={forwarded}")))
            }
        }
    }

    /// Builds the argument list without the rsync program name.
    ///
    /// This is shared between normal `build_with_paths` and secluded-args
    /// `build_tail_args_for_stdin`. The result includes `--server`, optional
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
        let mut args = self.build_head_args();

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
                // upstream: main.c:622 safe_arg(NULL, *remote_argv++)
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
        // upstream: options.c server_options() - `am_sender` is true when the
        // local process is the sender (a PUSH). RemoteRole::Sender == am_sender.
        // A large block of options (options.c:2825-2857, 2911-2943, 2990) is
        // emitted only inside `if (am_sender)` because they steer the remote
        // RECEIVER; forwarding them on a PULL makes the remote sender link_stat()
        // the flag as a source path or mutate its own send behaviour (the
        // --delete-excluded leak is the worst: it rewrites the remote sender's
        // send_rules so excluded files vanish from the file list).
        let am_sender = self.role == RemoteRole::Sender;

        // upstream: options.c:2896-2897 - `if (ignore_errors) --ignore-errors`,
        // well after the protect_args NULL cutoff, so under secluded-args this
        // rides the deferred stdin stream rather than the command line.
        if self.config.ignore_errors() {
            args.push(OsString::from("--ignore-errors"));
        }

        // upstream: options.c:2930-2931 - `if (do_fsync) --fsync` lives inside
        // the `if (am_sender)` block, so it is forwarded only on a PUSH
        // (RemoteRole::Sender): the remote receiver fsyncs the files it writes.
        // On a PULL the local receiver fsyncs its own writes and the remote
        // sender, which never writes destination files, must not receive it.
        if self.config.fsync() && self.role == RemoteRole::Sender {
            args.push(OsString::from("--fsync"));
        }

        if let Some(depth) = self.config.io_uring_depth() {
            args.push(OsString::from(format!("--io-uring-depth={depth}")));
        }

        // upstream: options.c:2747-2748 - `if (list_only > 1) "--list-only"`.
        // Only the EXPLICIT `--list-only` (list_only == 2) is forwarded; the
        // implicit single-source listing (list_only == 1) is not. The compact
        // 'n' letter is NOT packed for list-only (that tracks dry_run only), so
        // this is the sole signal the remote receives.
        if self.config.list_only_arg() {
            args.push(OsString::from("--list-only"));
        }

        // upstream: options.c:2750-2753 - `if (xfer_dirs && !recurse &&
        // delete_mode && am_sender) args[ac++] = "--no-r"`. When a PUSH deletes
        // with --dirs but without recursion (`-d --delete` sans `-r`), the
        // remote receiver must be told recursion is off: the compact flag string
        // carries 'd' but no 'r', and older receivers gained `--no-r` at the
        // same time as `-d`, so the explicit negation guarantees the remote does
        // not re-enable recursion. `--files-from` resolves to xfer_dirs=1,
        // recurse=0 (options.c:2188-2191), matching `effective_*` below.
        let files_from_active = self.config.files_from().is_active();
        let effective_recursive = self.config.recursive() && !files_from_active;
        let effective_dirs = self.config.dirs() || files_from_active;
        if am_sender && effective_dirs && !effective_recursive && self.config.delete() {
            args.push(OsString::from("--no-r"));
        }

        // upstream: options.c:2782-2785 - `if (msgs2stderr == 1) "--msgs2stderr";
        // else if (msgs2stderr == 0) "--no-msgs2stderr"`. The default (2) is not
        // forwarded. Modelled as the tri-state `Option<bool>`.
        match self.config.msgs2stderr() {
            Some(true) => args.push(OsString::from("--msgs2stderr")),
            Some(false) => args.push(OsString::from("--no-msgs2stderr")),
            None => {}
        }

        // upstream: options.c:2825-2857 - the delete timing/limit and size
        // filters all sit inside the `if (am_sender)` block, so they are
        // forwarded only on a PUSH; the remote receiver is what performs the
        // deletion and size-based skip decisions.
        if am_sender {
            // upstream: options.c:2826-2831 - `if (max_delete > 0)
            // --max-delete=N; else if (max_delete == 0) --max-delete=-1`. A
            // ceiling of 0 MUST be remapped to -1: the remote receiver reads
            // `--max-delete=0` as UNLIMITED (max_delete <= 0 disables the cap at
            // options.c:2182-2184), so forwarding `--max-delete=0` with --delete
            // would delete every extraneous file instead of none. Placed first
            // to match upstream's emission order within the am_sender block.
            if let Some(max) = self.config.max_delete() {
                if max > 0 {
                    args.push(OsString::from(format!("--max-delete={max}")));
                } else {
                    args.push(OsString::from("--max-delete=-1"));
                }
            }

            // upstream: options.c:2832-2835 - --min-size / --max-size.
            if let Some(min) = self.config.min_file_size() {
                args.push(OsString::from(format!("--min-size={min}")));
            }
            if let Some(max) = self.config.max_file_size() {
                args.push(OsString::from(format!("--max-size={max}")));
            }

            // upstream: options.c:2836-2845 - delete timing variants. Explicit
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

            // upstream: options.c:2846-2847 - --delete-excluded. On a PULL this
            // must NOT be forwarded: it rewrites the remote sender's send_rules
            // so excluded files disappear from the file list entirely.
            if self.config.delete_excluded() {
                args.push(OsString::from("--delete-excluded"));
            }

            // upstream: options.c:2848-2849 - --force.
            if self.config.force_replacements() {
                args.push(OsString::from("--force"));
            }
        }

        // upstream: options.c:2863-2864 - `--max-alloc=arg` is forwarded to
        // the server when the user supplied a non-default value. Each side
        // owns its own cap, so forwarding lets the remote enforce the same
        // budget the client requested.
        if let Some(limit) = self.config.max_alloc() {
            args.push(OsString::from(format!("--max-alloc={limit}")));
        }

        // upstream: options.c:2873-2875 - server_options() forwards the
        // modify_window value only when it was explicitly set AND the local
        // side is the sender (`modify_window_set && am_sender`): the remote
        // receiver's generator is what performs the mtime quick-check. A
        // negative window (nanosecond-exact) is sent via the short `-@%d`
        // spelling (e.g. `-@-1`); a non-negative window uses `--modify-window=%d`.
        if self.role == RemoteRole::Sender
            && let Some(window) = self.config.modify_window()
        {
            if window < 0 {
                args.push(OsString::from(format!("-@{window}")));
            } else {
                args.push(OsString::from(format!("--modify-window={window}")));
            }
        }

        // upstream: options.c - compress_level sent to server when
        // explicitly set.
        if let Some(level) = self.config.compression_level() {
            let numeric = compression_level_to_numeric(level);
            args.push(OsString::from(format!("--compress-level={numeric}")));
        }

        // upstream: options.c:2818-2823 - compress choice forwarding.
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
                // upstream: options.c:2820 - explicit zlib sent as --old-compress
                "zlib" => args.push(OsString::from("--old-compress")),
                // upstream: options.c:2822-2823 - other algorithms
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

        // upstream: options.c:2799 - `--bwlimit=%d` forwards the rate in whole
        // KiB (options.c:1718), NOT bytes: the remote peer re-parses the value
        // with a default `K` suffix, so a byte count would be scaled up 1024x.
        if let Some(bwlimit) = self.config.bandwidth_limit() {
            args.push(OsString::from(format!(
                "--bwlimit={}",
                bwlimit.server_option_kib()
            )));
        }

        // upstream: options.c:2886-2894 server_options() -
        //   if (partial_dir && am_sender) {
        //       if (partial_dir != tmp_partialdir) --partial-dir <dir>;
        //       if (delay_updates) --delay-updates;
        //   } else if (keep_partial && am_sender) --partial
        // The whole group is am_sender (PUSH) only. --delay-updates implies an
        // implicit tmp partial_dir upstream, so it is emitted even without an
        // explicit --partial-dir; config.partial_directory() holds only an
        // explicit dir, mirroring the `partial_dir != tmp_partialdir` guard.
        // There is no compact 'P'.
        if am_sender {
            if let Some(dir) = self.config.partial_directory() {
                let mut arg = OsString::from("--partial-dir=");
                arg.push(dir.as_os_str());
                args.push(arg);
                if self.config.delay_updates() {
                    args.push(OsString::from("--delay-updates"));
                }
            } else if self.config.delay_updates() {
                args.push(OsString::from("--delay-updates"));
            } else if self.config.partial() {
                args.push(OsString::from("--partial"));
            }
        }

        // upstream: options.c:2925-2928 - `if (tmpdir) { --temp-dir; ... }`
        // inside the `if (am_sender)` block. Forwarded only on a PUSH so the
        // remote receiver writes temp files under the requested directory; a
        // remote sender never writes temp files and must not receive it.
        if am_sender && let Some(dir) = self.config.temp_directory() {
            let mut arg = OsString::from("--temp-dir=");
            arg.push(dir.as_os_str());
            args.push(arg);
        }

        // upstream: options.c:2951-2960 server_options() - `if (append_mode)
        // { --append... } else if (inplace) { --inplace }`. append_mode takes
        // precedence, so --inplace is suppressed when appending (the receiver
        // derives inplace from append); the two are mutually exclusive.
        if self.config.inplace() && !self.config.append() {
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

        // upstream: options.c:2760-2765 - the compact 'D' letter tracks
        // preserve_devices only (see build_flag_string). specials are conveyed
        // separately: `if (preserve_devices) { if (!preserve_specials)
        // --no-specials } else if (preserve_specials) --specials`. Note
        // --no-specials (not --devices) because sending --devices would not be
        // backward-compatible; -D already carries devices.
        if self.config.preserve_devices() {
            if !self.config.preserve_specials() {
                args.push(OsString::from("--no-specials"));
            }
        } else if self.config.preserve_specials() {
            args.push(OsString::from("--specials"));
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

        // upstream: options.c:2905-2906 - --numeric-ids is long-form only.
        if self.config.numeric_ids() {
            args.push(OsString::from("--numeric-ids"));
        }

        // upstream: options.c:2908-2909 - `if (use_qsort) "--use-qsort"`.
        // Unconditional (not direction-gated): the peer sorts its file list
        // with the same comparator so both sides agree on ordering.
        if self.config.qsort() {
            args.push(OsString::from("--use-qsort"));
        }

        // upstream: options.c:2511 - server_options() NEVER forwards
        // --trust-sender. `parse_arguments()` only sets the internal
        // `trust_sender_args`/`trust_sender` locals; the server side always
        // trusts the client (am_server implies trust), so the flag is not a
        // wire option. Forwarding it diverged from upstream.

        // upstream: options.c:2892-2894 - --checksum-seed=N forwarded so the
        // server uses the same seed for rolling and strong checksum generation.
        if let Some(seed) = self.config.checksum_seed() {
            args.push(OsString::from(format!("--checksum-seed={seed}")));
        }

        // upstream: options.c:2854-2855 - `if (size_only) --size-only` inside
        // the `if (am_sender)` block (a PUSH), so the remote receiver's
        // generator applies the size-only quick-check the client requested.
        if am_sender && self.config.size_only() {
            args.push(OsString::from("--size-only"));
        }

        // upstream: options.c:2858-2860 - `else { if (skip_compress)
        // safe_arg("--skip-compress", skip_compress); }`. The else is the
        // `!am_sender` branch, so `--skip-compress` is forwarded only on a PULL
        // (RemoteRole::Receiver): the remote sender performs the compression and
        // must skip the same suffixes. Only an explicitly-set spec is forwarded;
        // the built-in default suffix list is never sent (upstream's
        // skip_compress global is NULL unless --skip-compress was given).
        if !am_sender && let Some(spec) = self.config.skip_compress_spec() {
            args.push(OsString::from(format!("--skip-compress={spec}")));
        }
        // upstream: options.c:2918-2923 - --ignore-existing and --existing
        // (sent as --existing for ignore_non_existing) both sit inside the
        // `if (am_sender)` block: they steer the remote receiver's generator,
        // so they are forwarded only on a PUSH.
        // upstream: options.c:2711-2712 - --ignore-times is emitted as the
        // compact `I` letter in build_flag_string(), not as a long-form arg.
        if am_sender {
            if self.config.ignore_existing() {
                args.push(OsString::from("--ignore-existing"));
            }
            if self.config.existing_only() {
                args.push(OsString::from("--existing"));
            }
        }

        // upstream: options.c:817-818 - missing_args forwarded as long-form.
        if self.config.ignore_missing_args() {
            args.push(OsString::from("--ignore-missing-args"));
        }
        if self.config.delete_missing_args() {
            args.push(OsString::from("--delete-missing-args"));
        }

        // upstream: options.c:2982-2985 - `if (remove_source_files == 1)
        // "--remove-source-files"; else if (remove_source_files)
        // "--remove-sent-files"`. The deprecated alias is forwarded verbatim so
        // a pre-3.0 remote sees the spelling the user typed; every supported
        // remote accepts both, but we mirror upstream byte-for-byte.
        if self.config.remove_source_files() {
            if self.config.remove_sent_files() {
                args.push(OsString::from("--remove-sent-files"));
            } else {
                args.push(OsString::from("--remove-source-files"));
            }
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

        // upstream: options.c:2979 - `if (write_devices && am_sender) args[ac++]
        // = "--write-devices"`. Forwarded only on a PUSH (local process is the
        // sender), so the remote receiver writes file data into matching device
        // destinations instead of recreating them with mknod. `RemoteRole::Sender`
        // is am_sender (see `am_sender` above).
        if self.config.write_devices() && self.role == RemoteRole::Sender {
            args.push(OsString::from("--write-devices"));
        }

        // upstream: options.c:2987 - `if (copy_devices && !am_sender) args[ac++]
        // = "--copy-devices"`. Forwarded only on a PULL (local process is the
        // receiver), so the remote sender reads device contents as regular file
        // data. `RemoteRole::Receiver` is !am_sender (a pull).
        if self.config.copy_devices() && self.role == RemoteRole::Receiver {
            args.push(OsString::from("--copy-devices"));
        }

        // upstream: options.c:2852-2857 server_options() - inside the
        // `if (am_sender)` block: `--super` when `am_root > 1` (an explicit
        // --super, never mere root), then `--stats` when `do_stats`. Both are
        // forwarded only on a push (RemoteRole::Sender is am_sender), where the
        // remote receiver/generator performs the privileged operations and
        // computes the transfer statistics. `--fake-super` (am_root == -1) is
        // never forwarded in either direction: it is a receiver-local storage
        // mode (special-file metadata goes to user.rsync.%stat xattrs) that the
        // peer must not learn about; forwarding it to a remote sender on a PULL
        // made the remote stat source paths under fake-super semantics, which
        // upstream never does.
        if self.role == RemoteRole::Sender {
            args.extend(flags::sender_super_stats_args(self.config).map(OsString::from));
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

        // upstream: options.c:2648-2649 - bare `make_backups` is emitted as the
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

        // upstream: options.c:2911-2934 - the basis-dir args (--compare-dest,
        // --copy-dest, --link-dest via alt_dest_opt) live entirely inside the
        // `if (am_sender)` block: "the server only needs this option if it is
        // not the sender". On a PUSH (local is sender, RemoteRole::Sender) the
        // remote server is the receiver and needs the basis dirs. On a PULL the
        // local receiver applies them locally and must NOT forward them, or the
        // remote sender would link_stat() the flag as a source path.
        if self.role == RemoteRole::Sender {
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
        }

        // upstream: options.c:2945-2949 server_options() - make_output_option()
        // forwards the explicitly-set --info / --debug levels to the peer so
        // its diagnostic output matches. `am_sender` (a push) selects the
        // receiving half of the role `where` filter.
        if let Some(arg) =
            make_output_option(OutputWordKind::Info, self.config.info_flags(), am_sender)
        {
            args.push(OsString::from(arg));
        }
        if let Some(arg) =
            make_output_option(OutputWordKind::Debug, self.config.debug_flags(), am_sender)
        {
            args.push(OsString::from(arg));
        }

        // upstream: options.c:2993 - `if (open_noatime && preserve_atimes <= 1)
        // args[ac++] = "--open-noatime"`. When `-UU` (preserve_atimes == 2) is
        // active the flag is suppressed: at that level the receiver already
        // opens files with O_NOATIME implicitly, so forwarding it is redundant
        // and upstream drops it.
        if self.config.open_noatime() && self.config.preserve_atimes_level() <= 1 {
            args.push(OsString::from("--open-noatime"));
        }

        // upstream: options.c:2990-2991 - `if (preallocate_files && am_sender)
        // --preallocate`. Forwarded only on a PUSH so the remote receiver
        // preallocates the destination file extents; a remote sender allocates
        // nothing and must not receive it.
        if am_sender && self.config.preallocate() {
            args.push(OsString::from("--preallocate"));
        }

        // upstream: options.c:2768-2780 - `if (stdout_format && am_sender)` the
        // server is told a little about the client's out-format via a
        // `--log-format` arg, in a first-match-wins chain. The `%i` branches key
        // off `stdout_format_has_i`, which upstream derives from the RESOLVED
        // out-format string (options.c:2345-2358), not the `-i` flag: an
        // explicit `--out-format` without `%i` clears it even under `-i`, while
        // `-i` alone installs the default `"%i %n%L"` format. `%i%I` is the
        // `-ii` form (stdout_format_has_i > 1) that itemizes unchanged entries
        // too; `%o` is forwarded when the format has the `%o` operation
        // directive; the placeholder `X` is forwarded when a non-verbose client
        // set an out-format with neither `%i` nor `%o`. The whole chain is gated
        // on `am_sender` (a PUSH); a remote sender never needs it.
        if am_sender {
            if self.config.out_format_forwards_i() {
                if self.config.itemize_unchanged() {
                    args.push(OsString::from("--log-format=%i%I"));
                } else {
                    args.push(OsString::from("--log-format=%i"));
                }
            } else if self.config.out_format_has_operation() {
                args.push(OsString::from("--log-format=%o"));
            } else if self.config.out_format_placeholder() && self.config.verbosity() == 0 {
                args.push(OsString::from("--log-format=X"));
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
            // upstream: options.c:368-369 - a peer that reads the --files-from
            // list defaults relative_paths=1 (options.c:2205-2206). When the
            // client resolved relative paths off (explicit --no-relative), emit
            // --no-relative so the remote sender overrides that default and
            // flattens each entry to its basename (flist.c:2338-2349) with no
            // implied parent dirs.
            if !self.config.relative_paths() {
                args.push(OsString::from("--no-relative"));
            }
        }

        // upstream: options.c:2912-2916 - --usermap / --groupmap are
        // forwarded as `--key=value` arguments. With `protect_args` the value
        // is shipped verbatim; without `protect_args`, upstream wraps it in
        // `safe_arg("--usermap", value)` which escapes shell + wildcard
        // characters so a downstream `eval "$@"` does not glob-expand them.
        // We rely on `protect_args` being the default for SSH transports
        // (matching upstream's `old_style_args = -1` default at options.c:325),
        // so the verbatim form is correct and the wildcard `*` survives.
        //
        // upstream: options.c:2911-2917 - both --usermap and --groupmap sit
        // inside the `if (am_sender)` block, so they are forwarded only on a
        // PUSH: the remote receiver applies the id remapping when it writes
        // ownership. On a PULL the local receiver owns the mapping and the
        // remote sender must not receive it.
        if am_sender {
            if let Some(mapping) = self.config.user_mapping() {
                args.push(OsString::from(format!("--usermap={}", mapping.spec())));
            }
            if let Some(mapping) = self.config.group_mapping() {
                args.push(OsString::from(format!("--groupmap={}", mapping.spec())));
            }
        }

        // upstream: options.c:2734-2741 - --iconv forwarding (post-comma half
        // of iconv_opt when a comma is present, else the whole spec) is
        // emitted immediately before the protect_args NULL cutoff, so it
        // belongs in the protected head (`Self::build_head_args`/
        // `Self::iconv_arg`), not here.

        // upstream: options.c:3004-3011 - remote_options[] are appended after
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

        // upstream: options.c:2187-2206 - when --files-from is active, upstream
        // sets recurse=0, xfer_dirs=1, relative_paths=1. Suppress 'r' and imply
        // 'R' to match this behaviour.
        let files_from_active = self.config.files_from().is_active();
        let effective_recursive = self.config.recursive() && !files_from_active;
        // upstream: options.c:109-110 - server_options() packs the compact `R`
        // letter for the RESOLVED relative_paths. `relative_paths()` already
        // folds in the --files-from default (options.c:2205-2206: relative
        // defaults to 1 under --files-from) at the CLI layer, so it must NOT be
        // re-forced with `|| files_from_active` - that wrongly packs `R` even
        // when the user passed --no-relative, defeating the --no-relative arg
        // emitted below and making the remote sender keep the leading path
        // components (`sub/file` instead of `file`).
        let effective_relative = self.config.relative_paths();
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
        // upstream: options.c:2646-2647 - `if (quiet && msgs2stderr) 'q'`. The
        // default `msgs2stderr` is 2 (nonzero), so plain `-q` packs 'q';
        // `--no-msgs2stderr` (msgs2stderr == 0) suppresses it. Modelled as
        // `msgs2stderr() != Some(false)`.
        if self.config.quiet() && self.config.msgs2stderr() != Some(false) {
            flags.push('q');
        }
        // upstream: options.c:2648-2649 - `make_backups` rides as `b`. Emitting
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
        // upstream: options.c:2677-2678 - `if (preserve_devices) argstr[x++] =
        // 'D'; /* ignore preserve_specials here */`. The compact 'D' letter
        // tracks preserve_devices ONLY; specials ride separately as the
        // long-form --specials/--no-specials (emitted in append_long_form_args).
        if self.config.preserve_devices() {
            flags.push('D');
        }
        // upstream: options.c:2679-2680 - preserve_mtimes.
        if self.config.preserve_times() {
            flags.push('t');
        }
        // upstream: options.c:2681-2685 - `if (preserve_atimes) { 'U'; if
        // (preserve_atimes > 1) 'U'; }`. Level 2 (`-UU`) doubles the letter so
        // the peer also preserves directory access times.
        for _ in 0..self.config.preserve_atimes_level().min(2) {
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
        // upstream: options.c:2698-2704 - `if (preserve_xattrs) { 'X'; if
        // (preserve_xattrs > 1) 'X'; }` (before r). Level 2 (`-XX`) doubles the
        // letter so the peer transfers xattrs even in a fake-super store.
        for _ in 0..self.config.preserve_xattrs_level().min(2) {
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
        // upstream: options.c:2709-2710 - `if (cvs_exclude) argstr[x++] = 'C';`.
        // Forwarded so the remote peer runs get_cvs_excludes() itself (matching
        // upstream, which also forwards the letter alongside the transmitted CVS
        // rules); duplicate excludes are idempotent.
        if self.config.cvs_exclude() {
            flags.push('C');
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
        // upstream: options.c:2722-2723 - the compact 'z' is packed only when
        // `do_compression == CPRES_ZLIB`. Plain `-z` defaults to zlib, but an
        // explicit `--compress-choice` of zlibx/zstd/lz4 is forwarded via the
        // long-form `--new-compress`/`--compress-choice` above and must NOT also
        // pack 'z' (upstream sends `-logDtpre...`, not `-logDtprze...`).
        // `CompressionAlgorithm` folds zlibx onto Zlib, so the enum's name()
        // returns "zlib" for zlibx; use the raw `compress_choice_name` to
        // distinguish it (upstream sends zlibx via --new-compress, no 'z').
        if self.config.compress()
            && (!self.config.explicit_compress_choice()
                || self
                    .config
                    .compress_choice_name()
                    .unwrap_or_else(|| self.config.compression_algorithm().name())
                    == "zlib")
        {
            flags.push('z');
        }
        // upstream: options.c has NO compact 'P' letter for --partial. keep_partial
        // rides as the long-form --partial (append_long_form_args), gated on
        // am_sender && !partial_dir (options.c:2884-2893). Packing 'P' here
        // diverged from every upstream server invocation.

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
/// upstream: options.c:2755-2756 - `--compress-level=%d` forwards the signed
/// `do_compression_level`, so a negative zstd "fast" level reaches the server
/// verbatim rather than being collapsed into an unsigned range.
pub(super) fn compression_level_to_numeric(level: compress::zlib::CompressionLevel) -> i32 {
    use compress::zlib::CompressionLevel;
    match level {
        CompressionLevel::None => 0,
        CompressionLevel::Fast => 1,
        CompressionLevel::Default => 6,
        CompressionLevel::Best => 9,
        CompressionLevel::Precise(n) => i32::from(n.get()),
        CompressionLevel::PreciseSigned(v) => v,
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
