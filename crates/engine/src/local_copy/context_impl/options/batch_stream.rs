/// A record grows to this many bytes in memory before its bytes go straight to
/// the batch, when every record before it is already written.
const BATCH_DIRECT_THRESHOLD: usize = 1 << 20;

impl<'a> CopyContext<'a> {
    /// Reports whether the batch file lists stream as the walk reads each
    /// directory.
    pub(crate) const fn batch_streams(&self) -> bool {
        self.batch_stream.is_some()
    }

    /// The traversal index of the entry being itemized.
    pub(crate) const fn batch_entry_index(&self) -> i32 {
        self.batch_entry_index
    }

    /// Notes a top-level entry the list half just recorded: it belongs to the
    /// initial list.
    pub(super) fn note_batch_top_entry(&mut self, id: usize) {
        if let Some(initial) = self
            .batch_stream
            .as_mut()
            .and_then(|stream| stream.initial.as_mut())
        {
            initial.push(id);
        }
    }

    /// Releases the initial list of a non-directory operand, complete once
    /// its one entry is listed.
    ///
    /// upstream: flist.c:send_file_list() - the initial list ends with the
    /// last operand's top-level entry.
    pub(crate) fn complete_batch_initial(&mut self) -> Result<(), crate::local_copy::LocalCopyError> {
        self.push_batch_initial()?;
        self.release_batch_lists()
    }

    /// Records the scan of directory `dir`: plans its list and writes every
    /// list and record that is now ready.
    pub(crate) fn stream_batch_scan(
        &mut self,
        dir: &std::path::Path,
        mut listed: Vec<usize>,
    ) -> Result<(), crate::local_copy::LocalCopyError> {
        let top_level = dir.as_os_str().is_empty() || dir == std::path::Path::new(".");
        // A top-level scan lists the initial list's own entries.
        self.push_batch_initial()?;
        if !top_level {
            let sort_data = &self.batch_entry_sort_data;
            listed.sort_by(|a, b| {
                let (name_a, dir_a) = &sort_data[*a];
                let (name_b, dir_b) = &sort_data[*b];
                batch_entry_compare(name_a, *dir_a, name_b, *dir_b)
            });
            let entries = &self.batch_pending_entries;
            if let Some(stream) = self.batch_stream.as_mut() {
                stream
                    .planner
                    .push_scan(dir, listed.iter().map(|&i| (i, &entries[i])))
                    .map_err(batch_plan_error)?;
            }
        }
        self.release_batch_lists()
    }

    /// Hands the initial list to the planner once: every top-level entry
    /// listed so far, sorted.
    fn push_batch_initial(&mut self) -> Result<(), crate::local_copy::LocalCopyError> {
        let Some(mut initial) = self
            .batch_stream
            .as_mut()
            .and_then(|stream| stream.initial.take())
        else {
            return Ok(());
        };
        let sort_data = &self.batch_entry_sort_data;
        initial.sort_by(|a, b| {
            let (name_a, dir_a) = &sort_data[*a];
            let (name_b, dir_b) = &sort_data[*b];
            batch_entry_compare(name_a, *dir_a, name_b, *dir_b)
        });
        let entries = &self.batch_pending_entries;
        if let Some(stream) = self.batch_stream.as_mut() {
            stream
                .planner
                .push_initial(initial.iter().map(|&i| (i, &entries[i])))
                .map_err(batch_plan_error)?;
        }
        Ok(())
    }

    /// Encodes and releases every list the planner can place.
    fn release_batch_lists(&mut self) -> Result<(), crate::local_copy::LocalCopyError> {
        loop {
            let Some(stream) = self.batch_stream.as_mut() else {
                return Ok(());
            };
            let Some(list) = stream.planner.pop_ready() else {
                return Ok(());
            };
            // upstream: flist.c:2729-2730 - NDX_FLIST_EOF follows the last
            // sub-list in the same send_extra_file_list() call.
            let last = stream.planner.finished();
            let chunk = self.encode_batch_list(&list, last)?;
            let slots = self.batch_list_slots(&list);
            let mut out = Vec::new();
            if let Some(stream) = self.batch_stream.as_mut() {
                stream
                    .order
                    .release(chunk, list.members.len(), slots, &mut out);
                if last {
                    stream.order.lists_complete(&mut out);
                }
            }
            self.write_batch_emits(out)?;
        }
    }

    /// Encodes one list: its `NDX_FLIST_OFFSET - dir_ndx` announcement, its
    /// entries from `ndx_start`, and its end marker.
    fn encode_batch_list(
        &mut self,
        list: &protocol::flist::StreamedList,
        last: bool,
    ) -> Result<Vec<u8>, crate::local_copy::LocalCopyError> {
        let err = |e: std::io::Error| {
            crate::local_copy::LocalCopyError::io(
                "write batch incremental file list",
                std::path::PathBuf::new(),
                e,
            )
        };
        let io_error = self.batch_flist_io_error();
        // upstream: flist.c:2694-2701 - a list whose scan raised an error ends
        // with it; the transfer root's listing is the initial list.
        let listing = list
            .parent
            .map_or(std::path::Path::new("."), |p| {
                self.batch_pending_entries[p].path().as_path()
            });
        let carries_error = io_error != 0
            && self
                .batch_flist_error_dirs
                .iter()
                .any(|d| d.as_path() == listing);
        let codec = self
            .batch_ndx_codec
            .as_mut()
            .expect("batch NDX codec exists on the write-batch path");
        let flist_writer = self
            .batch_flist_writer
            .as_mut()
            .expect("batch flist writer exists on the write-batch path");
        let stream = self
            .batch_stream
            .as_mut()
            .expect("a streamed list has a stream");
        let mut buf = Vec::new();
        if let Some(dir_ndx) = list.parent_dir_ndx {
            protocol::codec::NdxCodec::write_ndx(
                codec,
                &mut buf,
                protocol::codec::NDX_FLIST_OFFSET - dir_ndx,
            )
            .map_err(err)?;
        }
        flist_writer.set_first_ndx(list.ndx_start);
        for (pos, &id) in list.members.iter().enumerate() {
            let mut entry = self.batch_pending_entries[id].clone();
            if let Some((access, default)) =
                self.batch_pending_acls.get_mut(id).and_then(Option::take)
            {
                flist_writer.set_pending_acl(access, default);
            }
            if let Some(idx) = entry.hardlink_idx() {
                // upstream: flist.c:828-854 - the group's first member in send
                // order leads it (XMIT_HLINK_FIRST); every later one names its
                // NDX.
                let group = if idx == u32::MAX { id as u32 } else { idx };
                match stream.group_leader.entry(group) {
                    std::collections::hash_map::Entry::Vacant(slot) => {
                        slot.insert(list.ndx_start + pos as i32);
                        entry.set_hardlink_idx(u32::MAX);
                    }
                    std::collections::hash_map::Entry::Occupied(leader) => {
                        entry.set_hardlink_idx(*leader.get() as u32);
                    }
                }
            }
            flist_writer.write_entry(&mut buf, &entry).map_err(err)?;
        }
        flist_writer
            .write_end(&mut buf, carries_error.then_some(io_error))
            .map_err(err)?;
        if last {
            protocol::codec::NdxCodec::write_ndx(codec, &mut buf, protocol::codec::NDX_FLIST_EOF)
                .map_err(err)?;
            stream.eof_written = true;
        }
        Ok(buf)
    }

    /// The entries whose records a list carries: the owning directory at the
    /// gap NDX below the list (`.` for the initial list), then every
    /// non-directory member at its own NDX. A member directory's record goes
    /// with its own sub-list.
    ///
    /// upstream: generator.c:1669, 2780-2788 - a directory is itemized at the
    /// gap NDX of its own list.
    fn batch_list_slots(&self, list: &protocol::flist::StreamedList) -> Vec<crate::local_copy::batch_order::Slot> {
        use crate::local_copy::batch_order::Slot;
        let mut slots = Vec::with_capacity(list.members.len() + 1);
        if let Some(owner) = list.parent {
            slots.push(Slot {
                id: owner,
                ndx: list.gap_ndx(),
            });
        }
        for (pos, &id) in list.members.iter().enumerate() {
            let entry = &self.batch_pending_entries[id];
            if !entry.is_dir() {
                slots.push(Slot {
                    id,
                    ndx: list.ndx_start + pos as i32,
                });
            } else if list.parent.is_none() && entry.name() == "." {
                slots.push(Slot {
                    id,
                    ndx: list.gap_ndx(),
                });
            }
        }
        slots
    }

    /// Writes stream pieces to the batch file in order.
    fn write_batch_emits(
        &mut self,
        out: Vec<crate::local_copy::batch_order::Emit>,
    ) -> Result<(), crate::local_copy::LocalCopyError> {
        use crate::local_copy::batch_order::Emit;
        if out.is_empty() {
            return Ok(());
        }
        let err = |what: &'static str, e: std::io::Error| {
            crate::local_copy::LocalCopyError::io(what, std::path::PathBuf::new(), e)
        };
        let writer = self
            .options
            .get_batch_writer()
            .expect("batch writer set on the write-batch path")
            .clone();
        let codec = self
            .batch_ndx_codec
            .as_mut()
            .expect("batch NDX codec exists on the write-batch path");
        let mut writer = writer.lock().expect("batch writer mutex poisoned");
        for emit in out {
            match emit {
                Emit::List(bytes) => writer
                    .write_data(&bytes)
                    .map_err(|e| err("write batch sub-list", std::io::Error::other(e)))?,
                Emit::Record { ndx, bytes } => {
                    let mut ndx_buf = Vec::with_capacity(4);
                    protocol::codec::NdxCodec::write_ndx(codec, &mut ndx_buf, ndx)
                        .map_err(|e| err("write batch NDX", e))?;
                    writer
                        .write_data(&ndx_buf)
                        .and_then(|()| writer.write_data(&bytes))
                        .map_err(|e| err("write batch delta data", std::io::Error::other(e)))?;
                }
            }
        }
        Ok(())
    }

    /// Hands the file record completed last to the order, now that its
    /// iflags are final.
    fn submit_batch_pending(&mut self) -> Result<(), crate::local_copy::LocalCopyError> {
        let mut out = Vec::new();
        if let Some(stream) = self.batch_stream.as_mut()
            && let Some((id, bytes, _)) = stream.pending.take()
        {
            stream.order.record(id, bytes, &mut out);
            stream.note_held(0);
        }
        self.write_batch_emits(out)
    }

    /// Keeps a stream write error raised where it cannot be returned, for
    /// the next call that can.
    fn defer_batch_stream_error(&mut self, error: crate::local_copy::LocalCopyError) {
        if let Some(stream) = self.batch_stream.as_mut() {
            stream.error.get_or_insert(error);
        }
    }

    /// Marks the entry at traversal index `id` finished: it adds no record.
    pub(crate) fn finish_batch_entry(&mut self, id: i32) -> Result<(), crate::local_copy::LocalCopyError> {
        let Some(stream) = self.batch_stream.as_mut() else {
            return Ok(());
        };
        if let Some(error) = stream.error.take() {
            return Err(error);
        }
        self.submit_batch_pending()?;
        let mut out = Vec::new();
        if let Some(stream) = self.batch_stream.as_mut() {
            if stream.direct.is_some_and(|(direct, _)| direct == id as usize) {
                stream.direct = None;
            }
            stream.order.finish(id as usize, &mut out);
        }
        self.write_batch_emits(out)
    }

    /// Accepts a finished record for entry `id`: a file's waits for its
    /// transfer report, anything else is ordered at once.
    fn stream_batch_record(
        &mut self,
        id: i32,
        bytes: Vec<u8>,
        iflags_offset: Option<usize>,
        file: bool,
    ) -> Result<(), crate::local_copy::LocalCopyError> {
        self.submit_batch_pending()?;
        let Some(stream) = self.batch_stream.as_mut() else {
            return Ok(());
        };
        if file {
            stream.pending = Some((id as usize, bytes, iflags_offset));
            stream.note_held(0);
            return Ok(());
        }
        let mut out = Vec::new();
        stream.order.record(id as usize, bytes, &mut out);
        stream.note_held(0);
        self.write_batch_emits(out)
    }

    /// Writes the current file's record straight to the batch once it is
    /// large and at the head of the stream, so a large file is never held
    /// whole in memory.
    fn stream_batch_record_through(&mut self) -> Result<(), crate::local_copy::LocalCopyError> {
        let len = match (&mut self.batch_stream, &self.batch_delta_buf) {
            (Some(stream), Some(buf)) => {
                let len = buf.get_ref().len();
                stream.note_held(len);
                len
            }
            _ => return Ok(()),
        };
        if len < BATCH_DIRECT_THRESHOLD {
            return Ok(());
        }
        let err = |what: &'static str, e: std::io::Error| {
            crate::local_copy::LocalCopyError::io(what, std::path::PathBuf::new(), e)
        };
        let writer = self
            .options
            .get_batch_writer()
            .expect("batch writer set on the write-batch path")
            .clone();
        if !self.batch_record_is_direct() {
            // The head (iflags, basis, xattr values, sum_head) must be final
            // before any of it is written.
            if !(self.batch_delta_head_final || self.batch_whole_file_sum.is_some()) {
                return Ok(());
            }
            let id = self.batch_current_delta_idx as usize;
            let mut out = Vec::new();
            let stream = self.batch_stream.as_mut().expect("checked above");
            if !stream.order.begin_direct(id, &mut out) {
                return Ok(());
            }
            let ndx = stream.order.ndx(id).expect("a head entry is placed");
            self.write_batch_emits(out)?;
            let head = self.batch_delta_sum_head.encode();
            let start = self.batch_delta_sum_head_offset;
            let buf = self.batch_delta_buf.as_mut().expect("checked above").get_mut();
            buf[start..start + head.len()].copy_from_slice(&head);
            let word = self
                .batch_delta_iflags_offset
                .map(|off| (off, u16::from_le_bytes([buf[off], buf[off + 1]])));
            let codec = self
                .batch_ndx_codec
                .as_mut()
                .expect("batch NDX codec exists on the write-batch path");
            let mut ndx_buf = Vec::with_capacity(4);
            protocol::codec::NdxCodec::write_ndx(codec, &mut ndx_buf, ndx)
                .map_err(|e| err("write batch NDX", e))?;
            let mut guard = writer.lock().expect("batch writer mutex poisoned");
            guard
                .write_data(&ndx_buf)
                .map_err(|e| err("write batch NDX", std::io::Error::other(e)))?;
            let at = guard
                .data_position()
                .map_err(|e| err("write batch delta data", std::io::Error::other(e)))?;
            guard
                .write_data(buf)
                .map_err(|e| err("write batch delta data", std::io::Error::other(e)))?;
            drop(guard);
            let iflags = word.map(|(off, word)| (at + off as u64, word));
            self.batch_stream.as_mut().expect("checked above").direct = Some((id, iflags));
            // The head is written; nothing may patch the buffer's copy.
            self.batch_delta_iflags_offset = None;
        } else {
            let buf = self.batch_delta_buf.as_ref().expect("checked above");
            writer
                .lock()
                .expect("batch writer mutex poisoned")
                .write_data(buf.get_ref())
                .map_err(|e| err("write batch delta data", std::io::Error::other(e)))?;
        }
        let buf = self.batch_delta_buf.as_mut().expect("checked above");
        buf.get_mut().clear();
        buf.set_position(0);
        Ok(())
    }

    /// Whether the current file's record is being written straight through.
    fn batch_record_is_direct(&self) -> bool {
        self.batch_stream
            .as_ref()
            .and_then(|stream| stream.direct)
            .is_some_and(|(id, _)| id == self.batch_current_delta_idx as usize)
    }

    /// Writes what is left of a record written straight through.
    fn finish_batch_record_through(&mut self, rest: &[u8]) -> Result<(), crate::local_copy::LocalCopyError> {
        self.options
            .get_batch_writer()
            .expect("batch writer set on the write-batch path")
            .lock()
            .expect("batch writer mutex poisoned")
            .write_data(rest)
            .map_err(|e| {
                crate::local_copy::LocalCopyError::io(
                    "write batch delta data",
                    std::path::PathBuf::new(),
                    std::io::Error::other(e),
                )
            })
    }

    /// ORs `report` into the iflags word of a record already written straight
    /// through, in place.
    fn patch_batch_record_through(&mut self, report: u16) -> Result<(), crate::local_copy::LocalCopyError> {
        let Some((_, Some((at, word)))) = self.batch_stream.as_ref().and_then(|s| s.direct) else {
            return Ok(());
        };
        let patched = word | report;
        if patched == word {
            return Ok(());
        }
        self.options
            .get_batch_writer()
            .expect("batch writer set on the write-batch path")
            .lock()
            .expect("batch writer mutex poisoned")
            .patch_data(at, &patched.to_le_bytes())
            .map_err(|e| {
                crate::local_copy::LocalCopyError::io(
                    "write batch iflags",
                    std::path::PathBuf::new(),
                    std::io::Error::other(e),
                )
            })
    }

    /// Ends the stream: every list is released and every held record written.
    fn close_batch_stream(&mut self) -> Result<(), crate::local_copy::LocalCopyError> {
        if let Some(error) = self.batch_stream.as_mut().and_then(|s| s.error.take()) {
            return Err(error);
        }
        self.submit_batch_pending()?;
        self.push_batch_initial()?;
        if let Some(stream) = self.batch_stream.as_mut() {
            stream.planner.end_walk();
        }
        self.release_batch_lists()?;
        let mut out = Vec::new();
        if let Some(stream) = self.batch_stream.as_mut() {
            stream.order.close(&mut out);
            debug_assert!(stream.order.is_empty(), "a batch record was never written");
            debug_assert!(stream.eof_written, "the last list carries NDX_FLIST_EOF");
            self.batch_list_count = stream.order.list_count();
            #[cfg(test)]
            super::batch_order::record_peak_held(stream.peak_held);
        }
        // Listings the walk never entered go with the stream.
        self.batch_prescan = None;
        self.write_batch_emits(out)
    }

    /// Takes the listings made ahead of the walk, for the lister to extend.
    pub(crate) fn take_batch_prescan(&mut self) -> Option<crate::local_copy::BatchPrescan> {
        self.batch_prescan.take()
    }

    /// Puts the listings back after the lister has used them.
    pub(crate) fn restore_batch_prescan(&mut self, prescan: crate::local_copy::BatchPrescan) {
        self.batch_prescan = Some(prescan);
    }

    /// A copy of the walk's source-side merge-file stack as it is now, for a
    /// lister that follows its own path through the tree.
    pub(crate) fn detached_dir_merge(&self) -> super::context::DirectoryFilterHandles {
        self.dir_merge.detached_copy()
    }

    /// Runs `f` with `stack` as the source-side merge-file stack, so filter
    /// checks and merge-file loading in `f` use it.
    pub(crate) fn with_dir_merge<R>(
        &mut self,
        stack: &mut super::context::DirectoryFilterHandles,
        f: impl FnOnce(&mut Self) -> R,
    ) -> R {
        std::mem::swap(&mut self.dir_merge, stack);
        let result = f(self);
        std::mem::swap(&mut self.dir_merge, stack);
        result
    }

    /// The directory whose scan the next list in send order waits for: its
    /// batch index and path.
    pub(crate) fn next_unscanned_batch_dir(&self) -> Option<(usize, std::path::PathBuf)> {
        self.batch_stream
            .as_ref()?
            .planner
            .next_unscanned()
            .map(|(id, dir)| (id, dir.to_path_buf()))
    }

    /// Whether entry `id`'s record would wait for a list not planned yet.
    pub(crate) fn batch_lists_wanted(&self, id: usize) -> bool {
        self.batch_stream
            .as_ref()
            .is_some_and(|stream| stream.order.lists_wanted(id))
    }

    /// Gives directory `dir`, which the walk never lists, an empty sub-list.
    pub(crate) fn release_unlisted_batch_dir(
        &mut self,
        dir: &std::path::Path,
    ) -> Result<(), crate::local_copy::LocalCopyError> {
        self.stream_batch_scan(dir, Vec::new())
    }
}

/// Maps a planner refusal to the batch writer's error.
fn batch_plan_error(e: protocol::flist::SubListPlanError) -> crate::local_copy::LocalCopyError {
    crate::local_copy::LocalCopyError::io(
        "write batch incremental file list",
        std::path::PathBuf::new(),
        std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()),
    )
}
