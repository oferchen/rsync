//! Wire format encoding and decoding for filter rules.

use crate::{ProtocolVersion, read_varint, write_varint};
use std::io::{self, Read, Write};

/// Rule type prefix character.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuleType {
    /// Include rule (`+` prefix).
    Include,
    /// Exclude rule (`-` prefix).
    Exclude,
    /// Clear previously defined rules (`:` prefix).
    Clear,
    /// Merge rules from file (`.` prefix).
    Merge,
    /// Directory merge rules (`,` prefix).
    DirMerge,
    /// Protect from deletion (`P` prefix).
    Protect,
    /// Risk (allow deletion) (`R` prefix).
    Risk,
}

impl RuleType {
    /// Returns the prefix character for this rule type.
    pub fn prefix_char(self) -> char {
        match self {
            RuleType::Include => '+',
            RuleType::Exclude => '-',
            RuleType::Clear => ':',
            RuleType::Merge => '.',
            RuleType::DirMerge => ',',
            RuleType::Protect => 'P',
            RuleType::Risk => 'R',
        }
    }

    /// Parses a rule type from its prefix character.
    pub fn from_prefix_char(c: char) -> Option<Self> {
        match c {
            '+' => Some(RuleType::Include),
            '-' => Some(RuleType::Exclude),
            ':' => Some(RuleType::Clear),
            '.' => Some(RuleType::Merge),
            ',' => Some(RuleType::DirMerge),
            'P' => Some(RuleType::Protect),
            'R' => Some(RuleType::Risk),
            _ => None,
        }
    }
}

/// Filter rule in wire format representation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FilterRuleWireFormat {
    /// Rule type (Include/Exclude/Clear/etc.).
    pub rule_type: RuleType,
    /// Glob pattern.
    pub pattern: String,
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
}

impl FilterRuleWireFormat {
    /// Creates a simple exclude rule with default modifiers.
    pub fn exclude(pattern: String) -> Self {
        Self {
            rule_type: RuleType::Exclude,
            pattern,
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
        }
    }

    /// Creates a simple include rule with default modifiers.
    pub fn include(pattern: String) -> Self {
        Self {
            rule_type: RuleType::Include,
            pattern,
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
        }
    }

    /// Sets the anchored flag.
    pub fn with_anchored(mut self, anchored: bool) -> Self {
        self.anchored = anchored;
        self
    }

    /// Sets the directory-only flag.
    pub fn with_directory_only(mut self, directory_only: bool) -> Self {
        self.directory_only = directory_only;
        self
    }

    /// Sets sender and receiver side flags.
    pub fn with_sides(mut self, sender: bool, receiver: bool) -> Self {
        self.sender_side = sender;
        self.receiver_side = receiver;
        self
    }

    /// Sets the perishable flag.
    pub fn with_perishable(mut self, perishable: bool) -> Self {
        self.perishable = perishable;
        self
    }
}

/// Reads filter list from wire format.
///
/// Reads a sequence of filter rules terminated by a 4-byte zero (varint 0).
pub fn read_filter_list<R: Read>(
    reader: &mut R,
    protocol: ProtocolVersion,
) -> io::Result<Vec<FilterRuleWireFormat>> {
    let mut rules = Vec::new();

    loop {
        let len = read_varint(reader)?;
        if len == 0 {
            break; // Terminator
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

/// Writes filter list to wire format.
///
/// Writes a sequence of filter rules followed by a 4-byte zero terminator.
pub fn write_filter_list<W: Write>(
    writer: &mut W,
    rules: &[FilterRuleWireFormat],
    protocol: ProtocolVersion,
) -> io::Result<()> {
    for rule in rules {
        let bytes = serialize_rule(rule, protocol)?;
        write_varint(writer, bytes.len() as i32)?;
        writer.write_all(&bytes)?;
    }

    write_varint(writer, 0)?; // Terminator
    Ok(())
}

/// Parses a single filter rule from wire format bytes.
fn parse_wire_rule(buf: &[u8], protocol: ProtocolVersion) -> io::Result<FilterRuleWireFormat> {
    if buf.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "empty filter rule",
        ));
    }

    let text = std::str::from_utf8(buf).map_err(|e| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("invalid UTF-8 in filter rule: {e}"),
        )
    })?;

    let mut chars = text.chars();
    let first = chars
        .next()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "empty filter rule"))?;

    let rule_type = RuleType::from_prefix_char(first).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("invalid rule type prefix: '{first}'"),
        )
    })?;

    let mut rule = FilterRuleWireFormat {
        rule_type,
        pattern: String::new(),
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
    };

    // Parse modifier flags
    let mut pattern_start = 1;
    for (i, c) in chars.clone().enumerate() {
        match c {
            '/' if i == 0 => {
                rule.anchored = true;
                pattern_start += 1;
            }
            '!' => {
                rule.negate = true;
                pattern_start += 1;
            }
            'C' => {
                rule.cvs_exclude = true;
                pattern_start += 1;
            }
            'n' => {
                rule.no_inherit = true;
                pattern_start += 1;
            }
            'w' => {
                rule.word_split = true;
                pattern_start += 1;
            }
            'e' => {
                rule.exclude_from_merge = true;
                pattern_start += 1;
            }
            'x' => {
                rule.xattr_only = true;
                pattern_start += 1;
            }
            's' if protocol.as_u8() >= 29 => {
                rule.sender_side = true;
                pattern_start += 1;
            }
            'r' if protocol.as_u8() >= 29 => {
                rule.receiver_side = true;
                pattern_start += 1;
            }
            'p' if protocol.as_u8() >= 30 => {
                rule.perishable = true;
                pattern_start += 1;
            }
            ' ' => {
                pattern_start += 1;
                break; // Trailing space ends modifiers
            }
            _ => break, // Start of pattern
        }
    }

    // Extract pattern (remaining text)
    let pattern_text = &text[pattern_start..];

    // Check for trailing slash (directory-only)
    if let Some(stripped) = pattern_text.strip_suffix('/') {
        rule.directory_only = true;
        rule.pattern = stripped.to_string();
    } else {
        rule.pattern = pattern_text.to_string();
    }

    Ok(rule)
}

/// Serializes a filter rule to wire format bytes.
fn serialize_rule(rule: &FilterRuleWireFormat, protocol: ProtocolVersion) -> io::Result<Vec<u8>> {
    let prefix = super::prefix::build_rule_prefix(rule, protocol);
    let mut bytes = prefix.into_bytes();
    bytes.extend_from_slice(rule.pattern.as_bytes());

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

        // Should be single byte zero (varint encoding of 0)
        assert_eq!(buf, vec![0]);

        let rules = read_filter_list(&mut &buf[..], protocol).unwrap();
        assert_eq!(rules, vec![]);
    }

    #[test]
    fn simple_exclude_pattern() {
        let protocol = ProtocolVersion::from_supported(32).unwrap();
        let rule = FilterRuleWireFormat::exclude("*.log".to_string());

        let mut buf = Vec::new();
        write_filter_list(&mut buf, &[rule.clone()], protocol).unwrap();

        let parsed = read_filter_list(&mut &buf[..], protocol).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].rule_type, RuleType::Exclude);
        assert_eq!(parsed[0].pattern, "*.log");
    }

    #[test]
    fn simple_include_pattern() {
        let protocol = ProtocolVersion::from_supported(32).unwrap();
        let rule = FilterRuleWireFormat::include("*.txt".to_string());

        let mut buf = Vec::new();
        write_filter_list(&mut buf, &[rule.clone()], protocol).unwrap();

        let parsed = read_filter_list(&mut &buf[..], protocol).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].rule_type, RuleType::Include);
        assert_eq!(parsed[0].pattern, "*.txt");
    }

    #[test]
    fn anchored_pattern() {
        let protocol = ProtocolVersion::from_supported(32).unwrap();
        let rule = FilterRuleWireFormat::exclude("/tmp".to_string()).with_anchored(true);

        let mut buf = Vec::new();
        write_filter_list(&mut buf, &[rule.clone()], protocol).unwrap();

        let parsed = read_filter_list(&mut &buf[..], protocol).unwrap();
        assert_eq!(parsed.len(), 1);
        assert!(parsed[0].anchored);
        assert_eq!(parsed[0].pattern, "/tmp");
    }

    #[test]
    fn directory_only_pattern() {
        let protocol = ProtocolVersion::from_supported(32).unwrap();
        let rule = FilterRuleWireFormat::exclude("cache".to_string()).with_directory_only(true);

        let mut buf = Vec::new();
        write_filter_list(&mut buf, &[rule.clone()], protocol).unwrap();

        let parsed = read_filter_list(&mut &buf[..], protocol).unwrap();
        assert_eq!(parsed.len(), 1);
        assert!(parsed[0].directory_only);
        assert_eq!(parsed[0].pattern, "cache");
    }

    #[test]
    fn sender_side_filter_v29() {
        let protocol = ProtocolVersion::from_supported(29).unwrap();
        let rule = FilterRuleWireFormat::exclude("*.tmp".to_string()).with_sides(true, false);

        let mut buf = Vec::new();
        write_filter_list(&mut buf, &[rule.clone()], protocol).unwrap();

        let parsed = read_filter_list(&mut &buf[..], protocol).unwrap();
        assert_eq!(parsed.len(), 1);
        assert!(parsed[0].sender_side);
        assert!(!parsed[0].receiver_side);
    }

    #[test]
    fn receiver_side_filter_v29() {
        let protocol = ProtocolVersion::from_supported(29).unwrap();
        let rule = FilterRuleWireFormat::exclude("*.bak".to_string()).with_sides(false, true);

        let mut buf = Vec::new();
        write_filter_list(&mut buf, &[rule.clone()], protocol).unwrap();

        let parsed = read_filter_list(&mut &buf[..], protocol).unwrap();
        assert_eq!(parsed.len(), 1);
        assert!(!parsed[0].sender_side);
        assert!(parsed[0].receiver_side);
    }

    #[test]
    fn perishable_filter_v30() {
        let protocol = ProtocolVersion::from_supported(30).unwrap();
        let rule = FilterRuleWireFormat::exclude("*.swp".to_string()).with_perishable(true);

        let mut buf = Vec::new();
        write_filter_list(&mut buf, &[rule.clone()], protocol).unwrap();

        let parsed = read_filter_list(&mut &buf[..], protocol).unwrap();
        assert_eq!(parsed.len(), 1);
        assert!(parsed[0].perishable);
    }

    #[test]
    fn protocol_downgrade_strips_unsupported() {
        // Create rule with v30 features
        let rule = FilterRuleWireFormat::exclude("test".to_string())
            .with_sides(true, false)
            .with_perishable(true);

        // Serialize with v28 protocol (doesn't support s/r/p)
        let protocol_v28 = ProtocolVersion::from_supported(28).unwrap();
        let mut buf = Vec::new();
        write_filter_list(&mut buf, &[rule], protocol_v28).unwrap();

        // Parse back
        let parsed = read_filter_list(&mut &buf[..], protocol_v28).unwrap();
        assert_eq!(parsed.len(), 1);

        // v28 should ignore s, r, p modifiers
        assert!(!parsed[0].sender_side);
        assert!(!parsed[0].receiver_side);
        assert!(!parsed[0].perishable);
    }
}
