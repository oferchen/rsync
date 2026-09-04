//! The rule line under parse, paired with where its text came from.
//!
//! upstream keeps these two together implicitly: `*rulestr_ptr` is the line and
//! the file-scope `rule_src_*` statics record whether the parser is currently
//! reading a file's contents (`exclude.c:56-68`). Every diagnostic built from
//! the line then crosses one of two chokepoints - `rule_text` for the text
//! itself and `rule_detail` for the extra detail *about* the text
//! (`exclude.c:88-131`).
//!
//! oc has no ambient parser state, so the pair is passed explicitly. Bundling
//! them in one value is what preserves upstream's stated property: "Doing it
//! here rather than at each site is the point: a message added later cannot
//! reintroduce the leak by forgetting to check, and there is one place to
//! audit." (`exclude.c:96-99`).

use std::borrow::Cow;

use filters::RuleSource;

/// A filter-rule line together with the provenance of its text.
///
/// `text()` is the line as the parser must see it - byte-exact, never redacted,
/// because parsing decisions depend on it. `shown()` is the line as a
/// diagnostic may render it, which for a rule read out of a file's contents is
/// a description of where it came from rather than the line itself.
#[derive(Clone, Copy)]
pub(super) struct RuleLine<'a> {
    text: &'a str,
    source: RuleSource<'a>,
}

impl<'a> RuleLine<'a> {
    /// Pairs a rule line with the provenance of its text.
    pub(super) fn new(text: &'a str, source: RuleSource<'a>) -> Self {
        Self { text, source }
    }

    /// The line verbatim, for parsing. Never render this into a message.
    pub(super) fn text(&self) -> &'a str {
        self.text
    }

    /// The line as a diagnostic may render it.
    ///
    /// upstream: `rule_text` (exclude.c:103-124). An argument-sourced rule is
    /// returned unchanged, "because it is the user's own and hiding it only
    /// makes typos harder to fix"; a rule out of a file's contents is replaced,
    /// "because the peer chooses which file gets merged and any line of it that
    /// reaches a message is a line the peer can read back".
    pub(super) fn shown(&self) -> Cow<'a, str> {
        self.source.rule_text(self.text)
    }

    /// Extra detail *about* the text - a character of it, an offset into it.
    ///
    /// upstream: `rule_detail` (exclude.c:126-131) - "Dropped along with the
    /// text it describes." Reporting a character of a hidden line would leak it
    /// one byte at a time.
    pub(super) fn detail<'d>(&self, detail: &'d str) -> &'d str {
        self.source.rule_detail(detail)
    }
}
