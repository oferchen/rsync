use super::{
    dir_merge::parse_dir_merge_directive,
    merge::{parse_merge_directive, parse_short_merge_directive_line},
    modifiers::split_short_rule_modifiers,
    types::{FilterParseError, ParsedFilterDirective},
};
use crate::local_copy::filter_program::ExcludeIfPresentRule;
use filters::FilterRule;
use std::fmt;

#[derive(Default)]
struct RuleModifierState {
    anchor_root: bool,
    sender: Option<bool>,
    receiver: Option<bool>,
    perishable: bool,
    xattr_only: bool,
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
    allow_perishable: bool,
    allow_xattr: bool,
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
            'p' => {
                if allow_perishable {
                    state.perishable = true;
                } else {
                    return Err(unsupported_modifier_error(directive, modifier));
                }
            }
            'x' => {
                if allow_xattr {
                    state.xattr_only = true;
                } else {
                    return Err(unsupported_modifier_error(directive, modifier));
                }
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

    if modifiers.xattr_only {
        match rule.action() {
            filters::FilterAction::Include | filters::FilterAction::Exclude => {
                // upstream: exclude.c:1438 - `case 'x': rule->rflags |=
                // FILTRULE_XATTR;`. The modifier sets one bit and binds no
                // side, so forcing both sides here erased whatever `s`/`r`
                // had already selected: `-sx pat` became a both-sides rule.
                rule = rule.with_xattr_only(true);
            }
            _ => {
                return Err(FilterParseError::new(format!(
                    "filter directive '{directive}' cannot combine 'x' modifiers with this directive"
                )));
            }
        }
    }

    Ok(rule)
}

fn split_keyword_modifiers(keyword: &str) -> (&str, &str) {
    if let Some((name, modifiers)) = keyword.split_once(',') {
        (name, modifiers)
    } else {
        (keyword, "")
    }
}

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

    if trimmed == "!" || trimmed.eq_ignore_ascii_case("clear") {
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

    const EXCLUDE_IF_PRESENT_PREFIX: &str = "exclude-if-present";

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
        let modifiers = parse_rule_modifiers(modifier_text, trimmed, true, true, false)?;
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
        let modifiers = parse_rule_modifiers(modifier_text, trimmed, true, true, false)?;
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
                          allow_perishable: bool,
                          allow_xattr: bool,
                          prefix_specifies_side: bool|
     -> Result<Option<ParsedFilterDirective>, FilterParseError> {
        if pattern.is_empty() {
            return Err(FilterParseError::new("filter directive missing pattern"));
        }
        let modifiers = parse_rule_modifiers(
            keyword_modifiers,
            trimmed,
            allow_perishable,
            allow_xattr,
            prefix_specifies_side,
        )?;
        let rule = builder(pattern.to_owned());
        let rule = apply_rule_modifiers(rule, modifiers, trimmed)?;
        Ok(Some(ParsedFilterDirective::Rule(rule)))
    };

    if keyword.len() == 1 {
        let shorthand = keyword
            .chars()
            .next()
            .expect("keyword has exactly one char")
            .to_ascii_lowercase();
        // upstream: exclude.c:1365 - the modifier loop runs after EVERY rule
        // prefix, so `P`/`R` take a modifier run exactly as `S`/`H` do. The
        // blanket refusal that used to sit on these two arms had no upstream
        // counterpart; per-modifier validity is `parse_rule_modifiers`' job.
        match shorthand {
            'p' => {
                return handle_keyword(remainder, FilterRule::protect, true, false, true);
            }
            'r' => {
                return handle_keyword(remainder, FilterRule::risk, true, false, true);
            }
            's' => {
                return handle_keyword(remainder, FilterRule::show, true, false, true);
            }
            'h' => {
                return handle_keyword(remainder, FilterRule::hide, true, false, true);
            }
            _ => {}
        }
    }

    if keyword.eq_ignore_ascii_case("include") {
        return handle_keyword(remainder, FilterRule::include, true, true, false);
    }

    if keyword.eq_ignore_ascii_case("exclude") {
        return handle_keyword(remainder, FilterRule::exclude, true, true, false);
    }

    if keyword.eq_ignore_ascii_case("show") {
        return handle_keyword(remainder, FilterRule::show, true, false, true);
    }

    if keyword.eq_ignore_ascii_case("hide") {
        return handle_keyword(remainder, FilterRule::hide, true, false, true);
    }

    if keyword.eq_ignore_ascii_case("protect") {
        return handle_keyword(remainder, FilterRule::protect, true, false, true);
    }

    if keyword.eq_ignore_ascii_case("risk") {
        return handle_keyword(remainder, FilterRule::risk, true, false, true);
    }

    Err(FilterParseError::new(format!(
        "Unknown filter rule: `{trimmed}'"
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

    /// MEASURED divergence this change does NOT close, pinned so it cannot be
    /// mistaken for intended behaviour: upstream takes the modifier run
    /// adjacent to a single-letter prefix (`Pp FOO`, `Px user.drop`), while
    /// this parser only reaches its modifier scan when a comma separates them
    /// (`P,p FOO`). Upstream 3.5.0 accepts `Pp FOO` at exit 0; oc exits 1.
    ///
    /// This is a parse-SHAPE gap, distinct from the `x` gate below it - which
    /// is why `Px` fails for two independent reasons today.
    #[test]
    fn adjacent_modifiers_after_a_single_letter_prefix_are_not_yet_parsed() {
        assert!(
            parse_filter_directive_line("Pp FOO").is_err(),
            "pin the known gap: adjacent-modifier support is not implemented"
        );
        assert!(
            parse_filter_directive_line("P,p FOO").is_ok(),
            "the comma form is what oc supports today"
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
}
