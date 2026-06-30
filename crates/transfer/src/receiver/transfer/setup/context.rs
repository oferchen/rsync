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

use logging::{debug_log, info_log};
use metadata::MetadataOptions;
use protocol::filters::read_filter_list;

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

impl ReceiverContext {
    /// Common setup for all transfer modes.
    ///
    /// Activates input multiplex, reads filter list if needed, receives the file
    /// list (including INC_RECURSE extra segments), sanitizes paths, and builds
    /// the `PipelineSetup` with checksum and metadata configuration.
    ///
    /// # Upstream Reference
    ///
    /// - `main.c:1342-1343` - client receiver activates multiplex at protocol >= 23
    /// - `main.c:1167-1168` - server receiver activates multiplex at protocol >= 30
    pub(in crate::receiver) fn setup_transfer<R: Read, W: io::Write + ?Sized>(
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

            // upstream: clientserver.c:rsync_module() - daemon_filter_list is applied
            // on top of client filters. Daemon rules take precedence (prepended).
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

            // Build a FilterChain from the combined rules for deletion filtering.
            // upstream: generator.c:delete_in_dir() - is_excluded() before deletion
            if !combined.is_empty() {
                let (filter_set, merge_configs) = parse_wire_filters_for_receiver(&combined)
                    .map_err(|e| {
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
            }
        } else if self.config.connection.client_mode
            && !self.config.connection.filter_rules.is_empty()
        {
            // upstream: generator.c:delete_in_dir() -> change_local_filter_dir()
            // reloads each DESTINATION directory's per-directory merge files
            // before deciding deletions. On a local-client pull the wire filter
            // list is never received (should_read_filter_list() is false in
            // client mode), so the receiver's `--delete` pass would otherwise
            // run against an empty filter chain and mis-handle dir-merge-governed
            // trees (over-deleting self-protected `.filt`/`.filt2` merge files,
            // under-deleting extraneous entries in filter-hidden dirs). Build a
            // dedicated deletion chain from the same local CLI filter rules the
            // generator consumes (generator/filters.rs), so the per-directory
            // merge reload in `delete_extraneous_files` has the dir-merge
            // configs. Held separately from `filter_chain` so `--prune-empty-dirs`
            // is unaffected. Only the deletion pass consults this, so it is inert
            // when `--delete` is not active.
            let (filter_set, merge_configs) = parse_wire_filters_for_receiver(
                &self.config.connection.filter_rules,
            )
            .map_err(|e| {
                io::Error::new(
                    e.kind(),
                    format!(
                        "filter error: {e} {}{}",
                        crate::role_trailer::error_location!(),
                        crate::role_trailer::receiver()
                    ),
                )
            })?;
            let mut chain = FilterChain::new(filter_set)
                .with_delete_excluded(self.config.deletion.delete_excluded);
            for config in merge_configs {
                chain.add_merge_config(config);
            }
            logging::debug_log!(
                Del,
                2,
                "deletion filter chain built: delete_excluded={} merge_configs_active={}",
                self.config.deletion.delete_excluded,
                chain.has_per_dir_merge()
            );
            self.deletion_filter_chain = chain;
        }

        // FSM: filter list reading is complete. Advance to FileListTransfer.
        self.pipeline
            .advance_to(TransferPhase::FileListTransfer)
            .map_err(crate::fsm_error)?;

        // upstream: main.c:1173-1180 - server-receiver opened a local
        // `--files-from` file (filesfrom_fd) and now forwards its contents
        // to the sender (the client) over f_out so the sender can build the
        // file list. Upstream interleaves this with `recv_file_list` via the
        // I/O scheduler; we write the whole file out as a single push before
        // entering the flist read because oc-rsync's reader/writer streams
        // are decoupled (no select() loop fanning across them).
        self.forward_files_from_to_sender(writer)?;

        if self.config.flags.verbose && self.config.connection.client_mode {
            info_log!(Flist, 1, "receiving incremental file list");
        }

        let file_count = self.receive_file_list(&mut reader)?;

        let extra_count = self.receive_extra_file_lists(&mut reader)?;
        let file_count = file_count + extra_count;

        let removed = self.sanitize_file_list();
        let file_count = file_count - removed;

        let checksum_factory = ChecksumFactory::from_negotiation(
            self.negotiated_algorithms.as_ref(),
            self.protocol,
            self.checksum_seed,
            self.compat_flags.as_ref(),
        );
        let checksum_algorithm = checksum_factory.signature_algorithm();
        let checksum_length = PHASE1_CHECKSUM_LENGTH;

        let metadata_opts = MetadataOptions::new()
            .preserve_permissions(self.config.flags.perms)
            .preserve_times(self.config.flags.times)
            .preserve_atimes(self.config.flags.atimes)
            .preserve_crtimes(self.config.flags.crtimes)
            .preserve_owner(self.config.flags.owner)
            .preserve_group(self.config.flags.group)
            .numeric_ids(self.config.flags.numeric_ids)
            // upstream: generator.c:1344 - `link_stat(fname, &sx.st,
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
            // upstream: clientserver.c:rsync_module() + generator.c -
            // `daemon_chmod_modes` rewrites the destination mode at finalize
            // time. The parser ran at module-load and stored a parsed
            // `ChmodModifiers`; we hand it to MetadataOptions so the existing
            // chmod-application site in `apply_permissions_with_chmod`
            // performs the rewrite without a separate code path.
            .with_chmod(self.config.daemon_incoming_chmod.clone())
            // upstream: uidlist.c:recv_id_list() applies parsed --usermap /
            // --groupmap rules at file-list receive time on the receiver.
            // The daemon parsed the wire arg in `apply_long_form_args` and
            // stashed the typed mapping on `ServerConfig`; hand it to
            // `MetadataOptions` so `metadata::apply::ownership` consults it
            // when remapping uid/gid before chown. Without this the wildcard
            // spec `--groupmap=*:GID` from the client is silently dropped on
            // daemon uploads (upstream regression #829 / daemon-groupmap-wild).
            .with_user_mapping(self.config.user_mapping.clone())
            .with_group_mapping(self.config.group_mapping.clone());

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
        let created_dest_root = ensure_dest_root_exists(
            &dest_dir,
            file_count,
            trailing_slash,
            self.config.flags.skip_dest_writes(),
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
        })?;
        // upstream: main.c:794-796 - record whether the pre-flight mkdir
        // created the dest root so the root entry's itemize row can OR in
        // ITEM_IS_NEW (cd+++++++++ ./) only when it was actually created.
        self.dest_root_created = created_dest_root;
        if created_dest_root {
            debug_log!(Recv, 1, "created destination root {}", dest_dir.display());
            // upstream: main.c:798-799 - `rprintf(FINFO, "created directory %s\n", dest_path)`
            // gated on `INFO_GTE(NAME, 1) || stdout_format_has_i`. The notice
            // is for the receiver's destination root only; alt-basis dirs
            // (`--copy-dest`, `--link-dest`, `--compare-dest`) never produce
            // this message because upstream's get_local_name() only mkdir's
            // `dest_path`, not the ref_dirs.
            //
            // Restrict the println to client-mode runs: server-mode receivers
            // (SSH/daemon) share stdout with the rsync multiplex stream, so a
            // raw `println!` would inject "created directory ..." bytes into
            // the wire protocol and corrupt the transfer (this is the
            // alt-dest interop regression). The client-side
            // `cli::frontend::progress::render` path already emits this
            // notice for local-mode transfers via the local-copy summary, so
            // gating here on client_mode keeps the upstream itemize.test
            // golden satisfied without breaking SSH/daemon paths.
            if self.config.flags.info_flags.itemize && self.config.connection.client_mode {
                println!("created directory {}", dest_dir.display());
            }
        }

        // UTS-SLDB: when the dest root is a symlink that resolved to a real
        // directory via the stat path in ensure_dest_root_exists, lock the
        // canonical target in here so every downstream open (DirSandbox,
        // per-entry `*at` syscalls) operates on the resolved directory.
        // Upstream `main.c:748` reaches the same state by calling
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
            reader,
            file_count,
            PipelineSetup {
                dest_dir,
                metadata_opts,
                checksum_length,
                checksum_algorithm,
                acl_cache,
                #[cfg(unix)]
                sandbox,
            },
        ))
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
    /// - `receiver.c:594` - `fname = local_name ? local_name : f_name(...)`
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

    /// Forwards a server-receiver-side `--files-from=<localpath>` file to the
    /// sender (peer) over the protocol writer.
    ///
    /// Upstream's `main.c:1173-1180` server-receiver opens `filesfrom_fd`
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
    /// - `main.c:1173-1180` - `start_filesfrom_forwarding(filesfrom_fd)`
    /// - `io.c:370-381` - `forward_filesfrom_data()` core loop
    /// - `options.c:2944-2956` - server-side `--files-from <path>` arg form
    fn forward_files_from_to_sender<W: io::Write + ?Sized>(
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

        // upstream: options.c:2483 open(files_from, O_RDONLY|O_BINARY).
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
        writer.write_all(&staged)?;
        writer.flush()?;

        debug_log!(
            Flist,
            1,
            "forwarded local --files-from '{path}' to peer sender"
        );

        Ok(())
    }
}
