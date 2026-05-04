
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

