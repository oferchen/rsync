//! Itemize-changes and info-line emission for [`super::ReceiverContext`].
//!
//! Routes already-formatted info lines (itemize, skip notices) to the correct
//! sink and renders per-entry itemize output.

use super::ReceiverContext;

impl ReceiverContext {
    /// Returns whether itemize-changes output should be produced for this
    /// transfer.
    ///
    /// Emitted whenever the user requested `--itemize-changes` (`-i`). Only a
    /// client receiver (a pull, where oc is the generator/receiver on the
    /// client) writes the row to its own stdout (see [`Self::emit_itemize`]); a
    /// server receiver (the remote end of a push) produces no client-visible
    /// row, since upstream `log.c:822` gates the `FCLIENT` write on `!am_server`
    /// and the push row is printed by the client's sender instead.
    #[must_use]
    pub(in crate::receiver) const fn should_emit_itemize(&self) -> bool {
        self.config.flags.info_flags.itemize
    }

    /// Classifies the received file list into the per-type counts the pulling
    /// client needs to reconstruct the `--stats` "Number of files" breakdown.
    ///
    /// Returns `(dirs, symlinks, devices, specials)`; the regular-file count is
    /// the remainder (`files_listed - sum`). Mirrors upstream's per-type tally
    /// as the file list is received, so a remote pull reports the same
    /// `reg: R, dir: D, link: L` line as a local copy.
    ///
    /// # Upstream Reference
    ///
    /// - `flist.c:2699-2712` - `recv_file_list()` bumps `stats.num_dirs` /
    ///   `num_symlinks` / `num_devices` / `num_specials` per entry.
    /// - `main.c:387-411` - `output_itemized_counts()` derives `reg` as the
    ///   total minus the other four categories.
    pub(in crate::receiver) fn file_type_counts(&self) -> (u64, u64, u64, u64) {
        let mut dirs = 0u64;
        let mut symlinks = 0u64;
        let mut devices = 0u64;
        let mut specials = 0u64;
        for entry in &self.file_list {
            if entry.is_dir() {
                dirs += 1;
            } else if entry.is_symlink() {
                symlinks += 1;
            } else if entry.is_device() {
                devices += 1;
            } else if entry.is_special() {
                specials += 1;
            }
        }
        (dirs, symlinks, devices, specials)
    }

    /// Sum of source sizes counted toward the `--stats` "total size".
    ///
    /// Only regular files and symlinks contribute, never directories, devices,
    /// or FIFOs - directory `st_size` in particular would inflate the total.
    ///
    /// # Upstream Reference
    ///
    /// - `flist.c:690-691` / `flist.c:1242-1243` - `stats.total_size +=
    ///   F_LENGTH(file)` guarded by `S_ISREG(mode) || S_ISLNK(mode)`.
    pub(in crate::receiver) fn total_source_size(&self) -> u64 {
        self.file_list
            .iter()
            .filter(|entry| {
                matches!(
                    entry.file_type(),
                    protocol::flist::FileType::Regular | protocol::flist::FileType::Symlink
                )
            })
            .map(|entry| entry.size())
            .sum()
    }

    /// Computes the itemize flags for an existing (already-present) directory
    /// by comparing its current on-disk metadata against the sender's file-list
    /// entry, mirroring upstream `generator.c:1480-1483` -> `itemize()` for the
    /// `statret == 0` directory branch.
    ///
    /// Upstream's `itemize()` sets `ITEM_REPORT_TIME` for a directory whenever
    /// `mtime_differs(&sxp->st, file)` (`generator.c:526-530`, `keep_time` true
    /// for a dir under `--times`), independent of `--checksum`: the transfer
    /// root `.` therefore emits `.d..t......` when the destination directory
    /// mtime differs from the source. The previous receiver behaviour passed
    /// `iflags == 0` for every existing directory, so this row was never
    /// produced on a remote pull.
    ///
    /// Must be called BEFORE the directory's metadata is (re)applied so the
    /// stat reflects the pre-transfer state upstream's `itemize()` compares
    /// against. Returns raw flags (0 when nothing differs or the stat fails,
    /// matching upstream's benign "no change" outcome).
    pub(in crate::receiver) fn existing_dir_iflags(
        &self,
        entry: &protocol::flist::FileEntry,
        dir_path: &std::path::Path,
    ) -> u32 {
        match std::fs::metadata(dir_path) {
            Ok(meta) => self.itemize_existing_flags(entry, &meta, 0),
            Err(_) => 0,
        }
    }

    /// Whether upstream would print the transfer root's plain-`-v` NAME line
    /// (`./`) for this run.
    ///
    /// Upstream emits a directory's `-v` name only when `set_file_attrs()`
    /// changed it (`generator.c:1503-1505`). For the implied root `.` that is
    /// true when oc created the destination root (`FLAG_DIR_CREATED`,
    /// `main.c:794-796`) or when the root's pre-transfer attributes differ from
    /// the source entry. Must be consulted BEFORE `create_directories` applies
    /// the root's metadata (and before child mkdirs bump the root mtime), so the
    /// stat reflects the pre-transfer state - the same pre-mkdir gate the `-i`
    /// root row uses (see `existing_dir_iflags`).
    pub(in crate::receiver) fn root_verbose_name_emit(&self, dest_dir: &std::path::Path) -> bool {
        if self.dest_root_created {
            return true;
        }
        self.file_list
            .iter()
            .find(|entry| entry.is_dir() && entry.path().as_os_str() == ".")
            .is_some_and(|entry| {
                crate::generator::ItemFlags::from_raw(self.existing_dir_iflags(entry, dest_dir))
                    .has_significant_flags()
            })
    }

    /// Builds the plain-`-v` directory NAME lines (bare `path/`, no newline)
    /// keyed by flist index, gated exactly as upstream gates the directory name
    /// output at `generator.c:1503-1505`: a directory prints only when
    /// `set_file_attrs()` changed it - i.e. it was newly created or its
    /// pre-transfer attributes differ from the source entry.
    ///
    /// Must be called BEFORE `create_directories` applies the directories'
    /// metadata (and before child mkdirs bump a parent's mtime), so each stat
    /// reflects the pre-transfer state - the same pre-mkdir gate the `-i` rows
    /// use (see [`Self::existing_dir_iflags`]). A destination directory that is
    /// absent pre-transfer (a fresh transfer, `--list-only`, or a dry run) is
    /// treated as newly created and always named, preserving the prior output
    /// for those cases; only an unchanged re-sync now correctly stays silent.
    pub(in crate::receiver) fn verbose_dir_name_lines(
        &self,
        dest_dir: &std::path::Path,
    ) -> Vec<(usize, String)> {
        self.file_list
            .iter()
            .enumerate()
            .filter_map(|(idx, entry)| {
                if !entry.is_dir() {
                    return None;
                }
                let rel = entry.path();
                if rel.as_os_str() == "." {
                    return self
                        .root_verbose_name_emit(dest_dir)
                        .then(|| (idx, "./".to_string()));
                }
                let dir_path = dest_dir.join(rel);
                let emit = match std::fs::symlink_metadata(&dir_path) {
                    // upstream: itemize() compares an existing dir's pre-apply
                    // stat and prints its name only when an attribute differs.
                    Ok(meta) if meta.is_dir() => crate::generator::ItemFlags::from_raw(
                        self.itemize_existing_flags(entry, &meta, 0),
                    )
                    .has_significant_flags(),
                    // Present but not a directory: upstream deletes it and
                    // creates the dir (ITEM_IS_NEW), so it is named.
                    Ok(_) => true,
                    // Absent pre-transfer: the dir will be created (named), or
                    // this is a list-only/dry-run with no destination tree.
                    Err(_) => true,
                };
                emit.then(|| (idx, format!("{}/", rel.display())))
            })
            .collect()
    }

    /// Records one newly created entry (destination absent before the transfer)
    /// against the receiver's per-type created tally, classifying it by the
    /// entry's Unix mode bits.
    ///
    /// Call once per `ITEM_IS_NEW` entry - a new directory, symlink, device,
    /// FIFO, or regular file - regardless of whether any file data moved and
    /// regardless of `--itemize-changes` visibility. Upstream counts these in
    /// the receiver independent of the `-i` gate, so the `--stats` breakdown is
    /// correct even without itemize output. The tally is folded into the
    /// returned `TransferStats` and never crosses the wire.
    ///
    /// # Upstream Reference
    ///
    /// - `receiver.c:733-746` - `stats.created_files++` plus the per-mode
    ///   `created_dirs`/`created_symlinks`/`created_devices`/`created_specials`
    ///   cascade under the `iflags & ITEM_IS_NEW` guard.
    pub(in crate::receiver) fn record_created(&self, mode: u32) {
        let mut stats = self.created_stats.get();
        stats.record(mode);
        self.created_stats.set(stats);
    }

    /// Routes an already-formatted info line (itemize, skip notice) to the
    /// correct sink: a server receiver multiplexes it as `MSG_INFO`; a client
    /// receiver (pull) writes it directly to stdout.
    ///
    /// upstream: log.c:rwrite() - `am_server` -> `MSG_INFO`, else write to the
    /// client output fd.
    pub(in crate::receiver) fn emit_info_line<W: crate::writer::MsgInfoSender + ?Sized>(
        &self,
        writer: &mut W,
        line: &str,
    ) -> std::io::Result<()> {
        if self.config.connection.client_mode {
            use std::io::Write as _;
            std::io::stdout().write_all(line.as_bytes())
        } else {
            writer.send_msg_info(line.as_bytes())
        }
    }

    /// Builds the display context for itemize time-position rendering.
    ///
    /// # Upstream Reference
    ///
    /// - `log.c:708-710` - symlink time: `T` when `!preserve_mtimes || !receiver_symlink_times`
    /// - `log.c:716-717` - non-symlink time: `T` when `!preserve_mtimes`
    fn itemize_context(&self) -> crate::generator::itemize::ItemizeContext {
        crate::generator::itemize::ItemizeContext {
            preserve_mtimes: self.config.flags.times,
            receiver_symlink_times: self
                .compat_flags
                .is_some_and(|f| f.contains(protocol::CompatibilityFlags::SYMLINK_TIMES)),
        }
    }

    /// Renders the itemize line for a file entry, or `None` when the entry is
    /// completely unchanged and the itemize level does not request unchanged
    /// rows.
    ///
    /// Pure formatting: applies the created-root-directory override and the
    /// significance gate, then formats via `format_itemize_line`. Routing to a
    /// sink (stdout vs MSG_INFO vs suppression) is the caller's concern - see
    /// [`Self::emit_itemize`].
    ///
    /// # Upstream Reference
    ///
    /// - `generator.c:574-576` - `iflags & (SIGNIFICANT_ITEM_FLAGS|ITEM_REPORT_XATTR)`
    /// - `main.c:794-796` - `FLAG_DIR_CREATED` for a pre-flight-mkdir'd root
    /// - `log.c:707-710` - direction glyph selection
    pub(in crate::receiver) fn render_itemize_line(
        &self,
        iflags: &crate::generator::ItemFlags,
        entry: &protocol::flist::FileEntry,
    ) -> Option<String> {
        // upstream: main.c:794-796 - when the receiver pre-flight-mkdirs the
        // destination root, `flist->files[0]->flags |= FLAG_DIR_CREATED`. The
        // generator's `itemize()` then sees `statret < 0` for the root entry,
        // ORs in `ITEM_IS_NEW`, and emits `cd+++++++++ ./`. oc-rsync's
        // `create_directory_incremental` cannot observe the
        // `ensure_dest_root_exists` mkdir after the fact, so the root entry
        // arrives here with `iflags == 0`. Force the created-directory glyph
        // ONLY when the pre-flight mkdir actually created the root this run
        // (`dest_root_created`); when the dest root already existed (e.g.
        // `up1/ -> up2/`), `FLAG_DIR_CREATED` is clear upstream and the root
        // reports a metadata-only row that the significance gate drops.
        let is_created_root_dir =
            self.dest_root_created && entry.is_dir() && entry.path().as_os_str() == ".";
        // upstream: generator.c:575-576 - emit when significant flags are set OR
        // the itemize level requests unchanged rows (`-ii` / `--info=name2` /
        // `-vv`). Without one of those, an all-unchanged entry produces no line.
        let show_unchanged =
            self.config.flags.info_flags.itemize_unchanged || self.config.flags.verbose_level > 1;
        if !is_created_root_dir && !show_unchanged && !iflags.has_significant_flags() {
            return None;
        }
        let effective_iflags = if is_created_root_dir {
            // upstream: generator.c:1468-1471 + generator.c:566-572 - a root that
            // the pre-flight mkdir created this run is itemize()'d with
            // `statret < 0`, which takes the `else` branch and ORs
            // `ITEM_LOCAL_CHANGE | ITEM_IS_NEW` WITHOUT computing any attribute
            // diff. Force the same bits here so a created root always renders the
            // full `cd+++++++++` glyph, even though `existing_dir_iflags` (which
            // classifies the already-mkdir'd root as "existing") may have
            // computed a spurious `ITEM_REPORT_TIME` against the fresh dir mtime.
            crate::generator::ItemFlags::from_raw(
                crate::generator::ItemFlags::ITEM_LOCAL_CHANGE
                    | crate::generator::ItemFlags::ITEM_IS_NEW,
            )
        } else {
            *iflags
        };
        let ctx = self.itemize_context();
        // upstream: log.c:707-710 - the direction glyph is `<` when
        // `!local_server && *op == 's'` (this side's peer is the sender's
        // client) and `>` otherwise. `op` is `am_sender ? "send" : "recv"`
        // (log.c:820), and the client-visible itemize is always produced by the
        // non-`am_server` side. A server-mode receiver is only ever the remote
        // end of a push (the client is the sender), so it renders `<`; a
        // client-mode receiver is a local pull and renders `>`.
        let is_sender = !self.config.connection.client_mode;
        // The receiver-side generator computes iflags locally rather than reading
        // them off the wire, so no alternate-basis xname is available here; the
        // hard-link ` => leader` suffix is owned by the push sender renderer
        // (generator/transfer/transfer_loop.rs::maybe_emit_itemize).
        Some(crate::generator::itemize::format_itemize_line(
            &effective_iflags,
            entry,
            is_sender,
            &ctx,
            None,
        ))
    }

    /// Emits itemize output for a file entry to the client-visible sink.
    ///
    /// A client-mode receiver (a pull, where oc is the generator/receiver on the
    /// client) writes the row to its own stdout. A server-mode receiver (the
    /// remote end of a push) writes NOTHING: upstream `log.c:822` gates the
    /// `FCLIENT` write on `!am_server`, so the remote receiver's generator never
    /// prints the client-visible row. Instead it writes the iflags over the wire
    /// (`generator.c:583-599 write_shortint(sock_f_out, iflags)`) and the
    /// client's SENDER prints them (`sender.c:461 log_item(FCLIENT)`). Forwarding
    /// a pre-rendered MSG_INFO row from here would double every pushed file
    /// against the client sender's own row.
    ///
    /// # Upstream Reference
    ///
    /// - `log.c:818-826` - `log_item()` only writes `FCLIENT` when `!am_server`
    /// - `generator.c:583-599` - the generator forwards iflags over the wire
    /// - `sender.c:461` - the sender prints the push itemize row
    pub(in crate::receiver) fn emit_itemize<W: crate::writer::MsgInfoSender + ?Sized>(
        &self,
        writer: &mut W,
        iflags: &crate::generator::ItemFlags,
        entry: &protocol::flist::FileEntry,
    ) -> std::io::Result<()> {
        if !self.should_emit_itemize() {
            return Ok(());
        }
        let Some(line) = self.render_itemize_line(iflags, entry) else {
            return Ok(());
        };
        if self.config.connection.client_mode {
            self.emit_info_line(writer, &line)
        } else {
            Ok(())
        }
    }

    /// Emits over-the-wire itemize records for every hardlink follower so a
    /// pushing client's sender renders the `hf...` / `=> leader` row.
    ///
    /// A hardlink follower is never sent for transfer, so it produces no
    /// `NDX + iflags` request in the per-file loop. Upstream instead itemizes
    /// each follower from `finish_hard_link()` -> `maybe_hard_link()`, which
    /// writes `NDX + write_shortint(iflags) + write_vstring(xname)` to
    /// `sock_f_out`; the peer's sender reads those attrs and logs the row
    /// (`sender.c:287` `maybe_log_item`). Without this a server-mode receiver
    /// (the remote end of a push) drops every follower row, because
    /// [`emit_itemize`](Self::emit_itemize) is a no-op off the client.
    ///
    /// This is the server-only counterpart of the client-mode follower rows that
    /// `create_hardlinks` renders locally, so a pull is left untouched. The
    /// `iflags` mirror `maybe_hard_link()`'s `ITEM_LOCAL_CHANGE |
    /// ITEM_XNAME_FOLLOWS`, plus `ITEM_IS_NEW` for the new-follower case, and the
    /// xname carries the leader's transfer-relative name the peer renders after
    /// `=>`.
    ///
    /// Writes go through `ndx_codec`, which MUST be the same NDX diff-state used
    /// for this phase's file requests so the delta encoding stays in sync with
    /// the peer's read state.
    ///
    /// # Upstream Reference
    ///
    /// - `generator.c:585-591` - `itemize()` writes `NDX`,
    ///   `write_shortint(iflags)`, then `write_vstring(xname)`.
    /// - `hlink.c:218-234` - `maybe_hard_link()` itemizes each follower with
    ///   `ITEM_LOCAL_CHANGE | ITEM_XNAME_FOLLOWS`, passing the leader realname.
    pub(in crate::receiver) fn emit_server_hardlink_follower_itemize<W>(
        &self,
        writer: &mut W,
        ndx_codec: &mut protocol::codec::NdxCodecEnum,
    ) -> std::io::Result<()>
    where
        W: std::io::Write + ?Sized,
    {
        use protocol::codec::NdxCodec;

        // Client-mode (pull) receivers render follower rows locally in
        // create_hardlinks; only a server-mode (push) receiver forwards them over
        // the wire. Pre-iflags protocols (< 29) carry no itemize attrs.
        if self.config.connection.client_mode
            || !self.config.flags.hard_links
            || !self.protocol.supports_iflags()
        {
            return Ok(());
        }

        // Leader group index -> transfer-relative name, so each follower can name
        // its leader in the xname the peer renders after "=>".
        let mut leader_names: std::collections::HashMap<u32, &str> =
            std::collections::HashMap::new();
        for entry in &self.file_list {
            if entry.hlink_first() {
                if let Some(gnum) = entry.hardlink_idx() {
                    leader_names.entry(gnum).or_insert_with(|| entry.name());
                }
            }
        }

        let mut emitted = 0usize;
        for (flat_idx, entry) in self.file_list.iter().enumerate() {
            if !entry.hlinked() || entry.hlink_first() {
                continue;
            }
            let Some(gnum) = entry.hardlink_idx() else {
                continue;
            };
            let Some(leader_name) = leader_names.get(&gnum).copied() else {
                continue;
            };

            let ndx = self.flat_to_wire_ndx(flat_idx);
            ndx_codec.write_ndx(writer, ndx)?;
            // upstream: generator.c:587 write_shortint(sock_f_out, iflags)
            let iflags = (crate::generator::ItemFlags::ITEM_LOCAL_CHANGE
                | crate::generator::ItemFlags::ITEM_XNAME_FOLLOWS
                | crate::generator::ItemFlags::ITEM_IS_NEW) as u16;
            writer.write_all(&iflags.to_le_bytes())?;
            // upstream: generator.c:591 write_vstring(sock_f_out, xname, len)
            protocol::write_vstring(writer, leader_name.as_bytes())?;
            emitted += 1;
        }

        if emitted > 0 {
            // upstream: generator.c flushes each itemize via rwrite(); flush once
            // so the peer's sender sees the follower rows without waiting on the
            // create_hardlinks pass that follows.
            writer.flush()?;
            // The peer's sender echoes every non-transfer item back
            // (upstream sender.c:286-292). Record the count so the phase-done
            // read drains those echoes before expecting NDX_DONE - the pipeline
            // response loop is request-count driven and never reads them.
            self.hardlink_follower_echoes
                .set(self.hardlink_follower_echoes.get() + emitted);
        }
        Ok(())
    }

    /// Emits an itemize row immediately, or buffers it for the deferred
    /// flist-index-order flush when [`Self::defer_itemize`] is set.
    ///
    /// Callers on the deferred path (currently only `run_pipelined`) pass the
    /// entry's flist index so the buffered row can be re-ordered against rows
    /// recorded by other passes (directory creation, candidate selection). When
    /// deferral is off every other receive path emits at the call site exactly
    /// as before.
    pub(in crate::receiver) fn emit_or_record_itemize<W: crate::writer::MsgInfoSender + ?Sized>(
        &self,
        writer: &mut W,
        flist_idx: usize,
        iflags: &crate::generator::ItemFlags,
        entry: &protocol::flist::FileEntry,
    ) -> std::io::Result<()> {
        if self.defer_itemize {
            self.record_itemize(flist_idx, iflags, entry);
            Ok(())
        } else {
            self.emit_itemize(writer, iflags, entry)
        }
    }

    /// Buffers one itemize row under its flist index for the deferred
    /// flist-index-order flush.
    ///
    /// The visibility gate mirrors [`Self::emit_itemize`] exactly: only a
    /// client-mode receiver (a pull) produces a client-visible row. A server
    /// receiver's row travels as wire iflags and is printed by the client's
    /// sender (upstream `log.c:822` gates the `FCLIENT` write on `!am_server`),
    /// so recording one here would double it. Preserving this gate keeps the
    /// deferred flush byte-identical to the immediate emit - only the ordering
    /// changes.
    pub(in crate::receiver) fn record_itemize(
        &self,
        flist_idx: usize,
        iflags: &crate::generator::ItemFlags,
        entry: &protocol::flist::FileEntry,
    ) {
        if !self.should_emit_itemize() || !self.config.connection.client_mode {
            return;
        }
        if let Some(line) = self.render_itemize_line(iflags, entry) {
            self.itemize_rows
                .borrow_mut()
                .entry(flist_idx)
                .or_default()
                .push(line);
        }
    }

    /// Drains every buffered itemize row in ascending flist-index order,
    /// routing each through the client sink.
    ///
    /// Mirrors upstream's single flist-index-order walk in `generate_files`
    /// (`generator.c:2329-2344`), where each entry is itemized as it is reached
    /// so a directory row immediately precedes its children. Because only
    /// client-mode rows are ever recorded (see [`Self::record_itemize`]),
    /// [`Self::emit_info_line`] writes them to the client's stdout. Called once
    /// by the driver just before finalization.
    pub(in crate::receiver) fn flush_itemize_rows<W: crate::writer::MsgInfoSender + ?Sized>(
        &self,
        writer: &mut W,
    ) -> std::io::Result<()> {
        let rows = std::mem::take(&mut *self.itemize_rows.borrow_mut());
        for (_idx, lines) in rows {
            for line in lines {
                self.emit_info_line(writer, &line)?;
            }
        }
        Ok(())
    }

    /// Writes one `-v` name line to the client's own output stream, honouring
    /// `--msgs2stderr` (upstream FINFO routes to stderr when set, else stdout).
    ///
    /// Client-mode only: this is the local-pull equivalent of upstream
    /// `log_item(FCLIENT, ...)`. A server receiver's names travel as wire
    /// itemize and are printed by the peer's sender, so this is never called
    /// off the client.
    fn emit_name_line(&self, line: &str) -> std::io::Result<()> {
        use std::io::Write as _;
        if self.names_to_stderr {
            std::io::stderr().write_all(line.as_bytes())
        } else {
            std::io::stdout().write_all(line.as_bytes())
        }
    }

    /// Buffers a pre-transfer `-v` name (a directory reached in phase 1) under
    /// its flist index, to be released in order just before its first child is
    /// reached in the phase-2 transfer loop.
    pub(in crate::receiver) fn buffer_deferred_name(&self, flist_idx: usize, line: String) {
        self.name_rows
            .borrow_mut()
            .entry(flist_idx)
            .or_default()
            .push(line);
    }

    /// Records a transferred file's `-v` name at its flist index and flushes
    /// every buffered name up to and including that index, in ascending order,
    /// so any directories that precede the file print immediately before it -
    /// interleaved with `--progress`. Mirrors upstream `log_before_transfer`
    /// (`receiver.c:1008-1012`, name printed per file just before its data).
    pub(in crate::receiver) fn emit_name_in_order(
        &self,
        flist_idx: usize,
        line: String,
    ) -> std::io::Result<()> {
        self.name_rows
            .borrow_mut()
            .entry(flist_idx)
            .or_default()
            .push(line);
        self.flush_names_through(flist_idx)
    }

    /// Drains buffered `-v` name lines with a flist index `<= upto`, in
    /// ascending index order, writing each to the client stream. Also used on
    /// the `--progress` path to release directory names in order without
    /// emitting the file name (the progress renderer prints that).
    pub(in crate::receiver) fn flush_names_through(&self, upto: usize) -> std::io::Result<()> {
        let ready = take_names_through(&mut self.name_rows.borrow_mut(), upto);
        for line in ready {
            self.emit_name_line(&line)?;
        }
        Ok(())
    }

    /// Flushes any remaining buffered `-v` names (trailing directories with no
    /// following transferred child) in ascending flist-index order. Called once
    /// by the driver after the transfer loop, before finalization.
    pub(in crate::receiver) fn flush_names_all(&self) -> std::io::Result<()> {
        let all = take_names_through(&mut self.name_rows.borrow_mut(), usize::MAX);
        for line in all {
            self.emit_name_line(&line)?;
        }
        Ok(())
    }
}

/// Removes every buffered name line whose flist index is `<= upto` and returns
/// them flattened in ascending index order (and, within an index, in insertion
/// order). Entries with an index above `upto` stay buffered for a later flush.
///
/// This is the watermark that interleaves `-v` directory names with their
/// children: the transfer loop calls it with each transferred file's index, so
/// the directories that precede that file drain out immediately before it.
fn take_names_through(
    rows: &mut std::collections::BTreeMap<usize, Vec<String>>,
    upto: usize,
) -> Vec<String> {
    let keys: Vec<usize> = rows.range(..=upto).map(|(k, _)| *k).collect();
    keys.into_iter()
        .filter_map(|k| rows.remove(&k))
        .flatten()
        .collect()
}

#[cfg(test)]
mod name_reorder_tests {
    use super::take_names_through;
    use std::collections::BTreeMap;

    fn buf(pairs: &[(usize, &str)]) -> BTreeMap<usize, Vec<String>> {
        let mut m: BTreeMap<usize, Vec<String>> = BTreeMap::new();
        for (idx, line) in pairs {
            m.entry(*idx).or_default().push((*line).to_string());
        }
        m
    }

    #[test]
    fn drains_prefix_in_flist_order_and_retains_the_rest() {
        // Dirs buffered up front at their flist indices; files reached in order.
        let mut rows = buf(&[(0, "a/"), (2, "a/b/"), (5, "c/")]);

        // Reaching file at index 1 releases only dir 0 (a/), before the file.
        assert_eq!(take_names_through(&mut rows, 1), vec!["a/".to_string()]);
        // File at index 3 releases dir 2 (a/b/) - which precedes it - not dir 5.
        assert_eq!(take_names_through(&mut rows, 3), vec!["a/b/".to_string()]);
        // The trailing dir at index 5 flushes only at end (usize::MAX).
        assert!(take_names_through(&mut rows, 4).is_empty());
        assert_eq!(
            take_names_through(&mut rows, usize::MAX),
            vec!["c/".to_string()]
        );
        assert!(rows.is_empty());
    }

    #[test]
    fn same_index_preserves_insertion_order() {
        let mut rows = buf(&[(2, "first"), (2, "second"), (1, "dir/")]);
        // Ascending index, then insertion order within an index.
        assert_eq!(
            take_names_through(&mut rows, 2),
            vec![
                "dir/".to_string(),
                "first".to_string(),
                "second".to_string()
            ],
        );
    }
}
