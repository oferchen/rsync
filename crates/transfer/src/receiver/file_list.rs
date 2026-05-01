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
                if entry.flags().hlink_first() {
                    entry.set_hardlink_idx((ndx_start + i as i32) as u32);
                }
            }
        }

        // upstream: flist.c:2736 - flist_sort_and_clean() after recv_id_list()
        let pre29 = self.protocol.as_u8() < 29;
        sort_file_list(&mut self.file_list, self.config.qsort, pre29);
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
                    if entry.flags().hlink_first() {
                        entry.set_hardlink_idx((seg_ndx_start + i as i32) as u32);
                    }
                }
            }

            // upstream: flist.c:2155,2736 - both sides call flist_sort_and_clean()
            // independently. Unstable sort (true) is safe - entries have unique paths.
            // INC_RECURSE requires protocol >= 30, so pre29 is always false here.
            sort_file_list(&mut self.file_list[flat_start..], true, false);
            match_hard_links(&mut self.file_list[flat_start..], &mut self.prior_hlinks);

            // Normalize pre-30 hardlinks in this segment.
            if self.protocol.as_u8() < 30 && self.config.flags.hard_links {
                normalize_pre30_hardlinks(&mut self.file_list[flat_start..]);
            }

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
                        "ERROR: rejecting file-list entry with absolute path from sender: {} {}{}",
                        path.display(),
                        crate::role_trailer::error_location!(),
                        crate::role_trailer::receiver()
                    );
                    return false;
                }

                // Windows-only: reject any path that carries a Component::Prefix
                // (drive letter, UNC, `\\?\`, `\\.\`). `Path::has_root()` is
                // false for drive-relative inputs such as `C:foo`, but joining
                // such a path onto `dest_dir` on Windows discards `dest_dir`
                // entirely (`Path::join` semantics), letting a malicious sender
                // escape the destination tree. Upstream rsync runs only under
                // Cygwin's POSIX layer where these forms cannot occur, so the
                // defense lives only on the native-Win32 build. The `--relative`
                // exemption above does not apply: drive prefixes are never
                // valid in a wire path.
                #[cfg(windows)]
                if path
                    .components()
                    .next()
                    .is_some_and(|c| matches!(c, std::path::Component::Prefix(_)))
                {
                    info_log!(
                        Misc,
                        1,
                        "ERROR: rejecting file-list entry with Windows drive or UNC prefix from sender: {} {}{}",
                        path.display(),
                        crate::role_trailer::error_location!(),
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
                        "ERROR: rejecting file-list entry with \"..\" component from sender: {} {}{}",
                        path.display(),
                        crate::role_trailer::error_location!(),
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
        // IncrementalFileListReceiver is only used for INC_RECURSE (protocol >= 30).
        sort_file_list(&mut entries, self.use_qsort, false);
        let mut prior_hlinks = HashMap::new();
        match_hard_links(&mut entries, &mut prior_hlinks);

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
/// This function mirrors upstream `hlink.c:match_gnums()`: it groups entries by
/// `hardlink_idx` (gnum) and assigns `XMIT_HLINK_FIRST` to the first entry in each
/// group in sorted (positional) order, clearing it on all others.
///
/// The `prior_hlinks` map persists across INC_RECURSE segments so that a follower
/// whose leader was received in a previous segment is correctly identified as a
/// follower rather than being promoted to leader (which would happen if only the
/// current segment's entries were considered).
///
/// Must be called after `sort_file_list()` and before any code that inspects
/// `hlink_first()` to decide leader vs follower (transfer, quick-check, hardlink
/// creation).
///
/// # Upstream Reference
///
/// - `hlink.c:match_gnums()` - post-sort leader/follower assignment with
///   `prior_hlinks` hashtable for cross-segment state
/// - `hlink.c:idev_find()` - two-level (dev, ino) hashtable lookup
fn match_hard_links(entries: &mut [FileEntry], prior_hlinks: &mut HashMap<u32, bool>) {
    // Collect the first sorted position for each hardlink group within this segment.
    // Key: hardlink_idx (gnum), Value: index into entries slice.
    let mut first_in_group: HashMap<u32, usize> = HashMap::new();

    for (i, entry) in entries.iter().enumerate() {
        if let Some(idx) = entry.hardlink_idx() {
            first_in_group.entry(idx).or_insert(i);
        }
    }

    // Reassign flags: the leader is the first entry in sorted order, but only
    // if this gnum was not already seen in a prior INC_RECURSE segment.
    // upstream: hlink.c:match_gnums() - `node->data == data_when_new` check
    for (i, entry) in entries.iter_mut().enumerate() {
        if let Some(gnum) = entry.hardlink_idx() {
            let is_first_in_segment = first_in_group.get(&gnum) == Some(&i);
            let seen_before = prior_hlinks.contains_key(&gnum);

            if is_first_in_segment && !seen_before {
                // First occurrence of this gnum across all segments - this is the leader.
                entry.flags_mut().set_hlink_first(true);
                prior_hlinks.insert(gnum, true);
            } else {
                // Either not first in segment, or gnum was seen in a prior segment.
                entry.flags_mut().set_hlink_first(false);
                // Record the gnum even if it was already present, so future
                // segments know about it.
                prior_hlinks.entry(gnum).or_insert(true);
            }
        }
    }
}

/// Normalizes protocol 28-29 hardlink entries to use `hardlink_idx` and
/// `hlink_first` flags, matching the protocol 30+ representation.
///
/// For protocol < 30, the sender transmits raw (dev, ino) pairs instead of
/// hardlink group indices. This function groups entries by (dev, ino),
/// assigns a synthetic `hardlink_idx` to each group member, and sets
/// `hlink_first` on the first entry in sorted order. Entries with only one
/// occurrence of a (dev, ino) pair are left untouched - they are not part of
/// a hardlink group (nlink == 1 on the source).
///
/// After this normalization, `is_hardlink_follower()` and `create_hardlinks()`
/// work identically for both protocol versions.
///
/// # Upstream Reference
///
/// - `hlink.c:init_hard_links()` - builds hardlink table from (dev, ino) pairs
/// - `hlink.c:match_hard_links()` - assigns leader/follower after sorting
fn normalize_pre30_hardlinks(entries: &mut [FileEntry]) {
    // Group entries by (dev, ino) pairs. Key: (dev, ino), Value: list of indices.
    let mut groups: HashMap<(i64, i64), Vec<usize>> = HashMap::new();

    for (i, entry) in entries.iter().enumerate() {
        if !entry.is_file() {
            continue;
        }
        let dev = match entry.hardlink_dev() {
            Some(d) => d,
            None => continue,
        };
        let ino = match entry.hardlink_ino() {
            Some(n) => n,
            None => continue,
        };
        groups.entry((dev, ino)).or_default().push(i);
    }

    // Assign synthetic hardlink_idx and hlink_first flags.
    // Only process groups with 2+ members (actual hardlinks).
    // Use the first entry's position as the group key to avoid collisions
    // with protocol 30+ gnum values.
    for indices in groups.values() {
        if indices.len() < 2 {
            continue;
        }
        // Use the first entry's sorted position as the group's gnum.
        let gnum = indices[0] as u32;
        for (pos, &idx) in indices.iter().enumerate() {
            entries[idx].set_hardlink_idx(gnum);
            entries[idx].flags_mut().set_hlinked(true);
            entries[idx].flags_mut().set_hlink_first(pos == 0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::flist::FileEntry;

    /// Verifies that `match_hard_links` correctly assigns leader/follower flags
    /// when directory entries are interspersed among hardlinked files, as happens
    /// with `--relative` implied directories.
    ///
    /// After sorting, the first entry in each hardlink group (by sorted position)
    /// must get `hlink_first = true`. Directory entries that are not part of any
    /// hardlink group must be unaffected.
    #[test]
    fn match_hard_links_with_interspersed_directories() {
        // Simulate a sorted file list with implied directories from --relative:
        //   [0] dir  "a/"           (no hardlink)
        //   [1] file "a/orig.txt"   (group 7 - should become leader)
        //   [2] dir  "b/"           (no hardlink)
        //   [3] file "b/link.txt"   (group 7 - should become follower)
        let dir_a = FileEntry::new_directory("a".into(), 0o755);

        let mut leader = FileEntry::new_file("a/orig.txt".into(), 256, 0o644);
        leader.set_hardlink_idx(7);

        let dir_b = FileEntry::new_directory("b".into(), 0o755);

        let mut follower = FileEntry::new_file("b/link.txt".into(), 256, 0o644);
        follower.set_hardlink_idx(7);

        let mut entries = vec![dir_a, leader, dir_b, follower];
        let mut prior_hlinks = HashMap::new();
        match_hard_links(&mut entries, &mut prior_hlinks);

        // Directory entries remain unaffected
        assert!(!entries[0].flags().hlink_first());
        assert!(!entries[2].flags().hlink_first());

        // First file in group 7 (sorted position 1) becomes leader
        assert!(entries[1].flags().hlink_first());
        // Second file in group 7 (sorted position 3) becomes follower
        assert!(!entries[3].flags().hlink_first());
    }

    /// Verifies `match_hard_links` handles multiple hardlink groups interspersed
    /// with directories. Each group independently assigns its own leader.
    #[test]
    fn match_hard_links_multiple_groups_with_directories() {
        //   [0] dir  "d/"             (no hardlink)
        //   [1] file "d/a.txt"        (group 1 - leader)
        //   [2] file "d/a_link.txt"   (group 1 - follower)
        //   [3] dir  "e/"             (no hardlink)
        //   [4] file "e/b.txt"        (group 4 - leader)
        //   [5] file "e/b_link.txt"   (group 4 - follower)
        let dir_d = FileEntry::new_directory("d".into(), 0o755);

        let mut la = FileEntry::new_file("d/a.txt".into(), 100, 0o644);
        la.set_hardlink_idx(1);

        let mut fa = FileEntry::new_file("d/a_link.txt".into(), 100, 0o644);
        fa.set_hardlink_idx(1);

        let dir_e = FileEntry::new_directory("e".into(), 0o755);

        let mut lb = FileEntry::new_file("e/b.txt".into(), 200, 0o644);
        lb.set_hardlink_idx(4);

        let mut fb = FileEntry::new_file("e/b_link.txt".into(), 200, 0o644);
        fb.set_hardlink_idx(4);

        let mut entries = vec![dir_d, la, fa, dir_e, lb, fb];
        let mut prior_hlinks = HashMap::new();
        match_hard_links(&mut entries, &mut prior_hlinks);

        // Group 1: position 1 is leader, position 2 is follower
        assert!(entries[1].flags().hlink_first());
        assert!(!entries[2].flags().hlink_first());

        // Group 4: position 4 is leader, position 5 is follower
        assert!(entries[4].flags().hlink_first());
        assert!(!entries[5].flags().hlink_first());
    }

    /// Verifies `normalize_pre30_hardlinks` assigns synthetic hardlink_idx and
    /// hlink_first from (dev, ino) pairs for a simple two-file group.
    #[test]
    fn normalize_pre30_two_file_group() {
        let mut a = FileEntry::new_file("a.txt".into(), 100, 0o644);
        a.set_hardlink_dev(1);
        a.set_hardlink_ino(42);

        let mut b = FileEntry::new_file("b.txt".into(), 100, 0o644);
        b.set_hardlink_dev(1);
        b.set_hardlink_ino(42);

        let mut entries = vec![a, b];
        normalize_pre30_hardlinks(&mut entries);

        // Both entries get the same hardlink_idx
        assert_eq!(entries[0].hardlink_idx(), entries[1].hardlink_idx());
        // First entry is leader
        assert!(entries[0].flags().hlinked());
        assert!(entries[0].flags().hlink_first());
        // Second entry is follower
        assert!(entries[1].flags().hlinked());
        assert!(!entries[1].flags().hlink_first());
    }

    /// Verifies `normalize_pre30_hardlinks` leaves single-entry (dev, ino) pairs
    /// untouched - they are not part of a hardlink group.
    #[test]
    fn normalize_pre30_single_entry_not_grouped() {
        let mut a = FileEntry::new_file("only.txt".into(), 50, 0o644);
        a.set_hardlink_dev(99);
        a.set_hardlink_ino(1);

        let mut entries = vec![a];
        normalize_pre30_hardlinks(&mut entries);

        assert!(entries[0].hardlink_idx().is_none());
        assert!(!entries[0].flags().hlinked());
        assert!(!entries[0].flags().hlink_first());
    }

    /// Verifies `normalize_pre30_hardlinks` handles multiple independent groups.
    #[test]
    fn normalize_pre30_multiple_groups() {
        let mut a1 = FileEntry::new_file("a1.txt".into(), 100, 0o644);
        a1.set_hardlink_dev(1);
        a1.set_hardlink_ino(10);

        let mut a2 = FileEntry::new_file("a2.txt".into(), 100, 0o644);
        a2.set_hardlink_dev(1);
        a2.set_hardlink_ino(10);

        let mut b1 = FileEntry::new_file("b1.txt".into(), 200, 0o644);
        b1.set_hardlink_dev(2);
        b1.set_hardlink_ino(20);

        let mut b2 = FileEntry::new_file("b2.txt".into(), 200, 0o644);
        b2.set_hardlink_dev(2);
        b2.set_hardlink_ino(20);

        let mut entries = vec![a1, a2, b1, b2];
        normalize_pre30_hardlinks(&mut entries);

        // Group A: entries 0, 1
        let idx_a = entries[0].hardlink_idx().unwrap();
        assert_eq!(entries[1].hardlink_idx().unwrap(), idx_a);
        assert!(entries[0].flags().hlink_first());
        assert!(!entries[1].flags().hlink_first());

        // Group B: entries 2, 3
        let idx_b = entries[2].hardlink_idx().unwrap();
        assert_eq!(entries[3].hardlink_idx().unwrap(), idx_b);
        assert!(entries[2].flags().hlink_first());
        assert!(!entries[3].flags().hlink_first());

        // Different groups have different indices
        assert_ne!(idx_a, idx_b);
    }

    /// Verifies `normalize_pre30_hardlinks` skips directories (only files are hardlinked).
    #[test]
    fn normalize_pre30_skips_directories() {
        let dir = FileEntry::new_directory("dir".into(), 0o755);

        let mut f1 = FileEntry::new_file("f1.txt".into(), 100, 0o644);
        f1.set_hardlink_dev(1);
        f1.set_hardlink_ino(5);

        let mut f2 = FileEntry::new_file("f2.txt".into(), 100, 0o644);
        f2.set_hardlink_dev(1);
        f2.set_hardlink_ino(5);

        let mut entries = vec![dir, f1, f2];
        normalize_pre30_hardlinks(&mut entries);

        // Directory is untouched
        assert!(entries[0].hardlink_idx().is_none());
        // Files are grouped
        assert_eq!(entries[1].hardlink_idx(), entries[2].hardlink_idx());
        assert!(entries[1].flags().hlink_first());
        assert!(!entries[2].flags().hlink_first());
    }

    /// Verifies `normalize_pre30_hardlinks` skips entries without dev/ino.
    #[test]
    fn normalize_pre30_skips_entries_without_dev_ino() {
        let plain = FileEntry::new_file("plain.txt".into(), 100, 0o644);

        let mut linked = FileEntry::new_file("linked.txt".into(), 100, 0o644);
        linked.set_hardlink_dev(1);
        linked.set_hardlink_ino(5);

        let mut entries = vec![plain, linked];
        normalize_pre30_hardlinks(&mut entries);

        // Neither entry forms a group of 2+, so no normalization
        assert!(entries[0].hardlink_idx().is_none());
        assert!(entries[1].hardlink_idx().is_none());
    }

    /// Verifies that `match_hard_links` reassigns the leader when the readdir-order
    /// leader appears after a follower in sorted order. This happens when --relative
    /// paths cause the sender's first-seen file to sort after another file in the
    /// same hardlink group.
    #[test]
    fn match_hard_links_reassigns_leader_after_sort() {
        // Sender saw "z/file.txt" first (readdir order), but after sort
        // "a/file.txt" comes first. Both share hardlink group 5.
        let mut entry_a = FileEntry::new_file("a/file.txt".into(), 300, 0o644);
        entry_a.set_hardlink_idx(5);
        // Sender marked this as follower (not first in readdir order)
        entry_a.flags_mut().set_hlink_first(false);

        let mut entry_z = FileEntry::new_file("z/file.txt".into(), 300, 0o644);
        entry_z.set_hardlink_idx(5);
        // Sender marked this as leader (first in readdir order)
        entry_z.flags_mut().set_hlink_first(true);

        let mut entries = vec![entry_a, entry_z];
        let mut prior_hlinks = HashMap::new();
        match_hard_links(&mut entries, &mut prior_hlinks);

        // After match_hard_links, sorted position 0 becomes the new leader
        assert!(
            entries[0].flags().hlink_first(),
            "first in sorted order must be leader"
        );
        assert!(
            !entries[1].flags().hlink_first(),
            "second in sorted order must be follower"
        );
    }

    /// Verifies that cross-segment hardlink followers are not promoted to leaders.
    ///
    /// When INC_RECURSE delivers entries in multiple segments, a follower whose
    /// leader was in a previous segment must remain a follower. Before the fix,
    /// `match_hard_links` only saw the current segment and would incorrectly
    /// promote such followers to leaders.
    ///
    /// upstream: hlink.c:match_gnums() - `prior_hlinks` hashtable persists across
    /// segments so cross-segment followers are correctly identified.
    #[test]
    fn cross_segment_follower_not_promoted_to_leader() {
        // Segment 1: contains the leader for gnum 42
        let mut leader = FileEntry::new_file("a/original.txt".into(), 512, 0o644);
        leader.set_hardlink_idx(42);

        let mut seg1 = vec![leader];
        let mut prior_hlinks = HashMap::new();
        match_hard_links(&mut seg1, &mut prior_hlinks);

        // Leader in segment 1 should be marked as leader
        assert!(
            seg1[0].flags().hlink_first(),
            "first occurrence of gnum 42 must be leader"
        );
        // prior_hlinks should now contain gnum 42
        assert!(prior_hlinks.contains_key(&42));

        // Segment 2: contains a follower for gnum 42 (cross-directory hardlink)
        let mut follower = FileEntry::new_file("b/link.txt".into(), 512, 0o644);
        follower.set_hardlink_idx(42);

        let mut seg2 = vec![follower];
        match_hard_links(&mut seg2, &mut prior_hlinks);

        // Follower in segment 2 must NOT be promoted to leader - its leader is
        // in segment 1.
        assert!(
            !seg2[0].flags().hlink_first(),
            "cross-segment follower must not be promoted to leader"
        );
    }

    /// Verifies that multiple segments with mixed gnums correctly identify
    /// leaders and followers across segment boundaries.
    #[test]
    fn cross_segment_mixed_groups() {
        let mut prior_hlinks = HashMap::new();

        // Segment 1: leader for gnum 10, follower for gnum 10, leader for gnum 20
        let mut a = FileEntry::new_file("a/file1.txt".into(), 100, 0o644);
        a.set_hardlink_idx(10);
        let mut b = FileEntry::new_file("a/file2.txt".into(), 100, 0o644);
        b.set_hardlink_idx(10);
        let mut c = FileEntry::new_file("a/file3.txt".into(), 200, 0o644);
        c.set_hardlink_idx(20);

        let mut seg1 = vec![a, b, c];
        match_hard_links(&mut seg1, &mut prior_hlinks);

        assert!(seg1[0].flags().hlink_first(), "gnum 10 leader in seg1");
        assert!(!seg1[1].flags().hlink_first(), "gnum 10 follower in seg1");
        assert!(seg1[2].flags().hlink_first(), "gnum 20 leader in seg1");

        // Segment 2: follower for gnum 10, follower for gnum 20, leader for gnum 30
        let mut d = FileEntry::new_file("b/link1.txt".into(), 100, 0o644);
        d.set_hardlink_idx(10);
        let mut e = FileEntry::new_file("b/link3.txt".into(), 200, 0o644);
        e.set_hardlink_idx(20);
        let mut f = FileEntry::new_file("b/new.txt".into(), 300, 0o644);
        f.set_hardlink_idx(30);

        let mut seg2 = vec![d, e, f];
        match_hard_links(&mut seg2, &mut prior_hlinks);

        assert!(
            !seg2[0].flags().hlink_first(),
            "gnum 10 cross-segment follower"
        );
        assert!(
            !seg2[1].flags().hlink_first(),
            "gnum 20 cross-segment follower"
        );
        assert!(seg2[2].flags().hlink_first(), "gnum 30 new leader in seg2");
    }
}
