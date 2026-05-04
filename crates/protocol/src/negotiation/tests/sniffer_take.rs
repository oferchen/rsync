
#[test]
fn prologue_sniffer_take_buffered_remainder_into_reuses_destination() {
    let mut sniffer = NegotiationPrologueSniffer::new();
    let (decision, consumed) = sniffer
        .observe(LEGACY_DAEMON_PREFIX.as_bytes())
        .expect("buffer reservation succeeds");
    assert_eq!(decision, NegotiationPrologue::LegacyAscii);
    assert_eq!(consumed, LEGACY_DAEMON_PREFIX_LEN);

    sniffer
        .buffered_storage_mut()
        .extend_from_slice(b" hello\n");

    let mut reused = b"seed".to_vec();
    let drained = sniffer
        .take_buffered_remainder_into(&mut reused)
        .expect("destination should grow for remainder");

    assert_eq!(reused, b" hello\n");
    assert_eq!(drained, reused.len());
    assert_eq!(sniffer.buffered(), LEGACY_DAEMON_PREFIX.as_bytes());
    assert_eq!(sniffer.buffered_remainder(), b"");
}

#[test]
fn prologue_sniffer_take_buffered_remainder_into_slice_copies_remainder() {
    let mut sniffer = NegotiationPrologueSniffer::new();
    let (decision, consumed) = sniffer
        .observe(LEGACY_DAEMON_PREFIX.as_bytes())
        .expect("buffer reservation succeeds");
    assert_eq!(decision, NegotiationPrologue::LegacyAscii);
    assert_eq!(consumed, LEGACY_DAEMON_PREFIX_LEN);

    sniffer
        .buffered_storage_mut()
        .extend_from_slice(b" trailer");

    let mut scratch = [0u8; 8];
    let copied = sniffer
        .take_buffered_remainder_into_slice(&mut scratch)
        .expect("slice should fit remainder");

    assert_eq!(copied, b" trailer".len());
    assert_eq!(&scratch[..copied], b" trailer");
    assert_eq!(sniffer.buffered(), LEGACY_DAEMON_PREFIX.as_bytes());
}

#[test]
fn prologue_sniffer_take_buffered_remainder_into_slice_reports_small_buffer() {
    let mut sniffer = NegotiationPrologueSniffer::new();
    let (decision, consumed) = sniffer
        .observe(LEGACY_DAEMON_PREFIX.as_bytes())
        .expect("buffer reservation succeeds");
    assert_eq!(decision, NegotiationPrologue::LegacyAscii);
    assert_eq!(consumed, LEGACY_DAEMON_PREFIX_LEN);

    sniffer.buffered_storage_mut().extend_from_slice(b" tail");

    let mut scratch = [0u8; 3];
    let err = sniffer
        .take_buffered_remainder_into_slice(&mut scratch)
        .expect_err("slice without capacity should error");

    assert_eq!(err.required(), b" tail".len());
    assert_eq!(err.available(), scratch.len());
    assert_eq!(sniffer.buffered_remainder(), b" tail");
}

#[test]
fn prologue_sniffer_take_buffered_remainder_into_vectored_copies_remainder() {
    let mut sniffer = NegotiationPrologueSniffer::new();
    let (decision, consumed) = sniffer
        .observe(LEGACY_DAEMON_PREFIX.as_bytes())
        .expect("buffer reservation succeeds");
    assert_eq!(decision, NegotiationPrologue::LegacyAscii);
    assert_eq!(consumed, LEGACY_DAEMON_PREFIX_LEN);

    let remainder = b" trailer";
    sniffer.buffered_storage_mut().extend_from_slice(remainder);

    let mut first = vec![0u8; 3];
    let mut second = vec![0u8; remainder.len() - first.len()];
    let mut buffers = [
        IoSliceMut::new(first.as_mut_slice()),
        IoSliceMut::new(second.as_mut_slice()),
    ];

    let copied = sniffer
        .take_buffered_remainder_into_vectored(&mut buffers)
        .expect("vectored remainder transfer should succeed");

    let mut actual = Vec::new();
    let mut remaining = copied;
    for buf in &buffers {
        if remaining == 0 {
            break;
        }
        let slice: &[u8] = buf.as_ref();
        let take = slice.len().min(remaining);
        actual.extend_from_slice(&slice[..take]);
        remaining -= take;
    }

    assert_eq!(copied, remainder.len());
    assert_eq!(actual, remainder);
    assert_eq!(sniffer.buffered(), LEGACY_DAEMON_PREFIX.as_bytes());
    assert!(sniffer.buffered_remainder().is_empty());
}

#[test]
fn prologue_sniffer_take_buffered_remainder_into_vectored_reports_small_capacity() {
    let mut sniffer = NegotiationPrologueSniffer::new();
    let (decision, consumed) = sniffer
        .observe(LEGACY_DAEMON_PREFIX.as_bytes())
        .expect("buffer reservation succeeds");
    assert_eq!(decision, NegotiationPrologue::LegacyAscii);
    assert_eq!(consumed, LEGACY_DAEMON_PREFIX_LEN);

    let remainder = b" tail";
    sniffer.buffered_storage_mut().extend_from_slice(remainder);

    let mut small = vec![0u8; remainder.len() - 1];
    let mut buffers = [IoSliceMut::new(small.as_mut_slice())];

    let err = sniffer
        .take_buffered_remainder_into_vectored(&mut buffers)
        .expect_err("insufficient vectored remainder capacity should error");

    assert_eq!(err.required(), remainder.len());
    assert_eq!(err.available(), small.len());
    assert_eq!(sniffer.buffered_remainder(), remainder);
    assert_eq!(sniffer.buffered(), {
        let mut expected = LEGACY_DAEMON_PREFIX.as_bytes().to_vec();
        expected.extend_from_slice(remainder);
        expected
    });
}

#[test]
fn prologue_sniffer_take_buffered_remainder_into_writer_transfers_tail() {
    let mut sniffer = NegotiationPrologueSniffer::new();
    let (decision, consumed) = sniffer
        .observe(LEGACY_DAEMON_PREFIX.as_bytes())
        .expect("buffer reservation succeeds");
    assert_eq!(decision, NegotiationPrologue::LegacyAscii);
    assert_eq!(consumed, LEGACY_DAEMON_PREFIX_LEN);

    sniffer.buffered_storage_mut().extend_from_slice(b" extra");

    let mut sink = Vec::new();
    let written = sniffer
        .take_buffered_remainder_into_writer(&mut sink)
        .expect("remainder should be written to sink");

    assert_eq!(sink, b" extra");
    assert_eq!(written, sink.len());
    assert_eq!(sniffer.buffered(), LEGACY_DAEMON_PREFIX.as_bytes());
    assert_eq!(sniffer.buffered_remainder(), b"");
}

struct FailingWriter {
    error: io::Error,
}

impl FailingWriter {
    fn new() -> Self {
        Self {
            error: io::Error::other("simulated write failure"),
        }
    }
}

impl Write for FailingWriter {
    fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
        Err(io::Error::other(self.error.to_string()))
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn prologue_sniffer_take_buffered_into_writer_preserves_buffer_on_error() {
    let mut sniffer = NegotiationPrologueSniffer::new();
    let mut reader = Cursor::new(b"@RSYNCD: 29.0\n".to_vec());
    sniffer
        .read_from(&mut reader)
        .expect("legacy negotiation detection succeeds");

    let mut failing = FailingWriter::new();
    let err = sniffer
        .take_buffered_into_writer(&mut failing)
        .expect_err("writer failure should be propagated");
    assert_eq!(err.kind(), io::ErrorKind::Other);
    assert_eq!(sniffer.buffered(), LEGACY_DAEMON_PREFIX.as_bytes());
}

#[test]
fn prologue_sniffer_take_sniffed_prefix_into_preserves_remainder() {
    let mut sniffer = NegotiationPrologueSniffer::new();
    let payload = LEGACY_DAEMON_PREFIX.as_bytes();
    let (decision, consumed) = sniffer
        .observe(payload)
        .expect("buffer reservation succeeds");
    assert_eq!(decision, NegotiationPrologue::LegacyAscii);
    assert_eq!(consumed, LEGACY_DAEMON_PREFIX_LEN);

    let remainder = b"module data";
    sniffer.buffered_storage_mut().extend_from_slice(remainder);

    let mut prefix = Vec::new();
    let drained = sniffer
        .take_sniffed_prefix_into(&mut prefix)
        .expect("draining prefix should not allocate");
    assert_eq!(drained, LEGACY_DAEMON_PREFIX_LEN);
    assert_eq!(prefix, payload);
    assert_eq!(sniffer.sniffed_prefix_len(), 0);
    assert_eq!(sniffer.buffered(), remainder);
    assert_eq!(sniffer.decision(), Some(NegotiationPrologue::LegacyAscii));
    assert!(!sniffer.requires_more_data());
}

#[test]
fn prologue_sniffer_take_sniffed_prefix_into_slice_preserves_remainder() {
    let mut sniffer = NegotiationPrologueSniffer::new();
    let payload = LEGACY_DAEMON_PREFIX_BYTES;
    let (decision, consumed) = sniffer
        .observe(payload.as_slice())
        .expect("buffer reservation succeeds");
    assert_eq!(decision, NegotiationPrologue::LegacyAscii);
    assert_eq!(consumed, LEGACY_DAEMON_PREFIX_LEN);

    let remainder = b"module list";
    sniffer.buffered_storage_mut().extend_from_slice(remainder);

    let mut scratch = [0u8; LEGACY_DAEMON_PREFIX_LEN];
    let copied = sniffer
        .take_sniffed_prefix_into_slice(&mut scratch)
        .expect("copying sniffed prefix into slice succeeds");
    assert_eq!(copied, LEGACY_DAEMON_PREFIX_LEN);
    assert_eq!(&scratch[..copied], &payload[..]);
    assert_eq!(sniffer.sniffed_prefix_len(), 0);
    assert_eq!(sniffer.buffered(), remainder);
    assert_eq!(sniffer.decision(), Some(NegotiationPrologue::LegacyAscii));
    assert!(!sniffer.requires_more_data());
}

#[test]
fn prologue_sniffer_take_sniffed_prefix_into_writer_preserves_remainder() {
    let mut sniffer = NegotiationPrologueSniffer::new();
    let mut reader = Cursor::new(LEGACY_DAEMON_PREFIX_BYTES.to_vec());
    let decision = sniffer
        .read_from(&mut reader)
        .expect("legacy negotiation detection succeeds");
    assert_eq!(decision, NegotiationPrologue::LegacyAscii);

    let tail = b"payload";
    sniffer.buffered_storage_mut().extend_from_slice(tail);

    let mut sink = Vec::new();
    let written = sniffer
        .take_sniffed_prefix_into_writer(&mut sink)
        .expect("writing sniffed prefix succeeds");
    assert_eq!(written, LEGACY_DAEMON_PREFIX_LEN);
    assert_eq!(sink.as_slice(), LEGACY_DAEMON_PREFIX.as_bytes());
    assert_eq!(sniffer.sniffed_prefix_len(), 0);
    assert_eq!(sniffer.buffered(), tail);
}

#[test]
fn prologue_sniffer_take_sniffed_prefix_into_writer_handles_binary_negotiation() {
    let mut sniffer = NegotiationPrologueSniffer::new();
    let (decision, consumed) = sniffer
        .observe(&[0x26, 0x01, 0x02])
        .expect("buffer reservation succeeds");
    assert_eq!(decision, NegotiationPrologue::Binary);
    assert_eq!(consumed, 1);

    sniffer
        .buffered_storage_mut()
        .extend_from_slice(&[0x01, 0x02]);

    let mut sink = Vec::new();
    let written = sniffer
        .take_sniffed_prefix_into_writer(&mut sink)
        .expect("writing sniffed binary prefix succeeds");
    assert_eq!(written, 1);
    assert_eq!(sink, vec![0x26]);
    assert!(sniffer.sniffed_prefix().is_empty());
    assert_eq!(sniffer.buffered(), &[0x01, 0x02]);
}

#[test]
fn prologue_sniffer_take_sniffed_prefix_into_writer_preserves_buffer_on_error() {
    let mut sniffer = NegotiationPrologueSniffer::new();
    let mut reader = Cursor::new(b"@RSYNCD: 30.0\n".to_vec());
    let decision = sniffer
        .read_from(&mut reader)
        .expect("legacy negotiation detection succeeds");
    assert_eq!(decision, NegotiationPrologue::LegacyAscii);

    let mut failing = FailingWriter::new();
    let err = sniffer
        .take_sniffed_prefix_into_writer(&mut failing)
        .expect_err("writer failure should be surfaced");
    assert_eq!(err.kind(), io::ErrorKind::Other);
    assert_eq!(sniffer.buffered(), LEGACY_DAEMON_PREFIX.as_bytes());
}

#[test]
fn prologue_sniffer_take_sniffed_prefix_into_handles_binary_negotiation() {
    let mut sniffer = NegotiationPrologueSniffer::new();
    let (decision, consumed) = sniffer
        .observe(&[0x7F, 0x00, 0x01])
        .expect("buffer reservation succeeds");
    assert_eq!(decision, NegotiationPrologue::Binary);
    assert_eq!(consumed, 1);

    sniffer
        .buffered_storage_mut()
        .extend_from_slice(&[0xAA, 0x55]);

    let mut prefix = Vec::new();
    let drained = sniffer
        .take_sniffed_prefix_into(&mut prefix)
        .expect("binary prefix extraction should succeed");
    assert_eq!(drained, 1);
    assert_eq!(prefix, &[0x7F]);
    assert!(sniffer.sniffed_prefix().is_empty());
    assert_eq!(sniffer.buffered(), &[0xAA, 0x55]);
    assert_eq!(sniffer.decision(), Some(NegotiationPrologue::Binary));
    assert!(!sniffer.requires_more_data());
}

#[test]
fn prologue_sniffer_take_sniffed_prefix_into_clears_destination_after_drain() {
    let mut sniffer = NegotiationPrologueSniffer::new();
    let (decision, consumed) = sniffer
        .observe(LEGACY_DAEMON_PREFIX.as_bytes())
        .expect("buffer reservation succeeds");
    assert_eq!(decision, NegotiationPrologue::LegacyAscii);
    assert_eq!(consumed, LEGACY_DAEMON_PREFIX_LEN);
    assert!(sniffer.legacy_prefix_complete());

    let mut prefix = Vec::with_capacity(LEGACY_DAEMON_PREFIX_LEN);
    let drained = sniffer
        .take_sniffed_prefix_into(&mut prefix)
        .expect("initial prefix drain succeeds");
    assert_eq!(drained, LEGACY_DAEMON_PREFIX_LEN);
    assert_eq!(prefix, LEGACY_DAEMON_PREFIX.as_bytes());

    prefix.extend_from_slice(b"stale");
    let drained_again = sniffer
        .take_sniffed_prefix_into(&mut prefix)
        .expect("subsequent drain should be a no-op");
    assert_eq!(drained_again, 0);
    assert!(prefix.is_empty());
}

#[test]
fn prologue_sniffer_take_sniffed_prefix_into_slice_copies_prefix() {
    let mut sniffer = NegotiationPrologueSniffer::new();
    let payload = LEGACY_DAEMON_PREFIX.as_bytes();
    let (decision, consumed) = sniffer
        .observe(payload)
        .expect("buffer reservation succeeds");
    assert_eq!(decision, NegotiationPrologue::LegacyAscii);
    assert_eq!(consumed, LEGACY_DAEMON_PREFIX_LEN);

    let remainder = b"module";
    sniffer.buffered_storage_mut().extend_from_slice(remainder);

    let mut scratch = [0xAA; LEGACY_DAEMON_PREFIX_LEN];
    let drained = sniffer
        .take_sniffed_prefix_into_slice(&mut scratch)
        .expect("slice large enough to hold prefix");

    assert_eq!(drained, LEGACY_DAEMON_PREFIX_LEN);
    assert_eq!(scratch, *LEGACY_DAEMON_PREFIX_BYTES);
    assert_eq!(sniffer.buffered(), remainder);
    assert_eq!(sniffer.decision(), Some(NegotiationPrologue::LegacyAscii));
}

#[test]
fn prologue_sniffer_take_sniffed_prefix_into_slice_reports_small_buffer() {
    let mut sniffer = NegotiationPrologueSniffer::new();
    sniffer
        .observe(LEGACY_DAEMON_PREFIX.as_bytes())
        .expect("buffer reservation succeeds");

    let mut scratch = [0u8; LEGACY_DAEMON_PREFIX_LEN - 1];
    let err = sniffer
        .take_sniffed_prefix_into_slice(&mut scratch)
        .expect_err("insufficient slice should error");

    assert_eq!(err.required(), LEGACY_DAEMON_PREFIX_LEN);
    assert_eq!(err.available(), scratch.len());
    assert_eq!(sniffer.buffered(), LEGACY_DAEMON_PREFIX.as_bytes());
    assert_eq!(sniffer.decision(), Some(NegotiationPrologue::LegacyAscii));
}

#[test]
fn prologue_sniffer_take_sniffed_prefix_into_array_copies_prefix() {
    let mut sniffer = NegotiationPrologueSniffer::new();
    let (decision, consumed) = sniffer
        .observe(&[0x7F, 0x80])
        .expect("buffer reservation succeeds");
    assert_eq!(decision, NegotiationPrologue::Binary);
    assert_eq!(consumed, 1);

    let remainder = [0x90, 0x91];
    sniffer.buffered_storage_mut().extend_from_slice(&remainder);

    let mut scratch = [0xFFu8; LEGACY_DAEMON_PREFIX_LEN];
    let drained = sniffer
        .take_sniffed_prefix_into_array(&mut scratch)
        .expect("array large enough for binary prefix");

    assert_eq!(drained, 1);
    assert_eq!(scratch[0], 0x7F);
    assert!(scratch[1..].iter().all(|&byte| byte == 0xFF));
    assert_eq!(sniffer.buffered(), &remainder);
    assert_eq!(sniffer.decision(), Some(NegotiationPrologue::Binary));
}

#[test]
fn prologue_sniffer_take_sniffed_prefix_into_is_noop_when_prefix_incomplete() {
    let mut sniffer = NegotiationPrologueSniffer::new();
    let (decision, consumed) = sniffer.observe(b"@R").expect("buffer reservation succeeds");
    assert_eq!(decision, NegotiationPrologue::NeedMoreData);
    assert_eq!(consumed, 2);
    assert!(sniffer.requires_more_data());

    let mut prefix = Vec::new();
    prefix.extend_from_slice(b"previous");
    let drained = sniffer
        .take_sniffed_prefix_into(&mut prefix)
        .expect("draining incomplete prefix should not allocate");
    assert_eq!(drained, 0);
    assert_eq!(prefix, b"previous");
    assert_eq!(sniffer.buffered(), b"@R");
    assert!(sniffer.requires_more_data());
}

#[test]
fn prologue_sniffer_take_sniffed_prefix_preserves_remainder() {
    let mut sniffer = NegotiationPrologueSniffer::new();
    let mut reader = Cursor::new(LEGACY_DAEMON_PREFIX_BYTES.to_vec());
    let decision = sniffer
        .read_from(&mut reader)
        .expect("legacy negotiation detection succeeds");
    assert_eq!(decision, NegotiationPrologue::LegacyAscii);

    let remainder = b"module data";
    sniffer.buffered_storage_mut().extend_from_slice(remainder);
    let remainder_snapshot = sniffer.buffered_remainder().to_vec();

    let prefix = sniffer.take_sniffed_prefix();
    assert_eq!(prefix, LEGACY_DAEMON_PREFIX_BYTES);
    assert_eq!(sniffer.sniffed_prefix_len(), 0);
    assert_eq!(sniffer.buffered(), remainder_snapshot);
    assert_eq!(sniffer.buffered_remainder(), remainder);
    assert_eq!(sniffer.decision(), Some(NegotiationPrologue::LegacyAscii));
    assert!(!sniffer.requires_more_data());
}

#[test]
fn prologue_sniffer_take_sniffed_prefix_is_noop_when_prefix_incomplete() {
    let mut sniffer = NegotiationPrologueSniffer::new();
    sniffer.observe(b"@R").expect("buffer reservation succeeds");
    assert!(sniffer.requires_more_data());

    let before = sniffer.buffered().to_vec();
    let prefix = sniffer.take_sniffed_prefix();
    assert!(prefix.is_empty());
    assert_eq!(sniffer.buffered(), before);
    assert!(sniffer.requires_more_data());
}

#[test]
fn prologue_sniffer_take_sniffed_prefix_handles_binary_negotiation() {
    let mut sniffer = NegotiationPrologueSniffer::new();
    let mut reader = Cursor::new(vec![0x7F, 0x01, 0x02]);
    let decision = sniffer
        .read_from(&mut reader)
        .expect("binary negotiation detection succeeds");
    assert_eq!(decision, NegotiationPrologue::Binary);

    sniffer
        .buffered_storage_mut()
        .extend_from_slice(&[0xAA, 0x55]);

    let prefix = sniffer.take_sniffed_prefix();
    assert_eq!(prefix, vec![0x7F]);
    assert_eq!(sniffer.sniffed_prefix_len(), 0);
    assert_eq!(sniffer.buffered(), &[0xAA, 0x55]);
    assert_eq!(sniffer.decision(), Some(NegotiationPrologue::Binary));
    assert!(!sniffer.requires_more_data());
}

#[test]
fn prologue_sniffer_reset_trims_oversized_buffer_capacity() {
    let mut sniffer = NegotiationPrologueSniffer::new();
    let oversized = LEGACY_DAEMON_PREFIX_LEN * 4;
    *sniffer.buffered_storage_mut() = Vec::with_capacity(oversized);
    assert!(
        sniffer.buffered_storage().capacity() >= oversized,
        "allocator must provide at least the requested oversize capacity"
    );

    sniffer.reset();

    assert!(sniffer.buffered().is_empty());
    assert_eq!(sniffer.decision(), None);
    assert!(
        sniffer.buffered_storage().capacity() <= LEGACY_DAEMON_PREFIX_LEN,
        "reset should shrink oversize buffers back to the canonical prefix capacity"
    );
    assert!(sniffer.requires_more_data());
}
