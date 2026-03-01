//! File list reception and incremental processing for the receiver role.
//!
//! This submodule handles receiving the file list from the sender, including:
//! - Initial file list reception (`receive_file_list`)
//! - Incremental sub-list reception for `INC_RECURSE` mode (`receive_extra_file_lists`)
//! - UID/GID mapping list reception (`receive_id_lists`)
//! - Streaming incremental file list processing (`IncrementalFileListReceiver`)
//! - Failed directory tracking for error recovery (`FailedDirectories`)
//!
//! # Upstream Reference
//!
//! - `flist.c:recv_file_list()` — receives file entries from the wire
//! - `uidlist.c:recv_id_list()` — receives UID/GID name mappings
//! - `io.c:read_ndx_and_attrs()` — detects `NDX_FLIST_OFFSET` framing

use std::io::{self, Read};

use logging::debug_log;
use protocol::CompatibilityFlags;
use protocol::codec::{NDX_FLIST_EOF, NDX_FLIST_OFFSET, NdxCodec, create_ndx_codec};
use protocol::flist::{FileEntry, FileListReader, IncrementalFileListBuilder, sort_file_list};

#[cfg(unix)]
use metadata::id_lookup::{lookup_group_by_name, lookup_user_by_name};

use super::ReceiverContext;

// ============================================================================
// ReceiverContext flist methods
// ============================================================================

impl ReceiverContext {
    /// Creates a configured `FileListReader` matching the current protocol and flags.
    pub(super) fn build_flist_reader(&self) -> FileListReader {
        let mut reader = if let Some(flags) = self.compat_flags {
            FileListReader::with_compat_flags(self.protocol, flags)
        } else {
            FileListReader::new(self.protocol)
        }
        .with_preserve_uid(self.config.flags.owner)
        .with_preserve_gid(self.config.flags.group)
        .with_preserve_links(self.config.flags.links)
        .with_preserve_devices(self.config.flags.devices)
        .with_preserve_specials(self.config.flags.specials)
        .with_preserve_hard_links(self.config.flags.hard_links)
        .with_preserve_acls(self.config.flags.acls)
        .with_preserve_xattrs(self.config.flags.xattrs)
        .with_preserve_atimes(self.config.flags.atimes);

        if let Some(ref converter) = self.config.iconv {
            reader = reader.with_iconv(converter.clone());
        }
        reader
    }

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

        // Read entries until end marker or error
        // If SAFE_FILE_LIST is enabled, sender may transmit I/O error marker
        while let Some(entry) = flist_reader.read_entry(reader)? {
            self.file_list.push(entry);
            count += 1;
        }

        // Read ID lists (UID/GID mappings) after file list
        // Upstream: recv_id_list() is called when !inc_recurse
        // See flist.c:2726-2727 and uidlist.c:460
        let inc_recurse = self
            .compat_flags
            .is_some_and(|f| f.contains(CompatibilityFlags::INC_RECURSE));
        if !inc_recurse {
            self.receive_id_lists(reader)?;
        }

        // Sort file list to match sender's sorted order.
        // Upstream: flist_sort_and_clean() is called after recv_id_list()
        // See flist.c:2736 - both sides must sort to ensure matching NDX indices.
        sort_file_list(&mut self.file_list, self.config.qsort);

        // Cache the reader so receive_extra_file_lists() inherits the compression
        // state (prev_name, prev_mode, etc.), matching upstream's static variables.
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
    /// - `flist.c:recv_file_list()` — reads entries for each sub-list
    /// - `io.c:read_ndx_and_attrs()` — detects NDX_FLIST_OFFSET framing
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
        // Reuse the cached reader from receive_file_list() to preserve compression
        // state across sub-lists, matching upstream's static variables in
        // recv_file_entry() (prev_name, prev_mode, prev_uid, prev_gid).
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

            // Sort sub-list segment to match sender's sorted order.
            // upstream: flist.c:2155 — sender calls flist_sort_and_clean() after sending
            // upstream: flist.c:2736 — receiver calls flist_sort_and_clean() after receiving
            // Entries arrive in readdir() order; both sides must sort independently.
            // Use unstable sort (true) — sub-list entries have unique paths within
            // a directory, so stability is irrelevant and unstable is ~15% faster.
            sort_file_list(&mut self.file_list[flat_start..], true);

            // Build ndx_segments entry for this sub-list.
            // upstream: flist.c:2931 — flist->ndx_start = prev->ndx_start + prev->used + 1
            let &(prev_flat_start, prev_ndx_start) =
                self.ndx_segments.last().expect("initial segment exists");
            let prev_used = (flat_start - prev_flat_start) as i32;
            let seg_ndx_start = prev_ndx_start + prev_used + 1;
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
        // Skip ID lists when numeric_ids is set (upstream: numeric_ids <= 0)
        if self.config.flags.numeric_ids {
            return Ok(());
        }

        let id0_names = self
            .compat_flags
            .is_some_and(|f| f.contains(CompatibilityFlags::ID0_NAMES));

        let protocol_version = self.protocol.as_u8();

        // Read UID list if preserving ownership
        if self.config.flags.owner {
            self.uid_list
                .read(reader, id0_names, protocol_version, |name| {
                    lookup_user_by_name(name).ok().flatten()
                })?;
        }

        // Read GID list if preserving group
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

    /// Translates a remote UID to a local UID using the received mappings.
    ///
    /// Returns the mapped local UID if a mapping exists, otherwise returns the
    /// remote UID unchanged (falling back to numeric ID).
    #[inline]
    #[must_use]
    pub fn match_uid(&self, remote_uid: u32) -> u32 {
        self.uid_list.match_id(remote_uid)
    }

    /// Translates a remote GID to a local GID using the received mappings.
    ///
    /// Returns the mapped local GID if a mapping exists, otherwise returns the
    /// remote GID unchanged (falling back to numeric ID).
    #[inline]
    #[must_use]
    pub fn match_gid(&self, remote_gid: u32) -> u32 {
        self.gid_list.match_id(remote_gid)
    }
}

// ============================================================================
// FailedDirectories
// ============================================================================

/// Tracks directories that failed to create.
///
/// Children of failed directories are skipped during incremental processing.
#[derive(Debug, Default)]
pub(super) struct FailedDirectories {
    /// Failed directory paths (normalized, no trailing slash).
    paths: std::collections::HashSet<String>,
}

impl FailedDirectories {
    /// Creates a new empty tracker.
    pub(super) fn new() -> Self {
        Self::default()
    }

    /// Marks a directory as failed.
    pub(super) fn mark_failed(&mut self, path: &str) {
        self.paths.insert(path.to_string());
    }

    /// Checks if an entry path has a failed ancestor directory.
    ///
    /// Returns the failed ancestor path if found, `None` otherwise.
    pub(super) fn failed_ancestor(&self, entry_path: &str) -> Option<&str> {
        // Check if exact path is failed
        if self.paths.contains(entry_path) {
            return self.paths.get(entry_path).map(|s| s.as_str());
        }

        // Check each parent path component
        let mut check_path = entry_path;
        while let Some(pos) = check_path.rfind('/') {
            check_path = &check_path[..pos];
            if let Some(failed) = self.paths.get(check_path) {
                return Some(failed.as_str());
            }
        }
        None
    }

    /// Returns the number of failed directories.
    #[cfg(test)]
    pub(super) fn count(&self) -> usize {
        self.paths.len()
    }
}

// ============================================================================
// IncrementalFileListReceiver
// ============================================================================

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
    pub(super) incremental: protocol::flist::IncrementalFileList,
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
        // First check if we have any ready entries
        if let Some(entry) = self.incremental.pop() {
            return Ok(Some(entry));
        }

        // If we've finished reading, nothing more to yield
        if self.finished_reading {
            return Ok(None);
        }

        // Read entries until we get one that's ready or hit end of list
        loop {
            match self.flist_reader.read_entry(&mut self.source)? {
                Some(entry) => {
                    self.entries_read += 1;
                    self.incremental.push(entry);

                    // Check if this or any other entry is now ready
                    if let Some(ready) = self.incremental.pop() {
                        return Ok(Some(ready));
                    }
                    // No entry ready yet, keep reading
                }
                None => {
                    // End of file list
                    self.finished_reading = true;
                    // Return any remaining ready entry
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

        // Drain any already-ready entries
        entries.extend(self.incremental.drain_ready());

        // Read remaining entries
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

        // Drain any pending entries (they may have become orphans)
        entries.extend(self.incremental.drain_ready());

        // Sort to match sender's order for NDX indexing
        sort_file_list(&mut entries, self.use_qsort);

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
