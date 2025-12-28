#![allow(unsafe_code)]

use crate::id_lookup::{
    lookup_group_by_name, lookup_group_name, lookup_user_by_name, lookup_user_name,
};
use rustix::process::{RawGid, RawUid};
use std::cmp::Ordering;
use std::io;

use thiserror::Error;

/// Represents the role associated with a name-mapping specification.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum MappingKind {
    /// User mapping specified via `--usermap`.
    #[default]
    User,
    /// Group mapping specified via `--groupmap`.
    Group,
}

impl MappingKind {
    /// Returns the command-line flag associated with the mapping kind.
    #[must_use]
    pub const fn flag(self) -> &'static str {
        match self {
            Self::User => "--usermap",
            Self::Group => "--groupmap",
        }
    }
}

/// Error returned when parsing a mapping specification fails.
#[derive(Clone, Debug, Eq, PartialEq, Error)]
#[error("{message}")]
pub struct MappingParseError {
    kind: MappingKind,
    message: String,
}

impl MappingParseError {
    fn new(kind: MappingKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    /// Returns the mapping kind associated with this error.
    #[must_use]
    pub const fn kind(&self) -> MappingKind {
        self.kind
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum MappingMatcher {
    Any,
    IdRange { start: u32, end: u32 },
    ExactName(String),
    Pattern(String),
}

impl MappingMatcher {
    fn matches<F>(&self, identifier: u32, mut name_lookup: F) -> io::Result<bool>
    where
        F: FnMut() -> io::Result<Option<String>>,
    {
        Ok(match self {
            Self::Any => true,
            Self::IdRange { start, end } => (identifier >= *start) && (identifier <= *end),
            Self::ExactName(expected) => {
                if let Some(name) = name_lookup()? {
                    name == *expected
                } else {
                    false
                }
            }
            Self::Pattern(pattern) => {
                if let Some(name) = name_lookup()? {
                    wildcard_matches(pattern, &name)
                } else {
                    false
                }
            }
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum MappingTarget {
    Id(u32),
    Name(String),
}

impl MappingTarget {
    fn resolve_uid(&self) -> io::Result<RawUid> {
        match self {
            Self::Id(id) => Ok(*id as RawUid),
            Self::Name(name) => match lookup_user_by_name(name.as_bytes())? {
                Some(uid) => Ok(uid),
                None => Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("Unknown --usermap name on receiver: {name}"),
                )),
            },
        }
    }

    fn resolve_gid(&self) -> io::Result<RawGid> {
        match self {
            Self::Id(id) => Ok(*id as RawGid),
            Self::Name(name) => match lookup_group_by_name(name.as_bytes())? {
                Some(gid) => Ok(gid),
                None => Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("Unknown --groupmap name on receiver: {name}"),
                )),
            },
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct MappingRule {
    matcher: MappingMatcher,
    target: MappingTarget,
}

/// Parsed mapping rules associated with `--usermap` or `--groupmap`.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct NameMapping {
    rules: Vec<MappingRule>,
    kind: MappingKind,
}

impl NameMapping {
    /// Parses a mapping specification into a [`NameMapping`].
    pub fn parse(kind: MappingKind, spec: &str) -> Result<Self, MappingParseError> {
        let trimmed = spec.trim();
        if trimmed.is_empty() {
            return Err(MappingParseError::new(
                kind,
                format!("{} requires a non-empty mapping specification", kind.flag()),
            ));
        }

        let mut rules = Vec::new();
        for raw_entry in trimmed.split(',') {
            let entry = raw_entry.trim();
            if entry.is_empty() {
                return Err(MappingParseError::new(
                    kind,
                    format!("{} entries must not be empty", kind.flag()),
                ));
            }

            let (source, target) = entry.split_once(':').ok_or_else(|| {
                MappingParseError::new(
                    kind,
                    format!("No colon found in {}: {}", kind.flag(), entry),
                )
            })?;

            if target.is_empty() {
                return Err(MappingParseError::new(
                    kind,
                    format!("No name found after colon {}: {}", kind.flag(), entry),
                ));
            }

            let matcher = parse_matcher(kind, source.trim(), entry)?;
            let target = parse_target(kind, target.trim(), entry)?;
            rules.push(MappingRule { matcher, target });
        }

        Ok(Self { rules, kind })
    }

    fn resolve_rule(&self, identifier: u32) -> io::Result<Option<&MappingRule>> {
        if self.rules.is_empty() {
            return Ok(None);
        }

        let mut cached_name: Option<Option<String>> = None;
        for rule in &self.rules {
            let matches = rule.matcher.matches(identifier, || {
                if cached_name.is_none() {
                    cached_name = Some(self.lookup_name(identifier)?);
                }
                Ok(cached_name.as_ref().unwrap().clone())
            })?;

            if matches {
                return Ok(Some(rule));
            }
        }

        Ok(None)
    }

    fn lookup_name(&self, identifier: u32) -> io::Result<Option<String>> {
        match self.kind {
            MappingKind::User => lookup_user_name(identifier as RawUid)
                .map(|opt| opt.map(|bytes| String::from_utf8_lossy(&bytes).into_owned())),
            MappingKind::Group => lookup_group_name(identifier as RawGid)
                .map(|opt| opt.map(|bytes| String::from_utf8_lossy(&bytes).into_owned())),
        }
    }

    /// Returns the number of mapping rules.
    #[must_use]
    pub fn len(&self) -> usize {
        self.rules.len()
    }

    /// Reports whether the mapping is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    fn map_uid(&self, uid: RawUid) -> io::Result<Option<RawUid>> {
        self.resolve_rule(uid)?
            .map(|rule| rule.target.resolve_uid())
            .transpose()
    }

    fn map_gid(&self, gid: RawGid) -> io::Result<Option<RawGid>> {
        self.resolve_rule(gid)?
            .map(|rule| rule.target.resolve_gid())
            .transpose()
    }
}

/// Parsed `--usermap` rules.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct UserMapping(NameMapping);

impl UserMapping {
    /// Parses a `--usermap` specification.
    pub fn parse(spec: &str) -> Result<Self, MappingParseError> {
        NameMapping::parse(MappingKind::User, spec).map(Self)
    }

    /// Applies the mapping to the supplied UID.
    pub(crate) fn map_uid(&self, uid: RawUid) -> io::Result<Option<RawUid>> {
        self.0.map_uid(uid)
    }

    /// Reports whether the mapping contains any rules.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// Parsed `--groupmap` rules.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct GroupMapping(NameMapping);

impl GroupMapping {
    /// Parses a `--groupmap` specification.
    pub fn parse(spec: &str) -> Result<Self, MappingParseError> {
        NameMapping::parse(MappingKind::Group, spec).map(Self)
    }

    /// Applies the mapping to the supplied GID.
    pub(crate) fn map_gid(&self, gid: RawGid) -> io::Result<Option<RawGid>> {
        self.0.map_gid(gid)
    }

    /// Reports whether the mapping contains any rules.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl From<NameMapping> for UserMapping {
    fn from(mapping: NameMapping) -> Self {
        Self(mapping)
    }
}

impl From<NameMapping> for GroupMapping {
    fn from(mapping: NameMapping) -> Self {
        Self(mapping)
    }
}

fn parse_matcher(
    kind: MappingKind,
    source: &str,
    _entry: &str,
) -> Result<MappingMatcher, MappingParseError> {
    if source == "*" {
        return Ok(MappingMatcher::Any);
    }

    if let Some((start, end)) = parse_numeric_range(source) {
        return Ok(MappingMatcher::IdRange { start, end });
    }

    if source.chars().any(|ch| matches!(ch, '*' | '?' | '[')) {
        return Ok(MappingMatcher::Pattern(source.to_owned()));
    }

    if source.is_empty() {
        return Err(MappingParseError::new(
            kind,
            format!("{} entries must specify a source selector", kind.flag()),
        ));
    }

    Ok(MappingMatcher::ExactName(source.to_owned()))
}

fn parse_numeric_range(source: &str) -> Option<(u32, u32)> {
    let mut parts = source.split('-');
    let start = parts.next()?;
    if start.is_empty() || !start.chars().all(|ch| ch.is_ascii_digit()) {
        return None;
    }

    if let Some(rest) = parts.next() {
        if rest.is_empty() || !rest.chars().all(|ch| ch.is_ascii_digit()) || parts.next().is_some()
        {
            return None;
        }
        let start_value = start.parse::<u32>().ok()?;
        let end_value = rest.parse::<u32>().ok()?;
        let (start, end) = match start_value.cmp(&end_value) {
            Ordering::Greater => (end_value, start_value),
            _ => (start_value, end_value),
        };
        Some((start, end))
    } else {
        start.parse::<u32>().ok().map(|value| (value, value))
    }
}

fn parse_target(
    kind: MappingKind,
    target: &str,
    entry: &str,
) -> Result<MappingTarget, MappingParseError> {
    if target.is_empty() {
        return Err(MappingParseError::new(
            kind,
            format!("No name found after colon {}: {}", kind.flag(), entry),
        ));
    }

    if target.chars().all(|ch| ch.is_ascii_digit()) {
        return target.parse::<u32>().map(MappingTarget::Id).map_err(|_| {
            MappingParseError::new(
                kind,
                format!("Invalid number in {}: {}", kind.flag(), entry),
            )
        });
    }

    Ok(MappingTarget::Name(target.to_owned()))
}

fn wildcard_matches(pattern: &str, text: &str) -> bool {
    let pattern = pattern.as_bytes();
    let text = text.as_bytes();
    let mut pat_index = 0usize;
    let mut text_index = 0usize;
    let mut star_index: Option<usize> = None;
    let mut match_index = 0usize;

    while text_index < text.len() {
        if pat_index < pattern.len() {
            match pattern[pat_index] {
                b'?' => {
                    pat_index += 1;
                    text_index += 1;
                    continue;
                }
                b'*' => {
                    star_index = Some(pat_index);
                    pat_index += 1;
                    match_index = text_index;
                    continue;
                }
                b'[' => {
                    if let Some((matched, consumed)) =
                        match_bracket(pattern, pat_index, text[text_index])
                    {
                        if matched {
                            pat_index = consumed;
                            text_index += 1;
                            continue;
                        }
                    } else if pattern[pat_index] == text[text_index] {
                        pat_index += 1;
                        text_index += 1;
                        continue;
                    }
                }
                byte if byte == text[text_index] => {
                    pat_index += 1;
                    text_index += 1;
                    continue;
                }
                _ => {}
            }
        }

        if let Some(star_pos) = star_index {
            pat_index = star_pos + 1;
            match_index += 1;
            text_index = match_index;
        } else {
            return false;
        }
    }

    while pat_index < pattern.len() && pattern[pat_index] == b'*' {
        pat_index += 1;
    }

    pat_index == pattern.len()
}

fn match_bracket(pattern: &[u8], start: usize, byte: u8) -> Option<(bool, usize)> {
    let mut index = start + 1;
    if index >= pattern.len() {
        return None;
    }

    let mut negate = false;
    if pattern[index] == b'!' || pattern[index] == b'^' {
        negate = true;
        index += 1;
    }

    let mut matched = false;
    let mut first = true;

    while index < pattern.len() {
        let mut current = pattern[index];
        if current == b']' && !first {
            let result = if negate { !matched } else { matched };
            return Some((result, index + 1));
        }

        if current == b'\\' && index + 1 < pattern.len() {
            index += 1;
            current = pattern[index];
        }

        if index + 2 < pattern.len() && pattern[index + 1] == b'-' {
            let mut end_index = index + 2;
            let mut end = pattern[end_index];
            if end == b'\\' && end_index + 1 < pattern.len() {
                end_index += 1;
                end = pattern[end_index];
            }

            if end_index < pattern.len() {
                if current <= byte && byte <= end {
                    matched = true;
                }
                index = end_index + 1;
                first = false;
                continue;
            }
        }

        if current == b']' && first {
            if byte == current {
                matched = true;
            }
            index += 1;
            first = false;
            continue;
        }

        if byte == current {
            matched = true;
        }
        index += 1;
        first = false;
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    // MappingKind tests
    #[test]
    fn mapping_kind_user_flag() {
        assert_eq!(MappingKind::User.flag(), "--usermap");
    }

    #[test]
    fn mapping_kind_group_flag() {
        assert_eq!(MappingKind::Group.flag(), "--groupmap");
    }

    #[test]
    fn mapping_kind_default() {
        let kind: MappingKind = Default::default();
        assert_eq!(kind, MappingKind::User);
    }

    #[test]
    fn mapping_kind_clone() {
        let kind = MappingKind::Group;
        let cloned = kind;
        assert_eq!(cloned, MappingKind::Group);
    }

    #[test]
    fn mapping_kind_debug() {
        let kind = MappingKind::User;
        let debug = format!("{kind:?}");
        assert!(debug.contains("User"));
    }

    // MappingParseError tests
    #[test]
    fn mapping_parse_error_kind() {
        let error = MappingParseError::new(MappingKind::Group, "test error");
        assert_eq!(error.kind(), MappingKind::Group);
    }

    #[test]
    fn mapping_parse_error_display() {
        let error = MappingParseError::new(MappingKind::User, "custom error message");
        assert_eq!(error.to_string(), "custom error message");
    }

    #[test]
    fn mapping_parse_error_debug() {
        let error = MappingParseError::new(MappingKind::User, "test");
        let debug = format!("{error:?}");
        assert!(debug.contains("MappingParseError"));
    }

    #[test]
    fn mapping_parse_error_clone() {
        let error = MappingParseError::new(MappingKind::User, "test");
        let cloned = error.clone();
        assert_eq!(cloned, error);
    }

    // NameMapping parsing tests
    #[test]
    fn parse_numeric_usermap() {
        let mapping = NameMapping::parse(MappingKind::User, "100:200").expect("parse mapping");
        assert_eq!(mapping.len(), 1);
        assert!(!mapping.is_empty());
    }

    #[test]
    fn parse_rejects_invalid_number() {
        let error = NameMapping::parse(MappingKind::User, "abc:999999999999999999999999999999")
            .unwrap_err();
        assert!(error.to_string().contains("Invalid number"));
    }

    #[test]
    fn parse_empty_spec_fails() {
        let error = NameMapping::parse(MappingKind::User, "").unwrap_err();
        assert!(error.to_string().contains("requires a non-empty"));
    }

    #[test]
    fn parse_whitespace_only_fails() {
        let error = NameMapping::parse(MappingKind::User, "   ").unwrap_err();
        assert!(error.to_string().contains("requires a non-empty"));
    }

    #[test]
    fn parse_empty_entry_fails() {
        let error = NameMapping::parse(MappingKind::User, "100:200,,300:400").unwrap_err();
        assert!(error.to_string().contains("must not be empty"));
    }

    #[test]
    fn parse_no_colon_fails() {
        let error = NameMapping::parse(MappingKind::User, "100-200").unwrap_err();
        assert!(error.to_string().contains("No colon found"));
    }

    #[test]
    fn parse_empty_target_fails() {
        let error = NameMapping::parse(MappingKind::User, "100:").unwrap_err();
        assert!(error.to_string().contains("No name found after colon"));
    }

    #[test]
    fn parse_wildcard_source() {
        let mapping = NameMapping::parse(MappingKind::User, "*:0").expect("parse mapping");
        assert_eq!(mapping.len(), 1);
    }

    #[test]
    fn parse_range_source() {
        let mapping = NameMapping::parse(MappingKind::User, "100-200:1000").expect("parse mapping");
        assert_eq!(mapping.len(), 1);
    }

    #[test]
    fn parse_pattern_source() {
        let mapping = NameMapping::parse(MappingKind::User, "test*:nobody").expect("parse mapping");
        assert_eq!(mapping.len(), 1);
    }

    #[test]
    fn parse_exact_name_source() {
        let mapping = NameMapping::parse(MappingKind::User, "testuser:0").expect("parse mapping");
        assert_eq!(mapping.len(), 1);
    }

    #[test]
    fn parse_multiple_rules() {
        let mapping =
            NameMapping::parse(MappingKind::User, "100:200, 300:400, *:0").expect("parse mapping");
        assert_eq!(mapping.len(), 3);
    }

    #[test]
    fn parse_empty_source_fails() {
        let error = NameMapping::parse(MappingKind::User, ":100").unwrap_err();
        assert!(error.to_string().contains("must specify a source"));
    }

    #[test]
    fn parse_target_as_name() {
        let mapping = NameMapping::parse(MappingKind::User, "100:nobody").expect("parse mapping");
        assert_eq!(mapping.len(), 1);
    }

    // parse_numeric_range tests
    #[test]
    fn numeric_range_single_value() {
        assert_eq!(parse_numeric_range("100"), Some((100, 100)));
    }

    #[test]
    fn numeric_range_two_values() {
        assert_eq!(parse_numeric_range("100-200"), Some((100, 200)));
    }

    #[test]
    fn numeric_range_reversed_values() {
        // When start > end, they get swapped
        assert_eq!(parse_numeric_range("200-100"), Some((100, 200)));
    }

    #[test]
    fn numeric_range_empty_fails() {
        assert_eq!(parse_numeric_range(""), None);
    }

    #[test]
    fn numeric_range_non_numeric_fails() {
        assert_eq!(parse_numeric_range("abc"), None);
    }

    #[test]
    fn numeric_range_empty_start_fails() {
        assert_eq!(parse_numeric_range("-100"), None);
    }

    #[test]
    fn numeric_range_empty_end_fails() {
        assert_eq!(parse_numeric_range("100-"), None);
    }

    #[test]
    fn numeric_range_non_numeric_end_fails() {
        assert_eq!(parse_numeric_range("100-abc"), None);
    }

    #[test]
    fn numeric_range_triple_range_fails() {
        assert_eq!(parse_numeric_range("100-200-300"), None);
    }

    // wildcard_matches tests
    #[test]
    fn wildcard_matches_pattern() {
        assert!(wildcard_matches("ab*", "abc"));
        assert!(wildcard_matches("a?c", "abc"));
        assert!(!wildcard_matches("a?d", "abc"));
    }

    #[test]
    fn wildcard_matches_exact() {
        assert!(wildcard_matches("abc", "abc"));
        assert!(!wildcard_matches("abc", "abd"));
    }

    #[test]
    fn wildcard_matches_star_anywhere() {
        assert!(wildcard_matches("*abc", "xyzabc"));
        assert!(wildcard_matches("abc*", "abcxyz"));
        assert!(wildcard_matches("*abc*", "xyzabcdef"));
    }

    #[test]
    fn wildcard_matches_multiple_stars() {
        assert!(wildcard_matches("a*b*c", "aXYZbXYZc"));
        assert!(wildcard_matches("*a*b*", "xaxbx"));
    }

    #[test]
    fn wildcard_matches_question_mark() {
        assert!(wildcard_matches("a?c", "abc"));
        assert!(wildcard_matches("???", "abc"));
        assert!(!wildcard_matches("???", "ab"));
        assert!(!wildcard_matches("???", "abcd"));
    }

    #[test]
    fn wildcard_matches_bracket_simple() {
        assert!(wildcard_matches("a[bc]d", "abd"));
        assert!(wildcard_matches("a[bc]d", "acd"));
        assert!(!wildcard_matches("a[bc]d", "aed"));
    }

    #[test]
    fn wildcard_matches_bracket_range() {
        assert!(wildcard_matches("a[a-z]c", "abc"));
        assert!(wildcard_matches("a[0-9]c", "a5c"));
        assert!(!wildcard_matches("a[a-z]c", "a5c"));
    }

    #[test]
    fn wildcard_matches_bracket_negation() {
        assert!(wildcard_matches("a[!b]c", "adc"));
        assert!(!wildcard_matches("a[!b]c", "abc"));
        assert!(wildcard_matches("a[^b]c", "adc"));
        assert!(!wildcard_matches("a[^b]c", "abc"));
    }

    #[test]
    fn wildcard_matches_empty_pattern() {
        assert!(wildcard_matches("", ""));
        assert!(!wildcard_matches("", "abc"));
    }

    #[test]
    fn wildcard_matches_only_star() {
        assert!(wildcard_matches("*", ""));
        assert!(wildcard_matches("*", "anything"));
    }

    #[test]
    fn wildcard_matches_trailing_stars() {
        assert!(wildcard_matches("abc***", "abc"));
    }

    #[test]
    fn wildcard_no_match_shorter_text() {
        assert!(!wildcard_matches("abcd", "abc"));
    }

    // match_bracket tests
    #[test]
    fn match_bracket_simple() {
        assert_eq!(match_bracket(b"[abc]", 0, b'a'), Some((true, 5)));
        assert_eq!(match_bracket(b"[abc]", 0, b'b'), Some((true, 5)));
        assert_eq!(match_bracket(b"[abc]", 0, b'd'), Some((false, 5)));
    }

    #[test]
    fn match_bracket_negated() {
        assert_eq!(match_bracket(b"[!abc]", 0, b'd'), Some((true, 6)));
        assert_eq!(match_bracket(b"[!abc]", 0, b'a'), Some((false, 6)));
        assert_eq!(match_bracket(b"[^abc]", 0, b'd'), Some((true, 6)));
    }

    #[test]
    fn match_bracket_range() {
        assert_eq!(match_bracket(b"[a-z]", 0, b'm'), Some((true, 5)));
        assert_eq!(match_bracket(b"[a-z]", 0, b'0'), Some((false, 5)));
    }

    #[test]
    fn match_bracket_unclosed() {
        assert_eq!(match_bracket(b"[abc", 0, b'a'), None);
    }

    #[test]
    fn match_bracket_empty() {
        assert_eq!(match_bracket(b"[", 0, b'a'), None);
    }

    #[test]
    fn match_bracket_literal_close() {
        // First character can be a literal ]
        assert_eq!(match_bracket(b"[]abc]", 0, b']'), Some((true, 6)));
    }

    #[test]
    fn match_bracket_escaped() {
        assert_eq!(match_bracket(b"[\\]a]", 0, b']'), Some((true, 5)));
    }

    #[test]
    fn match_bracket_escaped_in_range() {
        // Range with escaped end
        assert_eq!(match_bracket(b"[a-\\z]", 0, b'z'), Some((true, 6)));
    }

    // UserMapping tests
    #[test]
    fn user_mapping_parse() {
        let mapping = UserMapping::parse("100:200").expect("parse");
        assert!(!mapping.is_empty());
    }

    #[test]
    fn user_mapping_parse_error() {
        let error = UserMapping::parse("").unwrap_err();
        assert_eq!(error.kind(), MappingKind::User);
    }

    #[test]
    fn user_mapping_default() {
        let mapping = UserMapping::default();
        assert!(mapping.is_empty());
    }

    #[test]
    fn user_mapping_from_name_mapping() {
        let name_mapping = NameMapping::parse(MappingKind::User, "100:200").unwrap();
        let user_mapping: UserMapping = name_mapping.into();
        assert!(!user_mapping.is_empty());
    }

    // GroupMapping tests
    #[test]
    fn group_mapping_parse() {
        let mapping = GroupMapping::parse("100:200").expect("parse");
        assert!(!mapping.is_empty());
    }

    #[test]
    fn group_mapping_parse_error() {
        let error = GroupMapping::parse("").unwrap_err();
        assert_eq!(error.kind(), MappingKind::Group);
    }

    #[test]
    fn group_mapping_default() {
        let mapping = GroupMapping::default();
        assert!(mapping.is_empty());
    }

    #[test]
    fn group_mapping_from_name_mapping() {
        let name_mapping = NameMapping::parse(MappingKind::Group, "100:200").unwrap();
        let group_mapping: GroupMapping = name_mapping.into();
        assert!(!group_mapping.is_empty());
    }

    // NameMapping clone and debug
    #[test]
    fn name_mapping_clone() {
        let mapping = NameMapping::parse(MappingKind::User, "100:200").unwrap();
        let cloned = mapping.clone();
        assert_eq!(cloned.len(), mapping.len());
    }

    #[test]
    fn name_mapping_debug() {
        let mapping = NameMapping::parse(MappingKind::User, "100:200").unwrap();
        let debug = format!("{mapping:?}");
        assert!(debug.contains("NameMapping"));
    }

    #[test]
    fn name_mapping_default() {
        let mapping = NameMapping::default();
        assert!(mapping.is_empty());
        assert_eq!(mapping.len(), 0);
    }

    // MappingTarget tests
    #[test]
    fn mapping_target_id() {
        let target = MappingTarget::Id(100);
        let uid = target.resolve_uid().unwrap();
        assert_eq!(uid, 100);
    }

    #[test]
    fn mapping_target_id_as_gid() {
        let target = MappingTarget::Id(100);
        let gid = target.resolve_gid().unwrap();
        assert_eq!(gid, 100);
    }

    // MappingMatcher tests
    #[test]
    fn mapping_matcher_any() {
        let matcher = MappingMatcher::Any;
        let result = matcher
            .matches(12345, || Ok(Some("test".to_owned())))
            .unwrap();
        assert!(result);
    }

    #[test]
    fn mapping_matcher_id_range_in_range() {
        let matcher = MappingMatcher::IdRange {
            start: 100,
            end: 200,
        };
        assert!(matcher.matches(150, || Ok(None)).unwrap());
        assert!(matcher.matches(100, || Ok(None)).unwrap());
        assert!(matcher.matches(200, || Ok(None)).unwrap());
    }

    #[test]
    fn mapping_matcher_id_range_out_of_range() {
        let matcher = MappingMatcher::IdRange {
            start: 100,
            end: 200,
        };
        assert!(!matcher.matches(50, || Ok(None)).unwrap());
        assert!(!matcher.matches(250, || Ok(None)).unwrap());
    }

    #[test]
    fn mapping_matcher_exact_name_match() {
        let matcher = MappingMatcher::ExactName("testuser".to_owned());
        let result = matcher
            .matches(1000, || Ok(Some("testuser".to_owned())))
            .unwrap();
        assert!(result);
    }

    #[test]
    fn mapping_matcher_exact_name_no_match() {
        let matcher = MappingMatcher::ExactName("testuser".to_owned());
        let result = matcher
            .matches(1000, || Ok(Some("otheruser".to_owned())))
            .unwrap();
        assert!(!result);
    }

    #[test]
    fn mapping_matcher_exact_name_no_name() {
        let matcher = MappingMatcher::ExactName("testuser".to_owned());
        let result = matcher.matches(1000, || Ok(None)).unwrap();
        assert!(!result);
    }

    #[test]
    fn mapping_matcher_pattern_match() {
        let matcher = MappingMatcher::Pattern("test*".to_owned());
        let result = matcher
            .matches(1000, || Ok(Some("testuser".to_owned())))
            .unwrap();
        assert!(result);
    }

    #[test]
    fn mapping_matcher_pattern_no_match() {
        let matcher = MappingMatcher::Pattern("test*".to_owned());
        let result = matcher
            .matches(1000, || Ok(Some("otheruser".to_owned())))
            .unwrap();
        assert!(!result);
    }

    #[test]
    fn mapping_matcher_pattern_no_name() {
        let matcher = MappingMatcher::Pattern("test*".to_owned());
        let result = matcher.matches(1000, || Ok(None)).unwrap();
        assert!(!result);
    }

    #[test]
    fn mapping_matcher_clone() {
        let matcher = MappingMatcher::IdRange {
            start: 100,
            end: 200,
        };
        let cloned = matcher.clone();
        assert_eq!(cloned, matcher);
    }

    #[test]
    fn mapping_matcher_debug() {
        let matcher = MappingMatcher::Any;
        let debug = format!("{matcher:?}");
        assert!(debug.contains("Any"));
    }

    // parse_matcher tests
    #[test]
    fn parse_matcher_star() {
        let matcher = parse_matcher(MappingKind::User, "*", "*:0").unwrap();
        assert!(matches!(matcher, MappingMatcher::Any));
    }

    #[test]
    fn parse_matcher_range() {
        let matcher = parse_matcher(MappingKind::User, "100-200", "100-200:0").unwrap();
        assert!(matches!(matcher, MappingMatcher::IdRange { .. }));
    }

    #[test]
    fn parse_matcher_single_id() {
        let matcher = parse_matcher(MappingKind::User, "100", "100:0").unwrap();
        assert!(matches!(
            matcher,
            MappingMatcher::IdRange {
                start: 100,
                end: 100
            }
        ));
    }

    #[test]
    fn parse_matcher_pattern_star() {
        let matcher = parse_matcher(MappingKind::User, "test*", "test*:0").unwrap();
        assert!(matches!(matcher, MappingMatcher::Pattern(_)));
    }

    #[test]
    fn parse_matcher_pattern_question() {
        let matcher = parse_matcher(MappingKind::User, "test?", "test?:0").unwrap();
        assert!(matches!(matcher, MappingMatcher::Pattern(_)));
    }

    #[test]
    fn parse_matcher_pattern_bracket() {
        let matcher = parse_matcher(MappingKind::User, "test[abc]", "test[abc]:0").unwrap();
        assert!(matches!(matcher, MappingMatcher::Pattern(_)));
    }

    #[test]
    fn parse_matcher_exact_name() {
        let matcher = parse_matcher(MappingKind::User, "testuser", "testuser:0").unwrap();
        assert!(matches!(matcher, MappingMatcher::ExactName(_)));
    }

    #[test]
    fn parse_matcher_empty_fails() {
        let error = parse_matcher(MappingKind::User, "", ":0").unwrap_err();
        assert!(error.to_string().contains("must specify a source"));
    }

    // parse_target tests
    #[test]
    fn parse_target_numeric() {
        let target = parse_target(MappingKind::User, "100", "x:100").unwrap();
        assert!(matches!(target, MappingTarget::Id(100)));
    }

    #[test]
    fn parse_target_name() {
        let target = parse_target(MappingKind::User, "nobody", "x:nobody").unwrap();
        assert!(matches!(target, MappingTarget::Name(_)));
    }

    #[test]
    fn parse_target_empty_fails() {
        let error = parse_target(MappingKind::User, "", "x:").unwrap_err();
        assert!(error.to_string().contains("No name found after colon"));
    }
}
