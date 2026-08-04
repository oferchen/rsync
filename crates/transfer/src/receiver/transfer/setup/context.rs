//! The `ReceiverContext` transfer-setup entry point and its helpers.
//!
//! `setup_transfer` activates input multiplex, reads the filter list, receives
//! the (possibly incremental) file list, sanitizes paths, and builds the
//! `PipelineSetup` used by `run_sync`, `run_pipelined`, and
//! `run_pipelined_incremental`. The single-file rename and `--files-from`
//! forwarding helpers live alongside it.

use std::io::{self, Read};
use std::path::PathBuf;
use std::sync::Arc;

use logging::debug_log;
use metadata::{ChmodModifiers, MetadataOptions};
use protocol::filters::{FilterRuleWireFormat, read_filter_list};

use filters::FilterChain;

use crate::receiver::{
    PHASE1_CHECKSUM_LENGTH, PipelineSetup, ReceiverContext, dest_arg_has_trailing_slash,
    ensure_dest_root_exists,
};
use crate::shared::ChecksumFactory;
use crate::transfer_state::TransferPhase;

#[cfg(unix)]
use super::sandbox::open_sandbox_for_dest_strict;
use super::wire_filters::parse_wire_filters_for_receiver;

/// Merges the daemon module `incoming chmod` modifiers with the client
/// `--chmod` modifiers into the single list the receiver applies.
///
/// Upstream keeps both in one `chmod_modes` list: the daemon prepends its
/// module modes ahead of the client's (`clientserver.c:1217`
/// `parse_chmod(p, &chmod_modes)`), and `chmod.c:tweak_mode()` walks the list
/// in order. This mirrors that order - daemon modes first, then the client's -
/// and collapses the two `Option`s so at most one allocation is produced. On a
/// pull `daemon` is always `None`, so only the client `--chmod` survives.
fn merge_chmod(
    daemon: Option<ChmodModifiers>,
    client: Option<ChmodModifiers>,
) -> Option<ChmodModifiers> {
    match (daemon, client) {
        (Some(mut daemon), Some(client)) => {
            daemon.extend(client);
            Some(daemon)
        }
        (daemon, client) => daemon.or(client),
    }
}

impl ReceiverContext {
    /// Common setup for all transfer modes.
    ///
    /// Activates input multiplex, reads filter list if needed, receives the
    /// initial file list, sanitizes paths, and builds the `PipelineSetup` with
    /// checksum and metadata configuration. INC_RECURSE sub-list segments are
    /// pulled on demand by the drivers rather than drained here.
    ///
    /// # Upstream Reference
    ///
    /// - `main.c:1342-1343` - client receiver activates multiplex at protocol >= 23
    /// - `main.c:1167-1168` - server receiver activates multiplex at protocol >= 30
    pub(in crate::receiver) fn setup_transfer<
        R: Read,
        W: io::Write + crate::writer::MsgInfoSender + ?Sized,
    >(
        &mut self,
        reader: crate::reader::ServerReader<R>,
        writer: &mut W,
    ) -> io::Result<(crate::reader::ServerReader<R>, usize, PipelineSetup)> {
        // upstream: generator.c:2260-2261 - emitted at the top of generate_files,
        // just before the per-segment dispatch loop. The receiver-side transfer
        // setup is the closest analog (every `run*` entry point routes through
        // setup_transfer).
        debug_log!(Genr, 1, "generator starting pid={}", std::process::id());

        // upstream: generator.c:2290-2295 - the generator prints the
        // delta-transmission status once, gated on DEBUG_GTE(FLIST, 1) (first
        // active at -vv). whole_file is forced on for local transfers and
        // --whole-file; otherwise the rolling-checksum delta path is used.
        debug_log!(
            Flist,
            1,
            "delta-transmission {}",
            if self.config.flags.whole_file {
                "disabled for local transfer or --whole-file"
            } else {
                "enabled"
            }
        );

        // Parallel receive-side delta apply is unconditionally compiled (PFF-7).
        debug_log!(Recv, 1, "parallel receive-delta path active");

        let mut reader = if self.should_activate_input_multiplex() {
            reader.activate_multiplex().map_err(|e| {
                io::Error::new(
                    e.kind(),
                    format!(
                        "failed to activate INPUT multiplex: {e} {}{}",
                        crate::role_trailer::error_location!(),
                        crate::role_trailer::receiver()
                    ),
                )
            })?
        } else {
            reader
        };

        if self.should_read_filter_list() {
            let wire_rules = read_filter_list(&mut reader, self.protocol).map_err(|e| {
                io::Error::new(
                    e.kind(),
                    format!(
                        "failed to read filter list: {e} {}{}",
                        crate::role_trailer::error_location!(),
                        crate::role_trailer::receiver()
                    ),
                )
            })?;

            self.apply_received_filter_rules(wire_rules)?;
        } else if self.config.connection.client_mode
            && !self.config.connection.filter_rules.is_empty()
        {
            self.populate_client_filter_chains()?;
        }

        // FSM: filter list reading is complete. Advance to FileListTransfer.
        self.pipeline
            .advance_to(TransferPhase::FileListTransfer)
            .map_err(crate::fsm_error)?;

        // upstream: main.c:1191-1198 - server-receiver opened a local
        // `--files-from` file (filesfrom_fd) and now forwards its contents
        // to the sender (the client) over f_out so the sender can build the
        // file list. Upstream interleaves this with `recv_file_list` via the
        // I/O scheduler; we write the whole file out as a single push before
        // entering the flist read because oc-rsync's reader/writer streams
        // are decoupled (no select() loop fanning across them).
        self.forward_files_from_to_sender(writer)?;

        // upstream: flist.c:2639-2642 recv_file_list() - the first list arrival
        // prints `receiving incremental file list` on the client's own output.
        // Write it DIRECTLY to the client stream here instead of through the
        // deferred `info_log!` event buffer: that buffer is only drained by the
        // CLI's post-run flush_diagnostics, after the per-file names, itemize
        // rows, and the summary stats have already gone straight to stdout, so a
        // buffered banner printed dead last. The per-file names take this same
        // direct stdout path (itemize.rs `emit_name_line`), so matching it keeps
        // the banner ahead of them.
        if self.should_announce_incremental_flist() {
            use std::io::Write as _;
            let banner: &[u8] = b"receiving incremental file list\n";
            if self.config.flags.msgs_to_stderr {
                std::io::stderr().write_all(banner)?;
            } else {
                std::io::stdout().write_all(banner)?;
            }
        }

        // INC_RECURSE sub-list segments are no longer drained here. The whole
        // list arrives in `receive_file_list` for the non-INC_RECURSE case (it
        // sets `flist_eof`); under INC_RECURSE the drivers pull segments lazily
        // via the on-demand primitives (`ensure_flat_idx` /
        // `ensure_all_segments_loaded`), mirroring upstream's generator which
        // fetches sub-lists on demand rather than up front (generator.c:2299).
        let file_count = self.receive_file_list(&mut reader)?;

        let (file_count, setup) = self.build_pipeline_setup(file_count)?;

        // upstream: main.c:807-808 - the receiver prints `created directory
        // <dest>` right after the file list arrives, before generate_files()
        // drives the per-entry itemize rows. Emit it here, after the dest-root
        // pre-flight mkdir recorded `dest_root_created` and before the drivers
        // enter their transfer loops, so the notice precedes the per-file
        // output on every driver (`run_sync`, `run_pipelined`,
        // `run_pipelined_incremental`).
        self.announce_created_directory(writer, &setup.dest_dir)?;

        Ok((reader, file_count, setup))
    }

    /// Emits upstream's `created directory <dest>` notice when the receiver
    /// pre-flight-mkdir'd the destination root.
    ///
    /// Routing is delegated to [`emit_info_line`](ReceiverContext::emit_info_line):
    /// a client-mode receiver (pull) writes to its own stdout, while a
    /// server-mode receiver (the SSH/daemon side of a push) frames the line as
    /// `MSG_INFO` so the pushing client renders it - exactly as upstream's
    /// `rprintf(FINFO, ...)` routes through `rwrite()`.
    ///
    /// # Upstream Reference
    ///
    /// - `main.c:807-808` - `if (INFO_GTE(NAME, 1) || stdout_format_has_i)
    ///   rprintf(FINFO, "created directory %s\n", dest_path)`. The print
    ///   precedes the `dry_run++` at `main.c:810`, so a dry run still reports
    ///   the directory it would create.
    /// - `main.c:788-789` - `*cp = '\0'` lops the operand's trailing slash
    ///   before the print, so `dest/` is reported as `dest`.
    ///
    /// The gate reads the `NAME` info category (seeded on the server receiver
    /// from the client's forwarded `-v`/`--info` by
    /// `cli::frontend::server::run`) rather than the raw verbose count, so
    /// `--info=name0` suppresses the notice even under `-v`. The itemize half of
    /// the OR is `stdout_format_has_i` (`out_format_forwards_i`): `-i` sets it
    /// via the default `"%i %n%L"` format, a custom `--out-format` carrying
    /// `%i` sets it without `-i`, and a `%i`-less `--out-format` clears it even
    /// under `-i`. The notice covers the destination root only; alt-basis dirs
    /// (`--copy-dest`/`--link-dest`/`--compare-dest`) never trigger it because
    /// `get_local_name()` only mkdir's `dest_path`.
    fn announce_created_directory<W: crate::writer::MsgInfoSender + ?Sized>(
        &self,
        writer: &mut W,
        dest_dir: &std::path::Path,
    ) -> io::Result<()> {
        if !self.dest_root_created {
            return Ok(());
        }
        if !(logging::info_gte(logging::InfoFlag::Name, 1)
            || self.config.flags.info_flags.out_format_forwards_i)
        {
            return Ok(());
        }
        // upstream lops the operand's single trailing slash (main.c:788-789);
        // mirror that here so `dest/` renders as `dest`, while keeping a bare
        // separator (a root path) intact.
        let shown = dest_dir.to_string_lossy();
        let trimmed = shown.trim_end_matches('/');
        let trimmed = if trimmed.is_empty() {
            shown.as_ref()
        } else {
            trimmed
        };
        self.emit_info_line(writer, &format!("created directory {trimmed}\n"))
    }

    /// Whether this receiver prints the `receiving incremental file list` banner
    /// on its own client-visible output.
    ///
    /// Mirrors upstream `flist.c:2606-2607`: the banner fires only for a
    /// client-side receiver (`!am_server` -> `client_mode`), under incremental
    /// recursion - which upstream disables when `!recurse` (compat.c:172-173), so a
    /// non-recursive single-file `-v` prints nothing - and when the FLIST info
    /// category is at level >= 1, so `--info=flist0` suppresses it even at `-v`.
    /// This is the receive-side twin of the sender's `sending incremental file
    /// list` gate (`recursive() && INFO_GTE(FLIST, 1)`) in
    /// `cli::frontend::execution::drive::summary`.
    pub(in crate::receiver) fn should_announce_incremental_flist(&self) -> bool {
        self.config.connection.client_mode
            && self.config.flags.recursive
            && logging::info_gte(logging::InfoFlag::Flist, 1)
    }

    /// Builds the [`PipelineSetup`] from the received file list.
    ///
    /// This is the reader-free tail of transfer setup: it sanitizes the file
    /// list, derives the checksum and metadata configuration, applies the
    /// single-file rename, creates and (on Unix) sandboxes the destination
    /// root, and advances the FSM into [`TransferPhase::DeltaTransfer`]. Because
    /// none of these steps touch the wire, both [`setup_transfer`](Self::setup_transfer)
    /// and its async twin call it verbatim so they produce an identical
    /// `PipelineSetup` and post-sanitize `file_count` for the same file list.
    ///
    /// `file_count` is the raw count returned by the file-list receive path
    /// (initial plus INC_RECURSE sub-lists) before sanitization.
    /// Builds the receiver's [`MetadataOptions`] from the resolved transfer
    /// config, the single source of the metadata-preservation policy applied to
    /// every file, directory, and `--backup-dir` subdirectory the receiver
    /// writes. Shared by [`build_pipeline_setup`](Self::build_pipeline_setup)
    /// and the backup-parent creation paths so a `--backup-dir` subtree inherits
    /// its source directory's attributes identically regardless of caller
    /// (upstream `copy_valid_path` -> `set_file_attrs`, backup.c:115-138).
    pub(in crate::receiver) fn build_metadata_options(&self) -> MetadataOptions {
        MetadataOptions::new()
            .preserve_permissions(self.config.flags.perms)
            // upstream: options.c:2692-2693 packs the compact 'E' into
            // server_options only inside `else if (preserve_executability &&
            // am_sender)`, so on a pull `-E` never rides the wire to the remote
            // sender; the local client IS the receiver and applies it itself.
            // rsync.c:457-465 set_file_attrs() layers the source executability
            // bits onto the destination mode when `!preserve_perms`. Without
            // this the ssh/daemon pull left files at their existing mode while
            // the local copy executor honoured `-E` (local_copy metadata.rs:7).
            .preserve_executability(self.config.flags.preserve_executability)
            .preserve_times(self.config.flags.times)
            .preserve_atimes(self.config.flags.atimes)
            .preserve_crtimes(self.config.flags.crtimes)
            .preserve_owner(self.config.flags.owner)
            .preserve_group(self.config.flags.group)
            .numeric_ids(self.config.flags.numeric_ids.maps_numeric())
            // upstream: generator.c:1356 - `link_stat(fname, &sx.st,
            // keep_dirlinks && is_dir)` follows a destination symlink-to-dir
            // at stat time instead of rejecting it. The
            // `chmod_path_honoring_keep_dirlinks` helper in
            // `crates/metadata/src/apply/permissions.rs` consults this flag
            // to route past the dirfd sandbox when the symlinked parent
            // would otherwise surface `ELOOP`/`ENOTDIR`. Without this the
            // SSH receiver runs with `keep_dirlinks: false` even when the
            // client sent `K` in the compact flag string, breaking the
            // `symlink-dirlink-basis` regression test (Issue #715).
            .with_keep_dirlinks(self.config.flags.keep_dirlinks)
            // upstream: clientserver.c:1106-1107 - `fake super = yes` on the
            // daemon module forces fake-super metadata storage on the receiver
            // (ownership and special-file metadata go to user.rsync.%stat
            // xattrs instead of being applied to inodes).
            .fake_super(self.config.fake_super)
            // upstream: two chmod sources rewrite the destination mode on the
            // receiver, both funnelled through `chmod.c:tweak_mode()`:
            //   - daemon module `incoming chmod` (clientserver.c:rsync_module()
            //     + generator.c, `daemon_chmod_modes`), and
            //   - the client `--chmod` flag (options.c:1762 `chmod_modes`),
            //     which is never forwarded to the remote, so on a pull the
            //     local client IS the receiver and applies it itself
            //     (flist.c:905-906 recv_file_entry()).
            // Upstream keeps both in one list (clientserver.c:1217 prepends the
            // daemon modes ahead of `chmod_modes`); we merge here in the same
            // order and hand the result to the single chmod-application site in
            // `apply_permissions_with_chmod`. On a pull `daemon_incoming_chmod`
            // is always None, so only the client `--chmod` applies.
            .with_chmod(merge_chmod(
                self.config.daemon_incoming_chmod.clone(),
                self.config.chmod.clone(),
            ))
            // upstream: uidlist.c:recv_id_list() applies parsed --usermap /
            // --groupmap rules at file-list receive time on the receiver.
            // The daemon parsed the wire arg in `apply_long_form_args` and
            // stashed the typed mapping on `ServerConfig`; hand it to
            // `MetadataOptions` so `metadata::apply::ownership` consults it
            // when remapping uid/gid before chown. Without this the wildcard
            // spec `--groupmap=*:GID` from the client is silently dropped on
            // daemon uploads (upstream regression #829 / daemon-groupmap-wild).
            .with_user_mapping(self.config.user_mapping.clone())
            .with_group_mapping(self.config.group_mapping.clone())
    }

    fn build_pipeline_setup(&mut self, file_count: usize) -> io::Result<(usize, PipelineSetup)> {
        let removed = self.sanitize_file_list();
        let file_count = file_count - removed;

        // upstream: flist.c:1019-1030 recv_file_entry() re-runs each received
        // name through the receiver's own filter list and aborts with
        // RERR_UNSUPPORTED if the sender sent a name the receiver excludes. This
        // runs after sanitize (which mirrors clean_fname) so the paths are
        // already cleaned, matching upstream's per-entry ordering.
        self.recheck_received_filter()?;

        // upstream: flist.c:1026-1029 recv_file_entry() also validates each
        // received name against the implied-include list built from the
        // client's requested source args, aborting with RERR_UNSUPPORTED if the
        // sender injected a name that was never requested (CVE-2022-29154).
        self.recheck_received_implied_includes()?;

        let checksum_factory = ChecksumFactory::from_negotiation(
            self.negotiated_algorithms.as_ref(),
            self.protocol,
            self.checksum_seed,
            self.compat_flags.as_ref(),
        );
        let checksum_algorithm = checksum_factory.signature_algorithm();
        let checksum_length = PHASE1_CHECKSUM_LENGTH;

        let metadata_opts = self.build_metadata_options();

        let dest_arg = self.config.args.first();
        let trailing_slash = dest_arg.is_some_and(|arg| dest_arg_has_trailing_slash(arg));
        let dest_dir = dest_arg.map_or_else(|| PathBuf::from("."), PathBuf::from);

        // upstream: main.c:805-832 get_local_name() - single-file rename
        // semantics. When the transfer is exactly one non-directory entry,
        // the operand carries no trailing slash, and the destination path
        // does not name an existing directory, upstream's get_local_name()
        // returns `cp + 1` (the basename of `dest_path`) as `local_name`.
        // The receiver's recv_files() then writes the single payload to
        // `local_name` under `change_dir(parent)` instead of treating the
        // operand as a directory and joining the flist entry's name.
        //
        // Without this remap the daemon receiver treats the operand as a
        // directory: a `rsync -t legit.txt rsync://h/upload/legit.txt`
        // push lands at `mod/legit.txt/legit.txt` because dest_dir is the
        // operand and the per-entry mkdir then creates `legit.txt/` under
        // it. Mirror upstream by rewriting the lone flist entry's name to
        // the operand basename and pointing dest_dir at the parent. The
        // sandbox open below still anchors at the parent directory, so
        // SEC-1.{e..s} symlink-race defences continue to apply at the
        // same dirfd they always did.
        let dest_dir = self.apply_single_file_rename(dest_dir, file_count, trailing_slash);

        // upstream: main.c:778-792 get_local_name() - pre-flight mkdir of the
        // destination root when the transfer is multi-file or the operand
        // carries a trailing slash. The local-mode receiver creates the root
        // implicitly via the file-list-driven mkdir, but `--server` mode
        // never did, breaking the alt-dest interop test that uses a
        // non-existent destination over remote shell.
        //
        // This site is reachable from every `--server` receiver entry:
        // `run_sync`, `run_pipelined`, and `run_pipelined_incremental` all
        // route through `setup_transfer` before per-entry dispatch, so the
        // pre-flight runs uniformly under `--copy-dest`, `--link-dest`, and
        // `--compare-dest` over remote shell. The mkdir is receiver-local;
        // no `MSG_*` frame is emitted on the wire, matching upstream's
        // `get_local_name()` which calls `do_mkdir()` directly against the
        // local filesystem.
        // upstream: main.c:1383-1388 - `get_local_name()`, and with it the
        // pre-flight mkdir at main.c:778-792, lives inside the non-empty-list
        // arm of client_run(). A client handed an empty list must not create
        // the destination directory: `rsync host:/missing /new/` leaves
        // `/new/` absent. `do_server_recv()` calls `get_local_name()`
        // unconditionally (main.c:1212-1213), so the gate is client-side only.
        let created_dest_root = if self.is_empty_client_flist(file_count) {
            false
        } else {
            ensure_dest_root_exists(
                &dest_dir,
                file_count,
                trailing_slash,
                self.config.flags.skip_dest_writes(),
                self.config.flags.mkpath,
            )
            .map_err(|e| {
                io::Error::new(
                    e.kind(),
                    format!(
                        "failed to create destination root {}: {e} {}{}",
                        dest_dir.display(),
                        crate::role_trailer::error_location!(),
                        crate::role_trailer::receiver()
                    ),
                )
            })?
        };
        // upstream: main.c:794-796 - record whether the pre-flight mkdir
        // created the dest root so the root entry's itemize row can OR in
        // ITEM_IS_NEW (cd+++++++++ ./) only when it was actually created.
        self.dest_root_created = created_dest_root;
        if created_dest_root {
            debug_log!(Recv, 1, "created destination root {}", dest_dir.display());
        }
        // The `created directory <dest>` notice is emitted by
        // `announce_created_directory` from `setup_transfer`, where the
        // `MSG_INFO`-capable writer is in scope. Deferring it there lets a
        // server-mode (SSH/daemon) receiver forward the notice to the pushing
        // client over the multiplex stream instead of corrupting that stream
        // with a raw `println!`. See that method for the upstream citation and
        // the `INFO_GTE(NAME, 1) || stdout_format_has_i` gate.

        // UTS-SLDB: when the dest root is a symlink that resolved to a real
        // directory via the stat path in ensure_dest_root_exists, lock the
        // canonical target in here so every downstream open (DirSandbox,
        // per-entry `*at` syscalls) operates on the resolved directory.
        // Upstream `main.c:757` reaches the same state by calling
        // `change_dir(dest_path, CD_NORMAL)` after `S_ISDIR` succeeds: the
        // kernel resolves the link once and every subsequent syscall is
        // relative to the resolved cwd. We mirror that by canonicalizing
        // here instead of relying on chdir.
        //
        // Skipped under daemon connections: the daemon strict path in
        // `open_sandbox_for_dest_strict` refuses a symlinked dest outright
        // (chdir-symlink-race defense), and the module loader has already
        // restricted `module.path`. Canonicalizing here would mask the
        // symlink and let strict mode silently succeed against the resolved
        // target. Local-mode and non-daemon SSH transfers are the
        // upstream-parity case (issue #715 `symlink-dirlink-basis`).
        let dest_dir = if !self.config.flags.skip_dest_writes()
            && !self.config.connection.is_daemon_connection
            && dest_dir
                .symlink_metadata()
                .is_ok_and(|m| m.file_type().is_symlink())
        {
            match std::fs::canonicalize(&dest_dir) {
                Ok(resolved) => {
                    debug_log!(
                        Recv,
                        2,
                        "resolved symlinked destination root {} -> {}",
                        dest_dir.display(),
                        resolved.display()
                    );
                    resolved
                }
                Err(err) => {
                    debug_log!(
                        Recv,
                        1,
                        "canonicalize({}) failed: {err}; keeping link path",
                        dest_dir.display()
                    );
                    dest_dir
                }
            }
        } else {
            dest_dir
        };

        let acl_cache = if self.config.flags.acls {
            self.flist_reader_cache
                .as_ref()
                .map(|r| Arc::new(r.acl_cache().clone()))
        } else {
            None
        };

        // upstream: uidlist.c:483-484 match_acl_ids() - build the cross-host id
        // remapper for named ACL entries from the received uid/gid id-lists plus
        // `--usermap`/`--groupmap`, snapshotted so it can ride the Arc onto the
        // disk-commit thread that applies cached ACLs.
        let acl_id_map = if self.config.flags.acls {
            Some(Arc::new(self.build_acl_id_mapper()))
        } else {
            None
        };

        // SEC-1.e: open the destination root as a sandboxed dirfd carrier.
        // The carrier rides through every per-entry operation so the
        // SEC-1.f-j cutover sites can replace path-based syscalls with
        // their `*at` siblings without re-walking the path through the
        // kernel. This PR only threads the carrier; no syscalls are
        // migrated, so a failed open is non-fatal (a brand-new
        // destination root may not exist yet, and the existing
        // path-based fall-backs cover that case today).
        //
        // Daemon receivers without chroot tighten this: a leaf-symlink at
        // the destination is the chdir-symlink-race attack window, so we
        // refuse the transfer outright when DirSandbox open fails with
        // ELOOP/ENOTDIR instead of falling through to path-based syscalls
        // that would follow the symlink. ENOENT (first-run push that has
        // not created the directory yet) and EACCES (real permission
        // problems) keep the existing soft-fail behaviour.
        // upstream: clientserver.c:1018 - `use_secure_symlinks = am_daemon
        // && !am_chrooted` gates the do_*_at wrappers in syscall.c that
        // implement the same refusal.
        #[cfg(unix)]
        let sandbox = {
            let strict = self.config.connection.is_daemon_connection;
            open_sandbox_for_dest_strict(&dest_dir, strict)?
        };

        // FSM: file list received and sanitized. Advance to DeltaTransfer.
        self.pipeline
            .advance_to(TransferPhase::DeltaTransfer)
            .map_err(crate::fsm_error)?;

        Ok((
            file_count,
            PipelineSetup {
                dest_dir,
                metadata_opts,
                checksum_length,
                checksum_algorithm,
                acl_cache,
                acl_id_map,
                #[cfg(unix)]
                sandbox,
            },
        ))
    }

    /// Combines the received wire filter rules with any daemon filter rules and
    /// compiles them into the receiver's per-directory [`FilterChain`].
    ///
    /// This is the reader-free tail of the filter-list read: it takes the
    /// already-decoded `wire_rules` (read by [`read_filter_list`]) and produces
    /// the `filter_chain`.
    ///
    /// upstream: clientserver.c:rsync_module() - daemon_filter_list is applied
    /// on top of client filters. Daemon rules take precedence (prepended).
    fn apply_received_filter_rules(
        &mut self,
        wire_rules: Vec<FilterRuleWireFormat>,
    ) -> io::Result<()> {
        let daemon_rules = &self.config.daemon_filter_rules;
        let combined = if daemon_rules.is_empty() {
            wire_rules
        } else if wire_rules.is_empty() {
            daemon_rules.clone()
        } else {
            let mut combined = daemon_rules.clone();
            combined.extend(wire_rules);
            combined
        };

        // Compile the combined rules into the receiver's filter chains.
        // upstream: generator.c:delete_in_dir() - is_excluded() before deletion
        if !combined.is_empty() {
            self.compile_receiver_filter_chains(&combined)?;
        }
        Ok(())
    }

    /// Populates the client-receiver's filter chains from its own CLI
    /// `--filter`/`--exclude`/`--include` rules on a local-client pull, where
    /// the wire filter list is never received
    /// (`should_read_filter_list()` is false in client mode).
    ///
    /// A server-receiver reads its rules off the wire and compiles them in
    /// [`apply_received_filter_rules`](Self::apply_received_filter_rules); the
    /// client-receiver has no wire list and keeps its rules in
    /// `config.connection.filter_rules`. Both funnel through the same
    /// [`compile_receiver_filter_chains`](Self::compile_receiver_filter_chains)
    /// so the receiver holds an identical `filter_chain` +
    /// `deletion_filter_chain` regardless of transfer role.
    pub(in crate::receiver) fn populate_client_filter_chains(&mut self) -> io::Result<()> {
        let rules = self.config.connection.filter_rules.clone();
        self.compile_receiver_filter_chains(&rules)
    }

    /// Compiles the receiver's own filter rules into both the transfer-facing
    /// `filter_chain` (consulted by the `--prune-empty-dirs` pass) and the
    /// dedicated `deletion_filter_chain` (consulted by the `--delete` pass, with
    /// `--delete-excluded` folded in).
    ///
    /// This is the single population path shared by a server-receiver (whose
    /// `rules` are the wire list its client transmitted, prepended with any
    /// daemon rules) and a local-client pull (whose `rules` are its own CLI
    /// filter rules). On either side these are the same single rule list, so
    /// keeping one compilation site means the client's `-f`/`--exclude`/
    /// `--include` reach every receiver decision point exactly as a server's
    /// wire rules do - including `--prune-empty-dirs`, which reads
    /// `filter_chain` and previously saw an empty chain in client mode.
    ///
    /// # Upstream Reference
    ///
    /// - `exclude.c:recv_filter_list()` parses every rule into the single
    ///   `filter_list` that both `generator.c:delete_in_dir()` (the `--delete`
    ///   pass) and `flist.c:3142` (`is_excluded()` in the `--prune-empty-dirs`
    ///   pass) consult. The client parses its argv rules into that same list at
    ///   startup (`exclude.c:parse_filter_str`), so its own rules reach both
    ///   decision points unchanged. `deletion_filter_chain` is cloned from
    ///   `filter_chain` and folds in `--delete-excluded` (exclude.c:1685) so the
    ///   per-directory merge reload in `delete_extraneous_files` keeps the same
    ///   global rules and dir-merge configs while `--prune-empty-dirs` stays
    ///   unperturbed.
    fn compile_receiver_filter_chains(&mut self, rules: &[FilterRuleWireFormat]) -> io::Result<()> {
        let (filter_set, merge_configs) = parse_wire_filters_for_receiver(rules).map_err(|e| {
            io::Error::new(
                e.kind(),
                format!(
                    "filter error: {e} {}{}",
                    crate::role_trailer::error_location!(),
                    crate::role_trailer::receiver()
                ),
            )
        })?;
        let mut chain = FilterChain::new(filter_set);
        for config in merge_configs {
            chain.add_merge_config(config);
        }
        self.filter_chain = chain;
        self.deletion_filter_chain = self
            .filter_chain
            .clone()
            .with_delete_excluded(self.config.deletion.delete_excluded);
        logging::debug_log!(
            Del,
            2,
            "receiver filter chains built: delete_excluded={} merge_configs_active={}",
            self.config.deletion.delete_excluded,
            self.deletion_filter_chain.has_per_dir_merge()
        );
        Ok(())
    }

    /// Applies upstream's `get_local_name()` single-file rename semantics.
    ///
    /// When the transfer carries exactly one non-directory flist entry, the
    /// operand has no trailing slash, and the destination does not already
    /// name a directory, upstream's `get_local_name()` returns the operand
    /// basename as the receiver's `local_name`. `recv_files()` then writes
    /// the lone payload to that basename under `change_dir(parent)`,
    /// instead of treating the operand as a destination directory and
    /// joining the flist entry's name.
    ///
    /// oc-rsync does not chdir per connection; instead this helper rewrites
    /// the single flist entry's name in place to match the operand
    /// basename and points `dest_dir` at the operand's parent. Every
    /// downstream `dest_dir.join(entry.path())` then resolves to the
    /// operand path the client requested. Behaviour is unchanged when:
    ///
    /// - `file_count != 1` (multi-file transfer goes into a directory).
    /// - `trailing_slash` (caller asked for directory semantics).
    /// - The lone entry is a directory.
    /// - The operand already exists as a directory (treat as directory).
    /// - The operand has no parent component (`legit.txt`, dest stays `.`).
    ///
    /// # Security
    ///
    /// Pointing `dest_dir` at the parent of a daemon module-resolved
    /// operand keeps every SEC-1.{e..s} guard intact: the sandbox open
    /// below anchors at the new `dest_dir` (which is still under the
    /// module path), per-entry `openat2` opens still refuse symlinks at
    /// the leaf, and the operand basename is a single path component so
    /// it cannot traverse out of the sandbox. The bare-do-open symlink-
    /// race attack scenarios continue to be rejected because they target
    /// path operations against an attacker-planted parent symlink
    /// (`cd -> /outside`): that symlink lives under the module root and
    /// the sandbox open of the new `dest_dir` still resolves through it
    /// under `RESOLVE_NO_SYMLINKS`.
    ///
    /// # Upstream Reference
    ///
    /// - `main.c:805-832` - `get_local_name()` rename branch
    /// - `receiver.c:706` - `fname = local_name ? local_name : f_name(...)`
    fn apply_single_file_rename(
        &mut self,
        dest_dir: PathBuf,
        file_count: usize,
        trailing_slash: bool,
    ) -> PathBuf {
        use std::path::Path;

        if file_count != 1 || trailing_slash {
            return dest_dir;
        }
        if self.config.flags.skip_dest_writes() {
            // Dry-run and list-only never touch disk, so the directory-vs-file
            // ambiguity does not change observable output. Keep behaviour stable.
            return dest_dir;
        }
        // An existing directory at the operand keeps the directory branch
        // (mirrors upstream's `S_ISDIR(st.st_mode)` path in
        // `get_local_name()`). `metadata()` follows symlinks, matching
        // upstream's `do_stat()`.
        if dest_dir.metadata().is_ok_and(|m| m.is_dir()) {
            return dest_dir;
        }
        let entry_is_dir = self.file_list.first().is_some_and(|e| e.is_dir());
        if entry_is_dir {
            return dest_dir;
        }
        let Some(target_basename) = dest_dir.file_name().map(std::ffi::OsString::from) else {
            return dest_dir;
        };
        // Skip the rewrite when the dest operand is just a bare name with
        // no parent component (e.g. dest = "legit.txt" relative to cwd).
        // `Path::parent()` returns `Some("")` for that shape, which the
        // join chain treats as `cwd`.
        let parent = match dest_dir.parent() {
            Some(p) if !p.as_os_str().is_empty() => Some(p.to_path_buf()),
            _ => None,
        };
        // Belt-and-suspenders: never let the rewritten basename escape its
        // parent. `file_name()` already strips separators, but a defensive
        // single-component check makes the invariant explicit alongside
        // the SEC-1 sandbox guard.
        let basename_path = Path::new(&target_basename);
        if basename_path.components().count() != 1 {
            return dest_dir;
        }
        if let Some(entry) = self.file_list.first_mut() {
            entry.set_name(PathBuf::from(&target_basename));
        }
        parent.unwrap_or_else(|| PathBuf::from("."))
    }

    /// Builds the cross-host ACL id remapper from the received id-lists.
    ///
    /// Snapshots the resolved remote->local uid/gid maps and, on Unix, the
    /// parsed `--usermap`/`--groupmap` rules so named ACL entries are remapped
    /// exactly like file owners.
    ///
    /// upstream: uidlist.c:483-484 + acls.c:1059-1081 - `match_acl_ids()`.
    #[cfg(unix)]
    pub(in crate::receiver) fn build_acl_id_mapper(&self) -> metadata::AclIdMapper {
        metadata::AclIdMapper::new(
            self.uid_list.resolved_map(),
            self.gid_list.resolved_map(),
            self.config.user_mapping.clone(),
            self.config.group_mapping.clone(),
            self.config.flags.numeric_ids.maps_numeric(),
        )
    }

    /// Builds the cross-host ACL id remapper (non-Unix: no `--usermap`).
    #[cfg(not(unix))]
    pub(in crate::receiver) fn build_acl_id_mapper(&self) -> metadata::AclIdMapper {
        metadata::AclIdMapper::new(
            self.uid_list.resolved_map(),
            self.gid_list.resolved_map(),
            self.config.flags.numeric_ids.maps_numeric(),
        )
    }

    /// Forwards a server-receiver-side `--files-from=<localpath>` file to the
    /// sender (peer) over the protocol writer.
    ///
    /// Upstream's `main.c:1191-1198` server-receiver opens `filesfrom_fd`
    /// locally and registers it with `start_filesfrom_forwarding`. The I/O
    /// scheduler then interleaves writes to `f_out` (toward the sender) with
    /// reads from `f_in` (the incoming flist). The sender's `send_file_list`
    /// reads its `filesfrom_fd = f_in` to discover filenames.
    ///
    /// This is only triggered when:
    /// - we are server-side (`!client_mode`), and
    /// - `files_from_path` is set to a real local path (not `-`, which means
    ///   the *client* is forwarding stdin into us).
    ///
    /// Without this push the upstream client (sender) blocks forever on
    /// `recv_files_from`, causing the upstream testsuite `files-from` 4th
    /// invocation to hang at "building file list ...".
    ///
    /// # Upstream Reference
    ///
    /// - `main.c:1191-1198` - `start_filesfrom_forwarding(filesfrom_fd)`
    /// - `io.c:370-381` - `forward_filesfrom_data()` core loop
    /// - `options.c:2944-2956` - server-side `--files-from <path>` arg form
    fn forward_files_from_to_sender<W: io::Write + crate::writer::MsgInfoSender + ?Sized>(
        &self,
        writer: &mut W,
    ) -> io::Result<()> {
        if self.config.connection.client_mode {
            return Ok(());
        }
        let path = match &self.config.file_selection.files_from_path {
            Some(path) if path != "-" => path,
            _ => return Ok(()),
        };

        // upstream: options.c:2501 open(files_from, O_RDONLY|O_BINARY).
        let file = std::fs::File::open(path).map_err(|err| {
            io::Error::new(
                err.kind(),
                format!(
                    "failed to open files-from file '{path}': {err} {}{}",
                    crate::role_trailer::error_location!(),
                    crate::role_trailer::receiver()
                ),
            )
        })?;
        let mut reader = io::BufReader::new(file);

        // upstream: io.c:370 forward_filesfrom_data() preserves --from0
        // semantics for already-NUL-delimited inputs. Use the same gating
        // here so a `--from0 --files-from /path` push round-trips cleanly.
        //
        // Stage into an in-memory buffer first because `writer: &mut W` is
        // unsized when W is `?Sized` and `protocol::forward_files_from`
        // requires a sized writer. The buffered approach also matches
        // upstream's `iobuf.out` enqueue model: the receiver hands the
        // whole filesfrom payload to the outgoing socket buffer, and the
        // kernel drains it while `recv_file_list` reads back from f_in.
        let from0 = self.config.file_selection.from0;
        let mut staged = Vec::with_capacity(4096);
        protocol::forward_files_from(&mut reader, &mut staged, from0, None)?;

        // upstream: io.c:1228 start_filesfrom_forwarding - below protocol 31 the
        // names are forwarded un-multiplexed (MPLX_TO_BUFFERED), so on a
        // multiplexed server stream we bypass MSG_DATA framing to match the
        // wire a real upstream sender expects; at protocol >= 31 they stay
        // framed like any other multiplexed write.
        if self.protocol.forwards_files_from_unmultiplexed() && writer.is_output_multiplexed() {
            writer.write_files_from_unframed(&staged)?;
        } else {
            writer.write_all(&staged)?;
        }
        writer.flush()?;

        debug_log!(
            Flist,
            1,
            "forwarded local --files-from '{path}' to peer sender"
        );

        Ok(())
    }
}

#[cfg(all(test, unix))]
mod merge_chmod_tests {
    use super::merge_chmod;
    use metadata::ChmodModifiers;

    /// A regular-file `FileType`, needed because `ChmodModifiers::apply` selects
    /// the `F` (files-only) clauses by file type on Unix.
    fn regular_file_type() -> std::fs::FileType {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("f");
        std::fs::write(&path, b"x").expect("write");
        std::fs::metadata(&path).expect("stat").file_type()
    }

    /// On a pull `daemon_incoming_chmod` is always None, so only the client
    /// `--chmod` survives the merge - the exact case the remote pull receivers
    /// rely on to force the destination mode.
    #[test]
    fn client_only_survives() {
        let client = ChmodModifiers::parse("F640").expect("parse");
        let merged = merge_chmod(None, Some(client)).expect("some");
        assert_eq!(merged.apply(0o600, regular_file_type()) & 0o777, 0o640);
    }

    /// With no chmod on either side the merge yields None so the receiver leaves
    /// the transferred mode untouched.
    #[test]
    fn both_none_is_none() {
        assert!(merge_chmod(None, None).is_none());
    }

    /// When both sources are present the daemon modes apply first and the client
    /// modes second, mirroring upstream's single `chmod_modes` list
    /// (clientserver.c:1217 prepends the daemon modes). The client's `F640` OR
    /// clause runs last, so it wins over the daemon's earlier `Fg-r` AND.
    #[test]
    fn daemon_and_client_both_apply_in_order() {
        let daemon = ChmodModifiers::parse("Fg-r").expect("parse");
        let client = ChmodModifiers::parse("F640").expect("parse");
        let merged = merge_chmod(Some(daemon), Some(client)).expect("some");
        assert_eq!(merged.apply(0o600, regular_file_type()) & 0o777, 0o640);
    }
}

/// A remote push creates the destination root on the server-mode receiver, and
/// upstream reports it with `created directory <dest>` via `rprintf(FINFO, ...)`
/// (main.c:807-808). These tests pin that the server receiver frames the notice
/// as `MSG_INFO` (so the pushing client renders it) exactly when upstream's
/// `INFO_GTE(NAME, 1) || stdout_format_has_i` gate is satisfied, and stays
/// silent otherwise.
#[cfg(test)]
mod created_directory_notice_tests {
    use std::io;
    use std::path::Path;

    use logging::{InfoFlag, VerbosityConfig};
    use protocol::ProtocolVersion;

    use crate::config::ServerConfig;
    use crate::flags::ParsedServerFlags;
    use crate::handshake::HandshakeResult;
    use crate::receiver::ReceiverContext;
    use crate::role::ServerRole;

    /// Captures the `MSG_INFO` frames a server-mode receiver emits, standing in
    /// for the pushing client's multiplex reader.
    #[derive(Default)]
    struct CaptureWriter {
        info: Vec<String>,
    }

    impl io::Write for CaptureWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl crate::writer::MsgInfoSender for CaptureWriter {
        fn send_msg_info(&mut self, data: &[u8]) -> io::Result<()> {
            self.info.push(String::from_utf8_lossy(data).into_owned());
            Ok(())
        }
    }

    fn handshake() -> HandshakeResult {
        HandshakeResult {
            protocol: ProtocolVersion::try_from(32u8).unwrap(),
            buffered: Vec::new(),
            compat_exchanged: false,
            client_args: None,
            io_timeout: None,
            negotiated_algorithms: None,
            compat_flags: None,
            checksum_seed: 0,
        }
    }

    /// Builds a server-mode (push) receiver whose pre-flight mkdir created the
    /// destination root, with an optional `%i`-bearing out-format.
    fn server_ctx(created: bool, out_format_forwards_i: bool) -> ReceiverContext {
        let mut config = ServerConfig {
            role: ServerRole::Receiver,
            protocol: ProtocolVersion::try_from(32u8).unwrap(),
            flags: ParsedServerFlags::default(),
            ..Default::default()
        };
        config.flags.info_flags.out_format_forwards_i = out_format_forwards_i;
        // Server-mode receiver: the notice must route through MSG_INFO, not the
        // shared stdout stream.
        config.connection.client_mode = false;

        let hs = handshake();
        let mut ctx = ReceiverContext::new_for_test(&hs, config);
        ctx.dest_root_created = created;
        ctx
    }

    fn run(ctx: &ReceiverContext, dest: &str) -> Vec<String> {
        let mut writer = CaptureWriter::default();
        ctx.announce_created_directory(&mut writer, Path::new(dest))
            .expect("announce");
        writer.info
    }

    /// upstream: main.c:807-808 - `-v` raises `INFO_NAME` to 1, so a push that
    /// created the dest root reports `created directory <dest>` as a `MSG_INFO`
    /// frame. The trailing slash of the `dest/` operand is lopped
    /// (main.c:788-789), so `dst/` renders as `dst`.
    #[test]
    fn push_dash_v_reports_created_directory_via_msg_info() {
        logging::init(VerbosityConfig::from_verbose_level(1));
        let ctx = server_ctx(true, false);
        assert_eq!(
            run(&ctx, "dst/"),
            vec!["created directory dst\n".to_string()]
        );
    }

    /// upstream: main.c:807-808 - without `-v` and without `%i` in the
    /// out-format, `INFO_GTE(NAME, 1) || stdout_format_has_i` is false, so the
    /// notice is suppressed even though the dest root was created.
    #[test]
    fn push_without_name_or_itemize_stays_silent() {
        logging::init(VerbosityConfig::from_verbose_level(0));
        let ctx = server_ctx(true, false);
        assert!(run(&ctx, "dst/").is_empty());
    }

    /// upstream: main.c:807-808 - the `stdout_format_has_i` half of the OR fires
    /// the notice under a `%i`-bearing out-format even without `-v`.
    #[test]
    fn push_with_itemize_reports_even_without_verbose() {
        logging::init(VerbosityConfig::from_verbose_level(0));
        let ctx = server_ctx(true, true);
        assert_eq!(
            run(&ctx, "dst"),
            vec!["created directory dst\n".to_string()]
        );
    }

    /// upstream: main.c:807-808 - `--info=name0` drops `INFO_NAME` to 0 even
    /// under `-v`, suppressing the notice, matching the local-copy gate.
    #[test]
    fn push_info_name0_suppresses_notice_under_verbose() {
        let mut cfg = VerbosityConfig::from_verbose_level(1);
        cfg.info.set(InfoFlag::Name, 0);
        logging::init(cfg);
        let ctx = server_ctx(true, false);
        assert!(run(&ctx, "dst/").is_empty());
    }

    /// When the dest root already existed, upstream never reaches the mkdir
    /// branch, so no notice is emitted regardless of verbosity.
    #[test]
    fn preexisting_dest_root_emits_nothing() {
        logging::init(VerbosityConfig::from_verbose_level(1));
        let ctx = server_ctx(false, false);
        assert!(run(&ctx, "dst/").is_empty());
    }
}

/// A server-mode receiver forwarding a local `--files-from` list to the sender
/// must reproduce upstream's `start_filesfrom_forwarding` framing decision: the
/// names ride the socket un-multiplexed (raw) below protocol 31 and MSG_DATA
/// framed at protocol >= 31.
///
/// upstream: io.c:1230 `if (protocol_version < 31 && OUT_MULTIPLEXED)` switches
/// the output stream to `MPLX_TO_BUFFERED` for the forwarding window.
#[cfg(test)]
mod files_from_forwarding_framing_tests {
    use std::io::{self, Write};
    use std::sync::{Arc, Mutex};

    use protocol::ProtocolVersion;

    use crate::config::ServerConfig;
    use crate::flags::ParsedServerFlags;
    use crate::handshake::HandshakeResult;
    use crate::receiver::ReceiverContext;
    use crate::role::ServerRole;
    use crate::writer::ServerWriter;

    /// `MSG_DATA` tag byte: `MPLEX_BASE (7) + MSG_DATA (0)` in the header's high
    /// byte. A framed payload of length `L` is `[L, L>>8, L>>16, 7] ++ payload`.
    const MSG_DATA_TAG: u8 = 7;

    /// A capturing `Write` sink shared with the caller so the test can inspect
    /// the exact wire bytes the multiplexed writer emitted.
    #[derive(Clone, Default)]
    struct SharedSink(Arc<Mutex<Vec<u8>>>);

    impl Write for SharedSink {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl SharedSink {
        fn bytes(&self) -> Vec<u8> {
            self.0.lock().unwrap().clone()
        }
    }

    fn handshake(proto: u8) -> HandshakeResult {
        HandshakeResult {
            protocol: ProtocolVersion::try_from(proto).unwrap(),
            buffered: Vec::new(),
            compat_exchanged: false,
            client_args: None,
            io_timeout: None,
            negotiated_algorithms: None,
            compat_flags: None,
            checksum_seed: 0,
        }
    }

    /// Builds a server-mode receiver whose local `--files-from` points at
    /// `path`, then forwards it through a freshly multiplexed [`ServerWriter`],
    /// returning the captured wire bytes.
    fn forward(proto: u8, path: &str) -> Vec<u8> {
        let mut config = ServerConfig {
            role: ServerRole::Receiver,
            protocol: ProtocolVersion::try_from(proto).unwrap(),
            flags: ParsedServerFlags::default(),
            ..Default::default()
        };
        // Server-side receiver with a real local files-from path (not "-").
        config.connection.client_mode = false;
        config.file_selection.files_from_path = Some(path.to_owned());

        let hs = handshake(proto);
        let ctx = ReceiverContext::new_for_test(&hs, config);

        let sink = SharedSink::default();
        // Upstream multiplexes the server-receiver's output for protocol >= 23
        // (start_server -> io_start_multiplex_out); reproduce that here.
        let mut writer = ServerWriter::new_plain(sink.clone())
            .activate_multiplex()
            .expect("activate multiplex");
        ctx.forward_files_from_to_sender(&mut writer)
            .expect("forward files-from");
        sink.bytes()
    }

    /// Writes a two-name newline-delimited files-from list and returns the raw
    /// payload the forwarder stages (what upstream would place on the wire).
    fn write_list(dir: &std::path::Path) -> (String, Vec<u8>) {
        let path = dir.join("list");
        std::fs::write(&path, b"alpha\nbeta\n").expect("write list");
        let mut expected = Vec::new();
        let file = std::fs::File::open(&path).expect("open list");
        protocol::forward_files_from(&mut io::BufReader::new(file), &mut expected, false, None)
            .expect("stage payload");
        (path.to_string_lossy().into_owned(), expected)
    }

    /// upstream: io.c:1230 - at protocol 29 (< 31) the forwarded names are sent
    /// raw, with no `MSG_DATA` framing, so a real upstream sender's `read_line`
    /// consumes the bytes verbatim.
    #[test]
    fn proto29_forwards_names_raw() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (path, expected) = write_list(dir.path());
        let bytes = forward(29, &path);
        assert_eq!(
            bytes, expected,
            "protocol 29 must forward files-from names un-multiplexed (raw)"
        );
    }

    /// upstream: io.c:1230 - at protocol 30 (< 31) the stream is still switched
    /// to buffered, so the names remain raw even though the server output is
    /// multiplexed.
    #[test]
    fn proto30_forwards_names_raw() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (path, expected) = write_list(dir.path());
        let bytes = forward(30, &path);
        assert_eq!(
            bytes, expected,
            "protocol 30 must forward files-from names un-multiplexed (raw)"
        );
    }

    /// upstream: io.c:1230 - at protocol 31+ the stream stays multiplexed, so
    /// the forwarded names are wrapped in a single `MSG_DATA` frame.
    #[test]
    fn proto31_forwards_names_framed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (path, expected) = write_list(dir.path());
        let bytes = forward(31, &path);

        assert_ne!(bytes, expected, "protocol 31 must MSG_DATA-frame the names");
        assert_eq!(
            bytes.len(),
            expected.len() + 4,
            "expected one MSG_DATA header (4 bytes) plus the payload"
        );
        let len = u32::from(bytes[0]) | (u32::from(bytes[1]) << 8) | (u32::from(bytes[2]) << 16);
        assert_eq!(len as usize, expected.len(), "framed length mismatch");
        assert_eq!(bytes[3], MSG_DATA_TAG, "high byte must be the MSG_DATA tag");
        assert_eq!(&bytes[4..], expected.as_slice(), "framed payload mismatch");
    }
}
