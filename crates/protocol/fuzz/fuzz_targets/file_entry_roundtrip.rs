#![no_main]

//! Fuzz target for file entry wire format encode/decode roundtrip.
//!
//! Tests that file entry field encoding followed by decoding produces
//! the original values. Uses structured input via `arbitrary` to generate
//! valid parameter combinations, catching encode/decode mismatches and
//! panics on edge-case inputs.

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use std::io::Cursor;

/// Structured input for file entry field roundtrip testing.
#[derive(Arbitrary, Debug)]
struct FileEntryInput {
    /// File size to encode/decode.
    size: u64,
    /// Modification time to encode/decode.
    mtime: i64,
    /// Unix mode bits.
    mode: u32,
    /// User ID.
    uid: u32,
    /// Group ID.
    gid: u32,
    /// Protocol version selector (mapped to 28-32 range).
    proto_selector: u8,
    /// Whether to use varint flags encoding.
    use_varint_flags: bool,
    /// Whether the entry is a directory.
    is_dir: bool,
    /// XMIT flags value for flag encode/decode.
    xflags: u16,
    /// File name bytes (short, for prefix compression testing).
    name: Vec<u8>,
    /// Previous name bytes (for prefix compression).
    prev_name: Vec<u8>,
    /// Symlink target bytes.
    symlink_target: Vec<u8>,
    /// Access time.
    atime: i64,
    /// Creation time.
    crtime: i64,
    /// Checksum bytes (up to 16).
    checksum: [u8; 16],
    /// Checksum length (mapped to valid range).
    csum_len_selector: u8,
}

impl FileEntryInput {
    /// Maps the proto_selector to a valid protocol version (28-32).
    fn protocol_version(&self) -> u8 {
        28 + (self.proto_selector % 5)
    }

    /// Maps csum_len_selector to a valid checksum length (1-16).
    fn csum_len(&self) -> usize {
        1 + (self.csum_len_selector % 16) as usize
    }
}

fuzz_target!(|input: FileEntryInput| {
    let proto = input.protocol_version();

    // Roundtrip: flags encode/decode
    {
        let mut buf = Vec::new();
        let xflags = input.xflags as u32;
        if protocol::wire::file_entry::encode_flags(
            &mut buf,
            xflags,
            proto,
            input.use_varint_flags,
            input.is_dir,
        )
        .is_ok()
        {
            let mut cursor = Cursor::new(&buf);
            let _ = protocol::wire::file_entry_decode::decode_flags(
                &mut cursor,
                proto,
                input.use_varint_flags,
            );
        }
    }

    // Roundtrip: size encode/decode
    {
        // Clamp size to non-negative i64 range for wire format compatibility
        let size = input.size & 0x7FFF_FFFF_FFFF_FFFF;
        let mut buf = Vec::new();
        if protocol::wire::file_entry::encode_size(&mut buf, size, proto).is_ok() {
            let mut cursor = Cursor::new(&buf);
            if let Ok(decoded) = protocol::wire::file_entry_decode::decode_size(&mut cursor, proto)
            {
                assert_eq!(decoded as u64, size, "size roundtrip mismatch");
            }
        }
    }

    // Roundtrip: mtime encode/decode
    // Encode writes unconditionally; decode only reads when XMIT_SAME_TIME is NOT set.
    // Use flags=0 so decode reads from wire.
    {
        let mut buf = Vec::new();
        if protocol::wire::file_entry::encode_mtime(&mut buf, input.mtime, proto).is_ok() {
            let mut cursor = Cursor::new(&buf);
            if let Ok(Some(decoded)) =
                protocol::wire::file_entry_decode::decode_mtime(&mut cursor, 0, 0, proto)
            {
                if proto >= 30 {
                    assert_eq!(decoded, input.mtime, "mtime roundtrip mismatch (proto 30+)");
                } else {
                    // Proto < 30 truncates to i32
                    assert_eq!(
                        decoded, input.mtime as i32 as i64,
                        "mtime roundtrip mismatch (proto < 30)"
                    );
                }
            }
        }
    }

    // Roundtrip: mode encode/decode
    // Encode writes unconditionally; decode reads when XMIT_SAME_MODE is NOT set (flags=0).
    {
        let mut buf = Vec::new();
        if protocol::wire::file_entry::encode_mode(&mut buf, input.mode).is_ok() {
            let mut cursor = Cursor::new(&buf);
            if let Ok(Some(decoded)) =
                protocol::wire::file_entry_decode::decode_mode(&mut cursor, 0, 0)
            {
                assert_eq!(decoded, input.mode, "mode roundtrip mismatch");
            }
        }
    }

    // Roundtrip: uid encode/decode
    // Use flags=0 (no XMIT_SAME_UID) so decode reads from wire.
    {
        let mut buf = Vec::new();
        if protocol::wire::file_entry::encode_uid(&mut buf, input.uid, proto).is_ok() {
            let mut cursor = Cursor::new(&buf);
            if let Ok(Some((decoded_uid, _name))) =
                protocol::wire::file_entry_decode::decode_uid(&mut cursor, 0, 0, proto)
            {
                assert_eq!(decoded_uid, input.uid, "uid roundtrip mismatch");
            }
        }
    }

    // Roundtrip: gid encode/decode
    // Use flags=0 (no XMIT_SAME_GID) so decode reads from wire.
    {
        let mut buf = Vec::new();
        if protocol::wire::file_entry::encode_gid(&mut buf, input.gid, proto).is_ok() {
            let mut cursor = Cursor::new(&buf);
            if let Ok(Some((decoded_gid, _name))) =
                protocol::wire::file_entry_decode::decode_gid(&mut cursor, 0, 0, proto)
            {
                assert_eq!(decoded_gid, input.gid, "gid roundtrip mismatch");
            }
        }
    }

    // Roundtrip: name encode/decode with prefix compression
    if !input.name.is_empty() {
        let name = &input.name[..input.name.len().min(255)];
        let prev_name = &input.prev_name[..input.prev_name.len().min(255)];

        // Calculate prefix length
        let same_len = name
            .iter()
            .zip(prev_name.iter())
            .take_while(|(a, b)| a == b)
            .count()
            .min(255);

        let mut xflags = 0u32;
        if same_len > 0 {
            xflags |= protocol::wire::file_entry::XMIT_SAME_NAME as u32;
        }

        let mut buf = Vec::new();
        if protocol::wire::file_entry::encode_name(&mut buf, name, same_len, xflags, proto).is_ok()
        {
            let mut cursor = Cursor::new(&buf);
            if let Ok(decoded) = protocol::wire::file_entry_decode::decode_name(
                &mut cursor,
                xflags,
                prev_name,
                proto,
            ) {
                assert_eq!(decoded, name, "name roundtrip mismatch");
            }
        }
    }

    // Roundtrip: symlink target encode/decode
    if !input.symlink_target.is_empty() {
        let target = &input.symlink_target[..input.symlink_target.len().min(4096)];
        let mut buf = Vec::new();
        if protocol::wire::file_entry::encode_symlink_target(&mut buf, target, proto).is_ok() {
            let mut cursor = Cursor::new(&buf);
            if let Ok(decoded) =
                protocol::wire::file_entry_decode::decode_symlink_target(&mut cursor, proto)
            {
                assert_eq!(decoded, target, "symlink target roundtrip mismatch");
            }
        }
    }

    // Roundtrip: atime encode/decode
    // Use flags=0 (no XMIT_SAME_ATIME) so decode reads from wire.
    {
        let mut buf = Vec::new();
        if protocol::wire::file_entry::encode_atime(&mut buf, input.atime).is_ok() {
            let mut cursor = Cursor::new(&buf);
            if let Ok(Some(decoded)) =
                protocol::wire::file_entry_decode::decode_atime(&mut cursor, 0, 0)
            {
                assert_eq!(decoded, input.atime, "atime roundtrip mismatch");
            }
        }
    }

    // Roundtrip: crtime encode/decode
    // Use flags=0 (no XMIT_CRTIME_EQ_MTIME) so decode reads from wire.
    {
        let mut buf = Vec::new();
        if protocol::wire::file_entry::encode_crtime(&mut buf, input.crtime).is_ok() {
            let mut cursor = Cursor::new(&buf);
            if let Ok(Some(decoded)) =
                protocol::wire::file_entry_decode::decode_crtime(&mut cursor, 0, 0)
            {
                assert_eq!(decoded, input.crtime, "crtime roundtrip mismatch");
            }
        }
    }

    // Roundtrip: checksum encode/decode
    {
        let csum_len = input.csum_len();
        let sum = &input.checksum[..csum_len.min(16)];
        let mut buf = Vec::new();
        if protocol::wire::file_entry::encode_checksum(&mut buf, Some(sum), csum_len).is_ok() {
            let mut cursor = Cursor::new(&buf);
            if let Ok(decoded) =
                protocol::wire::file_entry_decode::decode_checksum(&mut cursor, csum_len)
            {
                assert_eq!(
                    &decoded[..],
                    &sum[..csum_len.min(sum.len())],
                    "checksum roundtrip mismatch"
                );
            }
        }
    }

    // Roundtrip: end marker encode/decode
    {
        let mut buf = Vec::new();
        if protocol::wire::file_entry::encode_end_marker(
            &mut buf,
            input.use_varint_flags,
            false,
            None,
        )
        .is_ok()
        {
            let mut cursor = Cursor::new(&buf);
            let _ = protocol::wire::file_entry_decode::decode_flags(
                &mut cursor,
                proto,
                input.use_varint_flags,
            );
        }
    }
});
