use std::num::NonZeroU8;

use super::metadata;

/// Highest protocol version supported by the workspace.
pub const PROTOCOL_VERSION: u32 = parse_u32(env!("OC_RSYNC_WORKSPACE_PROTOCOL"));

/// Returns the configured protocol version as an 8-bit integer.
///
/// The workspace manifest records the highest supported protocol as a decimal
/// integer. Upstream rsync encodes negotiated protocol numbers in a single
/// byte, so the manifest value must remain within the `u8` range. The helper
/// performs the bounds check at compile time and therefore causes compilation
/// to fail immediately if the manifest is updated inconsistently. Callers that
/// render diagnostics or capability banners can rely on this accessor without
/// repeating the conversion logic.
///
/// # Examples
///
/// ```
/// use oc_rsync_core::workspace;
///
/// assert_eq!(
///     workspace::protocol_version_u8() as u32,
///     workspace::metadata().protocol_version()
/// );
/// ```
#[must_use]
pub const fn protocol_version_u8() -> u8 {
    let value = metadata().protocol_version();
    if value > u8::MAX as u32 {
        panic!("workspace protocol version must fit within a u8");
    }
    value as u8
}

/// Returns the configured protocol version as a [`NonZeroU8`].
///
/// Upstream rsync has never advertised protocol version `0`. Encoding the value
/// as [`NonZeroU8`] allows call sites to rely on this invariant without
/// repeating ad-hoc checks. The helper reuses [`protocol_version_u8`] to
/// preserve the compile-time bounds validation against the manifest metadata.
///
/// # Examples
///
/// ```
/// use oc_rsync_core::workspace;
///
/// assert_eq!(workspace::protocol_version_nonzero_u8().get(), 32);
/// ```
#[must_use]
pub const fn protocol_version_nonzero_u8() -> NonZeroU8 {
    match NonZeroU8::new(protocol_version_u8()) {
        Some(value) => value,
        None => panic!("workspace protocol version must be non-zero"),
    }
}

const fn parse_u32(input: &str) -> u32 {
    let bytes = input.as_bytes();
    let mut value = 0u32;
    let mut index = 0;
    if bytes.is_empty() {
        panic!("protocol must not be empty");
    }
    while index < bytes.len() {
        let digit = bytes[index];
        if !digit.is_ascii_digit() {
            panic!("protocol must be an ASCII integer");
        }
        value = value * 10 + (digit - b'0') as u32;
        index += 1;
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::panic;

    #[test]
    fn parse_u32_accepts_decimal_digits() {
        assert_eq!(parse_u32("0"), 0);
        assert_eq!(parse_u32("32"), 32);
        assert_eq!(parse_u32("0010"), 10);
    }

    #[test]
    fn parse_u32_rejects_empty_strings() {
        let result = panic::catch_unwind(|| parse_u32(""));
        assert!(result.is_err(), "empty input must trigger a panic");
    }

    #[test]
    fn parse_u32_rejects_non_ascii_digits() {
        let result = panic::catch_unwind(|| parse_u32("3a"));
        assert!(result.is_err(), "non-digit input must trigger a panic");
    }

    #[test]
    fn protocol_accessors_match_metadata_protocol() {
        let snapshot = metadata();
        assert_eq!(protocol_version_u8() as u32, snapshot.protocol_version());
        assert_eq!(
            protocol_version_nonzero_u8().get() as u32,
            snapshot.protocol_version()
        );
    }
}
