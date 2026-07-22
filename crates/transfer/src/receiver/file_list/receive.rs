//! Wire reception of the initial file list and INC_RECURSE sub-lists.
//!
//! These methods drive the receiver-side flist pipeline: read entries from
//! the wire, normalize hardlinks, run `sort_file_list()` to match the
//! sender's ordering, and (for INC_RECURSE) publish each segment into the
//! parallel-deterministic-delete pipeline.

use std::io::{self, Read};
use std::path::Path;

use logging::debug_log;
use protocol::CompatibilityFlags;
use protocol::codec::{NDX_FLIST_EOF, NDX_FLIST_OFFSET, NdxCodec, create_ndx_codec};
use protocol::flist::{FileEntry, IncrementalFileListBuilder, sort_and_clean_file_list};

use super::super::ReceiverContext;
use super::hardlinks::{match_hard_links, normalize_pre30_hardlinks};
use super::incremental::IncrementalFileListReceiver;
use super::prune::prune_empty_dirs_pass;

impl ReceiverContext {
    /// Receives the file list from the sender.
    ///
    /// The file list is sent by the client in the rsync wire format with
    /// path compression and conditional fields based on flags.
    ///
    /// If the sender transmits an I/O error marker (SAFE_FILE_LIST mode), this
    /// method propagates the error up to the caller for handling. The caller should
    /// decide whether to continue or abort based on the error severity and context.
    ///
    /// After the file list entries, this also consumes the UID/GID lists that follow
    /// (unless using incremental recursion). See upstream `recv_id_list()` in uidlist.c.
    pub fn receive_file_list<R: Read + ?Sized>(&mut self, reader: &mut R) -> io::Result<usize> {
        let mut flist_reader = self.build_flist_reader();

        // Set ndx_start so the reader can distinguish abbreviated vs
        // unabbreviated hardlink followers (leader in same vs previous segment).
        // upstream: flist.c:recv_file_entry() uses flist->ndx_start
        let &(_flat_start, initial_ndx_start) =
            self.ndx_segments.last().expect("initial segment exists");
        flist_reader.set_ndx_start(initial_ndx_start);

        let mut count = 0;
        let seg_start = self.file_list.len();

        // upstream: flist.c:recv_file_list() - reads entries until end marker.
        // Pass segment entries so abbreviated hardlink followers can look up
        // their leader and copy metadata + update compression state.
        while let Some(entry) =
            flist_reader.read_entry_with_flist(reader, &self.file_list[seg_start..])?
        {
            self.file_list.push(entry);
            count += 1;
        }

        // upstream: flist.c:757-759 recv_file_entry() - a filename that cannot
        // be strictly transcoded under --iconv sets io_error |= IOERR_GENERAL on
        // the receiver. Fold the reader's accumulated flag into the receiver's
        // so the transfer exits 23, unless --ignore-errors suppresses it.
        if !self.config.deletion.ignore_errors {
            self.flist_io_error |= flist_reader.io_error();
        }

        // upstream: flist.c:2761-2762 - recv_id_list() called when !inc_recurse
        let inc_recurse = self
            .compat_flags
            .is_some_and(|f| f.contains(CompatibilityFlags::INC_RECURSE));

        // upstream: flist.c:2695-2701 - every received directory is appended to
        // dir_flist as the read loop runs, so dir_flist->used counts all dirs in
        // this list. We track the equivalent count only under INC_RECURSE (the
        // sole mode that frames sub-list headers by dir_ndx) and take it before
        // the receiver-only prune pass so it matches the sender's numbering.
        if inc_recurse {
            self.dir_flist_used += count_directories(&self.file_list[seg_start..]);
        }

        // Without INC_RECURSE the whole list arrives in this single call, so the
        // file list is complete. With INC_RECURSE the sender streams per-directory
        // sub-lists afterwards, terminated by NDX_FLIST_EOF, so leave `flist_eof`
        // clear until that marker is read (in `receive_one_extra_segment`'s caller).
        // upstream: io.c:1750-1786 - `flist_eof` gates the on-demand fetch loop.
        self.flist_eof = !inc_recurse;

        if !inc_recurse {
            self.receive_id_lists(reader)?;
            // upstream: uidlist.c:483-494 recv_id_list() remaps the whole flist
            // from sender ids to local ids right after reading the name lists.
            self.remap_flist_ownership_from_id_lists();
        }

        // upstream: flist.c:2773-2777 - read io_error flag for protocol < 30.
        // The sender writes write_int(f, io_error) as a 4-byte LE integer after
        // the id lists. Protocol >= 30 uses MSG_IO_ERROR or SAFE_FILE_LIST instead.
        if self.protocol.uses_fixed_encoding() {
            let mut buf = [0u8; 4];
            reader.read_exact(&mut buf)?;
            let err = i32::from_le_bytes(buf);
            if err != 0 && !self.config.deletion.ignore_errors {
                self.flist_io_error |= err;
            }
        }

        // upstream: flist.c:1682 - send_file_entry() is called with flist->used
        // (readdir-order position) BEFORE flist_sort_and_clean(). Leader GNUM values
        // (F_HL_GNUM) are readdir-order wire NDXes. Replace the u32::MAX sentinel
        // with the actual readdir-order wire NDX so followers can find their leader
        // after sorting reorders entries.
        if self.config.flags.hard_links {
            let &(_flat_start, ndx_start) =
                self.ndx_segments.last().expect("initial segment exists");
            for (i, entry) in self.file_list.iter_mut().enumerate() {
                if entry.hlink_first() {
                    entry.set_hardlink_idx((ndx_start + i as i32) as u32);
                }
            }
        }

        // upstream: flist.c:2771 - flist_sort_and_clean() after recv_id_list()
        //
        // When `--iconv` is in effect, upstream sets `need_unsorted_flist = 1`
        // (options.c:2074) so the receiver's NDX-addressed `flist->files[]`
        // array stays in sender scan order; only a separate `flist->sorted[]`
        // pointer array is reordered. We do not maintain a parallel pointer
        // array, so we mirror upstream by skipping the in-place reorder when
        // an active (non-identity) iconv converter is configured. This keeps
        // `self.file_list` in wire (NDX) order so subsequent generator
        // requests resolve to the entry the sender meant.
        // upstream: flist.c:2496-2498 - "both sides keep an unsorted
        // file-list array because the names will differ on the sending and
        // receiving sides".
        // upstream: flist.c:3016 flist_sort_and_clean() runs sort THEN the
        // duplicate-clean pass (flist.c:3046-3082) on the receiver (am_sender
        // is false). A sender that emits the same normalized name twice
        // (redundant or hostile) must collapse to a single entry, keeping the
        // upstream survivor: a directory over a plain file of the same name
        // "because it might have contents in the list" (flist.c:3060), else the
        // first entry. We reuse the shared sort+dedup primitive so both sides
        // clean identically. `_clean` stats mirror the DEBUG_GTE(DUP) trace and
        // are not surfaced elsewhere.
        //
        // When `--iconv` is in effect, upstream sets `need_unsorted_flist = 1`
        // (options.c:2074) so the receiver's NDX-addressed `flist->files[]`
        // array stays in sender scan order; only a separate `flist->sorted[]`
        // pointer array is reordered and dedup-cleared. We do not maintain a
        // parallel pointer array, so we mirror upstream by skipping the in-place
        // reorder+dedup when an active (non-identity) iconv converter is
        // configured. This keeps `self.file_list` in wire (NDX) order so
        // subsequent generator requests resolve to the entry the sender meant.
        // upstream: flist.c:2496-2498 - "both sides keep an unsorted file-list
        // array because the names will differ on the sending and receiving
        // sides".
        let pre29 = self.protocol.as_u8() < 29;
        if !self.iconv_reorder_suppressed() {
            let list = std::mem::take(&mut self.file_list);
            // am_sender=false: the receiver always runs the duplicate-clean,
            // tombstoning dropped duplicates in place so NDX stays aligned with
            // the sender's full un-deduped array (flist.c:3031,3089).
            let (cleaned, _clean) =
                sort_and_clean_file_list(list, self.config.qsort, pre29, false, inc_recurse);
            self.file_list = cleaned;
        }

        // upstream: flist.c:recv_file_list() appends every directory to
        // `dir_flist` as it is read, so a later INC_RECURSE sub-list header's
        // `dir_ndx` resolves to that directory's full path (flist.c:2685). Record
        // the same mapping in wire `dir_ndx` order - sorted here (or sender scan
        // order under iconv) and taken before the receiver-only prune pass, so it
        // stays aligned with the sender's `dir_ndx` numbering over every shipped
        // directory. `receive_one_extra_segment()` uses it to reject a sub-list
        // entry that does not live under its declared parent.
        if inc_recurse {
            self.record_dir_flist_names(0);
        }

        // upstream: flist.c:3121-3184 - flist_sort_and_clean() runs the
        // `--prune-empty-dirs` pass after sorting and dedup, before the caller's
        // match_hard_links() in recv_file_list(). Only the receiver runs this
        // pass (am_sender is false); the sender ships every directory.
        if self.config.flags.prune_empty_dirs {
            prune_empty_dirs_pass(&mut self.file_list, &self.filter_chain);
        }

        match_hard_links(&mut self.file_list, &mut self.prior_hlinks);

        // For protocol < 30, normalize (dev, ino) pairs into hardlink_idx and
        // hlink_first flags so the rest of the code handles both protocol versions
        // uniformly. Must run after sort and after match_hard_links (which is a
        // no-op for pre-30 entries that lack hardlink_idx).
        // upstream: hlink.c:init_hard_links() builds the idev table from dev/ino
        if self.protocol.as_u8() < 30 && self.config.flags.hard_links {
            normalize_pre30_hardlinks(&mut self.file_list);
        }

        // upstream: flist.c:recv_file_entry() uses static variables that persist
        // across recv_file_list() calls - cache the reader to preserve that state.
        self.flist_reader_cache = Some(flist_reader);

        Ok(count)
    }

    /// Receives incremental file list segments sent after the initial file list.
    ///
    /// When INC_RECURSE is negotiated, the sender sends per-directory sub-lists
    /// framed by `NDX_FLIST_OFFSET - dir_ndx` values, terminated by `NDX_FLIST_EOF`.
    /// Each sub-list contains file entries sorted within that directory.
    ///
    /// Entries are appended to `self.file_list` in the order received, maintaining
    /// the sender's NDX ordering (NDX = index in the combined list).
    ///
    /// Returns the total number of entries received across all sub-lists.
    ///
    /// # Upstream Reference
    ///
    /// - `flist.c:recv_file_list()` - reads entries for each sub-list
    /// - `io.c:read_ndx_and_attrs()` - detects NDX_FLIST_OFFSET framing
    ///
    /// Retained as the batched drain for the wire-parity tests and future
    /// `--files-from` use; the drivers now fetch segments lazily via the
    /// on-demand primitives, so the non-test lib build has no caller.
    #[allow(dead_code)]
    pub(in crate::receiver) fn receive_extra_file_lists<R: Read + ?Sized>(
        &mut self,
        reader: &mut R,
    ) -> io::Result<usize> {
        let inc_recurse = self
            .compat_flags
            .is_some_and(|f| f.contains(CompatibilityFlags::INC_RECURSE));
        if !inc_recurse {
            return Ok(0);
        }

        let mut ndx_codec = create_ndx_codec(self.protocol.as_u8());
        let mut total_extra = 0;

        loop {
            let ndx = ndx_codec.read_ndx(reader)?;

            if ndx == NDX_FLIST_EOF {
                debug_log!(Flist, 2, "received NDX_FLIST_EOF, file list complete");
                self.flist_eof = true;
                break;
            }

            total_extra += self.receive_one_extra_segment(reader, ndx)?;
        }

        debug_log!(
            Flist,
            2,
            "received {} extra entries across all sub-lists",
            total_extra
        );
        Ok(total_extra)
    }

    /// Receives one INC_RECURSE sub-list segment framed by a `NDX_FLIST_OFFSET`
    /// marker whose raw wire value is `ndx`.
    ///
    /// This is the body of a single [`receive_extra_file_lists`] loop iteration,
    /// minus the `read_ndx` and the `NDX_FLIST_EOF` termination check. It is the
    /// shared segment-append primitive used both by the batched
    /// [`receive_extra_file_lists`] wrapper (kept for `--files-from` and the
    /// wire-parity tests) and by the lazy on-demand fetch in
    /// [`super::super::ReceiverContext::read_next_frame`]. Returns the number of
    /// entries appended for this segment.
    ///
    /// Validates the marker (`ndx <= NDX_FLIST_OFFSET`), decodes the segment
    /// entries with the cached [`protocol::flist::FileListReader`] (preserving
    /// compression state across sub-lists), assigns leader GNUM wire NDXes, sorts
    /// and hardlink-matches the segment slice, pushes the `(flat_start,
    /// ndx_start)` boundary, and publishes the segment to the delete pipeline.
    ///
    /// # Upstream Reference
    ///
    /// - `flist.c:recv_file_list()` - per-sub-list entry decode
    /// - `flist.c:2966` - `ndx_start = prev->ndx_start + prev->used + 1`
    pub(in crate::receiver) fn receive_one_extra_segment<R: Read + ?Sized>(
        &mut self,
        reader: &mut R,
        ndx: i32,
    ) -> io::Result<usize> {
        if ndx > NDX_FLIST_OFFSET {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "expected NDX_FLIST_OFFSET or NDX_FLIST_EOF, got {ndx} {}{}",
                    crate::role_trailer::error_location!(),
                    crate::role_trailer::receiver()
                ),
            ));
        }

        // `ndx <= NDX_FLIST_OFFSET` (checked above), so `dir_ndx = NDX_FLIST_OFFSET
        // - ndx` is always in `0..=2147483547` and cannot overflow or go negative.
        let dir_ndx = NDX_FLIST_OFFSET - ndx;
        self.validate_extra_segment_dir_ndx(dir_ndx)?;

        // upstream: flist.c:recv_file_entry() - reuse cached reader to preserve
        // compression state (prev_name, prev_mode, prev_uid, prev_gid).
        let mut flist_reader = self
            .flist_reader_cache
            .take()
            .unwrap_or_else(|| self.build_flist_reader());

        let flat_start = self.file_list.len();

        // Compute seg_ndx_start BEFORE reading entries so the reader can
        // distinguish abbreviated vs unabbreviated hardlink followers.
        // upstream: flist.c:recv_file_entry() uses flist->ndx_start
        let &(prev_flat_start, prev_ndx_start) =
            self.ndx_segments.last().expect("initial segment exists");
        let prev_used = (flat_start - prev_flat_start) as i32;
        let seg_ndx_start = prev_ndx_start + prev_used + 1;
        flist_reader.set_ndx_start(seg_ndx_start);

        let mut segment_count = 0;

        // Pass segment entries so abbreviated followers can resolve leaders.
        while let Some(entry) =
            flist_reader.read_entry_with_flist(reader, &self.file_list[flat_start..])?
        {
            self.file_list.push(entry);
            segment_count += 1;
        }

        // upstream: flist.c:2684-2695 - every entry in a sub-list must live under
        // the directory named by its header `dir_ndx`; a mismatch is an attempt
        // by a hostile sender to inject a path that escapes its declared parent,
        // which upstream aborts with `exit_cleanup(RERR_UNSUPPORTED)`. Validate
        // before any further processing and drop the offending entries so nothing
        // escapes into the transfer.
        self.validate_extra_segment_path_belongs(dir_ndx, flat_start)?;

        // upstream: flist.c:1646 - leader GNUM is readdir-order wire NDX,
        // assigned before sorting.
        if self.config.flags.hard_links {
            for (i, entry) in self.file_list[flat_start..].iter_mut().enumerate() {
                if entry.hlink_first() {
                    entry.set_hardlink_idx((seg_ndx_start + i as i32) as u32);
                }
            }
        }

        // upstream: flist.c:2190,2771 - both sides call flist_sort_and_clean()
        // on EACH sub-list independently (send_extra_file_list / recv_file_list).
        // That runs sort THEN the duplicate-clean pass (flist.c:3031, active for
        // the receiver because `!am_sender || inc_recurse`), so a sub-list that
        // repeats a normalized name collapses to a single entry with the upstream
        // tie-break (a directory over a same-named file, else the first). We reuse
        // the shared sort+dedup primitive so this matches the receiver's initial
        // pass and the sender's per-dir clean, keeping the NDX numbering of the
        // next segment identical on both sides. INC_RECURSE requires protocol >=
        // 30, so pre29 is always false here.
        //
        // Iconv suppresses the in-place reorder+dedup for the same reason as the
        // initial flist: upstream's `need_unsorted_flist` keeps the NDX-addressed
        // array in scan order so the receiver can resolve generator requests
        // against the bytes the sender emitted.
        if !self.iconv_reorder_suppressed() {
            let tail = self.file_list.split_off(flat_start);
            // Receiver sub-list clean: am_sender=false, inc_recurse=true.
            let (cleaned, _clean) = sort_and_clean_file_list(tail, true, false, false, true);
            self.file_list.extend(cleaned);
        }
        match_hard_links(&mut self.file_list[flat_start..], &mut self.prior_hlinks);

        // Normalize pre-30 hardlinks in this segment.
        if self.protocol.as_u8() < 30 && self.config.flags.hard_links {
            normalize_pre30_hardlinks(&mut self.file_list[flat_start..]);
        }

        // upstream: flist.c:2695-2701 - directories in this sub-list are appended
        // to dir_flist, so a later sub-list may legitimately reference them by
        // dir_ndx. Fold them into the running count and record their full paths in
        // the same wire dir_ndx order so a deeper sub-list's path-belongs check
        // resolves this segment's directories.
        self.dir_flist_used += count_directories(&self.file_list[flat_start..]);
        self.record_dir_flist_names(flat_start);

        // upstream: flist.c:2966 - ndx_start = prev->ndx_start + prev->used + 1
        self.ndx_segments.push((flat_start, seg_ndx_start));

        // Restore the cached reader so the next segment continues the same
        // compression state (upstream's static recv_file_entry() variables).
        self.flist_reader_cache = Some(flist_reader);

        // DDP-B3 (#2257): if a parallel-deterministic-delete context
        // is attached, publish a DeletePlan for this segment's
        // content directory into the shared DeletePlanMap. Failures
        // are logged + skipped; the legacy batched-sweep path
        // remains the active delete driver until the emitter wiring
        // lands (tasks DDP-E1-E5).
        self.publish_segment_to_delete_pipeline(dir_ndx, flat_start);

        debug_log!(
            Flist,
            2,
            "received sub-list for dir_ndx={}, {} entries (ndx_start={})",
            dir_ndx,
            segment_count,
            seg_ndx_start
        );
        Ok(segment_count)
    }

    /// Validates an INC_RECURSE sub-list header's `dir_ndx` against untrusted
    /// wire data, failing closed exactly like upstream, and claims the directory
    /// so a duplicate sub-list is rejected.
    ///
    /// Two guards, both mapped to `RERR_PROTOCOL` (2) via
    /// [`protocol::protocol_violation`]:
    ///
    /// 1. **Range** - a `dir_ndx` that references a directory not yet received
    ///    (`dir_ndx >= dir_flist_used`) cannot belong to any real sender tree and
    ///    is rejected. `dir_ndx` is always `>= 0` (see the caller), so the single
    ///    `>=` test also covers the negative case upstream checks separately.
    /// 2. **Duplicate** - a second sub-list for a directory already served is a
    ///    malicious duplicate that would otherwise grow `file_list` unbounded.
    ///
    /// Used by [`Self::receive_one_extra_segment`] to reject malformed segments
    /// with the exit codes upstream uses.
    ///
    /// # Upstream Reference
    ///
    /// - `flist.c:2622-2626` - `if (dir_ndx >= dir_flist->used) ... "refusing
    ///   invalid dir_ndx %u >= %u" ... exit_cleanup(RERR_PROTOCOL)`.
    /// - `flist.c:2627-2632` - `FLAG_GOT_DIR_FLIST` duplicate guard, "refusing
    ///   malicious duplicate flist for dir %d", `exit_cleanup(RERR_PROTOCOL)`.
    pub(in crate::receiver) fn validate_extra_segment_dir_ndx(
        &mut self,
        dir_ndx: i32,
    ) -> io::Result<()> {
        if dir_ndx < 0 || dir_ndx as usize >= self.dir_flist_used {
            return Err(protocol::protocol_violation(format!(
                "refusing invalid dir_ndx {dir_ndx} >= {} {}{}",
                self.dir_flist_used,
                crate::role_trailer::error_location!(),
                crate::role_trailer::receiver()
            )));
        }
        if !self.served_dir_flists.insert(dir_ndx) {
            return Err(protocol::protocol_violation(format!(
                "refusing malicious duplicate flist for dir {dir_ndx} {}{}",
                crate::role_trailer::error_location!(),
                crate::role_trailer::receiver()
            )));
        }
        Ok(())
    }

    /// Records the full relative path of every directory in `file_list[from..]`
    /// into [`dir_flist_names`](super::super::ReceiverContext::dir_flist_names),
    /// in the order they appear. Called after each list/sub-list is sorted (and,
    /// under non-iconv, deduped) so the vector index matches the wire `dir_ndx`
    /// the sender assigns to that directory.
    ///
    /// # Upstream Reference
    ///
    /// - `flist.c:2704` - `dir_flist->files[dir_flist->used++] = file` appends
    ///   each directory so `dir_flist->files[dir_ndx]` names it later.
    pub(in crate::receiver) fn record_dir_flist_names(&mut self, from: usize) {
        for entry in &self.file_list[from..] {
            if entry.is_dir() {
                self.dir_flist_names.push(entry.path().clone());
            }
        }
    }

    /// Validates that every entry in the just-read INC_RECURSE sub-list
    /// (`file_list[flat_start..]`) lives directly under the directory named by
    /// the header's `dir_ndx`. A hostile sender that frames a sub-list for a
    /// legitimate parent but fills it with an entry whose dirname escapes that
    /// parent (`../` or an unrelated tree) is rejected, mapping to
    /// `RERR_UNSUPPORTED` (4) exactly like upstream's `exit_cleanup`. The
    /// offending segment's entries are dropped so nothing escapes into the
    /// transfer.
    ///
    /// The parent name is looked up in [`dir_flist_names`] built by
    /// [`record_dir_flist_names`](Self::record_dir_flist_names). Leading slashes
    /// (present only under `--relative`, where upstream defers stripping to
    /// `flist_sort_and_clean(..., strip_root)`) are ignored on both sides so a
    /// legitimate relative transfer is never falsely rejected.
    ///
    /// # Upstream Reference
    ///
    /// - `flist.c:2684-2695` - `strcmp(cur_dir, d) != 0` -> "ABORTING due to
    ///   invalid path from sender" -> `exit_cleanup(RERR_UNSUPPORTED)`.
    pub(in crate::receiver) fn validate_extra_segment_path_belongs(
        &mut self,
        dir_ndx: i32,
        flat_start: usize,
    ) -> io::Result<()> {
        // The range check in validate_extra_segment_dir_ndx guarantees
        // `dir_ndx < dir_flist_used`; the name vector is kept in lockstep with
        // that count, so a present entry is expected. If it is somehow absent we
        // cannot name the parent, so skip rather than falsely abort.
        let Some(parent) = self.dir_flist_names.get(dir_ndx as usize) else {
            return Ok(());
        };
        let parent = strip_leading_slashes(parent);
        for i in flat_start..self.file_list.len() {
            let child_dir = strip_leading_slashes(self.file_list[i].dirname());
            if child_dir != parent {
                let basename = self.file_list[i].name().to_owned();
                let cur = self.file_list[i].dirname().display().to_string();
                // Drop this segment's entries so nothing escapes the tree.
                self.file_list.truncate(flat_start);
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    format!(
                        "ABORTING due to invalid path from sender: {cur}/{basename} {}{}",
                        crate::role_trailer::error_location!(),
                        crate::role_trailer::receiver()
                    ),
                ));
            }
        }
        Ok(())
    }

    /// Publishes one INC_RECURSE segment into the parallel-deterministic-
    /// delete pipeline, if a [`engine::delete::DeleteContext`] has been
    /// attached via [`super::super::ReceiverContext::set_delete_context`].
    ///
    /// `dir_ndx` is the wire NDX of the segment's parent directory (the
    /// content directory the segment describes). `flat_start` is the
    /// flat-array index where this segment's entries begin in
    /// `self.file_list`; the entries slice is
    /// `self.file_list[flat_start..]`.
    ///
    /// # Behaviour
    ///
    /// - When no context is attached, returns immediately.
    /// - Otherwise, resolves the parent directory's destination-relative
    ///   path via [`Self::wire_to_flat_ndx`] and
    ///   [`protocol::flist::FileEntry::path`], then forwards the segment
    ///   to [`engine::delete::DeleteContext::observe_segment_for_delete`].
    /// - I/O failures from `compute_extras` are logged at level 2 and
    ///   swallowed; the legacy batched-sweep path remains the
    ///   authoritative delete driver, so a transient read_dir error here
    ///   does not abort the transfer.
    ///
    /// # Upstream Reference
    ///
    /// - `generator.c:279-354` `delete_in_dir()` - per-directory extras
    ///   computation that this hook publishes a plan for.
    pub(in crate::receiver) fn publish_segment_to_delete_pipeline(
        &self,
        dir_ndx: i32,
        flat_start: usize,
    ) {
        let Some(ctx) = self.delete_ctx.as_ref() else {
            return;
        };
        let Some(parent_flat) = self.wire_to_flat_ndx(dir_ndx) else {
            debug_log!(
                Flist,
                2,
                "delete pipeline: dir_ndx={} did not resolve to a flat index; skipping segment publish",
                dir_ndx
            );
            return;
        };
        let Some(parent) = self.file_list.get(parent_flat) else {
            debug_log!(
                Flist,
                2,
                "delete pipeline: parent flat_idx={} out of range; skipping segment publish",
                parent_flat
            );
            return;
        };
        let dir = parent.path().to_path_buf();
        let entries = &self.file_list[flat_start..];
        if let Err(err) = ctx.observe_segment_for_delete(&dir, entries) {
            debug_log!(
                Flist,
                2,
                "delete pipeline: observe_segment_for_delete({}) failed: {}; legacy sweep will still run",
                dir.display(),
                err
            );
        }
    }

    /// Creates an incremental file list receiver for streaming processing.
    ///
    /// Instead of waiting for the complete file list before processing, this
    /// method returns an [`IncrementalFileListReceiver`] that yields entries
    /// as they arrive from the sender, with proper dependency tracking.
    ///
    /// # Benefits
    ///
    /// - Reduced startup latency: Transfers begin as soon as first entries arrive
    /// - Better memory efficiency: Don't need entire list in memory before starting
    /// - Improved progress feedback: Users see activity immediately
    ///
    /// # Dependency Tracking
    ///
    /// The incremental receiver tracks parent directory dependencies. Entries are
    /// only yielded when their parent directory has been processed, ensuring:
    ///
    /// 1. Directories are created before their contents
    /// 2. Nested directories are created in order
    /// 3. Files can be transferred immediately once their parent exists
    pub fn incremental_file_list_receiver<R: Read>(
        &self,
        reader: R,
    ) -> IncrementalFileListReceiver<R> {
        let flist_reader = self.build_flist_reader();

        // Build incremental processor with pre-existing destination directories
        let incremental = IncrementalFileListBuilder::new()
            .incremental_recursion(self.config.flags.incremental_recursion)
            .build();

        IncrementalFileListReceiver {
            flist_reader,
            source: reader,
            incremental,
            finished_reading: false,
            entries_read: 0,
            use_qsort: self.config.qsort,
            iconv_reorder_suppressed: self.iconv_reorder_suppressed(),
        }
    }
}

/// Counts the directory entries in a received file-list slice.
///
/// Mirrors upstream's `dir_flist` accounting: every `S_ISDIR` entry read in a
/// list is appended to `dir_flist`, so this count is what `dir_flist->used`
/// grows by for that list.
///
/// # Upstream Reference
///
/// - `flist.c:2695-2701` - `if (S_ISDIR(file->mode)) { ... dir_flist->files[
///   dir_flist->used++] = file; }`.
pub(super) fn count_directories(entries: &[FileEntry]) -> usize {
    entries.iter().filter(|e| e.is_dir()).count()
}

/// Strips leading path separators so a `--relative` dirname - which carries a
/// leading `/` until upstream's `strip_root` pass runs - compares equal to the
/// slash-free parent path recorded in `dir_flist_names`.
///
/// # Upstream Reference
///
/// - `flist.c:2685-2686` - `if (relative_paths && *cur_dir == '/') cur_dir++;`
fn strip_leading_slashes(p: &Path) -> &Path {
    let mut s = p;
    while let Ok(rest) = s.strip_prefix("/") {
        if rest.as_os_str().is_empty() {
            break;
        }
        s = rest;
    }
    s
}
