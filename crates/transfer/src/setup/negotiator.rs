//! Protocol negotiation trait abstractions.
//!
//! Defines the `ProtocolNegotiator` trait and its three component traits that
//! decouple the high-level setup orchestration from concrete wire-protocol
//! details. This follows the Dependency Inversion Principle - the orchestrator
//! in [`super::setup_protocol`] depends on these abstractions rather than on
//! the concrete `protocol` crate functions directly.
//!
//! The default implementation ([`RsyncNegotiator`]) wires everything to the
//! real protocol crate, preserving full upstream compatibility.

use protocol::{CompatibilityFlags, NegotiationResult, ProtocolVersion};
use std::borrow::Cow;
use std::io::{self, Read, Write};

/// Exchanges compatibility flags between server and client.
///
/// Abstracts the unidirectional compat-flags exchange described in
/// upstream `compat.c:710-741`. The server writes flags; the client reads them.
pub trait CompatFlagsExchanger {
    /// Writes the server's compatibility flags to the output stream.
    ///
    /// Handles the pre-release `'V'` encoding difference - when the client
    /// advertises `'V'`, flags are written as a single byte with
    /// `CF_VARINT_FLIST_FLAGS` implicitly set. Otherwise standard varint
    /// encoding is used.
    ///
    /// Returns the final flags (which may differ from `flags` when `'V'`
    /// implicitly adds `CF_VARINT_FLIST_FLAGS`).
    fn write_compat_flags(
        &self,
        writer: &mut dyn Write,
        flags: CompatibilityFlags,
        client_info: &str,
    ) -> io::Result<CompatibilityFlags>;

    /// Reads the server's compatibility flags from the input stream.
    ///
    /// The client calls this to obtain the flags the server wrote via
    /// [`write_compat_flags`](CompatFlagsExchanger::write_compat_flags).
    fn read_compat_flags(&self, reader: &mut dyn Read) -> io::Result<CompatibilityFlags>;

    /// Builds compatibility flags from the client's `-e` capability string.
    ///
    /// Uses the table-driven mapping in [`super::capability::CAPABILITY_MAPPINGS`]
    /// to convert capability characters into `CompatibilityFlags`.
    ///
    /// # Upstream reference
    ///
    /// Mirrors `compat.c:712-734`.
    fn build_flags_from_client_info(
        &self,
        client_info: &str,
        allow_inc_recurse: bool,
    ) -> CompatibilityFlags;

    /// Parses the client capability string from the `-e` option in client args.
    ///
    /// Returns the capability characters (e.g. `"LsfxCIvu"`) extracted from
    /// arguments like `["-e.LsfxCIvu"]` or `["-e", "LsfxCIvu"]`.
    fn parse_client_info<'a>(&self, client_args: &'a [String]) -> Cow<'a, str>;

    /// Returns `true` when `client_info` contains the pre-release `'V'`
    /// capability flag.
    fn has_pre_release_v_flag(&self, client_info: &str) -> bool;
}

/// Negotiates checksum and compression algorithms with the peer.
///
/// Abstracts the `negotiate_the_strings()` exchange from upstream
/// `compat.c:534-585`. Protocol 30+ peers exchange supported algorithm lists
/// and each side independently selects the first mutually supported entry.
pub trait CapabilityNegotiator {
    /// Performs the full capability negotiation exchange.
    ///
    /// When `config.do_negotiation` is false (peer lacks
    /// `CF_VARINT_FLIST_FLAGS`), returns defaults without any wire I/O. When
    /// true, both sides exchange their supported algorithm lists via vstrings
    /// and select the first mutually supported algorithm.
    ///
    /// When `config.compression_override` is `Some`, the compression vstring
    /// exchange is skipped and the specified algorithm is used directly -
    /// matching upstream `compat.c:543` (`do_compression && !compress_choice`).
    fn negotiate(
        &self,
        protocol: ProtocolVersion,
        stdin: &mut dyn Read,
        stdout: &mut dyn Write,
        config: &protocol::NegotiationConfig,
    ) -> io::Result<NegotiationResult>;
}

/// Exchanges the checksum seed between server and client.
///
/// Abstracts the seed exchange at upstream `compat.c:750`. The server generates
/// (or uses a fixed) seed and writes 4 bytes LE; the client reads them.
pub trait ChecksumSeedExchanger {
    /// Generates (or uses a fixed) seed and writes it to the output stream.
    ///
    /// # Seed generation (upstream `options.c:835`)
    ///
    /// - `None` or `Some(0)`: generate from `time() ^ (pid << 6)`
    /// - `Some(n)`: use `n` as the fixed seed
    fn write_seed(&self, writer: &mut dyn Write, fixed_seed: Option<u32>) -> io::Result<i32>;

    /// Reads the 4-byte LE checksum seed sent by the server.
    fn read_seed(&self, reader: &mut dyn Read) -> io::Result<i32>;
}

/// Composed negotiator that drives the full protocol setup exchange.
///
/// Bundles the three negotiation concerns - compat flags, capability
/// negotiation, and checksum seed - behind a single interface. The
/// orchestrator ([`super::setup_protocol_with`]) depends only on this trait,
/// making each concern independently replaceable for testing or alternative
/// protocol implementations.
///
/// The default implementation is [`RsyncNegotiator`], which delegates to the
/// concrete `protocol` crate functions and matches upstream rsync behaviour.
pub trait ProtocolNegotiator:
    CompatFlagsExchanger + CapabilityNegotiator + ChecksumSeedExchanger
{
}

/// Blanket implementation - any type implementing all three component traits
/// automatically satisfies `ProtocolNegotiator`.
impl<T> ProtocolNegotiator for T where
    T: CompatFlagsExchanger + CapabilityNegotiator + ChecksumSeedExchanger
{
}

/// Default negotiator wired to the concrete `protocol` crate.
///
/// This is the production implementation used by [`super::setup_protocol`].
/// Each method delegates directly to the corresponding function in the
/// `protocol` crate or in the sibling `capability`/`compat` modules,
/// preserving full upstream rsync wire compatibility.
#[derive(Debug, Default, Clone, Copy)]
pub struct RsyncNegotiator;

impl CompatFlagsExchanger for RsyncNegotiator {
    fn write_compat_flags(
        &self,
        writer: &mut dyn Write,
        flags: CompatibilityFlags,
        client_info: &str,
    ) -> io::Result<CompatibilityFlags> {
        super::compat::write_compat_flags(writer, flags, client_info)
    }

    fn read_compat_flags(&self, reader: &mut dyn Read) -> io::Result<CompatibilityFlags> {
        let value = protocol::read_varint(reader)?;
        Ok(CompatibilityFlags::from_bits(value as u32))
    }

    fn build_flags_from_client_info(
        &self,
        client_info: &str,
        allow_inc_recurse: bool,
    ) -> CompatibilityFlags {
        super::capability::build_compat_flags_from_client_info(client_info, allow_inc_recurse)
    }

    fn parse_client_info<'a>(&self, client_args: &'a [String]) -> Cow<'a, str> {
        super::capability::parse_client_info(client_args)
    }

    fn has_pre_release_v_flag(&self, client_info: &str) -> bool {
        super::capability::client_has_pre_release_v_flag(client_info)
    }
}

impl CapabilityNegotiator for RsyncNegotiator {
    fn negotiate(
        &self,
        protocol: ProtocolVersion,
        stdin: &mut dyn Read,
        stdout: &mut dyn Write,
        config: &protocol::NegotiationConfig,
    ) -> io::Result<NegotiationResult> {
        protocol::negotiate_capabilities_with_override(protocol, stdin, stdout, config)
    }
}

impl ChecksumSeedExchanger for RsyncNegotiator {
    fn write_seed(&self, writer: &mut dyn Write, fixed_seed: Option<u32>) -> io::Result<i32> {
        // upstream: options.c:835 - seed generation
        let seed = match fixed_seed {
            Some(0) | None => {
                use std::time::{SystemTime, UNIX_EPOCH};
                let timestamp = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs() as i32;
                let pid = std::process::id() as i32;
                timestamp ^ (pid << 6)
            }
            Some(fixed) => fixed as i32,
        };
        writer.write_all(&seed.to_le_bytes())?;
        writer.flush()?;
        Ok(seed)
    }

    fn read_seed(&self, reader: &mut dyn Read) -> io::Result<i32> {
        let mut buf = [0u8; 4];
        reader.read_exact(&mut buf)?;
        Ok(i32::from_le_bytes(buf))
    }
}
