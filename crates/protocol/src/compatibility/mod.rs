//! # Overview
//!
//! Compatibility flags extend the rsync protocol by advertising optional
//! capabilities once both peers have agreed on a protocol version. Upstream
//! exchanges these flags using the variable-length integer codec defined in
//! [`crate::varint`]. This module mirrors that behaviour and exposes a typed
//! bitfield so higher layers can reason about individual compatibility bits
//! without manipulating integers directly.
//!
//! # Design
//!
//! - [`CompatibilityFlags`] wraps a `u32` and provides associated constants for
//!   every flag currently defined by rsync 3.4.1. The bitfield implements the
//!   standard bit-operator traits (`BitOr`, `BitAnd`, `BitXor`) to keep usage
//!   ergonomic, and reuses the varint codec for serialization.
//! - [`KnownCompatibilityFlag`] is an enumeration of upstream flag definitions
//!   together with helpers for name resolution and conversions to the bitfield.
//! - [`KnownCompatibilityFlagsIter`] yields the known compatibility flags in
//!   ascending bit order, mirroring upstream iteration semantics.
//!
//! # See also
//!
//! - [`crate::varint`] for the encoding and decoding primitives used by the
//!   bitfield implementation.

mod flags;
mod iter;
mod known;

pub use self::flags::CompatibilityFlags;
pub use self::iter::KnownCompatibilityFlagsIter;
pub use self::known::{KnownCompatibilityFlag, ParseKnownCompatibilityFlagError};

#[cfg(test)]
mod tests;
