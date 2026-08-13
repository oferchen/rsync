use std::io::{self, Read, Write};

use logging::debug_log;

use crate::ProtocolVersion;
use crate::nstr::{
    CLVL_NOT_SPECIFIED, NstrCategory, NstrSide, trace_checksum_summary, trace_compress_summary,
    trace_recv_list, trace_send_list,
};

use super::algorithms::{
    ChecksumAlgorithm, CompressionAlgorithm, SUPPORTED_CHECKSUMS, resolve_checksum_name,
    resolve_compression_name, supported_compressions,
};
use super::env_list;

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
    /// Whether this process is recording a `--write-batch` file.
    ///
    /// Upstream pins both negotiation lists to their old-style choice while
    /// `write_batch` is set, because a `--read-batch` replay never negotiates:
    /// it has only the batch header, which records no algorithm. Advertising
    /// the full default order would let a modern peer pick XXH3, leaving a
    /// batch whose whole-file checksums no `--read-batch` can verify.
    ///
    /// upstream: `compat.c:412-414 getenv_nstr()`.
    pub write_batch: bool,
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
/// - No mutual algorithm in the peer's list: `Unsupported` error, matching
///   upstream's `RERR_UNSUPPORTED` abort (compat.c:406); unknown names inside
///   the list are skipped, never an error
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
            write_batch: false,
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
        write_batch,
    } = *config;
    // Protocol < 30 doesn't support negotiation, use defaults
    if protocol.uses_fixed_encoding() {
        // upstream: setup_protocol calls parse_*_choice regardless of protocol
        // version, so the server still validates a forced choice against the env
        // list. No vstring exchange runs here, so ordering is irrelevant.
        validate_forced_choices(is_server, checksum_override, compression_override)?;
        // upstream: negotiate_the_strings still runs for legacy protocols; the
        // prefilled default is "md4" (compat.c:553) here.
        validate_fallback_defaults(
            is_server,
            ChecksumAlgorithm::MD4,
            send_compression,
            checksum_override,
            compression_override,
        )?;
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
        // upstream: setup_protocol still calls parse_*_choice when
        // do_negotiated_strings == 0, so the server validates a forced choice
        // against the env list. No vstring exchange runs, so ordering is moot.
        validate_forced_choices(is_server, checksum_override, compression_override)?;
        // upstream: negotiate_the_strings runs even when do_negotiated_strings
        // == 0 - send_negotiate_str populates the env-restricted saw list and
        // recv_negotiate_str validates the prefilled "md5" default against it
        // (compat.c:550-554), aborting with RERR_UNSUPPORTED when the operator's
        // env excludes it. Without a forced choice this is the only env check.
        validate_fallback_defaults(
            is_server,
            ChecksumAlgorithm::MD5,
            send_compression,
            checksum_override,
            compression_override,
        )?;
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

    // upstream: compat.c:506-533 send_negotiate_str - RSYNC_CHECKSUM_LIST /
    // RSYNC_COMPRESS_LIST, when set, replace the advertised default order and the
    // local candidate list used for selection; an empty value falls through to
    // get_default_nno_list() so the default wire bytes are unchanged.
    let checksum_env = if send_checksum {
        env_list::checksum_candidates(is_server, write_batch, protocol.as_u8())
    } else {
        None
    };
    let compression_env = if send_compression {
        env_list::compression_candidates(is_server, write_batch)
    } else {
        None
    };

    if send_checksum {
        let checksum_list = match &checksum_env {
            Some(env) => env.advertised.clone(),
            None => advertised_list(SUPPORTED_CHECKSUMS.iter().copied(), is_server),
        };
        trace_send_list(side, NstrCategory::Checksum, &checksum_list);
        write_vstring(stdout, &checksum_list)?;
    }

    if send_compression {
        let compression_list = match &compression_env {
            Some(env) => env.advertised.clone(),
            None => advertised_list(supported_compressions(), is_server),
        };
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

    // upstream: setup_protocol runs parse_checksum_choice/parse_compress_choice
    // AFTER negotiate_the_strings completes, so the server validates a forced
    // choice against the env list only once the full vstring exchange (any
    // non-forced category) has been sent and received. Validating here - after
    // the recv above - preserves that wire ordering: a refusal does not truncate
    // the exchange the peer expects.
    validate_forced_choices(is_server, checksum_override, compression_override)?;

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
            // upstream: compat.c:350 - selection matches remote names against our
            // local `saw` list, which the env override reorders/restricts. With
            // no override, the default candidates drop `none` on the client
            // (compat.c:485-486).
            let default_candidates;
            let candidates: &[&str] = match &checksum_env {
                Some(env) => &env.candidates,
                None => {
                    default_candidates = default_selection_candidates(
                        SUPPORTED_CHECKSUMS.iter().copied(),
                        is_server,
                    );
                    &default_candidates
                }
            };
            choose_checksum_algorithm_in(list, is_server, candidates)?
        }
    };

    // upstream: compat.c:819 parse_compress_choice(1) - when the user
    // specified --compress-choice, the vstring exchange was skipped and the
    // override is used directly. When no override, use the negotiated list.
    let compression = if let Some(forced) = compression_override {
        forced
    } else if let Some(ref list) = remote_compression_list {
        let supported = supported_compressions();
        let default_candidates;
        let candidates: &[&str] = match &compression_env {
            Some(env) => &env.candidates,
            None => {
                default_candidates =
                    default_selection_candidates(supported.iter().copied(), is_server);
                &default_candidates
            }
        };
        choose_compression_algorithm_in(list, is_server, candidates)?
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

/// Refuses client-forced `--checksum-choice`/`--compress-choice` values that
/// the server's `RSYNC_CHECKSUM_LIST`/`RSYNC_COMPRESS_LIST` excludes.
///
/// upstream: `checksum.c:185-186` and `compat.c:193-194` - `parse_*_choice`
/// call `validate_choice_vs_env()` (`compat.c:426-449`) only on the server and
/// only for an explicitly forced choice. When neither env variable is set this
/// is a no-op, so the default path is unchanged. Called at every point where a
/// forced choice is finalised (legacy, no-negotiation and the main negotiated
/// path) so the check is version- and path-independent, exactly as upstream
/// validates inside `setup_protocol` regardless of `do_negotiated_strings`.
fn validate_forced_choices(
    is_server: bool,
    checksum_override: Option<ChecksumAlgorithm>,
    compression_override: Option<CompressionAlgorithm>,
) -> io::Result<()> {
    if !is_server {
        return Ok(());
    }
    if let Some(forced) = checksum_override {
        env_list::validate_checksum_choice(forced.as_str())?;
    }
    if let Some(forced) = compression_override {
        env_list::validate_compress_choice(forced.as_str())?;
    }
    Ok(())
}

/// Refuses the forced fallback default checksum/compression on the
/// non-negotiated path when `RSYNC_CHECKSUM_LIST`/`RSYNC_COMPRESS_LIST` excludes
/// it.
///
/// upstream: `compat.c:541-565 negotiate_the_strings` - when the peer cannot
/// negotiate (legacy protocol or missing 'v' capability), `send_negotiate_str`
/// still parses the env list into `nno->saw` for each category that was not
/// forced, then `recv_negotiate_str` validates the prefilled default against it:
/// `"md5"`/`"md4"` for the checksum (`compat.c:553`) and `"zlib"` for
/// compression (`compat.c:563`). A default absent from the operator's env list
/// aborts with `RERR_UNSUPPORTED`. This runs on both peers (the `&` split
/// selects the caller's half via `is_server`), and only for a category with no
/// forced override - a forced choice skips `send_negotiate_str`, leaving `saw`
/// NULL so `recv_negotiate_str` is not called; [`validate_forced_choices`]
/// covers that case instead. Compression is gated on `send_compression`,
/// matching upstream's `do_compression` guard at `compat.c:544`.
///
/// A strict no-op when neither variable is set: [`env_list`] returns early,
/// leaving the common (default) path unchanged.
fn validate_fallback_defaults(
    is_server: bool,
    checksum_default: ChecksumAlgorithm,
    send_compression: bool,
    checksum_override: Option<ChecksumAlgorithm>,
    compression_override: Option<CompressionAlgorithm>,
) -> io::Result<()> {
    if checksum_override.is_none() {
        env_list::validate_default_checksum(checksum_default.as_str(), is_server)?;
    }
    if send_compression && compression_override.is_none() {
        env_list::validate_default_compress(CompressionAlgorithm::Zlib.as_str(), is_server)?;
    }
    Ok(())
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

/// Selects the mutual algorithm name from the peer's space-separated list.
///
/// Mirrors upstream `parse_negotiate_str()` (compat.c:333-364) token by token:
///
/// - Each remote token is resolved via the case-insensitive, alias-aware
///   lookup (`get_nni_by_name()`, compat.c:223-238); an unrecognised name -
///   e.g. a future `sha256` from a newer peer - is skipped without error
///   (compat.c:350 `if (!nni || !nno->saw[nni->num] || ...) continue;`).
/// - A recognised token must also be in `local` (upstream's `nno->saw`, which
///   the env override reorders/restricts); the token with the lowest `local`
///   ordinal wins, matching upstream's `best` tracking.
/// - The server stops at the first acceptable client choice, the client keeps
///   scanning for its best own-preference match and stops early only on a
///   perfect one (compat.c:354 `if (best == 1 || am_server) break;`).
///
/// Returns `None` when nothing matched, which callers turn into upstream's
/// hard `RERR_UNSUPPORTED` failure (compat.c:369-406 `recv_negotiate_str`).
fn select_from_peer_list<'a>(
    remote_list: &str,
    is_server: bool,
    local: &[&'a str],
    resolve: impl Fn(&str) -> Option<&'static str>,
) -> Option<&'a str> {
    let mut best: Option<usize> = None;
    for token in remote_list.split_whitespace() {
        // upstream: compat.c:350 - unknown names are skipped, never an error.
        let Some(canonical) = resolve(token) else {
            continue;
        };
        let Some(pos) = local.iter().position(|&name| name == canonical) else {
            continue;
        };
        if best.is_none_or(|current| pos < current) {
            best = Some(pos);
            // upstream: compat.c:354 - the server stops at the first
            // acceptable client choice; the client stops only on its own
            // top preference.
            if pos == 0 || is_server {
                break;
            }
        }
    }
    best.map(|pos| local[pos])
}

/// Chooses a checksum algorithm using upstream rsync's precedence rules.
///
/// upstream: compat.c:333-364 `parse_negotiate_str()` - both sides converge on
/// the first entry in the client's list that also appears in the server's list.
///
/// Returns an error if no common algorithm is found - upstream rsync treats
/// this as a hard failure (compat.c:383-406 `recv_negotiate_str`, exiting with
/// `RERR_UNSUPPORTED`).
///
/// This is the default-list convenience wrapper exercised by the test suite;
/// production callers use [`choose_checksum_algorithm_in`] so the
/// `RSYNC_CHECKSUM_LIST` env override can substitute the candidate list.
#[cfg(test)]
pub(super) fn choose_checksum_algorithm(
    remote_list: &str,
    is_server: bool,
) -> io::Result<ChecksumAlgorithm> {
    choose_checksum_algorithm_in(
        remote_list,
        is_server,
        &default_selection_candidates(SUPPORTED_CHECKSUMS.iter().copied(), is_server),
    )
}

/// Chooses a checksum algorithm from an explicit local candidate list.
///
/// Behaves like [`choose_checksum_algorithm`] but takes the ordered local
/// candidate names as a parameter so the caller can substitute the
/// `RSYNC_CHECKSUM_LIST` env override (upstream `nno->saw`, compat.c:350).
pub(super) fn choose_checksum_algorithm_in(
    remote_list: &str,
    is_server: bool,
    local: &[&str],
) -> io::Result<ChecksumAlgorithm> {
    match select_from_peer_list(remote_list, is_server, local, resolve_checksum_name) {
        Some(name) => ChecksumAlgorithm::parse(name)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string())),
        // upstream: compat.c:381-406 recv_negotiate_str - "Failed to negotiate
        // a %s choice." plus the offered Server/Client lists, then
        // exit_cleanup(RERR_UNSUPPORTED). The negotiated path always runs with
        // do_negotiated_strings set, so pass `true`.
        None => Err(super::failure::negotiation_failure(
            "checksum",
            is_server,
            true,
            remote_list,
            local,
        )),
    }
}

/// Chooses a compression algorithm using upstream rsync's precedence rules.
///
/// Same convergence logic as [`choose_checksum_algorithm`] - both sides pick
/// the first entry in the client's list that also appears in the server's list.
///
/// upstream: compat.c:333-364 `parse_negotiate_str()`
///
/// This is the default-list convenience wrapper exercised by the test suite;
/// production callers use [`choose_compression_algorithm_in`] so the
/// `RSYNC_COMPRESS_LIST` env override can substitute the candidate list.
#[cfg(test)]
pub(super) fn choose_compression_algorithm(
    remote_list: &str,
    is_server: bool,
) -> io::Result<CompressionAlgorithm> {
    let supported = supported_compressions();
    choose_compression_algorithm_in(
        remote_list,
        is_server,
        &default_selection_candidates(supported.iter().copied(), is_server),
    )
}

/// Chooses a compression algorithm from an explicit local candidate list.
///
/// Behaves like [`choose_compression_algorithm`] but takes the ordered local
/// candidate names as a parameter so the caller can substitute the
/// `RSYNC_COMPRESS_LIST` env override.
pub(super) fn choose_compression_algorithm_in(
    remote_list: &str,
    is_server: bool,
    local: &[&str],
) -> io::Result<CompressionAlgorithm> {
    match select_from_peer_list(remote_list, is_server, local, resolve_compression_name) {
        Some(name) => CompressionAlgorithm::parse(name)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string())),
        // upstream: compat.c:381-406 recv_negotiate_str - a compression list
        // with no mutual algorithm is the same hard RERR_UNSUPPORTED failure
        // as an unmatched checksum list, with the same Server/Client detail
        // lines; "none" is never a silent fallback.
        None => Err(super::failure::negotiation_failure(
            "compress",
            is_server,
            true,
            remote_list,
            local,
        )),
    }
}

/// Builds the default local selection candidates for one side.
///
/// upstream: compat.c:485-486 `get_default_nno_list()` skips the
/// zero-numbered entry (`none` for both checksums and compressions) on the
/// client, so a client can neither advertise nor accept `none` unless the
/// operator re-enables it through `RSYNC_*_LIST`; the server keeps it. The
/// `saw` array that `parse_negotiate_str()` consults is populated by that same
/// loop, so the selection candidates must apply the identical filter.
fn default_selection_candidates<'a>(
    names: impl IntoIterator<Item = &'a str>,
    is_server: bool,
) -> Vec<&'a str> {
    names
        .into_iter()
        .filter(|&name| is_server || name != "none")
        .collect()
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
/// Format (upstream io.c:2297-2315 write_vstring):
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
    // upstream: io.c:2181 `if (len >= bufsize)` - one byte is reserved for the
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

    // upstream: io.c:2181-2185 - reject `len >= bufsize` (over-long vstring).
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
