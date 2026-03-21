//! Server protocol setup utilities.
//!
//! This module mirrors upstream rsync's `compat.c:setup_protocol()` function,
//! handling protocol version negotiation and compatibility flags exchange.
//!
//! # Submodules
//!
//! - [`capability`] - Capability string building and parsing (`-e.xxx`)
//! - [`compat`] - Compatibility flags exchange
//! - [`types`] - Configuration and result types

mod capability;
mod compat;
mod types;

pub use capability::build_capability_string;
pub use compat::exchange_compat_flags_direct;
pub use types::{ProtocolSetupConfig, SetupResult};

#[cfg(test)]
pub(crate) use capability::CAPABILITY_MAPPINGS;
pub(crate) use capability::{
    build_compat_flags_from_client_info, client_has_pre_release_v_flag, parse_client_info,
};
pub(crate) use compat::write_compat_flags;
pub(crate) use protocol::CompatibilityFlags;
#[cfg(test)]
pub(crate) use protocol::ProtocolVersion;
use std::io::{self, Read, Write};

/// Performs protocol setup for the server side.
///
/// This function mirrors upstream rsync's `setup_protocol()` at `compat.c:572-644`.
///
/// When `remote_protocol` is already set (e.g., from @RSYNCD negotiation in daemon mode),
/// the 4-byte binary protocol exchange is skipped (upstream compat.c:599-607).
///
/// For protocol >= 30, compatibility flags are ALWAYS exchanged (upstream compat.c:710-743),
/// regardless of whether the binary protocol exchange happened.
///
/// For protocol >= 30, capability negotiation (checksum and compression algorithms) also
/// happens inside this function, matching upstream compat.c:534-585 (negotiate_the_strings).
///
/// # Arguments
///
/// * `stdout` - Output stream for sending server's compatibility flags (f_out in upstream)
/// * `stdin` - Input stream for reading client's algorithm choices (f_in in upstream)
/// * `config` - Protocol setup configuration containing all parameters
///
/// # Returns
///
/// Returns the negotiated algorithms (or `None` for protocol < 30), or an I/O error if
/// the exchange fails.
///
/// **IMPORTANT:** Parameter order matches upstream: f_out first, f_in second!
pub fn setup_protocol(
    stdout: &mut dyn Write,
    stdin: &mut dyn Read,
    config: &ProtocolSetupConfig<'_>,
) -> io::Result<SetupResult> {
    // For daemon mode, the binary 4-byte protocol exchange has already happened
    // via the @RSYNCD text protocol (upstream compat.c:599-607 checks remote_protocol != 0).
    // We skip that exchange here since our HandshakeResult already contains the negotiated protocol.

    // CRITICAL ORDER (upstream compat.c):
    // 1. Compat flags (protocol >= 30)
    // 2. Checksum seed (ALL protocols)

    // Build compat flags and perform negotiation for protocol >= 30
    // This mirrors upstream compat.c:710-743 which happens INSIDE setup_protocol()
    let (compat_flags, negotiated_algorithms) = if config.protocol.uses_binary_negotiation()
        && !config.skip_compat_exchange
    {
        // Build our compat flags (server side)
        // This mirrors upstream compat.c:712-732 which builds flags from client_info string
        let (our_flags, client_info) = if let Some(args) = config.client_args {
            // Daemon server mode: parse client capabilities from -e option
            let client_info = parse_client_info(args);
            let flags = build_compat_flags_from_client_info(&client_info, config.allow_inc_recurse);
            (flags, Some(client_info))
        } else {
            // SSH/client mode: use default flags based on platform capabilities
            #[cfg(unix)]
            let mut flags =
                CompatibilityFlags::CHECKSUM_SEED_FIX | CompatibilityFlags::VARINT_FLIST_FLAGS;
            #[cfg(not(unix))]
            let mut flags =
                CompatibilityFlags::CHECKSUM_SEED_FIX | CompatibilityFlags::VARINT_FLIST_FLAGS;

            // Advertise symlink timestamp support on Unix platforms
            // (mirrors upstream CAN_SET_SYMLINK_TIMES at compat.c:713-714)
            #[cfg(unix)]
            {
                flags |= CompatibilityFlags::SYMLINK_TIMES;
            }

            // Advertise INC_RECURSE when incremental recursion is allowed
            if config.allow_inc_recurse {
                flags |= CompatibilityFlags::INC_RECURSE;
            }

            (flags, None)
        };

        // Compression negotiation is controlled by the `do_compression` parameter
        // which is passed from the caller based on whether -z flag was used.
        // Both sides MUST have the same value for this to work correctly.
        let send_compression = config.do_compression;

        // Compat flags exchange is UNIDIRECTIONAL (upstream compat.c:710-741):
        // - Server (am_server=true): WRITES compat flags
        // - Client (am_server=false): READS compat flags
        let compat_flags = if config.is_server {
            // Server: build and WRITE our compat flags
            // Handle pre-release 'V' flag: use single-byte write instead of varint
            // (upstream compat.c:737-741).
            let info_ref = client_info.as_deref().unwrap_or("");
            let final_flags = write_compat_flags(stdout, our_flags, info_ref)?;
            stdout.flush()?;
            final_flags
        } else {
            // Client: READ compat flags from server
            let compat_value = protocol::read_varint(stdin)?;
            let mut flags = CompatibilityFlags::from_bits(compat_value as u32);

            // Mask off INC_RECURSE if we don't support it for this transfer.
            // upstream: compat.c:720 - client clears INC_RECURSE when not allowed.
            if !config.allow_inc_recurse {
                flags &= !CompatibilityFlags::INC_RECURSE;
            }

            flags
        };

        // Protocol 30+ capability negotiation (upstream compat.c:534-585)
        // This MUST happen inside setup_protocol(), BEFORE the function returns,
        // so negotiation completes in RAW mode BEFORE multiplex activation.
        //
        // The negotiation implementation is in protocol::negotiate_capabilities(),
        // which mirrors upstream's negotiate_the_strings() function.
        //
        // Negotiation only happens if client has VARINT_FLIST_FLAGS ('v') capability.
        // This matches upstream's do_negotiated_strings check.

        // CRITICAL: Daemon mode and SSH mode have different negotiation flows!
        // - SSH mode: Bidirectional - both sides exchange algorithm lists
        // - Daemon mode: Unidirectional - server advertises, client selects silently
        //
        // For daemon mode, capability negotiation happens during @RSYNCD handshake,
        // NOT here in setup_protocol. The client never sends algorithm responses back
        // during setup_protocol in daemon mode.
        //
        // Upstream reference:
        // - SSH mode: negotiate_the_strings() in compat.c (bidirectional)
        // - Daemon mode: output_daemon_greeting() advertises, no response expected
        //
        // Protocol 30+ capability negotiation (upstream compat.c:534-585)
        // This is called in BOTH daemon and SSH modes.
        // The do_negotiation flag controls whether actual string exchange happens.
        //
        // CRITICAL: When acting as CLIENT (is_server=false), we must check the SERVER's
        // compat flags (compat_flags), not our own flags! Upstream compat.c:740-742:
        //   "compat_flags = read_varint(f_in);
        //    if (compat_flags & CF_VARINT_FLIST_FLAGS) do_negotiated_strings = 1;"
        let do_negotiation = if config.is_server {
            // Server: check if client has 'v' or pre-release 'V' capability.
            // Both imply CF_VARINT_FLIST_FLAGS and enable negotiation.
            client_info.as_deref().map_or(
                our_flags.contains(CompatibilityFlags::VARINT_FLIST_FLAGS),
                |info| info.contains('v') || client_has_pre_release_v_flag(info),
            )
        } else {
            // Client: check if SERVER's compat flags include VARINT_FLIST_FLAGS
            // This mirrors upstream compat.c:740-742 where client reads server's flags
            compat_flags.contains(CompatibilityFlags::VARINT_FLIST_FLAGS)
        };

        // Daemon mode uses unidirectional negotiation (server sends, client reads silently)
        // SSH mode uses bidirectional negotiation (both sides exchange)
        // The caller tells us which mode via the is_daemon_mode parameter

        // CRITICAL: Both daemon and SSH modes need to call negotiate_capabilities when
        // do_negotiation is true (client has 'v' capability). The difference is:
        // - Daemon mode (is_daemon_mode=true): Server sends lists, client doesn't respond back
        // - SSH mode (is_daemon_mode=false): Both sides send lists, then both read each other's
        //
        // The is_daemon_mode flag inside negotiate_capabilities controls whether we read
        // the client's response after sending our lists.
        let algorithms = protocol::negotiate_capabilities(
            config.protocol,
            stdin,
            stdout,
            do_negotiation,
            send_compression,
            config.is_daemon_mode,
            config.is_server,
        )?;

        (Some(compat_flags), Some(algorithms))
    } else {
        (None, None) // Protocol < 30 uses default algorithms and no compat flags
    };

    // Checksum seed exchange (ALL protocols, upstream compat.c:750)
    // - Server: generates and WRITES the seed
    // - Client: READS the seed from server
    //
    // --checksum-seed behavior (upstream rsync options.c:835):
    // - None: generate random seed (time ^ (pid << 6))
    // - Some(0): use current time (upstream treats 0 as "use time()")
    // - Some(N): use N as the fixed seed
    let checksum_seed = if config.is_server {
        // Server: generate or use fixed seed, then send
        let seed = match config.checksum_seed {
            Some(0) | None => {
                // Default behavior: generate from current time XOR PID
                // --checksum-seed=0 means "use current time" in upstream rsync
                use std::time::{SystemTime, UNIX_EPOCH};
                let timestamp = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs() as i32;
                let pid = std::process::id() as i32;
                timestamp ^ (pid << 6)
            }
            Some(fixed_seed) => {
                // --checksum-seed=NUM: use the exact seed value for reproducibility
                fixed_seed as i32
            }
        };
        let seed_bytes = seed.to_le_bytes();
        stdout.write_all(&seed_bytes)?;
        stdout.flush()?;
        seed
    } else {
        // Client: read seed from server
        let mut seed_bytes = [0u8; 4];
        stdin.read_exact(&mut seed_bytes)?;
        i32::from_le_bytes(seed_bytes)
    };

    Ok(SetupResult {
        negotiated_algorithms,
        compat_flags,
        checksum_seed,
    })
}

#[cfg(test)]
mod tests;
