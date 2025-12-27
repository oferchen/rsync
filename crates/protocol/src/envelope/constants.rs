/// Number of bytes in a multiplexed rsync message header.
pub const HEADER_LEN: usize = 4;

/// Maximum payload length representable in a multiplexed header.
pub const MAX_PAYLOAD_LENGTH: u32 = 0x00FF_FFFF;

/// Base offset added to multiplexed message codes when encoding headers.
///
/// Upstream rsync defines `MPLEX_BASE` as the separation point between raw data
/// and control messages flowing over the multiplexed stream. Tags transmitted on
/// the wire add this offset to the numeric [`super::MessageCode`] value so the high
/// header byte can be inspected quickly. Exposing the constant keeps the Rust
/// implementation in sync with the C reference and avoids duplicating the magic
/// value across crates that need to reason about multiplexed tags.
pub const MPLEX_BASE: u8 = 7;

/// Bitmask used to clamp payload lengths to the 24-bit range representable in
/// multiplexed headers.
pub(crate) const PAYLOAD_MASK: u32 = 0x00FF_FFFF;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_len_is_4() {
        assert_eq!(HEADER_LEN, 4);
    }

    #[test]
    fn max_payload_length_is_24_bits() {
        assert_eq!(MAX_PAYLOAD_LENGTH, 0x00FF_FFFF);
        assert_eq!(MAX_PAYLOAD_LENGTH, 16_777_215);
    }

    #[test]
    fn mplex_base_is_7() {
        assert_eq!(MPLEX_BASE, 7);
    }

    #[test]
    fn payload_mask_equals_max_payload_length() {
        assert_eq!(PAYLOAD_MASK, MAX_PAYLOAD_LENGTH);
    }

    #[test]
    fn max_payload_fits_in_24_bits() {
        assert!(MAX_PAYLOAD_LENGTH < (1 << 24));
        assert_eq!(MAX_PAYLOAD_LENGTH, (1 << 24) - 1);
    }
}
