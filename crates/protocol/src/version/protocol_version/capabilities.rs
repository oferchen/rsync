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
}
