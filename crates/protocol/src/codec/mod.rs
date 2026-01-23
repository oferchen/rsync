//! Protocol version-aware encoding/decoding using the Strategy pattern.
//!
//! This module provides codec implementations that encapsulate wire format
//! differences between protocol versions:
//!
//! - **Protocol < 30 (Legacy)**: Fixed-size integers, longint encoding
//! - **Protocol >= 30 (Modern)**: Variable-length integers, varlong encoding
//!
//! # Module Structure
//!
//! - `ProtocolCodec` trait for general wire encoding (file sizes, mtimes, etc.)
//! - `NdxCodec` trait for file-list index (NDX) encoding
//!
//! # Strategy Pattern
//!
//! Both submodules use the Strategy pattern with legacy/modern implementations:
//!
//! | Trait | Legacy (< 30) | Modern (>= 30) |
//! |-------|---------------|----------------|
//! | `ProtocolCodec` | `LegacyProtocolCodec` | `ModernProtocolCodec` |
//! | `NdxCodec` | `LegacyNdxCodec` | `ModernNdxCodec` |
//!
//! Use factory functions to get the appropriate codec:
//! - `create_protocol_codec(version)` - creates `ProtocolCodecEnum`
//! - `create_ndx_codec(version)` - creates `NdxCodecEnum`
//!
//! # Example
//!
//! ```
//! use protocol::codec::{create_protocol_codec, create_ndx_codec, ProtocolCodec, NdxCodec};
//!
//! // Create codecs for protocol 32
//! let wire_codec = create_protocol_codec(32);
//! let mut ndx_codec = create_ndx_codec(32);
//!
//! // Use wire codec for file sizes
//! let mut buf = Vec::new();
//! wire_codec.write_file_size(&mut buf, 1000).unwrap();
//!
//! // Use NDX codec for file indices
//! ndx_codec.write_ndx(&mut buf, 0).unwrap();
//! ```

mod ndx;
mod protocol;

/// Protocol codec types for version-aware encoding.
pub use protocol::{
    LegacyProtocolCodec, ModernProtocolCodec, ProtocolCodec, ProtocolCodecEnum,
    create_protocol_codec,
};

/// NDX (file index) codec types for file list operations.
pub use ndx::{
    LegacyNdxCodec, ModernNdxCodec, NDX_DEL_STATS, NDX_DONE, NDX_FLIST_EOF, NDX_FLIST_OFFSET,
    NdxCodec, NdxCodecEnum, NdxState, create_ndx_codec, write_ndx_done, write_ndx_flist_eof,
};

// ============================================================================
// Unified ProtocolCodecs container
// ============================================================================

/// Unified container for all protocol version-aware codecs.
///
/// This struct provides a single point of access to both wire encoding
/// (`ProtocolCodec`) and NDX encoding (`NdxCodec`) for a given protocol version.
///
/// # Motivation
///
/// Previously, code needed to call two factory functions to get both codecs:
/// ```
/// # use protocol::codec::{create_protocol_codec, create_ndx_codec};
/// let version = 32;
/// let wire_codec = create_protocol_codec(version);
/// let ndx_codec = create_ndx_codec(version);
/// ```
///
/// With `ProtocolCodecs`, you create both at once:
/// ```
/// # use protocol::codec::ProtocolCodecs;
/// let version = 32;
/// let codecs = ProtocolCodecs::for_version(version);
/// // Use codecs.wire for file sizes, mtimes, etc.
/// // Use codecs.ndx for file-list indices
/// ```
///
/// # Example
///
/// ```
/// use protocol::codec::{ProtocolCodecs, ProtocolCodec, NdxCodec};
///
/// let mut codecs = ProtocolCodecs::for_version(32);
///
/// // Use wire codec for file sizes
/// let mut buf = Vec::new();
/// codecs.wire.write_file_size(&mut buf, 1000).unwrap();
///
/// // Use NDX codec for file indices
/// codecs.ndx.write_ndx(&mut buf, 0).unwrap();
/// ```
#[derive(Debug)]
pub struct ProtocolCodecs {
    /// Wire-format codec for file sizes, mtimes, and other protocol data.
    pub wire: ProtocolCodecEnum,
    /// NDX codec for file-list index encoding.
    pub ndx: NdxCodecEnum,
}

impl ProtocolCodecs {
    /// Creates a new `ProtocolCodecs` for the given protocol version.
    ///
    /// Both the wire codec and NDX codec are configured for the same version:
    /// - Protocol < 30: Legacy codecs (fixed-size integers)
    /// - Protocol >= 30: Modern codecs (variable-length encoding)
    ///
    /// # Example
    ///
    /// ```
    /// use protocol::codec::ProtocolCodecs;
    ///
    /// // Protocol 29: both codecs use legacy format
    /// let codecs = ProtocolCodecs::for_version(29);
    /// assert!(codecs.is_legacy());
    ///
    /// // Protocol 32: both codecs use modern format
    /// let codecs = ProtocolCodecs::for_version(32);
    /// assert!(!codecs.is_legacy());
    /// ```
    #[must_use]
    pub fn for_version(version: u8) -> Self {
        Self {
            wire: create_protocol_codec(version),
            ndx: create_ndx_codec(version),
        }
    }

    /// Returns the protocol version these codecs are configured for.
    #[must_use]
    #[inline]
    pub fn protocol_version(&self) -> u8 {
        self.wire.protocol_version()
    }

    /// Returns `true` if these are legacy codecs (protocol < 30).
    #[must_use]
    #[inline]
    pub fn is_legacy(&self) -> bool {
        self.wire.is_legacy()
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protocol_codecs_for_version_28() {
        let codecs = ProtocolCodecs::for_version(28);
        assert_eq!(codecs.protocol_version(), 28);
        assert!(codecs.is_legacy());
        assert!(matches!(codecs.wire, ProtocolCodecEnum::Legacy(_)));
        assert!(matches!(codecs.ndx, NdxCodecEnum::Legacy(_)));
    }

    #[test]
    fn protocol_codecs_for_version_29() {
        let codecs = ProtocolCodecs::for_version(29);
        assert_eq!(codecs.protocol_version(), 29);
        assert!(codecs.is_legacy());
        assert!(matches!(codecs.wire, ProtocolCodecEnum::Legacy(_)));
        assert!(matches!(codecs.ndx, NdxCodecEnum::Legacy(_)));
    }

    #[test]
    fn protocol_codecs_for_version_30() {
        let codecs = ProtocolCodecs::for_version(30);
        assert_eq!(codecs.protocol_version(), 30);
        assert!(!codecs.is_legacy());
        assert!(matches!(codecs.wire, ProtocolCodecEnum::Modern(_)));
        assert!(matches!(codecs.ndx, NdxCodecEnum::Modern(_)));
    }

    #[test]
    fn protocol_codecs_for_version_32() {
        let codecs = ProtocolCodecs::for_version(32);
        assert_eq!(codecs.protocol_version(), 32);
        assert!(!codecs.is_legacy());
        assert!(matches!(codecs.wire, ProtocolCodecEnum::Modern(_)));
        assert!(matches!(codecs.ndx, NdxCodecEnum::Modern(_)));
    }

    #[test]
    fn protocol_codecs_wire_and_ndx_version_match() {
        for version in [28, 29, 30, 31, 32] {
            let codecs = ProtocolCodecs::for_version(version);
            assert_eq!(codecs.wire.protocol_version(), version);
            assert_eq!(codecs.ndx.protocol_version(), version);
        }
    }

    #[test]
    fn protocol_codecs_boundary_at_30() {
        // Protocol 29: legacy
        let codecs_29 = ProtocolCodecs::for_version(29);
        assert!(codecs_29.is_legacy());
        assert!(matches!(codecs_29.wire, ProtocolCodecEnum::Legacy(_)));
        assert!(matches!(codecs_29.ndx, NdxCodecEnum::Legacy(_)));

        // Protocol 30: modern
        let codecs_30 = ProtocolCodecs::for_version(30);
        assert!(!codecs_30.is_legacy());
        assert!(matches!(codecs_30.wire, ProtocolCodecEnum::Modern(_)));
        assert!(matches!(codecs_30.ndx, NdxCodecEnum::Modern(_)));
    }

    #[test]
    fn protocol_codecs_wire_roundtrip() {
        use std::io::Cursor;

        let codecs = ProtocolCodecs::for_version(32);
        let mut buf = Vec::new();

        // Write file size
        codecs.wire.write_file_size(&mut buf, 12345).unwrap();

        // Read it back
        let mut cursor = Cursor::new(&buf);
        let value = codecs.wire.read_file_size(&mut cursor).unwrap();
        assert_eq!(value, 12345);
    }

    #[test]
    fn protocol_codecs_ndx_roundtrip() {
        use std::io::Cursor;

        let mut codecs = ProtocolCodecs::for_version(32);
        let mut buf = Vec::new();

        // Write NDX values
        codecs.ndx.write_ndx(&mut buf, 0).unwrap();
        codecs.ndx.write_ndx(&mut buf, 1).unwrap();
        codecs.ndx.write_ndx(&mut buf, 5).unwrap();

        // Read them back with fresh codec state
        let mut read_codecs = ProtocolCodecs::for_version(32);
        let mut cursor = Cursor::new(&buf);
        assert_eq!(read_codecs.ndx.read_ndx(&mut cursor).unwrap(), 0);
        assert_eq!(read_codecs.ndx.read_ndx(&mut cursor).unwrap(), 1);
        assert_eq!(read_codecs.ndx.read_ndx(&mut cursor).unwrap(), 5);
    }

    #[test]
    fn protocol_codecs_combined_operations() {
        use std::io::Cursor;

        let mut codecs = ProtocolCodecs::for_version(30);
        let mut buf = Vec::new();

        // Mix wire and NDX operations
        codecs.wire.write_file_size(&mut buf, 1000).unwrap();
        codecs.ndx.write_ndx(&mut buf, 0).unwrap();
        codecs.wire.write_mtime(&mut buf, 1700000000).unwrap();
        codecs.ndx.write_ndx(&mut buf, 1).unwrap();

        // Read them back
        let mut read_codecs = ProtocolCodecs::for_version(30);
        let mut cursor = Cursor::new(&buf);

        assert_eq!(read_codecs.wire.read_file_size(&mut cursor).unwrap(), 1000);
        assert_eq!(read_codecs.ndx.read_ndx(&mut cursor).unwrap(), 0);
        assert_eq!(
            read_codecs.wire.read_mtime(&mut cursor).unwrap(),
            1700000000
        );
        assert_eq!(read_codecs.ndx.read_ndx(&mut cursor).unwrap(), 1);
    }

    // ========================================================================
    // Protocol Version Feature Tests
    // ========================================================================

    #[test]
    fn protocol_version_feature_gates_sender_receiver_modifiers() {
        // Protocol 28: does NOT support sender/receiver modifiers
        let codecs_28 = ProtocolCodecs::for_version(28);
        assert!(!codecs_28.wire.supports_sender_receiver_modifiers());

        // Protocol 29: DOES support sender/receiver modifiers
        let codecs_29 = ProtocolCodecs::for_version(29);
        assert!(codecs_29.wire.supports_sender_receiver_modifiers());

        // Protocol 30+: continues to support
        let codecs_30 = ProtocolCodecs::for_version(30);
        assert!(codecs_30.wire.supports_sender_receiver_modifiers());
    }

    #[test]
    fn protocol_version_feature_gates_flist_times() {
        // Protocol 28: does NOT support flist times
        let codecs_28 = ProtocolCodecs::for_version(28);
        assert!(!codecs_28.wire.supports_flist_times());

        // Protocol 29+: DOES support flist times
        let codecs_29 = ProtocolCodecs::for_version(29);
        assert!(codecs_29.wire.supports_flist_times());

        let codecs_30 = ProtocolCodecs::for_version(30);
        assert!(codecs_30.wire.supports_flist_times());
    }

    #[test]
    fn protocol_version_feature_gates_perishable_modifier() {
        // Protocol 28-29: do NOT support perishable modifier
        let codecs_28 = ProtocolCodecs::for_version(28);
        assert!(!codecs_28.wire.supports_perishable_modifier());

        let codecs_29 = ProtocolCodecs::for_version(29);
        assert!(!codecs_29.wire.supports_perishable_modifier());

        // Protocol 30+: DOES support perishable modifier
        let codecs_30 = ProtocolCodecs::for_version(30);
        assert!(codecs_30.wire.supports_perishable_modifier());

        let codecs_32 = ProtocolCodecs::for_version(32);
        assert!(codecs_32.wire.supports_perishable_modifier());
    }

    #[test]
    fn protocol_version_feature_gates_old_prefixes() {
        // Protocol 28: uses old filter prefixes
        let codecs_28 = ProtocolCodecs::for_version(28);
        assert!(codecs_28.wire.uses_old_prefixes());

        // Protocol 29+: uses new filter prefixes
        let codecs_29 = ProtocolCodecs::for_version(29);
        assert!(!codecs_29.wire.uses_old_prefixes());

        let codecs_30 = ProtocolCodecs::for_version(30);
        assert!(!codecs_30.wire.uses_old_prefixes());
    }

    #[test]
    fn all_supported_versions_create_valid_codecs() {
        // Test all supported protocol versions (28-32)
        for version in 28..=32 {
            let codecs = ProtocolCodecs::for_version(version);
            assert_eq!(codecs.protocol_version(), version);

            // Wire and NDX should agree on legacy/modern
            let is_legacy = version < 30;
            assert_eq!(codecs.is_legacy(), is_legacy);

            // Enum variants should match expected type
            if is_legacy {
                assert!(matches!(codecs.wire, ProtocolCodecEnum::Legacy(_)));
                assert!(matches!(codecs.ndx, NdxCodecEnum::Legacy(_)));
            } else {
                assert!(matches!(codecs.wire, ProtocolCodecEnum::Modern(_)));
                assert!(matches!(codecs.ndx, NdxCodecEnum::Modern(_)));
            }
        }
    }

    #[test]
    fn encoding_size_differs_between_legacy_and_modern() {
        use std::io::Cursor;

        // Legacy protocol 29: fixed 4-byte integers
        let mut codecs_29 = ProtocolCodecs::for_version(29);
        let mut buf_29 = Vec::new();
        codecs_29.ndx.write_ndx(&mut buf_29, 0).unwrap();
        assert_eq!(buf_29.len(), 4, "legacy NDX should be 4 bytes");

        // Modern protocol 30: delta-encoded variable-length
        let mut codecs_30 = ProtocolCodecs::for_version(30);
        let mut buf_30 = Vec::new();
        codecs_30.ndx.write_ndx(&mut buf_30, 0).unwrap();
        assert_eq!(buf_30.len(), 1, "modern NDX should be 1 byte for first index");

        // Both should decode to the same value
        let mut cursor_29 = Cursor::new(&buf_29);
        let mut read_29 = ProtocolCodecs::for_version(29);
        assert_eq!(read_29.ndx.read_ndx(&mut cursor_29).unwrap(), 0);

        let mut cursor_30 = Cursor::new(&buf_30);
        let mut read_30 = ProtocolCodecs::for_version(30);
        assert_eq!(read_30.ndx.read_ndx(&mut cursor_30).unwrap(), 0);
    }
}
