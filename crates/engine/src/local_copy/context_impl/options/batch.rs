impl<'a> CopyContext<'a> {
    /// Access the batch writer for recording transfer operations.
    ///
    /// Returns a reference to the batch writer if batch mode is enabled,
    /// or None if batch mode is not active.
    pub(super) const fn batch_writer(
        &self,
    ) -> Option<&std::sync::Arc<std::sync::Mutex<crate::batch::BatchWriter>>> {
        self.options.get_batch_writer()
    }

    /// Access the protocol flist writer for batch mode encoding.
    ///
    /// Returns a mutable reference to the [`FileListWriter`] used to encode
    /// file entries in the protocol wire format for batch files. The writer
    /// maintains cross-entry compression state.
    pub(super) fn batch_flist_writer_mut(
        &mut self,
    ) -> Option<&mut protocol::flist::FileListWriter> {
        self.batch_flist_writer.as_mut()
    }

    /// Writes the flist end-of-list marker to the batch file.
    ///
    /// Upstream rsync batch files are a raw tee of the protocol stream, which
    /// includes the end-of-list marker (0x00 byte for non-varint, varint(0) +
    /// varint(io_error) for varint mode) after all file entries. Without this
    /// marker, `BatchReader::read_protocol_flist` cannot determine where
    /// the file list ends and delta operations begin.
    ///
    /// Must be called after all file entries have been captured and before
    /// any delta operations or trailing stats are written.
    ///
    /// # Upstream Reference
    ///
    /// - `flist.c:send_file_list()` writes the end-of-list marker after all
    ///   entries via `write_byte(f, 0)` (pre-varint) or the varint equivalent.
    pub(crate) fn finalize_batch_flist(&mut self) -> Result<(), crate::local_copy::LocalCopyError> {
        let flist_writer = match self.batch_flist_writer.as_ref() {
            Some(w) => w,
            None => return Ok(()),
        };

        let mut buf = Vec::with_capacity(4);
        flist_writer.write_end(&mut buf, None).map_err(|e| {
            crate::local_copy::LocalCopyError::io(
                "write batch flist end marker",
                std::path::PathBuf::new(),
                e,
            )
        })?;

        let batch_writer_arc = match self.options.get_batch_writer() {
            Some(w) => w.clone(),
            None => return Ok(()),
        };
        let mut writer_guard = batch_writer_arc.lock().expect("batch writer mutex poisoned");
        writer_guard.write_data(&buf).map_err(|e| {
            crate::local_copy::LocalCopyError::io(
                "write batch flist end marker",
                std::path::PathBuf::new(),
                std::io::Error::other(e),
            )
        })?;

        Ok(())
    }

    /// Writes empty uid/gid ID lists to the batch file.
    ///
    /// upstream: uidlist.c:send_id_lists() - without INC_RECURSE, ID lists
    /// are written between the flist end marker and the delta data. Since
    /// user/group names are already embedded inline via XMIT_USER_NAME_FOLLOWS,
    /// the post-flist ID lists are empty (just varint30(0) terminators).
    ///
    /// upstream: flist.c:2548 - `if (numeric_ids <= 0 && !inc_recurse)
    /// send_id_lists(f)`. ID lists are only emitted when INC_RECURSE is
    /// inactive; under INC_RECURSE the uid/gid names are inlined into
    /// each flist entry via XMIT_USER_NAME_FOLLOWS / XMIT_GROUP_NAME_FOLLOWS
    /// and no post-flist ID list bytes appear on the wire. Emitting the
    /// terminators anyway leaves stray varints in the stream and drifts
    /// the reader's position so subsequent NDX reads decode garbage.
    ///
    /// Must be called after `finalize_batch_flist()` and before
    /// `flush_batch_delta_to_batch()`.
    pub(crate) fn write_batch_id_lists(&mut self) -> Result<(), crate::local_copy::LocalCopyError> {
        let batch_writer_arc = match self.options.get_batch_writer() {
            Some(w) => w.clone(),
            None => return Ok(()),
        };

        let (proto, compat_flags, numeric_ids, preserve_uid, preserve_gid, preserve_acls) = {
            let cfg = batch_writer_arc.lock().expect("batch writer mutex poisoned");
            let flags = cfg.stream_flags();
            (
                cfg.config().protocol_version,
                cfg.config().compat_flags,
                cfg.config().numeric_ids,
                flags.preserve_uid,
                flags.preserve_gid,
                flags.preserve_acls,
            )
        };

        // upstream: flist.c:2548 - `if (numeric_ids <= 0 && !inc_recurse)
        // send_id_lists(f)`. Under --numeric-ids no id-lists are emitted, so the
        // reader (reader/flist.rs) must find none. numeric_ids is not a stream
        // flag; it is carried in the batch config from the invocation.
        if numeric_ids {
            return Ok(());
        }

        // upstream: flist.c:2548 - skip send_id_lists() under INC_RECURSE.
        let inc_recurse = compat_flags
            .map(|cf| {
                protocol::CompatibilityFlags::from_bits(cf as u32)
                    .contains(protocol::CompatibilityFlags::INC_RECURSE)
            })
            .unwrap_or(false);
        if inc_recurse {
            return Ok(());
        }

        // upstream: uidlist.c:send_id_lists() - the uid list is emitted only
        // when (preserve_uid || preserve_acls), and the gid list only when
        // (preserve_gid || preserve_acls). The matching reader
        // (crates/batch/src/reader/flist.rs, recv_id_list) gates on the same
        // predicates, so emitting terminators unconditionally would drift the
        // stream cursor and corrupt the subsequent NDX reads. Each list, when
        // present, terminates with a single varint30(0) (no ID0_NAMES inline).
        let send_uid_list = preserve_uid || preserve_acls;
        let send_gid_list = preserve_gid || preserve_acls;
        if !send_uid_list && !send_gid_list {
            return Ok(());
        }

        let mut buf = Vec::with_capacity(2);
        if send_uid_list {
            protocol::write_varint30_int(&mut buf, 0, proto as u8).map_err(|e| {
                crate::local_copy::LocalCopyError::io(
                    "write batch uid list terminator",
                    std::path::PathBuf::new(),
                    e,
                )
            })?;
        }
        if send_gid_list {
            protocol::write_varint30_int(&mut buf, 0, proto as u8).map_err(|e| {
                crate::local_copy::LocalCopyError::io(
                    "write batch gid list terminator",
                    std::path::PathBuf::new(),
                    e,
                )
            })?;
        }

        let mut writer_guard = batch_writer_arc.lock().expect("batch writer mutex poisoned");
        writer_guard.write_data(&buf).map_err(|e| {
            crate::local_copy::LocalCopyError::io(
                "write batch id lists",
                std::path::PathBuf::new(),
                std::io::Error::other(e),
            )
        })?;

        Ok(())
    }

    /// Appends a literal token for the current file to the batch delta buffer.
    ///
    /// upstream: `token.c:1065 send_token()` dispatches on `do_compression`.
    /// With `-z` (stream-flag bit 8, `batch.c:68`) the batch tee records
    /// `send_deflated_token()` framing; without it, `simple_send_token()`'s
    /// plain 4-byte length prefix. A no-op when batch mode is inactive.
    pub(super) fn write_batch_literal_token(
        &mut self,
        chunk: &[u8],
        path: &std::path::Path,
    ) -> Result<(), crate::local_copy::LocalCopyError> {
        let Some(delta_file) = self.batch_delta_buf.as_mut() else {
            return Ok(());
        };
        match self.batch_token_encoder.as_mut() {
            Some(encoder) => encoder.send_literal(delta_file, chunk),
            None => protocol::wire::delta::write_token_literal(delta_file, chunk),
        }
        .map_err(|e| {
            crate::local_copy::LocalCopyError::io(
                "write batch literal token",
                path.to_path_buf(),
                e,
            )
        })
    }

    /// Appends a block-match token for the current file to the batch delta
    /// buffer.
    ///
    /// upstream: `token.c:simple_send_token()` writes `write_int(-(token+1))`;
    /// `token.c:send_deflated_token()` emits the run-length encoded form. Under
    /// CPRES_ZLIB the sender must also feed the matched bytes into the deflate
    /// history (`token.c:471-484`) so the receiver's inflate dictionary stays in
    /// sync; `see_data` carries those bytes for that purpose.
    pub(super) fn write_batch_block_match_token(
        &mut self,
        block_index: u32,
        see_data: &[u8],
        path: &std::path::Path,
    ) -> Result<(), crate::local_copy::LocalCopyError> {
        let Some(delta_file) = self.batch_delta_buf.as_mut() else {
            return Ok(());
        };
        let result = match self.batch_token_encoder.as_mut() {
            Some(encoder) => match encoder.send_block_match(delta_file, block_index) {
                Ok(()) => encoder.see_token(see_data),
                Err(e) => Err(e),
            },
            None => protocol::wire::delta::write_token_block_match(delta_file, block_index),
        };
        result.map_err(|e| {
            crate::local_copy::LocalCopyError::io(
                "write batch block match token",
                path.to_path_buf(),
                e,
            )
        })
    }

    /// Appends the end-of-file token for the current file to the batch delta
    /// buffer, flushing any pending compressed output.
    ///
    /// upstream: `token.c:simple_send_token()` writes `write_int(0)`;
    /// `token.c:468-470 send_deflated_token()` writes `END_FLAG` after draining
    /// the deflate stream.
    fn write_batch_token_end(
        &mut self,
        path: &std::path::Path,
    ) -> Result<(), crate::local_copy::LocalCopyError> {
        let Some(delta_file) = self.batch_delta_buf.as_mut() else {
            return Ok(());
        };
        match self.batch_token_encoder.as_mut() {
            Some(encoder) => encoder.finish(delta_file),
            None => protocol::wire::delta::write_token_end(delta_file),
        }
        .map_err(|e| {
            crate::local_copy::LocalCopyError::io(
                "write batch token end marker",
                path.to_path_buf(),
                e,
            )
        })
    }

    /// Writes the iflags + sum_head preamble for a file's delta data
    /// to the per-file batch delta buffer.
    ///
    /// The NDX is NOT written here - it is deferred to flush time so that
    /// the correct sorted-order index can be used. upstream sorts the flist
    /// after reading it from the batch file, so NDX values must reference
    /// sorted positions, not traversal order.
    ///
    /// The sum_head is likewise deferred: which blocks the body will reference
    /// is not known until the copy has run, so a whole-file placeholder is
    /// reserved here and [`Self::finalize_batch_file_delta`] overwrites it with
    /// the geometry the body was actually built against. Composing the head up
    /// front is what produced batches advertising `count=0` ahead of a body
    /// full of block matches, which upstream rejects at `receiver.c:414`.
    ///
    /// Must be called before any token writes for this file (before
    /// `capture_batch_whole_file` or inline delta token writes).
    ///
    /// upstream: sender.c:send_files() writes write_ndx_and_attrs() then
    /// write_sum_head() before delta tokens for each file.
    pub(crate) fn begin_batch_file_delta(
        &mut self,
    ) -> Result<(), crate::local_copy::LocalCopyError> {
        use std::io::Write;

        let delta_file = match self.batch_delta_buf.as_mut() {
            Some(f) => f,
            None => return Ok(()),
        };

        delta_file.get_mut().clear();
        delta_file.set_position(0);

        // upstream: token.c:387 - send_deflated_token() reinitialises the
        // deflate context at the start of every file, so the batch's per-file
        // token streams stay independently decodable.
        if let Some(encoder) = self.batch_token_encoder.as_mut() {
            encoder.reset();
        }

        // NDX is remapped to sorted order at flush time; record the
        // traversal index here.
        self.batch_current_delta_idx = self.batch_flist_index - 1;

        // upstream: rsync.c:383 - write iflags (u16 LE) for protocol >= 29.
        // ITEM_TRANSFER (0x8000) indicates delta data follows.
        let batch_writer_arc = self.options.get_batch_writer().expect("batch writer set on the write-batch path").clone();
        let proto = batch_writer_arc.lock().expect("batch writer mutex poisoned").config().protocol_version;
        if proto >= 29 {
            const ITEM_TRANSFER: u16 = 0x8000;
            delta_file
                .write_all(&ITEM_TRANSFER.to_le_bytes())
                .map_err(|e| {
                    crate::local_copy::LocalCopyError::io(
                        "write batch iflags",
                        std::path::PathBuf::new(),
                        e,
                    )
                })?;
        }

        // upstream: io.c:write_sum_head() - four i32 LE fields. Reserve the
        // slot with the whole-file (all-zero) head; the delta path replaces it
        // via `record_batch_delta_geometry` once the basis geometry is known,
        // and `finalize_batch_file_delta` patches the reserved bytes.
        self.batch_delta_sum_head = protocol::wire::SumHead::WHOLE_FILE;
        self.batch_delta_sum_head_offset = delta_file.get_ref().len();
        delta_file
            .write_all(&protocol::wire::SumHead::WHOLE_FILE.encode())
            .map_err(|e| {
                crate::local_copy::LocalCopyError::io(
                    "write batch sum_head",
                    std::path::PathBuf::new(),
                    e,
                )
            })?;

        Ok(())
    }

    /// Records the basis geometry the current file's delta body is being built
    /// against.
    ///
    /// Called by the delta executor once it has a basis signature, before any
    /// block-match token is emitted. Every subsequent match token is validated
    /// against this head, and the head is what
    /// [`Self::finalize_batch_file_delta`] writes into the reserved slot - so
    /// the head and the body it describes can only ever be built together.
    ///
    /// A file that never reaches the delta path keeps the whole-file head,
    /// matching upstream's `write_sum_head(f, NULL)`.
    pub(crate) fn record_batch_delta_geometry(&mut self, head: protocol::wire::SumHead) {
        if self.batch_delta_buf.is_some() {
            self.batch_delta_sum_head = head;
        }
    }

    /// Resolves a basis block index against the current file's recorded
    /// geometry, refusing to record a token the replaying receiver would
    /// reject.
    ///
    /// upstream: `receiver.c:414` aborts with `RERR_PROTOCOL` on an index that
    /// is not below `sum.count`. Checking here means a sum_head/body mismatch
    /// fails while writing the batch instead of silently shipping a file that
    /// crashes the peer replaying it.
    pub(crate) fn check_batch_block_index(
        &self,
        block_index: u32,
    ) -> Result<(), crate::local_copy::LocalCopyError> {
        let index = i32::try_from(block_index).unwrap_or(-1);
        self.batch_delta_sum_head
            .block_span(index)
            .map(|_| ())
            .map_err(|error| {
                crate::local_copy::LocalCopyError::io(
                    "write batch block match token",
                    std::path::PathBuf::new(),
                    std::io::Error::from(error),
                )
            })
    }

    /// Writes a token-format end marker and file checksum to the batch delta
    /// buffer for the current file, then moves the completed per-file data
    /// to `batch_delta_entries`.
    ///
    /// Each file's delta data is terminated by write_int(0), matching upstream
    /// `token.c:simple_send_token()` with token=-1. After the token end, a
    /// file-level MD5 checksum of `s2length` bytes (16) is written, computed
    /// over the source file contents.
    ///
    /// upstream: match.c:370 sum_init(xfer_sum_nni, checksum_seed) then
    /// sum_update on file content then sum_end(sender_file_sum). For MD5
    /// (protocol >= 30), sum_init ignores the seed - the checksum is plain
    /// MD5 of the file bytes.
    ///
    /// upstream: receiver.c:515 - read_buf(f_in, sender_file_sum, xfer_sum_len)
    pub(crate) fn finalize_batch_file_delta(
        &mut self,
        source: &std::path::Path,
    ) -> Result<(), crate::local_copy::LocalCopyError> {
        use std::io::{Read, Write};

        if self.batch_delta_buf.is_none() {
            return Ok(());
        }

        self.write_batch_token_end(source)?;

        // upstream: match.c:370-411 - compute MD5 of source file content.
        // For MD5 (protocol >= 30), sum_init() ignores checksum_seed.
        let file_sum = {
            let mut reader = std::fs::File::open(source).map_err(|e| {
                crate::local_copy::LocalCopyError::io(
                    "open source for batch checksum",
                    source.to_path_buf(),
                    e,
                )
            })?;
            let mut hasher = checksums::strong::Md5::new();
            let mut chunk = [0u8; 32 * 1024];
            loop {
                let n = reader.read(&mut chunk).map_err(|e| {
                    crate::local_copy::LocalCopyError::io(
                        "read source for batch checksum",
                        source.to_path_buf(),
                        e,
                    )
                })?;
                if n == 0 {
                    break;
                }
                hasher.update(&chunk[..n]);
            }
            hasher.finalize()
        };
        let Some(delta_file) = self.batch_delta_buf.as_mut() else {
            return Ok(());
        };
        delta_file.write_all(&file_sum).map_err(|e| {
            crate::local_copy::LocalCopyError::io(
                "write batch file checksum",
                std::path::PathBuf::new(),
                e,
            )
        })?;

        // Move the completed per-file data to batch_delta_entries.
        // The NDX will be written at flush time using the sort-order mapping.
        let mut data = std::mem::take(delta_file.get_mut());
        delta_file.set_position(0);

        // Patch the reserved sum_head with the geometry the body was built
        // against. Deferring it to here is what keeps the head and the tokens
        // it describes in agreement: the delta path records the basis geometry
        // as it matches blocks, and a whole-file body never records one, so it
        // keeps upstream's all-zero head.
        let head = std::mem::replace(
            &mut self.batch_delta_sum_head,
            protocol::wire::SumHead::WHOLE_FILE,
        );
        let start = self.batch_delta_sum_head_offset;
        let end = start + protocol::wire::SumHead::WIRE_LEN;
        let Some(slot) = data.get_mut(start..end) else {
            return Err(crate::local_copy::LocalCopyError::io(
                "write batch sum_head",
                source.to_path_buf(),
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "batch delta buffer is missing its reserved sum_head",
                ),
            ));
        };
        slot.copy_from_slice(&head.encode());

        let idx = self.batch_current_delta_idx;
        self.batch_delta_entries.push((idx, data));

        Ok(())
    }

    /// Captures whole-file content to the batch delta buffer as token-format
    /// literals.
    ///
    /// When batch mode is active and the transfer does not use delta encoding
    /// (new file, whole-file mode, or no basis), the entire file content must
    /// still be captured so that replay can reconstruct it.
    ///
    /// upstream: match.c:match_sums() writes literals for whole-file transfers.
    pub(crate) fn capture_batch_whole_file(
        &mut self,
        source: &std::path::Path,
        file_size: u64,
    ) -> Result<(), crate::local_copy::LocalCopyError> {
        if self.batch_delta_buf.is_none() {
            return Ok(());
        }

        let mut reader = std::fs::File::open(source).map_err(|e| {
            crate::local_copy::LocalCopyError::io(
                "open source for batch capture",
                source.to_path_buf(),
                e,
            )
        })?;

        let mut buf = vec![0u8; 32 * 1024]; // CHUNK_SIZE
        let mut remaining = file_size;

        while remaining > 0 {
            let to_read = (remaining as usize).min(buf.len());
            use std::io::Read;
            let n = reader.read(&mut buf[..to_read]).map_err(|e| {
                crate::local_copy::LocalCopyError::io(
                    "read source for batch capture",
                    source.to_path_buf(),
                    e,
                )
            })?;
            if n == 0 {
                break;
            }
            remaining = remaining.saturating_sub(n as u64);

            self.write_batch_literal_token(&buf[..n], source)?;
        }

        Ok(())
    }

    /// Flushes all per-file delta entries to the batch writer with
    /// sort-order-corrected NDX values, then writes NDX_DONE phase markers.
    ///
    /// upstream sorts the flist after reading it from the batch file
    /// (`flist_sort_and_clean()`), so NDX values in the delta stream must
    /// reference sorted positions, not traversal order. This method builds
    /// the traversal-to-sorted mapping from `batch_entry_sort_data` and
    /// writes each file's NDX using its sorted position.
    ///
    /// Must be called after `finalize_batch_flist()` to produce the correct
    /// upstream batch ordering: all flist entries first, then all file data.
    ///
    /// upstream: sender.c:send_files() writes NDX_DONE after all files in
    /// phase 1, then again after phase 2 redo (protocol >= 29).
    pub(crate) fn flush_batch_delta_to_batch(
        &mut self,
    ) -> Result<(), crate::local_copy::LocalCopyError> {
        if self.batch_delta_buf.is_none() {
            return Ok(());
        }

        let batch_writer_arc = match self.options.get_batch_writer() {
            Some(w) => w.clone(),
            None => return Ok(()),
        };

        // Build traversal-index to sorted-index mapping.
        // upstream: flist.c:flist_sort_and_clean() sorts after recv_file_list().
        // We replicate the same sort on our entry names to determine where each
        // traversal-order entry ends up in the sorted flist.
        let traversal_to_sorted = self.build_batch_sort_mapping();

        // Write each file's delta data with the correct sorted NDX.
        let codec = self
            .batch_ndx_codec
            .as_mut()
            .expect("batch_ndx_codec must exist when batch_delta_buf is set");

        let mut entries = std::mem::take(&mut self.batch_delta_entries);
        // Sort entries by their post-sort NDX so the delta stream is in
        // ascending NDX order, matching what upstream's recv_files() expects.
        entries.sort_by_key(|(traversal_idx, _)| {
            traversal_to_sorted
                .get(*traversal_idx as usize)
                .copied()
                .unwrap_or(*traversal_idx)
        });
        for (traversal_idx, data) in &entries {
            let sorted_idx = traversal_to_sorted
                .get(*traversal_idx as usize)
                .copied()
                .unwrap_or(*traversal_idx);

            let mut ndx_buf = Vec::with_capacity(4);
            protocol::codec::NdxCodec::write_ndx(codec, &mut ndx_buf, sorted_idx).map_err(
                |e| {
                    crate::local_copy::LocalCopyError::io(
                        "write batch NDX",
                        std::path::PathBuf::new(),
                        e,
                    )
                },
            )?;
            let mut writer_guard = batch_writer_arc.lock().expect("batch writer mutex poisoned");
            writer_guard.write_data(&ndx_buf).map_err(|e| {
                crate::local_copy::LocalCopyError::io(
                    "write batch NDX",
                    std::path::PathBuf::new(),
                    std::io::Error::other(e),
                )
            })?;
            writer_guard.write_data(data).map_err(|e| {
                crate::local_copy::LocalCopyError::io(
                    "write batch delta data",
                    std::path::PathBuf::new(),
                    std::io::Error::other(e),
                )
            })?;
        }

        // Write NDX_DONE markers for phase transitions.
        //
        // upstream: receiver.c:recv_files() reads NDX_DONEs to transition
        // phases. With INC_RECURSE (protocol >= 30), the first NDX_DONE
        // frees the flist and falls through to phase increment. For
        // protocol >= 29, max_phase=2, so recv_files needs 3 NDX_DONEs
        // to break (phase 0->1->2->3, breaks when phase > max_phase).
        // For protocol < 29, max_phase=1, needs 2 NDX_DONEs.
        let proto = batch_writer_arc.lock().expect("batch writer mutex poisoned").config().protocol_version;
        let ndx_done_count = if proto >= 29 { 3 } else { 2 };

        for _ in 0..ndx_done_count {
            let mut done_buf = Vec::with_capacity(4);
            protocol::codec::NdxCodec::write_ndx_done(codec, &mut done_buf).map_err(|e| {
                crate::local_copy::LocalCopyError::io(
                    "write batch NDX_DONE",
                    std::path::PathBuf::new(),
                    e,
                )
            })?;
            let mut writer_guard = batch_writer_arc.lock().expect("batch writer mutex poisoned");
            writer_guard.write_data(&done_buf).map_err(|e| {
                crate::local_copy::LocalCopyError::io(
                    "write batch NDX_DONE",
                    std::path::PathBuf::new(),
                    std::io::Error::other(e),
                )
            })?;
        }

        Ok(())
    }

    /// Builds a mapping from traversal-order index to sorted-order index.
    ///
    /// Replicates upstream's `flist_sort_and_clean()` sort order on the
    /// entry names collected during traversal. Returns a Vec where
    /// `result[traversal_index] = sorted_index`.
    fn build_batch_sort_mapping(&self) -> Vec<i32> {
        let n = self.batch_entry_sort_data.len();
        if n == 0 {
            return Vec::new();
        }

        // Build sort keys matching protocol::flist::sort logic.
        // Each key: (index, name_bytes, is_dir)
        let mut indices: Vec<usize> = (0..n).collect();
        indices.sort_by(|&a, &b| {
            let (ref name_a, is_dir_a) = self.batch_entry_sort_data[a];
            let (ref name_b, is_dir_b) = self.batch_entry_sort_data[b];
            batch_entry_compare(name_a, is_dir_a, name_b, is_dir_b)
        });

        // indices[sorted_pos] = traversal_index
        // We need the inverse: traversal_to_sorted[traversal_index] = sorted_pos
        let mut traversal_to_sorted = vec![0i32; n];
        for (sorted_pos, &traversal_idx) in indices.iter().enumerate() {
            traversal_to_sorted[traversal_idx] = sorted_pos as i32;
        }

        traversal_to_sorted
    }

    /// Increments the batch flist index counter.
    ///
    /// Called after each flist entry is captured to the batch file.
    pub(super) fn increment_batch_flist_index(&mut self) {
        self.batch_flist_index += 1;
    }

    /// Records sort metadata for a batch flist entry.
    ///
    /// Stores the entry name and directory flag in traversal order so that
    /// `flush_batch_delta_to_batch` can compute the same sort order that
    /// upstream's `flist_sort_and_clean()` produces after reading the batch.
    pub(super) fn record_batch_entry_sort_data(&mut self, name: &[u8], is_dir: bool) {
        self.batch_entry_sort_data.push((name.to_vec(), is_dir));
    }
}

/// Compares two batch flist entries for sorting, matching upstream's
/// `flist.c:f_name_cmp()` semantics.
///
/// Rules:
/// 1. "." always sorts first (root directory marker)
/// 2. Files sort before directories at the same level
/// 3. Directories are compared as if they have a trailing '/'
/// 4. Within the same type, sort by unsigned byte comparison
fn batch_entry_compare(
    name_a: &[u8],
    is_dir_a: bool,
    name_b: &[u8],
    is_dir_b: bool,
) -> std::cmp::Ordering {
    use std::cmp::Ordering;

    // "." always comes first
    match (name_a == b".", name_b == b".") {
        (true, true) => return Ordering::Equal,
        (true, false) => return Ordering::Less,
        (false, true) => return Ordering::Greater,
        (false, false) => {}
    }

    let last_slash_a = name_a.iter().rposition(|&b| b == b'/').unwrap_or(usize::MAX);
    let last_slash_b = name_b.iter().rposition(|&b| b == b'/').unwrap_or(usize::MAX);

    let mut i = 0;
    loop {
        let ch_a = if i < name_a.len() {
            name_a[i]
        } else if i == name_a.len() && is_dir_a {
            b'/'
        } else {
            0
        };

        let ch_b = if i < name_b.len() {
            name_b[i]
        } else if i == name_b.len() && is_dir_b {
            b'/'
        } else {
            0
        };

        let a_done = i > name_a.len() || (i == name_a.len() && !is_dir_a);
        let b_done = i > name_b.len() || (i == name_b.len() && !is_dir_b);

        if a_done && b_done {
            return Ordering::Equal;
        }
        if a_done {
            return Ordering::Less;
        }
        if b_done {
            return Ordering::Greater;
        }

        if ch_a != ch_b {
            let a_has_sep = last_slash_a != usize::MAX && last_slash_a >= i;
            let b_has_sep = last_slash_b != usize::MAX && last_slash_b >= i;

            let a_is_dir_here = a_has_sep || is_dir_a;
            let b_is_dir_here = b_has_sep || is_dir_b;

            match (a_is_dir_here, b_is_dir_here) {
                (true, false) => return Ordering::Greater,
                (false, true) => return Ordering::Less,
                _ => {}
            }

            return ch_a.cmp(&ch_b);
        }

        i += 1;
    }
}
