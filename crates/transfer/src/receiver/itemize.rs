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
    /// Emitted whenever the user requested `--itemize-changes` (`-i`). The
    /// destination of each line depends on the role (see [`Self::emit_info_line`]):
    /// a server receiver multiplexes it as a `MSG_INFO` frame to the client,
    /// while a client receiver (a pull, where oc is the generator) writes it to
    /// its own stdout - mirroring upstream `log.c:rwrite()` which sends
    /// `MSG_INFO` only when `am_server` and otherwise writes to the client fd.
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

    /// Emits a MSG_INFO frame with itemize output for a file entry.
    ///
    /// Formats the itemize string (`"%i %n%L\n"`) and sends it as a MSG_INFO
    /// multiplexed message. Uses `is_sender: false` since the daemon is receiving
    /// files (producing `>` direction indicator).
    ///
    /// Suppresses output when `iflags` has no significant flags set (the file is
    /// completely unchanged), matching upstream's gate in `itemize()` at
    /// `generator.c:574-576`.
    ///
    /// # Upstream Reference
    ///
    /// - `generator.c:574-576` - `iflags & (SIGNIFICANT_ITEM_FLAGS|ITEM_REPORT_XATTR)`
    /// - `generator.c:2260` - `itemize()` in receiver's generator context
    /// - `log.c:330-340` - `rwrite()` converts to `send_msg(MSG_INFO)` when `am_server`
    pub(in crate::receiver) fn emit_itemize<W: crate::writer::MsgInfoSender + ?Sized>(
        &self,
        writer: &mut W,
        iflags: &crate::generator::ItemFlags,
        entry: &protocol::flist::FileEntry,
    ) -> std::io::Result<()> {
        if !self.should_emit_itemize() {
            return Ok(());
        }
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
            return Ok(());
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
        let line =
            crate::generator::itemize::format_itemize_line(&effective_iflags, entry, false, &ctx);
        self.emit_info_line(writer, &line)
    }
}
