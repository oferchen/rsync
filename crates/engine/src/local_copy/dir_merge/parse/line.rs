use super::{
    dir_merge::parse_dir_merge_directive,
    merge::{parse_merge_directive, parse_short_merge_directive_line},
    modifiers::split_short_rule_modifiers,
    types::{FilterParseError, ParsedFilterDirective},
};
use crate::local_copy::filter_program::ExcludeIfPresentRule;
use filters::{FilterRule, RuleSource};
use std::fmt;

#[derive(Default)]
struct RuleModifierState {
    anchor_root: bool,
    sender: Option<bool>,
    receiver: Option<bool>,
    perishable: bool,
    xattr_only: bool,
    negate: bool,
}

fn unsupported_modifier_error(directive: &str, modifier: impl fmt::Display) -> FilterParseError {
    FilterParseError::new(format!(
        "filter directive '{directive}' uses unsupported modifier '{modifier}'"
    ))
}

/// Parses the `,modifier` run that may follow a rule prefix.
///
/// `prefix_specifies_side` mirrors upstream's variable of the same name, set by
/// the four side-bound prefixes (exclude.c:1345-1358 for `S`/`H`/`R`/`P`, and
/// the long keywords that map onto them). When it is set, a further `s` or `r`
/// is a redundant side and upstream refuses the rule (exclude.c:1423-1432).
fn parse_rule_modifiers(
    modifiers: &str,
    directive: &str,
    prefix_specifies_side: bool,
) -> Result<RuleModifierState, FilterParseError> {
    let mut state = RuleModifierState::default();

    // upstream: exclude.c:1365 - the modifier loop switches on the raw byte.
    // `R`/`S`/`P`/`X` are not modifiers at all; they reach the default arm and
    // raise RERR_SYNTAX (exclude.c:1371-1379). Case-folding them here silently
    // applied a side or perishable restriction the rule never asked for.
    for modifier in modifiers.chars() {
        match modifier {
            '/' => state.anchor_root = true,
            's' => {
                if prefix_specifies_side {
                    return Err(unsupported_modifier_error(directive, modifier));
                }
                state.sender = Some(true);
                if state.receiver.is_none() {
                    state.receiver = Some(false);
                }
            }
            'r' => {
                if prefix_specifies_side {
                    return Err(unsupported_modifier_error(directive, modifier));
                }
                state.receiver = Some(true);
                if state.sender.is_none() {
                    state.sender = Some(false);
                }
            }
            // upstream: exclude.c:1395-1401 - `!` sets FILTRULE_NEGATE and is
            // refused only on a merge rule. Merge prefixes never reach this
            // function (they are dispatched to the merge/dir-merge parsers
            // above), so the guard has no expressible case here.
            '!' => state.negate = true,
            // upstream: exclude.c:1420 (`p`) and exclude.c:1438 (`x`) are the
            // only modifiers besides `/` that carry NO guard at all - not the
            // merge-file test that gates `- + e n w`, not the `!`-on-merge
            // test, and not the `prefix_specifies_side` test that `C`, `r` and
            // `s` consult. Both are therefore legal after every prefix,
            // including the four side-bound ones.
            'p' => state.perishable = true,
            'x' => {
                state.xattr_only = true;
            }
            _ => {
                return Err(unsupported_modifier_error(directive, modifier));
            }
        }
    }

    Ok(state)
}

fn apply_rule_modifiers(
    mut rule: FilterRule,
    modifiers: RuleModifierState,
    directive: &str,
) -> Result<FilterRule, FilterParseError> {
    if modifiers.anchor_root {
        rule = rule.anchor_to_root();
    }

    if let Some(sender) = modifiers.sender {
        rule = rule.with_sender(sender);
    }

    if let Some(receiver) = modifiers.receiver {
        rule = rule.with_receiver(receiver);
    }

    if modifiers.perishable {
        rule = rule.with_perishable(true);
    }

    if modifiers.negate {
        rule = rule.with_negate(true);
    }

    if modifiers.xattr_only {
        // upstream: exclude.c:1438 - `case 'x': rule->rflags |=
        // FILTRULE_XATTR;`. The modifier sets one bit and binds no side, so
        // forcing both sides here erased whatever `s`/`r` had already
        // selected: `-sx pat` became a both-sides rule.
        //
        // Every prefix that carries an include/exclude decision accepts it.
        // Restricting the set to oc's `Include`/`Exclude` variants excluded
        // `protect`/`risk`, which upstream stores as the same two decisions
        // with a receiver-side bit (exclude.c:1352-1358) - so `protect,x` was
        // refused where upstream builds an ordinary receiver-side xattr rule.
        // [`FilterAction::xattr_decision`] is the shared owner of that table.
        if rule.action().xattr_decision().is_none() {
            return Err(FilterParseError::new(format!(
                "filter directive '{directive}' cannot combine 'x' modifiers with this directive"
            )));
        }
        rule = rule.with_xattr_only(true);
    }

    Ok(rule)
}

/// Splits a rule token into its prefix and the modifier run that follows.
///
/// upstream: exclude.c:1325-1329 - the prefix switch's `default:` arm takes
/// `ch = *s` and advances past only an OPTIONAL comma, leaving the modifier
/// loop at :1365 to read from the very next byte. So for a single-letter
/// prefix the comma is decoration: `Px pat` and `P,x pat` parse identically,
/// and the pattern cannot start until a space or `_` (:1365 loop exit).
///
/// A long keyword is different: `rule_strcmp` (exclude.c:1218-1227) accepts it
/// only when followed by space/`_`/NUL or `,`, so `protect,x` is valid and
/// `protectx` is not. Splitting on the comma alone - the whole rule oc used to
/// apply - therefore handled the keyword form and silently rejected the
/// adjacent single-letter form as an unknown rule.
fn split_keyword_modifiers(keyword: &str) -> (&str, &str) {
    let mut chars = keyword.chars();
    if let Some(first) = chars.next() {
        if SINGLE_LETTER_PREFIXES.contains(first) {
            let rest = chars.as_str();
            return (
                &keyword[..first.len_utf8()],
                rest.strip_prefix(',').unwrap_or(rest),
            );
        }
    }
    if let Some((name, modifiers)) = keyword.split_once(',') {
        (name, modifiers)
    } else {
        (keyword, "")
    }
}

/// The list-clearing keyword, lower case only.
///
/// upstream: exclude.c:1290 `RULE_STRCMP(s, "clear")`, reached only from the
/// `case 'c':` arm of the switch at :1288. `CLEAR` takes the `default:` arm and
/// is reported as an unknown rule.
const CLEAR_KEYWORD: &str = "clear";

/// The oc-only `exclude-if-present` keyword.
///
/// Upstream has no such directive, so there is no upstream case rule to mirror.
/// The spelling stays case-insensitive to match the CLI option of the same name
/// (`crates/cli/src/frontend/filters/parsing/rules.rs`), which accepts any case.
const EXCLUDE_IF_PRESENT_PREFIX: &str = "exclude-if-present";

/// Reports whether `token` names a directive whose pattern arrives as the next
/// whitespace-delimited word in a word-split filter file.
///
/// ⚠ This table has NO upstream counterpart, and it is not a reconstruction of
/// one. Upstream's word-split *file reader* ends every token at the first
/// whitespace before any keyword parsing runs (exclude.c:1772,
/// `if (word_split && isspace(ch)) break;`), so `exclude foo` in a `merge,w`
/// file is two tokens and the bare `exclude` is a fatal
/// `unexpected end of filter rule`. MEASURED against rsync 3.5.0: that input
/// exits 1, while `exclude_foo` excludes `foo` and exits 0.
///
/// oc instead glues the following token on, accepting a form upstream rejects.
/// Removing the glue is upstream parity but changes accepted input, so it is
/// deliberately left for its own change rather than folded in here; this
/// function only preserves the pre-existing set verbatim so the keyword-case
/// fix stays behaviour-neutral apart from case. In particular `risk` and
/// `per-dir` are absent because they were absent before - adding them would
/// widen the divergence, not narrow it.
///
/// The case policy is shared with [`parse_filter_directive_line`]: the upstream
/// keywords match exactly (`rule_strcmp` is `strncmp`, exclude.c:1218), while
/// the oc-only `exclude-if-present` keeps the case-insensitive spelling its own
/// parser accepts.
pub(crate) fn directive_takes_argument(token: &str) -> bool {
    matches!(
        token,
        "merge" | "include" | "exclude" | "show" | "hide" | "protect"
    ) || token.starts_with("dir-merge")
        || token.eq_ignore_ascii_case(EXCLUDE_IF_PRESENT_PREFIX)
}

/// The single-character rule prefixes this parser dispatches, upper case only.
///
/// upstream: exclude.c:1288-1330 - the long-keyword switch matches lower-case
/// initials and the `default:` arm passes the raw byte through, so `S H R P`
/// are prefixes while `s h r p` fail `RULE_STRCMP` and fall to
/// `filter_rule_err("Unknown filter rule")` at :1362-1363. Case-folding the
/// prefix accepted four spellings upstream rejects.
const SINGLE_LETTER_PREFIXES: &str = "SHRP";

/// Parses a single line of a per-directory merge file.
///
/// Returns `Ok(None)` for blank or comment-only lines. Recognises list-clear
/// (`!`/`clear`), short-form merges (`.`/`:`), `merge`/`dir-merge`/`per-dir`
/// directives, `exclude-if-present`, the `+`/`-` short-form rule prefixes,
/// and the `include`/`exclude`/`show`/`hide`/`protect`/`risk` keywords with
/// their `,modifier` suffix. Trailing whitespace is trimmed from patterns.
pub(crate) fn parse_filter_directive_line(
    text: &str,
) -> Result<Option<ParsedFilterDirective>, FilterParseError> {
    if text.is_empty() || text.starts_with('#') {
        return Ok(None);
    }

    let trimmed = text.trim_start();
    if trimmed.is_empty() {
        return Ok(None);
    }

    let trimmed = trimmed.trim_end();

    if trimmed == "!" || trimmed == CLEAR_KEYWORD {
        return Ok(Some(ParsedFilterDirective::Clear));
    }

    if let Some(directive) = parse_short_merge_directive_line(trimmed)? {
        return Ok(Some(directive));
    }

    if let Some(directive) = parse_merge_directive(trimmed)? {
        return Ok(Some(directive));
    }

    if let Some(directive) = parse_dir_merge_directive(trimmed)? {
        return Ok(Some(directive));
    }

    if trimmed.len() >= EXCLUDE_IF_PRESENT_PREFIX.len()
        && trimmed[..EXCLUDE_IF_PRESENT_PREFIX.len()]
            .eq_ignore_ascii_case(EXCLUDE_IF_PRESENT_PREFIX)
    {
        let mut remainder = trimmed[EXCLUDE_IF_PRESENT_PREFIX.len()..]
            .trim_start_matches(|ch: char| ch == '_' || ch.is_ascii_whitespace());
        if let Some(rest) = remainder.strip_prefix('=') {
            remainder = rest.trim_start_matches(|ch: char| ch == '_' || ch.is_ascii_whitespace());
        }

        let pattern_text = remainder.trim();
        if pattern_text.is_empty() {
            return Err(FilterParseError::new(
                "filter directive 'exclude-if-present' requires a marker file",
            ));
        }

        return Ok(Some(ParsedFilterDirective::ExcludeIfPresent(
            ExcludeIfPresentRule::new(pattern_text),
        )));
    }

    if let Some(remainder) = trimmed.strip_prefix('+') {
        let (modifier_text, remainder) = split_short_rule_modifiers(remainder);
        let modifiers = parse_rule_modifiers(modifier_text, trimmed, false)?;
        let pattern = remainder.trim_start();
        if pattern.is_empty() {
            return Err(FilterParseError::new("filter rule '+' requires a pattern"));
        }
        let rule = FilterRule::include(pattern.to_owned());
        let rule = apply_rule_modifiers(rule, modifiers, trimmed)?;
        return Ok(Some(ParsedFilterDirective::Rule(rule)));
    }

    if let Some(remainder) = trimmed.strip_prefix('-') {
        let (modifier_text, remainder) = split_short_rule_modifiers(remainder);
        let modifiers = parse_rule_modifiers(modifier_text, trimmed, false)?;
        let pattern = remainder.trim_start();
        if pattern.is_empty() {
            return Err(FilterParseError::new("filter rule '-' requires a pattern"));
        }
        let rule = FilterRule::exclude(pattern.to_owned());
        let rule = apply_rule_modifiers(rule, modifiers, trimmed)?;
        return Ok(Some(ParsedFilterDirective::Rule(rule)));
    }

    let mut parts = trimmed.splitn(2, char::is_whitespace);
    let keyword = parts.next().unwrap_or("");
    let remainder = parts.next().unwrap_or("").trim_start();
    let (keyword, keyword_modifiers) = split_keyword_modifiers(keyword);

    let handle_keyword = |pattern: &str,
                          builder: fn(String) -> FilterRule,
                          prefix_specifies_side: bool|
     -> Result<Option<ParsedFilterDirective>, FilterParseError> {
        if pattern.is_empty() {
            return Err(FilterParseError::new("filter directive missing pattern"));
        }
        let modifiers = parse_rule_modifiers(keyword_modifiers, trimmed, prefix_specifies_side)?;
        let rule = builder(pattern.to_owned());
        let rule = apply_rule_modifiers(rule, modifiers, trimmed)?;
        Ok(Some(ParsedFilterDirective::Rule(rule)))
    };

    if keyword.len() == 1 {
        let shorthand = keyword
            .chars()
            .next()
            .expect("keyword has exactly one char");
        // upstream: exclude.c:1365 - the modifier loop runs after EVERY rule
        // prefix, so `P`/`R` take a modifier run exactly as `S`/`H` do. The
        // blanket refusal that used to sit on these two arms had no upstream
        // counterpart; per-modifier validity is `parse_rule_modifiers`' job.
        match shorthand {
            'P' => {
                return handle_keyword(remainder, FilterRule::protect, true);
            }
            'R' => {
                return handle_keyword(remainder, FilterRule::risk, true);
            }
            'S' => {
                return handle_keyword(remainder, FilterRule::show, true);
            }
            'H' => {
                return handle_keyword(remainder, FilterRule::hide, true);
            }
            _ => {}
        }
    }

    // upstream: exclude.c:1288-1327 - the keyword switch dispatches on the raw
    // first byte (only lower-case initials `c d e h i m p r s` have arms) and
    // compares with `rule_strcmp`, which is `strncmp` (:1218). `EXCLUDE` falls
    // to `default: ch = *s` and then to the `Unknown filter rule` arm (:1362).
    match keyword {
        "include" => return handle_keyword(remainder, FilterRule::include, false),
        "exclude" => return handle_keyword(remainder, FilterRule::exclude, false),
        "show" => return handle_keyword(remainder, FilterRule::show, true),
        "hide" => return handle_keyword(remainder, FilterRule::hide, true),
        "protect" => return handle_keyword(remainder, FilterRule::protect, true),
        "risk" => return handle_keyword(remainder, FilterRule::risk, true),
        _ => {}
    }

    // upstream: exclude.c:1363 `filter_rule_err("Unknown filter rule", ...)`,
    // whose text argument is passed through `rule_text` (exclude.c:88-123) -
    // the chokepoint every diagnostic built from a rule's own text must cross.
    //
    // The rule text is REPLACED, not echoed. Both production callers of this
    // function are inside `load_dir_merge_rules_recursive`, i.e. the contents
    // of a per-directory merge file. That file is named by a rule the PEER
    // controls and which travelled over the protocol, so echoing an unparsable
    // line back turns the filter parser into a read-any-line oracle for any
    // file this process can open. Upstream describes that provenance as
    // `a file read earlier` (exclude.c:78) because the naming rule was read and
    // closed long ago and its origin is not retained.
    //
    // ⚠ The replacement is NOT unconditional redaction: upstream still echoes a
    // rule that came from an ARGUMENT, because that text is the operator's own.
    // `RuleSource` encodes exactly that distinction, and `Argument` returns the
    // text unchanged - which is why the funnel is used here rather than a local
    // `format!`.
    Err(FilterParseError::new(format!(
        "Unknown filter rule: {}",
        RuleSource::FileReadEarlier.rule_text(trimmed)
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule_of(line: &str) -> FilterRule {
        match parse_filter_directive_line(line) {
            Ok(Some(ParsedFilterDirective::Rule(rule))) => rule,
            other => panic!("expected a rule from {line:?}, got {other:?}"),
        }
    }

    /// MEASURED against rsync 3.5.0: `-R FOO` / `-S FOO` / `-P FOO` / `-X FOO`
    /// all exit 1. oc case-folded the modifier byte, so they parsed as
    /// receiver / sender / perishable / xattr and applied a restriction the
    /// rule never expressed - at exit 0. `-S FOO` dropped FOO while `-R FOO`
    /// kept it, which is what made this a silent wrong transfer rather than a
    /// cosmetic difference.
    ///
    /// upstream: exclude.c:1365 switches on the raw byte; the uppercase forms
    /// reach the default arm at :1371 and raise RERR_SYNTAX.
    #[test]
    fn uppercase_modifier_letters_are_not_modifiers() {
        for line in ["-R FOO", "-S FOO", "-P FOO", "-X FOO", "+R FOO"] {
            assert!(
                parse_filter_directive_line(line).is_err(),
                "{line:?} must be refused: upstream has no uppercase modifiers"
            );
        }
    }

    /// Non-vacuity companion to the case test: the LOWERCASE letters must still
    /// work. Without this, deleting the whole modifier match arm would also
    /// make the test above pass.
    #[test]
    fn lowercase_modifier_letters_are_still_accepted() {
        assert!(rule_of("-s FOO").applies_to_sender());
        assert!(!rule_of("-s FOO").applies_to_receiver());
        assert!(rule_of("-r FOO").applies_to_receiver());
        assert!(!rule_of("-r FOO").applies_to_sender());
        assert!(rule_of("-x user.drop").is_xattr_only());
    }

    /// upstream: exclude.c:1423-1432 - `s`/`r` are refused once a prefix has
    /// already bound a side, which the four side-bound spellings all do
    /// (exclude.c:1345-1358). oc accepted `protect,s`, building a SENDER-side
    /// protect rule - a rule upstream cannot express.
    #[test]
    fn a_side_bound_prefix_refuses_a_redundant_side_modifier() {
        for line in [
            "protect,s FOO",
            "protect,r FOO",
            "risk,s FOO",
            "show,r FOO",
            "hide,r FOO",
            "Ps FOO",
            "Hr FOO",
        ] {
            assert!(
                parse_filter_directive_line(line).is_err(),
                "{line:?} must be refused: the prefix already binds a side"
            );
        }
    }

    /// Non-vacuity companion: `p` is one of the three arms upstream leaves
    /// unguarded (exclude.c:1420), so it must be accepted on exactly the
    /// prefixes that reject `s`/`r`. Without this the guard above could be
    /// implemented as "side-bound prefixes take no modifiers at all", which is
    /// what oc previously did for `P`/`R` and is equally wrong.
    #[test]
    fn a_side_bound_prefix_still_accepts_the_unguarded_perishable_modifier() {
        for line in ["protect,p FOO", "risk,p FOO", "show,p FOO", "P,p FOO"] {
            assert!(
                parse_filter_directive_line(line).is_ok(),
                "{line:?} must parse: upstream's `p` arm has no prefix guard"
            );
        }
    }

    /// For a single-letter prefix the comma is decoration.
    ///
    /// upstream: exclude.c:1325-1329 - the prefix switch's `default:` arm
    /// advances past only an OPTIONAL comma, so the modifier loop at :1365
    /// starts at the same byte either way. `Pp FOO` and `P,p FOO` are the same
    /// rule. A long keyword is the opposite: `rule_strcmp` (exclude.c:1218)
    /// requires the comma, so `protect,x` parses and `protectx` does not.
    #[test]
    fn a_single_letter_prefix_takes_adjacent_modifiers_the_keyword_form_needs_a_comma() {
        for line in ["Pp FOO", "P,p FOO", "Px user.drop", "P,x user.drop"] {
            assert!(
                parse_filter_directive_line(line).is_ok(),
                "{line:?} must parse: the comma is optional after a one-letter prefix"
            );
        }
        assert_eq!(
            rule_of("Pp FOO").is_perishable(),
            rule_of("P,p FOO").is_perishable(),
            "the two spellings must produce the same rule, not merely both parse"
        );
        assert!(
            parse_filter_directive_line("protectx user.drop").is_err(),
            "a long keyword still requires the comma (rule_strcmp, exclude.c:1218)"
        );
    }

    /// Lower-case single letters are NOT prefixes.
    ///
    /// upstream: exclude.c:1288-1330 - the first switch matches lower-case
    /// keyword initials, so `p`/`r`/`s`/`h` enter `RULE_STRCMP`, fail it, leave
    /// `ch == 0` and reach `filter_rule_err("Unknown filter rule")` at :1362.
    /// Only `S H R P` reach the `default:` arm as prefixes. Measured against
    /// rsync 3.5.0: `p *.tmp` in a dir-merge file exits 1.
    #[test]
    fn lower_case_single_letters_are_not_rule_prefixes() {
        for line in ["p *.tmp", "r *.tmp", "s *.tmp", "h *.tmp"] {
            assert!(
                parse_filter_directive_line(line).is_err(),
                "{line:?} must be refused: upstream has no lower-case prefix"
            );
        }
        for line in ["P *.tmp", "R *.tmp", "S *.tmp", "H *.tmp"] {
            assert!(
                parse_filter_directive_line(line).is_ok(),
                "{line:?} is the upper-case prefix upstream does accept"
            );
        }
    }

    /// upstream: exclude.c:1288-1327 - the long-keyword switch dispatches on
    /// the raw first byte and has arms only for the lower-case initials
    /// `c d e h i m p r s`, and `rule_strcmp` is `strncmp` (:1218). An
    /// upper-case spelling therefore takes `default: ch = *s` and lands on
    /// `filter_rule_err("Unknown filter rule")` at :1362.
    ///
    /// Every upper-case spelling is refused, but in one of TWO ways, and which
    /// one depends on whether the keyword's first letter is itself a rule
    /// prefix. `show`/`hide`/`protect`/`risk` begin with `S`/`H`/`P`/`R`, so
    /// upstream consumes that byte as the prefix and the modifier loop
    /// (exclude.c:1365) then fails on the SECOND letter. `include`/`exclude`
    /// begin with `I`/`E`, which are not prefixes, so they reach the unknown-
    /// rule arm instead.
    ///
    /// MEASURED against rsync 3.5.0, one cell per row:
    ///   `INCLUDE foo` -> `Unknown filter rule: INCLUDE foo`
    ///   `EXCLUDE foo` -> `Unknown filter rule: EXCLUDE foo`
    ///   `Show foo`    -> `invalid modifier 'h' at position 1`
    ///   `Hide foo`    -> `invalid modifier 'i' at position 1`
    ///   `PROTECT foo` -> `invalid modifier 'R' at position 1`
    ///   `Risk foo`    -> `invalid modifier 'i' at position 1`
    /// all exit 1.
    #[test]
    fn the_long_keywords_are_case_sensitive() {
        for (line, expected_fragment) in [
            ("INCLUDE foo", "Unknown filter rule"),
            ("EXCLUDE foo", "Unknown filter rule"),
            ("Show foo", "modifier 'h'"),
            ("Hide foo", "modifier 'i'"),
            ("PROTECT foo", "modifier 'R'"),
            ("Risk foo", "modifier 'i'"),
        ] {
            let error = parse_filter_directive_line(line)
                .expect_err("an upper-case keyword is not a filter directive");
            assert!(
                error.to_string().contains(expected_fragment),
                "{line:?} should be refused with {expected_fragment:?}, got: {error}"
            );
        }
    }

    /// Non-vacuity companion for `the_long_keywords_are_case_sensitive`:
    /// without it that test would also pass if the parser rejected every
    /// keyword form.
    #[test]
    fn the_long_keywords_still_parse_in_lower_case() {
        for line in [
            "include foo",
            "exclude foo",
            "show foo",
            "hide foo",
            "protect foo",
            "risk foo",
        ] {
            assert!(
                parse_filter_directive_line(line).is_ok(),
                "{line:?} is the spelling upstream accepts"
            );
        }
    }

    /// `SHOW foo` is the sharpest cell: `S` IS a valid prefix, so upstream
    /// consumes it and then runs the modifier loop over `HOW`, failing on the
    /// first byte. MEASURED against rsync 3.5.0: `--filter='SHOW foo'` prints
    /// `invalid modifier 'H' at position 1` and exits 1 - a different error
    /// from the `Unknown filter rule` the other five produce.
    #[test]
    fn an_upper_case_keyword_starting_with_a_prefix_letter_fails_in_the_modifier_run() {
        let error = parse_filter_directive_line("SHOW foo")
            .expect_err("S is a prefix, so HOW is scanned as modifiers");
        let message = error.to_string();
        assert!(
            message.contains("modifier") && message.contains('H'),
            "expected a modifier error naming 'H', got: {message}"
        );
    }

    /// upstream: exclude.c:1395-1401 - `!` sets FILTRULE_NEGATE after any
    /// non-merge prefix. Measured against 3.5.0: `-! *.tmp` transfers only the
    /// `*.tmp` files (the sense is inverted), where oc used to exit 1.
    #[test]
    fn the_negate_modifier_is_accepted_after_every_non_merge_prefix() {
        for line in ["-! *.tmp", "+! *.tmp", "P! *.tmp", "R! *.tmp"] {
            assert!(
                parse_filter_directive_line(line).is_ok(),
                "{line:?} must parse: `!` carries no prefix guard upstream"
            );
        }
        assert!(
            rule_of("-! *.tmp").is_negated(),
            "the modifier must reach the rule, not merely be tolerated"
        );
        assert!(
            !rule_of("- *.tmp").is_negated(),
            "and the same rule without `!` must not be negated"
        );
    }

    /// upstream: exclude.c:1438 - the `x` arm sets `FILTRULE_XATTR` and nothing
    /// else. oc forced both sides on, so `-sx` silently widened a sender-only
    /// xattr rule to both ends.
    #[test]
    fn the_x_modifier_does_not_clobber_an_explicit_side() {
        let sender_only = rule_of("-sx user.drop");
        assert!(sender_only.is_xattr_only());
        assert!(sender_only.applies_to_sender());
        assert!(
            !sender_only.applies_to_receiver(),
            "`x` must not re-enable the receiver side that `s` switched off"
        );

        let receiver_only = rule_of("-rx user.drop");
        assert!(receiver_only.is_xattr_only());
        assert!(receiver_only.applies_to_receiver());
        assert!(!receiver_only.applies_to_sender());
    }

    #[test]
    fn split_keyword_modifiers_no_modifiers() {
        let (name, mods) = split_keyword_modifiers("include");
        assert_eq!(name, "include");
        assert_eq!(mods, "");
    }

    #[test]
    fn split_keyword_modifiers_with_modifiers() {
        let (name, mods) = split_keyword_modifiers("include,/s");
        assert_eq!(name, "include");
        assert_eq!(mods, "/s");
    }

    #[test]
    fn parse_filter_directive_line_empty() {
        let result = parse_filter_directive_line("").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn parse_filter_directive_line_comment() {
        let result = parse_filter_directive_line("# this is a comment").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn parse_filter_directive_line_whitespace() {
        let result = parse_filter_directive_line("   ").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn parse_filter_directive_line_clear_exclamation() {
        let result = parse_filter_directive_line("!").unwrap();
        assert!(matches!(result, Some(ParsedFilterDirective::Clear)));
    }

    #[test]
    fn parse_filter_directive_line_clear_keyword() {
        let result = parse_filter_directive_line("clear").unwrap();
        assert!(matches!(result, Some(ParsedFilterDirective::Clear)));
    }

    #[test]
    fn parse_filter_directive_line_include_short() {
        let result = parse_filter_directive_line("+ *.txt").unwrap();
        assert!(matches!(result, Some(ParsedFilterDirective::Rule(_))));
    }

    #[test]
    fn parse_filter_directive_line_exclude_short() {
        let result = parse_filter_directive_line("- *.bak").unwrap();
        assert!(matches!(result, Some(ParsedFilterDirective::Rule(_))));
    }

    #[test]
    fn parse_filter_directive_line_include_keyword() {
        let result = parse_filter_directive_line("include *.txt").unwrap();
        assert!(matches!(result, Some(ParsedFilterDirective::Rule(_))));
    }

    #[test]
    fn parse_filter_directive_line_exclude_keyword() {
        let result = parse_filter_directive_line("exclude *.bak").unwrap();
        assert!(matches!(result, Some(ParsedFilterDirective::Rule(_))));
    }

    #[test]
    fn parse_filter_directive_line_protect() {
        let result = parse_filter_directive_line("protect /important").unwrap();
        assert!(matches!(result, Some(ParsedFilterDirective::Rule(_))));
    }

    #[test]
    fn parse_filter_directive_line_risk() {
        let result = parse_filter_directive_line("risk /temp").unwrap();
        assert!(matches!(result, Some(ParsedFilterDirective::Rule(_))));
    }

    #[test]
    fn parse_filter_directive_line_show() {
        let result = parse_filter_directive_line("show *.log").unwrap();
        assert!(matches!(result, Some(ParsedFilterDirective::Rule(_))));
    }

    #[test]
    fn parse_filter_directive_line_hide() {
        let result = parse_filter_directive_line("hide *.secret").unwrap();
        assert!(matches!(result, Some(ParsedFilterDirective::Rule(_))));
    }

    #[test]
    fn parse_filter_directive_line_exclude_if_present() {
        let result = parse_filter_directive_line("exclude-if-present .nobackup").unwrap();
        assert!(matches!(
            result,
            Some(ParsedFilterDirective::ExcludeIfPresent(_))
        ));
    }

    #[test]
    fn parse_filter_directive_line_unsupported() {
        let result = parse_filter_directive_line("unknown directive");
        assert!(result.is_err());
    }

    #[test]
    fn parse_filter_directive_line_plus_missing_pattern() {
        let result = parse_filter_directive_line("+ ");
        assert!(result.is_err());
    }

    #[test]
    fn parse_filter_directive_line_minus_missing_pattern() {
        let result = parse_filter_directive_line("- ");
        assert!(result.is_err());
    }

    /// upstream: exclude.c:1419-1428 - the `dir-merge` keyword and the `:`
    /// short form both register a per-directory merge rule. Both forms must
    /// produce `ParsedFilterDirective::DirMerge` so the runtime can defer
    /// the file lookup to each subdirectory rather than loading it eagerly
    /// against the enclosing directory.
    #[test]
    fn parse_filter_directive_line_dir_merge_keyword_emits_dir_merge_variant() {
        let result = parse_filter_directive_line("dir-merge .filt2").unwrap();
        assert!(
            matches!(result, Some(ParsedFilterDirective::DirMerge { .. })),
            "dir-merge keyword must emit DirMerge variant"
        );
    }

    #[test]
    fn parse_filter_directive_line_colon_short_form_emits_dir_merge_variant() {
        let result = parse_filter_directive_line(": .filt2").unwrap();
        assert!(
            matches!(result, Some(ParsedFilterDirective::DirMerge { .. })),
            "':' short form must emit DirMerge variant"
        );
    }

    /// MEASURED against rsync 3.5.0 (`filter-merge-content-echo`): a per-directory
    /// merge file holding an unparsable line makes upstream report
    /// `<rule from a file read earlier>`, while oc echoed the line itself. The
    /// merge file is named by a rule the peer controls, so the echo turned the
    /// filter parser into a read-any-line oracle for any file this process can
    /// open.
    ///
    /// upstream: exclude.c:1363 routes the text through `rule_text`
    /// (exclude.c:78-123), whose `a file read earlier` arm is exactly this
    /// provenance.
    #[test]
    fn an_unparsable_merged_line_is_not_echoed_back() {
        const SECRET: &str = "TOP-SECRET-PASSWORD-abc123";

        let error = parse_filter_directive_line(SECRET)
            .expect_err("an unparsable line must still be refused");
        let rendered = error.to_string();

        assert!(
            !rendered.contains(SECRET),
            "the merged line's contents must not be echoed: {rendered}"
        );
        assert!(
            rendered.contains("a file read earlier"),
            "the diagnostic must name the rule's provenance: {rendered}"
        );
    }

    /// Non-vacuity companion: the redaction is a PROVENANCE decision, not blanket
    /// suppression. Upstream still echoes a rule that came from an argument,
    /// because that text is the operator's own (`rule_text` returns it unchanged,
    /// exclude.c:88-123). Without this, replacing the funnel with a constant
    /// string would also satisfy the test above.
    #[test]
    fn an_argument_sourced_rule_is_still_echoed() {
        const SECRET: &str = "TOP-SECRET-PASSWORD-abc123";

        assert_eq!(RuleSource::Argument.rule_text(SECRET), SECRET);
    }
}
