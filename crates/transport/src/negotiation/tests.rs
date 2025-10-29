use super::*;

use std::{
    collections::TryReserveError,
    error::Error as _,
    io::{self, BufRead, Cursor, IoSlice, IoSliceMut, Read, Write},
};

use rsync_protocol::{
    LEGACY_DAEMON_PREFIX_LEN, LegacyDaemonMessage, NegotiationPrologue, NegotiationPrologueSniffer,
    ProtocolVersion,
};

#[test]
fn map_line_reserve_error_for_io_marks_out_of_memory() {
    let mut buf = Vec::<u8>::new();
    let reserve_err = buf
        .try_reserve_exact(usize::MAX)
        .expect_err("capacity overflow must error");

    let mapped = super::map_line_reserve_error_for_io(reserve_err);
    assert_eq!(mapped.kind(), io::ErrorKind::OutOfMemory);
    assert!(
        mapped
            .to_string()
            .contains("failed to reserve memory for legacy negotiation buffer")
    );

    let source = mapped.source().expect("mapped error must retain source");
    assert!(source.downcast_ref::<TryReserveError>().is_some());
}

#[test]
fn buffered_copy_too_small_converts_into_io_error() {
    let err = BufferedCopyTooSmall::new(LEGACY_DAEMON_PREFIX_LEN + 4, 4);
    let io_err: io::Error = err.into();

    assert_eq!(io_err.kind(), io::ErrorKind::InvalidInput);
    assert_eq!(io_err.to_string(), err.to_string());

    let inner = io_err
        .into_inner()
        .expect("conversion retains source error");
    let recovered = inner
        .downcast::<BufferedCopyTooSmall>()
        .expect("inner error matches original type");
    assert_eq!(*recovered, err);
}

#[test]
fn copy_to_slice_error_converts_into_io_error() {
    let err = CopyToSliceError::new(LEGACY_DAEMON_PREFIX_LEN + 8, 8);
    let io_err: io::Error = err.into();

    assert_eq!(io_err.kind(), io::ErrorKind::InvalidInput);
    assert_eq!(io_err.to_string(), err.to_string());

    let inner = io_err
        .into_inner()
        .expect("conversion retains source error");
    let recovered = inner
        .downcast::<CopyToSliceError>()
        .expect("inner error matches original type");
    assert_eq!(*recovered, err);
}

fn sniff_bytes(data: &[u8]) -> io::Result<NegotiatedStream<Cursor<Vec<u8>>>> {
    let cursor = Cursor::new(data.to_vec());
    sniff_negotiation_stream(cursor)
}

#[derive(Debug)]
struct RecordingTransport {
    reader: Cursor<Vec<u8>>,
    writes: Vec<u8>,
    flushes: usize,
}

impl RecordingTransport {
    fn new(input: &[u8]) -> Self {
        Self {
            reader: Cursor::new(input.to_vec()),
            writes: Vec::new(),
            flushes: 0,
        }
    }

    fn from_cursor(cursor: Cursor<Vec<u8>>) -> Self {
        Self {
            reader: cursor,
            writes: Vec::new(),
            flushes: 0,
        }
    }

    fn writes(&self) -> &[u8] {
        &self.writes
    }

    fn flushes(&self) -> usize {
        self.flushes
    }
}

impl Read for RecordingTransport {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.reader.read(buf)
    }
}

impl Write for RecordingTransport {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.writes.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn write_vectored(&mut self, bufs: &[IoSlice<'_>]) -> io::Result<usize> {
        let mut total = 0;
        for slice in bufs {
            self.writes.extend_from_slice(slice);
            total += slice.len();
        }
        Ok(total)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.flushes += 1;
        Ok(())
    }
}

#[derive(Default)]
struct VectoredOnlyWriter {
    writes: Vec<u8>,
    vectored_calls: usize,
}

impl VectoredOnlyWriter {
    fn writes(&self) -> &[u8] {
        &self.writes
    }

    fn vectored_calls(&self) -> usize {
        self.vectored_calls
    }
}

impl Write for VectoredOnlyWriter {
    fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
        panic!("scalar write path should not be used when vectored IO is available");
    }

    fn write_vectored(&mut self, bufs: &[IoSlice<'_>]) -> io::Result<usize> {
        self.vectored_calls += 1;
        let mut total = 0usize;
        for slice in bufs {
            self.writes.extend_from_slice(slice);
            total += slice.len();
        }
        Ok(total)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[derive(Default)]
struct UnsupportedVectoredWriter {
    writes: Vec<u8>,
    write_calls: usize,
    vectored_calls: usize,
}

impl UnsupportedVectoredWriter {
    fn writes(&self) -> &[u8] {
        &self.writes
    }

    fn write_calls(&self) -> usize {
        self.write_calls
    }

    fn vectored_calls(&self) -> usize {
        self.vectored_calls
    }
}

impl Write for UnsupportedVectoredWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.write_calls += 1;
        self.writes.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn write_vectored(&mut self, _bufs: &[IoSlice<'_>]) -> io::Result<usize> {
        self.vectored_calls += 1;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "no vectored support",
        ))
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

struct PartialVectoredWriter {
    writes: Vec<u8>,
    first_call_limit: usize,
    vectored_calls: usize,
    write_calls: usize,
    first_call: bool,
}

impl PartialVectoredWriter {
    fn new(first_call_limit: usize) -> Self {
        Self {
            writes: Vec::new(),
            first_call_limit,
            vectored_calls: 0,
            write_calls: 0,
            first_call: true,
        }
    }

    fn writes(&self) -> &[u8] {
        &self.writes
    }

    fn vectored_calls(&self) -> usize {
        self.vectored_calls
    }

    fn write_calls(&self) -> usize {
        self.write_calls
    }
}

impl Write for PartialVectoredWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.write_calls += 1;
        self.writes.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn write_vectored(&mut self, bufs: &[IoSlice<'_>]) -> io::Result<usize> {
        self.vectored_calls += 1;

        if self.first_call {
            self.first_call = false;

            let mut remaining = self.first_call_limit;
            let mut written = 0usize;

            for slice in bufs {
                if remaining == 0 {
                    break;
                }

                let chunk = remaining.min(slice.len());
                self.writes.extend_from_slice(&slice[..chunk]);
                written += chunk;
                remaining -= chunk;
            }

            return Ok(written);
        }

        let mut total = 0usize;
        for slice in bufs {
            self.writes.extend_from_slice(slice);
            total += slice.len();
        }
        Ok(total)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[derive(Default)]
struct InterruptedVectoredWriter {
    writes: Vec<u8>,
    vectored_calls: usize,
    write_calls: usize,
    interrupted: bool,
}

impl InterruptedVectoredWriter {
    fn writes(&self) -> &[u8] {
        &self.writes
    }

    fn vectored_calls(&self) -> usize {
        self.vectored_calls
    }

    fn write_calls(&self) -> usize {
        self.write_calls
    }
}

impl Write for InterruptedVectoredWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.write_calls += 1;
        self.writes.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn write_vectored(&mut self, bufs: &[IoSlice<'_>]) -> io::Result<usize> {
        self.vectored_calls += 1;

        if !self.interrupted {
            self.interrupted = true;
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "vectored write interrupted",
            ));
        }

        let mut total = 0usize;
        for slice in bufs {
            self.writes.extend_from_slice(slice);
            total += slice.len();
        }
        Ok(total)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn negotiation_buffered_slices_write_to_prefers_vectored_io() {
    let prefix = b"@RSYNCD:";
    let remainder = b" 31.0\n";
    let slices = NegotiationBufferedSlices::new(prefix, remainder);
    let mut writer = VectoredOnlyWriter::default();

    slices.write_to(&mut writer).expect("write succeeds");

    let mut expected = Vec::new();
    expected.extend_from_slice(prefix);
    expected.extend_from_slice(remainder);

    assert_eq!(writer.vectored_calls(), 1);
    assert_eq!(writer.writes(), expected.as_slice());
}

#[test]
fn negotiation_buffered_slices_write_to_handles_unsupported_vectored_io() {
    let prefix = b"@RSYNCD:";
    let remainder = b" listing";
    let slices = NegotiationBufferedSlices::new(prefix, remainder);
    let mut writer = UnsupportedVectoredWriter::default();

    slices.write_to(&mut writer).expect("write succeeds");

    let mut expected = Vec::new();
    expected.extend_from_slice(prefix);
    expected.extend_from_slice(remainder);

    assert_eq!(writer.vectored_calls(), 1);
    assert_eq!(writer.write_calls(), 2);
    assert_eq!(writer.writes(), expected.as_slice());
}

#[test]
fn negotiation_buffered_slices_write_to_flushes_remaining_after_partial_vectored_call() {
    let prefix = b"@RSYNC"; // intentionally shorter to exercise partial writes
    let remainder = b" protocol";
    let slices = NegotiationBufferedSlices::new(prefix, remainder);
    let mut writer = PartialVectoredWriter::new(prefix.len() - 2);

    slices.write_to(&mut writer).expect("write succeeds");

    let mut expected = Vec::new();
    expected.extend_from_slice(prefix);
    expected.extend_from_slice(remainder);

    assert!(writer.vectored_calls() >= 2);
    assert_eq!(writer.write_calls(), 0);
    assert_eq!(writer.writes(), expected.as_slice());
}

#[test]
fn negotiation_buffered_slices_write_to_retries_after_interrupted_vectored_call() {
    let prefix = b"@RSYNCD:";
    let remainder = b" banner";
    let slices = NegotiationBufferedSlices::new(prefix, remainder);
    let mut writer = InterruptedVectoredWriter::default();

    slices
        .write_to(&mut writer)
        .expect("write succeeds after retry");

    let mut expected = Vec::new();
    expected.extend_from_slice(prefix);
    expected.extend_from_slice(remainder);

    assert_eq!(writer.vectored_calls(), 2);
    assert_eq!(writer.write_calls(), 0);
    assert_eq!(writer.writes(), expected.as_slice());
}

#[test]
fn negotiation_buffered_slices_copy_to_slice_copies_bytes() {
    let prefix = b"@RSYNCD:";
    let remainder = b" 31.0\nreply";
    let slices = NegotiationBufferedSlices::new(prefix, remainder);
    let mut buffer = [0u8; 32];

    let copied = slices
        .copy_to_slice(&mut buffer)
        .expect("buffer has enough capacity");

    assert_eq!(copied, prefix.len() + remainder.len());

    let mut expected = Vec::new();
    expected.extend_from_slice(prefix);
    expected.extend_from_slice(remainder);

    assert_eq!(&buffer[..copied], expected.as_slice());
}

#[test]
fn negotiation_buffered_slices_copy_to_slice_reports_required_length() {
    let slices = NegotiationBufferedSlices::new(b"@RSYNCD:", b" 31.0\nreply");
    let mut buffer = [0u8; 4];

    let err = slices
        .copy_to_slice(&mut buffer)
        .expect_err("buffer is too small");

    let expected_len = b"@RSYNCD: 31.0\nreply".len();

    assert_eq!(err.required(), expected_len);
    assert_eq!(err.provided(), buffer.len());
    assert_eq!(err.missing(), expected_len - buffer.len());
}

#[test]
fn negotiation_buffered_slices_copy_to_slice_handles_empty_transcript() {
    let slices = NegotiationBufferedSlices::new(&[], &[]);
    let mut buffer = [0u8; 1];

    let copied = slices
        .copy_to_slice(&mut buffer)
        .expect("empty transcript requires no space");

    assert_eq!(copied, 0);
    assert_eq!(buffer[0], 0);
}

#[test]
fn negotiation_buffered_slices_extend_vec_appends_buffered_bytes() {
    let prefix = b"@RSYNCD:";
    let remainder = b" motd";
    let slices = NegotiationBufferedSlices::new(prefix, remainder);

    let mut buffer = b"prefix: ".to_vec();
    let prefix_len = buffer.len();
    let appended = slices
        .extend_vec(&mut buffer)
        .expect("Vec<u8> growth should succeed for small transcripts");

    assert_eq!(&buffer[..prefix_len], b"prefix: ");

    let mut expected = Vec::new();
    expected.extend_from_slice(prefix);
    expected.extend_from_slice(remainder);
    assert_eq!(appended, expected.len());
    assert_eq!(&buffer[prefix_len..], expected.as_slice());
}

#[test]
fn negotiation_buffered_slices_to_vec_collects_buffered_bytes() {
    let prefix = b"@RSYNCD:";
    let remainder = b" motd";
    let slices = NegotiationBufferedSlices::new(prefix, remainder);

    let collected = slices
        .to_vec()
        .expect("Vec<u8> growth should succeed for small transcripts");

    let mut expected = Vec::new();
    expected.extend_from_slice(prefix);
    expected.extend_from_slice(remainder);

    assert_eq!(collected, expected);
}

#[test]
fn negotiation_buffer_copy_all_vectored_avoids_touching_extra_buffers() {
    let transcript = b"@RSYNCD:".to_vec();
    let buffer = NegotiationBuffer::new(LEGACY_DAEMON_PREFIX_LEN, 0, transcript.clone());

    let mut first = vec![0u8; 4];
    let mut second = vec![0u8; transcript.len() - first.len()];
    let mut extra = vec![0xAAu8; 6];

    let copied = {
        let mut bufs = [
            IoSliceMut::new(first.as_mut_slice()),
            IoSliceMut::new(second.as_mut_slice()),
            IoSliceMut::new(extra.as_mut_slice()),
        ];
        buffer
            .copy_all_into_vectored(&mut bufs)
            .expect("copy should succeed with ample capacity")
    };

    assert_eq!(copied, transcript.len());
    assert_eq!(first.as_slice(), &transcript[..first.len()]);
    assert_eq!(second.as_slice(), &transcript[first.len()..]);
    assert!(extra.iter().all(|byte| *byte == 0xAA));
}

#[test]
fn negotiation_buffer_copy_remaining_vectored_avoids_touching_extra_buffers() {
    let transcript = b"@RSYNCD: handshake".to_vec();
    let consumed = 3usize;
    let buffer = NegotiationBuffer::new(LEGACY_DAEMON_PREFIX_LEN, consumed, transcript.clone());
    let remaining_len = transcript.len() - consumed;

    let first_len = remaining_len / 2 + 1;
    let mut first = vec![0u8; first_len];
    let mut second = vec![0u8; remaining_len - first_len];
    let mut extra = vec![0xCCu8; 5];

    let copied = {
        let mut bufs = [
            IoSliceMut::new(first.as_mut_slice()),
            IoSliceMut::new(second.as_mut_slice()),
            IoSliceMut::new(extra.as_mut_slice()),
        ];
        buffer
            .copy_remaining_into_vectored(&mut bufs)
            .expect("copy should succeed with ample capacity")
    };

    assert_eq!(copied, remaining_len);
    assert_eq!(
        first.as_slice(),
        &transcript[consumed..consumed + first.len()]
    );
    assert_eq!(
        second.as_slice(),
        &transcript[consumed + first.len()..consumed + first.len() + second.len()]
    );
    assert!(extra.iter().all(|byte| *byte == 0xCC));
}

#[test]
fn negotiation_buffered_slices_into_iter_over_reference_yields_segments() {
    let prefix = b"@RSYNCD:";
    let remainder = b" reply";
    let slices = NegotiationBufferedSlices::new(prefix, remainder);

    let collected: Vec<u8> = (&slices)
        .into_iter()
        .flat_map(|slice| slice.as_ref().iter().copied())
        .collect();

    let mut expected = Vec::new();
    expected.extend_from_slice(prefix);
    expected.extend_from_slice(remainder);
    assert_eq!(collected, expected);
}

#[test]
fn negotiation_buffered_slices_into_iter_consumes_segments() {
    let prefix = b"@RSYNCD:";
    let remainder = b" banner";
    let slices = NegotiationBufferedSlices::new(prefix, remainder);

    let lengths: Vec<usize> = slices
        .into_iter()
        .map(|slice| slice.as_ref().len())
        .collect();

    assert_eq!(lengths, vec![prefix.len(), remainder.len()]);
}

#[test]
fn sniff_negotiation_detects_binary_prefix() {
    let mut stream = sniff_bytes(&[0x00, 0x12, 0x34]).expect("sniff succeeds");
    assert_eq!(stream.decision(), NegotiationPrologue::Binary);
    assert_eq!(stream.sniffed_prefix(), &[0x00]);
    assert_eq!(stream.sniffed_prefix_len(), 1);
    assert_eq!(stream.sniffed_prefix_remaining(), 1);
    assert!(!stream.legacy_prefix_complete());
    assert_eq!(stream.buffered_len(), 1);
    assert!(stream.buffered_remainder().is_empty());
    let (prefix, remainder) = stream.buffered_split();
    assert_eq!(prefix, &[0x00]);
    assert!(remainder.is_empty());

    let mut buf = [0u8; 3];
    stream
        .read_exact(&mut buf)
        .expect("read_exact drains buffered prefix and remainder");
    assert_eq!(&buf, &[0x00, 0x12, 0x34]);
    assert_eq!(stream.sniffed_prefix_remaining(), 0);
    assert!(!stream.legacy_prefix_complete());

    let mut tail = [0u8; 2];
    let read = stream
        .read(&mut tail)
        .expect("read after buffer consumes inner");
    assert_eq!(read, 0);
    assert!(tail.iter().all(|byte| *byte == 0));

    let parts = stream.into_parts();
    assert!(!parts.legacy_prefix_complete());
}

#[test]
fn ensure_decision_accepts_matching_style() {
    let stream = sniff_bytes(b"@RSYNCD: 31.0\nrest").expect("sniff succeeds");
    stream
        .ensure_decision(
            NegotiationPrologue::LegacyAscii,
            "legacy daemon negotiation requires @RSYNCD: prefix",
        )
        .expect("legacy decision matches expectation");
}

#[test]
fn ensure_decision_rejects_mismatched_style() {
    let stream = sniff_bytes(&[0x00, 0x12, 0x34]).expect("sniff succeeds");
    let err = stream
        .ensure_decision(
            NegotiationPrologue::LegacyAscii,
            "legacy daemon negotiation requires @RSYNCD: prefix",
        )
        .expect_err("binary decision must be rejected");
    assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    assert_eq!(
        err.to_string(),
        "legacy daemon negotiation requires @RSYNCD: prefix"
    );
}

#[test]
fn ensure_decision_reports_undetermined_prologue() {
    let stream = NegotiatedStream::from_raw_components(
        Cursor::new(Vec::<u8>::new()),
        NegotiationPrologue::NeedMoreData,
        0,
        0,
        Vec::new(),
    );

    let err = stream
        .ensure_decision(
            NegotiationPrologue::Binary,
            "binary negotiation requires binary prologue",
        )
        .expect_err("undetermined prologue must surface as EOF");
    assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
    assert_eq!(err.to_string(), NEGOTIATION_PROLOGUE_UNDETERMINED_MSG);
}

#[test]
fn negotiated_stream_reports_handshake_style_helpers() {
    let binary = sniff_bytes(&[0x00, 0x12, 0x34]).expect("sniff succeeds");
    assert!(binary.is_binary());
    assert!(!binary.is_legacy());

    let legacy = sniff_bytes(b"@RSYNCD: 31.0\nrest").expect("sniff succeeds");
    assert!(legacy.is_legacy());
    assert!(!legacy.is_binary());
}

#[test]
fn negotiated_stream_buffered_to_vec_matches_slice() {
    let stream = sniff_bytes(b"@RSYNCD: 31.0\nrest").expect("sniff succeeds");
    let owned = stream.buffered_to_vec().expect("allocation succeeds");
    assert_eq!(owned.as_slice(), stream.buffered());
}

#[test]
fn negotiated_stream_buffered_remaining_to_vec_matches_slice() {
    let stream = sniff_bytes(b"@RSYNCD: 31.0\nrest").expect("sniff succeeds");
    let owned = stream
        .buffered_remaining_to_vec()
        .expect("allocation succeeds");
    assert_eq!(owned.as_slice(), stream.buffered_remainder());
}

#[test]
fn negotiated_stream_parts_reports_handshake_style_helpers() {
    let binary_parts = sniff_bytes(&[0x00, 0x12, 0x34])
        .expect("sniff succeeds")
        .into_parts();
    assert!(binary_parts.is_binary());
    assert!(!binary_parts.is_legacy());

    let legacy_parts = sniff_bytes(b"@RSYNCD: 31.0\nrest")
        .expect("sniff succeeds")
        .into_parts();
    assert!(legacy_parts.is_legacy());
    assert!(!legacy_parts.is_binary());
}

#[test]
fn negotiated_stream_parts_buffered_to_vec_matches_slice() {
    let parts = sniff_bytes(b"@RSYNCD: 31.0\nrest")
        .expect("sniff succeeds")
        .into_parts();
    let owned = parts.buffered_to_vec().expect("allocation succeeds");
    assert_eq!(owned.as_slice(), parts.buffered());
}

#[test]
fn negotiated_stream_parts_buffered_remaining_to_vec_matches_slice() {
    let parts = sniff_bytes(b"@RSYNCD: 31.0\nrest")
        .expect("sniff succeeds")
        .into_parts();
    let owned = parts
        .buffered_remaining_to_vec()
        .expect("allocation succeeds");
    assert_eq!(owned.as_slice(), parts.buffered_remainder());
}

#[test]
fn buffered_consumed_tracks_reads() {
    let mut stream = sniff_bytes(b"@RSYNCD: 31.0\nremaining").expect("sniff succeeds");
    assert_eq!(stream.buffered_consumed(), 0);

    let total = stream.buffered_len();
    assert!(total > 0);

    let mut remaining = total;
    let mut scratch = [0u8; 4];
    while remaining > 0 {
        let chunk = remaining.min(scratch.len());
        let read = stream
            .read(&mut scratch[..chunk])
            .expect("buffered bytes are readable");
        assert!(read > 0);
        remaining -= read;
        assert_eq!(stream.buffered_consumed(), total - remaining);
    }

    assert_eq!(stream.buffered_consumed(), total);
}

#[test]
fn negotiated_stream_buffered_consumed_slice_exposes_replayed_bytes() {
    let mut stream = sniff_bytes(b"@RSYNCD: 31.0\nrest").expect("sniff succeeds");
    let transcript = stream.buffered().to_vec();
    assert!(stream.buffered_consumed_slice().is_empty());

    let mut prefix = vec![0u8; 4];
    stream
        .read_exact(&mut prefix)
        .expect("buffered prefix is readable");
    assert_eq!(stream.buffered_consumed_slice(), &transcript[..4]);

    let remaining = transcript.len().saturating_sub(4);
    if remaining > 0 {
        let mut tail = vec![0u8; remaining];
        stream
            .read_exact(&mut tail)
            .expect("remaining buffered bytes are readable");
    }

    assert_eq!(stream.buffered_consumed_slice(), transcript.as_slice());
}

#[test]
fn parts_buffered_consumed_matches_stream_state() {
    let mut stream = sniff_bytes(b"@RSYNCD: 31.0\nrest").expect("sniff succeeds");
    let total = stream.buffered_len();
    assert!(total > 1);

    let mut prefix = vec![0u8; total - 1];
    stream
        .read_exact(&mut prefix)
        .expect("buffered prefix is replayed");
    assert_eq!(stream.buffered_consumed(), total - 1);

    let parts = stream.into_parts();
    assert_eq!(parts.buffered_consumed(), total - 1);
    assert_eq!(parts.buffered_remaining(), 1);
}

#[test]
fn negotiated_stream_parts_buffered_consumed_slice_reflects_progress() {
    let mut stream = sniff_bytes(b"@RSYNCD: 31.0\nrest").expect("sniff succeeds");
    let transcript = stream.buffered().to_vec();

    let mut consumed_prefix = vec![0u8; 5.min(transcript.len())];
    stream
        .read_exact(&mut consumed_prefix)
        .expect("buffered prefix is readable");
    let consumed = stream.buffered_consumed();

    let parts = stream.clone().into_parts();
    assert_eq!(parts.buffered_consumed_slice(), &transcript[..consumed]);

    let remaining = transcript.len().saturating_sub(consumed);
    if remaining > 0 {
        let mut rest = vec![0u8; remaining];
        stream
            .read_exact(&mut rest)
            .expect("remaining buffered bytes are readable");
    }

    let parts = stream.into_parts();
    assert_eq!(parts.buffered_consumed_slice(), transcript.as_slice());
}

#[test]
fn negotiated_stream_buffered_remaining_slice_tracks_consumption() {
    let mut stream = sniff_bytes(b"@RSYNCD: 31.0\nrest").expect("sniff succeeds");

    let expected = {
        let (prefix, remainder) = stream.buffered_split();
        let mut combined = Vec::new();
        combined.extend_from_slice(prefix);
        combined.extend_from_slice(remainder);
        combined
    };
    assert_eq!(stream.buffered_remaining_slice(), expected.as_slice());

    let mut prefix = [0u8; 5];
    stream
        .read_exact(&mut prefix)
        .expect("buffered prefix is replayed first");
    let expected_after = {
        let (prefix_slice, remainder_slice) = stream.buffered_split();
        let mut combined = Vec::new();
        combined.extend_from_slice(prefix_slice);
        combined.extend_from_slice(remainder_slice);
        combined
    };
    assert_eq!(stream.buffered_remaining_slice(), expected_after.as_slice());

    let mut remainder = Vec::new();
    stream
        .read_to_end(&mut remainder)
        .expect("remaining buffered bytes are replayed");
    let expected_final = {
        let (prefix_slice, remainder_slice) = stream.buffered_split();
        let mut combined = Vec::new();
        combined.extend_from_slice(prefix_slice);
        combined.extend_from_slice(remainder_slice);
        combined
    };
    assert_eq!(stream.buffered_remaining_slice(), expected_final.as_slice());
    assert!(expected_final.is_empty());
}

#[test]
fn parts_buffered_remaining_slice_matches_stream_state() {
    let mut stream = sniff_bytes(b"@RSYNCD: 31.0\nreply").expect("sniff succeeds");
    let mut prefix = [0u8; 4];
    stream
        .read_exact(&mut prefix)
        .expect("buffered prefix is replayed");
    let parts = stream.into_parts();
    let expected_remaining = {
        let (prefix_slice, remainder_slice) = parts.buffered_split();
        let mut combined = Vec::new();
        combined.extend_from_slice(prefix_slice);
        combined.extend_from_slice(remainder_slice);
        combined
    };
    assert_eq!(
        parts.buffered_remaining_slice(),
        expected_remaining.as_slice()
    );

    let rebuilt = parts.into_stream();
    assert_eq!(
        rebuilt.buffered_remaining_slice(),
        expected_remaining.as_slice()
    );
}

#[test]
fn negotiated_stream_into_parts_trait_matches_method() {
    let mut via_method = sniff_bytes(b"@RSYNCD: 31.0\nrest").expect("sniff succeeds");
    let mut via_trait = sniff_bytes(b"@RSYNCD: 31.0\nrest").expect("sniff succeeds");

    let mut prefix = [0u8; 3];
    via_method
        .read_exact(&mut prefix)
        .expect("buffered prefix is replayed");
    via_trait
        .read_exact(&mut prefix)
        .expect("buffered prefix is replayed");

    let parts_method = via_method.into_parts();
    let parts_trait: NegotiatedStreamParts<_> = via_trait.into();

    assert_eq!(parts_method.decision(), parts_trait.decision());
    assert_eq!(parts_method.buffered(), parts_trait.buffered());
    assert_eq!(
        parts_method.buffered_consumed(),
        parts_trait.buffered_consumed()
    );
}

#[test]
fn negotiated_stream_parts_into_stream_trait_matches_method() {
    let data = b"@RSYNCD: 31.0\npayload";

    let mut expected_stream = sniff_bytes(data).expect("sniff succeeds");
    let mut expected_prefix = [0u8; 5];
    expected_stream
        .read_exact(&mut expected_prefix)
        .expect("buffered prefix is replayed");
    let mut expected_output = Vec::new();
    expected_stream
        .read_to_end(&mut expected_output)
        .expect("remainder is readable");

    let mut original = sniff_bytes(data).expect("sniff succeeds");
    let mut prefix = [0u8; 5];
    original
        .read_exact(&mut prefix)
        .expect("buffered prefix is replayed");
    let parts = original.into_parts();
    let clone = parts.clone();

    let mut via_method = clone.into_stream();
    let mut method_output = Vec::new();
    via_method
        .read_to_end(&mut method_output)
        .expect("method conversion retains bytes");

    let mut via_trait: NegotiatedStream<_> = parts.into();
    let mut trait_output = Vec::new();
    via_trait
        .read_to_end(&mut trait_output)
        .expect("trait conversion retains bytes");

    assert_eq!(method_output, expected_output);
    assert_eq!(trait_output, expected_output);
}

#[test]
fn sniffed_prefix_remaining_tracks_consumed_bytes() {
    let mut stream = sniff_bytes(b"@RSYNCD: 29.0\nrest").expect("sniff succeeds");
    assert_eq!(stream.sniffed_prefix_remaining(), LEGACY_DAEMON_PREFIX_LEN);
    assert!(stream.legacy_prefix_complete());

    let mut prefix_fragment = [0u8; 3];
    stream
        .read_exact(&mut prefix_fragment)
        .expect("prefix fragment is replayed first");
    assert_eq!(
        stream.sniffed_prefix_remaining(),
        LEGACY_DAEMON_PREFIX_LEN - prefix_fragment.len()
    );
    assert!(stream.legacy_prefix_complete());

    let remaining_len = LEGACY_DAEMON_PREFIX_LEN - prefix_fragment.len();
    let mut rest_of_prefix = vec![0u8; remaining_len];
    stream
        .read_exact(&mut rest_of_prefix)
        .expect("remaining prefix bytes are replayed");
    assert_eq!(stream.sniffed_prefix_remaining(), 0);
    assert_eq!(rest_of_prefix, b"YNCD:");
    assert!(stream.legacy_prefix_complete());
}

#[test]
fn sniffed_prefix_remaining_visible_on_parts() {
    let initial_parts = sniff_bytes(b"@RSYNCD: 31.0\n")
        .expect("sniff succeeds")
        .into_parts();
    assert_eq!(
        initial_parts.sniffed_prefix_remaining(),
        LEGACY_DAEMON_PREFIX_LEN
    );
    assert!(initial_parts.legacy_prefix_complete());

    let mut stream = sniff_bytes(b"@RSYNCD: 31.0\nrest").expect("sniff succeeds");
    let mut prefix_fragment = [0u8; 5];
    stream
        .read_exact(&mut prefix_fragment)
        .expect("prefix fragment is replayed");
    let parts = stream.into_parts();
    assert_eq!(
        parts.sniffed_prefix_remaining(),
        LEGACY_DAEMON_PREFIX_LEN - prefix_fragment.len()
    );
    assert!(parts.legacy_prefix_complete());
}

#[test]
fn negotiated_stream_copy_buffered_into_preserves_replay_state() {
    let mut stream = sniff_bytes(b"@RSYNCD: 31.0\ntrailing").expect("sniff succeeds");
    let expected = stream.buffered().to_vec();
    let buffered_remaining = stream.buffered_remaining();

    let mut scratch = Vec::from([0xAAu8, 0xBB]);
    let copied = stream
        .copy_buffered_into(&mut scratch)
        .expect("copying buffered bytes succeeds");

    assert_eq!(copied, expected.len());
    assert_eq!(scratch, expected);
    assert_eq!(stream.buffered_remaining(), buffered_remaining);

    let mut replay = vec![0u8; expected.len()];
    stream
        .read_exact(&mut replay)
        .expect("buffered bytes remain available after copying");
    assert_eq!(replay, expected);
}

#[test]
fn negotiated_stream_copy_buffered_into_grows_from_sparse_len() {
    let stream = sniff_bytes(b"@RSYNCD: 31.0\nlegacy daemon payload").expect("sniff succeeds");
    let expected = stream.buffered().to_vec();

    let mut scratch = Vec::with_capacity(expected.len() / 2);
    scratch.extend_from_slice(b"seed");
    scratch.truncate(1);
    assert!(scratch.capacity() < expected.len());
    assert_eq!(scratch.len(), 1);

    let copied = stream
        .copy_buffered_into(&mut scratch)
        .expect("copying buffered bytes succeeds");

    assert_eq!(copied, expected.len());
    assert_eq!(scratch, expected);
}

#[test]
fn negotiated_stream_copy_buffered_into_slice_copies_bytes() {
    let mut stream = sniff_bytes(b"@RSYNCD: 31.0\nreplay").expect("sniff succeeds");
    let expected = stream.buffered().to_vec();
    let buffered_remaining = stream.buffered_remaining();

    let mut scratch = vec![0u8; expected.len()];
    let copied = stream
        .copy_buffered_into_slice(&mut scratch)
        .expect("copying into slice succeeds");

    assert_eq!(copied, expected.len());
    assert_eq!(scratch, expected);
    assert_eq!(stream.buffered_remaining(), buffered_remaining);

    let mut replay = vec![0u8; expected.len()];
    stream
        .read_exact(&mut replay)
        .expect("buffered bytes remain available after slicing copy");
    assert_eq!(replay, expected);
}

#[test]
fn negotiated_stream_copy_buffered_remaining_into_slice_copies_unread_bytes() {
    let mut stream = sniff_bytes(b"@RSYNCD: 31.0\nreplay").expect("sniff succeeds");
    let expected = stream.buffered().to_vec();

    let mut prefix = [0u8; 4];
    stream
        .read_exact(&mut prefix)
        .expect("buffered prefix is readable");
    let consumed = stream.buffered_consumed();

    let mut scratch = [0u8; 64];
    let copied = stream
        .copy_buffered_remaining_into_slice(&mut scratch)
        .expect("copying remaining bytes succeeds");

    assert_eq!(copied, expected.len() - consumed);
    assert_eq!(&scratch[..copied], &expected[consumed..]);
    assert_eq!(stream.buffered_consumed(), consumed);
}

#[test]
fn negotiated_stream_copy_buffered_into_vec_copies_bytes() {
    let stream = sniff_bytes(b"@RSYNCD: 31.0\nreplay").expect("sniff succeeds");
    let expected = stream.buffered().to_vec();

    let mut target = Vec::with_capacity(expected.len() + 8);
    target.extend_from_slice(b"junk data");
    let initial_capacity = target.capacity();
    let initial_ptr = target.as_ptr();

    let copied = stream
        .copy_buffered_into_vec(&mut target)
        .expect("copying into vec succeeds");

    assert_eq!(copied, expected.len());
    assert_eq!(target, expected);
    assert_eq!(target.capacity(), initial_capacity);
    assert_eq!(target.as_ptr(), initial_ptr);
}

#[test]
fn negotiated_stream_copy_buffered_remaining_into_vec_copies_unread_bytes() {
    let mut stream = sniff_bytes(b"@RSYNCD: 31.0\nreplay").expect("sniff succeeds");
    let expected = stream.buffered().to_vec();

    let mut prefix = [0u8; 4];
    stream
        .read_exact(&mut prefix)
        .expect("buffered prefix is readable");
    let consumed = stream.buffered_consumed();

    let mut target = Vec::new();
    let copied = stream
        .copy_buffered_remaining_into_vec(&mut target)
        .expect("copying remaining bytes succeeds");

    assert_eq!(copied, expected.len() - consumed);
    assert_eq!(target, &expected[consumed..]);
    assert_eq!(stream.buffered_consumed(), consumed);
}

#[test]
fn negotiated_stream_copy_helpers_accept_empty_buffers() {
    let stream = NegotiatedStream::from_raw_parts(
        Cursor::new(Vec::<u8>::new()),
        NegotiationPrologue::Binary,
        0,
        0,
        Vec::new(),
    );

    let mut cleared = vec![0xAA];
    let copied_vec = stream
        .copy_buffered_into_vec(&mut cleared)
        .expect("copying into vec succeeds");
    assert_eq!(copied_vec, 0);
    assert!(cleared.is_empty());

    let mut replaced = vec![0xBB];
    let copied = stream
        .copy_buffered_into(&mut replaced)
        .expect("copying into vec succeeds");
    assert_eq!(copied, 0);
    assert!(replaced.is_empty());

    let mut appended = b"log".to_vec();
    let extended = stream
        .extend_buffered_into_vec(&mut appended)
        .expect("extending into vec succeeds");
    assert_eq!(extended, 0);
    assert_eq!(appended, b"log");

    let mut slice = [0xCC; 4];
    let copied_slice = stream
        .copy_buffered_into_slice(&mut slice)
        .expect("copying into slice succeeds");
    assert_eq!(copied_slice, 0);
    assert_eq!(slice, [0xCC; 4]);

    let mut array = [0xDD; 2];
    let copied_array = stream
        .copy_buffered_into_array(&mut array)
        .expect("copying into array succeeds");
    assert_eq!(copied_array, 0);
    assert_eq!(array, [0xDD; 2]);

    let mut vectored = [IoSliceMut::new(&mut slice[..])];
    let copied_vectored = stream
        .copy_buffered_into_vectored(&mut vectored)
        .expect("empty buffer copies successfully");
    assert_eq!(copied_vectored, 0);
    assert_eq!(slice, [0xCC; 4]);

    let mut output = Vec::new();
    let written = stream
        .copy_buffered_into_writer(&mut output)
        .expect("writing into vec succeeds");
    assert_eq!(written, 0);
    assert!(output.is_empty());
}

#[test]
fn negotiated_stream_extend_buffered_into_vec_appends_bytes() {
    let stream = sniff_bytes(b"@RSYNCD: 31.0\nextend").expect("sniff succeeds");
    let expected = stream.buffered().to_vec();
    let buffered_remaining = stream.buffered_remaining();

    let mut target = b"prefix: ".to_vec();
    let initial_capacity = target.capacity();

    let appended = stream
        .extend_buffered_into_vec(&mut target)
        .expect("vector can reserve space for replay bytes");

    assert_eq!(appended, expected.len());
    assert!(target.capacity() >= initial_capacity);
    let mut expected_target = b"prefix: ".to_vec();
    expected_target.extend_from_slice(&expected);
    assert_eq!(target, expected_target);
    assert_eq!(stream.buffered_remaining(), buffered_remaining);
}

#[test]
fn negotiated_stream_extend_buffered_remaining_into_vec_appends_unread_bytes() {
    let mut stream = sniff_bytes(b"@RSYNCD: 31.0\nextend").expect("sniff succeeds");
    let expected = stream.buffered().to_vec();

    let mut prefix = [0u8; 8];
    stream
        .read_exact(&mut prefix)
        .expect("buffered prefix is readable");
    let consumed = stream.buffered_consumed();

    let mut target = b"prefix: ".to_vec();
    let appended = stream
        .extend_buffered_remaining_into_vec(&mut target)
        .expect("extending remaining bytes succeeds");

    assert_eq!(appended, expected.len() - consumed);
    let mut expected_target = b"prefix: ".to_vec();
    expected_target.extend_from_slice(&expected[consumed..]);
    assert_eq!(target, expected_target);
    assert_eq!(stream.buffered_consumed(), consumed);
}

#[test]
fn negotiated_stream_copy_buffered_into_array_copies_bytes() {
    let mut stream = sniff_bytes(b"@RSYNCD: 31.0\narray").expect("sniff succeeds");
    let expected = stream.buffered().to_vec();
    let buffered_remaining = stream.buffered_remaining();

    let mut scratch = [0u8; 64];
    let copied = stream
        .copy_buffered_into_array(&mut scratch)
        .expect("copying into array succeeds");

    assert_eq!(copied, expected.len());
    assert_eq!(&scratch[..copied], expected.as_slice());
    assert_eq!(stream.buffered_remaining(), buffered_remaining);

    let mut replay = vec![0u8; expected.len()];
    stream
        .read_exact(&mut replay)
        .expect("buffered bytes remain available after array copy");
    assert_eq!(replay, expected);
}

#[test]
fn negotiated_stream_copy_buffered_into_vectored_copies_bytes() {
    let mut stream = sniff_bytes(b"@RSYNCD: 31.0\nvectored payload").expect("sniff succeeds");
    let expected = stream.buffered().to_vec();
    let buffered_remaining = stream.buffered_remaining();

    let mut prefix = [0u8; 12];
    let mut suffix = [0u8; 64];
    let mut bufs = [IoSliceMut::new(&mut prefix), IoSliceMut::new(&mut suffix)];
    let copied = stream
        .copy_buffered_into_vectored(&mut bufs)
        .expect("vectored copy succeeds");

    assert_eq!(copied, expected.len());

    let prefix_len = prefix.len().min(copied);
    let remainder_len = copied - prefix_len;
    let mut assembled = Vec::new();
    assembled.extend_from_slice(&prefix[..prefix_len]);
    if remainder_len > 0 {
        assembled.extend_from_slice(&suffix[..remainder_len]);
    }
    assert_eq!(assembled, expected);
    assert_eq!(stream.buffered_remaining(), buffered_remaining);

    let mut replay = vec![0u8; expected.len()];
    stream
        .read_exact(&mut replay)
        .expect("buffered bytes remain available after vectored copy");
    assert_eq!(replay, expected);
}

#[test]
fn negotiated_stream_copy_buffered_remaining_into_vectored_copies_unread_bytes() {
    let mut stream = sniff_bytes(b"@RSYNCD: 31.0\nvectored payload").expect("sniff succeeds");
    let expected = stream.buffered().to_vec();

    let mut consumed_prefix = [0u8; 9];
    stream
        .read_exact(&mut consumed_prefix)
        .expect("buffered prefix is readable");
    let consumed = stream.buffered_consumed();

    let mut prefix = [0u8; 12];
    let mut suffix = [0u8; 64];
    let mut bufs = [IoSliceMut::new(&mut prefix), IoSliceMut::new(&mut suffix)];
    let copied = stream
        .copy_buffered_remaining_into_vectored(&mut bufs)
        .expect("vectored copy of remaining bytes succeeds");

    assert_eq!(copied, expected.len() - consumed);

    let prefix_len = prefix.len().min(copied);
    let remainder_len = copied - prefix_len;
    let mut assembled = Vec::new();
    assembled.extend_from_slice(&prefix[..prefix_len]);
    if remainder_len > 0 {
        assembled.extend_from_slice(&suffix[..remainder_len]);
    }
    assert_eq!(assembled, expected[consumed..]);
    assert_eq!(stream.buffered_consumed(), consumed);
}

#[test]
fn negotiated_stream_copy_buffered_into_vectored_reports_small_buffers() {
    let stream = sniff_bytes(b"@RSYNCD: 31.0\nshort").expect("sniff succeeds");
    let required = stream.buffered().len();

    let mut prefix = [0u8; 4];
    let mut suffix = [0u8; 3];
    let mut bufs = [IoSliceMut::new(&mut prefix), IoSliceMut::new(&mut suffix)];
    let err = stream
        .copy_buffered_into_vectored(&mut bufs)
        .expect_err("insufficient capacity must error");

    assert_eq!(err.required(), required);
    assert_eq!(err.provided(), prefix.len() + suffix.len());
    assert_eq!(err.missing(), required - (prefix.len() + suffix.len()));
}

#[test]
fn negotiated_stream_copy_buffered_into_vectored_preserves_buffers_on_error() {
    let stream = sniff_bytes(b"@RSYNCD: 31.0\nshort").expect("sniff succeeds");
    let buffered_remaining = stream.buffered_remaining();

    let mut prefix = [0x11u8; 4];
    let mut suffix = [0x22u8; 3];
    let original_prefix = prefix;
    let original_suffix = suffix;
    let mut bufs = [IoSliceMut::new(&mut prefix), IoSliceMut::new(&mut suffix)];

    stream
        .copy_buffered_into_vectored(&mut bufs)
        .expect_err("insufficient capacity must error");

    assert_eq!(prefix, original_prefix);
    assert_eq!(suffix, original_suffix);
    assert_eq!(stream.buffered_remaining(), buffered_remaining);
}

#[test]
fn negotiated_stream_copy_buffered_into_slice_reports_small_buffer() {
    let stream = sniff_bytes(b"@RSYNCD: 30.0\nrest").expect("sniff succeeds");
    let expected_len = stream.buffered_len();
    let buffered_remaining = stream.buffered_remaining();

    let mut scratch = vec![0u8; expected_len.saturating_sub(1)];
    let err = stream
        .copy_buffered_into_slice(&mut scratch)
        .expect_err("insufficient slice capacity must error");

    assert_eq!(err.required(), expected_len);
    assert_eq!(err.provided(), scratch.len());
    assert_eq!(err.missing(), expected_len - scratch.len());
    assert_eq!(stream.buffered_remaining(), buffered_remaining);
}

#[test]
fn negotiated_stream_copy_buffered_into_array_reports_small_array() {
    let stream = sniff_bytes(b"@RSYNCD: 31.0\nrest").expect("sniff succeeds");
    let expected_len = stream.buffered_len();
    let buffered_remaining = stream.buffered_remaining();

    let mut scratch = [0u8; 4];
    let err = stream
        .copy_buffered_into_array(&mut scratch)
        .expect_err("insufficient array capacity must error");

    assert_eq!(err.required(), expected_len);
    assert_eq!(err.provided(), scratch.len());
    assert_eq!(err.missing(), expected_len - scratch.len());
    assert_eq!(stream.buffered_remaining(), buffered_remaining);
}

#[test]
fn negotiated_stream_copy_buffered_into_writer_copies_bytes() {
    let stream = sniff_bytes(b"@RSYNCD: 31.0\npayload").expect("sniff succeeds");
    let expected = stream.buffered().to_vec();
    let buffered_remaining = stream.buffered_remaining();

    let mut output = Vec::new();
    let written = stream
        .copy_buffered_into_writer(&mut output)
        .expect("writing buffered bytes succeeds");

    assert_eq!(written, expected.len());
    assert_eq!(output, expected);
    assert_eq!(stream.buffered_remaining(), buffered_remaining);
}

#[test]
fn negotiated_stream_copy_buffered_remaining_into_writer_copies_unread_bytes() {
    let mut stream = sniff_bytes(b"@RSYNCD: 31.0\npayload").expect("sniff succeeds");
    let expected = stream.buffered().to_vec();

    let mut consumed_prefix = [0u8; 6];
    stream
        .read_exact(&mut consumed_prefix)
        .expect("buffered prefix is readable");
    let consumed = stream.buffered_consumed();

    let mut output = Vec::new();
    let written = stream
        .copy_buffered_remaining_into_writer(&mut output)
        .expect("writing remaining bytes succeeds");

    assert_eq!(written, expected.len() - consumed);
    assert_eq!(output, expected[consumed..]);
    assert_eq!(stream.buffered_consumed(), consumed);
}

#[test]
fn negotiated_stream_parts_copy_buffered_into_preserves_replay_state() {
    let parts = sniff_bytes(b"@RSYNCD: 30.0\nleftovers")
        .expect("sniff succeeds")
        .into_parts();
    let expected = parts.buffered().to_vec();

    let mut scratch = Vec::with_capacity(1);
    scratch.extend_from_slice(b"junk");
    let copied = parts
        .copy_buffered_into(&mut scratch)
        .expect("copying buffered bytes succeeds");

    assert_eq!(copied, expected.len());
    assert_eq!(scratch, expected);

    let mut rebuilt = parts.into_stream();
    let mut replay = vec![0u8; expected.len()];
    rebuilt
        .read_exact(&mut replay)
        .expect("rebuilt stream still replays buffered bytes");
    assert_eq!(replay, expected);
}

#[test]
fn negotiated_stream_parts_copy_buffered_into_slice_copies_bytes() {
    let parts = sniff_bytes(b"@RSYNCD: 30.0\nlisting")
        .expect("sniff succeeds")
        .into_parts();
    let expected = parts.buffered().to_vec();

    let mut scratch = vec![0u8; expected.len()];
    let copied = parts
        .copy_buffered_into_slice(&mut scratch)
        .expect("copying into slice succeeds");

    assert_eq!(copied, expected.len());
    assert_eq!(scratch, expected);

    let mut rebuilt = parts.into_stream();
    let mut replay = vec![0u8; expected.len()];
    rebuilt
        .read_exact(&mut replay)
        .expect("rebuilt stream still replays buffered bytes after slice copy");
    assert_eq!(replay, expected);
}

#[test]
fn negotiated_stream_parts_copy_buffered_remaining_into_slice_copies_unread_bytes() {
    let mut stream = sniff_bytes(b"@RSYNCD: 30.0\nlisting").expect("sniff succeeds");
    let expected = stream.buffered().to_vec();

    let mut prefix = [0u8; 5];
    stream
        .read_exact(&mut prefix)
        .expect("buffered prefix is readable");
    let consumed = stream.buffered_consumed();

    let parts = stream.into_parts();

    let mut scratch = vec![0u8; expected.len()];
    let copied = parts
        .copy_buffered_remaining_into_slice(&mut scratch)
        .expect("copying remaining bytes succeeds");

    assert_eq!(copied, expected.len() - consumed);
    assert_eq!(&scratch[..copied], &expected[consumed..]);
    assert_eq!(parts.buffered_consumed(), consumed);
}

#[test]
fn negotiated_stream_parts_copy_buffered_into_vec_copies_bytes() {
    let parts = sniff_bytes(b"@RSYNCD: 30.0\nlisting")
        .expect("sniff succeeds")
        .into_parts();
    let expected = parts.buffered().to_vec();

    let vectored = parts.buffered_vectored();
    assert_eq!(vectored.len(), expected.len());

    let flattened: Vec<u8> = vectored
        .iter()
        .flat_map(|slice| slice.as_ref().iter().copied())
        .collect();
    assert_eq!(flattened, expected);

    let mut target = Vec::with_capacity(expected.len() + 8);
    target.extend_from_slice(b"junk data");
    let initial_capacity = target.capacity();
    let initial_ptr = target.as_ptr();

    let copied = parts
        .copy_buffered_into_vec(&mut target)
        .expect("copying into vec succeeds");

    assert_eq!(copied, expected.len());
    assert_eq!(target, expected);
    assert_eq!(target.capacity(), initial_capacity);
    assert_eq!(target.as_ptr(), initial_ptr);
}

#[test]
fn negotiated_stream_parts_copy_buffered_remaining_into_vec_copies_unread_bytes() {
    let mut stream = sniff_bytes(b"@RSYNCD: 30.0\nlisting").expect("sniff succeeds");
    let expected = stream.buffered().to_vec();

    let mut prefix = [0u8; 5];
    stream
        .read_exact(&mut prefix)
        .expect("buffered prefix is readable");
    let consumed = stream.buffered_consumed();

    let parts = stream.into_parts();

    let remaining_vectored = parts.buffered_remaining_vectored();
    assert_eq!(remaining_vectored.len(), expected.len() - consumed);

    let flattened: Vec<u8> = remaining_vectored
        .iter()
        .flat_map(|slice| slice.as_ref().iter().copied())
        .collect();
    assert_eq!(flattened, expected[consumed..]);

    let mut target = Vec::new();
    let copied = parts
        .copy_buffered_remaining_into_vec(&mut target)
        .expect("copying remaining bytes succeeds");

    assert_eq!(copied, expected.len() - consumed);
    assert_eq!(target, expected[consumed..]);
    assert_eq!(parts.buffered_consumed(), consumed);
}

#[test]
fn negotiated_stream_parts_buffered_vectored_handles_manual_components() {
    let cursor = Cursor::new(Vec::<u8>::new());
    let stream = NegotiatedStream::from_raw_components(
        cursor,
        NegotiationPrologue::Binary,
        1,
        0,
        vec![0x11, b'm', b'n'],
    );

    let parts = stream.into_parts();
    let vectored = parts.buffered_vectored();
    assert_eq!(vectored.segment_count(), 2);
    let slices: Vec<&[u8]> = vectored.iter().map(|slice| slice.as_ref()).collect();
    assert_eq!(slices, vec![&[0x11][..], &b"mn"[..]]);
}

#[test]
fn negotiated_stream_parts_buffered_remaining_vectored_handles_consumed_prefix() {
    let cursor = Cursor::new(Vec::<u8>::new());
    let mut stream = NegotiatedStream::from_raw_components(
        cursor,
        NegotiationPrologue::Binary,
        2,
        0,
        vec![0x22, 0x33, b'p', b'q'],
    );

    let mut buf = [0u8; 1];
    stream
        .read_exact(&mut buf)
        .expect("reading consumes part of the prefix");
    assert_eq!(&buf, &[0x22]);

    let parts = stream.into_parts();
    let remaining = parts.buffered_remaining_vectored();
    assert_eq!(remaining.segment_count(), 2);
    let slices: Vec<&[u8]> = remaining.iter().map(|slice| slice.as_ref()).collect();
    assert_eq!(slices, vec![&[0x33][..], &b"pq"[..]]);
}

#[test]
fn negotiated_stream_parts_copy_helpers_accept_empty_buffers() {
    let stream = NegotiatedStream::from_raw_parts(
        Cursor::new(Vec::<u8>::new()),
        NegotiationPrologue::Binary,
        0,
        0,
        Vec::new(),
    );
    let parts = stream.into_parts();

    assert_eq!(parts.buffered_len(), 0);
    assert_eq!(parts.sniffed_prefix_len(), 0);
    assert!(parts.buffered().is_empty());

    let mut cleared = vec![0xAA];
    let copied_vec = parts
        .copy_buffered_into_vec(&mut cleared)
        .expect("copying into vec succeeds");
    assert_eq!(copied_vec, 0);
    assert!(cleared.is_empty());

    let mut replaced = vec![0xBB];
    let copied = parts
        .copy_buffered_into(&mut replaced)
        .expect("copying into vec succeeds");
    assert_eq!(copied, 0);
    assert!(replaced.is_empty());

    let mut appended = b"log".to_vec();
    let extended = parts
        .extend_buffered_into_vec(&mut appended)
        .expect("extending into vec succeeds");
    assert_eq!(extended, 0);
    assert_eq!(appended, b"log");

    let mut slice = [0xCC; 4];
    let copied_slice = parts
        .copy_buffered_into_slice(&mut slice)
        .expect("copying into slice succeeds");
    assert_eq!(copied_slice, 0);
    assert_eq!(slice, [0xCC; 4]);

    let mut array = [0xDD; 2];
    let copied_array = parts
        .copy_buffered_into_array(&mut array)
        .expect("copying into array succeeds");
    assert_eq!(copied_array, 0);
    assert_eq!(array, [0xDD; 2]);

    let mut vectored = [IoSliceMut::new(&mut slice[..])];
    let copied_vectored = parts
        .copy_buffered_into_vectored(&mut vectored)
        .expect("empty buffer copies successfully");
    assert_eq!(copied_vectored, 0);
    assert_eq!(slice, [0xCC; 4]);

    let mut output = Vec::new();
    let written = parts
        .copy_buffered_into_writer(&mut output)
        .expect("writing into vec succeeds");
    assert_eq!(written, 0);
    assert!(output.is_empty());
}

#[test]
fn negotiated_stream_parts_extend_buffered_into_vec_appends_bytes() {
    let parts = sniff_bytes(b"@RSYNCD: 30.0\next parts")
        .expect("sniff succeeds")
        .into_parts();
    let expected = parts.buffered().to_vec();
    let buffered_remaining = parts.buffered_remaining();

    let mut target = b"parts: ".to_vec();
    let initial_capacity = target.capacity();

    let appended = parts
        .extend_buffered_into_vec(&mut target)
        .expect("vector can reserve space for replay bytes");

    assert_eq!(appended, expected.len());
    assert!(target.capacity() >= initial_capacity);
    let mut expected_target = b"parts: ".to_vec();
    expected_target.extend_from_slice(&expected);
    assert_eq!(target, expected_target);
    assert_eq!(parts.buffered_remaining(), buffered_remaining);
}

#[test]
fn negotiated_stream_parts_extend_buffered_remaining_into_vec_appends_unread_bytes() {
    let mut stream = sniff_bytes(b"@RSYNCD: 30.0\next parts").expect("sniff succeeds");
    let expected = stream.buffered().to_vec();

    let mut prefix = [0u8; 7];
    stream
        .read_exact(&mut prefix)
        .expect("buffered prefix is readable");
    let consumed = stream.buffered_consumed();

    let parts = stream.into_parts();

    let mut target = b"parts: ".to_vec();
    let appended = parts
        .extend_buffered_remaining_into_vec(&mut target)
        .expect("extending remaining bytes succeeds");

    assert_eq!(appended, expected.len() - consumed);
    let mut expected_target = b"parts: ".to_vec();
    expected_target.extend_from_slice(&expected[consumed..]);
    assert_eq!(target, expected_target);
    assert_eq!(parts.buffered_consumed(), consumed);
}

#[test]
fn negotiated_stream_parts_copy_buffered_into_array_copies_bytes() {
    let parts = sniff_bytes(b"@RSYNCD: 30.0\nlisting")
        .expect("sniff succeeds")
        .into_parts();
    let expected = parts.buffered().to_vec();

    let mut scratch = [0u8; 64];
    let copied = parts
        .copy_buffered_into_array(&mut scratch)
        .expect("copying into array succeeds");

    assert_eq!(copied, expected.len());
    assert_eq!(&scratch[..copied], expected.as_slice());

    let mut rebuilt = parts.into_stream();
    let mut replay = vec![0u8; expected.len()];
    rebuilt
        .read_exact(&mut replay)
        .expect("rebuilt stream still replays buffered bytes after array copy");
    assert_eq!(replay, expected);
}

#[test]
fn negotiated_stream_parts_copy_buffered_into_vectored_copies_bytes() {
    let parts = sniff_bytes(b"@RSYNCD: 30.0\nrecord")
        .expect("sniff succeeds")
        .into_parts();
    let expected = parts.buffered().to_vec();

    let mut first = [0u8; 10];
    let mut second = [0u8; 64];
    let mut bufs = [IoSliceMut::new(&mut first), IoSliceMut::new(&mut second)];
    let copied = parts
        .copy_buffered_into_vectored(&mut bufs)
        .expect("vectored copy succeeds");

    assert_eq!(copied, expected.len());

    let prefix_len = first.len().min(copied);
    let remainder_len = copied - prefix_len;
    let mut assembled = Vec::new();
    assembled.extend_from_slice(&first[..prefix_len]);
    if remainder_len > 0 {
        assembled.extend_from_slice(&second[..remainder_len]);
    }
    assert_eq!(assembled, expected);
}

#[test]
fn negotiated_stream_parts_copy_buffered_remaining_into_vectored_copies_unread_bytes() {
    let mut stream = sniff_bytes(b"@RSYNCD: 30.0\nrecord").expect("sniff succeeds");
    let expected = stream.buffered().to_vec();

    let mut prefix_buf = [0u8; 4];
    stream
        .read_exact(&mut prefix_buf)
        .expect("buffered prefix is readable");
    let consumed = stream.buffered_consumed();

    let parts = stream.into_parts();

    let mut first = [0u8; 10];
    let mut second = [0u8; 64];
    let mut bufs = [IoSliceMut::new(&mut first), IoSliceMut::new(&mut second)];
    let copied = parts
        .copy_buffered_remaining_into_vectored(&mut bufs)
        .expect("copying remaining bytes succeeds");

    assert_eq!(copied, expected.len() - consumed);

    let prefix_len = first.len().min(copied);
    let remainder_len = copied - prefix_len;
    let mut assembled = Vec::new();
    assembled.extend_from_slice(&first[..prefix_len]);
    if remainder_len > 0 {
        assembled.extend_from_slice(&second[..remainder_len]);
    }
    assert_eq!(assembled, expected[consumed..]);
    assert_eq!(parts.buffered_consumed(), consumed);
}

#[test]
fn negotiated_stream_parts_copy_buffered_into_vectored_reports_small_buffers() {
    let parts = sniff_bytes(b"@RSYNCD: 31.0\nlimited")
        .expect("sniff succeeds")
        .into_parts();
    let expected_len = parts.buffered().len();

    let mut first = [0u8; 4];
    let mut second = [0u8; 3];
    let mut bufs = [IoSliceMut::new(&mut first), IoSliceMut::new(&mut second)];
    let err = parts
        .copy_buffered_into_vectored(&mut bufs)
        .expect_err("insufficient capacity must error");

    assert_eq!(err.required(), expected_len);
    assert_eq!(err.provided(), first.len() + second.len());
    assert_eq!(err.missing(), expected_len - (first.len() + second.len()));
}

#[test]
fn negotiated_stream_parts_copy_buffered_into_vectored_preserves_buffers_on_error() {
    let parts = sniff_bytes(b"@RSYNCD: 31.0\nlimited")
        .expect("sniff succeeds")
        .into_parts();
    let buffered_snapshot = parts.buffered().to_vec();
    let consumed_before = parts.buffered_consumed();

    let mut first = [0x11u8; 4];
    let mut second = [0x22u8; 3];
    let original_first = first;
    let original_second = second;
    let mut bufs = [IoSliceMut::new(&mut first), IoSliceMut::new(&mut second)];

    parts
        .copy_buffered_into_vectored(&mut bufs)
        .expect_err("insufficient capacity must error");

    assert_eq!(first, original_first);
    assert_eq!(second, original_second);
    assert_eq!(parts.buffered(), buffered_snapshot.as_slice());
    assert_eq!(parts.buffered_consumed(), consumed_before);
}

#[test]
fn negotiated_stream_parts_copy_buffered_into_slice_reports_small_buffer() {
    let parts = sniff_bytes(b"@RSYNCD: 31.0\nlisting")
        .expect("sniff succeeds")
        .into_parts();
    let expected_len = parts.buffered_len();

    let mut scratch = vec![0u8; expected_len.saturating_sub(1)];
    let err = parts
        .copy_buffered_into_slice(&mut scratch)
        .expect_err("insufficient slice capacity must error");

    assert_eq!(err.required(), expected_len);
    assert_eq!(err.provided(), scratch.len());
    assert_eq!(err.missing(), expected_len - scratch.len());
}

#[test]
fn negotiated_stream_parts_copy_buffered_into_array_reports_small_array() {
    let parts = sniff_bytes(b"@RSYNCD: 31.0\nlisting")
        .expect("sniff succeeds")
        .into_parts();
    let expected_len = parts.buffered_len();

    let mut scratch = [0u8; 4];
    let err = parts
        .copy_buffered_into_array(&mut scratch)
        .expect_err("insufficient array capacity must error");

    assert_eq!(err.required(), expected_len);
    assert_eq!(err.provided(), scratch.len());
    assert_eq!(err.missing(), expected_len - scratch.len());
}

#[test]
fn negotiated_stream_parts_copy_buffered_into_writer_copies_bytes() {
    let parts = sniff_bytes(b"@RSYNCD: 30.0\ntrailing")
        .expect("sniff succeeds")
        .into_parts();
    let expected = parts.buffered().to_vec();

    let mut output = Vec::new();
    let written = parts
        .copy_buffered_into_writer(&mut output)
        .expect("writing buffered bytes succeeds");

    assert_eq!(written, expected.len());
    assert_eq!(output, expected);

    let mut rebuilt = parts.into_stream();
    let mut replay = vec![0u8; expected.len()];
    rebuilt
        .read_exact(&mut replay)
        .expect("rebuilt stream still replays buffered bytes after writer copy");
    assert_eq!(replay, expected);
}

#[test]
fn negotiated_stream_parts_copy_buffered_remaining_into_writer_copies_unread_bytes() {
    let mut stream = sniff_bytes(b"@RSYNCD: 30.0\ntrailing").expect("sniff succeeds");
    let expected = stream.buffered().to_vec();

    let mut prefix = [0u8; 4];
    stream
        .read_exact(&mut prefix)
        .expect("buffered prefix is readable");
    let consumed = stream.buffered_consumed();

    let parts = stream.into_parts();

    let mut output = Vec::new();
    let written = parts
        .copy_buffered_remaining_into_writer(&mut output)
        .expect("writing remaining bytes succeeds");

    assert_eq!(written, expected.len() - consumed);
    assert_eq!(output, expected[consumed..]);
    assert_eq!(parts.buffered_consumed(), consumed);
}

#[test]
fn legacy_prefix_complete_reports_status_for_legacy_sessions() {
    let mut stream = sniff_bytes(b"@RSYNCD: 30.0\nrest").expect("sniff succeeds");
    assert!(stream.legacy_prefix_complete());

    let mut consumed = [0u8; 4];
    stream
        .read_exact(&mut consumed)
        .expect("read_exact consumes part of the prefix");
    assert!(stream.legacy_prefix_complete());

    let parts = stream.into_parts();
    assert!(parts.legacy_prefix_complete());
}

#[test]
fn legacy_prefix_complete_reports_status_for_binary_sessions() {
    let mut stream = sniff_bytes(&[0x00, 0x42, 0x99]).expect("sniff succeeds");
    assert!(!stream.legacy_prefix_complete());

    let mut consumed = [0u8; 1];
    stream
        .read_exact(&mut consumed)
        .expect("read_exact consumes buffered byte");
    assert!(!stream.legacy_prefix_complete());

    let parts = stream.into_parts();
    assert!(!parts.legacy_prefix_complete());
}

#[test]
fn sniff_negotiation_detects_legacy_prefix_and_preserves_remainder() {
    let legacy = b"@RSYNCD: 31.0\n#list";
    let mut stream = sniff_bytes(legacy).expect("sniff succeeds");
    assert_eq!(stream.decision(), NegotiationPrologue::LegacyAscii);
    assert_eq!(stream.sniffed_prefix(), b"@RSYNCD:");
    assert_eq!(stream.sniffed_prefix_len(), LEGACY_DAEMON_PREFIX_LEN);
    assert_eq!(stream.buffered_len(), LEGACY_DAEMON_PREFIX_LEN);
    assert!(stream.buffered_remainder().is_empty());
    let (prefix, remainder) = stream.buffered_split();
    assert_eq!(prefix, b"@RSYNCD:");
    assert!(remainder.is_empty());

    let mut replay = Vec::new();
    stream
        .read_to_end(&mut replay)
        .expect("read_to_end succeeds");
    assert_eq!(replay, legacy);
}

#[test]
fn try_map_inner_transforms_transport_without_losing_buffer() {
    let legacy = b"@RSYNCD: 31.0\nrest";
    let stream = sniff_bytes(legacy).expect("sniff succeeds");

    let mut mapped = stream
        .try_map_inner(
            |cursor| -> Result<RecordingTransport, (io::Error, Cursor<Vec<u8>>)> {
                Ok(RecordingTransport::from_cursor(cursor))
            },
        )
        .expect("mapping succeeds");

    let mut replay = Vec::new();
    mapped
        .read_to_end(&mut replay)
        .expect("replay remains available");
    assert_eq!(replay, legacy);

    mapped.write_all(b"payload").expect("writes propagate");
    mapped.flush().expect("flush propagates");
    assert_eq!(mapped.inner().writes(), b"payload");
    assert_eq!(mapped.inner().flushes(), 1);
}

#[test]
fn try_map_inner_preserves_original_on_error() {
    let legacy = b"@RSYNCD: 31.0\n";
    let stream = sniff_bytes(legacy).expect("sniff succeeds");

    let err = stream
        .try_map_inner(
            |cursor| -> Result<RecordingTransport, (io::Error, Cursor<Vec<u8>>)> {
                Err((io::Error::other("boom"), cursor))
            },
        )
        .expect_err("mapping fails");

    assert_eq!(err.error().kind(), io::ErrorKind::Other);
    let mut original = err.into_original();
    let mut replay = Vec::new();
    original
        .read_to_end(&mut replay)
        .expect("original stream still readable");
    assert_eq!(replay, legacy);
}

#[test]
fn negotiated_stream_try_clone_with_clones_inner_reader() {
    let legacy = b"@RSYNCD: 31.0\nreply";
    let mut stream = sniff_bytes(legacy).expect("sniff succeeds");
    let expected_buffer = stream.buffered().to_vec();

    let mut cloned = stream
        .try_clone_with(|cursor| Ok::<Cursor<Vec<u8>>, io::Error>(cursor.clone()))
        .expect("cursor clone succeeds");

    assert_eq!(cloned.decision(), stream.decision());
    assert_eq!(cloned.buffered(), expected_buffer.as_slice());

    let mut cloned_replay = Vec::new();
    cloned
        .read_to_end(&mut cloned_replay)
        .expect("cloned stream can replay handshake");
    assert_eq!(cloned_replay, legacy);

    let mut original_replay = Vec::new();
    stream
        .read_to_end(&mut original_replay)
        .expect("original stream remains usable after clone");
    assert_eq!(original_replay, legacy);
}

#[test]
fn negotiated_stream_try_clone_with_propagates_error_without_side_effects() {
    let legacy = b"@RSYNCD: 31.0\nrest";
    let mut stream = sniff_bytes(legacy).expect("sniff succeeds");
    let expected_len = stream.buffered_len();

    let err = stream
        .try_clone_with(|_| Err::<Cursor<Vec<u8>>, io::Error>(io::Error::other("boom")))
        .expect_err("clone should propagate errors");
    assert_eq!(err.kind(), io::ErrorKind::Other);
    assert_eq!(stream.buffered_len(), expected_len);

    let mut replay = Vec::new();
    stream
        .read_to_end(&mut replay)
        .expect("original stream remains readable after failed clone");
    assert_eq!(replay, legacy);
}

#[test]
fn try_map_inner_on_parts_transforms_transport() {
    let legacy = b"@RSYNCD: 31.0\nrest";
    let parts = sniff_bytes(legacy).expect("sniff succeeds").into_parts();

    let mapped = parts
        .try_map_inner(
            |cursor| -> Result<RecordingTransport, (io::Error, Cursor<Vec<u8>>)> {
                Ok(RecordingTransport::from_cursor(cursor))
            },
        )
        .expect("mapping succeeds");

    let mut replay = Vec::new();
    mapped
        .into_stream()
        .read_to_end(&mut replay)
        .expect("stream reconstruction works");
    assert_eq!(replay, legacy);
}

#[test]
fn try_map_inner_on_parts_preserves_original_on_error() {
    let legacy = b"@RSYNCD: 31.0\n";
    let parts = sniff_bytes(legacy).expect("sniff succeeds").into_parts();

    let err = parts
        .try_map_inner(
            |cursor| -> Result<RecordingTransport, (io::Error, Cursor<Vec<u8>>)> {
                Err((io::Error::other("boom"), cursor))
            },
        )
        .expect_err("mapping fails");

    assert_eq!(err.error().kind(), io::ErrorKind::Other);
    let mut original = err.into_original().into_stream();
    let mut replay = Vec::new();
    original
        .read_to_end(&mut replay)
        .expect("original stream still readable");
    assert_eq!(replay, legacy);
}

#[test]
fn negotiated_stream_parts_try_clone_with_clones_inner_reader() {
    let legacy = b"@RSYNCD: 30.0\nlisting";
    let parts = sniff_bytes(legacy).expect("sniff succeeds").into_parts();
    let expected_buffer = parts.buffered().to_vec();

    let cloned = parts
        .try_clone_with(|cursor| Ok::<Cursor<Vec<u8>>, io::Error>(cursor.clone()))
        .expect("cursor clone succeeds");

    assert_eq!(cloned.decision(), parts.decision());
    assert_eq!(cloned.buffered(), expected_buffer.as_slice());

    let mut cloned_stream = cloned.into_stream();
    let mut cloned_replay = Vec::new();
    cloned_stream
        .read_to_end(&mut cloned_replay)
        .expect("cloned parts replay buffered bytes");
    assert_eq!(cloned_replay, legacy);

    let mut original_stream = parts.clone().into_stream();
    let mut original_replay = Vec::new();
    original_stream
        .read_to_end(&mut original_replay)
        .expect("original parts remain usable after clone");
    assert_eq!(original_replay, legacy);
}

#[test]
fn negotiated_stream_parts_try_clone_with_propagates_error_without_side_effects() {
    let legacy = b"@RSYNCD: 30.0\nlisting";
    let parts = sniff_bytes(legacy).expect("sniff succeeds").into_parts();
    let expected_len = parts.buffered_len();

    let err = parts
        .try_clone_with(|_| Err::<Cursor<Vec<u8>>, io::Error>(io::Error::other("boom")))
        .expect_err("clone should propagate errors");
    assert_eq!(err.kind(), io::ErrorKind::Other);
    assert_eq!(parts.buffered_len(), expected_len);

    let mut original_stream = parts.into_stream();
    let mut replay = Vec::new();
    original_stream
        .read_to_end(&mut replay)
        .expect("parts remain convertible to stream after failed clone");
    assert_eq!(replay, legacy);
}

#[test]
fn try_map_inner_error_can_transform_error_without_losing_original() {
    let legacy = b"@RSYNCD: 31.0\n";
    let parts = sniff_bytes(legacy).expect("sniff succeeds").into_parts();

    let err = parts
        .try_map_inner(
            |cursor| -> Result<RecordingTransport, (io::Error, Cursor<Vec<u8>>)> {
                Err((io::Error::other("boom"), cursor))
            },
        )
        .expect_err("mapping fails");

    let mapped = err.map_error(|error| error.kind());
    assert_eq!(*mapped.error(), io::ErrorKind::Other);

    let mut original = mapped.into_original().into_stream();
    let mut replay = Vec::new();
    original
        .read_to_end(&mut replay)
        .expect("mapped error preserves original stream");
    assert_eq!(replay, legacy);
}

#[test]
fn try_map_inner_error_map_original_transforms_preserved_value() {
    let legacy = b"@RSYNCD: 31.0\nmotd\n";
    let stream = sniff_bytes(legacy).expect("sniff succeeds");

    let err = stream
        .try_map_inner(
            |cursor| -> Result<RecordingTransport, (io::Error, Cursor<Vec<u8>>)> {
                Err((io::Error::other("boom"), cursor))
            },
        )
        .expect_err("mapping fails");

    let mapped = err.map_original(NegotiatedStream::into_parts);
    assert_eq!(mapped.error().kind(), io::ErrorKind::Other);

    let parts = mapped.into_original();
    assert_eq!(parts.sniffed_prefix_len(), LEGACY_DAEMON_PREFIX_LEN);

    let mut rebuilt = parts.into_stream();
    let mut replay = Vec::new();
    rebuilt
        .read_to_end(&mut replay)
        .expect("mapped original preserves replay bytes");
    assert_eq!(replay, legacy);
}

#[test]
fn try_map_inner_error_map_parts_transforms_error_and_original() {
    let legacy = b"@RSYNCD: 31.0\nlisting";
    let stream = sniff_bytes(legacy).expect("sniff succeeds");

    let err = stream
        .try_map_inner(
            |cursor| -> Result<RecordingTransport, (io::Error, Cursor<Vec<u8>>)> {
                Err((io::Error::other("boom"), cursor))
            },
        )
        .expect_err("mapping fails");

    let mapped = err.map_parts(|error, stream| (error.kind(), stream.into_parts()));
    assert_eq!(mapped.error(), &io::ErrorKind::Other);

    let parts = mapped.into_original();
    assert_eq!(parts.sniffed_prefix_len(), LEGACY_DAEMON_PREFIX_LEN);

    let mut rebuilt = parts.into_stream();
    let mut replay = Vec::new();
    rebuilt
        .read_to_end(&mut replay)
        .expect("mapped parts preserve replay bytes");
    assert_eq!(replay, legacy);
}

#[test]
fn try_map_inner_error_clone_preserves_components() {
    let parts = sniff_bytes(b"@RSYNCD: 31.0\nreply")
        .expect("sniff succeeds")
        .into_parts();
    let err = TryMapInnerError::new(String::from("clone"), parts.clone());

    let cloned = err.clone();
    assert_eq!(cloned.error(), "clone");
    assert_eq!(cloned.original().buffered(), parts.buffered());
    assert_eq!(err.original().buffered(), parts.buffered());
}

#[test]
fn try_map_inner_error_from_tuple_matches_constructor() {
    let parts = sniff_bytes(b"@RSYNCD: 31.0\nreply")
        .expect("sniff succeeds")
        .into_parts();
    let tuple_err: TryMapInnerError<_, _> = (io::Error::other("tuple"), parts.clone()).into();

    assert_eq!(tuple_err.error().kind(), io::ErrorKind::Other);
    assert_eq!(tuple_err.original().buffered(), parts.buffered());
}

#[test]
fn try_map_inner_error_into_tuple_recovers_parts() {
    let parts = sniff_bytes(b"@RSYNCD: 31.0\nreply")
        .expect("sniff succeeds")
        .into_parts();
    let err = TryMapInnerError::new(io::Error::other("into"), parts.clone());

    let (error, original): (io::Error, _) =
        TryMapInnerError::from((io::Error::other("shadow"), parts.clone())).into();
    assert_eq!(error.kind(), io::ErrorKind::Other);
    assert_eq!(original.buffered(), parts.buffered());

    let (error, original): (io::Error, _) = err.into();
    assert_eq!(error.kind(), io::ErrorKind::Other);
    assert_eq!(original.buffered(), parts.buffered());
}

#[test]
fn try_map_inner_error_display_mentions_original_type() {
    let legacy = b"@RSYNCD: 31.0\nlisting";
    let stream = sniff_bytes(legacy).expect("sniff succeeds");

    let err = stream
        .try_map_inner(
            |cursor| -> Result<RecordingTransport, (io::Error, Cursor<Vec<u8>>)> {
                Err((io::Error::other("wrap failed"), cursor))
            },
        )
        .expect_err("mapping fails");

    let display = format!("{err}");
    assert!(display.contains("wrap failed"));
    assert!(display.contains("Cursor"));
    assert!(display.contains("original type"));

    let alternate = format!("{err:#}");
    assert!(alternate.contains("recover via into_original"));
    assert!(alternate.contains("Cursor"));
}

#[test]
fn try_map_inner_error_debug_reports_recovery_hint() {
    let legacy = b"@RSYNCD: 31.0\nlisting";
    let stream = sniff_bytes(legacy).expect("sniff succeeds");

    let err = stream
        .try_map_inner(
            |cursor| -> Result<RecordingTransport, (io::Error, Cursor<Vec<u8>>)> {
                Err((io::Error::other("wrap failed"), cursor))
            },
        )
        .expect_err("mapping fails");

    let debug = format!("{err:?}");
    assert!(debug.contains("TryMapInnerError"));
    assert!(debug.contains("original_type"));
    assert!(debug.contains("Cursor"));

    let debug_alternate = format!("{err:#?}");
    assert!(debug_alternate.contains("recover"));
    assert!(debug_alternate.contains("Cursor"));
}

#[test]
fn try_map_inner_error_mut_accessors_preserve_state() {
    let legacy = b"@RSYNCD: 31.0\n";
    let stream = sniff_bytes(legacy).expect("sniff succeeds");

    let mut err = stream
        .try_map_inner(
            |cursor| -> Result<RecordingTransport, (io::Error, Cursor<Vec<u8>>)> {
                Err((io::Error::other("boom"), cursor))
            },
        )
        .expect_err("mapping fails");

    *err.error_mut() = io::Error::new(io::ErrorKind::TimedOut, "timeout");
    assert_eq!(err.error().kind(), io::ErrorKind::TimedOut);

    {
        let original = err.original_mut();
        let mut first = [0u8; 1];
        original
            .read_exact(&mut first)
            .expect("reading from preserved stream succeeds");
        assert_eq!(&first, b"@");
    }

    let mut restored = err.into_original();
    let mut replay = Vec::new();
    restored
        .read_to_end(&mut replay)
        .expect("mutations persist when recovering the original stream");
    assert_eq!(replay, &legacy[1..]);
}

#[test]
fn try_map_inner_error_combined_accessors_expose_borrows() {
    let legacy = b"@RSYNCD: 31.0\npayload";
    let stream = sniff_bytes(legacy).expect("sniff succeeds");

    let mut err = stream
        .try_map_inner(
            |cursor| -> Result<RecordingTransport, (io::Error, Cursor<Vec<u8>>)> {
                Err((io::Error::other("boom"), cursor))
            },
        )
        .expect_err("mapping fails");

    {
        let (error, original) = err.as_ref();
        assert_eq!(error.kind(), io::ErrorKind::Other);
        let (prefix, remainder) = original.buffered_split();
        assert_eq!(prefix, b"@RSYNCD:");
        assert!(remainder.is_empty());
    }

    {
        let (error, original) = err.as_mut();
        *error = io::Error::new(io::ErrorKind::TimedOut, "timeout");
        let mut first = [0u8; 1];
        original
            .read_exact(&mut first)
            .expect("reading from preserved stream succeeds");
        assert_eq!(&first, b"@");
    }

    assert_eq!(err.error().kind(), io::ErrorKind::TimedOut);

    let mut restored = err.into_original();
    let mut replay = Vec::new();
    restored
        .read_to_end(&mut replay)
        .expect("remaining bytes preserved after combined access");
    assert_eq!(replay, &legacy[1..]);
}

#[test]
fn try_map_inner_error_into_parts_returns_error_and_original() {
    let legacy = b"@RSYNCD: 31.0\nremainder";
    let stream = sniff_bytes(legacy).expect("sniff succeeds");

    let err = stream
        .try_map_inner(
            |cursor| -> Result<RecordingTransport, (io::Error, Cursor<Vec<u8>>)> {
                Err((io::Error::other("boom"), cursor))
            },
        )
        .expect_err("mapping fails");

    let (error, mut original) = err.into_parts();
    assert_eq!(error.kind(), io::ErrorKind::Other);

    let mut replay = Vec::new();
    original
        .read_to_end(&mut replay)
        .expect("replay remains available");
    assert_eq!(replay, legacy);
}

#[test]
fn sniff_negotiation_with_supplied_sniffer_reuses_internal_buffer() {
    let mut sniffer = NegotiationPrologueSniffer::new();

    {
        let mut stream = sniff_negotiation_stream_with_sniffer(
            Cursor::new(b"@RSYNCD: 31.0\nrest".to_vec()),
            &mut sniffer,
        )
        .expect("sniff succeeds");

        assert_eq!(stream.decision(), NegotiationPrologue::LegacyAscii);
        assert_eq!(stream.sniffed_prefix(), b"@RSYNCD:");

        let mut replay = Vec::new();
        stream
            .read_to_end(&mut replay)
            .expect("replay reads all bytes");
        assert_eq!(replay, b"@RSYNCD: 31.0\nrest");
    }

    assert_eq!(sniffer.buffered_len(), 0);
    assert_eq!(sniffer.sniffed_prefix_len(), 0);

    {
        let mut stream = sniff_negotiation_stream_with_sniffer(
            Cursor::new(vec![0x00, 0x12, 0x34, 0x56]),
            &mut sniffer,
        )
        .expect("sniff succeeds");

        assert_eq!(stream.decision(), NegotiationPrologue::Binary);
        assert_eq!(stream.sniffed_prefix(), &[0x00]);

        let mut replay = Vec::new();
        stream
            .read_to_end(&mut replay)
            .expect("binary replay drains reader");
        assert_eq!(replay, &[0x00, 0x12, 0x34, 0x56]);
    }

    assert_eq!(sniffer.buffered_len(), 0);
    assert_eq!(sniffer.sniffed_prefix_len(), 0);
}

#[test]
fn sniff_negotiation_buffered_split_exposes_prefix_and_remainder() {
    let cursor = Cursor::new(Vec::<u8>::new());
    let mut stream = NegotiatedStream::from_raw_components(
        cursor,
        NegotiationPrologue::Binary,
        1,
        0,
        vec![0x00, b'a', b'b', b'c'],
    );

    let (prefix, remainder) = stream.buffered_split();
    assert_eq!(prefix, &[0x00]);
    assert_eq!(remainder, b"abc");

    // Partially consume the prefix to ensure the tuple remains stable.
    let mut buf = [0u8; 1];
    stream
        .read_exact(&mut buf)
        .expect("read_exact consumes the buffered prefix");
    assert_eq!(buf, [0x00]);

    let (after_read_prefix, after_read_remainder) = stream.buffered_split();
    assert!(after_read_prefix.is_empty());
    assert_eq!(after_read_remainder, b"abc");

    let mut partial = [0u8; 2];
    stream
        .read_exact(&mut partial)
        .expect("read_exact drains part of the buffered remainder");
    assert_eq!(&partial, b"ab");
    assert_eq!(stream.buffered_remainder(), b"c");

    let mut final_byte = [0u8; 1];
    stream
        .read_exact(&mut final_byte)
        .expect("read_exact consumes the last buffered byte");
    assert_eq!(&final_byte, b"c");
    assert!(stream.buffered_remainder().is_empty());

    let (after_read_prefix, after_read_remainder) = stream.buffered_split();
    assert!(after_read_prefix.is_empty());
    assert!(after_read_remainder.is_empty());
}

#[test]
fn sniff_negotiation_buffered_vectored_matches_buffered_bytes() {
    let cursor = Cursor::new(Vec::<u8>::new());
    let stream = NegotiatedStream::from_raw_components(
        cursor,
        NegotiationPrologue::Binary,
        1,
        0,
        vec![0x00, b'a', b'b'],
    );

    let vectored = stream.buffered_vectored();
    assert_eq!(vectored.segment_count(), 2);
    assert_eq!(vectored.len(), stream.buffered().len());

    let collected: Vec<&[u8]> = vectored.iter().map(|slice| slice.as_ref()).collect();
    assert_eq!(collected, vec![&[0x00][..], &b"ab"[..]]);

    let flattened: Vec<u8> = vectored
        .iter()
        .flat_map(|slice| slice.as_ref().iter().copied())
        .collect();
    assert_eq!(flattened, stream.buffered());
}

#[test]
fn sniff_negotiation_buffered_remaining_vectored_tracks_consumption() {
    let cursor = Cursor::new(Vec::<u8>::new());
    let mut stream = NegotiatedStream::from_raw_components(
        cursor,
        NegotiationPrologue::Binary,
        2,
        0,
        vec![0xAA, 0xBB, b'x', b'y'],
    );

    let mut buf = [0u8; 1];
    stream
        .read_exact(&mut buf)
        .expect("read_exact consumes part of the prefix");
    assert_eq!(&buf, &[0xAA]);

    let remaining = stream.buffered_remaining_vectored();
    assert_eq!(remaining.segment_count(), 2);
    assert_eq!(remaining.len(), stream.buffered_remaining());

    let slices: Vec<&[u8]> = remaining.iter().map(|slice| slice.as_ref()).collect();
    assert_eq!(slices, vec![&[0xBB][..], &b"xy"[..]]);

    let flattened: Vec<u8> = remaining
        .iter()
        .flat_map(|slice| slice.as_ref().iter().copied())
        .collect();
    assert_eq!(flattened, stream.buffered_remaining_slice());
}

#[test]
fn sniff_negotiation_errors_on_empty_stream() {
    let err = sniff_bytes(&[]).expect_err("sniff should fail");
    assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
}

#[test]
fn sniffed_stream_supports_bufread_semantics() {
    let data = b"@RSYNCD: 32.0\nhello";
    let mut stream = sniff_bytes(data).expect("sniff succeeds");

    assert_eq!(stream.fill_buf().expect("fill_buf succeeds"), b"@RSYNCD:");
    stream.consume(LEGACY_DAEMON_PREFIX_LEN);
    assert_eq!(
        stream.fill_buf().expect("fill_buf after consume succeeds"),
        b" 32.0\nhello"
    );
    stream.consume(3);
    assert_eq!(
        stream.fill_buf().expect("fill_buf after partial consume"),
        b".0\nhello"
    );
}

#[test]
fn sniffed_stream_supports_writing_via_wrapper() {
    let transport = RecordingTransport::new(b"@RSYNCD: 31.0\nrest");
    let mut stream = sniff_negotiation_stream(transport).expect("sniff succeeds");

    stream
        .write_all(b"CLIENT\n")
        .expect("write forwards to inner transport");

    let vectored = [IoSlice::new(b"V1"), IoSlice::new(b"V2")];
    let written = stream
        .write_vectored(&vectored)
        .expect("vectored write forwards to inner transport");
    assert_eq!(written, 4);

    stream.flush().expect("flush forwards to inner transport");

    let mut line = Vec::new();
    stream
        .read_legacy_daemon_line(&mut line)
        .expect("legacy line remains readable after writes");
    assert_eq!(line, b"@RSYNCD: 31.0\n");

    let inner = stream.into_inner();
    assert_eq!(inner.writes(), b"CLIENT\nV1V2");
    assert_eq!(inner.flushes(), 1);
}

#[test]
fn sniffed_stream_supports_vectored_reads_from_buffer() {
    let data = b"@RSYNCD: 31.0\nrest";
    let mut stream = sniff_bytes(data).expect("sniff succeeds");

    let mut head = [0u8; 4];
    let mut tail = [0u8; 8];
    let mut bufs = [IoSliceMut::new(&mut head), IoSliceMut::new(&mut tail)];

    let read = stream
        .read_vectored(&mut bufs)
        .expect("vectored read drains buffered prefix");
    assert_eq!(read, LEGACY_DAEMON_PREFIX_LEN);
    assert_eq!(&head, b"@RSY");

    let tail_prefix = read.saturating_sub(head.len());
    assert_eq!(&tail[..tail_prefix], b"NCD:");

    let mut remainder = Vec::new();
    stream
        .read_to_end(&mut remainder)
        .expect("remaining bytes are readable");
    assert_eq!(remainder, &data[read..]);
}

#[test]
fn vectored_reads_delegate_to_inner_after_buffer_is_drained() {
    let data = b"\x00rest";
    let mut stream = sniff_bytes(data).expect("sniff succeeds");

    let mut prefix_buf = [0u8; 1];
    let mut bufs = [IoSliceMut::new(&mut prefix_buf)];
    let read = stream
        .read_vectored(&mut bufs)
        .expect("vectored read captures sniffed prefix");
    assert_eq!(read, 1);
    assert_eq!(prefix_buf, [0x00]);

    let mut first = [0u8; 2];
    let mut second = [0u8; 8];
    let mut remainder_bufs = [IoSliceMut::new(&mut first), IoSliceMut::new(&mut second)];
    let remainder_read = stream
        .read_vectored(&mut remainder_bufs)
        .expect("vectored read forwards to inner reader");

    let mut remainder = Vec::new();
    remainder.extend_from_slice(&first[..first.len().min(remainder_read)]);
    if remainder_read > first.len() {
        let extra = (remainder_read - first.len()).min(second.len());
        remainder.extend_from_slice(&second[..extra]);
    }
    if remainder.len() < b"rest".len() {
        let mut tail = Vec::new();
        stream
            .read_to_end(&mut tail)
            .expect("consume any bytes left by the default vectored implementation");
        remainder.extend_from_slice(&tail);
    }
    assert_eq!(remainder, b"rest");
}

#[test]
fn vectored_reads_delegate_to_inner_even_without_specialized_support() {
    let data = b"\x00rest".to_vec();
    let mut stream =
        sniff_negotiation_stream(NonVectoredCursor::new(data)).expect("sniff succeeds");

    let mut prefix_buf = [0u8; 1];
    let mut prefix_vecs = [IoSliceMut::new(&mut prefix_buf)];
    let read = stream
        .read_vectored(&mut prefix_vecs)
        .expect("vectored read yields buffered prefix");
    assert_eq!(read, 1);
    assert_eq!(prefix_buf, [0x00]);

    let mut first = [0u8; 2];
    let mut second = [0u8; 8];
    let mut remainder_bufs = [IoSliceMut::new(&mut first), IoSliceMut::new(&mut second)];
    let remainder_read = stream
        .read_vectored(&mut remainder_bufs)
        .expect("vectored read falls back to inner read implementation");

    let mut remainder = Vec::new();
    remainder.extend_from_slice(&first[..first.len().min(remainder_read)]);
    if remainder_read > first.len() {
        let extra = (remainder_read - first.len()).min(second.len());
        remainder.extend_from_slice(&second[..extra]);
    }
    let mut tail = Vec::new();
    stream
        .read_to_end(&mut tail)
        .expect("consume any bytes left by the default vectored implementation");
    remainder.extend_from_slice(&tail);

    assert_eq!(remainder, b"rest");
}

#[test]
fn parts_structure_exposes_buffered_state() {
    let data = b"\x00more";
    let stream = sniff_bytes(data).expect("sniff succeeds");
    let parts = stream.into_parts();
    assert_eq!(parts.decision(), NegotiationPrologue::Binary);
    assert_eq!(parts.sniffed_prefix(), b"\x00");
    assert!(parts.buffered_remainder().is_empty());
    assert_eq!(parts.buffered_len(), parts.sniffed_prefix_len());
    assert_eq!(parts.buffered_remaining(), parts.sniffed_prefix_len());
    let (prefix, remainder) = parts.buffered_split();
    assert_eq!(prefix, b"\x00");
    assert!(remainder.is_empty());
}

#[test]
fn negotiated_stream_clone_preserves_buffer_progress() {
    let data = b"@RSYNCD: 30.0\nrest";
    let mut stream = sniff_bytes(data).expect("sniff succeeds");

    let mut prefix = [0u8; 5];
    stream
        .read_exact(&mut prefix)
        .expect("read_exact consumes part of the buffered prefix");
    assert_eq!(&prefix, b"@RSYN");

    let remaining_before_clone = stream.buffered_remaining();
    let consumed_before_clone = stream.buffered_consumed();

    let mut cloned = stream.clone();
    assert_eq!(cloned.decision(), stream.decision());
    assert_eq!(cloned.buffered_remaining(), remaining_before_clone);
    assert_eq!(cloned.buffered_consumed(), consumed_before_clone);
    assert_eq!(
        cloned.sniffed_prefix_remaining(),
        stream.sniffed_prefix_remaining()
    );

    let mut cloned_replay = Vec::new();
    cloned
        .read_to_end(&mut cloned_replay)
        .expect("cloned stream replays remaining bytes");

    assert_eq!(stream.buffered_remaining(), remaining_before_clone);
    assert_eq!(stream.buffered_consumed(), consumed_before_clone);

    let mut original_replay = Vec::new();
    stream
        .read_to_end(&mut original_replay)
        .expect("original stream still replays bytes");

    assert_eq!(cloned_replay, original_replay);
}

#[test]
fn parts_can_be_cloned_without_sharing_state() {
    let data = b"@RSYNCD: 30.0\nrest";
    let mut stream = sniff_bytes(data).expect("sniff succeeds");

    let mut prefix_fragment = [0u8; 3];
    stream
        .read_exact(&mut prefix_fragment)
        .expect("read_exact consumes part of the buffered prefix");
    assert_eq!(&prefix_fragment, b"@RS");

    let parts = stream.into_parts();
    let cloned = parts.clone();

    assert_eq!(cloned.decision(), parts.decision());
    assert_eq!(cloned.sniffed_prefix(), parts.sniffed_prefix());
    assert_eq!(cloned.buffered_remainder(), parts.buffered_remainder());
    assert_eq!(cloned.buffered_remaining(), parts.buffered_remaining());

    let mut original_stream = parts.into_stream();
    let mut original_replay = Vec::new();
    original_stream
        .read_to_end(&mut original_replay)
        .expect("original stream replays buffered bytes");

    let mut cloned_stream = cloned.into_stream();
    let mut cloned_replay = Vec::new();
    cloned_stream
        .read_to_end(&mut cloned_replay)
        .expect("cloned stream replays its buffered bytes");

    assert_eq!(original_replay, cloned_replay);
}

#[test]
fn parts_can_be_rehydrated_without_rewinding_consumed_bytes() {
    let data = b"@RSYNCD: 29.0\nrest";
    let mut stream = sniff_bytes(data).expect("sniff succeeds");

    let mut prefix_chunk = [0u8; 4];
    stream
        .read_exact(&mut prefix_chunk)
        .expect("read_exact consumes part of the buffered prefix");
    assert_eq!(&prefix_chunk, b"@RSY");

    let parts = stream.into_parts();
    assert_eq!(
        parts.buffered_remaining(),
        LEGACY_DAEMON_PREFIX_LEN - prefix_chunk.len()
    );
    assert_eq!(parts.buffered_len(), LEGACY_DAEMON_PREFIX_LEN);

    let mut rehydrated = NegotiatedStream::from_parts(parts);
    assert_eq!(
        rehydrated.buffered_remaining(),
        LEGACY_DAEMON_PREFIX_LEN - prefix_chunk.len()
    );

    let (rehydrated_prefix, rehydrated_remainder) = rehydrated.buffered_split();
    assert_eq!(rehydrated_prefix, b"NCD:");
    assert_eq!(rehydrated_remainder, rehydrated.buffered_remainder());

    let mut remainder = Vec::new();
    rehydrated
        .read_to_end(&mut remainder)
        .expect("reconstructed stream yields the remaining bytes");
    assert_eq!(remainder, b"NCD: 29.0\nrest");
}

#[test]
fn parts_rehydrate_sniffer_restores_binary_snapshot() {
    let data = [0x42, 0x10, 0x11, 0x12];
    let stream = sniff_bytes(&data).expect("sniff succeeds");
    let decision = stream.decision();
    assert_eq!(decision, NegotiationPrologue::Binary);
    let prefix_len = stream.sniffed_prefix_len();
    let mut snapshot = Vec::new();
    stream
        .copy_buffered_into_vec(&mut snapshot)
        .expect("snapshot capture succeeds");

    let parts = stream.into_parts();
    let mut sniffer = NegotiationPrologueSniffer::new();
    parts
        .rehydrate_sniffer(&mut sniffer)
        .expect("rehydration succeeds");

    assert_eq!(sniffer.buffered(), snapshot.as_slice());
    assert_eq!(sniffer.sniffed_prefix_len(), prefix_len);
    assert_eq!(
        sniffer
            .read_from(&mut Cursor::new(Vec::new()))
            .expect("cached decision is returned"),
        decision
    );
}

#[test]
fn parts_rehydrate_sniffer_preserves_partial_legacy_state() {
    let buffered = b"@RS".to_vec();
    let decision = NegotiationPrologue::LegacyAscii;
    let stream = NegotiatedStream::from_raw_components(
        Cursor::new(Vec::<u8>::new()),
        decision,
        buffered.len(),
        0,
        buffered.clone(),
    );
    assert!(stream.sniffed_prefix_remaining() > 0);
    let prefix_len = stream.sniffed_prefix_len();
    let mut snapshot = Vec::new();
    stream
        .copy_buffered_into_vec(&mut snapshot)
        .expect("snapshot capture succeeds");

    let parts = stream.into_parts();
    let expected_remaining = LEGACY_DAEMON_PREFIX_LEN.saturating_sub(prefix_len);
    let mut sniffer = NegotiationPrologueSniffer::new();
    parts
        .rehydrate_sniffer(&mut sniffer)
        .expect("rehydration succeeds");

    assert_eq!(sniffer.buffered(), snapshot.as_slice());
    assert_eq!(sniffer.sniffed_prefix_len(), prefix_len);
    assert_eq!(sniffer.decision(), Some(decision));
    let expected_remaining_opt = (expected_remaining > 0).then_some(expected_remaining);
    assert_eq!(sniffer.legacy_prefix_remaining(), expected_remaining_opt);
    assert_eq!(
        sniffer.requires_more_data(),
        expected_remaining_opt.is_some()
    );
}

#[test]
fn raw_parts_round_trip_binary_state() {
    let data = [0x00, 0x12, 0x34, 0x56];
    let stream = sniff_bytes(&data).expect("sniff succeeds");
    let expected_decision = stream.decision();
    assert_eq!(expected_decision, NegotiationPrologue::Binary);
    assert_eq!(stream.sniffed_prefix(), &[0x00]);

    let (decision, sniffed_prefix_len, buffered_pos, buffered, inner) = stream.into_raw_parts();
    assert_eq!(decision, expected_decision);
    assert_eq!(sniffed_prefix_len, 1);
    assert_eq!(buffered_pos, 0);
    assert_eq!(buffered, vec![0x00]);

    let mut reconstructed = NegotiatedStream::from_raw_parts(
        inner,
        decision,
        sniffed_prefix_len,
        buffered_pos,
        buffered,
    );
    let mut replay = Vec::new();
    reconstructed
        .read_to_end(&mut replay)
        .expect("reconstructed stream replays buffered prefix and remainder");
    assert_eq!(replay, data);
}

#[test]
fn raw_parts_preserve_consumed_progress() {
    let data = b"@RSYNCD: 31.0\nrest";
    let mut stream = sniff_bytes(data).expect("sniff succeeds");
    assert_eq!(stream.decision(), NegotiationPrologue::LegacyAscii);

    let mut consumed = [0u8; 3];
    stream
        .read_exact(&mut consumed)
        .expect("prefix consumption succeeds");
    assert_eq!(&consumed, b"@RS");

    let (decision, sniffed_prefix_len, buffered_pos, buffered, inner) = stream.into_raw_parts();
    assert_eq!(decision, NegotiationPrologue::LegacyAscii);
    assert_eq!(sniffed_prefix_len, LEGACY_DAEMON_PREFIX_LEN);
    assert_eq!(buffered_pos, consumed.len());
    assert_eq!(buffered, b"@RSYNCD:".to_vec());

    let mut reconstructed = NegotiatedStream::from_raw_parts(
        inner,
        decision,
        sniffed_prefix_len,
        buffered_pos,
        buffered,
    );
    let mut remainder = Vec::new();
    reconstructed
        .read_to_end(&mut remainder)
        .expect("reconstructed stream resumes after consumed prefix");
    assert_eq!(remainder, b"YNCD: 31.0\nrest");

    let mut combined = Vec::new();
    combined.extend_from_slice(&consumed);
    combined.extend_from_slice(&remainder);
    assert_eq!(combined, data);
}

#[test]
fn raw_parts_round_trip_legacy_state() {
    let data = b"@RSYNCD: 32.0\nrest";
    let stream = sniff_bytes(data).expect("sniff succeeds");
    assert_eq!(stream.decision(), NegotiationPrologue::LegacyAscii);
    assert_eq!(stream.sniffed_prefix(), b"@RSYNCD:");

    let (decision, sniffed_prefix_len, buffered_pos, buffered, inner) = stream.into_raw_parts();
    assert_eq!(decision, NegotiationPrologue::LegacyAscii);
    assert_eq!(sniffed_prefix_len, LEGACY_DAEMON_PREFIX_LEN);
    assert_eq!(buffered_pos, 0);
    assert_eq!(buffered, b"@RSYNCD:".to_vec());

    let mut reconstructed = NegotiatedStream::from_raw_parts(
        inner,
        decision,
        sniffed_prefix_len,
        buffered_pos,
        buffered,
    );
    let mut replay = Vec::new();
    reconstructed
        .read_to_end(&mut replay)
        .expect("reconstructed stream replays full buffer");
    assert_eq!(replay, data);
}

#[test]
fn map_inner_preserves_buffered_progress() {
    let mut stream = sniff_bytes(&[0x00, 0x12, 0x34, 0x56]).expect("sniff succeeds");
    assert_eq!(stream.decision(), NegotiationPrologue::Binary);

    let mut prefix = [0u8; 1];
    stream
        .read_exact(&mut prefix)
        .expect("read_exact delivers sniffed prefix");
    assert_eq!(prefix, [0x00]);

    let mut mapped = stream.map_inner(|cursor| {
        let position = cursor.position();
        let boxed = cursor.into_inner().into_boxed_slice();
        let mut replacement = Cursor::new(boxed);
        replacement.set_position(position);
        replacement
    });
    assert_eq!(mapped.decision(), NegotiationPrologue::Binary);

    let mut remainder = [0u8; 3];
    mapped
        .read_exact(&mut remainder)
        .expect("replay continues from buffered position");
    assert_eq!(&remainder, &[0x12, 0x34, 0x56]);
}

#[test]
fn parts_map_inner_allows_rewrapping_inner_reader() {
    let data = b"@RSYNCD: 31.0\n#list";
    let parts = sniff_bytes(data).expect("sniff succeeds").into_parts();
    let remaining = parts.buffered_remaining();

    let mapped_parts = parts.map_inner(|cursor| {
        let position = cursor.position();
        let boxed = cursor.into_inner().into_boxed_slice();
        let mut replacement = Cursor::new(boxed);
        replacement.set_position(position);
        replacement
    });
    assert_eq!(mapped_parts.decision(), NegotiationPrologue::LegacyAscii);
    assert_eq!(mapped_parts.buffered_remaining(), remaining);
    let (prefix, remainder) = mapped_parts.buffered_split();
    assert_eq!(prefix, b"@RSYNCD:");
    assert_eq!(remainder, mapped_parts.buffered_remainder());

    let mut rebuilt = mapped_parts.into_stream();
    let mut replay = Vec::new();
    rebuilt
        .read_to_end(&mut replay)
        .expect("rebuilt stream yields original contents");
    assert_eq!(replay, data);
}

#[test]
fn read_legacy_daemon_line_replays_buffered_prefix() {
    let mut stream = sniff_bytes(b"@RSYNCD: 30.0\n#list\n").expect("sniff succeeds");
    let mut line = Vec::new();
    stream
        .read_legacy_daemon_line(&mut line)
        .expect("legacy line is read");
    assert_eq!(line, b"@RSYNCD: 30.0\n");

    let mut remainder = Vec::new();
    stream
        .read_to_end(&mut remainder)
        .expect("remaining bytes are replayed");
    assert_eq!(remainder, b"#list\n");
}

#[test]
fn read_and_parse_legacy_daemon_message_after_greeting() {
    let mut stream =
        sniff_bytes(b"@RSYNCD: 31.0\n@RSYNCD: AUTHREQD module\n@ERROR: access denied\n")
            .expect("sniff succeeds");

    let mut line = Vec::new();
    let version = stream
        .read_and_parse_legacy_daemon_greeting(&mut line)
        .expect("greeting parses");
    let expected = ProtocolVersion::from_supported(31).expect("supported version");
    assert_eq!(version, expected);
    assert_eq!(line, b"@RSYNCD: 31.0\n");

    let message = stream
        .read_and_parse_legacy_daemon_message(&mut line)
        .expect("message parses");
    match message {
        LegacyDaemonMessage::AuthRequired { module } => {
            assert_eq!(module, Some("module"));
        }
        other => panic!("unexpected message: {other:?}"),
    }
    assert_eq!(line, b"@RSYNCD: AUTHREQD module\n");

    let error = stream
        .read_and_parse_legacy_daemon_error_message(&mut line)
        .expect("error parses")
        .expect("payload present");
    assert_eq!(error, "access denied");
    assert_eq!(line, b"@ERROR: access denied\n");
}

#[test]
fn read_and_parse_legacy_daemon_message_routes_keywords() {
    let mut stream = sniff_bytes(b"@RSYNCD: AUTHREQD module\n").expect("sniff succeeds");
    let mut line = Vec::new();
    match stream
        .read_and_parse_legacy_daemon_message(&mut line)
        .expect("message parses")
    {
        LegacyDaemonMessage::AuthRequired { module } => {
            assert_eq!(module, Some("module"));
        }
        other => panic!("unexpected message: {other:?}"),
    }
    assert_eq!(line, b"@RSYNCD: AUTHREQD module\n");
}

#[test]
fn read_and_parse_legacy_daemon_message_routes_capabilities() {
    let mut stream = sniff_bytes(b"@RSYNCD: CAP 0x1f 0x2\n").expect("sniff succeeds");
    let mut line = Vec::new();
    match stream
        .read_and_parse_legacy_daemon_message(&mut line)
        .expect("message parses")
    {
        LegacyDaemonMessage::Capabilities { flags } => {
            assert_eq!(flags, "0x1f 0x2");
        }
        other => panic!("unexpected message: {other:?}"),
    }
    assert_eq!(line, b"@RSYNCD: CAP 0x1f 0x2\n");
}

#[test]
fn read_and_parse_legacy_daemon_message_routes_versions() {
    let mut stream = sniff_bytes(b"@RSYNCD: 29.0\n").expect("sniff succeeds");
    let mut line = Vec::new();
    match stream
        .read_and_parse_legacy_daemon_message(&mut line)
        .expect("message parses")
    {
        LegacyDaemonMessage::Version(version) => {
            let expected = ProtocolVersion::from_supported(29).expect("supported version");
            assert_eq!(version, expected);
        }
        other => panic!("unexpected message: {other:?}"),
    }
    assert_eq!(line, b"@RSYNCD: 29.0\n");
}

#[test]
fn read_and_parse_legacy_daemon_message_propagates_parse_errors() {
    let mut stream = sniff_bytes(b"@RSYNCD:\n").expect("sniff succeeds");
    let mut line = Vec::new();
    let err = stream
        .read_and_parse_legacy_daemon_message(&mut line)
        .expect_err("message parsing should fail");
    assert_eq!(err.kind(), io::ErrorKind::InvalidData);
}

#[test]
fn read_and_parse_legacy_daemon_error_message_returns_payload() {
    let mut stream = sniff_bytes(b"@ERROR: something went wrong\n").expect("sniff succeeds");
    let mut line = Vec::new();
    {
        let payload = stream
            .read_and_parse_legacy_daemon_error_message(&mut line)
            .expect("error payload parses")
            .expect("payload is present");
        assert_eq!(payload, "something went wrong");
    }
    assert_eq!(line, b"@ERROR: something went wrong\n");
}

#[test]
fn read_and_parse_legacy_daemon_error_message_allows_empty_payloads() {
    let mut stream = sniff_bytes(b"@ERROR:\n").expect("sniff succeeds");
    let mut line = Vec::new();
    {
        let payload = stream
            .read_and_parse_legacy_daemon_error_message(&mut line)
            .expect("empty payload parses");
        assert_eq!(payload, Some(""));
    }
    assert_eq!(line, b"@ERROR:\n");
}

#[test]
fn read_and_parse_legacy_daemon_warning_message_returns_payload() {
    let mut stream = sniff_bytes(b"@WARNING: check perms\n").expect("sniff succeeds");
    let mut line = Vec::new();
    {
        let payload = stream
            .read_and_parse_legacy_daemon_warning_message(&mut line)
            .expect("warning payload parses")
            .expect("payload is present");
        assert_eq!(payload, "check perms");
    }
    assert_eq!(line, b"@WARNING: check perms\n");
}

#[test]
fn read_legacy_daemon_line_errors_when_prefix_already_consumed() {
    let mut stream = sniff_bytes(b"@RSYNCD: 29.0\nrest").expect("sniff succeeds");
    let mut prefix_chunk = [0u8; 4];
    stream
        .read_exact(&mut prefix_chunk)
        .expect("prefix chunk is replayed before parsing");

    let mut line = Vec::new();
    let err = stream
        .read_legacy_daemon_line(&mut line)
        .expect_err("consuming prefix first should fail");
    assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
}

#[test]
fn read_legacy_daemon_line_errors_for_incomplete_prefix_state() {
    let mut stream = NegotiatedStream::from_raw_parts(
        Cursor::new(b" 31.0\n".to_vec()),
        NegotiationPrologue::LegacyAscii,
        LEGACY_DAEMON_PREFIX_LEN - 1,
        0,
        b"@RSYNCD".to_vec(),
    );

    let mut line = Vec::new();
    let err = stream
        .read_legacy_daemon_line(&mut line)
        .expect_err("incomplete prefix must error");
    assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    assert!(line.is_empty());
}

#[test]
fn read_legacy_daemon_line_errors_for_binary_negotiation() {
    let mut stream = sniff_bytes(&[0x00, 0x12, 0x34]).expect("sniff succeeds");
    let mut line = Vec::new();
    let err = stream
        .read_legacy_daemon_line(&mut line)
        .expect_err("binary negotiations do not yield legacy lines");
    assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
}

#[test]
fn read_legacy_daemon_line_errors_on_eof_before_newline() {
    let mut stream = sniff_bytes(b"@RSYNCD:").expect("sniff succeeds");
    let mut line = Vec::new();
    let err = stream
        .read_legacy_daemon_line(&mut line)
        .expect_err("EOF before newline must error");
    assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
}

#[test]
fn read_and_parse_legacy_daemon_message_errors_when_prefix_partially_consumed() {
    let mut stream = sniff_bytes(b"@RSYNCD: AUTHREQD module\n").expect("sniff succeeds");
    let mut prefix_fragment = [0u8; 3];
    stream
        .read_exact(&mut prefix_fragment)
        .expect("prefix fragment is replayed before parsing");

    let mut line = Vec::new();
    let err = stream
        .read_and_parse_legacy_daemon_message(&mut line)
        .expect_err("partial prefix consumption must error");
    assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    assert!(line.is_empty());
}

#[test]
fn read_and_parse_legacy_daemon_message_clears_line_on_error() {
    let mut stream = sniff_bytes(b"\x00rest").expect("sniff succeeds");
    let mut line = b"stale".to_vec();

    let err = stream
        .read_and_parse_legacy_daemon_message(&mut line)
        .expect_err("binary negotiation cannot parse legacy message");

    assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    assert!(line.is_empty());
}

#[test]
fn read_and_parse_legacy_daemon_greeting_from_stream() {
    let mut stream = sniff_bytes(b"@RSYNCD: 31.0\n").expect("sniff succeeds");
    let mut line = Vec::new();
    let version = stream
        .read_and_parse_legacy_daemon_greeting(&mut line)
        .expect("greeting parses");
    assert_eq!(version, ProtocolVersion::from_supported(31).unwrap());
    assert_eq!(line, b"@RSYNCD: 31.0\n");
}

#[test]
fn read_and_parse_legacy_daemon_greeting_details_from_stream() {
    let mut stream = sniff_bytes(b"@RSYNCD: 31.0 md4 md5\n").expect("sniff succeeds");
    let mut line = Vec::new();
    let details = stream
        .read_and_parse_legacy_daemon_greeting_details(&mut line)
        .expect("detailed greeting parses");
    assert_eq!(
        details.protocol(),
        ProtocolVersion::from_supported(31).unwrap()
    );
    assert_eq!(details.digest_list(), Some("md4 md5"));
    assert!(details.has_subprotocol());
    assert_eq!(line, b"@RSYNCD: 31.0 md4 md5\n");
}

#[derive(Debug)]
struct NonVectoredCursor(Cursor<Vec<u8>>);

impl NonVectoredCursor {
    fn new(bytes: Vec<u8>) -> Self {
        Self(Cursor::new(bytes))
    }
}

impl Read for NonVectoredCursor {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.0.read(buf)
    }
}
