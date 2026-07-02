//! Async twins of the receiver's file-list entry reception, gated on the
//! `tokio-transfer` feature.
//!
//! These are the `.await`-driven counterparts to
//! [`ReceiverContext::receive_file_list`](super::super::ReceiverContext::receive_file_list)
//! and
//! [`ReceiverContext::receive_extra_file_lists`](super::super::ReceiverContext::receive_extra_file_lists).
//! They decode the file-list entry stream - the bulk of the receiver's wire read
//! on multi-file transfers - off a [`tokio::io::AsyncRead`] via the shared
//! sans-io decode core in `protocol` (`read_entry_with_flist_async`), then run
//! the *identical* post-processing the sync path runs (leader GNUM assignment,
//! `sort_file_list`, `--prune-empty-dirs`, `match_hard_links`, pre-30
//! normalization, reader-cache preservation, delete-pipeline publish).
//!
//! Because the entry decode and all post-processing are the same code the sync
//! path uses, these twins produce a byte-identical `file_list` for the same wire
//! bytes, independent of how the bytes are chunked across `.await` points.
//!
//! # Scope
//!
//! This rung covers the file-list *entry* loops only - the largest and always-
//! present wire read. The trailing UID/GID id-list and the protocol < 30
//! io-error integer that the sync `receive_file_list` reads after the entries
//! are a separate wire surface with their own future async twin; the async
//! entry readers here stop at the end-of-list marker, exactly like the entry
//! loop in the sync path, and leave those tail reads to the (not-yet-async)
//! id-list path. Additive and unwired: the receiver hot path is unchanged.

use std::io;

use logging::debug_log;
use protocol::CompatibilityFlags;
use protocol::codec::{NDX_FLIST_EOF, NDX_FLIST_OFFSET, create_ndx_codec};
use protocol::flist::{read_entry_with_flist_async, sort_file_list};
use tokio::io::AsyncRead;

use super::super::ReceiverContext;
use super::hardlinks::{match_hard_links, normalize_pre30_hardlinks};
use super::prune::prune_empty_dirs_pass;

impl ReceiverContext {
    /// Async twin of
    /// [`receive_file_list`](super::super::ReceiverContext::receive_file_list),
    /// reading the initial file-list entries off an [`AsyncRead`].
    ///
    /// Decodes entries via the shared async leaf until the end-of-list marker,
    /// then applies the identical post-processing (leader GNUM assignment,
    /// sort, prune, hardlink matching, pre-30 normalization, reader-cache
    /// preservation). Returns the number of entries received.
    ///
    /// Unlike the sync path, this does not read the trailing UID/GID id-lists or
    /// the protocol < 30 io-error integer (see the module scope note).
    pub async fn receive_file_list_async<R>(&mut self, src: &mut R) -> io::Result<usize>
    where
        R: AsyncRead + Unpin + ?Sized,
    {
        let mut flist_reader = self.build_flist_reader();

        let &(_flat_start, initial_ndx_start) =
            self.ndx_segments.last().expect("initial segment exists");
        flist_reader.set_ndx_start(initial_ndx_start);

        let mut count = 0;
        let seg_start = self.file_list.len();
        let mut carry: Vec<u8> = Vec::new();

        // upstream: flist.c:recv_file_list() - reads entries until end marker.
        loop {
            let entry = read_entry_with_flist_async(
                &mut flist_reader,
                src,
                &mut carry,
                &self.file_list[seg_start..],
            )
            .await?;
            let Some(entry) = entry else { break };
            self.file_list.push(entry);
            count += 1;
        }

        // upstream: flist.c:1646 - leader GNUM values are readdir-order wire
        // NDXes assigned before sorting.
        if self.config.flags.hard_links {
            let &(_flat_start, ndx_start) =
                self.ndx_segments.last().expect("initial segment exists");
            for (i, entry) in self.file_list.iter_mut().enumerate() {
                if entry.hlink_first() {
                    entry.set_hardlink_idx((ndx_start + i as i32) as u32);
                }
            }
        }

        // upstream: flist.c:2736 - flist_sort_and_clean() after recv_id_list().
        let pre29 = self.protocol.as_u8() < 29;
        if !self.iconv_reorder_suppressed() {
            sort_file_list(&mut self.file_list, self.config.qsort, pre29);
        }

        // upstream: flist.c:3121-3184 - `--prune-empty-dirs` pass after sorting.
        if self.config.flags.prune_empty_dirs {
            prune_empty_dirs_pass(&mut self.file_list, &self.filter_chain);
        }

        match_hard_links(&mut self.file_list, &mut self.prior_hlinks);

        if self.protocol.as_u8() < 30 && self.config.flags.hard_links {
            normalize_pre30_hardlinks(&mut self.file_list);
        }

        // upstream: flist.c:recv_file_entry() static variables persist across
        // recv_file_list() calls - cache the reader to preserve that state.
        self.flist_reader_cache = Some(flist_reader);

        Ok(count)
    }

    /// Async twin of
    /// [`receive_extra_file_lists`](super::super::ReceiverContext::receive_extra_file_lists),
    /// reading INC_RECURSE sub-list segments off an [`AsyncRead`].
    ///
    /// The NDX segment framing (`NDX_FLIST_OFFSET - dir_ndx`, terminated by
    /// `NDX_FLIST_EOF`) is read via the codec's async NDX reader; each segment's
    /// entries are decoded via the shared async leaf. Post-processing per segment
    /// (leader GNUM, sort, hardlink matching, pre-30 normalization, delete-pipeline
    /// publish) is identical to the sync path. Returns the total number of entries
    /// received across all sub-lists.
    ///
    // Unwired by design: the atomic receiver fork (ASY-7 redo) is the consuming
    // rung. Exercised only by the `async_parity` tests, so the non-test lib
    // build sees no caller.
    #[allow(dead_code)]
    pub(in crate::receiver) async fn receive_extra_file_lists_async<R>(
        &mut self,
        src: &mut R,
    ) -> io::Result<usize>
    where
        R: AsyncRead + Unpin + ?Sized,
    {
        let inc_recurse = self
            .compat_flags
            .is_some_and(|f| f.contains(CompatibilityFlags::INC_RECURSE));
        if !inc_recurse {
            return Ok(0);
        }

        let mut ndx_codec = create_ndx_codec(self.protocol.as_u8());
        let mut flist_reader = self
            .flist_reader_cache
            .take()
            .unwrap_or_else(|| self.build_flist_reader());
        let mut total_extra = 0;
        let mut carry: Vec<u8> = Vec::new();

        loop {
            let ndx = ndx_codec.read_ndx_from_carry_async(src, &mut carry).await?;

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

            let &(prev_flat_start, prev_ndx_start) =
                self.ndx_segments.last().expect("initial segment exists");
            let prev_used = (flat_start - prev_flat_start) as i32;
            let seg_ndx_start = prev_ndx_start + prev_used + 1;
            flist_reader.set_ndx_start(seg_ndx_start);

            let mut segment_count = 0;

            loop {
                let entry = read_entry_with_flist_async(
                    &mut flist_reader,
                    src,
                    &mut carry,
                    &self.file_list[flat_start..],
                )
                .await?;
                let Some(entry) = entry else { break };
                self.file_list.push(entry);
                segment_count += 1;
            }

            // upstream: flist.c:1646 - leader GNUM is readdir-order wire NDX.
            if self.config.flags.hard_links {
                for (i, entry) in self.file_list[flat_start..].iter_mut().enumerate() {
                    if entry.hlink_first() {
                        entry.set_hardlink_idx((seg_ndx_start + i as i32) as u32);
                    }
                }
            }

            // upstream: flist.c:2155,2736 - both sides call flist_sort_and_clean().
            if !self.iconv_reorder_suppressed() {
                sort_file_list(&mut self.file_list[flat_start..], true, false);
            }
            match_hard_links(&mut self.file_list[flat_start..], &mut self.prior_hlinks);

            if self.protocol.as_u8() < 30 && self.config.flags.hard_links {
                normalize_pre30_hardlinks(&mut self.file_list[flat_start..]);
            }

            // upstream: flist.c:2931 - ndx_start = prev->ndx_start + prev->used + 1
            self.ndx_segments.push((flat_start, seg_ndx_start));

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
}
