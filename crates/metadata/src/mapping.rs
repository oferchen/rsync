#![allow(unsafe_code)]

use crate::id_lookup::{
    lookup_group_by_name, lookup_group_name, lookup_user_by_name, lookup_user_name,
};
use rustix::process::{RawGid, RawUid};
use std::cmp::Ordering;
use std::fmt;
use std::io;

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
#[derive(Clone, Debug, Eq, PartialEq)]
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

impl fmt::Display for MappingParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for MappingParseError {}

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
        return Ok(MappingMatcher::Pattern(source.to_string()));
    }

    if source.is_empty() {
        return Err(MappingParseError::new(
            kind,
            format!("{} entries must specify a source selector", kind.flag()),
        ));
    }

    Ok(MappingMatcher::ExactName(source.to_string()))
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

    Ok(MappingTarget::Name(target.to_string()))
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
    fn wildcard_matches_pattern() {
        assert!(wildcard_matches("ab*", "abc"));
        assert!(wildcard_matches("a?c", "abc"));
        assert!(!wildcard_matches("a?d", "abc"));
    }
}
