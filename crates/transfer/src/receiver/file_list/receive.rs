//! Wire reception of the initial file list and INC_RECURSE sub-lists.
//!
//! These methods drive the receiver-side flist pipeline: read entries from
//! the wire, normalize hardlinks, run `sort_file_list()` to match the
//! sender's ordering, and (for INC_RECURSE) publish each segment into the
//! parallel-deterministic-delete pipeline.

use std::io::{self, Read};

use logging::debug_log;
use protocol::CompatibilityFlags;
use protocol::codec::{NDX_FLIST_EOF, NDX_FLIST_OFFSET, NdxCodec, create_ndx_codec};
use protocol::flist::{IncrementalFileListBuilder, sort_file_list};

use super::super::ReceiverContext;
use super::hardlinks::{match_hard_links, normalize_pre30_hardlinks};
use super::incremental::IncrementalFileListReceiver;

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

        // upstream: flist.c:2726-2727 - recv_id_list() called when !inc_recurse
        let inc_recurse = self
            .compat_flags
            .is_some_and(|f| f.contains(CompatibilityFlags::INC_RECURSE));
        if !inc_recurse {
            self.receive_id_lists(reader)?;
        }

        // upstream: flist.c:2738-2742 - read io_error flag for protocol < 30.
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

        // upstream: flist.c:1646 - send_file_entry() is called with flist->used
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

        // upstream: flist.c:2736 - flist_sort_and_clean() after recv_id_list()
        //
        // When `--iconv` is in effect, upstream sets `need_unsorted_flist = 1`
        // (options.c:2056) so the receiver's NDX-addressed `flist->files[]`
        // array stays in sender scan order; only a separate `flist->sorted[]`
        // pointer array is reordered. We do not maintain a parallel pointer
        // array, so we mirror upstream by skipping the in-place reorder when
        // an active (non-identity) iconv converter is configured. This keeps
        // `self.file_list` in wire (NDX) order so subsequent generator
        // requests resolve to the entry the sender meant.
        // upstream: flist.c:2496-2498 - "both sides keep an unsorted
        // file-list array because the names will differ on the sending and
        // receiving sides".
        let pre29 = self.protocol.as_u8() < 29;
        if !self.iconv_reorder_suppressed() {
            sort_file_list(&mut self.file_list, self.config.qsort, pre29);
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
        // upstream: flist.c:recv_file_entry() - reuse cached reader to preserve
        // compression state (prev_name, prev_mode, prev_uid, prev_gid).
        let mut flist_reader = self
            .flist_reader_cache
            .take()
            .unwrap_or_else(|| self.build_flist_reader());
        let mut total_extra = 0;

        loop {
            let ndx = ndx_codec.read_ndx(reader)?;

            if ndx == NDX_FLIST_EOF {
                debug_log!(Flist, 2, "received NDX_FLIST_EOF, file list complete");
                break;
            }

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

            let dir_ndx = NDX_FLIST_OFFSET - ndx;
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

            // upstream: flist.c:1646 - leader GNUM is readdir-order wire NDX,
            // assigned before sorting.
            if self.config.flags.hard_links {
                for (i, entry) in self.file_list[flat_start..].iter_mut().enumerate() {
                    if entry.hlink_first() {
                        entry.set_hardlink_idx((seg_ndx_start + i as i32) as u32);
                    }
                }
            }

            // upstream: flist.c:2155,2736 - both sides call flist_sort_and_clean()
            // independently. Unstable sort (true) is safe - entries have unique paths.
            // INC_RECURSE requires protocol >= 30, so pre29 is always false here.
            //
            // Iconv suppresses the in-place reorder for the same reason as the
            // initial flist: upstream's `need_unsorted_flist` keeps the
            // NDX-addressed array in scan order so the receiver can resolve
            // generator requests against the bytes the sender emitted.
            if !self.iconv_reorder_suppressed() {
                sort_file_list(&mut self.file_list[flat_start..], true, false);
            }
            match_hard_links(&mut self.file_list[flat_start..], &mut self.prior_hlinks);

            // Normalize pre-30 hardlinks in this segment.
            if self.protocol.as_u8() < 30 && self.config.flags.hard_links {
                normalize_pre30_hardlinks(&mut self.file_list[flat_start..]);
            }

            // upstream: flist.c:2931 - ndx_start = prev->ndx_start + prev->used + 1
            self.ndx_segments.push((flat_start, seg_ndx_start));

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
            total_extra += segment_count;
        }

        debug_log!(
            Flist,
            2,
            "received {} extra entries across all sub-lists",
            total_extra
        );
        Ok(total_extra)
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
    /// - `generator.c:272-347` `delete_in_dir()` - per-directory extras
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
