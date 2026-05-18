//! Serialization contract for items routed through the spill layer.
//!
//! The spill layer is agnostic to item shape; it only needs a deterministic
//! byte representation plus a memory-accounting hint. Implementations live
//! next to their data types (e.g. `DeltaResult` in
//! `concurrent_delta/types.rs`), keeping wire-format details adjacent to
//! the types they describe.

use std::io::{self, Read, Write};

/// Codec for serializing and deserializing items to the spill file.
///
/// Implementations must produce a deterministic byte representation and
/// report an accurate `encoded_size` for memory accounting. The encoded
/// format is opaque to the spill layer - only `encode` and `decode` must
/// agree on the wire format.
pub trait SpillCodec: Sized {
    /// Writes the item to `writer` in a format that [`decode`](Self::decode) can read back.
    ///
    /// # Errors
    ///
    /// Returns an I/O error if writing fails.
    fn encode(&self, writer: &mut dyn Write) -> io::Result<()>;

    /// Reads an item from `reader` that was previously written by [`encode`](Self::encode).
    ///
    /// # Errors
    ///
    /// Returns an I/O error if reading fails or the data is corrupt.
    fn decode(reader: &mut dyn Read) -> io::Result<Self>;

    /// Returns the approximate in-memory size of this item in bytes.
    ///
    /// Used for memory accounting to decide when to spill. Does not need
    /// to be exact - a conservative overestimate is fine.
    fn estimated_size(&self) -> usize;
}
