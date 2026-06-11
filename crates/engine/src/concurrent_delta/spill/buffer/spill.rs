//! Spill-to-disk path for [`SpillableReorderBuffer`]: candidate selection,
//! per-item and whole-batch writers, RSS pressure trigger, payload codec
//! wrapper, raw record writer, and directory recreation.

use std::fs;
use std::io::{self, SeekFrom};

use super::super::tempfile::open_backend;
use super::super::{SpillCodec, SpillCompression, SpillError, SpillGranularity, rss};
#[cfg(feature = "spill-compression")]
use super::SPILL_TAG_ZSTD;
use super::{HOT_ZONE, SPILL_TAG_RAW, SpillableReorderBuffer};

/// Emits the one-shot spill-activation warning the first time the reorder
/// buffer spills to disk. Subsequent calls with `already_warned = true` are
/// no-ops so the operator sees the diagnostic exactly once per transfer.
///
/// The message goes to stderr via [`eprintln!`] so default builds always
/// surface it, matching the workspace convention for unconditional operator
/// warnings (see `metadata::acl_stub`,
/// `core::client::remote::ssh_transfer`). When the optional `tracing`
/// feature is enabled the same warning is mirrored to the structured log
/// at the `warn` level.
fn emit_spill_warning(
    spill_dir: Option<&std::path::Path>,
    threshold: usize,
    already_warned: bool,
) -> bool {
    if already_warned {
        return true;
    }
    let dir_display = spill_dir
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "<system tempdir>".to_string());
    eprintln!(
        "warning: reorder buffer spilled to disk during transfer; \
         this indicates either an adversarial chunk ordering or undersized \
         ring capacity. spilled to {dir_display} (threshold={threshold} bytes). \
         Use OC_RSYNC_SPILL_DIR / OC_RSYNC_SPILL_THRESHOLD_BYTES to tune. \
         (one-time warning per transfer)"
    );
    #[cfg(feature = "tracing")]
    tracing::warn!(
        spill_dir = %dir_display,
        threshold_bytes = threshold,
        "reorder buffer spilled to disk during transfer; \
         use OC_RSYNC_SPILL_DIR / OC_RSYNC_SPILL_THRESHOLD_BYTES to tune \
         (one-time warning per transfer)"
    );
    true
}

impl<T: SpillCodec> SpillableReorderBuffer<T> {
    /// Spills the highest-sequence items to disk until memory usage drops
    /// below the threshold.
    ///
    /// Items close to `next_expected` are preserved in memory when possible
    /// (the "hot zone"). If the hot zone alone exceeds the threshold, the
    /// hot zone shrinks to ensure at least one item can be spilled.
    ///
    /// When the RSS-pressure trigger
    /// ([`memory_pressure_bytes`](Self::memory_pressure_bytes)) caused this
    /// call, at least one item is spilled regardless of the byte budget so
    /// pressure is actively relieved instead of merely surveyed.
    pub(super) fn spill_excess(&mut self) -> Result<(), SpillError> {
        if self.in_memory_only {
            return Err(SpillError::SpillDisabled);
        }

        let next = self.inner.next_expected();
        let count = self.inner.buffered_count();
        if count == 0 {
            return Ok(());
        }

        // The hot zone protects items near next_expected from thrashing.
        // Scale it down when the threshold is very tight.
        let hot_zone = HOT_ZONE.min(count as u64 / 2).max(1);
        let hot_limit = next.saturating_add(hot_zone);

        // Collect sequences eligible for spilling: those above the hot zone,
        // ordered from highest to lowest so we spill the furthest-from-delivery
        // items first.
        let capacity = self.inner.capacity();
        let mut candidates: Vec<u64> = Vec::new();
        for offset in (0..capacity).rev() {
            let seq = next + offset as u64;
            if seq < hot_limit {
                break;
            }
            candidates.push(seq);
        }

        // When RSS pressure forced this call we must spill at least one item,
        // even if the byte budget would otherwise allow the data to stay
        // resident. The whole-batch path always spills every candidate so the
        // demand is met automatically; the per-item path needs the explicit
        // flag.
        let rss_forced = self.should_force_spill_for_rss();
        let prev_spill_count = self.spill_count;
        let result = match self.granularity {
            SpillGranularity::PerItem => self.spill_candidates_per_item(&candidates, rss_forced),
            SpillGranularity::WholeBatch => self.spill_candidates_whole_batch(&candidates),
        };
        // Emit the one-shot warning after a successful spill that actually
        // wrote at least one record. Checking spill_count > prev_spill_count
        // avoids false positives when all candidates were in the hot zone.
        // The per-call `spill_activations` counter is granularity-invariant
        // (one increment per successful call) so adaptive ring sizing can
        // measure pressure without compensating for PerItem vs WholeBatch
        // record fan-out.
        if result.is_ok() && self.spill_count > prev_spill_count {
            self.spill_activations += 1;
            self.spill_warned =
                emit_spill_warning(self.spill_dir.as_deref(), self.threshold, self.spill_warned);
        }
        result
    }

    /// Per-item spill: each candidate becomes its own length-prefixed record.
    ///
    /// Matches the historical on-disk layout: `[u32 len][payload]` per item.
    /// When `rss_forced` is `true` the loop spills at least one candidate even
    /// if the byte budget is satisfied, so an RSS-pressure trigger actively
    /// relieves pressure instead of merely surveying it.
    fn spill_candidates_per_item(
        &mut self,
        candidates: &[u64],
        rss_forced: bool,
    ) -> Result<(), SpillError> {
        let mut spilled_any = false;
        for &seq in candidates {
            let byte_budget_ok = self.memory_used <= self.threshold;
            let rss_demand_met = !rss_forced || spilled_any;
            if byte_budget_ok && rss_demand_met {
                break;
            }
            if let Some(item) = self.inner.take(seq) {
                let item_size = item.estimated_size();
                match self.spill_item(seq, &item) {
                    Ok(()) => {
                        self.memory_used = self.memory_used.saturating_sub(item_size);
                        self.spill_count += 1;
                        spilled_any = true;
                    }
                    Err(e) => {
                        // Re-insert the item on spill failure so the caller
                        // can retry or shut down without losing the result.
                        // A NotFound here means spill_item refused to retry
                        // because prior records are unrecoverable; upgrade
                        // to the typed PriorSpillsLost variant so the
                        // receiver can emit an actionable diagnostic.
                        self.inner.force_insert(seq, item);
                        if e.kind() == io::ErrorKind::NotFound {
                            if let Some(lost) = self.prior_spills_lost_error() {
                                return Err(lost);
                            }
                        }
                        return Err(SpillError::Io(e));
                    }
                }
            }
        }
        Ok(())
    }

    /// Whole-batch spill: combine every candidate selected for this spill
    /// event into a single length-prefixed record so the per-item header
    /// overhead is paid once.
    ///
    /// The disk layout is `[u32 total_payload_len][payload1][payload2]...`.
    /// Every non-hot-zone candidate is evicted in one event so the next
    /// write amortises the 4-byte header across many items - the spill
    /// event leaves the hot zone in memory and nothing else, instead of
    /// repeatedly re-entering [`spill_excess`](Self::spill_excess) one
    /// item at a time.
    fn spill_candidates_whole_batch(&mut self, candidates: &[u64]) -> Result<(), SpillError> {
        // Collect every candidate eligible for eviction. Walk the selection
        // in the same highest-first order as the per-item path so the
        // closest-to-delivery items stay in memory.
        let mut taken: Vec<(u64, T, usize)> = Vec::new();
        for &seq in candidates {
            if let Some(item) = self.inner.take(seq) {
                let item_size = item.estimated_size();
                taken.push((seq, item, item_size));
            }
        }

        if taken.is_empty() {
            return Ok(());
        }

        // Encode all payloads up front. A codec failure must not leave a
        // partial record on disk, and re-insertion is straightforward while
        // the items are still owned here.
        let mut payload = Vec::new();
        for (_, item, _) in &taken {
            if let Err(e) = item.encode(&mut payload) {
                self.restore_taken(taken);
                return Err(SpillError::Io(e));
            }
        }
        if payload.len() > u32::MAX as usize {
            self.restore_taken(taken);
            return Err(SpillError::Io(io::Error::new(
                io::ErrorKind::InvalidData,
                "spill record exceeds u32::MAX bytes",
            )));
        }
        let len = payload.len() as u32;
        let mut record_offset = self.spill_write_pos;

        let written = match self.write_record(&len.to_le_bytes(), &payload) {
            Ok(w) => w,
            Err(e) if e.kind() == io::ErrorKind::NotFound && self.spill_dir.is_some() => {
                if !self.spill_index.is_empty() {
                    let lost = self.prior_spills_lost_error();
                    self.restore_taken(taken);
                    return Err(lost.unwrap_or(SpillError::Io(e)));
                }
                if let Err(retry_err) = self.recreate_spill_dir() {
                    self.restore_taken(taken);
                    return Err(SpillError::Io(retry_err));
                }
                // recreate_spill_dir resets write_pos and clears the index,
                // so re-anchor the record offset before the retry write.
                record_offset = self.spill_write_pos;
                match self.write_record(&len.to_le_bytes(), &payload) {
                    Ok(w) => w,
                    Err(retry_err) => {
                        self.restore_taken(taken);
                        return Err(SpillError::Io(retry_err));
                    }
                }
            }
            Err(e) => {
                self.restore_taken(taken);
                return Err(SpillError::Io(e));
            }
        };

        // Record the placement of every item now that the write committed.
        // batch_members must include single-item records: the reader
        // dispatches on its presence to pick reload_batch (4-byte header) over
        // reload_item (5-byte tag+len header). Skipping the insert when
        // slots.len() == 1 used to send the reader through reload_item, which
        // misreads the missing tag byte and surfaces an UnexpectedEof on
        // every default-granularity workload that spills one item at a time.
        let slots: Vec<Option<u64>> = taken.iter().map(|(seq, _, _)| Some(*seq)).collect();
        for (seq, _, item_size) in &taken {
            self.spill_index.insert(*seq, record_offset);
            self.memory_used = self.memory_used.saturating_sub(*item_size);
        }
        self.batch_members.insert(record_offset, slots);
        self.spill_write_pos = record_offset.saturating_add(written);
        self.spill_count += 1;
        Ok(())
    }

    /// Returns `true` when the optional RSS-pressure threshold is set and
    /// the cached process RSS reading has crossed it. Probe errors and the
    /// `None` configuration are treated as "no pressure" so the historical
    /// byte-budget path stays in charge.
    pub(super) fn should_force_spill_for_rss(&self) -> bool {
        let Some(limit) = self.memory_pressure_bytes else {
            return false;
        };
        match rss::cached_rss_bytes() {
            Ok(rss) => rss > limit,
            // Probe failure (including the Windows `Unsupported` stub) keeps
            // the caller on the byte-budget path - the knob silently degrades.
            Err(_) => false,
        }
    }

    /// Serializes a single item to the spill file.
    ///
    /// On [`io::ErrorKind::NotFound`] for a directory-backed buffer this
    /// invokes [`recreate_spill_dir`](Self::recreate_spill_dir) and retries
    /// once. All other errors (ENOSPC, partial writes via the
    /// [`Write::write_all`] contract, encoder failures) bubble up unchanged.
    ///
    /// The on-disk record layout is `[u8 tag][u32 LE len][payload]` where
    /// `tag` selects the payload codec ([`SPILL_TAG_RAW`] or
    /// [`SPILL_TAG_ZSTD`]) and `len` is the on-disk byte length of the
    /// (possibly compressed) payload.
    fn spill_item(&mut self, sequence: u64, item: &T) -> io::Result<()> {
        // Encode payload up front so a codec error never leaves a partial
        // record in the spill file.
        let mut encoded = Vec::new();
        item.encode(&mut encoded)?;

        let (tag, payload) = self.compress_payload(encoded)?;
        if payload.len() > u32::MAX as usize {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "spill record exceeds u32::MAX bytes",
            ));
        }
        let len = payload.len() as u32;
        let header = build_header(tag, len);

        match self.write_record(&header, &payload) {
            Ok(written) => {
                self.spill_index.insert(sequence, self.spill_write_pos);
                self.spill_write_pos += written;
                Ok(())
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound && self.spill_dir.is_some() => {
                // Temp directory vanished mid-transfer. Recovery is only
                // safe when no prior items had been spilled - otherwise
                // those items are lost on disk and silently continuing
                // would corrupt the transfer. With prior items present
                // we surface the original NotFound here; the caller
                // upgrades it to SpillError::PriorSpillsLost so the
                // receiver can emit an actionable diagnostic.
                if !self.spill_index.is_empty() {
                    return Err(e);
                }
                self.recreate_spill_dir()?;
                let written = self.write_record(&header, &payload)?;
                self.spill_index.insert(sequence, self.spill_write_pos);
                self.spill_write_pos += written;
                Ok(())
            }
            Err(e) => Err(e),
        }
    }

    /// Builds a [`SpillError::PriorSpillsLost`] for the configured directory.
    ///
    /// Returns `None` only when the spooled flavour (no caller-supplied
    /// directory) somehow reached the recovery-refusal branch, in which case
    /// callers should fall back to the original [`SpillError::Io`].
    pub(super) fn prior_spills_lost_error(&self) -> Option<SpillError> {
        self.spill_dir
            .clone()
            .map(|dir| SpillError::PriorSpillsLost {
                dir,
                count: self.spill_index.len(),
            })
    }

    /// Applies the configured compression codec to the freshly encoded payload.
    ///
    /// Returns `(tag, bytes_to_write)`. [`SpillCompression::None`] is a
    /// pass-through that emits [`SPILL_TAG_RAW`]; [`SpillCompression::Zstd`]
    /// emits [`SPILL_TAG_ZSTD`] and the zstd-encoded bytes.
    fn compress_payload(&self, encoded: Vec<u8>) -> io::Result<(u8, Vec<u8>)> {
        match self.compression {
            SpillCompression::None => Ok((SPILL_TAG_RAW, encoded)),
            #[cfg(feature = "spill-compression")]
            SpillCompression::Zstd { level } => {
                let compressed = zstd::stream::encode_all(encoded.as_slice(), level)?;
                Ok((SPILL_TAG_ZSTD, compressed))
            }
        }
    }

    /// Writes a tag-prefixed length-prefixed record to the spill file, opening
    /// it lazily.
    ///
    /// Returns the number of bytes written (always `header.len() + payload.len()`
    /// on success). All `write_all` calls obey the standard library contract
    /// of returning [`io::ErrorKind::WriteZero`] on partial writes.
    fn write_record(&mut self, header: &[u8], payload: &[u8]) -> io::Result<u64> {
        let dir = self.spill_dir.clone();
        let backend = match self.spill_file.as_mut() {
            Some(b) => b,
            None => self.spill_file.insert(open_backend(dir.as_deref())?),
        };
        let file = backend.file();
        file.seek(SeekFrom::Start(self.spill_write_pos))?;
        file.write_all(header)?;
        file.write_all(payload)?;
        Ok(header.len() as u64 + payload.len() as u64)
    }

    /// Re-creates the spill directory after a [`io::ErrorKind::NotFound`].
    ///
    /// Drops any stale file handle, attempts `create_dir_all` once, and
    /// resets the in-flight write cursor and spill index. On retry success
    /// the next write opens a fresh tempfile. Any items previously spilled
    /// to the vanished file are now unrecoverable; the caller's transfer
    /// must treat the surrounding error as fatal if it needed those items.
    fn recreate_spill_dir(&mut self) -> io::Result<()> {
        let Some(dir) = self.spill_dir.clone() else {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "spill backend has no directory to re-create",
            ));
        };
        // Drop the stale file handle before recreating the parent so the
        // OS does not keep a deleted inode pinned in our process.
        self.spill_file = None;
        fs::create_dir_all(&dir)?;
        self.spill_write_pos = 0;
        self.spill_index.clear();
        self.batch_members.clear();
        self.dir_recreate_count += 1;
        Ok(())
    }
}

/// Builds the on-disk record header: one tag byte followed by the little-endian
/// payload length.
///
/// Returning a fixed-size array (instead of a `Vec`) keeps the hot path
/// allocation-free.
fn build_header(tag: u8, len: u32) -> [u8; 5] {
    let mut header = [0u8; 5];
    header[0] = tag;
    header[1..5].copy_from_slice(&len.to_le_bytes());
    header
}
