//! Server protocol setup utilities.
//!
//! This module mirrors upstream rsync's `compat.c:setup_protocol()` function,
//! handling protocol version negotiation and compatibility flags exchange.
//!
//! # Dependency Inversion
//!
//! The high-level orchestration in [`setup_protocol`] depends on trait
//! abstractions ([`ProtocolNegotiator`] and its component traits) rather than
//! on concrete `protocol` crate functions. The default production wiring uses
//! [`RsyncNegotiator`], which delegates to the real protocol crate. Tests or
//! alternative implementations can substitute custom negotiators.
//!
//! # Submodules
//!
//! - `capability` - Capability string building and parsing (`-e.xxx`)
//! - `compat` - Compatibility flags exchange
//! - `negotiator` - Trait abstractions and default implementation
//! - `restrictions` - Protocol version feature restrictions (compat.c:641-709)
//! - `types` - Configuration and result types

mod capability;
mod compat;
mod negotiator;
mod restrictions;
mod types;

pub use capability::{build_capability_string, build_capability_string_suffix};
pub use compat::exchange_compat_flags_direct;
pub use negotiator::{
    CapabilityNegotiator, ChecksumSeedExchanger, CompatFlagsExchanger, ProtocolNegotiator,
    RsyncNegotiator,
};
pub use restrictions::{
    ProtocolRestrictionFlags, RestrictionAdjustments, apply_protocol_restrictions,
};
pub use types::{ProtocolSetupConfig, SetupResult};

#[cfg(test)]
pub(crate) use capability::CAPABILITY_MAPPINGS;
#[cfg(test)]
pub(crate) use capability::{
    build_compat_flags_from_client_info, client_has_pre_release_v_flag, parse_client_info,
};
#[cfg(test)]
pub(crate) use compat::write_compat_flags;
pub(crate) use protocol::CompatibilityFlags;
#[cfg(test)]
pub(crate) use protocol::ProtocolVersion;
use std::io::{self, Read, Write};

/// Performs protocol setup using the default [`RsyncNegotiator`].
///
/// This is the standard entry point for production code. It delegates to
/// [`setup_protocol_with`] with a [`RsyncNegotiator`] instance, preserving
/// full upstream rsync wire compatibility.
///
/// Mirrors upstream rsync's `setup_protocol()` at `compat.c:572-644`.
///
/// # Arguments
///
/// * `stdout` - Output stream for sending server's compatibility flags (f_out in upstream)
/// * `stdin` - Input stream for reading client's algorithm choices (f_in in upstream)
/// * `config` - Protocol setup configuration containing all parameters
///
/// **IMPORTANT:** Parameter order matches upstream: f_out first, f_in second!
pub fn setup_protocol(
    stdout: &mut dyn Write,
    stdin: &mut dyn Read,
    config: &ProtocolSetupConfig<'_>,
) -> io::Result<SetupResult> {
    setup_protocol_with(stdout, stdin, config, &RsyncNegotiator)
}

/// Performs protocol setup using a caller-supplied negotiator.
///
/// This function contains the high-level orchestration logic from upstream
/// `compat.c:setup_protocol()`. It depends only on the [`ProtocolNegotiator`]
/// trait abstraction, allowing the three negotiation concerns (compat flags,
/// capability negotiation, checksum seed) to be independently replaced.
///
/// # Protocol phases (upstream compat.c order)
///
/// 1. Compat flags exchange (protocol >= 30) - upstream compat.c:710-743
/// 2. Capability negotiation (protocol >= 30) - upstream compat.c:534-585
/// 3. Checksum seed exchange (ALL protocols) - upstream compat.c:750
///
/// # Arguments
///
/// * `stdout` - Output stream (f_out in upstream)
/// * `stdin` - Input stream (f_in in upstream)
/// * `config` - Protocol setup configuration
/// * `negotiator` - Trait object providing the three negotiation implementations
pub fn setup_protocol_with<'a>(
    stdout: &mut dyn Write,
    stdin: &mut dyn Read,
    config: &ProtocolSetupConfig<'a>,
    negotiator: &dyn ProtocolNegotiator,
) -> io::Result<SetupResult> {
    // upstream compat.c:599-607 - when remote_protocol != 0 (daemon mode),
    // binary 4-byte protocol exchange was already done via @RSYNCD text protocol.

    let (compat_flags, negotiated_algorithms) =
        if config.protocol.uses_binary_negotiation() && !config.skip_compat_exchange {
            let (our_flags, client_info) = build_our_flags(config, negotiator);
            // upstream: compat.c:543 - compression vstrings are only exchanged
            // when do_compression && !compress_choice. When --compress-choice is
            // specified, both sides already know the algorithm.
            let send_compression = config.do_compression && config.compress_choice.is_none();

            // Compat flags exchange is UNIDIRECTIONAL (upstream compat.c:710-741):
            // Server writes, client reads.
            let compat_flags = if config.is_server {
                let info_ref = client_info.as_deref().unwrap_or("");
                let final_flags = negotiator.write_compat_flags(stdout, our_flags, info_ref)?;
                stdout.flush()?;
                final_flags
            } else {
                // The compat-flags exchange is unidirectional: the server writes
                // the negotiated set and the client honours it verbatim.
                // upstream: compat.c:745-746 recv side -
                //   `inc_recurse = compat_flags & CF_INC_RECURSE ? 1 : 0;`
                // The server only sets CF_INC_RECURSE when the client advertised
                // the 'i' capability (compat.c:713, gated by
                // set_allow_inc_recurse), so a received CF_INC_RECURSE means this
                // side opted in and must drain the per-directory sub-lists framed
                // by NDX_FLIST_OFFSET. Masking the flag here (the old
                // `!allow_inc_recurse` strip) left the receiver reading that
                // segment framing as file entries, tripping
                // `overflow in read_varint`. When 'i' is not advertised the server
                // never sets the bit, so honouring it is inert for every transfer
                // that does not opt in.
                negotiator.read_compat_flags(stdin)?
            };

            // upstream: compat.c:751-753 - create-times ride the extended varint
            // file-list flags (crtimes_ndx, compat.c:582-583). A peer that never
            // advertised CF_VARINT_FLIST_FLAGS (rsync older than 3.2.0) cannot
            // parse that field, so upstream aborts with RERR_PROTOCOL here -
            // after the compat-flags exchange but before negotiate_the_strings()
            // (compat.c:809). Abort at the same point so no negotiation strings
            // are exchanged on failure.
            require_crtimes_capability(config.preserve_crtimes, compat_flags)?;

            // Determine whether capability negotiation should happen.
            // upstream compat.c:740-742 - do_negotiated_strings requires CF_VARINT_FLIST_FLAGS.
            let do_negotiation = should_negotiate(
                config.is_server,
                &client_info,
                our_flags,
                compat_flags,
                negotiator,
            );

            // upstream: compat.c:819 parse_compress_choice(1) - when an
            // explicit compress_choice is set (--compress-choice=ALGO,
            // --new-compress, --old-compress), pass it as a compression
            // override so the protocol layer uses it directly without
            // vstring exchange.
            let algorithms = negotiator.negotiate(
                config.protocol,
                stdin,
                stdout,
                &protocol::NegotiationConfig {
                    do_negotiation,
                    send_compression,
                    is_daemon_mode: config.is_daemon_mode,
                    is_server: config.is_server,
                    // upstream: compat.c:819 parse_checksum_choice(1) - an
                    // explicit --checksum-choice=ALGO forces the algorithm and
                    // skips the checksum vstring exchange (compat.c:541). Mirror
                    // the compress_choice threading directly above.
                    checksum_override: config.checksum_choice,
                    compression_override: config.compress_choice,
                    compression_level: config.compression_level,
                },
            )?;

            (Some(compat_flags), Some(algorithms))
        } else {
            // upstream: compat.c - at protocol < 30, no binary negotiation
            // occurs. Compression is determined solely by the -z flag (CPRES_ZLIB).
            // Checksum is always MD4. We must still populate negotiated_algorithms
            // so the token reader/writer uses compressed format.
            let legacy_algorithms = if config.do_compression {
                let compression = config
                    .compress_choice
                    .unwrap_or(protocol::CompressionAlgorithm::Zlib);
                Some(protocol::NegotiationResult {
                    checksum: protocol::ChecksumAlgorithm::MD4,
                    compression,
                })
            } else {
                None
            };
            (None, legacy_algorithms)
        };

    // Checksum seed exchange (ALL protocols, upstream compat.c:750)
    let checksum_seed = if config.is_server {
        negotiator.write_seed(stdout, config.checksum_seed)?
    } else {
        negotiator.read_seed(stdin)?
    };

    Ok(SetupResult {
        negotiated_algorithms,
        compat_flags,
        checksum_seed,
    })
}

/// Aborts when `--crtimes` is requested but the negotiated peer did not
/// advertise `CF_VARINT_FLIST_FLAGS`.
///
/// Create-times are transmitted through the extended varint file-list flags, so
/// a peer that lacks that capability (rsync older than 3.2.0) cannot parse the
/// extra flist field. Upstream prints the message below and calls
/// `exit_cleanup(RERR_PROTOCOL)`; the returned error is tagged as a
/// [`protocol::ProtocolViolation`] so the core exit-code mapper yields
/// `RERR_PROTOCOL` (2) rather than `RERR_STREAMIO` (12).
///
/// # Upstream Reference
///
/// `compat.c:751-753`:
/// ```c
/// if (!xfer_flags_as_varint && preserve_crtimes) {
///     fprintf(stderr, "Both rsync versions must be at least 3.2.0 for --crtimes.\n");
///     exit_cleanup(RERR_PROTOCOL);
/// }
/// ```
fn require_crtimes_capability(
    preserve_crtimes: bool,
    compat_flags: CompatibilityFlags,
) -> io::Result<()> {
    if preserve_crtimes && !compat_flags.contains(CompatibilityFlags::VARINT_FLIST_FLAGS) {
        return Err(protocol::protocol_violation(
            "Both rsync versions must be at least 3.2.0 for --crtimes.",
        ));
    }
    Ok(())
}

/// Builds compatibility flags for our side of the connection.
///
/// In daemon server mode, parses the client's `-e` capability string from
/// `client_args`. In SSH server mode, extracts it from the compact
/// `flag_string`. In SSH/client mode, uses platform defaults.
///
/// Returns the flags and optionally the parsed client info string.
///
/// upstream: compat.c:161-179 - `set_allow_inc_recurse()` sets `client_info`
/// from `shell_cmd` (SSH) or `maybe_add_e_option()` (local), then
/// compat.c:712-732 uses `client_info` to build compat flags.
fn build_our_flags<'a>(
    config: &ProtocolSetupConfig<'a>,
    negotiator: &dyn ProtocolNegotiator,
) -> (CompatibilityFlags, Option<std::borrow::Cow<'a, str>>) {
    if let Some(args) = config.client_args {
        // Daemon server mode: parse client capabilities from -e option
        // upstream: compat.c:712-732
        let client_info = negotiator.parse_client_info(args);
        let flags = negotiator.build_flags_from_client_info(&client_info, config.allow_inc_recurse);
        (flags, Some(client_info))
    } else if config.is_server {
        if let Some(flag_str) = config.flag_string {
            // SSH server mode: extract capability string from the compact
            // flag string. Upstream sets `client_info = shell_cmd` where
            // `shell_cmd` is the value of the `-e` option parsed from the
            // compact flag string (compat.c:163-164).
            //
            // Wrap the flag string in a slice so parse_client_info can
            // scan it for the embedded `-e.xxx` capability chars.
            let args = [flag_str.to_owned()];
            let client_info = negotiator.parse_client_info(&args);
            if client_info.is_empty() {
                // No `-e` in flag string - use platform defaults.
                // This matches upstream where `shell_cmd` is empty.
                (build_default_flags(config.allow_inc_recurse), None)
            } else {
                let flags =
                    negotiator.build_flags_from_client_info(&client_info, config.allow_inc_recurse);
                (
                    flags,
                    Some(std::borrow::Cow::Owned(client_info.into_owned())),
                )
            }
        } else {
            // SSH server mode without flag string - use platform defaults.
            (build_default_flags(config.allow_inc_recurse), None)
        }
    } else {
        // Client mode: set all flags we support, matching upstream
        // compat.c:712-732 which sets flags based on compile-time features
        // and the capability string advertised to the peer.
        (build_default_flags(config.allow_inc_recurse), None)
    }
}

/// Builds the default set of compatibility flags for our platform.
///
/// Used when we are the client (advertising our capabilities) or when
/// the server has no capability string from the remote.
///
/// upstream: compat.c:712-732 - flags set based on compile-time features.
fn build_default_flags(allow_inc_recurse: bool) -> CompatibilityFlags {
    let mut flags = CompatibilityFlags::CHECKSUM_SEED_FIX
        | CompatibilityFlags::VARINT_FLIST_FLAGS
        | CompatibilityFlags::SAFE_FILE_LIST
        | CompatibilityFlags::AVOID_XATTR_OPTIMIZATION
        | CompatibilityFlags::INPLACE_PARTIAL_DIR
        | CompatibilityFlags::ID0_NAMES;

    // upstream: CAN_SET_SYMLINK_TIMES at compat.c:713-714
    #[cfg(unix)]
    {
        flags |= CompatibilityFlags::SYMLINK_TIMES;
    }

    // upstream: compat.c:715-716 - CF_SYMLINK_ICONV is gated on
    // `#ifdef ICONV_OPTION`. Mirror that with the `iconv` cargo feature
    // so SSH/client builds without iconv neither set the flag locally
    // nor advertise 's' to the peer (see capability::CAPABILITY_MAPPINGS).
    #[cfg(all(unix, feature = "iconv"))]
    {
        flags |= CompatibilityFlags::SYMLINK_ICONV;
    }

    if allow_inc_recurse {
        flags |= CompatibilityFlags::INC_RECURSE;
    }

    flags
}

/// Determines whether capability negotiation (vstring exchange) should occur.
///
/// Server side: checks if the client has `'v'` or pre-release `'V'` capability.
/// Client side: checks if the server's compat flags include
/// `CF_VARINT_FLIST_FLAGS`.
///
/// # Upstream reference
///
/// `compat.c:740-742` - `do_negotiated_strings` is set when the peer's flags
/// contain `CF_VARINT_FLIST_FLAGS`.
fn should_negotiate(
    is_server: bool,
    client_info: &Option<std::borrow::Cow<'_, str>>,
    our_flags: CompatibilityFlags,
    peer_flags: CompatibilityFlags,
    negotiator: &dyn ProtocolNegotiator,
) -> bool {
    if is_server {
        client_info.as_deref().map_or(
            our_flags.contains(CompatibilityFlags::VARINT_FLIST_FLAGS),
            |info| info.contains('v') || negotiator.has_pre_release_v_flag(info),
        )
    } else {
        // Client checks SERVER's compat flags
        // upstream: compat.c:740-742
        peer_flags.contains(CompatibilityFlags::VARINT_FLIST_FLAGS)
    }
}

#[cfg(test)]
mod tests;
