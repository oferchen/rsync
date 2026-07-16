//! Itemize-changes and info-line emission for [`super::ReceiverContext`].
//!
//! Extracted verbatim from the receiver hub. Routes already-formatted info
//! lines (itemize, skip notices) to the correct sink and renders per-entry
//! itemize output.

use super::ReceiverContext;

impl ReceiverContext {
    /// Returns whether itemize emission should be active.
    ///
    /// Whether itemize-changes output should be produced for this transfer.
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
        Some(crate::generator::itemize::format_itemize_line(
            &effective_iflags,
            entry,
            is_sender,
            &ctx,
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
            write_itemize_vstring(writer, leader_name.as_bytes())?;
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
}

/// Writes an itemize xname as an upstream `io.c:write_vstring()` length-prefixed
/// string: a single length byte for `len <= 0x7F`, otherwise a two-byte prefix
/// (`0x80 | len >> 8`, then `len & 0xFF`). Mirrors the reader in
/// `generator::item_flags::ItemFlags::read_trailing` so a peer's sender decodes
/// the exact bytes it emitted.
fn write_itemize_vstring<W: std::io::Write + ?Sized>(
    writer: &mut W,
    bytes: &[u8],
) -> std::io::Result<()> {
    let len = bytes.len();
    debug_assert!(len <= 0x7FFF, "itemize xname exceeds vstring capacity");
    if len > 0x7F {
        writer.write_all(&[((len >> 8) as u8) | 0x80, (len & 0xFF) as u8])?;
    } else {
        writer.write_all(&[len as u8])?;
    }
    if len > 0 {
        writer.write_all(bytes)?;
    }
    Ok(())
}
