//! Provenance of the text of a filter rule.
//!
//! upstream: `exclude.c:49-131`. A rule that fails to parse used to be echoed
//! back verbatim, and the peer chooses which file gets merged - a per-directory
//! merge rule travels over the protocol, so no argument of ours ever names it.
//! That made the filter parser a read-any-line oracle: any line that is not
//! valid filter syntax came straight back in the error. 3.5.0 reports *where*
//! the bad rule is instead of *what it says*.
//!
//! The rule is deliberately not "redact everything". Text that came from the
//! operator's own argument is returned unchanged, "because it is the user's own
//! and hiding it only makes typos harder to fix" (`exclude.c:90-95`); only text
//! that came out of a file's contents is replaced.
//!
//! Upstream expresses this with four module-globals (`rule_src_in_file`,
//! `rule_src_file`, `rule_src_line`, `rule_src_named_at`) plus a per-rule
//! `FILTRULE_FROM_FILE` bit, read through the `TEXT_FROM_FILE` macro. Those are
//! process state because C has no better option for a value threaded through a
//! recursive descent; here the same information is a value passed down the
//! parse, which is both thread-safe and impossible to leave stale.
//!
//! Design note: upstream's `rule_text_len` returns one of two rotated static
//! buffers so that two calls in a single `rprintf` do not alias
//! (`exclude.c:104-109`). Owned/borrowed returns make that unnecessary here,
//! but the property it protects is real - five upstream messages interpolate
//! two of these calls - so it is pinned by test rather than assumed.

use std::borrow::Cow;

use crate::action::FilterAction;

/// Where the text of a filter rule came from.
///
/// upstream: the `TEXT_FROM_FILE` predicate (`exclude.c:67-69`) plus the four
/// arms of `rule_src_where` (`exclude.c:72-86`). [`Self::Argument`] is
/// upstream's "not from a file" case; the other four are its four descriptions,
/// in the order upstream tests them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuleSource<'a> {
    /// The operator's own command line. Text is shown verbatim.
    Argument,
    /// A file's contents, where the file's own name is safe to show and the
    /// position is a line count.
    ///
    /// upstream: `"%s line %d"`.
    File {
        /// The merge file's name, as the operator or the walk named it.
        name: &'a str,
        /// 1-indexed physical line, counting comments and blank lines.
        line: usize,
    },
    /// A file's contents, where the name is safe to show but the position is
    /// not a line count - a word-split merge file yields tokens, not lines.
    ///
    /// upstream: `rule_src_line < 0`, which returns the file name alone.
    FileWordSplit {
        /// The merge file's name.
        name: &'a str,
    },
    /// A file's contents, where the file's OWN name came from a file and so
    /// must not be printed, but where it was named is known and printable.
    ///
    /// upstream: `"a file named at %s"`.
    FileNamedAt {
        /// Description of the location that named this file.
        location: &'a str,
    },
    /// A file's contents whose origin was not retained - a deferred per-dir
    /// merge, whose naming file was read and closed long ago.
    ///
    /// upstream: `"a file read earlier"`.
    FileReadEarlier,
}

impl<'a> RuleSource<'a> {
    /// Reports whether the text came from a file's contents.
    ///
    /// upstream: `TEXT_FROM_FILE` (`exclude.c:67-69`).
    #[must_use]
    pub const fn is_from_file(&self) -> bool {
        !matches!(self, Self::Argument)
    }

    /// Describes where the rule came from, for interpolation into
    /// `<rule from %s>`.
    ///
    /// upstream: `rule_src_where` (`exclude.c:72-86`). Returns `None` for
    /// [`Self::Argument`], which has no such description because its text is
    /// never replaced.
    #[must_use]
    pub fn describe(&self) -> Option<Cow<'a, str>> {
        match *self {
            Self::Argument => None,
            Self::File { name, line } => Some(Cow::Owned(format!("{name} line {line}"))),
            Self::FileWordSplit { name } => Some(Cow::Borrowed(name)),
            Self::FileNamedAt { location } => {
                Some(Cow::Owned(format!("a file named at {location}")))
            }
            Self::FileReadEarlier => Some(Cow::Borrowed("a file read earlier")),
        }
    }

    /// Returns the rule text to show in a diagnostic.
    ///
    /// Argument-sourced text is returned unchanged; file-sourced text is
    /// replaced by `<rule from ...>`.
    ///
    /// upstream: `rule_text` / `rule_text_len` (`exclude.c:88-123`), the
    /// chokepoint every diagnostic built from a rule's own text must pass
    /// through.
    #[must_use]
    pub fn rule_text<'t>(&self, text: &'t str) -> Cow<'t, str>
    where
        'a: 't,
    {
        match self.describe() {
            None => Cow::Borrowed(text),
            Some(where_from) => Cow::Owned(format!("<rule from {where_from}>")),
        }
    }

    /// Returns extra detail *about* the rule text - a character of it, an
    /// offset into it - or the empty string when the text itself is redacted.
    ///
    /// upstream: `rule_detail` (`exclude.c:127-131`): "For the extra detail
    /// some messages add ABOUT the text - a character of it, an offset into it.
    /// Dropped along with the text it describes." Without this, a message like
    /// `invalid modifier 'k' at position 1` leaks the file's contents one byte
    /// at a time, which is a finer-grained oracle than echoing the whole line.
    #[must_use]
    pub fn rule_detail<'d>(&self, detail: &'d str) -> &'d str {
        if self.is_from_file() { "" } else { detail }
    }
}

/// An owned [`RuleSource`], retained on a rule so its provenance outlives the
/// parse that produced it.
///
/// upstream keeps the same information on the rule itself - the
/// `FILTRULE_FROM_FILE` bit plus the retained `elt` location
/// (`exclude.c:259-275`) - and it has to live there because the redaction
/// decision is not always made at parse time. `report_filter_result`
/// (`exclude.c:1099`) runs when a rule MATCHES A PATH, long after the file that
/// supplied the rule was read and closed; a deferred per-directory merge is
/// parsed later still. [`RuleSource`] borrows its strings from the parse, so it
/// cannot serve either case.
///
/// This type is storage only. Every redaction decision stays in [`RuleSource`],
/// reached through [`Self::as_source`], so the two cannot drift apart.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum OwnedRuleSource {
    /// See [`RuleSource::Argument`].
    #[default]
    Argument,
    /// See [`RuleSource::File`].
    File {
        /// The merge file's name.
        name: String,
        /// 1-indexed physical line.
        line: usize,
    },
    /// See [`RuleSource::FileWordSplit`].
    FileWordSplit {
        /// The merge file's name.
        name: String,
    },
    /// See [`RuleSource::FileNamedAt`].
    FileNamedAt {
        /// Description of the location that named this file.
        location: String,
    },
    /// See [`RuleSource::FileReadEarlier`].
    FileReadEarlier,
}

impl OwnedRuleSource {
    /// Borrows this provenance as a [`RuleSource`], which owns every redaction
    /// rule - storage and policy each stay in exactly one place.
    #[must_use]
    pub fn as_source(&self) -> RuleSource<'_> {
        match self {
            Self::Argument => RuleSource::Argument,
            Self::File { name, line } => RuleSource::File { name, line: *line },
            Self::FileWordSplit { name } => RuleSource::FileWordSplit { name },
            Self::FileNamedAt { location } => RuleSource::FileNamedAt { location },
            Self::FileReadEarlier => RuleSource::FileReadEarlier,
        }
    }

    /// Reports whether the text came from a file's contents.
    ///
    /// upstream: `TEXT_FROM_FILE` (`exclude.c:67-69`) read through the rule's
    /// own `FILTRULE_FROM_FILE` bit rather than the parser's module state.
    #[must_use]
    pub fn is_from_file(&self) -> bool {
        self.as_source().is_from_file()
    }
}

impl From<RuleSource<'_>> for OwnedRuleSource {
    fn from(source: RuleSource<'_>) -> Self {
        match source {
            RuleSource::Argument => Self::Argument,
            RuleSource::File { name, line } => Self::File {
                name: name.to_owned(),
                line,
            },
            RuleSource::FileWordSplit { name } => Self::FileWordSplit {
                name: name.to_owned(),
            },
            RuleSource::FileNamedAt { location } => Self::FileNamedAt {
                location: location.to_owned(),
            },
            RuleSource::FileReadEarlier => Self::FileReadEarlier,
        }
    }
}

/// The action prefix upstream renders ahead of a rule's text.
///
/// upstream: `get_rule_prefix` (`exclude.c`), whose result `add_rule` passes to
/// `rule_detail`. The trailing space belongs to the prefix, so a redacted rule
/// renders as `add_rule(<rule from ...>)` with no leading gap.
fn action_prefix(action: FilterAction) -> &'static str {
    match action {
        FilterAction::Include => "+ ",
        FilterAction::Exclude => "- ",
        FilterAction::Protect => "P ",
        FilterAction::Risk => "R ",
        FilterAction::Clear => "! ",
        FilterAction::Merge => "merge ",
        FilterAction::DirMerge => "dir-merge ",
    }
}

/// The `--debug=FILTER` level a rule prints at, and the suffix it prints with.
///
/// upstream: `add_rule` (`exclude.c:265-269`) gates the trace on *two* levels,
/// not one. A rule whose last byte is a space or a tab is worth flagging on its
/// own, so it prints from FILTER1 carrying a caution; everything else waits for
/// FILTER2 and prints with an empty suffix. Below FILTER1 nothing prints.
///
/// The whitespace test reads the rule's raw text, so an empty pattern can never
/// take the caution arm - matching upstream's leading `pat_len &&`.
fn mention_rule_suffix(pattern: &str) -> Option<(u8, &'static str)> {
    if logging::debug_gte(logging::DebugFlag::Filter, 1) && pattern.ends_with([' ', '\t']) {
        return Some((1, " -- CAUTION: trailing whitespace!"));
    }
    if logging::debug_gte(logging::DebugFlag::Filter, 2) {
        return Some((2, ""));
    }
    None
}

/// Logs a parsed rule under upstream's two-level `--debug=FILTER` gate,
/// redacting text - and details *about* text - that came from a file's
/// contents.
///
/// upstream: `add_rule` (`exclude.c:265-275`) routes all three variable halves
/// through redactors - `rule_text_len` replaces a file-sourced pattern with
/// `<rule from ...>`, and `rule_detail` drops both the action prefix and the
/// trailing-whitespace caution, because each describes text the peer is no
/// longer being shown. A caution on a hidden rule would report whether that
/// rule ends in a space, which is exactly the kind of fact the redaction
/// exists to withhold.
///
/// This is called from the parser rather than from rule compilation because
/// that is where upstream calls it: `add_rule` runs with the rule's provenance
/// in hand. Emitting downstream of the parse, where provenance has already been
/// dropped, is what made this trace echo merged-file contents verbatim.
///
/// Residual: upstream also prints `listp->debug_type` (`exclude.c:274`), the
/// label naming *which* filter list the rule joined - `` for the main list,
/// ` [daemon]`, ` [per-dir .rsync-filter]`. oc has no filter-list identity to
/// pass here, so that field is absent rather than wrong.
pub fn trace_add_rule(action: FilterAction, pattern: &str, source: &RuleSource<'_>) {
    let Some((level, suffix)) = mention_rule_suffix(pattern) else {
        return;
    };

    logging::emit_debug(
        logging::DebugFlag::Filter,
        level,
        format!(
            "add_rule({}{}){}",
            source.rule_detail(action_prefix(action)),
            source.rule_text(pattern),
            source.rule_detail(suffix)
        ),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Installs a `--debug=FILTER<level>` verbosity on this thread and clears
    /// any events an earlier test left buffered, so each pin below observes
    /// only what its own call emitted.
    fn with_filter_debug(level: u8) {
        logging::init(logging::VerbosityConfig::default());
        for step in 1..=level {
            logging::apply_debug_flag(&format!("filter{step}")).expect("filter debug flag");
        }
        drop(logging::drain_events());
    }

    fn drained_messages() -> Vec<String> {
        logging::drain_events()
            .into_iter()
            .map(|event| match event {
                logging::DiagnosticEvent::Info { message, .. }
                | logging::DiagnosticEvent::Debug { message, .. } => message,
            })
            .collect()
    }

    /// WHY: upstream prints a rule ending in whitespace from FILTER1, one level
    /// earlier than every other rule, because that rule almost certainly does
    /// not do what its author meant - the space is part of the pattern
    /// (exclude.c:265-269).
    #[test]
    fn an_argument_rule_ending_in_whitespace_cautions_at_filter1() {
        with_filter_debug(1);
        trace_add_rule(FilterAction::Exclude, "my-own-rule ", &RuleSource::Argument);

        assert_eq!(
            drained_messages(),
            vec!["add_rule(- my-own-rule ) -- CAUTION: trailing whitespace!".to_owned()],
            "the operator's own trailing-whitespace rule must be flagged, and shown verbatim",
        );
    }

    /// The caution is a fact ABOUT the rule's text, so it goes through
    /// `rule_detail` with the text itself: telling the peer that a hidden rule
    /// ends in a space leaks exactly what the redaction withholds
    /// (exclude.c:274). The line still prints - upstream's gate fired - but
    /// carries neither the pattern, the action prefix, nor the caution.
    #[test]
    fn a_file_sourced_rule_ending_in_whitespace_is_flagged_silently() {
        with_filter_debug(1);
        trace_add_rule(
            FilterAction::Exclude,
            "trailing-ws-probe ",
            &RuleSource::File {
                name: ".rsync-filter",
                line: 1,
            },
        );

        let messages = drained_messages();
        assert_eq!(
            messages,
            vec!["add_rule(<rule from .rsync-filter line 1>)".to_owned()],
            "a file-sourced rule must not report its own trailing whitespace",
        );
        assert!(
            !messages[0].contains("CAUTION") && !messages[0].contains("trailing-ws-probe"),
            "neither the caution nor the pattern may reach the peer: {messages:?}",
        );
    }

    /// The non-vacuity companion for the level split: without it, a gate that
    /// simply printed everything from FILTER1 would satisfy both pins above.
    #[test]
    fn a_rule_without_trailing_whitespace_is_silent_at_filter1() {
        with_filter_debug(1);
        trace_add_rule(FilterAction::Exclude, "plain-rule", &RuleSource::Argument);

        assert!(
            drained_messages().is_empty(),
            "FILTER1 prints only the whitespace-flagged rules",
        );
    }

    /// ...and the companion for the other direction: FILTER2 prints every rule,
    /// with an empty suffix rather than the caution.
    #[test]
    fn every_rule_prints_at_filter2_without_a_caution() {
        with_filter_debug(2);
        trace_add_rule(FilterAction::Exclude, "plain-rule", &RuleSource::Argument);

        assert_eq!(
            drained_messages(),
            vec!["add_rule(- plain-rule)".to_owned()],
        );
    }

    /// upstream guards the whitespace test with `pat_len &&` (exclude.c:265),
    /// so an empty pattern cannot take the caution arm and reach FILTER1.
    #[test]
    fn an_empty_pattern_never_takes_the_caution_arm() {
        with_filter_debug(1);
        trace_add_rule(FilterAction::Clear, "", &RuleSource::Argument);

        assert!(
            drained_messages().is_empty(),
            "an empty pattern has no last byte to be whitespace",
        );
    }

    #[test]
    fn argument_text_is_returned_verbatim() {
        // upstream keeps argument text "because it is the user's own and hiding
        // it only makes typos harder to fix" (exclude.c:90-95). This is the
        // non-vacuity companion to every redaction case below: without it a
        // blanket-redaction bug passes the whole suite.
        let src = RuleSource::Argument;
        assert_eq!(src.rule_text("CLEAR"), "CLEAR");
        assert_eq!(src.rule_detail(" 'k' at position 1"), " 'k' at position 1");
        assert!(!src.is_from_file());
        assert_eq!(src.describe(), None);
    }

    #[test]
    fn line_counted_file_reports_name_and_line() {
        let src = RuleSource::File {
            name: "f.rules",
            line: 2,
        };
        assert_eq!(src.rule_text("CLEAR"), "<rule from f.rules line 2>");
    }

    #[test]
    fn word_split_file_reports_name_without_a_line() {
        // upstream: rule_src_line = word_split ? -1 : 0 (exclude.c:1754). A
        // word-split merge file yields tokens, so counting them as lines would
        // report a number that means nothing.
        let src = RuleSource::FileWordSplit { name: "w.rules" };
        assert_eq!(src.rule_text("CLEAR"), "<rule from w.rules>");
    }

    #[test]
    fn file_named_by_a_file_withholds_its_own_name() {
        // upstream: rule_src_file = named_by_file ? NULL : src_name
        // (exclude.c:1753). The name is withheld entirely, not shortened.
        let src = RuleSource::FileNamedAt {
            location: "outer.rules line 1",
        };
        assert_eq!(
            src.rule_text("CLEAR"),
            "<rule from a file named at outer.rules line 1>"
        );
    }

    #[test]
    fn deferred_merge_reports_the_generic_description() {
        let src = RuleSource::FileReadEarlier;
        assert_eq!(src.rule_text("CLEAR"), "<rule from a file read earlier>");
    }

    #[test]
    fn detail_is_dropped_for_every_file_arm() {
        // rule_detail is keyed on the same predicate as rule_text, so the
        // character-and-offset detail vanishes wherever the text does.
        for src in [
            RuleSource::File { name: "f", line: 1 },
            RuleSource::FileWordSplit { name: "f" },
            RuleSource::FileNamedAt { location: "f" },
            RuleSource::FileReadEarlier,
        ] {
            assert_eq!(src.rule_detail(" 'k' at position 1"), "");
            assert!(src.is_from_file());
        }
    }

    #[test]
    fn two_texts_in_one_message_do_not_alias() {
        // upstream needs two rotated static buffers for this (exclude.c:104-109,
        // "so two calls in one rprintf() are safe"); five of its messages
        // interpolate two of these. Pinned rather than assumed, because a future
        // refactor to a shared scratch buffer would silently reintroduce the
        // aliasing this property rules out.
        let inner = RuleSource::FileReadEarlier;
        let outer = RuleSource::File {
            name: "outer.rules",
            line: 1,
        };
        let message = format!(
            "add_rule({}) [per-dir {}]",
            inner.rule_text("- nothing"),
            outer.rule_text(": .sub-rules")
        );
        assert_eq!(
            message,
            "add_rule(<rule from a file read earlier>) [per-dir <rule from outer.rules line 1>]"
        );
    }

    #[test]
    fn one_redacted_and_one_verbatim_in_the_same_message() {
        // The mixed case, measured from upstream at --debug=FILTER3: the rules
        // inside an argument-named per-dir file are redacted while the file's
        // own name, which came from the argument, stays verbatim.
        let from_file = RuleSource::File {
            name: "/tmp/src/.sub-rules",
            line: 1,
        };
        let from_arg = RuleSource::Argument;
        let message = format!(
            "add_rule({}) [per-dir {}]",
            from_file.rule_text("- nothing"),
            from_arg.rule_text(".sub-rules")
        );
        assert_eq!(
            message,
            "add_rule(<rule from /tmp/src/.sub-rules line 1>) [per-dir .sub-rules]"
        );
    }
}
