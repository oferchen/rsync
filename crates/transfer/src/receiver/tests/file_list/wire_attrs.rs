//! Wire-format coverage for the receiver's `SumHead` block header and
//! the `SenderAttrs` per-file attribute frame (NDX + iflags + basis +
//! xname). Splits the protocol 28-only and protocol 29+ paths plus the
//! NDX-codec round-trips for protocols 30+.

use std::io::{self, Cursor};

use super::super::super::wire::{SenderAttrs, SumHead};

#[test]
fn sum_head_new_creates_with_correct_values() {
    let sum_head = SumHead::new(100, 1024, 16, 512);
    assert_eq!(sum_head.count, 100);
    assert_eq!(sum_head.blength, 1024);
    assert_eq!(sum_head.s2length, 16);
    assert_eq!(sum_head.remainder, 512);
}

#[test]
fn sum_head_empty_creates_zero_values() {
    let sum_head = SumHead::empty();
    assert_eq!(sum_head.count, 0);
    assert_eq!(sum_head.blength, 0);
    assert_eq!(sum_head.s2length, 0);
    assert_eq!(sum_head.remainder, 0);
    assert!(sum_head.is_empty());
}

#[test]
fn sum_head_default_is_empty() {
    let sum_head = SumHead::default();
    assert!(sum_head.is_empty());
    assert_eq!(sum_head, SumHead::empty());
}

#[test]
fn sum_head_is_empty_false_for_nonzero_count() {
    let sum_head = SumHead::new(1, 1024, 16, 0);
    assert!(!sum_head.is_empty());
}

#[test]
fn sum_head_write_produces_correct_wire_format() {
    let sum_head = SumHead::new(10, 700, 16, 100);
    let mut output = Vec::new();
    sum_head.write(&mut output).unwrap();

    assert_eq!(output.len(), 16);
    // All values as 32-bit little-endian
    assert_eq!(
        i32::from_le_bytes([output[0], output[1], output[2], output[3]]),
        10
    );
    assert_eq!(
        i32::from_le_bytes([output[4], output[5], output[6], output[7]]),
        700
    );
    assert_eq!(
        i32::from_le_bytes([output[8], output[9], output[10], output[11]]),
        16
    );
    assert_eq!(
        i32::from_le_bytes([output[12], output[13], output[14], output[15]]),
        100
    );
}

#[test]
fn sum_head_read_parses_wire_format() {
    // Prepare wire data: count=5, blength=512, s2length=16, remainder=128
    let mut data = Vec::new();
    data.extend_from_slice(&5i32.to_le_bytes());
    data.extend_from_slice(&512i32.to_le_bytes());
    data.extend_from_slice(&16i32.to_le_bytes());
    data.extend_from_slice(&128i32.to_le_bytes());

    let sum_head = SumHead::read(&mut Cursor::new(data)).unwrap();

    assert_eq!(sum_head.count, 5);
    assert_eq!(sum_head.blength, 512);
    assert_eq!(sum_head.s2length, 16);
    assert_eq!(sum_head.remainder, 128);
}

#[test]
fn sum_head_round_trip() {
    let original = SumHead::new(100, 1024, 20, 256);

    let mut buf = Vec::new();
    original.write(&mut buf).unwrap();

    let decoded = SumHead::read(&mut Cursor::new(buf)).unwrap();
    assert_eq!(original, decoded);
}

#[test]
fn sum_head_read_insufficient_data() {
    // Only 8 bytes instead of 16
    let data = vec![0u8; 8];
    let result = SumHead::read(&mut Cursor::new(data));
    assert!(result.is_err());
    assert_eq!(result.unwrap_err().kind(), io::ErrorKind::UnexpectedEof);
}

/// Encodes a raw 16-byte sum_head wire frame from signed field values.
fn sum_head_bytes(count: i32, blength: i32, s2length: i32, remainder: i32) -> Vec<u8> {
    let mut data = Vec::with_capacity(16);
    data.extend_from_slice(&count.to_le_bytes());
    data.extend_from_slice(&blength.to_le_bytes());
    data.extend_from_slice(&s2length.to_le_bytes());
    data.extend_from_slice(&remainder.to_le_bytes());
    data
}

/// A crafted sum_head with an enormous strong-sum length must be rejected with
/// `InvalidData` (RERR_PROTOCOL) rather than driving a multi-gigabyte `vec!`.
/// Guards the OOM site at `generator/protocol_io.rs` `vec![0u8; s2length]`.
#[test]
fn sum_head_rejects_oversized_s2length() {
    let data = sum_head_bytes(1, 512, i32::MAX, 0);
    let err = SumHead::read(&mut Cursor::new(data)).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidData);
}

/// A crafted sum_head with a negative block count must be rejected, mirroring
/// upstream io.c:2029, so `Vec::with_capacity(count)` never sees garbage.
#[test]
fn sum_head_rejects_negative_count() {
    let data = sum_head_bytes(-1, 512, 16, 0);
    let err = SumHead::read(&mut Cursor::new(data)).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidData);
}

/// A block length beyond the legacy MAX_BLOCK_SIZE ceiling is rejected
/// (upstream io.c:2050).
#[test]
fn sum_head_rejects_oversized_blength() {
    let data = sum_head_bytes(1, i32::MAX, 16, 0);
    let err = SumHead::read(&mut Cursor::new(data)).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidData);
}

/// A zero block length with a non-zero count is nonsense (division-by-zero) and
/// must be rejected.
#[test]
fn sum_head_rejects_zero_blength_with_blocks() {
    let data = sum_head_bytes(4, 0, 16, 0);
    let err = SumHead::read(&mut Cursor::new(data)).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidData);
}

/// A remainder larger than the block length is rejected (upstream io.c:2062).
#[test]
fn sum_head_rejects_remainder_over_blength() {
    let data = sum_head_bytes(1, 512, 16, 513);
    let err = SumHead::read(&mut Cursor::new(data)).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidData);
}

/// A legitimate large-file sum_head (SHA1 20-byte strong sums, max block size,
/// many blocks) must still decode cleanly - the bounds mirror upstream exactly,
/// so any header upstream accepts, oc accepts.
#[test]
fn sum_head_accepts_legit_large_transfer() {
    // count near the u32 edge, blength at the legacy ceiling, s2length = SHA1.
    let data = sum_head_bytes(1_000_000, 1 << 29, 20, (1 << 29) - 1);
    let sum_head = SumHead::read(&mut Cursor::new(data)).unwrap();
    assert_eq!(sum_head.count, 1_000_000);
    assert_eq!(sum_head.blength, 1 << 29);
    assert_eq!(sum_head.s2length, 20);
    assert_eq!(sum_head.remainder, (1 << 29) - 1);
}

/// The zero-count whole-file sentinel (`count=0`) with all-zero fields decodes
/// as an empty header, unaffected by the new bounds.
#[test]
fn sum_head_accepts_empty_whole_file() {
    let data = sum_head_bytes(0, 0, 0, 0);
    let sum_head = SumHead::read(&mut Cursor::new(data)).unwrap();
    assert!(sum_head.is_empty());
}

#[test]
fn sender_attrs_read_protocol_28_returns_default_iflags() {
    // Protocol 28 just reads the NDX byte, no iflags
    let data = vec![0x05u8]; // NDX byte only
    let attrs = SenderAttrs::read(&mut Cursor::new(data), 28).unwrap();

    assert_eq!(attrs.iflags, SenderAttrs::ITEM_TRANSFER);
    assert!(attrs.fnamecmp_type.is_none());
    assert!(attrs.xname.is_none());
}

#[test]
fn sender_attrs_read_protocol_29_parses_iflags() {
    // NDX byte + iflags (0x8000 = ITEM_TRANSFER)
    let mut data = vec![0x05u8]; // NDX byte
    data.extend_from_slice(&0x8000u16.to_le_bytes()); // iflags

    let attrs = SenderAttrs::read(&mut Cursor::new(data), 29).unwrap();

    assert_eq!(attrs.iflags, 0x8000);
    assert!(attrs.fnamecmp_type.is_none());
    assert!(attrs.xname.is_none());
}

#[test]
fn sender_attrs_read_with_basis_type() {
    // NDX byte + iflags (0x8800 = ITEM_TRANSFER | ITEM_BASIS_TYPE_FOLLOWS) + fnamecmp_type
    let mut data = vec![0x05u8]; // NDX byte
    data.extend_from_slice(&0x8800u16.to_le_bytes()); // iflags with BASIS_TYPE_FOLLOWS
    data.push(0x02); // fnamecmp_type = BasisDir(2)

    let attrs = SenderAttrs::read(&mut Cursor::new(data), 29).unwrap();

    assert_eq!(attrs.iflags, 0x8800);
    assert_eq!(
        attrs.fnamecmp_type,
        Some(protocol::FnameCmpType::BasisDir(2))
    );
    assert!(attrs.xname.is_none());
}

#[test]
fn sender_attrs_read_with_short_xname() {
    // NDX byte + iflags (0x9000 = ITEM_TRANSFER | ITEM_XNAME_FOLLOWS) + xname
    let mut data = vec![0x05u8]; // NDX byte
    data.extend_from_slice(&0x9000u16.to_le_bytes()); // iflags with XNAME_FOLLOWS
    data.push(0x04); // xname length (short form)
    data.extend_from_slice(b"test"); // xname content

    let attrs = SenderAttrs::read(&mut Cursor::new(data), 29).unwrap();

    assert_eq!(attrs.iflags, 0x9000);
    assert!(attrs.fnamecmp_type.is_none());
    assert_eq!(attrs.xname, Some(b"test".to_vec()));
}

#[test]
fn sender_attrs_read_with_long_xname() {
    // NDX + iflags + xname with extended length (> 127 bytes requires 2-byte length)
    let mut data = vec![0x05u8]; // NDX byte
    data.extend_from_slice(&0x9000u16.to_le_bytes()); // iflags with XNAME_FOLLOWS
    // Length 300 = 0x80 | (300 / 256) = 0x81, then 300 % 256 = 44
    data.push(0x81); // High byte: 0x80 flag + 1
    data.push(0x2C); // Low byte: 44 (1*256 + 44 = 300)
    data.extend(vec![b'x'; 300]); // xname content (300 'x' characters)

    let attrs = SenderAttrs::read(&mut Cursor::new(data), 29).unwrap();

    assert_eq!(attrs.iflags, 0x9000);
    assert!(attrs.fnamecmp_type.is_none());
    assert_eq!(attrs.xname.as_ref().unwrap().len(), 300);
}

#[test]
fn sender_attrs_read_empty_returns_eof_error() {
    let data: Vec<u8> = vec![];
    let result = SenderAttrs::read(&mut Cursor::new(data), 29);

    assert!(result.is_err());
    assert_eq!(result.unwrap_err().kind(), io::ErrorKind::UnexpectedEof);
}

#[test]
fn sender_attrs_constants_match_upstream() {
    // Verify our constants match upstream rsync.h values
    assert_eq!(SenderAttrs::ITEM_TRANSFER, 0x8000);
    assert_eq!(SenderAttrs::ITEM_BASIS_TYPE_FOLLOWS, 0x0800);
    assert_eq!(SenderAttrs::ITEM_XNAME_FOLLOWS, 0x1000);
}

#[test]
fn sender_attrs_read_with_codec_protocol_30_delta_encoded() {
    use protocol::codec::{NdxCodec, create_ndx_codec};

    // Simulate sender encoding NDX 0 for protocol 30+
    // With prev_positive=-1, ndx=0, diff=1, encoded as single byte 0x01
    let mut sender_codec = create_ndx_codec(31);
    let mut wire_data = Vec::new();
    sender_codec.write_ndx(&mut wire_data, 0).unwrap();
    // Add iflags (ITEM_TRANSFER = 0x8000)
    wire_data.extend_from_slice(&0x8000u16.to_le_bytes());

    // Receiver reads with its own codec
    let mut receiver_codec = create_ndx_codec(31);
    let mut cursor = Cursor::new(&wire_data);
    let (ndx, attrs) = SenderAttrs::read_with_codec(&mut cursor, &mut receiver_codec).unwrap();

    assert_eq!(ndx, 0);
    assert_eq!(attrs.iflags, 0x8000);
}

#[test]
fn sender_attrs_read_with_codec_protocol_30_sequential_indices() {
    use protocol::codec::{NdxCodec, create_ndx_codec};

    // Simulate sender sending sequential indices 0, 1, 2
    let mut sender_codec = create_ndx_codec(31);
    let mut wire_data = Vec::new();
    for ndx in 0..3 {
        sender_codec.write_ndx(&mut wire_data, ndx).unwrap();
        wire_data.extend_from_slice(&0x8000u16.to_le_bytes());
    }

    // Receiver reads all three
    let mut receiver_codec = create_ndx_codec(31);
    let mut cursor = Cursor::new(&wire_data);

    for expected_ndx in 0..3 {
        let (ndx, attrs) = SenderAttrs::read_with_codec(&mut cursor, &mut receiver_codec).unwrap();
        assert_eq!(ndx, expected_ndx, "expected NDX {expected_ndx}");
        assert_eq!(attrs.iflags, 0x8000);
    }
}

#[test]
fn sender_attrs_read_with_codec_legacy_protocol_29() {
    use protocol::codec::{NdxCodec, create_ndx_codec};

    // Protocol 29 uses 4-byte LE NDX
    let mut sender_codec = create_ndx_codec(29);
    let mut wire_data = Vec::new();
    sender_codec.write_ndx(&mut wire_data, 42).unwrap();
    // Add iflags
    wire_data.extend_from_slice(&0x8000u16.to_le_bytes());

    let mut receiver_codec = create_ndx_codec(29);
    let mut cursor = Cursor::new(&wire_data);
    let (ndx, attrs) = SenderAttrs::read_with_codec(&mut cursor, &mut receiver_codec).unwrap();

    assert_eq!(ndx, 42);
    assert_eq!(attrs.iflags, 0x8000);
}

#[test]
fn sender_attrs_read_with_codec_protocol_28_no_iflags() {
    use protocol::codec::{NdxCodec, create_ndx_codec};

    // Protocol 28: 4-byte LE NDX, no iflags
    let mut sender_codec = create_ndx_codec(28);
    let mut wire_data = Vec::new();
    sender_codec.write_ndx(&mut wire_data, 5).unwrap();
    // No iflags for protocol < 29

    let mut receiver_codec = create_ndx_codec(28);
    let mut cursor = Cursor::new(&wire_data);
    let (ndx, attrs) = SenderAttrs::read_with_codec(&mut cursor, &mut receiver_codec).unwrap();

    assert_eq!(ndx, 5);
    // Default iflags for protocol < 29
    assert_eq!(attrs.iflags, SenderAttrs::ITEM_TRANSFER);
}

#[test]
fn sender_attrs_read_with_codec_large_index() {
    use protocol::codec::{NdxCodec, create_ndx_codec};

    // Test with a large index that requires extended encoding in protocol 30+
    let large_index = 50000;

    let mut sender_codec = create_ndx_codec(31);
    let mut wire_data = Vec::new();
    sender_codec.write_ndx(&mut wire_data, large_index).unwrap();
    wire_data.extend_from_slice(&0x8000u16.to_le_bytes());

    let mut receiver_codec = create_ndx_codec(31);
    let mut cursor = Cursor::new(&wire_data);
    let (ndx, attrs) = SenderAttrs::read_with_codec(&mut cursor, &mut receiver_codec).unwrap();

    assert_eq!(ndx, large_index);
    assert_eq!(attrs.iflags, 0x8000);
}
