#![allow(clippy::module_name_repetitions)]

mod handshake;
mod negotiate;
mod parts;

pub use handshake::BinaryHandshake;
pub use negotiate::{
    negotiate_binary_session, negotiate_binary_session_from_stream,
    negotiate_binary_session_with_sniffer,
};
pub use parts::BinaryHandshakeParts;

#[cfg(test)]
mod tests;
