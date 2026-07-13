use std::io::{self, Read, Write};

use logging::debug_log;

use crate::ProtocolVersion;
use crate::nstr::{
    CLVL_NOT_SPECIFIED, NstrCategory, NstrSide, trace_checksum_summary, trace_compress_summary,
    trace_recv_list, trace_send_list,
};

use super::algorithms::{
    ChecksumAlgorithm, CompressionAlgorithm, SUPPORTED_CHECKSUMS, supported_compressions,
};

/// Configuration for capability negotiation.
///
/// Bundles negotiation context flags and optional algorithm overrides
/// into a single struct, following the Parameter Object pattern to keep
/// function signatures concise.
#[derive(Debug, Clone, Copy)]
pub struct NegotiationConfig {
    /// Whether to perform negotiation (false = use defaults without I/O).
    ///
    /// Set to false when the peer lacks `CF_VARINT_FLIST_FLAGS` ('v' capability)
    /// and does not support `negotiate_the_strings()`.
    pub do_negotiation: bool,
    /// Whether compression negotiation is enabled (true when `-z` is active).
    pub send_compression: bool,
    /// Whether this is daemon mode (vs SSH mode).
    pub is_daemon_mode: bool,
    /// Whether this is the server side.
    pub is_server: bool,
    /// Optional user-specified checksum algorithm override (`--checksum-choice`).
    ///
    /// When set, the advertised checksum list is replaced with just this
    /// algorithm, and selection is forced to it if the peer supports it.
    pub checksum_override: Option<ChecksumAlgorithm>,
    /// Optional user-specified compression algorithm override (`--compress-choice`).
    ///
    /// When set, the compression vstring exchange is skipped and this algorithm
    /// is used directly - matching upstream `compat.c:543`.
    pub compression_override: Option<CompressionAlgorithm>,
    /// User-specified compression level (`--compress-level=N`), or
    /// [`CLVL_NOT_SPECIFIED`] when the flag was not given.
    ///
    /// Rendered verbatim in the `parse_compress_choice` NSTR summary line
    /// (`compat.c:214-219`), mirroring upstream's raw `do_compression_level`
    /// value which stays `CLVL_NOT_SPECIFIED` (`INT_MIN`) unless
    /// `--compress-level` was passed (`options.c:88,767`).
    pub compression_level: i32,
}

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
        &NegotiationConfig {
            do_negotiation,
            send_compression,
            is_daemon_mode,
            is_server,
            checksum_override: None,
            compression_override: None,
            compression_level: CLVL_NOT_SPECIFIED,
        },
    )
}

/// Negotiates checksum and compression algorithms with the peer, with
/// optional user-specified algorithm overrides via [`NegotiationConfig`].
///
/// When `config.checksum_override` is `Some`, the advertised checksum list is
/// replaced with just the requested algorithm (mirroring upstream rsync's
/// `--checksum-choice` behavior from `options.c:valid_checksums`). The override
/// also forces selection of that algorithm from the peer's list, returning an
/// error if the peer does not support it.
///
/// When `config.compression_override` is `Some`, the compression vstring
/// exchange is skipped entirely (matching upstream `compat.c:543` which only
/// exchanges compression vstrings when `do_compression && !compress_choice`).
/// The override algorithm is used directly. The caller is responsible for
/// ensuring `send_compression` is `false` when a compression override is
/// active.
///
/// When both overrides are `None`, behaves identically to
/// [`negotiate_capabilities`].
pub fn negotiate_capabilities_with_override(
    protocol: ProtocolVersion,
    stdin: &mut dyn Read,
    stdout: &mut dyn Write,
    config: &NegotiationConfig,
) -> io::Result<NegotiationResult> {
    let NegotiationConfig {
        do_negotiation,
        send_compression,
        is_daemon_mode,
        is_server,
        checksum_override,
        compression_override,
        compression_level,
    } = *config;
    // Protocol < 30 doesn't support negotiation, use defaults
    if protocol.uses_fixed_encoding() {
        // When user forced a checksum on a legacy protocol, honour it directly
        // since there is no wire negotiation to perform.
        let checksum = checksum_override.unwrap_or(ChecksumAlgorithm::MD4);
        // upstream: compat.c:194-195 - legacy protocols always use zlib
        // unless the user explicitly chose an algorithm.
        let compression = compression_override.unwrap_or(CompressionAlgorithm::Zlib);
        debug_log!(
            Proto,
            1,
            "protocol {} uses legacy encoding, using checksum={} compression={}",
            protocol.as_u8(),
            checksum.as_str(),
            compression.as_str()
        );
        return Ok(NegotiationResult {
            checksum,
            compression,
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
        // upstream: compat.c:194 - when -z is active but no vstring negotiation,
        // parse_compress_choice() defaults to CPRES_ZLIB.
        let checksum = checksum_override.unwrap_or(ChecksumAlgorithm::MD5);
        // upstream: compat.c:194 - when no vstring negotiation and no explicit
        // choice, default to zlib. When compression_override is set, use it.
        let compression = compression_override.unwrap_or(if send_compression {
            CompressionAlgorithm::Zlib
        } else {
            CompressionAlgorithm::None
        });
        debug_log!(
            Proto,
            1,
            "client lacks VARINT_FLIST_FLAGS, using checksum={} compression={}",
            checksum.as_str(),
            compression.as_str()
        );

        // upstream: compat.c:819-820 - setup_protocol() still calls
        // parse_checksum_choice(1)/parse_compress_choice(1) after
        // negotiate_the_strings() even when do_negotiated_strings == 0. Both
        // emit their NSTR summary with no " negotiated" qualifier
        // (valid_*.negotiated_nni stays NULL because no vstring exchange ran),
        // so --debug=NSTR shows the fallback algorithms the wire actually uses
        // rather than nothing. The level is resolved via init_compression_level
        // (token.c:55), so the raw CLVL_NOT_SPECIFIED sentinel is never printed.
        let side = if is_server {
            NstrSide::Server
        } else {
            NstrSide::Client
        };
        trace_checksum_summary(side, false, checksum.as_str());
        if compression != CompressionAlgorithm::None {
            trace_compress_summary(
                side,
                false,
                compression.as_str(),
                resolved_compress_level(compression, compression_level),
            );
        }

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

    // upstream: compat.c:521-525 send_negotiate_str and compat.c:373-378
    // recv_negotiate_str. Both emit "<side> <type> list (on <local>): <list>"
    // at DEBUG_GTE(NSTR, am_server?3:2); see crates/protocol/src/nstr/trace.rs
    // for the byte-for-byte helpers.
    let side = if is_server {
        NstrSide::Server
    } else {
        NstrSide::Client
    };

    // Send our supported algorithm lists. upstream: compat.c:541-544 - the
    // checksum vstring is sent only when `!checksum_choice`. When
    // --checksum-choice forces the algorithm, the exchange is skipped
    // entirely: `send_negotiate_str` is not called, so `valid_checksums.saw`
    // stays NULL and the matching recv (compat.c:547) is also skipped. The
    // peer forwards the same --checksum-choice, so both sides skip
    // symmetrically. Sending a single-entry list here would desync against an
    // upstream peer that sends nothing.
    let send_checksum = checksum_override.is_none();
    if send_checksum {
        let checksum_list = advertised_list(SUPPORTED_CHECKSUMS.iter().copied(), is_server);
        trace_send_list(side, NstrCategory::Checksum, &checksum_list);
        write_vstring(stdout, &checksum_list)?;
    }

    if send_compression {
        let compression_list = advertised_list(supported_compressions(), is_server);
        trace_send_list(side, NstrCategory::Compress, &compression_list);
        write_vstring(stdout, &compression_list)?;
    }

    stdout.flush()?;

    // Read the remote side's algorithm lists. upstream: compat.c:547 - the
    // checksum recv is gated on `valid_checksums.saw`, which is only set by the
    // send above, so it mirrors the send gating exactly. compat.c:373-378
    // recv_negotiate_str emits "<remote> <type> list (on <local>): <list>".
    let remote_checksum_list = if send_checksum {
        let list = read_vstring(stdin)?;
        trace_recv_list(side, NstrCategory::Checksum, &list);
        Some(list)
    } else {
        None
    };

    let remote_compression_list = if send_compression {
        let list = read_vstring(stdin)?;
        trace_recv_list(side, NstrCategory::Compress, &list);
        Some(list)
    } else {
        None
    };

    // upstream: compat.c:528-529 - "Each side sends their list of valid names
    // to the other side and then both sides pick the first name in the client's
    // list that is also in the server's list."
    //
    // Server iterates client's list (remote), picks first match in local list.
    // Client iterates local list, picks first item also in server's list (remote).
    // Both sides converge on the same result: first client item in server's list.
    //
    // When the user forced a checksum via --checksum-choice, upstream's
    // parse_checksum_choice resolves the name directly (checksum.c:178-184) with
    // no wire exchange, so use it unconditionally.
    let checksum = match checksum_override {
        Some(forced) => forced,
        None => {
            let list = remote_checksum_list.as_deref().unwrap_or("");
            choose_checksum_algorithm(list, is_server)?
        }
    };

    // upstream: compat.c:819 parse_compress_choice(1) - when the user
    // specified --compress-choice, the vstring exchange was skipped and the
    // override is used directly. When no override, use the negotiated list.
    let compression = if let Some(forced) = compression_override {
        forced
    } else if let Some(ref list) = remote_compression_list {
        choose_compression_algorithm(list, is_server)?
    } else {
        CompressionAlgorithm::None
    };

    // upstream: checksum.c:206-211 parse_checksum_choice -
    //   "%s%s checksum: %s\n" at DEBUG_GTE(NSTR, am_server?3:1).
    // The " negotiated" qualifier fires iff valid_checksums.negotiated_nni
    // is set; --checksum-choice forces selection without negotiation
    // (compat.c:175-187), in which case the qualifier renders blank.
    let checksum_negotiated = checksum_override.is_none();
    trace_checksum_summary(side, checksum_negotiated, checksum.as_str());

    // upstream: compat.c:213-219 parse_compress_choice -
    //   "%s%s compress: %s (level %d)\n" at DEBUG_GTE(NSTR, am_server?3:1).
    // The (level <N>) clause always renders. Upstream calls
    // init_compression_level() (token.c:55) inside parse_compress_choice(1)
    // BEFORE this print, which resolves do_compression_level from
    // CLVL_NOT_SPECIFIED (INT_MIN) to the codec's def_level, so the raw
    // sentinel is never printed. Mirror that resolution here. The whole
    // emission is gated on `do_compression != CPRES_NONE || level !=
    // CLVL_NOT_SPECIFIED`; here compression is active so it always renders.
    if compression != CompressionAlgorithm::None {
        let compression_negotiated =
            compression_override.is_none() && remote_compression_list.is_some();
        trace_compress_summary(
            side,
            compression_negotiated,
            compression.as_str(),
            resolved_compress_level(compression, compression_level),
        );
    }
    Ok(NegotiationResult {
        checksum,
        compression,
    })
}

/// Resolves the compression level upstream renders in the NSTR compress
/// summary, mirroring `token.c:55 init_compression_level()`.
///
/// Maps the wire algorithm to the compress crate's enum and delegates to
/// `CompressionAlgorithm::resolve_debug_level` - the single source of truth for
/// the CLVL_NOT_SPECIFIED-to-def_level substitution and per-codec clamp shared
/// with the local-copy print path. Falls back to the raw value if the codec
/// cannot be mapped, which never happens for an active, build-supported codec.
fn resolved_compress_level(compression: CompressionAlgorithm, raw_level: i32) -> i32 {
    match compression.to_compress_algorithm() {
        Ok(Some(algorithm)) => algorithm.resolve_debug_level(raw_level),
        _ => raw_level,
    }
}

/// Chooses a checksum algorithm using upstream rsync's precedence rules.
///
/// upstream: compat.c:332-363 `parse_negotiate_str()` - both sides converge on
/// the first entry in the client's list that also appears in the server's list.
///
/// - Server (`is_server=true`): iterates the remote (client's) list, returns the
///   first entry that appears in our local list. This is the server-side fast path
///   where the server breaks on first acceptable client choice.
/// - Client (`is_server=false`): iterates our local list, returns the first entry
///   that also appears in the remote (server's) list. This finds the best local
///   preference among mutually supported algorithms.
///
/// Returns an error if no common algorithm is found - upstream rsync treats
/// this as a hard failure (compat.c:383-406 `recv_negotiate_str`).
pub(super) fn choose_checksum_algorithm(
    remote_list: &str,
    is_server: bool,
) -> io::Result<ChecksumAlgorithm> {
    let remote_items: Vec<&str> = remote_list.split_whitespace().collect();

    if is_server {
        // Server: iterate client's (remote) list, first match in our list wins.
        // upstream: compat.c:353 `if (best == 1 || am_server) break;`
        for algo in &remote_items {
            if let Ok(checksum) = ChecksumAlgorithm::parse(algo) {
                if SUPPORTED_CHECKSUMS.contains(&checksum.as_str()) {
                    return Ok(checksum);
                }
            }
        }
    } else {
        // Client: iterate our local list, first item also in server's (remote)
        // list wins. This gives client preference order priority.
        // upstream: compat.c:349-354 - client continues iterating to find the
        // local item with the best (lowest) position in our own list.
        for &local in SUPPORTED_CHECKSUMS {
            if remote_items.contains(&local) {
                return ChecksumAlgorithm::parse(local)
                    .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()));
            }
        }
    }

    // upstream: compat.c:383-406 - failure to negotiate is a hard error
    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        format!("failed to negotiate a common checksum algorithm (remote offers: {remote_list})"),
    ))
}

/// Chooses a compression algorithm using upstream rsync's precedence rules.
///
/// Same asymmetric logic as [`choose_checksum_algorithm`] - both sides converge
/// on the first entry in the client's list that also appears in the server's list.
///
/// upstream: compat.c:332-363 `parse_negotiate_str()`
pub(super) fn choose_compression_algorithm(
    remote_list: &str,
    is_server: bool,
) -> io::Result<CompressionAlgorithm> {
    let supported = supported_compressions();
    let remote_items: Vec<&str> = remote_list.split_whitespace().collect();

    if is_server {
        // Server: iterate client's (remote) list, first match in our list wins.
        for algo in &remote_items {
            if let Ok(compression) = CompressionAlgorithm::parse(algo) {
                if supported.contains(algo) {
                    return Ok(compression);
                }
            }
        }
    } else {
        // Client: iterate our local list, first item also in server's (remote)
        // list wins.
        for &local in &supported {
            if remote_items.contains(&local) {
                return CompressionAlgorithm::parse(local)
                    .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()));
            }
        }
    }

    // No common algorithm found - use "none"
    Ok(CompressionAlgorithm::None)
}

/// Builds the space-separated algorithm list a peer advertises during
/// `negotiate_the_strings()`.
///
/// Mirrors upstream `get_default_nno_list()` (compat.c:462-504): names are
/// joined with a single space with no leading or trailing space, and the
/// client (`is_server == false`) omits the `none` entry. Upstream skips the
/// zero-numbered item on the client with
/// `if (nni->num == 0 && !am_server && !dup_markup) continue;`
/// (compat.c:485-486); both `CSUM_NONE` and `CPRES_NONE` are `0`
/// (lib/md-defines.h:26, rsync.h:1177), so `none` is the entry dropped. The
/// server still advertises `none`. This keeps the emitted vstring byte-for-byte
/// identical to upstream, which matters because a mismatched client list
/// changes the wire bytes (length prefix plus payload) exchanged at
/// protocol >= 30.
fn advertised_list<'a>(names: impl IntoIterator<Item = &'a str>, is_server: bool) -> String {
    let mut out = String::new();
    for name in names {
        // upstream: compat.c:485-486 - the client drops the num == 0 ("none")
        // entry; the server keeps it.
        if !is_server && name == "none" {
            continue;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(name);
    }
    out
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
/// Format (upstream io.c:2004-2021 read_vstring):
/// - Read first byte
/// - If high bit set: len = (first & 0x7F) * 256 + read_another_byte
/// - Otherwise: len = first byte
/// - Then read `len` bytes of string data
///
/// This is DIFFERENT from varint encoding!
///
/// # Length limit
///
/// Upstream `recv_negotiate_str` (compat.c:369-407) always calls
/// `read_vstring(f_in, tmpbuf, MAX_NSTR_STRLEN)` with `bufsize` equal to
/// `MAX_NSTR_STRLEN` (256, compat.c:99). `read_vstring` rejects the frame with
/// `if (len >= bufsize)` because it appends a NUL terminator at `buf[len]`, so
/// the largest payload a real peer accepts is `MAX_NSTR_STRLEN - 1 == 255`
/// bytes. A 256-byte list is refused (`RERR_UNSUPPORTED`). Mirroring the
/// `>=`/`-1` boundary exactly keeps both peers agreeing on which negotiation
/// streams are valid; a `>` comparison here would let oc-rsync accept a
/// 256-byte list that upstream aborts on, desyncing the negotiation.
pub(super) fn read_vstring(reader: &mut dyn Read) -> io::Result<String> {
    // upstream: compat.c:99 #define MAX_NSTR_STRLEN 256; the caller's bufsize.
    const MAX_NSTR_STRLEN: usize = 256;
    // upstream: io.c:2011 `if (len >= bufsize)` - one byte is reserved for the
    // NUL terminator, so the accepted data length maxes out at bufsize - 1.
    const MAX_VSTRING_LEN: usize = MAX_NSTR_STRLEN - 1;

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

    // upstream: io.c:2011-2015 - reject `len >= bufsize` (over-long vstring).
    if len > MAX_VSTRING_LEN {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("vstring too long: {len} bytes (max {MAX_VSTRING_LEN})"),
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
