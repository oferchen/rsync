//! Capability string building and parsing.
//!
//! Handles the `-e.xxx` capability string that rsync uses to advertise
//! supported features during connection setup. Mirrors upstream
//! `options.c:maybe_add_e_option()` and `compat.c:712-734`.

use protocol::CompatibilityFlags;
use std::borrow::Cow;

/// Capability mapping entry for table-driven flag parsing.
///
/// Each entry maps a client capability character to a compatibility flag,
/// with optional platform-specific and conditional requirements.
pub(crate) struct CapabilityMapping {
    /// Character advertised by client in -e option
    pub(crate) char: char,
    /// Corresponding compatibility flag
    pub(crate) flag: CompatibilityFlags,
    /// Platform-specific requirement (None = all platforms)
    #[cfg(unix)]
    pub(crate) platform_ok: bool,
    #[cfg(not(unix))]
    pub(crate) platform_ok: bool,
    /// Whether this capability requires allow_inc_recurse to be true
    pub(crate) requires_inc_recurse: bool,
    /// Whether this capability requires the iconv feature to be compiled in.
    ///
    /// Mirrors upstream `#ifdef ICONV_OPTION` gating (compat.c:716-718) for
    /// CF_SYMLINK_ICONV. The runtime caller must skip mappings whose
    /// `requires_iconv` is true when the `iconv` cargo feature is disabled,
    /// otherwise the peer will run `sender_symlink_iconv` against a stream
    /// that has no transcoding hooks attached.
    pub(crate) requires_iconv: bool,
}

/// Table-driven capability to flag mappings.
///
/// This mirrors upstream compat.c:712-734 in a maintainable format.
/// Order matches upstream rsync for documentation consistency.
pub(crate) const CAPABILITY_MAPPINGS: &[CapabilityMapping] = &[
    // INC_RECURSE: 'i' - requires allow_inc_recurse
    CapabilityMapping {
        char: 'i',
        flag: CompatibilityFlags::INC_RECURSE,
        platform_ok: true,
        requires_inc_recurse: true,
        requires_iconv: false,
    },
    // SYMLINK_TIMES: 'L' - Unix only (CAN_SET_SYMLINK_TIMES)
    CapabilityMapping {
        char: 'L',
        flag: CompatibilityFlags::SYMLINK_TIMES,
        #[cfg(unix)]
        platform_ok: true,
        #[cfg(not(unix))]
        platform_ok: false,
        requires_inc_recurse: false,
        requires_iconv: false,
    },
    // SYMLINK_ICONV: 's' - gated on the `iconv` cargo feature, mirroring
    // upstream's `#ifdef ICONV_OPTION` (compat.c:716-718). Without the
    // feature the runtime filter in `iconv_capability_compiled_in()` skips
    // this row so we neither advertise CF_SYMLINK_ICONV in `-e.<...>` nor
    // accept it from the peer.
    CapabilityMapping {
        char: 's',
        flag: CompatibilityFlags::SYMLINK_ICONV,
        platform_ok: true,
        requires_inc_recurse: false,
        requires_iconv: true,
    },
    CapabilityMapping {
        char: 'f',
        flag: CompatibilityFlags::SAFE_FILE_LIST,
        platform_ok: true,
        requires_inc_recurse: false,
        requires_iconv: false,
    },
    // AVOID_XATTR_OPTIMIZATION: 'x' - disables xattr hardlink optimization
    CapabilityMapping {
        char: 'x',
        flag: CompatibilityFlags::AVOID_XATTR_OPTIMIZATION,
        platform_ok: true,
        requires_inc_recurse: false,
        requires_iconv: false,
    },
    // CHECKSUM_SEED_FIX: 'C' - proper seed ordering for MD5
    CapabilityMapping {
        char: 'C',
        flag: CompatibilityFlags::CHECKSUM_SEED_FIX,
        platform_ok: true,
        requires_inc_recurse: false,
        requires_iconv: false,
    },
    // INPLACE_PARTIAL_DIR: 'I' - --inplace behavior for partial dir
    CapabilityMapping {
        char: 'I',
        flag: CompatibilityFlags::INPLACE_PARTIAL_DIR,
        platform_ok: true,
        requires_inc_recurse: false,
        requires_iconv: false,
    },
    CapabilityMapping {
        char: 'v',
        flag: CompatibilityFlags::VARINT_FLIST_FLAGS,
        platform_ok: true,
        requires_inc_recurse: false,
        requires_iconv: false,
    },
    // ID0_NAMES: 'u' - include uid/gid 0 names
    CapabilityMapping {
        char: 'u',
        flag: CompatibilityFlags::ID0_NAMES,
        platform_ok: true,
        requires_inc_recurse: false,
        requires_iconv: false,
    },
];

/// Private oc-to-oc capability letter advertised in the `-e.<...>` string when
/// the consecutive-match extension is opted in.
///
/// This letter is NOT an upstream rsync capability. Upstream ignores unknown
/// capability letters in `client_info` (it only `strchr`s for its own known
/// set, compat.c:712-732), so advertising it to a stock peer is inert. It is
/// the wire channel that carries "this oc client opted in": the private
/// [`CompatibilityFlags::CONSECUTIVE_MATCH`] bit is only ever set when the oc
/// server sees this letter AND is itself opted in.
const CONSECUTIVE_MATCH_CHAR: char = 'Z';

/// Returns whether this process opted in to the consecutive-match extension.
///
/// The opt-in is a default-off internal switch driven by the
/// `OC_CONSECUTIVE_MATCH=1` environment variable. It is deliberately NOT a
/// default-advertised capability: both peers must set it (and both must be oc)
/// for the negotiation AND to retain
/// [`CompatibilityFlags::CONSECUTIVE_MATCH`]. Against any upstream rsync, or any
/// oc peer without the opt-in, the extension stays fully inert and the wire is
/// byte-identical to upstream.
#[must_use]
fn consecutive_match_opt_in() -> bool {
    std::env::var_os("OC_CONSECUTIVE_MATCH").is_some_and(|value| value == "1" || value == "true")
}

/// Returns whether the iconv capability ('s' / CF_SYMLINK_ICONV) is
/// compiled into this build.
///
/// Mirrors upstream's `#ifdef ICONV_OPTION` (compat.c:716-718) which
/// gates the advertisement and recognition of CF_SYMLINK_ICONV on
/// build-time iconv availability.
#[inline]
const fn iconv_capability_compiled_in() -> bool {
    cfg!(feature = "iconv")
}

/// Builds the `-e.xxx` capability string from the `CAPABILITY_MAPPINGS` table.
///
/// This is the single source of truth for which capability characters we
/// advertise. Used when the capability string must be a standalone argument
/// (e.g., daemon text protocol where args are newline-separated).
///
/// Mirrors upstream `options.c:3003-3050 maybe_add_e_option()`.
pub fn build_capability_string(allow_inc_recurse: bool) -> String {
    let mut result = String::from("-e.");
    append_capability_chars(&mut result, allow_inc_recurse);
    result
}

/// Builds the `e.xxx` capability suffix for embedding into a compact flag string.
///
/// Upstream `options.c:2710` appends the capability characters directly into
/// the same argstr buffer that holds the transfer flags, producing a single
/// argument like `-logDtprHve.iLsfxCIvu`. This function returns the `e.xxx`
/// portion without the leading `-` so callers can concatenate it with their
/// flag string.
///
/// Mirrors upstream `options.c:3003-3050 maybe_add_e_option()`.
pub fn build_capability_string_suffix(allow_inc_recurse: bool) -> String {
    let mut result = String::from("e.");
    append_capability_chars(&mut result, allow_inc_recurse);
    result
}

/// Appends capability characters to the given buffer.
///
/// Shared by both `build_capability_string` (standalone `-e.xxx`) and
/// `build_capability_string_suffix` (embeddable `e.xxx`).
fn append_capability_chars(buf: &mut String, allow_inc_recurse: bool) {
    for mapping in CAPABILITY_MAPPINGS {
        if !mapping.platform_ok {
            continue;
        }
        if mapping.requires_inc_recurse && !allow_inc_recurse {
            continue;
        }
        if mapping.requires_iconv && !iconv_capability_compiled_in() {
            continue;
        }
        buf.push(mapping.char);
    }

    // Private oc extension: advertise the consecutive-match capability letter
    // only when this process opted in. Upstream peers ignore the unknown
    // letter, so this is inert against stock rsync.
    if consecutive_match_opt_in() {
        buf.push(CONSECUTIVE_MATCH_CHAR);
    }
}

/// Parses client capabilities from the `-e` option argument.
///
/// The `-e` option contains a string like "efxCIvu" where each letter indicates
/// a capability the client supports. This mirrors upstream's `client_info` string
/// parsing in compat.c:712-732.
///
/// # Capability Letters
/// - 'i' - incremental recurse
/// - 'L' - symlink time-setting support
/// - 's' - symlink iconv translation support
/// - 'f' - flist I/O-error safety support
/// - 'x' - xattr hardlink optimization not desired
/// - 'C' - checksum seed order fix
/// - 'I' - inplace_partial behavior
/// - 'v' - varint for flist & compat flags
/// - 'V' - deprecated pre-release varint (implies 'v', uses `write_byte` encoding)
/// - 'u' - include name of uid 0 & gid 0
///
/// # Arguments
/// * `client_args` - Arguments received from client (e.g., ["-e", "efxCIvu", "--server", ...])
///
/// # Returns
/// The capability string (e.g., "fxCIvu") with the leading 'e' removed, or empty string if not found.
///
/// # Examples
/// - `["-e", "fxCIvu"]` -> "fxCIvu"
/// - `["-efxCIvu"]` -> "fxCIvu"
/// - `["-vvde.LsfxCIvu"]` -> ".LsfxCIvu" (combined short options)
pub(crate) fn parse_client_info(client_args: &[String]) -> Cow<'_, str> {
    for i in 0..client_args.len() {
        let arg = &client_args[i];

        // Combined short options like "-vvde.LsfxCIvu" embed -e in the middle.
        if arg.starts_with('-')
            && !arg.starts_with("--")
            && let Some(e_pos) = arg.find('e')
            && e_pos + 1 < arg.len()
        {
            let caps = &arg[e_pos + 1..];
            // Skip leading '.' version placeholder.
            // upstream: options.c puts '.' when protocol_version != PROTOCOL_VERSION
            if caps.starts_with('.') && caps.len() > 1 {
                return Cow::Borrowed(&caps[1..]);
            }
            return Cow::Borrowed(caps);
        }

        if arg == "-e" && i + 1 < client_args.len() {
            return Cow::Borrowed(&client_args[i + 1]);
        }
    }

    Cow::Borrowed("")
}

/// Extracts a peer's advertised `(protocol, subprotocol)` from the `-e`
/// capability payload embedded in a compact server flag string, mirroring
/// upstream `check_sub_protocol`'s parse of `client_info` (compat.c:139-148).
///
/// Upstream's `client_info` is the raw value of the client's `-e` option
/// (`client_info = shell_cmd`, compat.c:163-164). A pre-release client
/// negotiating the newest protocol emits `-e<proto>.<sub><caps>`
/// (options.c:3031-3036), e.g. `-e32.7LsfxCIvu`; a release client emits
/// `-e.<caps>`, e.g. `-e.LsfxCIvu`. `check_sub_protocol()` then computes
/// `their_protocol = atoi(client_info)` and, when a `.` follows,
/// `their_sub = atoi(dot+1)`. The leading `.` of a release payload makes the
/// first `atoi` return `0`, folding into the "no pre-release info" branch.
///
/// Returns `(0, 0)` when there is no usable VER.SUB - a release peer, no `-e`
/// payload, or a value whose protocol or subprotocol parses as zero - so the
/// caller's `check_sub_protocol` is a no-op against every stock release peer.
pub(crate) fn parse_peer_subprotocol(flag_string: &str) -> (u8, u8) {
    let Some(info) = e_capability_payload(flag_string) else {
        return (0, 0);
    };
    // upstream: compat.c:140 `atoi(client_info)` - a leading '.' (release peer)
    // or any non-digit start yields 0.
    let their_protocol = leading_atoi(info);
    if their_protocol == 0 {
        return (0, 0);
    }
    // upstream: compat.c:141 `strchr(client_info, '.')` then `atoi(dot+1)`.
    let Some(dot) = info.find('.') else {
        return (0, 0);
    };
    let their_sub = leading_atoi(&info[dot + 1..]);
    if their_sub == 0 {
        return (0, 0);
    }
    (their_protocol, their_sub)
}

/// Returns the raw capability payload following the `-e` option letter in a
/// compact flag string (everything after the first `e` in a `-`-prefixed,
/// non-`--` token), WITHOUT stripping the leading VER.SUB placeholder.
///
/// This is upstream's `client_info` string. Unlike [`parse_client_info`], the
/// leading `.`/`<proto>.<sub>` prefix is preserved so the subprotocol can be
/// read by [`parse_peer_subprotocol`].
fn e_capability_payload(flag_string: &str) -> Option<&str> {
    for token in flag_string.split_whitespace() {
        if token.starts_with('-')
            && !token.starts_with("--")
            && let Some(e_pos) = token.find('e')
            && e_pos + 1 < token.len()
        {
            return Some(&token[e_pos + 1..]);
        }
    }
    None
}

/// Mirrors C `atoi` for the leading run of ASCII digits, saturating at
/// `u8::MAX`. A non-digit prefix (e.g. the release `.` placeholder) yields 0.
fn leading_atoi(s: &str) -> u8 {
    let mut value: u32 = 0;
    for byte in s.bytes() {
        if !byte.is_ascii_digit() {
            break;
        }
        value = value
            .saturating_mul(10)
            .saturating_add(u32::from(byte - b'0'));
    }
    value.min(u32::from(u8::MAX)) as u8
}

/// Builds compatibility flags based on client capabilities.
///
/// Uses table-driven approach for maintainability. This mirrors upstream
/// compat.c:712-734 which checks the client_info string to determine
/// which flags to enable.
///
/// # Arguments
/// * `client_info` - Capability string from client's `-e` option (e.g., "fxCIvu")
/// * `allow_inc_recurse` - Whether incremental recursion is allowed
///
/// # Returns
/// CompatibilityFlags with only the capabilities the client advertised
pub(crate) fn build_compat_flags_from_client_info(
    client_info: &str,
    allow_inc_recurse: bool,
) -> CompatibilityFlags {
    let mut flags = CompatibilityFlags::from_bits(0);

    for mapping in CAPABILITY_MAPPINGS {
        if !mapping.platform_ok {
            continue;
        }

        if mapping.requires_inc_recurse && !allow_inc_recurse {
            continue;
        }

        // Mirror upstream's `#ifdef ICONV_OPTION` so we never set
        // CF_SYMLINK_ICONV for a peer who advertises 's' if we cannot
        // transcode our own stream.
        if mapping.requires_iconv && !iconv_capability_compiled_in() {
            continue;
        }

        if client_info.contains(mapping.char) {
            flags |= mapping.flag;
        }
    }

    // Private oc extension (CAP_CONSECUTIVE_MATCH): set the bit only when BOTH
    // sides opted in - the client advertised the letter AND this server process
    // is itself opted in. This is the negotiation AND that guards the halved
    // strong-sum length. A stock upstream client never sends the letter, and an
    // un-opted-in oc server never sets the bit, so the extension is inert unless
    // both peers are oc and both opted in.
    if consecutive_match_opt_in() && client_info.contains(CONSECUTIVE_MATCH_CHAR) {
        flags |= CompatibilityFlags::CONSECUTIVE_MATCH;
    }

    flags
}

/// Returns `true` when `client_info` contains the pre-release `'V'` capability.
///
/// Upstream rsync pre-release builds once used `'V'` (uppercase) instead of
/// `'v'` to advertise varint flist support. When a server detects `'V'` it
/// must implicitly set `CF_VARINT_FLIST_FLAGS` and write the compat flags
/// as a single byte (`write_byte`) rather than a varint, maintaining
/// backward compatibility with those older pre-release clients.
///
/// # Upstream reference
///
/// `compat.c:737` - `if (strchr(client_info, 'V') != NULL)`
pub(crate) fn client_has_pre_release_v_flag(client_info: &str) -> bool {
    client_info.contains('V')
}
