//! Streaming [`IncrementalFileListReceiver`] - yields file entries as they
//! arrive on the wire so the receiver can begin work without buffering the
//! full file list.
//!
//! Entries are released only after their parent directory has been yielded,
//! preserving the ordering guarantees the rest of the receiver relies on.

use std::collections::HashMap;
use std::io::{self, Read};

use protocol::flist::{FileEntry, FileListReader, IncrementalFileList, sort_file_list};

use super::hardlinks::match_hard_links;

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
    pub(in crate::receiver) flist_reader: FileListReader,
    /// Data source (network stream).
    pub(in crate::receiver) source: R,
    /// Incremental processor tracking dependencies.
    pub(in crate::receiver) incremental: IncrementalFileList,
    /// Whether we've finished reading from the wire.
    pub(in crate::receiver) finished_reading: bool,
    /// Number of entries read from the wire.
    pub(in crate::receiver) entries_read: usize,
    /// Whether to use unstable sort (qsort) instead of stable merge sort.
    pub(in crate::receiver) use_qsort: bool,
    /// When true, [`Self::collect_sorted`] skips the in-place reorder so the
    /// NDX-addressed array stays in sender scan order. Mirrors upstream's
    /// `need_unsorted_flist = 1` behaviour when `--iconv` is active and
    /// would transcode filenames between local and remote charsets.
    ///
    /// # Upstream Reference
    ///
    /// - `options.c:2069-2074` - `need_unsorted_flist = 1` when `iconv_opt`
    /// - `flist.c:2496-2498` - "both sides keep an unsorted file-list array
    ///   because the names will differ on the sending and receiving sides"
    pub(in crate::receiver) iconv_reorder_suppressed: bool,
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

        // upstream: flist.c:2736 - sort to match sender's order for NDX indexing.
        // IncrementalFileListReceiver is only used for INC_RECURSE (protocol >= 30).
        // When iconv would reorder the NDX-addressed array away from sender
        // scan order, skip the in-place sort - upstream's `need_unsorted_flist`
        // path keeps `flist->files[]` in scan order under the same condition.
        if !self.iconv_reorder_suppressed {
            sort_file_list(&mut entries, self.use_qsort, false);
        }
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
