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
        let effective_iflags = if is_created_root_dir && !iflags.has_significant_flags() {
            // upstream: generator.c:1468-1471 + generator.c:566-572 - itemize()
            // with `statret < 0` ORs `ITEM_LOCAL_CHANGE | ITEM_IS_NEW`. Apply
            // the same bits here so `format_itemize_line` emits the full
            // `cd+++++++++` glyph instead of `.d ...`.
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
}
