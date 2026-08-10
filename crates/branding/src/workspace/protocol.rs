use std::num::NonZeroU8;

use super::metadata;

/// Highest protocol version supported by the workspace.
pub const PROTOCOL_VERSION: u32 = crate::generated::PROTOCOL_VERSION;

/// Returns the configured protocol version as an 8-bit integer.
///
/// Upstream rsync encodes negotiated protocol numbers in a single byte, so the
/// manifest value must remain within the `u8` range. The bounds check runs at
/// compile time, failing the build immediately if the manifest is updated
/// inconsistently.
///
/// # Examples
///
/// ```
/// use branding::workspace;
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
/// Upstream rsync has never advertised protocol version `0`. Encoding the
/// value as [`NonZeroU8`] lets call sites rely on this invariant without
/// repeating ad-hoc checks.
///
/// # Examples
///
/// ```
/// use branding::workspace;
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

#[cfg(test)]
mod tests {
    use super::*;

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
