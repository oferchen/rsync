//! The filter-rule modifier loop, as one table shared by every rule parser.
//!
//! upstream: exclude.c:1365-1443 parse_rule_tok() reads the modifiers after a
//! rule prefix in a single `switch`, and each arm's validity depends on only
//! two things: what the prefix already decided (a merge file, or a side) and
//! which flags earlier modifiers set. Keeping that decision in one place is
//! what lets the command-line, merge-file and per-directory parsers agree on
//! which rules exist.

/// What the rule prefix decided before the modifier loop runs.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ModifierScope {
    /// The prefix is `.`/`:`/`merge`/`dir-merge` (`FILTRULE_MERGE_FILE`).
    pub merge_file: bool,
    /// The prefix is `H`/`S`/`P`/`R` or their long names, which bind a side.
    pub prefix_specifies_side: bool,
}

impl ModifierScope {
    /// A plain include/exclude (`+`/`-`), which binds no side.
    pub const PLAIN: Self = Self {
        merge_file: false,
        prefix_specifies_side: false,
    };

    /// A side-binding rule (`H`, `S`, `P`, `R`).
    pub const SIDED: Self = Self {
        merge_file: false,
        prefix_specifies_side: true,
    };

    /// A merge or per-directory merge rule (`.`, `:`).
    pub const MERGE: Self = Self {
        merge_file: true,
        prefix_specifies_side: false,
    };
}

/// The flags a modifier run set, one field per upstream `FILTRULE_*` bit.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RuleModifiers {
    /// `/`: `FILTRULE_ABS_PATH`.
    pub abs_path: bool,
    /// `!`: `FILTRULE_NEGATE`.
    pub negate: bool,
    /// `C`: `FILTRULE_CVS_IGNORE`.
    pub cvs_ignore: bool,
    /// `-`, `+` or `C`: `FILTRULE_NO_PREFIXES`.
    pub no_prefixes: bool,
    /// `+`: the `FILTRULE_INCLUDE` a merge-file `+` adds to `NO_PREFIXES`.
    pub no_prefixes_include: bool,
    /// `e`: `FILTRULE_EXCLUDE_SELF`.
    pub exclude_self: bool,
    /// `n` or `C`: `FILTRULE_NO_INHERIT`.
    pub no_inherit: bool,
    /// `p`: `FILTRULE_PERISHABLE`.
    pub perishable: bool,
    /// `r`: `FILTRULE_RECEIVER_SIDE`.
    pub receiver_side: bool,
    /// `s`: `FILTRULE_SENDER_SIDE`.
    pub sender_side: bool,
    /// `w` or `C`: `FILTRULE_WORD_SPLIT`.
    pub word_split: bool,
    /// `x`: `FILTRULE_XATTR`.
    pub xattr: bool,
}

/// A modifier byte upstream's loop sends to its `invalid:` label.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidModifier {
    /// The rejected character.
    pub ch: char,
    /// Byte offset of `ch` within the run handed to [`parse_rule_modifiers`].
    pub offset: usize,
}

/// Parses a modifier run - the bytes between the rule prefix (and its optional
/// comma) and the separator that ends it - under `scope`.
///
/// The first byte the table refuses is reported, as upstream reports the first
/// byte that reaches `invalid:`; callers render it with the byte's position in
/// the whole rule.
///
/// # Errors
///
/// Returns [`InvalidModifier`] for an unknown byte or a known one that is not
/// legal here: `-`/`+` outside a merge rule or after a `-`/`+`/`C`
/// (exclude.c:1381-1391), `!` on a merge rule (:1395-1401), `C` after
/// `-`/`+`/`C` or on a side-binding rule (:1402-1404), `e`/`n`/`w` outside a
/// merge rule (:1410-1419, 1433-1437), and `r`/`s` on a side-binding rule
/// (:1423-1432).
pub fn parse_rule_modifiers(
    run: &str,
    scope: ModifierScope,
) -> Result<RuleModifiers, InvalidModifier> {
    let mut mods = RuleModifiers::default();
    for (offset, ch) in run.char_indices() {
        if !apply_modifier(&mut mods, ch, scope) {
            return Err(InvalidModifier { ch, offset });
        }
    }
    Ok(mods)
}

/// [`parse_rule_modifiers`] for a run that is a slice of `rule`, reporting a
/// refused byte's offset within the whole rule - the position upstream prints
/// (exclude.c:1374-1375, `s - *rulestr_ptr`). A run that is not a slice of
/// `rule` is measured from its own start.
///
/// # Errors
///
/// As [`parse_rule_modifiers`].
pub fn parse_rule_modifiers_in(
    rule: &str,
    run: &str,
    scope: ModifierScope,
) -> Result<RuleModifiers, InvalidModifier> {
    let base = offset_within(rule, run).unwrap_or(0);
    parse_rule_modifiers(run, scope).map_err(|invalid| InvalidModifier {
        offset: base + invalid.offset,
        ..invalid
    })
}

/// Byte offset of `inner` within `outer`, when `inner` is a slice of it.
fn offset_within(outer: &str, inner: &str) -> Option<usize> {
    let offset = (inner.as_ptr() as usize).checked_sub(outer.as_ptr() as usize)?;
    (offset + inner.len() <= outer.len()).then_some(offset)
}

/// One arm of upstream's modifier `switch`; `false` is its `goto invalid`.
fn apply_modifier(mods: &mut RuleModifiers, ch: char, scope: ModifierScope) -> bool {
    match ch {
        '-' | '+' => {
            if !scope.merge_file || mods.no_prefixes {
                return false;
            }
            mods.no_prefixes = true;
            mods.no_prefixes_include = ch == '+';
        }
        '/' => mods.abs_path = true,
        '!' => {
            // "Negation really goes with the pattern, so it isn't useful as a
            // merge-file default." (exclude.c:1396-1397)
            if scope.merge_file {
                return false;
            }
            mods.negate = true;
        }
        'C' => {
            if mods.no_prefixes || scope.prefix_specifies_side {
                return false;
            }
            mods.no_prefixes = true;
            mods.word_split = true;
            mods.no_inherit = true;
            mods.cvs_ignore = true;
        }
        'e' | 'n' | 'w' if !scope.merge_file => return false,
        'e' => mods.exclude_self = true,
        'n' => mods.no_inherit = true,
        'w' => mods.word_split = true,
        'p' => mods.perishable = true,
        'r' | 's' if scope.prefix_specifies_side => return false,
        'r' => mods.receiver_side = true,
        's' => mods.sender_side = true,
        'x' => mods.xattr = true,
        _ => return false,
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rejects(run: &str, scope: ModifierScope) -> InvalidModifier {
        parse_rule_modifiers(run, scope).expect_err(run)
    }

    #[test]
    fn merge_rule_refuses_a_second_prefix_modifier_or_c_after_one() {
        // upstream: exclude.c:1381-1391 and :1402-1404 - once NO_PREFIXES is set,
        // neither `-`/`+` nor `C` may set it again. Measured against 3.5.1:
        // `:-C`, `:C-`, `:--`, `:++` and `:+-` all fail at position 2.
        for run in ["-C", "C-", "--", "++", "+-", "-+", "CC"] {
            assert_eq!(
                rejects(run, ModifierScope::MERGE),
                InvalidModifier {
                    ch: run.chars().nth(1).unwrap(),
                    offset: 1
                },
                "{run}"
            );
        }
    }

    #[test]
    fn merge_rule_accepts_one_prefix_modifier_or_c() {
        let plus = parse_rule_modifiers("+", ModifierScope::MERGE).unwrap();
        assert!(plus.no_prefixes && plus.no_prefixes_include);
        let cvs = parse_rule_modifiers("C", ModifierScope::MERGE).unwrap();
        assert!(cvs.cvs_ignore && cvs.no_prefixes && cvs.word_split && cvs.no_inherit);
    }

    #[test]
    fn plain_rule_accepts_c_with_side_and_perishable_modifiers() {
        // upstream: exclude.c:1402-1409 guards `C` only by NO_PREFIXES and a
        // side-binding prefix, so `-C`, `-sC`, `-pC`, `-Cs` and `-!C` parse.
        for run in ["C", "sC", "pC", "Cs", "!C", "xC"] {
            assert!(
                parse_rule_modifiers(run, ModifierScope::PLAIN)
                    .unwrap()
                    .cvs_ignore,
                "{run}"
            );
        }
    }

    #[test]
    fn side_binding_rule_refuses_c_s_and_r() {
        for run in ["C", "s", "r"] {
            assert_eq!(rejects(run, ModifierScope::SIDED).offset, 0, "{run}");
        }
        assert!(parse_rule_modifiers("px/", ModifierScope::SIDED).is_ok());
    }

    #[test]
    fn merge_only_modifiers_are_refused_on_plain_rules() {
        for run in ["e", "n", "w", "-", "+"] {
            assert_eq!(rejects(run, ModifierScope::PLAIN).offset, 0, "{run}");
        }
        assert_eq!(rejects("!", ModifierScope::MERGE).offset, 0);
    }

    #[test]
    fn a_run_inside_a_rule_is_measured_from_the_rule_start() {
        let rule = "-,sS pat";
        let run = &rule[2..4];
        assert_eq!(
            parse_rule_modifiers_in(rule, run, ModifierScope::PLAIN),
            Err(InvalidModifier { ch: 'S', offset: 3 })
        );
    }

    #[test]
    fn unknown_and_uppercase_bytes_are_refused_at_their_offset() {
        assert_eq!(
            rejects("pS", ModifierScope::PLAIN),
            InvalidModifier { ch: 'S', offset: 1 }
        );
        assert_eq!(rejects(",", ModifierScope::MERGE).ch, ',');
    }
}
