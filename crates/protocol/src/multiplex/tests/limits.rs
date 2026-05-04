use super::*;
use std::io;

#[test]
fn reserve_payload_extends_capacity_for_empty_buffers() {
    let mut buffer = Vec::with_capacity(4);
    assert!(buffer.capacity() < 10);

    reserve_payload(&mut buffer, 10).expect("reserve succeeds");

    assert!(
        buffer.capacity() >= 10,
        "capacity {} should be at least required length",
        buffer.capacity()
    );
    assert_eq!(buffer.len(), 0, "reserve must not mutate length");
}

#[test]
fn reserve_payload_extends_relative_to_current_length() {
    let mut buffer = Vec::with_capacity(8);
    buffer.extend_from_slice(&[0u8; 3]);
    assert_eq!(buffer.len(), 3);
    assert!(buffer.capacity() < 12);

    reserve_payload(&mut buffer, 12).expect("reserve succeeds");

    assert!(
        buffer.capacity() >= 12,
        "capacity {} should be at least required length",
        buffer.capacity()
    );
    assert_eq!(buffer.len(), 3, "reserve must not mutate length");
}

#[test]
fn reserve_payload_rejects_capacity_overflow() {
    let mut buffer = Vec::new();
    let err = reserve_payload(&mut buffer, usize::MAX).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::OutOfMemory);
}

#[test]
fn reserve_payload_maps_overflow_to_out_of_memory() {
    let mut buffer = Vec::new();
    let err = reserve_payload(&mut buffer, usize::MAX).unwrap_err();

    assert_eq!(err.kind(), io::ErrorKind::OutOfMemory);
}

#[test]
fn ensure_payload_length_accepts_maximum_payload() {
    let len = MAX_PAYLOAD_LENGTH as usize;
    let validated = ensure_payload_length(len).expect("maximum allowed");

    assert_eq!(validated, MAX_PAYLOAD_LENGTH);
}

#[test]
fn ensure_payload_length_rejects_values_above_limit() {
    let len = MAX_PAYLOAD_LENGTH as usize + 1;
    let err = ensure_payload_length(len).unwrap_err();

    assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    assert_eq!(
        err.to_string(),
        format!(
            "multiplexed payload length {} exceeds maximum {}",
            u128::from(MAX_PAYLOAD_LENGTH) + 1,
            u128::from(MAX_PAYLOAD_LENGTH)
        )
    );
}
