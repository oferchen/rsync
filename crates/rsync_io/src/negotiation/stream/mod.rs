//! [`NegotiatedStream`] and its supporting Read/Write/BufRead trait
//! implementations, legacy greeting parsing, and buffer access helpers.

mod base;
mod buffer_access;
mod legacy;
mod traits;

pub use base::{NEGOTIATION_PROLOGUE_UNDETERMINED_MSG, NegotiatedStream};
