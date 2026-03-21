use std::io::{self, Read, Write};

use logging::debug_log;

use crate::ProtocolVersion;

use super::algorithms::{
    ChecksumAlgorithm, CompressionAlgorithm, SUPPORTED_CHECKSUMS, supported_compressions,
};

/// Outcome of the protocol 30+ capability negotiation.
///
/// After both peers exchange their supported algorithm lists via the
/// `negotiate_the_strings()` exchange (upstream `compat.c:534-585`), each side
/// independently selects the first mutually supported checksum and compression
/// algorithm. This struct captures those selections so higher layers can
/// configure their I/O pipelines accordingly.
///
/// For protocol versions below 30, [`negotiate_capabilities`] returns
/// hard-coded defaults (`MD4` / `Zlib`) without performing any wire exchange,
/// matching upstream behaviour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct NegotiationResult {
    /// The checksum algorithm both peers agreed to use for block and file checksums.
    pub checksum: ChecksumAlgorithm,
    /// The compression algorithm both peers agreed to use for data transfer.
    pub compression: CompressionAlgorithm,
}

/// Negotiates checksum and compression algorithms with the peer.
///
/// This function implements upstream rsync's `negotiate_the_strings()` function
/// (compat.c:534-585), supporting both SSH mode (bidirectional exchange) and
/// daemon mode (unidirectional exchange).
///
/// # Protocol Flow
///
/// The exchange is always **bidirectional** in both SSH and daemon modes
/// (upstream compat.c:534-570 negotiate_the_strings):
/// 1. Both sides send their supported algorithm lists
/// 2. Both sides read each other's lists
/// 3. Each side selects the first mutually supported algorithm
///
/// # Arguments
///
/// * `protocol` - The negotiated protocol version
/// * `stdin` - Input stream for reading peer's choices/lists
/// * `stdout` - Output stream for sending algorithm lists
/// * `do_negotiation` - Whether to perform negotiation (false = use defaults without I/O)
/// * `send_compression` - Whether compression negotiation is enabled
/// * `is_daemon_mode` - Whether this is daemon mode (vs SSH mode)
/// * `is_server` - Whether this is the server side
///
/// # Returns
///
/// Returns the negotiated algorithms, or an I/O error if negotiation fails.
///
/// # Errors
///
/// - Protocol < 30: Not an error, returns default algorithms (MD4, Zlib)
/// - `do_negotiation=false`: Returns defaults (MD5, Zlib if `-z`) without I/O
/// - Peer chooses unsupported algorithm: InvalidData error
/// - I/O errors during send/receive
///
/// # Examples
///
/// ```no_run
/// use protocol::{ProtocolVersion, negotiate_capabilities};
/// use std::io::{stdin, stdout};
///
/// let protocol = ProtocolVersion::try_from(32)?;
/// // SSH mode client
/// let result = negotiate_capabilities(
///     protocol,
///     &mut stdin(),
///     &mut stdout(),
///     true,   // do_negotiation
///     true,   // send_compression
///     false,  // is_daemon_mode (SSH mode)
///     false,  // is_server (client side)
/// )?;
/// println!("Using checksum: {:?}, compression: {:?}",
///          result.checksum, result.compression);
/// # Ok::<(), std::io::Error>(())
/// ```
pub fn negotiate_capabilities(
    protocol: ProtocolVersion,
    stdin: &mut dyn Read,
    stdout: &mut dyn Write,
    do_negotiation: bool,
    send_compression: bool,
    is_daemon_mode: bool,
    is_server: bool,
) -> io::Result<NegotiationResult> {
    negotiate_capabilities_with_override(
        protocol,
        stdin,
        stdout,
        do_negotiation,
        send_compression,
        is_daemon_mode,
        is_server,
        None,
    )
}

/// Negotiates checksum and compression algorithms with the peer, with an
/// optional user-specified checksum algorithm override.
///
/// When `checksum_override` is `Some`, the advertised checksum list is replaced
/// with just the requested algorithm (mirroring upstream rsync's
/// `--checksum-choice` behavior from `options.c:valid_checksums`). The override
/// also forces selection of that algorithm from the peer's list, returning an
/// error if the peer does not support it.
///
/// When `checksum_override` is `None`, behaves identically to
/// [`negotiate_capabilities`].
pub fn negotiate_capabilities_with_override(
    protocol: ProtocolVersion,
    stdin: &mut dyn Read,
    stdout: &mut dyn Write,
    do_negotiation: bool,
    send_compression: bool,
    is_daemon_mode: bool,
    is_server: bool,
    checksum_override: Option<ChecksumAlgorithm>,
) -> io::Result<NegotiationResult> {
    // Protocol < 30 doesn't support negotiation, use defaults
    if protocol.uses_fixed_encoding() {
        // When user forced a checksum on a legacy protocol, honour it directly
        // since there is no wire negotiation to perform.
        let checksum = checksum_override.unwrap_or(ChecksumAlgorithm::MD4);
        debug_log!(
            Proto,
            1,
            "protocol {} uses legacy encoding, using checksum={} compression=Zlib",
            protocol.as_u8(),
            checksum.as_str()
        );
        return Ok(NegotiationResult {
            checksum,
            compression: CompressionAlgorithm::Zlib,
        });
    }

    // CRITICAL: If client doesn't have VARINT_FLIST_FLAGS ('v' capability), it doesn't
    // support negotiate_the_strings() at all. We must NOT send negotiation strings to
    // such clients - they will interpret the varint length as a multiplex tag and fail
    // with "unexpected tag N" errors.
    //
    // This matches upstream's do_negotiated_strings check (compat.c:561-585) where
    // negotiate_the_strings is ONLY CALLED when do_negotiated_strings is TRUE.
    // When FALSE, upstream uses pre-filled defaults without any wire protocol exchange.
    if !do_negotiation {
        // Use protocol 30+ defaults without sending or reading anything.
        // upstream: compat.c:194 — when -z is active but no vstring negotiation,
        // parse_compress_choice() defaults to CPRES_ZLIB.
        let checksum = checksum_override.unwrap_or(ChecksumAlgorithm::MD5);
        let compression = if send_compression {
            CompressionAlgorithm::Zlib
        } else {
            CompressionAlgorithm::None
        };
        debug_log!(
            Proto,
            1,
            "client lacks VARINT_FLIST_FLAGS, using checksum={} compression={}",
            checksum.as_str(),
            compression.as_str()
        );
        return Ok(NegotiationResult {
            checksum,
            compression,
        });
    }

    // Negotiation flow (upstream compat.c:534-570 negotiate_the_strings):
    //
    // BIDIRECTIONAL exchange in BOTH daemon and SSH modes when CF_VARINT_FLIST_FLAGS is set:
    //   - Both sides SEND their algorithm lists first
    //   - Then both sides READ each other's lists
    //   - Both independently choose the first match from the remote's list
    //
    // Upstream comment: "We send all the negotiation strings before we start
    // to read them to help avoid a slow startup."
    //
    // Note: Even though daemon mode advertises algorithms in the @RSYNCD greeting,
    // the vstring exchange STILL happens if CF_VARINT_FLIST_FLAGS is negotiated.
    // The greeting is just informational; the actual selection uses vstrings.
    let _ = is_daemon_mode; // Exchange happens in all modes when do_negotiation=true
    let _ = is_server; // Both sides behave symmetrically

    // Step 1: SEND our supported algorithm lists (upstream compat.c:541-544)
    // Uses vstring format (NOT varint) - see write_vstring documentation
    //
    // When --checksum-choice overrides the selection, advertise only that
    // algorithm (upstream options.c replaces valid_checksums with the user's
    // choice so negotiate_the_strings sees a single-entry list).
    let checksum_list = match checksum_override {
        Some(algo) => algo.as_str().to_owned(),
        None => SUPPORTED_CHECKSUMS.join(" "),
    };
    debug_log!(Proto, 2, "sending checksum list: {}", checksum_list);
    write_vstring(stdout, &checksum_list)?;

    // Send compression list only if compression is enabled
    if send_compression {
        let compression_list = supported_compressions().join(" ");
        debug_log!(Proto, 2, "sending compression list: {}", compression_list);
        write_vstring(stdout, &compression_list)?;
    }

    stdout.flush()?;

    // Step 2: READ the remote side's algorithm lists (upstream compat.c:546-564)
    // Uses vstring format (NOT varint) - see read_vstring documentation
    let remote_checksum_list = read_vstring(stdin)?;
    debug_log!(Proto, 2, "received checksum list: {}", remote_checksum_list);

    let remote_compression_list = if send_compression {
        let list = read_vstring(stdin)?;
        debug_log!(Proto, 2, "received compression list: {}", list);
        Some(list)
    } else {
        None
    };

    // Step 3: Choose algorithms - pick first from REMOTE's list that WE also support
    // This matches upstream where "the client picks the first name in the server's list
    // that is also in the client's list"
    //
    // When the user forced a checksum via --checksum-choice, verify the remote
    // advertises it and use it unconditionally.
    let checksum = match checksum_override {
        Some(forced) => {
            let forced_name = forced.as_str();
            if remote_checksum_list
                .split_whitespace()
                .any(|name| name == forced_name)
            {
                forced
            } else {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "--checksum-choice '{forced_name}' is not supported by the remote side (remote offers: {remote_checksum_list})"
                    ),
                ));
            }
        }
        None => choose_checksum_algorithm(&remote_checksum_list)?,
    };

    let compression = if let Some(ref list) = remote_compression_list {
        choose_compression_algorithm(list)?
    } else {
        CompressionAlgorithm::None
    };

    debug_log!(
        Proto,
        1,
        "negotiated checksum={}, compression={}",
        checksum.as_str(),
        compression.as_str()
    );
    Ok(NegotiationResult {
        checksum,
        compression,
    })
}

/// Chooses a checksum algorithm from the remote peer's list.
///
/// Selects the first algorithm in the remote's list that we also support.
/// Returns an error if no common algorithm is found - upstream rsync treats
/// this as a hard failure (compat.c:383-406 `recv_negotiate_str`).
pub(super) fn choose_checksum_algorithm(client_list: &str) -> io::Result<ChecksumAlgorithm> {
    for algo in client_list.split_whitespace() {
        // Try to parse each algorithm the client supports
        if let Ok(checksum) = ChecksumAlgorithm::parse(algo) {
            // Check if we support it
            if SUPPORTED_CHECKSUMS.contains(&checksum.as_str()) {
                return Ok(checksum);
            }
        }
    }

    // upstream: compat.c:383-406 — failure to negotiate is a hard error
    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        format!("failed to negotiate a common checksum algorithm (remote offers: {client_list})"),
    ))
}

/// Chooses a compression algorithm from the client's list.
///
/// Selects the first algorithm in the client's list that we also support.
pub(super) fn choose_compression_algorithm(client_list: &str) -> io::Result<CompressionAlgorithm> {
    let supported = supported_compressions();
    for algo in client_list.split_whitespace() {
        // Try to parse each algorithm the client supports
        if let Ok(compression) = CompressionAlgorithm::parse(algo) {
            // Check if we support it (both parse AND have feature enabled)
            if supported.contains(&algo) {
                return Ok(compression);
            }
        }
    }

    // No common algorithm found - use "none"
    Ok(CompressionAlgorithm::None)
}

/// Writes a vstring (variable-length string) using upstream rsync's format.
///
/// Format (upstream io.c:2222-2240 write_vstring):
/// - For len <= 127: 1 byte = len
/// - For len > 127: 2 bytes = [(len >> 8) | 0x80, len & 0xFF]
///
/// This is DIFFERENT from varint encoding! Varint uses 7 bits per byte with
/// continuation bits, while vstring uses a simpler 1-or-2 byte format.
pub(super) fn write_vstring(writer: &mut dyn Write, s: &str) -> io::Result<()> {
    let bytes = s.as_bytes();
    let len = bytes.len();

    if len > 0x7FFF {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("vstring too long: {len} > {}", 0x7FFF),
        ));
    }

    let len_bytes = if len > 0x7F {
        // 2-byte format: high byte with 0x80 marker, then low byte
        let high = ((len >> 8) as u8) | 0x80;
        let low = (len & 0xFF) as u8;
        vec![high, low]
    } else {
        // 1-byte format: just the length
        vec![len as u8]
    };

    writer.write_all(&len_bytes)?;
    writer.write_all(bytes)?;
    Ok(())
}

/// Reads a vstring (variable-length string) using upstream rsync's format.
///
/// Format (upstream io.c:1944-1961 read_vstring):
/// - Read first byte
/// - If high bit set: len = (first & 0x7F) * 256 + read_another_byte
/// - Otherwise: len = first byte
/// - Then read `len` bytes of string data
///
/// This is DIFFERENT from varint encoding!
pub(super) fn read_vstring(reader: &mut dyn Read) -> io::Result<String> {
    // upstream: compat.c:91 #define MAX_NSTR_STRLEN 256
    const MAX_NSTR_STRLEN: usize = 256;

    let mut first = [0u8; 1];
    reader.read_exact(&mut first)?;

    let len = if first[0] & 0x80 != 0 {
        // 2-byte format
        let high = (first[0] & 0x7F) as usize;
        let mut second = [0u8; 1];
        reader.read_exact(&mut second)?;
        high * 256 + second[0] as usize
    } else {
        // 1-byte format
        first[0] as usize
    };

    // Sanity check: must not exceed upstream's hard limit.
    if len > MAX_NSTR_STRLEN {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("vstring too long: {len} bytes (max {MAX_NSTR_STRLEN})"),
        ));
    }

    let mut buf = vec![0u8; len];
    reader.read_exact(&mut buf)?;

    // Algorithm names are ASCII (subset of UTF-8), but validate anyway
    // to catch protocol errors early (e.g., reading from wrong stream layer)
    String::from_utf8(buf).map_err(|e| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("invalid UTF-8 in vstring: {e}"),
        )
    })
}
