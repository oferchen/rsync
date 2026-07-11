#![deny(unsafe_code)]
//! Server-side reader abstraction supporting plain and multiplex modes.
//!
//! Mirrors the writer module to handle incoming multiplexed messages.
//! When multiplex is active (protocol >= 23), this wrapper automatically
//! demultiplexes incoming messages, extracting MSG_DATA payloads.

mod counting;
mod multiplex;
mod server;

#[cfg(feature = "tokio-transfer")]
mod compressed;

#[cfg(test)]
mod tests;

pub(crate) use counting::CountingReader;
pub(crate) use multiplex::MultiplexReader;
pub use multiplex::RemoteExitError;
pub use server::ServerReader;

#[cfg(feature = "tokio-transfer")]
#[cfg_attr(not(test), allow(unused_imports))]
pub(crate) use compressed::AsyncCompressedReader;

#[cfg(feature = "tokio-transfer")]
#[cfg_attr(not(test), allow(unused_imports))]
pub(crate) use server::AsyncServerReader;
