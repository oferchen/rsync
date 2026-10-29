use core::client::FilterRuleSpec;
use core::message::Message;
use filters::{CvsTemplate, ModifierScope, RuleModifiers};

use super::super::directive::FilterDirective;
use super::helpers::{parse_modifier_run, unexpected_end_of_filter_rule};
use super::rule_line::RuleLine;

/// Parses the modifier run that may trail a non-merge rule prefix or keyword,
/// through the table every rule parser shares.
///
/// `prefix_specifies_side` is set for the `H`/`S`/`P`/`R` (hide/show/protect/
/// risk) rules, whose prefix already binds the rule to a side. Upstream then
/// refuses `s`, `r` and `C` on them (exclude.c:1402-1404, 1423-1432); `p`, `x`,
/// `/` and `!` stay valid everywhere.
pub(super) fn parse_rule_modifiers(
    modifiers: &str,
    line: RuleLine<'_>,
    prefix_specifies_side: bool,
) -> Result<RuleModifiers, Message> {
    let scope = if prefix_specifies_side {
        ModifierScope::SIDED
    } else {
        ModifierScope::PLAIN
    };
    parse_modifier_run(modifiers, scope, line)
}

/// Builds what a non-merge rule parses to once its modifiers are known.
///
/// A `C` rule stands for the CVS ignore list rather than its own pattern:
/// upstream lets it have none (exclude.c:1474), never matches it
/// (exclude.c:1201-1206 check_filter) and builds the list from the rule's flags
/// (exclude.c:1595-1597). Any other rule needs a pattern, or it is upstream's
/// "unexpected end of filter rule".
pub(super) fn rule_directive(
    line: RuleLine<'_>,
    builder: fn(String) -> FilterRuleSpec,
    include: bool,
    pattern: &str,
    modifiers: RuleModifiers,
) -> Result<FilterDirective, Message> {
    if modifiers.cvs_ignore {
        return Ok(FilterDirective::CvsDefaults(CvsTemplate::new(
            include, &modifiers,
        )));
    }
    if pattern.is_empty() {
        return Err(unexpected_end_of_filter_rule(line));
    }
    Ok(FilterDirective::Rule(apply_rule_modifiers(
        builder(pattern.to_owned()),
        modifiers,
    )))
}

/// Applies parsed modifier flags onto a `FilterRuleSpec`: the anchor, the
/// sender/receiver side, perishable, negate and xattr-only.
pub(super) fn apply_rule_modifiers(
    mut rule: FilterRuleSpec,
    modifiers: RuleModifiers,
) -> FilterRuleSpec {
    if modifiers.abs_path {
        rule = rule.with_anchor();
    }

    if modifiers.sender_side != modifiers.receiver_side {
        rule = rule
            .with_sender(modifiers.sender_side)
            .with_receiver(modifiers.receiver_side);
    }

    if modifiers.perishable {
        rule = rule.with_perishable(true);
    }

    if modifiers.negate {
        rule = rule.with_negate(true);
    }

    // upstream: exclude.c:1438 - `case 'x': rule->rflags |= FILTRULE_XATTR;`.
    // The modifier sets one bit and touches nothing else; in particular it does
    // not bind a side. `+`/`-` already default to both sides, so forcing them
    // here was a no-op there while silently erasing the one-sided semantics of
    // the H/S/P/R prefixes (hide/show/protect/risk), which exist precisely to
    // express a side (exclude.c:1345-1358).
    if modifiers.xattr {
        rule = rule.with_xattr_only(true);
    }

    rule
}

#[cfg(test)]
mod tests {
    use filters::RuleSource;

    use super::*;

    fn arg(text: &str) -> RuleLine<'_> {
        RuleLine::new(text, RuleSource::Argument)
    }

    fn mods(run: &'static str, side: bool) -> RuleModifiers {
        parse_rule_modifiers(run, arg(run), side).expect("modifiers parse")
    }

    #[test]
    fn sender_or_receiver_modifier_binds_one_side() {
        let sender =
            apply_rule_modifiers(FilterRuleSpec::include("a".to_owned()), mods("s", false));
        assert!(sender.applies_to_sender() && !sender.applies_to_receiver());
        let receiver =
            apply_rule_modifiers(FilterRuleSpec::include("a".to_owned()), mods("r", false));
        assert!(!receiver.applies_to_sender() && receiver.applies_to_receiver());
        let both = apply_rule_modifiers(FilterRuleSpec::include("a".to_owned()), mods("sr", false));
        assert!(both.applies_to_sender() && both.applies_to_receiver());
    }

    #[test]
    fn anchor_perishable_negate_and_xattr_reach_the_rule() {
        let rule =
            apply_rule_modifiers(FilterRuleSpec::exclude("a".to_owned()), mods("/p!x", false));
        assert!(rule.pattern().starts_with('/'));
        assert!(rule.is_perishable() && rule.is_negated() && rule.is_xattr_only());
    }

    #[test]
    fn modifiers_are_case_sensitive_and_side_prefixes_refuse_s_r_and_c() {
        // upstream: exclude.c:1365-1443 matches the raw byte, and exclude.c:
        // 1402-1404, 1423-1432 refuse `C`/`s`/`r` after a side-binding prefix.
        for run in ["S", "R", "X", "z"] {
            assert!(parse_rule_modifiers(run, arg(run), false).is_err(), "{run}");
        }
        for run in ["s", "r", "C"] {
            assert!(parse_rule_modifiers(run, arg(run), true).is_err(), "{run}");
        }
        assert!(mods("p", true).perishable);
    }

    #[test]
    fn xattr_modifier_keeps_a_protect_rule_receiver_only() {
        // upstream: exclude.c:1355-1357 - `P` sets FILTRULE_RECEIVER_SIDE, and
        // :1438 `case 'x'` sets only FILTRULE_XATTR, so `Px` stays receiver-only
        // (measured against rsync 3.5.0: exit 0).
        let rule =
            apply_rule_modifiers(FilterRuleSpec::protect("*.rs".to_owned()), mods("x", true));
        assert!(rule.is_xattr_only());
        assert!(!rule.applies_to_sender() && rule.applies_to_receiver());
    }

    #[test]
    fn c_modifier_makes_the_rule_stand_for_the_cvs_list() {
        let directive = rule_directive(
            arg("-sC"),
            FilterRuleSpec::exclude,
            false,
            "",
            mods("sC", false),
        );
        let FilterDirective::CvsDefaults(template) = directive.expect("C needs no pattern") else {
            panic!("a C rule must stand for the CVS list");
        };
        assert!(!template.include && template.sender_side && !template.receiver_side);
    }

    #[test]
    fn a_rule_without_c_still_needs_a_pattern() {
        assert!(
            rule_directive(
                arg("-p"),
                FilterRuleSpec::exclude,
                false,
                "",
                mods("p", false)
            )
            .is_err()
        );
    }
}
