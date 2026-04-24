//! Protocol version feature and capability queries.
//!
//! Each method provides a semantic name for a protocol version threshold,
//! eliminating scattered magic-number comparisons throughout the codebase.

use super::ProtocolVersion;

impl ProtocolVersion {
    /// Reports whether sessions negotiated at this protocol version use the
    /// binary framing introduced in protocol 30.
    #[must_use]
    pub const fn uses_binary_negotiation(self) -> bool {
        self.as_u8() >= Self::BINARY_NEGOTIATION_INTRODUCED.as_u8()
    }

    /// Reports whether this protocol version still relies on the legacy ASCII
    /// daemon negotiation.
    #[must_use]
    pub const fn uses_legacy_ascii_negotiation(self) -> bool {
        self.as_u8() < Self::BINARY_NEGOTIATION_INTRODUCED.as_u8()
    }

    /// Returns `true` if this protocol version uses variable-length integer
    /// encoding.
    ///
    /// - Protocol < 30: fixed-size integers (4-byte, longint)
    /// - Protocol >= 30: varint/varlong encoding
    ///
    /// This is the primary encoding boundary in the rsync protocol.
    #[must_use]
    pub const fn uses_varint_encoding(self) -> bool {
        self.as_u8() >= 30
    }

    /// Returns `true` if this protocol version uses legacy fixed-size encoding.
    ///
    /// Inverse of [`uses_varint_encoding`](Self::uses_varint_encoding).
    #[must_use]
    pub const fn uses_fixed_encoding(self) -> bool {
        self.as_u8() < 30
    }

    /// Returns `true` if this protocol version supports sender/receiver side
    /// modifiers (`s`, `r`).
    ///
    /// # Upstream Reference
    ///
    /// `exclude.c:1567-1571` - sender/receiver modifier support gated by protocol >= 29
    #[must_use]
    pub const fn supports_sender_receiver_modifiers(self) -> bool {
        self.as_u8() >= 29
    }

    /// Returns `true` if this protocol version supports the perishable
    /// modifier (`p`).
    ///
    /// # Upstream Reference
    ///
    /// `exclude.c:1350` - `protocol_version >= 30 ? FILTRULE_PERISHABLE : 0`
    #[must_use]
    pub const fn supports_perishable_modifier(self) -> bool {
        self.as_u8() >= 30
    }

    /// Returns `true` if this protocol version uses old-style prefixes
    /// (protocol < 29).
    ///
    /// Old prefixes have restricted modifier support and different parsing
    /// rules.
    ///
    /// # Upstream Reference
    ///
    /// `exclude.c:1675` - `xflags = protocol_version >= 29 ? 0 : XFLG_OLD_PREFIXES`
    #[must_use]
    pub const fn uses_old_prefixes(self) -> bool {
        self.as_u8() < 29
    }

    /// Returns `true` if this protocol version supports file list timing
    /// statistics.
    ///
    /// # Upstream Reference
    ///
    /// `main.c` - `handle_stats()` sends flist times only for protocol >= 29
    #[must_use]
    pub const fn supports_flist_times(self) -> bool {
        self.as_u8() >= 29
    }

    /// Returns `true` if this protocol version sends iflags after NDX.
    ///
    /// - Protocol < 29: iflags default to `ITEM_TRANSFER`
    /// - Protocol >= 29: 2-byte iflags follow each NDX on the wire
    ///
    /// # Upstream Reference
    ///
    /// `sender.c:180-187` - `write_ndx_and_attrs()` sends iflags for protocol >= 29
    #[must_use]
    pub const fn supports_iflags(self) -> bool {
        self.as_u8() >= 29
    }

    /// Returns `true` if this protocol version supports multi-phase transfers.
    ///
    /// - Protocol < 29: single phase (`max_phase = 1`)
    /// - Protocol >= 29: two phases (`max_phase = 2`)
    ///
    /// # Upstream Reference
    ///
    /// `generator.c` / `receiver.c` - `max_phase = protocol >= 29 ? 2 : 1`
    #[must_use]
    pub const fn supports_multi_phase(self) -> bool {
        self.as_u8() >= 29
    }

    /// Returns `true` if this protocol version supports extended file flags.
    ///
    /// Extended flags allow for more file attributes to be transmitted.
    #[must_use]
    pub const fn supports_extended_flags(self) -> bool {
        self.as_u8() >= 28
    }

    /// Returns `true` if this protocol version uses varint-encoded file list
    /// flags.
    ///
    /// - Protocol < 30: 1-2 byte fixed flags
    /// - Protocol >= 30: varint-encoded flags with `COMPAT_VARINT_FLIST_FLAGS`
    #[must_use]
    pub const fn uses_varint_flist_flags(self) -> bool {
        self.as_u8() >= 30
    }

    /// Returns `true` if this protocol version supports safe file list mode.
    ///
    /// - Protocol < 30: not available
    /// - Protocol >= 30: `COMPAT_SAFE_FLIST` may be negotiated
    ///
    /// See [`safe_file_list_always_enabled`](Self::safe_file_list_always_enabled)
    /// to check if safe file list is mandatory (protocol >= 31).
    #[must_use]
    pub const fn uses_safe_file_list(self) -> bool {
        self.as_u8() >= 30
    }

    /// Returns `true` if safe file list mode is always enabled (protocol >= 31).
    ///
    /// Protocol 31+ unconditionally uses safe file list mode regardless of
    /// compatibility flags.
    #[must_use]
    pub const fn safe_file_list_always_enabled(self) -> bool {
        self.as_u8() >= 31
    }

    /// Returns `true` if this protocol version supports basic I/O
    /// multiplexing.
    ///
    /// # Upstream Reference
    ///
    /// `main.c:1304-1305` - client activates input multiplex for protocol >= 23;
    /// server activates output multiplex for protocol >= 23.
    #[must_use]
    pub const fn supports_multiplex_io(self) -> bool {
        self.as_u8() >= 23
    }

    /// Returns `true` if this protocol version supports the goodbye
    /// (`NDX_DONE`) exchange.
    ///
    /// # Upstream Reference
    ///
    /// `main.c:880-905` - `read_final_goodbye()` skips for protocol < 24.
    #[must_use]
    pub const fn supports_goodbye_exchange(self) -> bool {
        self.as_u8() >= 24
    }

    /// Returns `true` if this protocol version supports generator-to-sender
    /// messages.
    ///
    /// When true, the generator can send messages (including `send_no_send`)
    /// via the multiplexed stream, and both client/server activate their
    /// respective multiplex channels for bidirectional communication.
    ///
    /// # Upstream Reference
    ///
    /// `io.c` - `need_messages_from_generator` is true for protocol >= 30.
    #[must_use]
    pub const fn supports_generator_messages(self) -> bool {
        self.as_u8() >= 30
    }

    /// Returns `true` if this protocol version uses the extended 3-way goodbye
    /// exchange.
    ///
    /// Also used for `MSG_IO_TIMEOUT` delivery from server to client.
    ///
    /// # Upstream Reference
    ///
    /// `main.c:880-905` - protocol >= 31 performs extra `NDX_DONE` round-trip.
    #[must_use]
    pub const fn supports_extended_goodbye(self) -> bool {
        self.as_u8() >= 31
    }

    /// Returns `true` if this protocol version supports inline hardlink
    /// encoding in the file list.
    ///
    /// Protocol >= 30 encodes hardlink device/inode pairs inline in the file
    /// list entry rather than as separate messages. This enables incremental
    /// file list building with hardlink deduplication.
    ///
    /// # Upstream Reference
    ///
    /// upstream: flist.c - `protocol_version >= 30` gates inline dev/ino fields
    #[must_use]
    pub const fn supports_inline_hardlinks(self) -> bool {
        self.as_u8() >= 30
    }

    /// Returns the preferred compression algorithm name for this protocol version.
    ///
    /// Protocol >= 31 prefers zstd (if available at compile time), falling back
    /// to zlibx. Protocol 27-30 uses zlib (the only compression supported by
    /// those versions of upstream rsync).
    ///
    /// This does not perform negotiation - it returns the local preference.
    /// Actual algorithm selection happens during capability exchange.
    ///
    /// # Upstream Reference
    ///
    /// upstream: compat.c:100-112 `valid_compressions_items[]` - zstd first
    /// when `SUPPORT_ZSTD` is defined (protocol >= 31).
    #[must_use]
    pub const fn preferred_compression(self) -> &'static str {
        if self.as_u8() >= 31 {
            // Protocol 31+ supports negotiated compression (zstd, lz4, zlibx, zlib).
            // The actual preference depends on compile-time features, but zstd is
            // the canonical recommendation for modern protocol versions.
            "zstd"
        } else {
            "zlib"
        }
    }

    /// Returns `true` if this protocol version supports checksum negotiation.
    ///
    /// Protocol >= 30 can negotiate checksum algorithms (MD5, XXH3, XXH128)
    /// via the `-e.LsfxCIvu` capability string. Earlier versions are locked
    /// to MD4 (protocol 27-29) or MD5 (protocol 30 without negotiation).
    ///
    /// # Upstream Reference
    ///
    /// upstream: compat.c:720 `set_allow_inc_recurse()` - checksum negotiation
    /// gated by protocol >= 30 and `-e` capability flag.
    #[must_use]
    pub const fn supports_checksum_negotiation(self) -> bool {
        self.as_u8() >= 30
    }

    /// Returns `true` if this protocol version supports delete statistics.
    ///
    /// Protocol >= 31 sends per-type deletion counts (files, dirs, symlinks,
    /// devices, specials) via `NDX_DEL_STATS` during the goodbye phase.
    ///
    /// # Upstream Reference
    ///
    /// upstream: main.c - `read_del_stats()` gated by protocol >= 31.
    #[must_use]
    pub const fn supports_delete_stats(self) -> bool {
        self.as_u8() >= 31
    }

    /// Returns `true` if this protocol version supports incremental file list
    /// recursion.
    ///
    /// Protocol >= 30 with `inc_recurse` compatibility flag enables streaming
    /// file list exchange where files are transferred as they are discovered
    /// rather than after the complete file list is built.
    ///
    /// # Upstream Reference
    ///
    /// upstream: compat.c:720 `set_allow_inc_recurse()` - INC_RECURSE gated
    /// by protocol >= 30.
    #[must_use]
    pub const fn supports_inc_recurse(self) -> bool {
        self.as_u8() >= 30
    }
}

/// Protocol capabilities newtype providing a focused API for version-dependent
/// feature gating.
///
/// Wraps a [`ProtocolVersion`] and exposes only capability queries, making it
/// suitable for passing into subsystems that need to know what features are
/// available without access to the full protocol version API.
///
/// # Examples
///
/// ```
/// use protocol::{ProtocolVersion, ProtocolCapabilities};
///
/// let caps = ProtocolCapabilities::from(ProtocolVersion::V32);
/// assert!(caps.multiplex());
/// assert!(caps.extended_flags());
/// assert!(caps.inline_hardlinks());
/// assert_eq!(caps.preferred_compression(), "zstd");
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct ProtocolCapabilities(ProtocolVersion);

impl ProtocolCapabilities {
    /// Creates capabilities from a negotiated protocol version.
    #[must_use]
    pub const fn new(version: ProtocolVersion) -> Self {
        Self(version)
    }

    /// Returns the underlying protocol version.
    #[must_use]
    pub const fn version(self) -> ProtocolVersion {
        self.0
    }

    /// Returns `true` if multiplexed I/O is supported.
    ///
    /// Delegates to [`ProtocolVersion::supports_multiplex_io`].
    #[must_use]
    pub const fn multiplex(self) -> bool {
        self.0.supports_multiplex_io()
    }

    /// Returns the preferred compression algorithm name.
    ///
    /// Delegates to [`ProtocolVersion::preferred_compression`].
    #[must_use]
    pub const fn preferred_compression(self) -> &'static str {
        self.0.preferred_compression()
    }

    /// Returns `true` if extended file flags are supported.
    ///
    /// Delegates to [`ProtocolVersion::supports_extended_flags`].
    #[must_use]
    pub const fn extended_flags(self) -> bool {
        self.0.supports_extended_flags()
    }

    /// Returns `true` if inline hardlink encoding is supported.
    ///
    /// Delegates to [`ProtocolVersion::supports_inline_hardlinks`].
    #[must_use]
    pub const fn inline_hardlinks(self) -> bool {
        self.0.supports_inline_hardlinks()
    }

    /// Returns `true` if varint encoding is used.
    ///
    /// Delegates to [`ProtocolVersion::uses_varint_encoding`].
    #[must_use]
    pub const fn varint_encoding(self) -> bool {
        self.0.uses_varint_encoding()
    }

    /// Returns `true` if incremental recursion is supported.
    ///
    /// Delegates to [`ProtocolVersion::supports_inc_recurse`].
    #[must_use]
    pub const fn inc_recurse(self) -> bool {
        self.0.supports_inc_recurse()
    }

    /// Returns `true` if checksum algorithm negotiation is supported.
    ///
    /// Delegates to [`ProtocolVersion::supports_checksum_negotiation`].
    #[must_use]
    pub const fn checksum_negotiation(self) -> bool {
        self.0.supports_checksum_negotiation()
    }

    /// Returns `true` if delete statistics are supported.
    ///
    /// Delegates to [`ProtocolVersion::supports_delete_stats`].
    #[must_use]
    pub const fn delete_stats(self) -> bool {
        self.0.supports_delete_stats()
    }
}

impl From<ProtocolVersion> for ProtocolCapabilities {
    fn from(version: ProtocolVersion) -> Self {
        Self(version)
    }
}
