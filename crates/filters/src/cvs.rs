//! CVS exclusion patterns for rsync's `--cvs-exclude` (`-C`) option.
//!
//! This module provides the default exclusion patterns that rsync uses when
//! the `--cvs-exclude` option is specified. These patterns automatically
//! exclude files commonly found in version control systems and build artifacts.
//!
//! # Default Patterns
//!
//! The default CVS exclusion list includes:
//!
//! - Version control directories: `RCS`, `SCCS`, `CVS`, `.svn/`, `.git/`, `.hg/`, `.bzr/`
//! - Version control files: `CVS.adm`, `RCSLOG`, `cvslog.*`, `tags`, `TAGS`
//! - Build state files: `.make.state`, `.nse_depinfo`
//! - Editor backup files: `*~`, `#*`, `.#*`, `*.old`, `*.bak`, `*.BAK`, `*.orig`, `*.rej`
//! - Temporary files: `,*`, `_$*`, `*$`, `.del-*`
//! - Object files and libraries: `*.a`, `*.olb`, `*.o`, `*.obj`, `*.so`, `*.exe`
//! - Miscellaneous: `*.Z`, `*.elc`, `*.ln`, `core`
//!
//! # Perishable Rules
//!
//! In rsync protocol version 30 and later, these patterns are marked as
//! "perishable", meaning they can be overridden by explicit include rules
//! from `.cvsignore` files or the `CVSIGNORE` environment variable.
//!
//! # References
//!
//! - [rsync man page](https://download.samba.org/pub/rsync/rsync.1)
//! - upstream: exclude.c:get_cvs_excludes() - builds the default CVS list
//! - upstream: options.c - `--cvs-exclude` (`-C`) option handling

use std::env;
use std::fs;
use std::path::Path;
use std::sync::{Arc, OnceLock};

use crate::{FilterRule, RuleModifiers};

/// Default CVS exclusion patterns.
///
/// This is the single canonical definition of rsync's built-in `--cvs-exclude`
/// defaults from `exclude.c:get_cvs_excludes()`. The patterns use rsync's
/// filter syntax:
/// - Simple names match files/directories at any level
/// - `*` matches any characters except `/`
/// - Trailing `/` marks directory-only patterns
pub const DEFAULT_CVSIGNORE: &[&str] = &[
    "RCS",
    "SCCS",
    "CVS",
    "CVS.adm",
    "RCSLOG",
    "cvslog.*",
    "tags",
    "TAGS",
    ".make.state",
    ".nse_depinfo",
    "*~",
    "#*",
    ".#*",
    ",*",
    "_$*",
    "*$",
    "*.old",
    "*.bak",
    "*.BAK",
    "*.orig",
    "*.rej",
    ".del-*",
    "*.a",
    "*.olb",
    "*.o",
    "*.obj",
    "*.so",
    "*.exe",
    "*.Z",
    "*.elc",
    "*.ln",
    "core",
    ".svn/",
    ".git/",
    ".hg/",
    ".bzr/",
];

/// Returns an iterator over the default CVS exclusion patterns.
///
/// Each pattern is whitespace-trimmed and ready for use with filter rules.
///
/// # Examples
///
/// ```
/// use filters::cvs::default_patterns;
///
/// let patterns: Vec<&str> = default_patterns().collect();
/// assert!(patterns.contains(&"*.o"));
/// assert!(patterns.contains(&".git/"));
/// ```
pub fn default_patterns() -> impl Iterator<Item = &'static str> {
    DEFAULT_CVSIGNORE.iter().copied()
}

/// Returns the number of default CVS exclusion patterns in
/// [`DEFAULT_CVSIGNORE`].
#[must_use]
pub fn pattern_count() -> usize {
    default_patterns().count()
}

/// One pattern of the global CVS ignore list.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CvsIgnorePattern {
    /// The pattern text.
    pub pattern: String,
    /// Whether it is a built-in default, which upstream alone marks perishable
    /// at protocol 30 and later (exclude.c:1498-1500).
    pub builtin: bool,
}

/// Builds upstream's global CVS ignore list: the built-in defaults, then the
/// words of `$HOME/.cvsignore`, then the words of `$CVSIGNORE`.
///
/// upstream: exclude.c:1489-1507 get_cvs_excludes(). Both extra sources are
/// parsed word-split with no prefixes, so a bare `!` clears everything
/// collected so far, defaults included (exclude.c:1273-1275, 1472-1473,
/// 1541-1551), while `!foo` is a literal pattern. An unreadable
/// `$HOME/.cvsignore` adds nothing, as `parse_filter_file` without
/// XFLG_FATAL_ERRORS returns quietly (exclude.c:1703-1722).
#[must_use]
pub fn cvs_ignore_list() -> Vec<CvsIgnorePattern> {
    let mut list: Vec<CvsIgnorePattern> = default_patterns()
        .map(|pattern| CvsIgnorePattern {
            pattern: pattern.to_owned(),
            builtin: true,
        })
        .collect();
    if let Some(home) = env::var_os("HOME").filter(|home| !home.is_empty())
        && let Ok(contents) = fs::read(Path::new(&home).join(".cvsignore"))
    {
        push_cvsignore_words(&mut list, &String::from_utf8_lossy(&contents));
    }
    if let Some(value) = env::var_os("CVSIGNORE").filter(|value| !value.is_empty()) {
        push_cvsignore_words(&mut list, &value.to_string_lossy());
    }
    list
}

/// Appends the words of one CVS ignore source, honouring a bare `!` clear.
fn push_cvsignore_words(list: &mut Vec<CvsIgnorePattern>, text: &str) {
    for word in text.split_whitespace() {
        if word == "!" {
            list.clear();
        } else {
            list.push(CvsIgnorePattern {
                pattern: word.to_owned(),
                builtin: false,
            });
        }
    }
}

/// The flags a non-merge `C` rule hands to the CVS ignore patterns it stands
/// for.
///
/// upstream: exclude.c:1595-1597 passes the rule's flags to get_cvs_excludes(),
/// whose `rule_template()` makes every pattern inherit FILTRULES_FROM_CONTAINER,
/// include, negate and perishable among them (exclude.c:1229-1231, 1263), and
/// the rule's side (:1447-1456). So `+C` makes the list includes and `-sC`
/// makes it sender-only.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CvsTemplate {
    /// The rule's prefix was `+` (FILTRULE_INCLUDE).
    pub include: bool,
    /// `!`: FILTRULE_NEGATE.
    pub negate: bool,
    /// `p`: FILTRULE_PERISHABLE.
    pub perishable: bool,
    /// `s`: FILTRULE_SENDER_SIDE.
    pub sender_side: bool,
    /// `r`: FILTRULE_RECEIVER_SIDE.
    pub receiver_side: bool,
}

impl CvsTemplate {
    /// The template of a `C` rule whose prefix is an include when `include`,
    /// carrying the modifier flags `mods`.
    #[must_use]
    pub const fn new(include: bool, mods: &RuleModifiers) -> Self {
        Self {
            include,
            negate: mods.negate,
            perishable: mods.perishable,
            sender_side: mods.sender_side,
            receiver_side: mods.receiver_side,
        }
    }

    /// Whether `entry` is perishable under this template; `builtin_perishable`
    /// is the protocol >= 30 flag the built-in defaults carry.
    #[must_use]
    pub const fn perishable_for(&self, entry: &CvsIgnorePattern, builtin_perishable: bool) -> bool {
        self.perishable || (entry.builtin && builtin_perishable)
    }

    /// The `(sender, receiver)` sides the `C` rule named, or `None`.
    #[must_use]
    pub const fn sides(&self) -> Option<(bool, bool)> {
        if self.sender_side || self.receiver_side {
            Some((self.sender_side, self.receiver_side))
        } else {
            None
        }
    }

    /// The rule for one pattern of the list.
    #[must_use]
    pub fn rule(&self, entry: &CvsIgnorePattern, builtin_perishable: bool) -> FilterRule {
        let rule = if self.include {
            FilterRule::include(entry.pattern.as_str())
        } else {
            FilterRule::exclude(entry.pattern.as_str())
        };
        let rule = rule
            .with_perishable(self.perishable_for(entry, builtin_perishable))
            .with_negate(self.negate);
        match self.sides() {
            Some((sender, receiver)) => rule.with_sides(sender, receiver),
            None => rule,
        }
    }
}

/// The CVS ignore list one transfer session builds.
///
/// upstream: exclude.c:1489-1496 get_cvs_excludes() fills `cvs_filter_list`
/// once (`static int initialized`) under the first `C` rule's flags, and
/// check_filter() evaluates that one list for every `C` rule
/// (exclude.c:1201-1206). So after `-C` a later `-!C` still matches plain
/// excludes. The static lives as long as the process, and upstream runs one
/// session per process (a daemon forks per connection), so the list belongs to
/// the session: clones share it, and a new session starts empty.
#[derive(Clone, Debug, Default)]
pub struct CvsSession {
    first: Arc<OnceLock<CvsTemplate>>,
}

impl CvsSession {
    /// The template the session's list is built under: the first one any
    /// non-merge `C` rule asked for, whatever later rules ask.
    #[must_use]
    pub fn established(&self, template: CvsTemplate) -> CvsTemplate {
        *self.first.get_or_init(|| template)
    }

    /// The session's CVS ignore list, which a `C` rule with `template` stands
    /// for, as rules under the [established](Self::established) template.
    #[must_use]
    pub fn rules(&self, template: CvsTemplate, builtin_perishable: bool) -> Vec<FilterRule> {
        let template = self.established(template);
        cvs_ignore_list()
            .iter()
            .map(|entry| template.rule(entry, builtin_perishable))
            .collect()
    }
}

/// Two sessions compare by the template their list is built under, so
/// configurations that carry a session still compare by value.
impl PartialEq for CvsSession {
    fn eq(&self, other: &Self) -> bool {
        self.first.get() == other.first.get()
    }
}

impl Eq for CvsSession {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_patterns_contains_expected_vcs_dirs() {
        let patterns: Vec<&str> = default_patterns().collect();
        assert!(patterns.contains(&".git/"));
        assert!(patterns.contains(&".svn/"));
        assert!(patterns.contains(&".hg/"));
        assert!(patterns.contains(&".bzr/"));
        assert!(patterns.contains(&"CVS"));
        assert!(patterns.contains(&"RCS"));
        assert!(patterns.contains(&"SCCS"));
    }

    #[test]
    fn default_patterns_contains_object_files() {
        let patterns: Vec<&str> = default_patterns().collect();
        assert!(patterns.contains(&"*.o"));
        assert!(patterns.contains(&"*.obj"));
        assert!(patterns.contains(&"*.so"));
        assert!(patterns.contains(&"*.a"));
        assert!(patterns.contains(&"*.exe"));
    }

    #[test]
    fn default_patterns_contains_backup_files() {
        let patterns: Vec<&str> = default_patterns().collect();
        assert!(patterns.contains(&"*~"));
        assert!(patterns.contains(&"*.bak"));
        assert!(patterns.contains(&"*.BAK"));
        assert!(patterns.contains(&"*.old"));
        assert!(patterns.contains(&"*.orig"));
        assert!(patterns.contains(&"*.rej"));
    }

    #[test]
    fn default_patterns_contains_editor_files() {
        let patterns: Vec<&str> = default_patterns().collect();
        assert!(patterns.contains(&"#*"));
        assert!(patterns.contains(&".#*"));
    }

    #[test]
    fn default_patterns_count_matches_expected() {
        let count = pattern_count();
        assert!(count >= 30, "expected at least 30 patterns, got {count}");
        assert!(count <= 40, "expected at most 40 patterns, got {count}");
    }

    #[test]
    fn default_patterns_no_empty_entries() {
        for pattern in default_patterns() {
            assert!(!pattern.is_empty(), "found empty pattern");
            assert!(
                !pattern.contains(' '),
                "pattern contains space: {pattern:?}"
            );
        }
    }

    #[test]
    fn default_patterns_directory_markers_preserved() {
        let patterns: Vec<&str> = default_patterns().collect();
        assert!(patterns.contains(&".git/"));
        assert!(patterns.contains(&".svn/"));
        assert!(patterns.contains(&"*.o"));
        assert!(!patterns.contains(&"*.o/"));
    }

    fn words(list: &[CvsIgnorePattern]) -> Vec<&str> {
        list.iter().map(|entry| entry.pattern.as_str()).collect()
    }

    /// upstream: exclude.c:1273-1275, 1472-1473 - in a word-split, no-prefix
    /// CVS source a bare `!` keeps FILTRULE_CLEAR_LIST and pops the whole list,
    /// built-in defaults included (exclude.c:1541-1551), while `!foo` and
    /// `#foo` are literal patterns: the comment test is skipped when word
    /// splitting (exclude.c:1806).
    #[test]
    fn cvsignore_words_honour_a_bare_bang_and_keep_literals() {
        let mut list = vec![CvsIgnorePattern {
            pattern: "core".to_owned(),
            builtin: true,
        }];
        push_cvsignore_words(&mut list, "*.o ! !foo #bar");
        assert_eq!(words(&list), ["!foo", "#bar"]);
        assert!(list.iter().all(|entry| !entry.builtin));
    }

    #[test]
    fn template_flags_reach_the_rule() {
        let entry = CvsIgnorePattern {
            pattern: "*.o".to_owned(),
            builtin: true,
        };
        let template = CvsTemplate {
            include: true,
            negate: true,
            perishable: false,
            sender_side: false,
            receiver_side: true,
        };
        let rule = template.rule(&entry, true);
        assert_eq!(rule.action(), crate::FilterAction::Include);
        assert!(rule.is_negated() && rule.is_perishable());
        assert!(!rule.applies_to_sender() && rule.applies_to_receiver());
        assert!(!CvsTemplate::default().rule(&entry, false).is_perishable());
    }

    /// upstream: exclude.c:1489-1496 - the list is built once, so the first
    /// template asked for is the one every later `C` rule gets.
    #[test]
    fn the_first_template_asked_for_is_the_one_every_rule_gets() {
        let negated = CvsTemplate {
            negate: true,
            ..CvsTemplate::default()
        };
        let session = CvsSession::default();
        assert_eq!(session.established(negated), negated);
        assert_eq!(session.established(CvsTemplate::default()), negated);
        assert!(
            session
                .clone()
                .rules(CvsTemplate::default(), true)
                .iter()
                .all(FilterRule::is_negated)
        );
    }

    /// Two sessions in one process - QUIC daemon sessions share the daemon
    /// parent - each build their own list, as two forked upstream daemon
    /// children do.
    #[test]
    fn each_session_builds_its_list_under_its_own_first_template() {
        let negated = CvsTemplate {
            negate: true,
            ..CvsTemplate::default()
        };
        let first = CvsSession::default();
        let second = CvsSession::default();
        assert!(
            first
                .rules(negated, true)
                .iter()
                .all(FilterRule::is_negated)
        );
        assert!(
            second
                .rules(CvsTemplate::default(), true)
                .iter()
                .all(|rule| !rule.is_negated())
        );
    }
}
