#![allow(unsafe_code)]

//! Core [`NameMapping`] type that holds parsed mapping rules.
//!
//! Provides rule evaluation with lazy name caching and resolution to
//! concrete UIDs/GIDs. Used as the backing store for both [`super::UserMapping`]
//! and [`super::GroupMapping`].

use crate::id_lookup::{lookup_group_name, lookup_user_name};
use rustix::process::{RawGid, RawUid};
use std::io;

use super::parse::{parse_matcher, parse_target};
use super::types::{MappingKind, MappingParseError, MappingRule};

/// Parsed mapping rules associated with `--usermap` or `--groupmap`.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct NameMapping {
    pub(super) rules: Vec<MappingRule>,
    pub(super) kind: MappingKind,
}

impl NameMapping {
    /// Parses a mapping specification into a [`NameMapping`].
    ///
    /// The specification is a comma-separated list of `source:target` entries
    /// where `source` can be a name, numeric ID, range, or wildcard pattern,
    /// and `target` is a name or numeric ID.
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

    /// Finds the first matching rule for the given identifier.
    ///
    /// Caches the name lookup result across rule evaluations to avoid
    /// redundant system calls.
    pub(super) fn resolve_rule(&self, identifier: u32) -> io::Result<Option<&MappingRule>> {
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

    /// Resolves an identifier to a name using the appropriate system lookup.
    fn lookup_name(&self, identifier: u32) -> io::Result<Option<String>> {
        match self.kind {
            MappingKind::User => lookup_user_name(identifier as RawUid).map(|opt| {
                opt.map(|bytes| match String::from_utf8(bytes) {
                    Ok(s) => s,
                    Err(e) => String::from_utf8_lossy(e.as_bytes()).into_owned(),
                })
            }),
            MappingKind::Group => lookup_group_name(identifier as RawGid).map(|opt| {
                opt.map(|bytes| match String::from_utf8(bytes) {
                    Ok(s) => s,
                    Err(e) => String::from_utf8_lossy(e.as_bytes()).into_owned(),
                })
            }),
        }
    }

    /// Returns the number of mapping rules.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.rules.len()
    }

    /// Reports whether the mapping is empty.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    /// Applies the mapping rules to a UID, returning the mapped UID if a rule matches.
    pub(super) fn map_uid(&self, uid: RawUid) -> io::Result<Option<RawUid>> {
        self.resolve_rule(uid)?
            .map(|rule| rule.target.resolve_uid())
            .transpose()
    }

    /// Applies the mapping rules to a GID, returning the mapped GID if a rule matches.
    pub(super) fn map_gid(&self, gid: RawGid) -> io::Result<Option<RawGid>> {
        self.resolve_rule(gid)?
            .map(|rule| rule.target.resolve_gid())
            .transpose()
    }
}
