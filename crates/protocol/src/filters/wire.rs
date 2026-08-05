//! Wire format encoding and decoding for filter rules.

use crate::ProtocolVersion;
use std::ffi::{OsStr, OsString};
use std::io::{self, Read, Write};

/// Rule type prefix character.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum RuleType {
    /// Include rule (`+` prefix).
    Include,
    /// Exclude rule (`-` prefix).
    #[default]
    Exclude,
    /// Clear previously defined rules (`!` prefix).
    Clear,
    /// Merge rules from file (`.` prefix).
    Merge,
    /// Directory merge rules (`:` prefix).
    DirMerge,
    /// Protect from deletion (`P` prefix).
    Protect,
    /// Risk (allow deletion) (`R` prefix).
    Risk,
}

impl RuleType {
    /// Returns the prefix character for this rule type.
    ///
    /// # Upstream Reference
    ///
    /// `exclude.c:1137-1214` - prefix character to rule type mapping
    pub const fn prefix_char(self) -> char {
        match self {
            RuleType::Include => '+',
            RuleType::Exclude => '-',
            RuleType::Clear => '!',
            RuleType::Merge => '.',
            RuleType::DirMerge => ':',
            RuleType::Protect => 'P',
            RuleType::Risk => 'R',
        }
    }

    /// Parses a rule type from its prefix character.
    ///
    /// # Upstream Reference
    ///
    /// `exclude.c:1137-1214` - prefix character to rule type mapping
    pub const fn from_prefix_char(c: char) -> Option<Self> {
        match c {
            '+' => Some(RuleType::Include),
            '-' => Some(RuleType::Exclude),
            '!' => Some(RuleType::Clear),
            '.' => Some(RuleType::Merge),
            ':' => Some(RuleType::DirMerge),
            'P' => Some(RuleType::Protect),
            'R' => Some(RuleType::Risk),
            _ => None,
        }
    }
}

/// Filter rule in wire format representation.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct FilterRuleWireFormat {
    /// Rule type (Include/Exclude/Clear/etc.).
    pub rule_type: RuleType,
    /// Glob pattern, stored as raw bytes.
    ///
    /// Upstream carries filter patterns as a raw `char *` end-to-end
    /// (`exclude.c:219` allocates `rule->pattern = new_array(char, ...)`;
    /// `exclude.c:1638` writes it verbatim with `write_buf`; `exclude.c:1685`
    /// reads it back into a `char[]` with `read_sbuf`) and never validates the
    /// bytes as UTF-8. Storing an [`OsString`] rather than a `String` lets a
    /// pattern containing non-UTF-8 bytes round-trip through the wire
    /// serialize/parse pair byte-for-byte, matching upstream. For a valid-UTF-8
    /// pattern the on-wire bytes are unchanged.
    pub pattern: OsString,
    /// Anchored pattern (`/` modifier).
    pub anchored: bool,
    /// Directory-only pattern (trailing `/`).
    pub directory_only: bool,
    /// No-inherit modifier (`n` flag).
    pub no_inherit: bool,
    /// CVS exclude modifier (`C` flag).
    pub cvs_exclude: bool,
    /// Word-split modifier (`w` flag).
    pub word_split: bool,
    /// Exclude from merge (`e` flag).
    pub exclude_from_merge: bool,
    /// XAttr only (`x` flag).
    pub xattr_only: bool,
    /// Apply sender-side (`s` flag, protocol v29+).
    pub sender_side: bool,
    /// Apply receiver-side (`r` flag, protocol v29+).
    pub receiver_side: bool,
    /// Perishable (`p` flag, protocol v30+).
    pub perishable: bool,
    /// No-match-with-this negates (`!` modifier).
    pub negate: bool,
    /// No-prefixes modifier (`-` or `+` on a merge/dir-merge rule).
    ///
    /// upstream: `exclude.c:1227-1237` - `'-'` sets `FILTRULE_NO_PREFIXES`;
    /// `'+'` additionally sets `FILTRULE_INCLUDE`. Both are only legal when
    /// `FILTRULE_MERGE_FILE` (merge `.` or dir-merge `:`) is already set.
    pub no_prefixes: bool,
    /// Pairs with [`Self::no_prefixes`] to encode the `+` variant.
    ///
    /// When `no_prefixes && no_prefixes_include`, the merge file's per-dir
    /// rules are treated as include-only; otherwise they are exclude-only.
    pub no_prefixes_include: bool,
    /// Marks a rule produced by the `-C`/`--cvs-exclude` built-in expansion.
    ///
    /// This is transfer-decision metadata, NOT part of the wire encoding: it is
    /// never serialized and always parses back as `false`. It exists so the
    /// send path can reproduce upstream's `send_filter_list()` role/protocol
    /// gating for CVS rules - kept local on a receiving client, and (for the
    /// `:C` per-directory merge) only crossing the wire on protocol >= 29.
    ///
    /// upstream: exclude.c:1652-1668 send_filter_list() - the `-C` rules are
    /// added to the transmitted list only when `am_sender`, and `:C` only when
    /// `protocol_version >= 29`; otherwise they are appended after `send_rules()`
    /// and stay local.
    pub cvs_origin: bool,
}

impl FilterRuleWireFormat {
    /// Creates a simple exclude rule with default modifiers.
    pub fn exclude(pattern: impl Into<OsString>) -> Self {
        Self {
            rule_type: RuleType::Exclude,
            pattern: pattern.into(),
            anchored: false,
            directory_only: false,
            no_inherit: false,
            cvs_exclude: false,
            word_split: false,
            exclude_from_merge: false,
            xattr_only: false,
            sender_side: false,
            receiver_side: false,
            perishable: false,
            negate: false,
            no_prefixes: false,
            no_prefixes_include: false,
            cvs_origin: false,
        }
    }

    /// Creates a simple include rule with default modifiers.
    pub fn include(pattern: impl Into<OsString>) -> Self {
        Self {
            rule_type: RuleType::Include,
            pattern: pattern.into(),
            anchored: false,
            directory_only: false,
            no_inherit: false,
            cvs_exclude: false,
            word_split: false,
            exclude_from_merge: false,
            xattr_only: false,
            sender_side: false,
            receiver_side: false,
            perishable: false,
            negate: false,
            no_prefixes: false,
            no_prefixes_include: false,
            cvs_origin: false,
        }
    }

    /// Sets the anchored flag.
    pub const fn with_anchored(mut self, anchored: bool) -> Self {
        self.anchored = anchored;
        self
    }

    /// Sets the directory-only flag.
    pub const fn with_directory_only(mut self, directory_only: bool) -> Self {
        self.directory_only = directory_only;
        self
    }

    /// Sets sender and receiver side flags.
    pub const fn with_sides(mut self, sender: bool, receiver: bool) -> Self {
        self.sender_side = sender;
        self.receiver_side = receiver;
        self
    }

    /// Sets the perishable flag.
    pub const fn with_perishable(mut self, perishable: bool) -> Self {
        self.perishable = perishable;
        self
    }
}

/// Reads a 4-byte little-endian integer from the stream.
///
/// This mirrors upstream rsync's `read_int()` function in io.c:1774,
/// which reads 4 bytes and interprets them as a little-endian int32.
fn read_i32_le(reader: &mut dyn Read) -> io::Result<i32> {
    let mut buf = [0u8; 4];
    reader.read_exact(&mut buf)?;
    Ok(i32::from_le_bytes(buf))
}

/// Writes a 4-byte little-endian integer to the stream.
///
/// This mirrors upstream rsync's `write_int()` function in io.c:1815,
/// which writes 4 bytes as a little-endian int32.
fn write_i32_le(writer: &mut dyn Write, value: i32) -> io::Result<()> {
    writer.write_all(&value.to_le_bytes())
}

/// Returns the raw wire bytes of a filter pattern.
///
/// On unix the pattern bytes are the `OsStr` bytes verbatim, so any byte
/// sequence (including non-UTF-8) round-trips unchanged. On other platforms
/// `OsStr` is not byte-addressable, so we fall back to the UTF-8 encoding,
/// which is lossless for the valid-UTF-8 patterns those platforms produce.
#[cfg(unix)]
fn pattern_to_wire_bytes(pattern: &OsStr) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    pattern.as_bytes().to_vec()
}

/// Non-unix twin of [`pattern_to_wire_bytes`].
#[cfg(not(unix))]
fn pattern_to_wire_bytes(pattern: &OsStr) -> Vec<u8> {
    pattern.to_string_lossy().into_owned().into_bytes()
}

/// Reconstructs a filter pattern from its raw wire bytes.
///
/// The inverse of [`pattern_to_wire_bytes`]: on unix it wraps the bytes as an
/// `OsString` without any UTF-8 check, so a non-UTF-8 pattern survives
/// verbatim.
#[cfg(unix)]
fn wire_bytes_to_pattern(bytes: &[u8]) -> OsString {
    use std::os::unix::ffi::OsStrExt;
    OsStr::from_bytes(bytes).to_os_string()
}

/// Non-unix twin of [`wire_bytes_to_pattern`].
#[cfg(not(unix))]
fn wire_bytes_to_pattern(bytes: &[u8]) -> OsString {
    OsString::from(String::from_utf8_lossy(bytes).into_owned())
}

/// Reads filter list from wire format.
///
/// Reads a sequence of filter rules terminated by a 4-byte integer 0.
/// Upstream uses `read_int()` / `write_int()` which are 4-byte little-endian integers,
/// NOT varints. This matches upstream's send_filter_list() in exclude.c:1658.
pub fn read_filter_list(
    reader: &mut dyn Read,
    protocol: ProtocolVersion,
) -> io::Result<Vec<FilterRuleWireFormat>> {
    let mut rules = Vec::new();

    loop {
        let len = read_i32_le(reader)?;

        if len == 0 {
            // Wire-format terminator (zero-length record).
            break;
        }

        if len < 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("invalid filter rule length: {len}"),
            ));
        }

        let mut buf = vec![0u8; len as usize];
        reader.read_exact(&mut buf)?;

        let rule = parse_wire_rule(&buf, protocol)?;
        rules.push(rule);
    }

    Ok(rules)
}

/// Async twin of [`read_filter_list`].
///
/// Reads the same length-prefixed rule records (`.await`-driven) in the same
/// order, terminated by the 4-byte little-endian zero, and runs the identical
/// `parse_wire_rule` decode/validation on each record. It therefore yields the
/// same `Vec<FilterRuleWireFormat>` and consumes the same bytes for the same
/// wire input; only the I/O mechanism (await vs blocking) differs. Gated on
/// `tokio-transfer`.
///
/// This matches upstream's `recv_filter_list()` in `exclude.c:1658`, which reads
/// 4-byte `read_int()` length prefixes until a zero terminator.
///
/// # Errors
///
/// - A negative length prefix yields [`io::ErrorKind::InvalidData`], exactly as
///   the blocking reader surfaces it.
/// - Any decode error from `parse_wire_rule` propagates unchanged.
/// - Truncation mid-record surfaces the underlying read error (typically
///   [`io::ErrorKind::UnexpectedEof`]).
#[cfg(feature = "tokio-transfer")]
pub async fn read_filter_list_async<R>(
    reader: &mut R,
    protocol: ProtocolVersion,
) -> io::Result<Vec<FilterRuleWireFormat>>
where
    R: tokio::io::AsyncRead + Unpin + ?Sized,
{
    use tokio::io::AsyncReadExt;

    let mut rules = Vec::new();

    loop {
        let mut len_buf = [0u8; 4];
        reader.read_exact(&mut len_buf).await?;
        let len = i32::from_le_bytes(len_buf);

        if len == 0 {
            // Wire-format terminator (zero-length record).
            break;
        }

        if len < 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("invalid filter rule length: {len}"),
            ));
        }

        let mut buf = vec![0u8; len as usize];
        reader.read_exact(&mut buf).await?;

        let rule = parse_wire_rule(&buf, protocol)?;
        rules.push(rule);
    }

    Ok(rules)
}

/// Writes filter list to wire format.
///
/// Writes a sequence of filter rules followed by a 4-byte zero terminator.
/// Upstream uses `write_int()` which is a 4-byte little-endian integer, NOT varint.
/// This matches upstream's send_filter_list() in exclude.c:1658.
pub fn write_filter_list<W: Write>(
    writer: &mut W,
    rules: &[FilterRuleWireFormat],
    protocol: ProtocolVersion,
) -> io::Result<()> {
    for rule in rules {
        let bytes = serialize_rule(rule, protocol)?;
        write_i32_le(writer, bytes.len() as i32)?;
        writer.write_all(&bytes)?;
    }

    // Wire-format terminator (zero-length record).
    write_i32_le(writer, 0)?;
    Ok(())
}

/// Parses a single filter rule from wire format bytes.
///
/// For protocol < 29, only old-style prefixes are accepted: `"+ "`, `"- "`,
/// or `"!"`. No modifier characters are parsed. This matches upstream
/// `exclude.c:1119-1133` where `XFLG_OLD_PREFIXES` restricts parsing to
/// these three forms.
///
/// The rule-type prefix and every modifier byte are ASCII, so parsing scans
/// the leading bytes directly and treats the remaining bytes as the raw
/// pattern body. The pattern is never validated as UTF-8: upstream carries it
/// as a raw `char *` (`exclude.c:1685` `read_sbuf` into a `char[]`), so a
/// non-UTF-8 pattern is preserved verbatim rather than rejected.
fn parse_wire_rule(buf: &[u8], protocol: ProtocolVersion) -> io::Result<FilterRuleWireFormat> {
    if buf.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "empty filter rule",
        ));
    }

    // upstream: exclude.c:1675 - protocol < 29 uses XFLG_OLD_PREFIXES
    if protocol.uses_old_prefixes() {
        return parse_wire_rule_old_prefix(buf);
    }

    parse_wire_rule_modern(buf, protocol)
}

/// Parses a wire rule using old-style prefix rules (protocol < 29).
///
/// The `"- "` and `"+ "` prefixes are *optional*: a bare pattern is an
/// exclude, matching upstream where the sender emits plain excludes with no
/// prefix at these protocols (`get_rule_prefix()` sets `legal_len = 0`).
/// Valid forms:
/// - `"- pattern"` - exclude
/// - `"+ pattern"` - include
/// - `"!"` - clear list (a longer `"!..."` text is an exclude pattern)
/// - `"pattern"` - exclude
///
/// No modifier flags are parsed.
///
/// # Upstream Reference
///
/// `exclude.c:1125-1133` - `XFLG_OLD_PREFIXES` branch treats the prefixes
/// as optional; `exclude.c:1315-1323` cancels a tentative `'!'` clear when
/// more text follows.
///
/// # Errors
///
/// Returns `InvalidData` when a prefix is followed by an empty pattern,
/// mirroring upstream's "unexpected end of filter rule" `RERR_SYNTAX` exit
/// (exclude.c:1324-1327).
fn parse_wire_rule_old_prefix(buf: &[u8]) -> io::Result<FilterRuleWireFormat> {
    if buf == b"!" {
        return Ok(FilterRuleWireFormat {
            rule_type: RuleType::Clear,
            ..FilterRuleWireFormat::default()
        });
    }

    let (rule_type, pattern_bytes) = if let Some(pat) = buf.strip_prefix(b"- ".as_slice()) {
        (RuleType::Exclude, pat)
    } else if let Some(pat) = buf.strip_prefix(b"+ ".as_slice()) {
        (RuleType::Include, pat)
    } else {
        (RuleType::Exclude, buf)
    };

    if pattern_bytes.is_empty() {
        // upstream: exclude.c:1324-1327 exits RERR_SYNTAX on a rule that
        // ends right after its prefix.
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "unexpected end of filter rule: {:?}",
                String::from_utf8_lossy(buf)
            ),
        ));
    }

    let mut rule = FilterRuleWireFormat {
        rule_type,
        ..FilterRuleWireFormat::default()
    };

    if let Some(stripped) = pattern_bytes.strip_suffix(b"/".as_slice()) {
        rule.directory_only = true;
        rule.pattern = wire_bytes_to_pattern(stripped);
    } else {
        rule.pattern = wire_bytes_to_pattern(pattern_bytes);
    }

    Ok(rule)
}

/// Parses a wire rule using modern prefix rules (protocol >= 29).
///
/// Supports full modifier parsing including `/`, `!`, `C`, `n`, `w`, `e`,
/// `x`, `s`, `r`, `p` flags.
fn parse_wire_rule_modern(
    buf: &[u8],
    protocol: ProtocolVersion,
) -> io::Result<FilterRuleWireFormat> {
    // The rule-type prefix is always a single ASCII byte, so decode it as a
    // char without validating the rest of the buffer as UTF-8.
    let first = *buf
        .first()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "empty filter rule"))?;

    let rule_type = RuleType::from_prefix_char(first as char).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("invalid rule type prefix: '{}'", first as char),
        )
    })?;

    let mut rule = FilterRuleWireFormat {
        rule_type,
        ..FilterRuleWireFormat::default()
    };

    let mut pattern_start = 1;
    // Every modifier byte is ASCII; scan them directly and stop at the first
    // non-modifier byte, which begins the (possibly non-UTF-8) pattern body.
    for (i, &c) in buf[1..].iter().enumerate() {
        match c {
            b'/' if i == 0 => {
                rule.anchored = true;
                pattern_start += 1;
            }
            b'!' => {
                rule.negate = true;
                pattern_start += 1;
            }
            b'C' => {
                // upstream: exclude.c:1248-1255 parse_rule_tok() case 'C' sets
                // FILTRULE_NO_PREFIXES | FILTRULE_WORD_SPLIT | FILTRULE_NO_INHERIT
                // | FILTRULE_CVS_IGNORE together, so a `C` rule carries all the
                // flags it implies. Re-derive them here so a `:C` dir-merge from
                // an upstream peer keeps its no-inherit/word-split/no-prefixes
                // semantics (the receiver gates DirMergeConfig::with_inherit on
                // no_inherit). The serializer collapses these back to a bare `C`
                // (get_rule_prefix emits only `C` and suppresses the implied
                // flags, exclude.c:1548-1561), so re-encoding a decoded `:C`
                // stays `:C` - no double-application.
                rule.cvs_exclude = true;
                rule.no_inherit = true;
                rule.word_split = true;
                rule.no_prefixes = true;
                pattern_start += 1;
            }
            b'n' => {
                rule.no_inherit = true;
                pattern_start += 1;
            }
            b'w' => {
                rule.word_split = true;
                pattern_start += 1;
            }
            b'e' => {
                rule.exclude_from_merge = true;
                pattern_start += 1;
            }
            b'x' => {
                rule.xattr_only = true;
                pattern_start += 1;
            }
            // upstream: exclude.c:1227-1237 - `-` and `+` set FILTRULE_NO_PREFIXES
            // on merge/dir-merge rules. `+` additionally sets FILTRULE_INCLUDE.
            // Acceptance is gated on Merge/DirMerge to mirror upstream's
            // FILTRULE_MERGE_FILE precondition; on other rule types these
            // characters fall through to `_` and terminate modifier parsing.
            b'-' if matches!(rule.rule_type, RuleType::Merge | RuleType::DirMerge) => {
                rule.no_prefixes = true;
                pattern_start += 1;
            }
            b'+' if matches!(rule.rule_type, RuleType::Merge | RuleType::DirMerge) => {
                rule.no_prefixes = true;
                rule.no_prefixes_include = true;
                pattern_start += 1;
            }
            b's' if protocol.supports_sender_receiver_modifiers() => {
                rule.sender_side = true;
                pattern_start += 1;
            }
            b'r' if protocol.supports_sender_receiver_modifiers() => {
                rule.receiver_side = true;
                pattern_start += 1;
            }
            b'p' if protocol.supports_perishable_modifier() => {
                rule.perishable = true;
                pattern_start += 1;
            }
            b' ' => {
                // Trailing space terminates the modifier section per upstream
                // exclude.c parser.
                pattern_start += 1;
                break;
            }
            _ => break,
        }
    }

    let mut pattern_bytes = &buf[pattern_start..];

    // Non-merge rules encode the anchor as a leading `/` in the pattern body
    // (upstream keeps it in ent->pattern with FILTRULE_ABS_PATH unset). Fold it
    // back into the `anchored` flag so the parsed rule matches what
    // build_wire_format_rules() produced (bare pattern + anchored bit) and
    // round-trips byte-identically through serialize_rule(). Merge and
    // dir-merge rules reserve the leading `/` for FILTRULE_ABS_PATH, consumed as
    // the `/` prefix modifier in the loop above.
    if !rule.anchored
        && !matches!(rule.rule_type, RuleType::Merge | RuleType::DirMerge)
        && pattern_bytes.len() > 1
        && pattern_bytes.first() == Some(&b'/')
    {
        rule.anchored = true;
        pattern_bytes = &pattern_bytes[1..];
    }

    if let Some(stripped) = pattern_bytes.strip_suffix(b"/".as_slice()) {
        rule.directory_only = true;
        rule.pattern = wire_bytes_to_pattern(stripped);
    } else {
        rule.pattern = wire_bytes_to_pattern(pattern_bytes);
    }

    Ok(rule)
}

/// Serializes a filter rule to wire format bytes.
///
/// Returns an error if the rule cannot be represented in the current
/// protocol version (e.g., dir-merge or modifier-bearing rules for proto < 29).
///
/// # Upstream Reference
///
/// `exclude.c:1623-1627` - sender exits with RERR_PROTOCOL when prefix is NULL
fn serialize_rule(rule: &FilterRuleWireFormat, protocol: ProtocolVersion) -> io::Result<Vec<u8>> {
    let prefix = super::prefix::build_rule_prefix(rule, protocol).ok_or_else(|| {
        // upstream: exclude.c:1627 exit_cleanup(RERR_PROTOCOL) (exit 2). Tag the
        // error so the core exit-code mapper yields RERR_PROTOCOL, not
        // RERR_STREAMIO(12).
        crate::protocol_violation::protocol_violation(
            "filter rules are too modern for remote rsync",
        )
    })?;
    let mut bytes = prefix.into_bytes();
    let pattern_bytes = pattern_to_wire_bytes(&rule.pattern);
    // Non-merge anchored rules carry the anchor as a leading `/` in the pattern
    // body, mirroring upstream whose command-line `- /foo` keeps the slash in
    // ent->pattern with FILTRULE_ABS_PATH unset (exclude.c:200-208). The `/`
    // prefix modifier is reserved for merge/dir-merge ABS_PATH rules
    // (build_rule_prefix), so add the slash here for every other rule type.
    // `pattern` stores the bare body; split_pattern_modifiers() (client) and
    // parse_wire_rule_modern() (server) fold the leading `/` into `anchored`.
    if rule.anchored
        && !matches!(rule.rule_type, RuleType::Merge | RuleType::DirMerge)
        && pattern_bytes.first() != Some(&b'/')
    {
        bytes.push(b'/');
    }
    bytes.extend_from_slice(&pattern_bytes);

    if rule.directory_only {
        bytes.push(b'/');
    }

    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_filter_list_roundtrip() {
        let protocol = ProtocolVersion::from_supported(32).unwrap();
        let mut buf = Vec::new();

        write_filter_list(&mut buf, &[], protocol).unwrap();

        // Should be 4-byte little-endian zero (upstream write_int(0))
        assert_eq!(buf, vec![0, 0, 0, 0]);

        let rules = read_filter_list(&mut &buf[..], protocol).unwrap();
        assert_eq!(rules, vec![]);
    }

    #[test]
    fn simple_exclude_pattern() {
        let protocol = ProtocolVersion::from_supported(32).unwrap();
        let rule = FilterRuleWireFormat::exclude("*.log".to_owned());

        let mut buf = Vec::new();
        write_filter_list(&mut buf, std::slice::from_ref(&rule), protocol).unwrap();

        let parsed = read_filter_list(&mut &buf[..], protocol).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].rule_type, RuleType::Exclude);
        assert_eq!(parsed[0].pattern, "*.log");
    }

    /// A plain exclude round-trips at protocol 28, where the sender emits
    /// the bare pattern with no `"- "` prefix (upstream `get_rule_prefix()`
    /// sets `legal_len = 0`, exclude.c:1542) and the receiver treats the
    /// old-style prefixes as optional (exclude.c:1125-1133).
    #[test]
    fn old_prefix_bare_exclude_roundtrip() {
        let protocol = ProtocolVersion::from_supported(28).unwrap();
        let rule = FilterRuleWireFormat::exclude("*.tmp".to_owned());

        let mut buf = Vec::new();
        write_filter_list(&mut buf, std::slice::from_ref(&rule), protocol).unwrap();
        // Bare pattern on the wire: length prefix + "*.tmp" + terminator.
        assert_eq!(buf, b"\x05\0\0\0*.tmp\0\0\0\0");

        let parsed = read_filter_list(&mut &buf[..], protocol).unwrap();
        assert_eq!(parsed, vec![rule]);
    }

    /// A `'!'` followed by more text is not a clear-list marker under the
    /// old prefixes: upstream cancels the tentative clear when the token is
    /// longer than one byte (exclude.c:1315-1323) and the text becomes an
    /// exclude pattern.
    #[test]
    fn old_prefix_bang_with_trailing_text_is_exclude() {
        let protocol = ProtocolVersion::from_supported(28).unwrap();
        let payload = b"!keep";
        let mut buf = Vec::new();
        buf.extend_from_slice(&(payload.len() as i32).to_le_bytes());
        buf.extend_from_slice(payload);
        buf.extend_from_slice(&0i32.to_le_bytes());

        let parsed = read_filter_list(&mut &buf[..], protocol).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].rule_type, RuleType::Exclude);
        assert_eq!(parsed[0].pattern, "!keep");
    }

    #[test]
    fn simple_include_pattern() {
        let protocol = ProtocolVersion::from_supported(32).unwrap();
        let rule = FilterRuleWireFormat::include("*.txt".to_owned());

        let mut buf = Vec::new();
        write_filter_list(&mut buf, std::slice::from_ref(&rule), protocol).unwrap();

        let parsed = read_filter_list(&mut &buf[..], protocol).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].rule_type, RuleType::Include);
        assert_eq!(parsed[0].pattern, "*.txt");
    }

    #[test]
    fn anchored_pattern() {
        let protocol = ProtocolVersion::from_supported(32).unwrap();
        // Canonical client form: bare pattern body plus the `anchored` bit, as
        // produced by build_wire_format_rules()/split_pattern_modifiers().
        let rule = FilterRuleWireFormat::exclude("tmp".to_owned()).with_anchored(true);

        let mut buf = Vec::new();
        write_filter_list(&mut buf, std::slice::from_ref(&rule), protocol).unwrap();

        let parsed = read_filter_list(&mut &buf[..], protocol).unwrap();
        assert_eq!(parsed.len(), 1);
        assert!(parsed[0].anchored);
        assert_eq!(parsed[0].pattern, "tmp");
    }

    #[test]
    fn anchored_exclude_wire_bytes_match_upstream() {
        // Regression: an anchored command-line rule (`--filter '- /drop.txt'`)
        // must serialize as `- /drop.txt` (leading slash in the PATTERN), not
        // `-/ drop.txt` (slash as the ABS_PATH prefix modifier). Upstream keeps
        // the slash in ent->pattern with FILTRULE_ABS_PATH unset, so its sender
        // anchors the match to the transfer root (exclude.c:941-944). Encoding
        // the slash as the `/` modifier instead makes the remote sender treat
        // it as an unanchored basename match, wrongly excluding `sub/drop.txt`
        // as well as top-level `drop.txt` from the flist.
        let protocol = ProtocolVersion::from_supported(32).unwrap();
        let rule = FilterRuleWireFormat::exclude("drop.txt".to_owned()).with_anchored(true);

        let mut buf = Vec::new();
        write_filter_list(&mut buf, std::slice::from_ref(&rule), protocol).unwrap();

        // 4-byte LE length (11 = len of "- /drop.txt"), the rule bytes, then the
        // 4-byte LE zero terminator.
        let mut expected = Vec::new();
        expected.extend_from_slice(&11i32.to_le_bytes());
        expected.extend_from_slice(b"- /drop.txt");
        expected.extend_from_slice(&0i32.to_le_bytes());
        assert_eq!(buf, expected, "anchored exclude must emit `- /drop.txt`");

        // And it must round-trip back to the canonical bare-pattern + anchored
        // representation so oc<->oc transfers stay symmetric.
        let parsed = read_filter_list(&mut &buf[..], protocol).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].rule_type, RuleType::Exclude);
        assert!(parsed[0].anchored);
        assert_eq!(parsed[0].pattern, "drop.txt");
    }

    #[test]
    fn directory_only_pattern() {
        let protocol = ProtocolVersion::from_supported(32).unwrap();
        let rule = FilterRuleWireFormat::exclude("cache".to_owned()).with_directory_only(true);

        let mut buf = Vec::new();
        write_filter_list(&mut buf, std::slice::from_ref(&rule), protocol).unwrap();

        let parsed = read_filter_list(&mut &buf[..], protocol).unwrap();
        assert_eq!(parsed.len(), 1);
        assert!(parsed[0].directory_only);
        assert_eq!(parsed[0].pattern, "cache");
    }

    /// Pattern stored on `FilterRuleWireFormat` must omit the trailing `/`
    /// because `serialize_rule` re-appends it for directory-only rules.
    /// Storing both produces `*//` on the wire, which upstream parses as
    /// the pattern `*/` (slash-bearing, anchored-style) and breaks the
    /// `--include='*/' --exclude='*'` directory-traversal idiom.
    ///
    /// upstream: `exclude.c:923` - patterns with internal slashes are
    /// treated as anchored matches.
    #[test]
    fn directory_only_wildcard_emits_single_trailing_slash() {
        let protocol = ProtocolVersion::from_supported(32).unwrap();
        let rule = FilterRuleWireFormat::include("*".to_owned()).with_directory_only(true);

        let mut buf = Vec::new();
        write_filter_list(&mut buf, std::slice::from_ref(&rule), protocol).unwrap();

        // 4-byte length prefix + payload + 4-byte zero terminator.
        // Payload must be exactly `+ */` (4 bytes) - one trailing slash.
        assert_eq!(&buf[..4], &4i32.to_le_bytes()[..]);
        assert_eq!(&buf[4..8], b"+ */");
        assert_eq!(&buf[8..], &0i32.to_le_bytes()[..]);

        let parsed = read_filter_list(&mut &buf[..], protocol).unwrap();
        assert_eq!(parsed.len(), 1);
        assert!(parsed[0].directory_only);
        assert_eq!(parsed[0].pattern, "*");
    }

    #[test]
    fn sender_side_filter_v29() {
        let protocol = ProtocolVersion::from_supported(29).unwrap();
        let rule = FilterRuleWireFormat::exclude("*.tmp".to_owned()).with_sides(true, false);

        let mut buf = Vec::new();
        write_filter_list(&mut buf, std::slice::from_ref(&rule), protocol).unwrap();

        let parsed = read_filter_list(&mut &buf[..], protocol).unwrap();
        assert_eq!(parsed.len(), 1);
        assert!(parsed[0].sender_side);
        assert!(!parsed[0].receiver_side);
    }

    #[test]
    fn receiver_side_filter_v29() {
        let protocol = ProtocolVersion::from_supported(29).unwrap();
        let rule = FilterRuleWireFormat::exclude("*.bak".to_owned()).with_sides(false, true);

        let mut buf = Vec::new();
        write_filter_list(&mut buf, std::slice::from_ref(&rule), protocol).unwrap();

        let parsed = read_filter_list(&mut &buf[..], protocol).unwrap();
        assert_eq!(parsed.len(), 1);
        assert!(!parsed[0].sender_side);
        assert!(parsed[0].receiver_side);
    }

    #[test]
    fn perishable_filter_v30() {
        let protocol = ProtocolVersion::from_supported(30).unwrap();
        let rule = FilterRuleWireFormat::exclude("*.swp".to_owned()).with_perishable(true);

        let mut buf = Vec::new();
        write_filter_list(&mut buf, std::slice::from_ref(&rule), protocol).unwrap();

        let parsed = read_filter_list(&mut &buf[..], protocol).unwrap();
        assert_eq!(parsed.len(), 1);
        assert!(parsed[0].perishable);
    }

    /// upstream: `exclude.c:1555-1560` get_rule_prefix() emits `-` between `w`
    /// and `e` when FILTRULE_NO_PREFIXES is set on a merge/dir-merge rule;
    /// `exclude.c:1227-1231` parse_rule_tok() accepts `-` after `:` or `.`.
    /// Round-trip ensures encode/decode parity for `:- .excl`.
    #[test]
    fn dir_merge_no_prefixes_minus_roundtrip() {
        let protocol = ProtocolVersion::from_supported(32).unwrap();
        let rule = FilterRuleWireFormat {
            rule_type: RuleType::DirMerge,
            pattern: ".excl".into(),
            no_prefixes: true,
            ..FilterRuleWireFormat::default()
        };

        let mut buf = Vec::new();
        write_filter_list(&mut buf, std::slice::from_ref(&rule), protocol).unwrap();

        // Payload: ":- .excl" - 8 bytes.
        assert_eq!(&buf[..4], &8i32.to_le_bytes()[..]);
        assert_eq!(&buf[4..12], b":- .excl");
        assert_eq!(&buf[12..], &0i32.to_le_bytes()[..]);

        let parsed = read_filter_list(&mut &buf[..], protocol).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].rule_type, RuleType::DirMerge);
        assert_eq!(parsed[0].pattern, ".excl");
        assert!(parsed[0].no_prefixes);
        assert!(!parsed[0].no_prefixes_include);
    }

    /// upstream: `exclude.c:1232-1236` parse_rule_tok() - `+` after `:` or `.`
    /// sets FILTRULE_NO_PREFIXES|FILTRULE_INCLUDE; `exclude.c:1556-1557`
    /// get_rule_prefix() emits `+` when both bits are set.
    #[test]
    fn dir_merge_no_prefixes_plus_roundtrip() {
        let protocol = ProtocolVersion::from_supported(32).unwrap();
        let rule = FilterRuleWireFormat {
            rule_type: RuleType::DirMerge,
            pattern: ".incl".into(),
            no_prefixes: true,
            no_prefixes_include: true,
            ..FilterRuleWireFormat::default()
        };

        let mut buf = Vec::new();
        write_filter_list(&mut buf, std::slice::from_ref(&rule), protocol).unwrap();

        // Payload: ":+ .incl" - 8 bytes.
        assert_eq!(&buf[..4], &8i32.to_le_bytes()[..]);
        assert_eq!(&buf[4..12], b":+ .incl");

        let parsed = read_filter_list(&mut &buf[..], protocol).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].rule_type, RuleType::DirMerge);
        assert_eq!(parsed[0].pattern, ".incl");
        assert!(parsed[0].no_prefixes);
        assert!(parsed[0].no_prefixes_include);
    }

    /// upstream: `exclude.c:1228, 1233` - `-`/`+` modifiers are only valid
    /// after FILTRULE_MERGE_FILE is set. A plain exclude rule with `-` after
    /// the type prefix must NOT be parsed as no-prefixes; the modifier loop
    /// terminates and the remainder becomes the pattern.
    #[test]
    fn no_prefixes_modifier_rejected_on_non_merge_rule() {
        let protocol = ProtocolVersion::from_supported(32).unwrap();
        // Raw wire bytes: "--foo" - the leading `-` is the rule type prefix
        // (Exclude); the second `-` is not a legal modifier on Exclude rules
        // and must fall through to the pattern.
        let rules = read_filter_list(
            &mut &[5u8, 0, 0, 0, b'-', b'-', b'f', b'o', b'o', 0, 0, 0, 0][..],
            protocol,
        )
        .unwrap();
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].rule_type, RuleType::Exclude);
        assert!(!rules[0].no_prefixes);
        assert_eq!(rules[0].pattern, "-foo");
    }

    #[test]
    fn protocol_downgrade_rejects_unrepresentable_rules() {
        // v28 prefixes cannot encode v30 s/r/p modifiers, so write_filter_list
        // must reject the rule rather than silently dropping the flags.
        let rule = FilterRuleWireFormat::exclude("test".to_owned())
            .with_sides(true, false)
            .with_perishable(true);

        let protocol_v28 = ProtocolVersion::from_supported(28).unwrap();
        let mut buf = Vec::new();
        let result = write_filter_list(&mut buf, &[rule], protocol_v28);
        assert!(result.is_err());
    }

    /// A filter rule whose pattern contains a non-UTF-8 byte must round-trip
    /// through the wire serialize/parse pair byte-for-byte. Upstream carries the
    /// pattern as a raw `char *` and never validates it as UTF-8
    /// (`exclude.c:1638` `write_buf(f_out, ent->pattern, len)` on send,
    /// `exclude.c:1685` `read_sbuf(f_in, line, len)` into a `char[]` on recv),
    /// so a `0xFF` byte survives verbatim. Before the pattern became byte-based
    /// the reader rejected such a record as `InvalidData` (the pinned rejection
    /// is retired by this round-trip). The 0xFF byte cannot live in a `String`,
    /// so this case is unix-only where `OsStr` is byte-addressable.
    #[cfg(unix)]
    #[test]
    fn non_utf8_pattern_roundtrips_byte_for_byte() {
        use std::os::unix::ffi::OsStrExt;

        let protocol = ProtocolVersion::from_supported(32).unwrap();
        let pattern = OsStr::from_bytes(b"log\xff.txt").to_os_string();
        let rule = FilterRuleWireFormat::exclude(pattern);

        let mut buf = Vec::new();
        write_filter_list(&mut buf, std::slice::from_ref(&rule), protocol).unwrap();

        // Wire bytes: 4-byte LE length + "- " prefix + the raw pattern bytes
        // (0xFF emitted verbatim) + the 4-byte LE zero terminator.
        let payload = b"- log\xff.txt";
        let mut expected = Vec::new();
        expected.extend_from_slice(&(payload.len() as i32).to_le_bytes());
        expected.extend_from_slice(payload);
        expected.extend_from_slice(&0i32.to_le_bytes());
        assert_eq!(
            buf, expected,
            "0xFF pattern byte must reach the wire verbatim"
        );

        // Decode(encode(rule)) == rule, byte-for-byte on the pattern.
        let parsed = read_filter_list(&mut &buf[..], protocol).unwrap();
        assert_eq!(parsed, vec![rule]);
        assert_eq!(parsed[0].pattern.as_bytes(), b"log\xff.txt");
    }

    /// Proves the async twin decodes byte-for-byte identically to the blocking
    /// [`read_filter_list`] for the same wire bytes: empty list, and a list with
    /// a couple of rules exercising modifiers. Any divergence would be an
    /// async-driver bug, since both share the [`parse_wire_rule`] decode.
    #[cfg(feature = "tokio-transfer")]
    #[tokio::test(flavor = "current_thread")]
    async fn read_filter_list_async_matches_sync() {
        let protocol = ProtocolVersion::from_supported(32).unwrap();

        let cases: [Vec<FilterRuleWireFormat>; 3] = [
            Vec::new(),
            vec![FilterRuleWireFormat::exclude("*.log".to_owned())],
            vec![
                FilterRuleWireFormat::exclude("drop.txt".to_owned()).with_anchored(true),
                FilterRuleWireFormat::include("*".to_owned()).with_directory_only(true),
            ],
        ];

        for rules in cases {
            let mut buf = Vec::new();
            write_filter_list(&mut buf, &rules, protocol).unwrap();

            let sync = read_filter_list(&mut &buf[..], protocol).unwrap();
            let mut cursor = std::io::Cursor::new(&buf);
            let asyncd = read_filter_list_async(&mut cursor, protocol).await.unwrap();

            assert_eq!(asyncd, sync, "async filter-list read diverged from sync");
        }
    }
}
