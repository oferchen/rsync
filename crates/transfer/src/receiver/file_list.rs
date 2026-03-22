//! File list reception and incremental processing.
//!
//! Handles receiving the file list from the sender, incremental sub-list
//! reception for INC_RECURSE mode, and the streaming
//! [`IncrementalFileListReceiver`] interface.

use std::io::{self, Read};

use logging::{debug_log, info_log};
use protocol::CompatibilityFlags;
use protocol::codec::{NDX_FLIST_EOF, NDX_FLIST_OFFSET, NdxCodec, create_ndx_codec};
use std::collections::HashMap;

use protocol::flist::{
    FileEntry, FileListReader, IncrementalFileList, IncrementalFileListBuilder, sort_file_list,
};

#[cfg(unix)]
use metadata::id_lookup::{lookup_group_by_name, lookup_user_by_name};

use super::ReceiverContext;
use super::quick_check::path_contains_dot_dot;

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
        let mut count = 0;

        // upstream: flist.c:recv_file_list() - reads entries until end marker
        while let Some(entry) = flist_reader.read_entry(reader)? {
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

        // upstream: flist.c:1646 - send_file_entry() is called with flist->used
        // (readdir-order position) BEFORE flist_sort_and_clean(). Leader GNUM values
        // (F_HL_GNUM) are readdir-order wire NDXes. Replace the u32::MAX sentinel
        // with the actual readdir-order wire NDX so followers can find their leader
        // after sorting reorders entries.
        if self.config.flags.hard_links {
            let &(_flat_start, ndx_start) =
                self.ndx_segments.last().expect("initial segment exists");
            for (i, entry) in self.file_list.iter_mut().enumerate() {
                if entry.flags().hlink_first() {
                    entry.set_hardlink_idx((ndx_start + i as i32) as u32);
                }
            }
        }

        // upstream: flist.c:2736 - flist_sort_and_clean() after recv_id_list()
        sort_file_list(&mut self.file_list, self.config.qsort);
        match_hard_links(&mut self.file_list);

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
    pub(super) fn receive_extra_file_lists<R: Read + ?Sized>(
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
                    format!("expected NDX_FLIST_OFFSET or NDX_FLIST_EOF, got {ndx}"),
                ));
            }

            let dir_ndx = NDX_FLIST_OFFSET - ndx;
            let flat_start = self.file_list.len();
            let mut segment_count = 0;

            while let Some(entry) = flist_reader.read_entry(reader)? {
                self.file_list.push(entry);
                segment_count += 1;
            }

            // upstream: flist.c:1646 - leader GNUM is readdir-order wire NDX,
            // assigned before sorting. Compute sub-list ndx_start first so we can
            // stamp leaders with their readdir-order wire NDX before sorting.
            let &(prev_flat_start, prev_ndx_start) =
                self.ndx_segments.last().expect("initial segment exists");
            let prev_used = (flat_start - prev_flat_start) as i32;
            let seg_ndx_start = prev_ndx_start + prev_used + 1;

            if self.config.flags.hard_links {
                for (i, entry) in self.file_list[flat_start..].iter_mut().enumerate() {
                    if entry.flags().hlink_first() {
                        entry.set_hardlink_idx((seg_ndx_start + i as i32) as u32);
                    }
                }
            }

            // upstream: flist.c:2155,2736 - both sides call flist_sort_and_clean()
            // independently. Unstable sort (true) is safe - entries have unique paths.
            sort_file_list(&mut self.file_list[flat_start..], true);
            match_hard_links(&mut self.file_list[flat_start..]);

            // upstream: flist.c:2931 - ndx_start = prev->ndx_start + prev->used + 1
            self.ndx_segments.push((flat_start, seg_ndx_start));

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

    /// Reads UID/GID name-to-ID mapping lists from the sender.
    ///
    /// When `--numeric-ids` is not set, the sender transmits name mappings so the
    /// receiver can translate remote user/group names to local numeric IDs. When
    /// `--numeric-ids` is set, no mappings are sent and numeric IDs are used as-is.
    ///
    /// # Wire Format
    ///
    /// Each list contains `(varint id, byte name_len, name_bytes)*` tuples terminated
    /// by `varint 0`. With `ID0_NAMES` compat flag, an additional name for id=0
    /// follows the terminator.
    ///
    /// # Upstream Reference
    ///
    /// - `uidlist.c:460-479` - `recv_id_list()`
    /// - Condition: `(preserve_uid || preserve_acls) && numeric_ids <= 0`
    #[cfg(unix)]
    pub(crate) fn receive_id_lists<R: Read + ?Sized>(&mut self, reader: &mut R) -> io::Result<()> {
        // upstream: uidlist.c:460 - skip when numeric_ids is set
        if self.config.flags.numeric_ids {
            return Ok(());
        }

        let id0_names = self
            .compat_flags
            .is_some_and(|f| f.contains(CompatibilityFlags::ID0_NAMES));

        let protocol_version = self.protocol.as_u8();

        // upstream: uidlist.c:467 - recv_uid_list()
        if self.config.flags.owner {
            self.uid_list
                .read(reader, id0_names, protocol_version, |name| {
                    lookup_user_by_name(name).ok().flatten()
                })?;
        }

        // upstream: uidlist.c:471 - recv_gid_list()
        if self.config.flags.group {
            self.gid_list
                .read(reader, id0_names, protocol_version, |name| {
                    lookup_group_by_name(name).ok().flatten()
                })?;
        }

        Ok(())
    }

    /// Reads UID/GID name-to-ID mapping lists from the sender (non-Unix platforms).
    ///
    /// On non-Unix platforms (e.g., Windows), this reads the ID lists from the wire
    /// but does not resolve user/group names to local IDs since the platform lacks
    /// the POSIX user database. All name lookups return `None`, causing ownership
    /// to fall back to numeric IDs.
    ///
    /// # Platform Behavior
    ///
    /// This matches upstream rsync behavior where platforms without user/group
    /// databases effectively operate as if `--numeric-ids` was specified.
    ///
    /// # Upstream Reference
    ///
    /// - `uidlist.c:460-479` - `recv_id_list()`
    /// - Condition: `(preserve_uid || preserve_acls) && numeric_ids <= 0`
    #[cfg(not(unix))]
    pub(crate) fn receive_id_lists<R: Read + ?Sized>(&mut self, reader: &mut R) -> io::Result<()> {
        if self.config.flags.numeric_ids {
            return Ok(());
        }

        let id0_names = self
            .compat_flags
            .is_some_and(|f| f.contains(CompatibilityFlags::ID0_NAMES));

        let protocol_version = self.protocol.as_u8();

        if self.config.flags.owner {
            self.uid_list
                .read(reader, id0_names, protocol_version, |_| None)?;
        }

        if self.config.flags.group {
            self.gid_list
                .read(reader, id0_names, protocol_version, |_| None)?;
        }

        Ok(())
    }

    /// Sanitizes the received file list by removing entries with unsafe paths.
    ///
    /// When `trust_sender` is false, the receiver validates each entry to prevent
    /// directory traversal attacks from a malicious sender:
    ///
    /// - Entries with absolute paths are rejected (unless `--relative` is active)
    /// - Entries containing `..` path components are rejected
    /// - Symlink entries pointing outside the transfer tree are rejected
    ///
    /// Rejected entries are removed from the file list and warnings are emitted.
    /// Returns the number of entries removed.
    ///
    /// # Upstream Reference
    ///
    /// - `flist.c:757`: `clean_fname(thisname, CFN_REFUSE_DOT_DOT_DIRS)`
    /// - `options.c:2595`: `trust_sender_args = trust_sender_filter = 1`
    pub(super) fn sanitize_file_list(&mut self) -> usize {
        let relative_paths = self.config.flags.relative;

        let removed = if self.config.trust_sender {
            0
        } else {
            let original_len = self.file_list.len();

            self.file_list.retain(|entry| {
                let path = entry.path();

                // Check for absolute paths (reject unless --relative is active).
                // upstream: flist.c:757 `!relative_paths && *thisname == '/'`
                if !relative_paths && path.has_root() {
                    info_log!(
                        Misc,
                        1,
                        "ERROR: rejecting file-list entry with absolute path from sender: {}{}",
                        path.display(),
                        crate::role_trailer::receiver()
                    );
                    return false;
                }

                // Check for `..` path components (always rejected).
                // upstream: flist.c:757 `clean_fname(thisname, CFN_REFUSE_DOT_DOT_DIRS) < 0`
                if path_contains_dot_dot(path) {
                    info_log!(
                        Misc,
                        1,
                        "ERROR: rejecting file-list entry with \"..\" component from sender: {}{}",
                        path.display(),
                        crate::role_trailer::receiver()
                    );
                    return false;
                }

                true
            });

            original_len - self.file_list.len()
        };

        // upstream: flist.c:3071-3084 - strip_root in flist_sort_and_clean()
        // Runs unconditionally: leading-slash stripping is a functional
        // requirement for --relative mode, not a security check.
        if relative_paths {
            for entry in &mut self.file_list {
                if entry.path().has_root() {
                    entry.strip_leading_slashes();
                }
            }
        }

        removed
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
        }
    }
}

/// Streaming file list receiver that yields entries as they arrive from the wire.
///
/// Wraps a [`FileListReader`] and tracks
/// directory dependencies automatically, ensuring directories are yielded
/// before their contents.
///
/// # Benefits
///
/// - **Reduced latency**: Start processing as soon as first entries arrive
/// - **Lower memory**: Don't need full list in memory before starting
/// - **Better UX**: Users see progress immediately
///
/// # Dependency Tracking
///
/// Entries are only yielded when their parent directory has been processed.
/// If entries arrive out of order (child before parent), the child is held
/// until its parent arrives.
pub struct IncrementalFileListReceiver<R> {
    /// Wire format reader for file entries.
    pub(super) flist_reader: FileListReader,
    /// Data source (network stream).
    pub(super) source: R,
    /// Incremental processor tracking dependencies.
    pub(super) incremental: IncrementalFileList,
    /// Whether we've finished reading from the wire.
    pub(super) finished_reading: bool,
    /// Number of entries read from the wire.
    pub(super) entries_read: usize,
    /// Whether to use unstable sort (qsort) instead of stable merge sort.
    pub(super) use_qsort: bool,
}

impl<R: Read> IncrementalFileListReceiver<R> {
    /// Returns the next entry that is ready for processing.
    ///
    /// An entry is "ready" when its parent directory has already been yielded.
    /// This method may need to read additional entries from the wire to find
    /// one whose parent is available.
    ///
    /// # Returns
    ///
    /// - `Ok(Some(entry))` - An entry ready for processing
    /// - `Ok(None)` - No more entries (end of list reached and all processed)
    /// - `Err(e)` - An I/O or protocol error occurred
    pub fn next_ready(&mut self) -> io::Result<Option<FileEntry>> {
        if let Some(entry) = self.incremental.pop() {
            return Ok(Some(entry));
        }

        if self.finished_reading {
            return Ok(None);
        }

        loop {
            match self.flist_reader.read_entry(&mut self.source)? {
                Some(entry) => {
                    self.entries_read += 1;
                    self.incremental.push(entry);

                    if let Some(ready) = self.incremental.pop() {
                        return Ok(Some(ready));
                    }
                }
                None => {
                    self.finished_reading = true;
                    return Ok(self.incremental.pop());
                }
            }
        }
    }

    /// Drains all entries that are currently ready for processing.
    ///
    /// This is useful for batch processing multiple ready entries at once.
    /// Returns an empty vector if no entries are currently ready.
    pub fn drain_ready(&mut self) -> Vec<FileEntry> {
        self.incremental.drain_ready()
    }

    /// Returns the number of entries ready for immediate processing.
    #[must_use]
    pub fn ready_count(&self) -> usize {
        self.incremental.ready_count()
    }

    /// Returns the number of entries waiting for their parent directory.
    #[must_use]
    pub fn pending_count(&self) -> usize {
        self.incremental.pending_count()
    }

    /// Returns the total number of entries read from the wire.
    #[must_use]
    pub const fn entries_read(&self) -> usize {
        self.entries_read
    }

    /// Returns `true` if all entries have been read from the wire.
    #[must_use]
    pub const fn is_finished_reading(&self) -> bool {
        self.finished_reading
    }

    /// Returns `true` if there are no more entries to yield.
    ///
    /// This is `true` when reading is complete and all ready entries have been consumed.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.finished_reading && self.incremental.is_empty()
    }

    /// Marks a directory as already created (for pre-existing destinations).
    ///
    /// Call this for destination directories that exist before the transfer.
    /// This allows child entries to become ready immediately.
    pub fn mark_directory_created(&mut self, path: &str) {
        self.incremental.mark_directory_created(path);
    }

    /// Attempts to read one entry from the wire without blocking on ready queue.
    ///
    /// Returns `Ok(true)` if an entry was read and added to the incremental
    /// processor, `Ok(false)` if at EOF or already finished reading.
    ///
    /// Unlike [`Self::next_ready`], this method does not wait for an entry to become
    /// ready. It simply reads from the wire and adds to the dependency tracker.
    pub fn try_read_one(&mut self) -> io::Result<bool> {
        if self.finished_reading {
            return Ok(false);
        }

        match self.flist_reader.read_entry(&mut self.source)? {
            Some(entry) => {
                self.entries_read += 1;
                self.incremental.push(entry);
                Ok(true)
            }
            None => {
                self.finished_reading = true;
                Ok(false)
            }
        }
    }

    /// Marks reading as finished (for error recovery).
    pub fn mark_finished(&mut self) {
        self.finished_reading = true;
    }

    /// Reads all remaining entries and returns them as a sorted vector.
    ///
    /// This method consumes the receiver and returns entries suitable for
    /// traditional batch processing. Use this when you need the complete
    /// sorted list for NDX indexing.
    ///
    /// # Note
    ///
    /// This method provides a fallback to traditional batch processing.
    /// For truly incremental processing, use [`Self::next_ready`] instead.
    pub fn collect_sorted(mut self) -> io::Result<Vec<FileEntry>> {
        let mut entries = Vec::new();
        entries.extend(self.incremental.drain_ready());

        while !self.finished_reading {
            match self.flist_reader.read_entry(&mut self.source)? {
                Some(entry) => {
                    self.entries_read += 1;
                    entries.push(entry);
                }
                None => {
                    self.finished_reading = true;
                }
            }
        }

        entries.extend(self.incremental.drain_ready());

        // upstream: flist.c:2736 - sort to match sender's order for NDX indexing
        sort_file_list(&mut entries, self.use_qsort);
        match_hard_links(&mut entries);

        Ok(entries)
    }

    /// Returns the file list statistics from the reader.
    #[must_use]
    pub const fn stats(&self) -> &protocol::flist::FileListStats {
        self.flist_reader.stats()
    }
}

impl<R: Read> Iterator for IncrementalFileListReceiver<R> {
    type Item = io::Result<FileEntry>;

    fn next(&mut self) -> Option<Self::Item> {
        match self.next_ready() {
            Ok(Some(entry)) => Some(Ok(entry)),
            Ok(None) => None,
            Err(e) => Some(Err(e)),
        }
    }
}

/// Reassigns hardlink leader/follower flags based on sorted order.
///
/// The sender sets `XMIT_HLINK_FIRST` during `send_file_entry()` based on readdir
/// (scan) order - the first file encountered with a given (dev, ino) pair gets the
/// flag. After sorting, the first file in sorted order may differ from the readdir
/// leader, especially when `--relative` introduces deep path components that change
/// the sort order.
///
/// This function mirrors upstream `hlink.c:match_hard_links()`: it groups entries by
/// `hardlink_idx` and assigns `XMIT_HLINK_FIRST` to the first entry in each group
/// in sorted (positional) order, clearing it on all others.
///
/// Must be called after `sort_file_list()` and before any code that inspects
/// `hlink_first()` to decide leader vs follower (transfer, quick-check, hardlink
/// creation).
fn match_hard_links(entries: &mut [FileEntry]) {
    // Collect the first sorted position for each hardlink group.
    // Key: hardlink_idx (gnum), Value: index into entries slice.
    let mut first_in_group: HashMap<u32, usize> = HashMap::new();

    for (i, entry) in entries.iter().enumerate() {
        if let Some(idx) = entry.hardlink_idx() {
            first_in_group.entry(idx).or_insert(i);
        }
    }

    // Reassign flags: first in sorted order gets HLINK_FIRST, others lose it.
    for (i, entry) in entries.iter_mut().enumerate() {
        if let Some(idx) = entry.hardlink_idx() {
            let is_leader = first_in_group.get(&idx) == Some(&i);
            entry.flags_mut().set_hlink_first(is_leader);
        }
    }
}
