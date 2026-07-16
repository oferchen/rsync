#![deny(unsafe_code)]
//! Server-side reader abstraction supporting plain and multiplex modes.
//!
//! Mirrors the writer module to handle incoming multiplexed messages.
//! When multiplex is active (protocol >= 23), this wrapper automatically
//! demultiplexes incoming messages, extracting MSG_DATA payloads.

mod counting;
mod multiplex;
mod server;

#[cfg(test)]
mod tests;

pub(crate) use counting::CountingReader;
pub use multiplex::RemoteExitError;
pub(crate) use multiplex::{DeletedRender, MultiplexReader};
pub use server::ServerReader;
