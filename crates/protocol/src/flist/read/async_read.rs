//! Async file-list entry reader, gated on the `tokio-transfer` feature.
//!
//! This is the `.await`-driven counterpart to
//! [`FileListReader::read_entry_with_flist`](super::FileListReader::read_entry_with_flist).
//! It reuses the *identical* sync decode through the sans-io seam
//! ([`FileListReader::read_entry_step`](super::FileListReader::read_entry_step)):
//! bytes are pulled off an [`AsyncRead`] into a carry-over buffer, the shared
//! decode is run speculatively over that buffer, and when it needs more bytes
//! the driver awaits another read and retries. The decode logic is never forked,
//! so the async reader returns byte-identical [`FileEntry`] values for the same
//! wire bytes - including when the bytes are delivered in arbitrary chunks
//! across `.await` points.
//!
//! Additive and unwired: this leaf is not connected to the receiver pipeline.
//! Its consuming rung is the coupled ASY-7 receiver redo.

use std::io;

use tokio::io::{AsyncRead, AsyncReadExt};

use super::{EntryStep, FileListReader};
use crate::flist::entry::FileEntry;

/// Reads the next file-list entry from an [`AsyncRead`], awaiting the wire.
///
/// Byte-for-byte equivalent to
/// [`FileListReader::read_entry_with_flist`](super::FileListReader::read_entry_with_flist):
/// it decodes one entry (or detects end-of-list) using the shared sans-io seam,
/// returning `Ok(Some(entry))` for a decoded entry, `Ok(None)` at end-of-list,
/// or an error on malformed data / truncation.
///
/// `carry` is a caller-owned buffer that holds wire bytes read past the end of
/// the previous entry. It must be reused across successive calls in the same
/// flist segment so that bytes read ahead are not lost. On return, any bytes the
/// decoded entry consumed have been drained from the front of `carry`; leftover
/// bytes (belonging to the next entry) remain for the following call.
///
/// `segment_entries` has the same meaning as in the sync reader: the entries
/// already decoded in the current segment, used to resolve abbreviated hardlink
/// followers.
///
/// # Errors
///
/// - Any decode error from the shared core (malformed name, oversize field,
///   overflow, ...) propagates unchanged, exactly as the blocking reader would
///   surface it.
/// - If the stream ends while an entry is still incomplete, returns
///   [`io::ErrorKind::UnexpectedEof`], mirroring the blocking reader's behaviour
///   on a truncated wire.
pub async fn read_entry_with_flist_async<R>(
    reader: &mut FileListReader,
    src: &mut R,
    carry: &mut Vec<u8>,
    segment_entries: &[FileEntry],
) -> io::Result<Option<FileEntry>>
where
    R: AsyncRead + Unpin + ?Sized,
{
    let mut read_buf = [0u8; 8192];
    loop {
        match reader.read_entry_step(carry, segment_entries)? {
            EntryStep::Emit { entry, consumed } => {
                carry.drain(..consumed);
                return Ok(entry);
            }
            EntryStep::NeedMore => {
                let n = src.read(&mut read_buf).await?;
                if n == 0 {
                    // The stream ended mid-entry. An empty carry at end-of-stream
                    // is not our concern (the caller drives the segment loop and
                    // would not have called us), so any pending bytes here mean a
                    // genuinely truncated entry.
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "file-list entry truncated: stream ended mid-entry",
                    ));
                }
                carry.extend_from_slice(&read_buf[..n]);
            }
        }
    }
}
