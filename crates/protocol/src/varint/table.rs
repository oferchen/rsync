/// Additional byte count lookup used by rsync's variable-length integer codec.
///
/// The table mirrors `int_byte_extra` from upstream `io.c`. Each entry
/// specifies how many extra bytes follow the leading tag for a particular
/// high-bit pattern.
pub(super) const INT_BYTE_EXTRA: [u8; 64] = [
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, // (0x00-0x3F) / 4
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, // (0x40-0x7F) / 4
    1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, // (0x80-0xBF) / 4
    2, 2, 2, 2, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 5, 6, // (0xC0-0xFF) / 4
];

/// Maximum number of additional bytes read after the leading tag.
pub(super) const MAX_EXTRA_BYTES: usize = 4;

pub(super) fn invalid_data(message: &str) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, message)
}
