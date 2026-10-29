/// upstream: rsync.h ITEM_REPORT_XATTR - set when the entry's xattrs differ
/// from the destination's, promising the value block after the iflags.
const ITEM_REPORT_XATTR: u16 = 1 << 8;

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
        if self.batch_inc_recurse() {
            return self.write_incremental_batch_flist();
        }
        let flist_writer = match self.batch_flist_writer.as_ref() {
            Some(w) => w,
            None => return Ok(()),
        };

        // upstream: flist.c:3024-3031 - a nonzero io_error rides the end
        // marker only for a peer with a safe file list; write_end() applies
        // that gate.
        let io_error = self.batch_flist_io_error();
        let mut buf = Vec::with_capacity(4);
        flist_writer
            .write_end(&mut buf, (io_error != 0).then_some(io_error))
            .map_err(|e| {
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

    /// Writes the uid/gid ID lists to the batch file.
    ///
    /// upstream: uidlist.c:send_id_lists() - without INC_RECURSE, ID lists
    /// are written between the flist end marker and the delta data, naming
    /// every owner and group the file list carried (inline names ride the
    /// entries only under INC_RECURSE, flist.c:791).
    ///
    /// upstream: flist.c:3063 - `if (numeric_ids <= 0 && !inc_recurse)
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

        // upstream: flist.c:3063 - `if (numeric_ids <= 0 && !inc_recurse)
        // send_id_lists(f)`. Under --numeric-ids no id-lists are emitted, so the
        // reader (reader/flist.rs) must find none. numeric_ids is not a stream
        // flag; it is carried in the batch config from the invocation.
        if numeric_ids {
            return Ok(());
        }

        // upstream: flist.c:3063 - skip send_id_lists() under INC_RECURSE.
        if compat_flags.is_some_and(|cf| {
            protocol::CompatibilityFlags::from_bits(cf as u32)
                .contains(protocol::CompatibilityFlags::INC_RECURSE)
        }) {
            return Ok(());
        }

        // upstream: uidlist.c:send_id_lists() - the uid list is emitted only
        // when (preserve_uid || preserve_acls), and the gid list only when
        // (preserve_gid || preserve_acls). The matching reader gates on the
        // same predicates, so emitting a list it does not expect would drift
        // the stream cursor and corrupt the subsequent NDX reads. Each list
        // names the file owners in send order (flist.c:787-797 add_uid() only
        // under preserve_uid/preserve_gid), then its terminator - followed,
        // under CF_ID0_NAMES, by the name of id 0 (uidlist.c:398-405).
        let send_uid_list = preserve_uid || preserve_acls;
        let send_gid_list = preserve_gid || preserve_acls;
        if !send_uid_list && !send_gid_list {
            return Ok(());
        }
        let id0_names = compat_flags.is_some_and(|cf| {
            protocol::CompatibilityFlags::from_bits(cf as u32)
                .contains(protocol::CompatibilityFlags::ID0_NAMES)
        });

        let mut buf = Vec::new();
        let [mut owner_uids, mut owner_gids] = std::mem::take(&mut self.batch_owner_ids);
        if !preserve_uid {
            owner_uids.clear();
        }
        if !preserve_gid {
            owner_gids.clear();
        }
        if preserve_acls {
            self.add_batch_acl_named_ids([&mut owner_uids, &mut owner_gids]);
        }
        let lists = [
            (
                send_uid_list,
                owner_uids,
                "write batch uid list",
                metadata::id_lookup::lookup_user_name_cached as fn(u32) -> _,
            ),
            (
                send_gid_list,
                owner_gids,
                "write batch gid list",
                metadata::id_lookup::lookup_group_name_cached,
            ),
        ];
        for (send, owners, action, lookup) in lists {
            if !send {
                continue;
            }
            let id_error =
                |e| crate::local_copy::LocalCopyError::io(action, std::path::PathBuf::new(), e);
            let mut list = protocol::idlist::IdList::new();
            for id in owners {
                // upstream: uidlist.c:388-394 - id 0 is named only by the
                // CF_ID0_NAMES trailer below.
                if id != 0 {
                    let name = metadata::id_lookup::no_id_unless_converter_failed(lookup(id))
                        .map_err(id_error)?;
                    list.add_id(id, name);
                }
            }
            if id0_names {
                // upstream: uidlist.c:400-402 - uid_to_user(0)/gid_to_group(0).
                let name = metadata::id_lookup::no_id_unless_converter_failed(lookup(0))
                    .map_err(id_error)?;
                list.add_id(0, name);
            }
            list.write(&mut buf, id0_names, proto as u8)
                .map_err(id_error)?;
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

    /// Appends the user and group ids the written ACLs name to the owner
    /// lists, each id once.
    ///
    /// upstream: `acls.c:821-826 send_ida_entries()` - `add_uid()`/`add_gid()`
    /// every named entry, so the replaying receiver maps it through the same
    /// id lists as the file owners (`uidlist.c:483-484 match_acl_ids()`).
    fn add_batch_acl_named_ids(&self, ids: [&mut Vec<u32>; 2]) {
        let Some(writer) = self.batch_flist_writer.as_ref() else {
            return;
        };
        let [uids, gids] = ids;
        for ida in writer
            .acl_cache()
            .iter_acls()
            .flat_map(|acl| acl.names.iter())
        {
            let list = if ida.is_user() {
                &mut *uids
            } else {
                &mut *gids
            };
            if !list.contains(&ida.id) {
                list.push(ida.id);
            }
        }
    }

    /// Reports whether the batch header records `--acls`, so each flist entry
    /// carries its source ACL.
    pub(crate) fn batch_preserve_acls(&self) -> bool {
        self.options.get_batch_writer().is_some_and(|w| {
            w.lock()
                .expect("batch writer mutex poisoned")
                .stream_flags()
                .preserve_acls
        })
    }

    /// Reports whether the batch header records `--xattrs`, so each flist
    /// entry carries its source xattrs.
    pub(crate) fn batch_preserve_xattrs(&self) -> bool {
        self.options.get_batch_writer().is_some_and(|w| {
            w.lock()
                .expect("batch writer mutex poisoned")
                .stream_flags()
                .preserve_xattrs
        })
    }

    /// Sets the `ITEM_REPORT_XATTR` value block for the entry just captured.
    ///
    /// upstream: `sender.c:482-484 write_ndx_and_attrs()` - the block follows
    /// the entry's iflags whenever they carry `ITEM_REPORT_XATTR`.
    pub(crate) fn set_batch_xattr_report(&mut self, report: Option<Vec<u8>>) {
        self.batch_xattr_report = report;
    }

    /// Records the owner and group of a flat-list entry for the id lists.
    ///
    /// upstream: `flist.c:787-797 send_file_entry()` - `add_uid()`/`add_gid()`
    /// keep the first sighting of each id (uidlist.c:340-371).
    pub(crate) fn note_batch_entry_owner(&mut self, uid: Option<u32>, gid: Option<u32>) {
        for (ids, id) in self.batch_owner_ids.iter_mut().zip([uid, gid]) {
            if let Some(id) = id
                && !ids.contains(&id)
            {
                ids.push(id);
            }
        }
    }

    /// Returns the `io_error` bits the file list reports to the replaying peer.
    ///
    /// upstream: flist.c:3024 / :3066 - `ignore_errors` zeroes the value; a
    /// walk error is `IOERR_GENERAL`.
    pub(in crate::local_copy) fn batch_flist_io_error(&self) -> i32 {
        if !self.batch_flist_error_dirs.is_empty() && !self.options.ignore_errors_enabled() {
            protocol::io_error::IOERR_GENERAL
        } else {
            0
        }
    }

    /// Names the directory whose listing the walk reads next (`None`: the
    /// listing is done, so later errors belong to the top-level list).
    pub(crate) fn set_batch_listing_dir(&mut self, dir: Option<&std::path::Path>) {
        if self.batch_delta_buf.is_some() {
            self.batch_listing_dir = dir.unwrap_or(std::path::Path::new(".")).to_path_buf();
        }
    }

    /// Attributes a sender-side walk error to the listing being read.
    pub(in crate::local_copy) fn note_batch_flist_error(&mut self) {
        if self.batch_delta_buf.is_some()
            && self.batch_flist_error_dirs.last() != Some(&self.batch_listing_dir)
        {
            self.batch_flist_error_dirs
                .push(self.batch_listing_dir.clone());
        }
    }

    /// Writes the protocol 28-29 `io_error` word that follows the id lists.
    ///
    /// upstream: flist.c:3065-3067 - `if (protocol_version < 30)
    /// write_int(f, ignore_errors ? 0 : io_error)`. Without it a 3.x reader
    /// takes the first file's NDX as the error word and every later read is
    /// misaligned ("File-list index ... not in -1 - N").
    pub(crate) fn write_batch_flist_io_error(
        &mut self,
    ) -> Result<(), crate::local_copy::LocalCopyError> {
        let Some(batch_writer_arc) = self.options.get_batch_writer().cloned() else {
            return Ok(());
        };
        let io_error = self.batch_flist_io_error();
        let mut writer_guard = batch_writer_arc
            .lock()
            .expect("batch writer mutex poisoned");
        if writer_guard.config().protocol_version >= 30 {
            return Ok(());
        }
        writer_guard
            .write_data(&io_error.to_le_bytes())
            .map_err(|e| {
                crate::local_copy::LocalCopyError::io(
                    "write batch io_error",
                    std::path::PathBuf::new(),
                    std::io::Error::other(e),
                )
            })
    }

    /// Appends a literal token for the current file to the batch delta buffer.
    ///
    /// upstream: `token.c:1068 send_token()` dispatches on `do_compression`.
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
    /// history (`token.c:475-510`) so the receiver's inflate dictionary stays in
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
    /// `token.c:468-474 send_deflated_token()` writes `END_FLAG` after draining
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
    /// full of block matches, which upstream rejects at `receiver.c:587-589`.
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
        self.batch_report_slot = None;

        // upstream: token.c:390 - send_deflated_token() reinitialises the
        // deflate context at the start of every file, so the batch's per-file
        // token streams stay independently decodable.
        if let Some(encoder) = self.batch_token_encoder.as_mut() {
            encoder.reset();
        }

        // NDX is remapped to sorted order at flush time; record the
        // traversal index here.
        self.batch_current_delta_idx = self.batch_flist_index - 1;

        // upstream: rsync.c:384 - write iflags (u16 LE) for protocol >= 29.
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
            let xattr_report = self.batch_xattr_report.take();
            let iflags = if xattr_report.is_some() {
                ITEM_TRANSFER | ITEM_REPORT_XATTR
            } else {
                ITEM_TRANSFER
            };
            self.batch_delta_iflags_offset = Some(delta_file.get_ref().len());
            delta_file
                .write_all(&iflags.to_le_bytes())
                .and_then(|()| delta_file.write_all(xattr_report.as_deref().unwrap_or_default()))
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
    /// - `sender.c:469 write_ndx_and_attrs()` - re-emits that exact iflags
    ///   word to the peer reading the batch.
    /// - `sender.c:587,625` - `stats.created_files++` gated on
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
    /// - `generator.c:1869-1871` (directory), `:2004-2009` (symlink),
    ///   `:1679-1682` (device/special) - `itemize()` runs with a base of
    ///   `ITEM_LOCAL_CHANGE` (dirs) or `ITEM_LOCAL_CHANGE|ITEM_REPORT_CHANGE`
    ///   (symlinks/specials); `generator.c:583-584` ORs in `ITEM_IS_NEW` when
    ///   the destination is absent, and `generator.c:588-596` writes `NDX`
    ///   then the shortint iflags with no sum_head because `ITEM_TRANSFER` is
    ///   clear.
    /// - `receiver.c:916-940` reads the word in the `!(iflags & ITEM_TRANSFER)`
    ///   branch, logs the item, and bumps `stats.created_{dirs,symlinks,
    ///   devices,specials}` under the `ITEM_IS_NEW` guard.
    /// - `receiver.c:738-751 no_batched_update()` - a special needs this entry:
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
        // upstream: rsync.h:265 SIGNIFICANT_ITEM_FLAGS + generator.c:588-589
        // emit gate - only these bits (or ITEM_REPORT_XATTR) reach the wire;
        // ITEM_LOCAL_CHANGE, ITEM_BASIS_TYPE_FOLLOWS and ITEM_XNAME_FOLLOWS do
        // not qualify an entry on their own.
        const ITEM_BASIS_TYPE_FOLLOWS: u16 = 1 << 11;
        const ITEM_XNAME_FOLLOWS: u16 = 1 << 12;
        const ITEM_LOCAL_CHANGE: u16 = 1 << 14;
        const SIGNIFICANT_ITEM_FLAGS: u16 =
            !(ITEM_BASIS_TYPE_FOLLOWS | ITEM_XNAME_FOLLOWS | ITEM_LOCAL_CHANGE);
        // The bit promises the value block that follows the iflags, so only the
        // capture, which built that block, may set it.
        let xattr_report = self.batch_xattr_report.take();
        let iflags = match xattr_report {
            Some(_) => iflags | ITEM_REPORT_XATTR,
            None => iflags & !ITEM_REPORT_XATTR,
        };
        if iflags & (SIGNIFICANT_ITEM_FLAGS | ITEM_REPORT_XATTR) == 0 {
            return;
        }
        let idx = self.batch_flist_index - 1;
        let mut record = iflags.to_le_bytes().to_vec();
        record.extend(xattr_report.unwrap_or_default());
        self.batch_delta_entries.push((idx, record));
    }

    /// Resolves a basis block index against the current file's recorded
    /// geometry, refusing to record a token the replaying receiver would
    /// reject.
    ///
    /// upstream: `receiver.c:587-589` aborts with `RERR_PROTOCOL` on an index that
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
    /// 16-byte file-level checksum of the source file contents is written:
    /// MD5 for protocol 30+, seeded MD4 below it.
    ///
    /// upstream: match.c:403 sum_init(xfer_sum_nni, checksum_seed) then
    /// sum_update on file content then sum_end(sender_file_sum). For MD5
    /// (protocol >= 30), sum_init ignores the seed; protocol 28-29 use
    /// CSUM_MD4_OLD (checksum.c:142-144), whose sum_init() feeds the 4-byte LE
    /// seed ahead of the data (checksum.c:622-628).
    ///
    /// upstream: receiver.c:687 - read_buf(f_in, sender_file_sum, xfer_sum_len)
    pub(crate) fn finalize_batch_file_delta(
        &mut self,
        source: &std::path::Path,
    ) -> Result<(), crate::local_copy::LocalCopyError> {
        use std::io::{Read, Write};

        if self.batch_delta_buf.is_none() {
            return Ok(());
        }

        self.write_batch_token_end(source)?;

        let (proto, seed) = {
            let writer = self
                .options
                .get_batch_writer()
                .expect("batch writer set on the write-batch path")
                .lock()
                .expect("batch writer mutex poisoned");
            (
                writer.config().protocol_version,
                writer.config().checksum_seed,
            )
        };
        // upstream: match.c:403-452 - sum the source file content.
        let file_sum = {
            let mut reader = std::fs::File::open(source).map_err(|e| {
                crate::local_copy::LocalCopyError::io(
                    "open source for batch checksum",
                    source.to_path_buf(),
                    e,
                )
            })?;
            let mut hasher = if proto < 30 {
                let mut md4 = checksums::strong::Md4::new();
                md4.update(&seed.to_le_bytes());
                FileSum::Md4(md4)
            } else {
                FileSum::Md5(checksums::strong::Md5::new())
            };
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
        self.batch_report_slot = self
            .batch_delta_iflags_offset
            .map(|offset| (self.batch_delta_entries.len() - 1, offset));

        Ok(())
    }

    /// ORs a regular file's `ITEM_REPORT_*` bits into the iflags word of the
    /// delta entry just completed for it. A no-op when no such entry waits
    /// (batch mode inactive, protocol < 29, or no transfer for this file).
    ///
    /// upstream: `generator.c:517-548 itemize()` sets these bits in the word
    /// `sender.c:469 write_ndx_and_attrs()` echoes into the batch.
    pub(crate) fn record_batch_transfer_report(&mut self, report: u16) {
        let Some((entry, offset)) = self.batch_report_slot.take() else {
            return;
        };
        // `begin_batch_file_delta` already decided ITEM_REPORT_XATTR together
        // with the value block it wrote after the word.
        let report = report & !ITEM_REPORT_XATTR;
        if let Some(word) = self
            .batch_delta_entries
            .get_mut(entry)
            .and_then(|(_, data)| data.get_mut(offset..offset + 2))
        {
            let iflags = u16::from_le_bytes([word[0], word[1]]) | report;
            word.copy_from_slice(&iflags.to_le_bytes());
        }
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
        let traversal_to_sorted = match &self.batch_traversal_to_ndx {
            Some(map) => map.clone(),
            None => self.build_batch_sort_mapping(),
        };

        // upstream: hlink.c:113-208 match_gnums() - one member per hardlink
        // cluster is transferred and the rest are linked to it, so the batch
        // must carry the payload exactly once per cluster.
        let suppressed = self.batch_hlink_suppressed_indices(&traversal_to_sorted);

        // A cluster's single payload must be shipped under the NDX of its
        // sorted-LAST member, the one match_gnums() flags FLAG_HLINK_LAST and
        // whose transfer finish_hard_link() completes before linking every other
        // member to it (hlink.c:113-208, hlink.c:517-586). Shipping under any
        // other member leaves the real data-holder without data and breaks
        // cross-implementation replay (task #18).
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
        // the codec, so the sorted-last-leader remap above and the codec's
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
        // upstream: sender.c:515-547 - under INC_RECURSE the sub-lists go out
        // between file-data records, as far ahead as the lookahead allows.
        let mut pending_sublists = std::mem::take(&mut self.batch_pending_sublists).into_iter();
        let schedule = self.batch_list_plan.as_ref().map(|plan| {
            let ndxs: Vec<i32> = entries.iter().map(|(ndx, _)| *ndx).collect();
            plan.lookahead_schedule(&ndxs)
        });
        let mut sublists_sent = 0usize;
        let write_raw = |bytes: &[u8], what: &'static str| {
            batch_writer_arc
                .lock()
                .expect("batch writer mutex poisoned")
                .write_data(bytes)
                .map_err(|e| {
                    crate::local_copy::LocalCopyError::io(
                        what,
                        std::path::PathBuf::new(),
                        std::io::Error::other(e),
                    )
                })
        };
        for (record, (sorted_idx, data)) in entries.iter().enumerate() {
            let sorted_idx = *sorted_idx;
            if let Some(schedule) = &schedule {
                for chunk in pending_sublists
                    .by_ref()
                    .take(schedule[record] - sublists_sent)
                {
                    write_raw(&chunk, "write batch sub-list")?;
                }
                sublists_sent = schedule[record];
            }

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

        for chunk in pending_sublists {
            write_raw(&chunk, "write batch sub-list")?;
        }

        // Write NDX_DONE markers for phase transitions.
        //
        // upstream: receiver.c:recv_files() reads NDX_DONEs to transition
        // phases. For protocol >= 29, max_phase=2, so recv_files needs 3
        // NDX_DONEs to break (phase 0->1->2->3, breaks when phase > max_phase).
        // For protocol < 29, max_phase=1, needs 2 NDX_DONEs.
        //
        // upstream: sender.c:525-545 - under INC_RECURSE every NDX_DONE before
        // the last list only frees a list, so each extra list adds one.
        let proto = batch_writer_arc
            .lock()
            .expect("batch writer mutex poisoned")
            .config()
            .protocol_version;
        let ndx_done_count = if proto >= 29 {
            self.batch_list_count + 2
        } else {
            2
        };

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
    /// upstream: `flist.c:1853-1860 make_file()` remembers `(st_dev, st_ino)`
    /// for every non-directory with `st_nlink > 1`, and
    /// `flist.c:824-850 send_file_entry()` turns a first sighting into
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

    /// Maps each hardlink cluster to the sorted index of its sorted-LAST
    /// member, keyed by the cluster's group number.
    ///
    /// After the replaying receiver runs `flist_sort_and_clean()` and
    /// `match_hard_links()` (hlink.c:113-208), the *sorted-last* member of each
    /// cluster is the one flagged `FLAG_HLINK_LAST`; upstream's generator
    /// transfers that member and `finish_hard_link()` (hlink.c:517-586) then
    /// links every other member to it. The cluster's single recorded payload
    /// must therefore be emitted under the sorted-last member's NDX, so a
    /// replaying receiver (upstream or oc) finds the data on the member it
    /// materialises and links the rest - including the sorted-first member - to
    /// it. Shipping under the sorted-first member instead is what broke
    /// cross-implementation replay (task #18).
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
                    if sorted > *best {
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
    /// upstream: `hlink.c:113-208 match_gnums()` marks the *sorted-first*
    /// member of each group `FLAG_HLINK_FIRST`; `generator.c` then routes every
    /// other member through `hlink.c:314 hard_link_check()`, which returns 1
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

    /// Reports whether the batch header records `CF_INC_RECURSE`, so the file
    /// list is written in the incremental layout.
    pub(crate) fn batch_inc_recurse(&self) -> bool {
        self.options.get_batch_writer().is_some_and(|w| {
            w.lock()
                .expect("batch writer mutex poisoned")
                .config()
                .compat_flags
                .is_some_and(|cf| {
                    protocol::CompatibilityFlags::from_bits(cf as u32)
                        .contains(protocol::CompatibilityFlags::INC_RECURSE)
                })
        })
    }

    /// Holds an incremental-layout flist entry, with its source ACL when the
    /// batch records `--acls`, until the walk ends.
    pub(crate) fn hold_batch_entry(
        &mut self,
        entry: protocol::flist::FileEntry,
        acl: Option<crate::local_copy::context::BatchSourceAcl>,
    ) {
        self.batch_pending_entries.push(entry);
        if acl.is_some() {
            self.batch_pending_acls
                .resize(self.batch_pending_entries.len() - 1, None);
            self.batch_pending_acls.push(acl);
        }
    }

    /// Writes the held entries as upstream's incremental file lists: the top
    /// level, then one sub-list per directory announced as
    /// `NDX_FLIST_OFFSET - dir_ndx`, then `NDX_FLIST_EOF`.
    ///
    /// A recording sends every sub-list before any file data, as 3.5.1 does
    /// for a tree inside its `MIN_FILECNT_LOOKAHEAD`, so the replaying
    /// receiver has each list before an NDX names one of its files. Each list
    /// goes out sorted, so its send position is its sorted position and
    /// `ndx_start + position` is both the NDX a file's data goes under and the
    /// index a hardlink follower names its leader by. Upstream sends each list
    /// in readdir order and sorts it afterwards, so its list bytes can differ
    /// in order while every NDX agrees.
    ///
    /// # Upstream Reference
    ///
    /// - `flist.c:send_extra_file_list()` - per-directory sub-lists and
    ///   `NDX_FLIST_EOF` (flist.c:2662-2730)
    /// - `flist.c:3503,3511` - `ndx_start` of each list
    /// - `flist.c:824-850 send_file_entry()` - the first member sent leads its
    ///   hardlink group and a follower carries the leader's NDX
    fn write_incremental_batch_flist(&mut self) -> Result<(), crate::local_copy::LocalCopyError> {
        let err = |e: std::io::Error| {
            crate::local_copy::LocalCopyError::io(
                "write batch incremental file list",
                std::path::PathBuf::new(),
                e,
            )
        };
        let traversal_to_sorted = self.build_batch_sort_mapping();
        let mut sorted_to_traversal = vec![0usize; traversal_to_sorted.len()];
        for (traversal, &sorted) in traversal_to_sorted.iter().enumerate() {
            sorted_to_traversal[sorted as usize] = traversal;
        }
        let mut entries: Vec<Option<protocol::flist::FileEntry>> =
            std::mem::take(&mut self.batch_pending_entries)
                .into_iter()
                .map(Some)
                .collect();
        let mut acls = std::mem::take(&mut self.batch_pending_acls);
        let sorted: Vec<protocol::flist::FileEntry> = sorted_to_traversal
            .iter()
            .map(|&t| entries[t].take().expect("each entry is sorted once"))
            .collect();
        let plan = protocol::flist::SubListPlan::new(&sorted);
        if let Some(&orphan) = plan.orphans.first() {
            return Err(err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "parent directory is missing from the file list: {}",
                    sorted[orphan].path().display()
                ),
            )));
        }
        let starts = plan.ndx_starts();
        let lists = std::iter::once((None, plan.initial.as_slice())).chain(
            plan.sublists
                .iter()
                .map(|s| (Some(s.parent_dir_ndx), s.members.as_slice())),
        );
        let mut traversal_to_ndx = vec![0i32; sorted.len()];
        for (s, ndx) in plan.wire_ndx(&sorted).into_iter().enumerate() {
            traversal_to_ndx[sorted_to_traversal[s]] = ndx;
        }
        // upstream: flist.c:2683-2690 - a list whose scan raised an error ends
        // with it; the transfer root's listing is the initial list, and a
        // directory without a sub-list falls back to it rather than lose it.
        let io_error = self.batch_flist_io_error();
        let mut carries_error = vec![false; plan.list_count()];
        if io_error != 0 {
            for dir in &self.batch_flist_error_dirs {
                let list = plan
                    .sublists
                    .iter()
                    .position(|s| sorted[s.parent].path() == dir.as_path())
                    .map_or(0, |k| k + 1);
                carries_error[list] = true;
            }
        }

        let flist_writer = self
            .batch_flist_writer
            .as_mut()
            .expect("batch flist writer exists on the write-batch path");
        let codec = self
            .batch_ndx_codec
            .as_mut()
            .expect("batch NDX codec exists on the write-batch path");
        // One encoded chunk per list, in send order; the initial list goes out
        // now and the sub-lists wait for the lookahead schedule.
        let mut chunks: Vec<Vec<u8>> = Vec::with_capacity(plan.list_count());
        // Hardlink group (keyed by the traversal index of the member captured
        // first) to the wire NDX of the member sent first.
        let mut group_leader: std::collections::HashMap<u32, i32> =
            std::collections::HashMap::new();
        for (list, ((parent_dir_ndx, members), &start)) in lists.zip(&starts).enumerate() {
            let mut buf = Vec::new();
            if let Some(dir_ndx) = parent_dir_ndx {
                protocol::codec::NdxCodec::write_ndx(
                    codec,
                    &mut buf,
                    protocol::codec::NDX_FLIST_OFFSET - dir_ndx,
                )
                .map_err(err)?;
            }
            flist_writer.set_first_ndx(start);
            for &s in members {
                let mut entry = sorted[s].clone();
                let traversal = sorted_to_traversal[s];
                if let Some((access, default)) = acls.get_mut(traversal).and_then(Option::take) {
                    flist_writer.set_pending_acl(access, default);
                }
                if let Some(idx) = entry.hardlink_idx() {
                    // upstream: flist.c:824-850 - the group's first member in
                    // send order is its leader (XMIT_HLINK_FIRST) and every
                    // later one names that NDX; a receiver rejects a follower
                    // whose leader it has not yet read (flist.c:1103-1107).
                    // The walk captured another member first, so re-elect.
                    let group = if idx == u32::MAX {
                        traversal as u32
                    } else {
                        idx
                    };
                    match group_leader.entry(group) {
                        std::collections::hash_map::Entry::Vacant(slot) => {
                            slot.insert(traversal_to_ndx[traversal]);
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
                .write_end(&mut buf, carries_error[list].then_some(io_error))
                .map_err(err)?;
            chunks.push(buf);
        }
        // upstream: flist.c:2718-2719 - NDX_FLIST_EOF follows the last sub-list
        // in the same send_extra_file_list() call (flist.c:3090-3091 when the
        // initial list is the only one).
        let last = chunks
            .last_mut()
            .expect("the initial list is always planned");
        protocol::codec::NdxCodec::write_ndx(codec, last, protocol::codec::NDX_FLIST_EOF)
            .map_err(err)?;

        self.batch_list_count = plan.list_count();
        self.batch_traversal_to_ndx = Some(traversal_to_ndx);
        let initial = chunks.remove(0);
        self.batch_pending_sublists = chunks;
        self.batch_list_plan = Some(plan);
        let batch_writer_arc = match self.options.get_batch_writer() {
            Some(w) => w.clone(),
            None => return Ok(()),
        };
        batch_writer_arc
            .lock()
            .expect("batch writer mutex poisoned")
            .write_data(&initial)
            .map_err(|e| err(std::io::Error::other(e)))
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
/// upstream: `flist.c:1853-1860 make_file()` - under protocol 28 and above the
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

/// The whole-file checksum a batch records after each file's tokens.
enum FileSum {
    Md4(checksums::strong::Md4),
    Md5(checksums::strong::Md5),
}

impl FileSum {
    fn update(&mut self, data: &[u8]) {
        match self {
            Self::Md4(h) => h.update(data),
            Self::Md5(h) => h.update(data),
        }
    }

    fn finalize(self) -> [u8; 16] {
        match self {
            Self::Md4(h) => h.finalize(),
            Self::Md5(h) => h.finalize(),
        }
    }
}
