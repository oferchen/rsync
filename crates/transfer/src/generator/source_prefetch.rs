//! Read-ahead of whole-file sources whose requests are already buffered.
//!
//! On a first transfer the receiver's generator streams one whole-file
//! request (NDX + iflags + empty sum head) per file, and a single demuxed
//! `MSG_DATA` frame usually carries dozens of them. The sender still handles
//! them one at a time, but before opening the current file it peeks at the
//! requests already sitting in the frame and reads their sources together
//! through [`fast_io::prefetch_sources`] - one io_uring submission for every
//! open and one for every stat + read + close, instead of four syscalls per
//! file. Later requests are then served from memory.
//!
//! Only requests already received are prefetched, never speculative ones, so
//! the sender opens exactly the files upstream would, in the same order of
//! requests, and wire output is byte-identical. The look-ahead parse mirrors
//! the loop's own request decode on a clone of the NDX codec; anything it does
//! not fully understand ends the look-ahead rather than guessing.
//!
//! # Upstream Reference
//!
//! - `sender.c:230-420` `send_files()` - the per-request decode
//!   (`read_ndx_and_attrs` + `receive_sums`) and source open this reads ahead of.

use std::collections::HashMap;
use std::io::Read;
use std::path::PathBuf;

use protocol::codec::{NDX_DONE, NdxCodec, NdxCodecEnum};

use super::context::GeneratorContext;
use super::item_flags::ItemFlags;
use crate::receiver::SumHead;

/// Most files one read-ahead batch opens.
const MAX_BATCH_FILES: usize = 64;

/// Most bytes one read-ahead batch holds in memory.
const MAX_BATCH_BYTES: u64 = 4 << 20;

/// A batch of one would replace a plain `read(2)` with one ring round trip per
/// syscall - no fewer syscalls - so a lone request keeps the ordinary path.
const MIN_BATCH_FILES: usize = 2;

/// Session-constant facts the look-ahead needs to decode requests exactly as
/// the transfer loop does.
#[derive(Clone, Copy, Debug)]
pub(super) struct RequestShape {
    /// Negotiated protocol; gates the iflags encoding and the `blength` ceiling.
    pub(super) protocol: u8,
    /// Transfer digest length, bounding `s2length` like `read_sum_head()`.
    pub(super) xfer_sum_len: u32,
    /// `--xattrs`: an `ITEM_REPORT_XATTR` request carries an xattr list the
    /// look-ahead does not decode, so it ends the look-ahead.
    pub(super) xattrs: bool,
}

/// Wire NDX values of the whole-file requests buffered in `buf`, in order.
///
/// `flist_frees` is how many leading `NDX_DONE`s are INC_RECURSE flist-free
/// markers rather than a phase change (the loop's `flist_done_remaining`);
/// the look-ahead walks past that many and stops at the next one. It also
/// stops at any other negative index, a request it cannot fully decode, and
/// after `limit` finds.
pub(super) fn peek_whole_file_requests(
    buf: &[u8],
    codec: &NdxCodecEnum,
    shape: RequestShape,
    flist_frees: usize,
    limit: usize,
) -> Vec<i32> {
    let mut codec = codec.clone();
    let mut input = buf;
    let mut frees_left = flist_frees;
    let mut found = Vec::new();
    while found.len() < limit && !input.is_empty() {
        let Ok(ndx) = codec.read_ndx(&mut input) else {
            break;
        };
        if ndx == NDX_DONE && frees_left > 0 {
            frees_left -= 1;
            continue;
        }
        if ndx < 0 {
            break;
        }
        let Ok(iflags) = ItemFlags::read(&mut input, shape.protocol) else {
            break;
        };
        if iflags.read_trailing(&mut input).is_err()
            || (shape.xattrs && iflags.raw() & ItemFlags::ITEM_REPORT_XATTR != 0)
        {
            break;
        }
        if !iflags.needs_transfer() {
            continue;
        }
        let Ok(head) = SumHead::read_negotiated(&mut input, shape.protocol, shape.xfer_sum_len)
        else {
            break;
        };
        if head.is_empty() {
            found.push(ndx);
            continue;
        }
        let sums = head.count as usize * (4 + head.s2length as usize);
        if input.len() < sums {
            break;
        }
        input = &input[sums..];
    }
    found
}

/// Whole-file sources read ahead of their requests, keyed by flat file index.
pub(super) struct SourcePrefetcher {
    enabled: bool,
    ready: HashMap<usize, Vec<u8>>,
    served: usize,
    batches: usize,
}

impl SourcePrefetcher {
    /// A prefetcher that reads ahead only when `enabled`.
    pub(super) fn new(enabled: bool) -> Self {
        Self {
            enabled,
            ready: HashMap::new(),
            served: 0,
            batches: 0,
        }
    }

    /// Whole-file sends served from a read-ahead batch.
    pub(super) const fn served(&self) -> usize {
        self.served
    }

    /// Read-ahead batches submitted.
    pub(super) const fn batches(&self) -> usize {
        self.batches
    }

    /// Drops read-ahead data, e.g. at a phase change where no buffered
    /// request can still claim it.
    pub(super) fn clear(&mut self) {
        self.ready.clear();
    }

    /// Returns the contents of whole-file source `ndx` (flist length `len`)
    /// from read-ahead, or `None` for the caller's ordinary open.
    ///
    /// When `ndx` has not been read ahead, it is read now together with the
    /// whole-file requests peeked from `buffered` - but only when at least one
    /// such request exists, since batching a single file saves nothing.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn take_or_batch(
        &mut self,
        ctx: &GeneratorContext,
        ndx: usize,
        len: u64,
        buffered: &[u8],
        codec: &NdxCodecEnum,
        shape: RequestShape,
        flist_frees: usize,
    ) -> Option<Vec<u8>> {
        if !self.enabled || len > fast_io::PREFETCH_MAX_FILE_LEN {
            return None;
        }
        if let Some(data) = self.ready.remove(&ndx) {
            self.served += 1;
            return Some(data);
        }
        let batch = self.plan(ctx, ndx, len, buffered, codec, shape, flist_frees);
        if batch.len() < MIN_BATCH_FILES {
            return None;
        }
        let paths: Vec<PathBuf> = batch
            .iter()
            .map(|&(n, _)| ctx.reconstruct_source_path(n))
            .collect();
        let requests: Vec<fast_io::PrefetchRequest<'_>> = batch
            .iter()
            .zip(&paths)
            .map(|(&(_, len), path)| fast_io::PrefetchRequest { path, len })
            .collect();
        let source_open = ctx.source_open();
        let (open, noatime) = source_open.prefetch_open();
        let contents = fast_io::prefetch_sources(open, noatime, &requests);
        self.batches += 1;
        let mut current = None;
        for (&(n, _), data) in batch.iter().zip(contents) {
            match data {
                Some(data) if n == ndx => current = Some(data),
                Some(data) => {
                    self.ready.insert(n, data);
                }
                None => {}
            }
        }
        if current.is_some() {
            self.served += 1;
        }
        current
    }

    /// `(flat index, length)` of `ndx` followed by the buffered whole-file
    /// requests eligible to join its batch.
    #[allow(clippy::too_many_arguments)]
    fn plan(
        &self,
        ctx: &GeneratorContext,
        ndx: usize,
        len: u64,
        buffered: &[u8],
        codec: &NdxCodecEnum,
        shape: RequestShape,
        flist_frees: usize,
    ) -> Vec<(usize, u64)> {
        let mut batch = vec![(ndx, len)];
        let mut bytes = len;
        let files = ctx.file_list();
        for wire in
            peek_whole_file_requests(buffered, codec, shape, flist_frees, MAX_BATCH_FILES - 1)
        {
            let flat = ctx.resolve_itemize_ndx(wire);
            let Some(entry) = files.get(flat) else {
                continue;
            };
            let size = entry.size();
            if !entry.is_file()
                || size > fast_io::PREFETCH_MAX_FILE_LEN
                || self.ready.contains_key(&flat)
                || batch.iter().any(|&(n, _)| n == flat)
            {
                continue;
            }
            if bytes + size > MAX_BATCH_BYTES {
                break;
            }
            bytes += size;
            batch.push((flat, size));
        }
        batch
    }
}

/// Reader over a prefetched source; boxed where the loop expects its reader.
pub(super) fn prefetched_reader(data: Vec<u8>) -> Box<dyn Read> {
    Box::new(std::io::Cursor::new(data))
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::codec::{MonotonicNdxWriter, create_ndx_codec};

    const SHAPE: RequestShape = RequestShape {
        protocol: 32,
        xfer_sum_len: 16,
        xattrs: false,
    };
    const ITEM_TRANSFER: [u8; 2] = [0x00, 0x80];

    fn whole_file(writer: &mut MonotonicNdxWriter, wire: &mut Vec<u8>, ndx: i32) {
        writer.write_ndx(wire, ndx).unwrap();
        wire.extend_from_slice(&ITEM_TRANSFER);
        SumHead::empty().write(wire).unwrap();
    }

    /// The peek decodes a clone of the loop's codec: the diff-encoded NDX
    /// values come out right, and the real codec is left untouched.
    #[test]
    fn peek_decodes_buffered_whole_file_requests_without_consuming() {
        let mut writer = MonotonicNdxWriter::new(32);
        let mut wire = Vec::new();
        for ndx in [3, 4, 9] {
            whole_file(&mut writer, &mut wire, ndx);
        }
        let codec = create_ndx_codec(32);
        assert_eq!(
            peek_whole_file_requests(&wire, &codec, SHAPE, 0, 64),
            vec![3, 4, 9]
        );
        assert_eq!(
            peek_whole_file_requests(&wire, &codec, SHAPE, 0, 2),
            vec![3, 4]
        );
        let mut fresh = codec.clone();
        assert_eq!(fresh.read_ndx(&mut wire.as_slice()).unwrap(), 3);
    }

    /// A delta request (non-empty sum head) is stepped over, its block sums
    /// skipped, and look-ahead continues to the whole-file request behind it.
    #[test]
    fn peek_skips_delta_requests_and_their_sums() {
        let mut writer = MonotonicNdxWriter::new(32);
        let mut wire = Vec::new();
        writer.write_ndx(&mut wire, 1).unwrap();
        wire.extend_from_slice(&ITEM_TRANSFER);
        SumHead::new(2, 700, 16, 0).write(&mut wire).unwrap();
        wire.extend_from_slice(&[0xAB; 2 * (4 + 16)]);
        whole_file(&mut writer, &mut wire, 2);
        let codec = create_ndx_codec(32);
        assert_eq!(
            peek_whole_file_requests(&wire, &codec, SHAPE, 0, 64),
            vec![2]
        );
    }

    /// NDX_DONE is a phase change unless it is one of the INC_RECURSE
    /// flist-free markers the loop still expects; a phase change ends the
    /// look-ahead so the next phase's requests are never read early.
    #[test]
    fn peek_stops_at_a_phase_change_but_walks_flist_frees() {
        let mut writer = MonotonicNdxWriter::new(32);
        let mut wire = Vec::new();
        whole_file(&mut writer, &mut wire, 1);
        writer.write_ndx_done(&mut wire).unwrap();
        whole_file(&mut writer, &mut wire, 2);
        let codec = create_ndx_codec(32);
        assert_eq!(
            peek_whole_file_requests(&wire, &codec, SHAPE, 0, 64),
            vec![1]
        );
        assert_eq!(
            peek_whole_file_requests(&wire, &codec, SHAPE, 1, 64),
            vec![1, 2]
        );
    }

    /// A truncated trailing request (the rest is still on the socket) ends the
    /// look-ahead without inventing a request.
    #[test]
    fn peek_ignores_a_partial_trailing_request() {
        let mut writer = MonotonicNdxWriter::new(32);
        let mut wire = Vec::new();
        whole_file(&mut writer, &mut wire, 5);
        whole_file(&mut writer, &mut wire, 6);
        wire.truncate(wire.len() - 3);
        let codec = create_ndx_codec(32);
        assert_eq!(
            peek_whole_file_requests(&wire, &codec, SHAPE, 0, 64),
            vec![5]
        );
    }

    /// With `--xattrs` an `ITEM_REPORT_XATTR` request carries an xattr list
    /// the peek cannot size, so it must stop there.
    #[test]
    fn peek_stops_at_an_xattr_request() {
        let mut writer = MonotonicNdxWriter::new(32);
        let mut wire = Vec::new();
        whole_file(&mut writer, &mut wire, 1);
        writer.write_ndx(&mut wire, 2).unwrap();
        wire.extend_from_slice(&[0x00, 0x81]);
        wire.push(0);
        let codec = create_ndx_codec(32);
        let shape = RequestShape {
            xattrs: true,
            ..SHAPE
        };
        assert_eq!(
            peek_whole_file_requests(&wire, &codec, shape, 0, 64),
            vec![1]
        );
    }
}
