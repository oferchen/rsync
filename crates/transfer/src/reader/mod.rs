#![deny(unsafe_code)]
//! Server-side reader abstraction supporting plain and multiplex modes.
//!
//! Mirrors the writer module to handle incoming multiplexed messages.
//! When multiplex is active (protocol >= 23), this wrapper automatically
//! demultiplexes incoming messages, extracting MSG_DATA payloads.

mod multiplex;
mod server;

#[cfg(test)]
mod tests;

pub(crate) use multiplex::MultiplexReader;
pub use server::ServerReader;
