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
        let mut writer_guard = batch_writer_arc
            .lock()
            .expect("batch writer mutex poisoned");
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
            let cfg = batch_writer_arc
                .lock()
                .expect("batch writer mutex poisoned");
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

        let mut writer_guard = batch_writer_arc
            .lock()
            .expect("batch writer mutex poisoned");
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
        let batch_writer_arc = self
            .options
            .get_batch_writer()
            .expect("batch writer set on the write-batch path")
            .clone();
        let proto = batch_writer_arc
            .lock()
            .expect("batch writer mutex poisoned")
            .config()
            .protocol_version;
        if proto >= 29 {
            const ITEM_TRANSFER: u16 = 0x8000;
            self.batch_delta_iflags_offset = Some(delta_file.get_ref().len());
            delta_file
                .write_all(&ITEM_TRANSFER.to_le_bytes())
                .map_err(|e| {
                    crate::local_copy::LocalCopyError::io(
                        "write batch iflags",
                        std::path::PathBuf::new(),
                        e,
                    )
                })?;
        } else {
            self.batch_delta_iflags_offset = None;
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

    /// Patches `ITEM_IS_NEW` into the current file's reserved iflags word
    /// when the destination did not exist before this transfer.
    ///
    /// `begin_batch_file_delta()` reserves the word with the bare
    /// `ITEM_TRANSFER` bit because it runs before the destination has been
    /// stat'd; `copy_file()` calls this once that stat is resolved, before
    /// any token or `finalize_batch_file_delta()` call for the file. A no-op
    /// when the destination pre-existed, batch mode is inactive, or the
    /// protocol predates iflags (< 29, so `begin_batch_file_delta()` wrote no
    /// word to patch).
    ///
    /// Without this bit, a batch this writer produced itemizes as
    /// `>f.........` (unchanged) and contributes nothing to `--stats`
    /// "Number of created files" when replayed by an upstream
    /// `--read-batch` peer, instead of the correct `>f+++++++++` / counted
    /// creation.
    ///
    /// # Upstream Reference
    ///
    /// - `generator.c:583-584 itemize()` - `iflags |= ITEM_IS_NEW` when
    ///   `statret < 0` (destination absent).
    /// - `sender.c:468 write_ndx_and_attrs()` - re-emits that exact iflags
    ///   word to the peer reading the batch.
    /// - `sender.c:586,624` - `stats.created_files++` gated on
    ///   `iflags & ITEM_IS_NEW`.
    pub(crate) fn record_batch_is_new(&mut self, is_new: bool) {
        if !is_new {
            return;
        }
        let Some(offset) = self.batch_delta_iflags_offset else {
            return;
        };
        let Some(delta_file) = self.batch_delta_buf.as_mut() else {
            return;
        };
        const ITEM_IS_NEW: u16 = 0x2000;
        let buf = delta_file.get_mut();
        if let Some(slot) = buf.get_mut(offset..offset + 2) {
            let current = u16::from_le_bytes([slot[0], slot[1]]);
            slot.copy_from_slice(&(current | ITEM_IS_NEW).to_le_bytes());
        }
    }

    /// Records an itemize-only `NDX` + iflags entry into the batch delta
    /// stream for a created directory, symlink, or special file
    /// (device/FIFO).
    ///
    /// A regular-file transfer reserves its iflags word in
    /// [`Self::begin_batch_file_delta`] and carries `ITEM_TRANSFER` plus a
    /// sum_head and token body. A created non-regular entry moves no data, so
    /// upstream writes only its `NDX` and the 16-bit iflags word. This records
    /// that same two-byte entry, keyed to the traversal index the preceding
    /// `capture_batch_file_entry` assigned (`batch_flist_index - 1`, stable
    /// because the itemize record is emitted before any child entry is
    /// captured), so [`Self::flush_batch_delta_to_batch`] ships it under the
    /// sorted `NDX`.
    ///
    /// A no-op when batch mode is inactive, the protocol predates iflags
    /// (< 29), or `iflags` carries nothing the upstream emit gate keeps.
    ///
    /// # Upstream Reference
    ///
    /// - `generator.c:1480-1482` (directory), `:1605-1610` (symlink),
    ///   `:1679-1682` (device/special) - `itemize()` runs with a base of
    ///   `ITEM_LOCAL_CHANGE` (dirs) or `ITEM_LOCAL_CHANGE|ITEM_REPORT_CHANGE`
    ///   (symlinks/specials); `generator.c:583-584` ORs in `ITEM_IS_NEW` when
    ///   the destination is absent, and `generator.c:576-590` writes `NDX`
    ///   then the shortint iflags with no sum_head because `ITEM_TRANSFER` is
    ///   clear.
    /// - `receiver.c:726-786` reads the word in the `!(iflags & ITEM_TRANSFER)`
    ///   branch, logs the item, and bumps `stats.created_{dirs,symlinks,
    ///   devices,specials}` under the `ITEM_IS_NEW` guard.
    /// - `receiver.c:559-570 no_batched_update()` - a special needs this entry:
    ///   without it the replay aborts (exit 23) and the node is never created.
    ///   Dirs and symlinks are created from the flist regardless, but a missing
    ///   entry makes an upstream reader under-count them and drop their
    ///   `cd`/`cL` itemize rows.
    pub(crate) fn record_batch_metadata_item(&mut self, iflags: u16) {
        if self.batch_delta_buf.is_none() {
            return;
        }
        let Some(writer) = self.options.get_batch_writer() else {
            return;
        };
        let proto = writer
            .lock()
            .expect("batch writer mutex poisoned")
            .config()
            .protocol_version;
        if proto < 29 {
            return;
        }
        // upstream: rsync.h:258 SIGNIFICANT_ITEM_FLAGS + generator.c:582-583
        // emit gate - only these bits (or ITEM_REPORT_XATTR) reach the wire;
        // ITEM_LOCAL_CHANGE, ITEM_BASIS_TYPE_FOLLOWS and ITEM_XNAME_FOLLOWS do
        // not qualify an entry on their own.
        const ITEM_REPORT_XATTR: u16 = 1 << 8;
        const ITEM_BASIS_TYPE_FOLLOWS: u16 = 1 << 11;
        const ITEM_XNAME_FOLLOWS: u16 = 1 << 12;
        const ITEM_LOCAL_CHANGE: u16 = 1 << 14;
        const SIGNIFICANT_ITEM_FLAGS: u16 =
            !(ITEM_BASIS_TYPE_FOLLOWS | ITEM_XNAME_FOLLOWS | ITEM_LOCAL_CHANGE);
        if iflags & (SIGNIFICANT_ITEM_FLAGS | ITEM_REPORT_XATTR) == 0 {
            return;
        }
        let idx = self.batch_flist_index - 1;
        self.batch_delta_entries
            .push((idx, iflags.to_le_bytes().to_vec()));
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

        // upstream: hlink.c:113-194 match_gnums() - one member per hardlink
        // cluster is transferred and the rest are linked to it, so the batch
        // must carry the payload exactly once per cluster.
        let suppressed = self.batch_hlink_suppressed_indices(&traversal_to_sorted);

        // A cluster's single payload must be shipped under the NDX of its
        // sorted-first member, the one the replaying receiver flags
        // FLAG_HLINK_FIRST and transfers (hlink.c:113-194 match_gnums()); a
        // traversal-first recorder that sorts later would otherwise leave the
        // real leader without data.
        let cluster_leader_ndx = self.batch_hlink_cluster_leader_ndx(&traversal_to_sorted);
        let emit_ndx = |traversal_idx: i32| -> i32 {
            if let Some(Some(gnum)) = self.batch_entry_hlink_gnum.get(traversal_idx as usize)
                && let Some(&leader) = cluster_leader_ndx.get(gnum)
            {
                return leader;
            }
            traversal_to_sorted
                .get(traversal_idx as usize)
                .copied()
                .unwrap_or(traversal_idx)
        };

        // Resolve each kept entry to the NDX it will ship under before borrowing
        // the codec, so the sorted-first-leader remap above and the codec's
        // mutable borrow do not contend for `self`.
        let mut entries: Vec<(i32, Vec<u8>)> = std::mem::take(&mut self.batch_delta_entries)
            .into_iter()
            .filter(|(traversal_idx, _)| !suppressed.contains(traversal_idx))
            .map(|(traversal_idx, data)| (emit_ndx(traversal_idx), data))
            .collect();

        // Write each file's delta data with the correct sorted NDX.
        let codec = self
            .batch_ndx_codec
            .as_mut()
            .expect("batch_ndx_codec must exist when batch_delta_buf is set");
        // Sort entries by their post-sort NDX so the delta stream is in
        // ascending NDX order, matching what upstream's recv_files() expects.
        entries.sort_by_key(|(sorted_idx, _)| *sorted_idx);
        for (sorted_idx, data) in &entries {
            let sorted_idx = *sorted_idx;

            let mut ndx_buf = Vec::with_capacity(4);
            protocol::codec::NdxCodec::write_ndx(codec, &mut ndx_buf, sorted_idx).map_err(|e| {
                crate::local_copy::LocalCopyError::io(
                    "write batch NDX",
                    std::path::PathBuf::new(),
                    e,
                )
            })?;
            let mut writer_guard = batch_writer_arc
                .lock()
                .expect("batch writer mutex poisoned");
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
        let proto = batch_writer_arc
            .lock()
            .expect("batch writer mutex poisoned")
            .config()
            .protocol_version;
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
            let mut writer_guard = batch_writer_arc
                .lock()
                .expect("batch writer mutex poisoned");
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

    /// Returns the batch protocol version and whether the recorded stream
    /// flags carry `--hard-links`.
    ///
    /// The flag is taken from the header this same writer already wrote, for
    /// the reason spelled out on `build_batch_flist_writer`: the reader
    /// configures itself from the header, so deriving it from the live options
    /// here would let the two sides disagree.
    pub(super) fn batch_hard_link_context(&self) -> Option<(i32, bool)> {
        let guard = self.options.get_batch_writer()?.lock().ok()?;
        Some((
            guard.config().protocol_version,
            guard.stream_flags().preserve_hard_links,
        ))
    }

    /// Assigns the hardlink cluster of the flist entry about to be captured
    /// and records its group number in traversal order.
    ///
    /// Returns `None` for an entry outside any cluster. Otherwise returns the
    /// traversal index of the cluster leader together with whether this entry
    /// *is* that leader - the first sighting of the inode.
    ///
    /// upstream: `flist.c:1628-1635 make_file()` remembers `(st_dev, st_ino)`
    /// for every non-directory with `st_nlink > 1`, and
    /// `flist.c:599-625 send_file_entry()` turns a first sighting into
    /// `XMIT_HLINK_FIRST` (recording `first_ndx + ndx`) and every repeat into a
    /// follower carrying the leader's index.
    pub(super) fn assign_batch_hlink_group(
        &mut self,
        metadata: &fs::Metadata,
        preserve_hard_links: bool,
    ) -> Option<(i32, bool)> {
        let ndx = self.batch_flist_index;
        let assigned = batch_hlink_key(metadata, preserve_hard_links).map(|key| {
            let leader = *self.batch_hlink_first_ndx.entry(key).or_insert(ndx);
            (leader, leader == ndx)
        });
        self.batch_entry_hlink_gnum
            .push(assigned.map(|(leader, _)| leader));
        assigned
    }

    /// Maps each hardlink cluster to the sorted index of its sorted-first
    /// member, keyed by the cluster's group number.
    ///
    /// After the replaying receiver runs `flist_sort_and_clean()` and
    /// `match_hard_links()` (hlink.c:113-194), the *sorted-first* member of each
    /// cluster is the one flagged `FLAG_HLINK_FIRST`, so upstream transfers its
    /// payload and links every other member to it (`generator.c` ->
    /// `hard_link_check()`). The cluster's single recorded payload must therefore
    /// be emitted under that member's NDX, not under the NDX of whichever member
    /// the traversal happened to reach first - otherwise the sorted-first leader
    /// receives no data, is never written, and the cluster cannot be linked.
    fn batch_hlink_cluster_leader_ndx(&self, traversal_to_sorted: &[i32]) -> HashMap<i32, i32> {
        let mut leader_ndx: HashMap<i32, i32> = HashMap::new();
        for (traversal_idx, gnum) in self.batch_entry_hlink_gnum.iter().enumerate() {
            let Some(gnum) = gnum else { continue };
            let sorted = traversal_to_sorted
                .get(traversal_idx)
                .copied()
                .unwrap_or(traversal_idx as i32);
            leader_ndx
                .entry(*gnum)
                .and_modify(|best| {
                    if sorted < *best {
                        *best = sorted;
                    }
                })
                .or_insert(sorted);
        }
        leader_ndx
    }

    /// Traversal indices whose recorded delta data must not reach the batch
    /// because a hardlink cluster mate carries it instead.
    ///
    /// upstream: `hlink.c:113-194 match_gnums()` marks the *sorted-first*
    /// member of each group `FLAG_HLINK_FIRST`; `generator.c` then routes every
    /// other member through `hlink.c:300 hard_link_check()`, which returns 1
    /// (skip) so no data is ever requested for it. A batch that repeated the
    /// payload for each mate would therefore not be one upstream could have
    /// produced.
    ///
    /// Only members that actually recorded delta data are eligible to keep it:
    /// under plain `--write-batch` the live copy hardlinks the mates instead of
    /// reading them, so the payload sits on whichever member the walk reached
    /// first and dropping it would leave the cluster with no content at all.
    fn batch_hlink_suppressed_indices(&self, traversal_to_sorted: &[i32]) -> HashSet<i32> {
        let mut leaders: HashMap<i32, (i32, i32)> = HashMap::new();
        for (traversal_idx, _) in &self.batch_delta_entries {
            let Some(Some(gnum)) = self
                .batch_entry_hlink_gnum
                .get(*traversal_idx as usize)
                .copied()
            else {
                continue;
            };
            let sorted = traversal_to_sorted
                .get(*traversal_idx as usize)
                .copied()
                .unwrap_or(*traversal_idx);
            leaders
                .entry(gnum)
                .and_modify(|best| {
                    if sorted < best.0 {
                        *best = (sorted, *traversal_idx);
                    }
                })
                .or_insert((sorted, *traversal_idx));
        }

        let keep: HashSet<i32> = leaders.values().map(|&(_, idx)| idx).collect();
        self.batch_delta_entries
            .iter()
            .map(|(traversal_idx, _)| *traversal_idx)
            .filter(|traversal_idx| {
                matches!(
                    self.batch_entry_hlink_gnum.get(*traversal_idx as usize),
                    Some(Some(_))
                ) && !keep.contains(traversal_idx)
            })
            .collect()
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

    let last_slash_a = name_a
        .iter()
        .rposition(|&b| b == b'/')
        .unwrap_or(usize::MAX);
    let last_slash_b = name_b
        .iter()
        .rposition(|&b| b == b'/')
        .unwrap_or(usize::MAX);

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

/// The `(st_dev, st_ino)` key a batch flist entry contributes to hardlink
/// grouping, or `None` when the entry cannot belong to a cluster.
///
/// upstream: `flist.c:1628-1635 make_file()` - under protocol 28 and above the
/// candidate test is `!S_ISDIR(st.st_mode) && st.st_nlink > 1`.
#[cfg(unix)]
fn batch_hlink_key(metadata: &fs::Metadata, preserve_hard_links: bool) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;

    if !preserve_hard_links || metadata.is_dir() || metadata.nlink() <= 1 {
        return None;
    }
    Some((metadata.dev(), metadata.ino()))
}

/// Non-Unix platforms expose no inode identity through `fs::Metadata`, so no
/// entry can be recognised as a cluster member.
#[cfg(not(unix))]
fn batch_hlink_key(metadata: &fs::Metadata, preserve_hard_links: bool) -> Option<(u64, u64)> {
    let _ = (metadata, preserve_hard_links);
    None
}
