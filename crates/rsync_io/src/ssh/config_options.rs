//! The single ssh_config option table, shared by both config readers.
//!
//! oc has two consumers of `~/.ssh/config`, split by what they need from
//! it rather than by how they read it:
//!
//! * [`crate::ssh::config_lookup`] resolves one boolean - whether
//!   `Compression yes` is in effect - to warn about double compression.
//!   It honours `Host` and `Match` blocks and never fails.
//! * `crate::ssh::embedded::ssh_config` resolves the connection
//!   parameters the embedded transport acts on (`HostName`, `User`,
//!   `Port`, `IdentityFile`, `IdentitiesOnly`, `IdentityAgent`). It
//!   honours `Host` blocks and refuses a file real `ssh` would refuse.
//!
//! Before this module each carried its own keyword set, its own
//! `Key Value` splitter, its own yes/no parse and its own glob matcher, so
//! every keyword added to one had to be added to the other by hand. This
//! module is the one place a keyword is declared, mirroring upstream's
//! shape: a `name -> opcode` table read by one lookup, with the per-option
//! value handling selected by the opcode.
//!
//! upstream: openssh/readconf.c:175-325 `keywords[]`, openssh/readconf.c:132
//! `OpCodes`, openssh/readconf.c:958 `parse_token()`.
//!
//! # Scope
//!
//! The table holds the union of what oc's readers act on today, not
//! upstream's full ~180 keywords. A keyword absent from it resolves to
//! [`Opcode::Unknown`], which every reader ignores - the same answer both
//! readers gave before the table existed. Widening the table is one row per
//! keyword; oc's unknown-keyword *policy* (upstream counts bad options and
//! then aborts the load, openssh/readconf.c:2667-2669) is a separate row.
//!
//! # What this module does NOT own
//!
//! Which opcodes a reader acts on, and what it does with the value, stay
//! with that reader: the two have deliberately different failure policies
//! and deliberately different scopes. The table answers "what keyword is
//! this and how is its value shaped", never "what should happen next".

/// One ssh_config keyword, resolved from its spelling.
///
/// Mirrors upstream's `OpCodes` (openssh/readconf.c:132-174), narrowed to
/// the keywords oc resolves plus the catch-all. Upstream splits its
/// catch-all three ways (`oBadOption`, `oIgnore`,
/// `oIgnoredUnknownOption`) because it acts on the distinction; oc ignores
/// every unrecognised keyword identically, so one variant is the honest
/// model.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(in crate::ssh) enum Opcode {
    /// `Host <patterns>` - opens a block gated on the target alias.
    /// upstream: openssh/readconf.c:246 `oHost`, arm at :1823.
    Host,
    /// `Match <criteria>` - opens a conditionally active block.
    /// upstream: openssh/readconf.c:247 `oMatch`, arm at :1864.
    Match,
    /// `Compression yes|no`.
    /// upstream: openssh/readconf.c:256 `oCompression`, arm at :1344.
    Compression,
    /// `HostName <name>`.
    /// upstream: openssh/readconf.c:236 `oHostname`, arm at :1493.
    Hostname,
    /// `User <name>`.
    /// upstream: openssh/readconf.c:245 `oUser`, arm at :1439.
    User,
    /// `Port <number>`.
    /// upstream: openssh/readconf.c:239 `oPort`, arm at :1559.
    Port,
    /// `IdentityFile <path>`.
    /// upstream: openssh/readconf.c:230 `oIdentityFile`, arm at :1394.
    IdentityFile,
    /// `IdentitiesOnly yes|no`.
    /// upstream: openssh/readconf.c:232 `oIdentitiesOnly`, arm at :1914.
    IdentitiesOnly,
    /// `IdentityAgent <path>`.
    /// upstream: openssh/readconf.c:235 `oIdentityAgent`, arm at :2419.
    IdentityAgent,
    /// A keyword no reader resolves. Ignored by both.
    Unknown,
}

/// The option table: the ONE place an ssh_config keyword is declared.
///
/// Spellings are lower case and matched case-insensitively, because
/// upstream lowercases the keyword before its `strcmp`
/// (openssh/readconf.c:1184 `lowercase(keyword)` into
/// openssh/readconf.c:964-966).
const KEYWORDS: &[(&str, Opcode)] = &[
    ("compression", Opcode::Compression),
    ("host", Opcode::Host),
    ("hostname", Opcode::Hostname),
    ("identitiesonly", Opcode::IdentitiesOnly),
    ("identityagent", Opcode::IdentityAgent),
    ("identityfile", Opcode::IdentityFile),
    ("match", Opcode::Match),
    ("port", Opcode::Port),
    ("user", Opcode::User),
];

/// Resolves a keyword spelling to its [`Opcode`], case-insensitively.
///
/// upstream: openssh/readconf.c:958-973 `parse_token()`, which scans the
/// same flat table and returns `oBadOption` for anything it does not find.
pub(in crate::ssh) fn parse_token(keyword: &str) -> Opcode {
    KEYWORDS
        .iter()
        .find(|(name, _)| keyword.eq_ignore_ascii_case(name))
        .map_or(Opcode::Unknown, |(_, opcode)| *opcode)
}

/// How a keyword's value half is shaped, which is what decides the parse
/// and the wording of a missing-value refusal.
///
/// Upstream expresses this as the arm each opcode `goto`s into; oc names it
/// so the two readers cannot disagree about which shape a keyword has.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(in crate::ssh) enum ValueKind {
    /// A pattern list walked by the `oHost` arm itself
    /// (openssh/readconf.c:1831-1858). There is no missing-value refusal:
    /// the walk simply runs zero times and leaves the block inactive.
    HostPatterns,
    /// A `Match` criteria list, walked by upstream's `match_cfg_line`
    /// (openssh/readconf.c:1864-1870).
    MatchCriteria,
    /// One token, read as a multistate yes/no flag.
    /// upstream: `parse_multistate_value` (openssh/readconf.c:1103-1117).
    Flag,
    /// One token, taken verbatim - a string, a path, or a port number.
    /// upstream: the `parse_string` family (openssh/readconf.c:1441-1445).
    Single,
    /// No shape, because no reader resolves the keyword.
    Unknown,
}

impl Opcode {
    /// The value shape this keyword's arm expects.
    pub(in crate::ssh) fn value_kind(self) -> ValueKind {
        match self {
            Self::Host => ValueKind::HostPatterns,
            Self::Match => ValueKind::MatchCriteria,
            Self::Compression | Self::IdentitiesOnly => ValueKind::Flag,
            Self::Hostname | Self::User | Self::Port | Self::IdentityFile | Self::IdentityAgent => {
                ValueKind::Single
            }
            Self::Unknown => ValueKind::Unknown,
        }
    }

    /// The diagnostic upstream prints when this keyword's `argv_next`
    /// yields nothing, or `None` for the shapes that have no such refusal.
    ///
    /// The capitalisation is upstream's own and splits by shape, not by
    /// keyword: the multistate flags route through
    /// `parse_multistate_value` and print `missing argument.`
    /// (openssh/readconf.c:1108) while the single-token arms print
    /// `Missing argument.` (openssh/readconf.c:1364, :1397, :1444, :1562,
    /// :2423). Deriving it from [`ValueKind`] is what keeps a new keyword
    /// from picking the wrong one.
    #[cfg_attr(not(feature = "embedded-ssh"), allow(dead_code))]
    pub(in crate::ssh) fn missing_argument(self) -> Option<&'static str> {
        match self.value_kind() {
            ValueKind::Flag => Some("missing argument."),
            ValueKind::Single => Some("Missing argument."),
            ValueKind::HostPatterns | ValueKind::MatchCriteria | ValueKind::Unknown => None,
        }
    }
}

/// Splits a `Key Value` (or `Key=Value`) line on the first run of
/// whitespace or `=`. Returns `None` when the line carries only a keyword.
///
/// The keyword half is upstream's `strdelim` (openssh/readconf.c:1176,
/// openssh/misc.c:469-507), a *different* splitter from `argv_split`; it is
/// the returned value half that a reader then tokenises with
/// [`crate::ssh::argv_split`].
pub(in crate::ssh) fn split_directive(line: &str) -> Option<(&str, &str)> {
    let (key, rest) = line.split_once(|c: char| c.is_whitespace() || c == '=')?;
    let value = rest.trim_start_matches(|c: char| c.is_whitespace() || c == '=');
    if value.is_empty() {
        return None;
    }
    Some((key, value))
}

/// Parses a [`ValueKind::Flag`] value. Returns `None` for anything outside
/// the multistate set so a typo cannot silently flip a setting.
///
/// upstream: `multistate_flag` (openssh/readconf.c:994-1000) accepts
/// `true`, `false`, `yes` and `no`, compared with `strcasecmp`
/// (openssh/readconf.c:1113).
pub(in crate::ssh) fn parse_flag_value(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "yes" | "true" => Some(true),
        "no" | "false" => Some(false),
        _ => None,
    }
}

/// Byte-level glob matcher for a single `Host` or `Match` pattern token:
/// `*` matches any run, `?` matches exactly one byte. No character
/// classes, no extended globs.
///
/// upstream: `match_pattern` (openssh/match.c:57-114). Case folding is the
/// caller's, because it differs per keyword - the `oHost` arm calls
/// `match_pattern` directly and folds nothing (openssh/readconf.c:1844),
/// while `Match host` routes through `match_hostname`, which lowercases
/// first (openssh/match.c:194-203).
pub(in crate::ssh) fn glob_matches(input: &[u8], glob: &[u8]) -> bool {
    if glob.is_empty() {
        return input.is_empty();
    }
    match glob[0] {
        b'*' => {
            if glob.len() == 1 {
                return true;
            }
            (0..=input.len()).any(|i| glob_matches(&input[i..], &glob[1..]))
        }
        b'?' => !input.is_empty() && glob_matches(&input[1..], &glob[1..]),
        c => !input.is_empty() && input[0] == c && glob_matches(&input[1..], &glob[1..]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The union both readers dispatch on, pinned by name.
    ///
    /// This is the anti-rename cell: the row exists so that dropping a
    /// keyword from [`KEYWORDS`] fails HERE as well as in whichever
    /// reader happens to consume it, and so that a keyword can never be
    /// declared in one reader's private set again.
    const UNION: &[(&str, Opcode)] = &[
        ("Host", Opcode::Host),
        ("Match", Opcode::Match),
        ("Compression", Opcode::Compression),
        ("HostName", Opcode::Hostname),
        ("User", Opcode::User),
        ("Port", Opcode::Port),
        ("IdentityFile", Opcode::IdentityFile),
        ("IdentitiesOnly", Opcode::IdentitiesOnly),
        ("IdentityAgent", Opcode::IdentityAgent),
    ];

    #[test]
    fn every_keyword_both_readers_use_resolves_from_the_one_table() {
        for (spelling, expected) in UNION {
            assert_eq!(parse_token(spelling), *expected, "keyword {spelling}");
        }
        assert_eq!(KEYWORDS.len(), UNION.len(), "table and union disagree");
    }

    /// upstream lowercases the keyword before the table scan
    /// (openssh/readconf.c:1184), so every spelling resolves.
    #[test]
    fn keyword_lookup_is_case_insensitive() {
        for spelling in ["identityfile", "IDENTITYFILE", "IdEnTiTyFiLe"] {
            assert_eq!(parse_token(spelling), Opcode::IdentityFile);
        }
    }

    /// Non-vacuity for the two cells above: a keyword that is NOT in the
    /// table resolves to `Unknown`, so the lookup is a real table scan and
    /// not a function that answers every question affirmatively.
    #[test]
    fn a_keyword_outside_the_table_is_unknown() {
        for spelling in ["proxyjump", "ciphers", "", "hostnamex", "hos"] {
            assert_eq!(parse_token(spelling), Opcode::Unknown, "keyword {spelling}");
        }
    }

    #[test]
    fn no_keyword_is_declared_twice() {
        for (i, (name, _)) in KEYWORDS.iter().enumerate() {
            assert!(
                !KEYWORDS[i + 1..].iter().any(|(other, _)| other == name),
                "keyword {name} declared twice"
            );
        }
    }

    /// The table's spellings must be the lower-case form upstream compares
    /// against, or `eq_ignore_ascii_case` would be hiding a typo.
    #[test]
    fn table_spellings_are_lowercase() {
        for (name, _) in KEYWORDS {
            assert_eq!(*name, name.to_ascii_lowercase(), "keyword {name}");
        }
    }

    /// The two capitalisations are upstream's, and they split by VALUE
    /// SHAPE rather than by keyword - the property that makes deriving the
    /// wording from [`ValueKind`] correct for a keyword not yet added.
    #[test]
    fn the_missing_argument_wording_splits_by_value_shape() {
        assert_eq!(
            Opcode::IdentitiesOnly.missing_argument(),
            Some("missing argument.")
        );
        assert_eq!(
            Opcode::Compression.missing_argument(),
            Some("missing argument.")
        );
        for opcode in [
            Opcode::Hostname,
            Opcode::User,
            Opcode::Port,
            Opcode::IdentityFile,
            Opcode::IdentityAgent,
        ] {
            assert_eq!(
                opcode.missing_argument(),
                Some("Missing argument."),
                "{opcode:?}"
            );
        }
        // The block-opening keywords consume a LIST; an absent value
        // leaves the walk empty rather than refusing
        // (openssh/readconf.c:1829-1831).
        for opcode in [Opcode::Host, Opcode::Match, Opcode::Unknown] {
            assert_eq!(opcode.missing_argument(), None, "{opcode:?}");
        }
    }

    #[test]
    fn split_directive_accepts_both_separators() {
        assert_eq!(split_directive("Port 2222"), Some(("Port", "2222")));
        assert_eq!(split_directive("Port=2222"), Some(("Port", "2222")));
        assert_eq!(split_directive("Port = 2222"), Some(("Port", "2222")));
        assert_eq!(split_directive("Port"), None);
        assert_eq!(split_directive("Port "), None);
    }

    #[test]
    fn flag_values_cover_upstreams_multistate_set() {
        for yes in ["yes", "YES", "true", "True"] {
            assert_eq!(parse_flag_value(yes), Some(true), "{yes}");
        }
        for no in ["no", "NO", "false", "False"] {
            assert_eq!(parse_flag_value(no), Some(false), "{no}");
        }
        for other in ["", "1", "on", "ask"] {
            assert_eq!(parse_flag_value(other), None, "{other}");
        }
    }

    #[test]
    fn glob_star_and_question_mark() {
        assert!(glob_matches(b"anything", b"*"));
        assert!(glob_matches(b"", b"*"));
        assert!(glob_matches(b"a.example.com", b"*.example.com"));
        assert!(!glob_matches(b"a.example.org", b"*.example.com"));
        assert!(glob_matches(b"a", b"?"));
        assert!(!glob_matches(b"ab", b"?"));
        assert!(glob_matches(b"ab", b"??"));
        assert!(glob_matches(b"exact", b"exact"));
        assert!(!glob_matches(b"exact", b"exac"));
        // Case folding is the CALLER's: the matcher itself never folds
        // (openssh/match.c:105-106).
        assert!(!glob_matches(b"web1", b"WEB1"));
    }
}
