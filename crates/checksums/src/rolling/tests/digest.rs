use super::super::*;
use super::reference_digest;

use std::io::Cursor;

#[test]
fn digest_matches_reference_for_known_input() {
    let data = b"rsync rolling checksum";
    let digest = reference_digest(data);

    let mut checksum = RollingChecksum::new();
    checksum.update(data);
    assert_eq!(checksum.digest(), digest);
    assert_eq!(checksum.value(), digest.value());
}

#[test]
fn digest_default_matches_zero_constant() {
    let digest = RollingDigest::default();
    assert_eq!(digest, RollingDigest::ZERO);
    assert!(digest.is_empty());
    assert_eq!(digest.sum1(), 0);
    assert_eq!(digest.sum2(), 0);
}

#[test]
fn digest_from_bytes_matches_manual_update() {
    let data = b"from bytes helper";
    let digest = RollingDigest::from_bytes(data);

    let manual = {
        let mut checksum = RollingChecksum::new();
        checksum.update(data);
        checksum.digest()
    };

    assert_eq!(digest, manual);
    assert_eq!(digest.len(), data.len());
}

#[test]
fn digest_round_trips_through_packed_value() {
    let sample = RollingDigest::new(0x1357, 0x2468, 4096);
    let packed = sample.value();
    let unpacked = RollingDigest::from_value(packed, sample.len());

    assert_eq!(unpacked, sample);
    assert_eq!(unpacked.value(), packed);
    assert_eq!(unpacked.sum1(), sample.sum1());
    assert_eq!(unpacked.sum2(), sample.sum2());
    assert_eq!(unpacked.len(), sample.len());
}

#[test]
fn digest_round_trips_through_le_bytes() {
    let sample = RollingDigest::new(0xabcd, 0x1234, 512);
    let bytes = sample.to_le_bytes();
    let parsed = RollingDigest::from_le_bytes(bytes, sample.len());

    assert_eq!(parsed, sample);
    assert_eq!(parsed.to_le_bytes(), bytes);
    assert_eq!(parsed.sum1(), sample.sum1());
    assert_eq!(parsed.sum2(), sample.sum2());
    assert_eq!(parsed.len(), sample.len());
}

#[test]
fn digest_round_trips_through_le_slice() {
    let sample = RollingDigest::new(0x1357, 0x2468, 1024);
    let parsed = RollingDigest::from_le_slice(&sample.to_le_bytes(), sample.len())
        .expect("slice length matches the digest encoding");

    assert_eq!(parsed, sample);
    assert_eq!(parsed.to_le_bytes(), sample.to_le_bytes());
}

#[test]
fn digest_into_u32_matches_value() {
    let sample = RollingDigest::new(0x4321, 0x8765, 2048);
    let expected = sample.value();
    let packed = u32::from(sample);

    assert_eq!(packed, expected);
}

#[test]
fn digest_ref_into_u32_matches_value() {
    let sample = RollingDigest::new(0x1357, 0x2468, 1024);
    let expected = sample.value();
    let packed = u32::from(&sample);

    assert_eq!(packed, expected);
}

#[test]
fn digest_into_array_matches_le_bytes() {
    let sample = RollingDigest::new(0x1234, 0x5678, 4096);
    let array: [u8; 4] = sample.into();

    assert_eq!(array, sample.to_le_bytes());
}

#[test]
fn digest_ref_into_array_matches_le_bytes() {
    let sample = RollingDigest::new(0x2468, 0x1357, 8192);
    let array: [u8; 4] = (&sample).into();

    assert_eq!(array, sample.to_le_bytes());
}

#[test]
fn digest_from_le_slice_rejects_incorrect_length() {
    let err = RollingDigest::from_le_slice(&[0u8; 3], 0).expect_err("length mismatch");
    assert_eq!(err.len(), 3);
}

#[test]
fn digest_read_le_from_reports_truncated_input() {
    let mut cursor = Cursor::new(vec![0u8; 3]);
    let err = RollingDigest::read_le_from(&mut cursor, 0).expect_err("short read");
    assert_eq!(err.kind(), std::io::ErrorKind::UnexpectedEof);
}

#[test]
fn digest_write_le_bytes_populates_target_slice() {
    let digest = RollingDigest::new(0xaaaa, 0xbbbb, 1024);
    let mut target = [0u8; 4];
    digest
        .write_le_bytes(&mut target)
        .expect("buffer length matches");
    assert_eq!(target, digest.to_le_bytes());
}

#[test]
fn digest_write_le_bytes_rejects_wrong_length() {
    let digest = RollingDigest::new(0x1111, 0x2222, 512);
    let mut buf = [0u8; 3];
    let err = digest
        .write_le_bytes(&mut buf)
        .expect_err("length mismatch must fail");
    assert_eq!(err.len(), buf.len());
}

#[test]
fn digest_write_le_to_matches_le_bytes() {
    let digest = RollingDigest::new(0x1234, 0x5678, 2048);
    let mut cursor = Cursor::new(Vec::new());
    digest.write_le_to(&mut cursor).expect("write succeeds");
    assert_eq!(cursor.into_inner(), digest.to_le_bytes());
}

#[test]
fn digest_from_reader_matches_manual_update() {
    let data = b"digest reader";
    let mut cursor = Cursor::new(&data[..]);

    let digest = RollingDigest::from_reader(&mut cursor).expect("reader succeeds");

    let mut checksum = RollingChecksum::new();
    checksum.update(data);

    assert_eq!(digest, checksum.digest());
}

#[test]
fn digest_from_reader_with_buffer_matches_manual_update() {
    let data = b"digest reader with buffer";
    let mut cursor = Cursor::new(&data[..]);
    let mut buffer = [0u8; 5];

    let digest =
        RollingDigest::from_reader_with_buffer(&mut cursor, &mut buffer).expect("reader succeeds");

    let mut checksum = RollingChecksum::new();
    checksum.update(data);

    assert_eq!(digest, checksum.digest());
}

#[test]
fn digest_from_reader_with_buffer_rejects_empty_buffer() {
    let mut cursor = Cursor::new(b"data");
    let mut buffer = [0u8; 0];

    let err = RollingDigest::from_reader_with_buffer(&mut cursor, &mut buffer)
        .expect_err("empty buffer must fail");
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
}
