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
    /// Original specification string supplied by the caller.
    ///
    /// Preserved verbatim (after a trim of surrounding whitespace) so the
    /// daemon/SSH client can forward the exact value to the server. Re-parsing
    /// from the rule list would be lossy: a literal `*:1234` would round-trip
    /// to a wire string that no longer signals the wildcard matcher because
    /// the rules vector no longer carries the source representation.
    pub(super) spec: String,
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

        Ok(Self {
            rules,
            kind,
            spec: trimmed.to_owned(),
        })
    }

    /// Returns the original specification string (post-trim) used to construct
    /// this mapping.
    ///
    /// Mirrors upstream rsync's behavior: when the client forwards `--usermap`
    /// or `--groupmap` to a remote server (SSH or daemon), the spec is shipped
    /// verbatim so wildcard characters like `*` survive the round trip.
    #[must_use]
    pub fn spec(&self) -> &str {
        &self.spec
    }

    /// Finds the first matching rule for the given identifier.
    ///
    /// Rules are evaluated in declaration order - first match wins. The
    /// associated name is looked up at most once per call and cached across
    /// rule evaluations to avoid redundant system calls.
    ///
    /// When `numeric_ids` is set, the receiver treats every id as nameless:
    /// upstream never transmits id names in that mode, so `recv_add_id` matches
    /// against an empty name. We therefore skip the local name lookup and
    /// present an empty name, which lets an empty-name matcher (e.g. `:1`)
    /// match every id while named/wildcard matchers match nothing.
    /// upstream: uidlist.c:parse_name_map/recv_add_id under `numeric_ids`.
    pub(super) fn resolve_rule(
        &self,
        identifier: u32,
        numeric_ids: bool,
    ) -> io::Result<Option<&MappingRule>> {
        if self.rules.is_empty() {
            return Ok(None);
        }

        let mut cached_name: Option<Option<String>> = None;
        for rule in &self.rules {
            let matches = rule.matcher.matches(identifier, || {
                if numeric_ids {
                    return Ok(None);
                }
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
    pub(super) fn map_uid(&self, uid: RawUid, numeric_ids: bool) -> io::Result<Option<RawUid>> {
        self.resolve_rule(uid, numeric_ids)?
            .map(|rule| rule.target.resolve_uid())
            .transpose()
    }

    /// Applies the mapping rules to a GID, returning the mapped GID if a rule matches.
    pub(super) fn map_gid(&self, gid: RawGid, numeric_ids: bool) -> io::Result<Option<RawGid>> {
        self.resolve_rule(gid, numeric_ids)?
            .map(|rule| rule.target.resolve_gid())
            .transpose()
    }
}
