//! Enum-based dispatch and factory for protocol codecs.
//!
//! Provides [`ProtocolCodecEnum`] for selecting between legacy and modern
//! codecs at runtime without boxing, and [`create_protocol_codec`] as the
//! factory function.

use std::io::{self, Read, Write};

use super::legacy::LegacyProtocolCodec;
use super::modern::ModernProtocolCodec;
use super::ProtocolCodec;

/// Enum wrapper for dynamic codec dispatch.
///
/// Selects the appropriate codec at runtime based on protocol version
/// without heap allocation.
#[derive(Debug, Clone, Copy)]
pub enum ProtocolCodecEnum {
    /// Legacy codec for protocol 28-29.
    Legacy(LegacyProtocolCodec),
    /// Modern codec for protocol 30+.
    Modern(ModernProtocolCodec),
}

/// Generates a delegating trait method that dispatches to the inner Legacy or
/// Modern codec variant. Two arms handle the two method shapes in
/// [`ProtocolCodec`]:
///
/// - `read`: `fn name<R: Read + ?Sized>(&self, reader) -> ReturnType`
/// - `write`: `fn name<W: Write + ?Sized>(&self, writer, arg) -> io::Result<()>`
macro_rules! delegate_codec {
    (read $method:ident -> $ret:ty) => {
        fn $method<R: Read + ?Sized>(&self, reader: &mut R) -> $ret {
            match self {
                Self::Legacy(c) => c.$method(reader),
                Self::Modern(c) => c.$method(reader),
            }
        }
    };
    (write $method:ident, $arg:ident : $ty:ty) => {
        fn $method<W: Write + ?Sized>(&self, writer: &mut W, $arg: $ty) -> io::Result<()> {
            match self {
                Self::Legacy(c) => c.$method(writer, $arg),
                Self::Modern(c) => c.$method(writer, $arg),
            }
        }
    };
}

impl ProtocolCodec for ProtocolCodecEnum {
    fn protocol_version(&self) -> u8 {
        match self {
            Self::Legacy(c) => c.protocol_version(),
            Self::Modern(c) => c.protocol_version(),
        }
    }

    delegate_codec!(write write_file_size, size: i64);
    delegate_codec!(read read_file_size -> io::Result<i64>);
    delegate_codec!(write write_mtime, mtime: i64);
    delegate_codec!(read read_mtime -> io::Result<i64>);
    delegate_codec!(write write_long_name_len, len: usize);
    delegate_codec!(read read_long_name_len -> io::Result<usize>);
}

/// Creates the appropriate protocol codec for the given version.
///
/// - Protocol 28-29: Returns [`LegacyProtocolCodec`] wrapped in [`ProtocolCodecEnum::Legacy`]
/// - Protocol 30+: Returns [`ModernProtocolCodec`] wrapped in [`ProtocolCodecEnum::Modern`]
#[must_use]
pub fn create_protocol_codec(protocol_version: u8) -> ProtocolCodecEnum {
    if protocol_version < 30 {
        ProtocolCodecEnum::Legacy(LegacyProtocolCodec::new(protocol_version))
    } else {
        ProtocolCodecEnum::Modern(ModernProtocolCodec::new(protocol_version))
    }
}
