#[test]
fn prologue_sniffer_with_buffer_reuses_canonical_allocation() {
    let mut buffer = Vec::with_capacity(LEGACY_DAEMON_PREFIX_LEN);
    buffer.extend_from_slice(b"junk");
    let ptr = buffer.as_ptr();

    let sniffer = NegotiationPrologueSniffer::with_buffer(buffer);

    assert_eq!(sniffer.decision(), None, "fresh sniffer must be undecided");
    assert!(sniffer.buffered().is_empty(), "buffer must start empty");
    assert_eq!(
        sniffer.buffered_storage().capacity(),
        LEGACY_DAEMON_PREFIX_LEN,
        "capacity should match canonical prefix length",
    );
    assert!(
        ptr::eq(sniffer.buffered_storage().as_ptr(), ptr),
        "canonical capacity should be reused without reallocating",
    );
}

#[test]
fn prologue_sniffer_with_buffer_trims_oversized_allocations() {
    let buffer = vec![0u8; LEGACY_DAEMON_PREFIX_LEN * 4];

    let sniffer = NegotiationPrologueSniffer::with_buffer(buffer);

    assert!(sniffer.buffered().is_empty());
    assert_eq!(sniffer.decision(), None);
    assert_eq!(
        sniffer.buffered_storage().capacity(),
        LEGACY_DAEMON_PREFIX_LEN,
        "oversized buffers should shrink to canonical prefix length",
    );
}

#[test]
fn prologue_sniffer_with_buffer_grows_small_allocations() {
    let buffer = Vec::with_capacity(1);

    let sniffer = NegotiationPrologueSniffer::with_buffer(buffer);

    assert!(sniffer.buffered().is_empty());
    assert_eq!(sniffer.decision(), None);
    assert!(
        sniffer.buffered_storage().capacity() >= LEGACY_DAEMON_PREFIX_LEN,
        "small buffers must grow to hold the canonical prefix",
    );
}
