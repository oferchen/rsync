use core::client::FilterRuleSpec;
use core::message::{Message, Role};
use core::rsync_error;

use super::rule_line::RuleLine;

/// Accumulated rule modifiers parsed from a rule prefix or keyword.
#[derive(Default)]
pub(super) struct RuleModifierState {
    anchor_root: bool,
    sender: Option<bool>,
    receiver: Option<bool>,
    perishable: bool,
    xattr_only: bool,
    negate: bool,
}

fn unsupported_modifier(line: RuleLine<'_>, modifier: char) -> Message {
    // The rule text crosses `rule_text` (exclude.c:88-123) and the offending
    // character crosses `rule_detail` (exclude.c:126-131): a character of a
    // hidden line is still a byte of that line.
    rsync_error!(
        1,
        format!(
            "filter rule '{}' uses unsupported modifier{}",
            line.shown(),
            line.detail(&format!(" '{modifier}'"))
        )
    )
    .with_role(Role::Client)
}

/// Parses the modifier characters that may trail a rule prefix or keyword.
///
/// upstream: exclude.c:1214-1287 parse_rule_tok - the modifier loop switches on
/// the literal byte, so modifiers are strictly case-sensitive. An uppercase or
/// otherwise unknown character reaches the `default:` arm and raises
/// "invalid modifier" (RERR_SYNTAX). We mirror that by matching the char
/// verbatim rather than lower-casing it first.
///
/// `prefix_specifies_side` is set for the `H`/`S`/`P`/`R` (hide/show/protect/
/// risk) rules, whose prefix already binds the rule to a side. Upstream then
/// rejects the `s`/`r` modifiers as invalid on those rules
/// (exclude.c:1269-1277); `p` (perishable) carries no such guard and stays
/// valid everywhere (exclude.c:1265-1267).
pub(super) fn parse_rule_modifiers(
    modifiers: &str,
    line: RuleLine<'_>,
    allow_perishable: bool,
    allow_xattr: bool,
    prefix_specifies_side: bool,
) -> Result<RuleModifierState, Message> {
    let mut state = RuleModifierState::default();

    for modifier in modifiers.chars() {
        match modifier {
            '/' => {
                state.anchor_root = true;
            }
            's' => {
                // upstream: exclude.c:1275-1276 - `s` is invalid once the prefix
                // has already fixed the rule's side.
                if prefix_specifies_side {
                    return Err(unsupported_modifier(line, modifier));
                }
                state.sender = Some(true);
                if state.receiver.is_none() {
                    state.receiver = Some(false);
                }
            }
            'r' => {
                // upstream: exclude.c:1270-1271 - `r` is invalid once the prefix
                // has already fixed the rule's side.
                if prefix_specifies_side {
                    return Err(unsupported_modifier(line, modifier));
                }
                state.receiver = Some(true);
                if state.sender.is_none() {
                    state.sender = Some(false);
                }
            }
            '!' => {
                // upstream: exclude.c - '!' modifier inverts the match result
                // (FILTRULE_NEGATE). Not valid on merge-file rules.
                state.negate = true;
            }
            'p' => {
                if allow_perishable {
                    state.perishable = true;
                } else {
                    return Err(unsupported_modifier(line, modifier));
                }
            }
            'x' => {
                if allow_xattr {
                    state.xattr_only = true;
                } else {
                    return Err(unsupported_modifier(line, modifier));
                }
            }
            _ => {
                return Err(unsupported_modifier(line, modifier));
            }
        }
    }

    Ok(state)
}

/// Applies a parsed `RuleModifierState` onto a `FilterRuleSpec`, setting the
/// anchor, sender/receiver side, perishable, xattr-only, and negate attributes.
pub(super) fn apply_rule_modifiers(
    mut rule: FilterRuleSpec,
    modifiers: RuleModifierState,
) -> Result<FilterRuleSpec, Message> {
    if modifiers.anchor_root {
        rule = rule.with_anchor();
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

    // upstream: exclude.c:1438 - `case 'x': rule->rflags |= FILTRULE_XATTR;`.
    // The modifier sets one bit and touches nothing else; in particular it does
    // not bind a side. `+`/`-` already default to both sides, so forcing them
    // here was a no-op there while silently erasing the one-sided semantics of
    // the H/S/P/R prefixes (hide/show/protect/risk), which exist precisely to
    // express a side (exclude.c:1345-1358).
    if modifiers.xattr_only {
        rule = rule.with_xattr_only(true);
    }

    Ok(rule)
}

#[cfg(test)]
mod tests {

    use filters::RuleSource;

    fn arg(text: &str) -> super::RuleLine<'_> {
        super::RuleLine::new(text, RuleSource::Argument)
    }
    use super::*;

    #[test]
    fn rule_modifier_state_default() {
        let state = RuleModifierState::default();
        assert!(!state.anchor_root);
        assert!(state.sender.is_none());
        assert!(state.receiver.is_none());
        assert!(!state.perishable);
        assert!(!state.xattr_only);
    }

    #[test]
    fn parse_rule_modifiers_empty_string() {
        let result = parse_rule_modifiers("", arg("+"), true, true, false).expect("parse");
        assert!(!result.anchor_root);
        assert!(result.sender.is_none());
        assert!(result.receiver.is_none());
    }

    #[test]
    fn parse_rule_modifiers_anchor_root() {
        let result = parse_rule_modifiers("/", arg("+"), true, true, false).expect("parse");
        assert!(result.anchor_root);
    }

    #[test]
    fn parse_rule_modifiers_sender_only() {
        let result = parse_rule_modifiers("s", arg("+"), true, true, false).expect("parse");
        assert_eq!(result.sender, Some(true));
        assert_eq!(result.receiver, Some(false));
    }

    #[test]
    fn parse_rule_modifiers_receiver_only() {
        let result = parse_rule_modifiers("r", arg("+"), true, true, false).expect("parse");
        assert_eq!(result.receiver, Some(true));
        assert_eq!(result.sender, Some(false));
    }

    #[test]
    fn parse_rule_modifiers_sender_and_receiver() {
        let result = parse_rule_modifiers("sr", arg("+"), true, true, false).expect("parse");
        assert_eq!(result.sender, Some(true));
        assert_eq!(result.receiver, Some(true));
    }

    #[test]
    fn parse_rule_modifiers_perishable_when_allowed() {
        let result = parse_rule_modifiers("p", arg("+"), true, true, false).expect("parse");
        assert!(result.perishable);
    }

    #[test]
    fn parse_rule_modifiers_perishable_when_disallowed() {
        let result = parse_rule_modifiers("p", arg("+"), false, true, false);
        assert!(result.is_err());
    }

    #[test]
    fn parse_rule_modifiers_xattr_when_allowed() {
        let result = parse_rule_modifiers("x", arg("+"), true, true, false).expect("parse");
        assert!(result.xattr_only);
    }

    #[test]
    fn parse_rule_modifiers_xattr_when_disallowed() {
        let result = parse_rule_modifiers("x", arg("+"), true, false, false);
        assert!(result.is_err());
    }

    #[test]
    fn parse_rule_modifiers_negate() {
        let result = parse_rule_modifiers("!", arg("-"), true, true, false).expect("parse");
        assert!(result.negate);
    }

    #[test]
    fn parse_rule_modifiers_negate_with_others() {
        let result = parse_rule_modifiers("!s", arg("-"), true, true, false).expect("parse");
        assert!(result.negate);
        assert_eq!(result.sender, Some(true));
        assert_eq!(result.receiver, Some(false));
    }

    #[test]
    fn parse_rule_modifiers_unknown_modifier() {
        let result = parse_rule_modifiers("z", arg("+"), true, true, false);
        assert!(result.is_err());
    }

    #[test]
    fn parse_rule_modifiers_case_sensitive_rejects_upper() {
        // upstream: exclude.c:1214-1287 - modifiers are matched by literal byte,
        // so the uppercase `S`/`R` reach the `default:` arm and are rejected as
        // invalid modifiers (RERR_SYNTAX). Their lowercase forms remain valid.
        assert!(parse_rule_modifiers("S", arg("+"), true, true, false).is_err());
        assert!(parse_rule_modifiers("R", arg("+"), true, true, false).is_err());
        assert!(parse_rule_modifiers("X", arg("+"), true, true, false).is_err());
    }

    #[test]
    fn parse_rule_modifiers_side_prefix_rejects_s_and_r() {
        // upstream: exclude.c:1269-1277 - when the prefix already fixes the side
        // (hide/show/protect/risk), the `s`/`r` modifiers are invalid.
        assert!(parse_rule_modifiers("s", arg("protect"), false, false, true).is_err());
        assert!(parse_rule_modifiers("r", arg("show"), false, false, true).is_err());
    }

    #[test]
    fn parse_rule_modifiers_side_prefix_allows_perishable() {
        // upstream: exclude.c:1265-1267 - `p` (perishable) is valid on every
        // rule kind, including the side-bound hide/show/protect/risk rules.
        let result = parse_rule_modifiers("p", arg("protect"), true, false, true).expect("parse");
        assert!(result.perishable);
    }

    #[test]
    fn parse_rule_modifiers_complex_combination() {
        let result = parse_rule_modifiers("/srp", arg("+"), true, true, false).expect("parse");
        assert!(result.anchor_root);
        assert_eq!(result.sender, Some(true));
        assert_eq!(result.receiver, Some(true));
        assert!(result.perishable);
    }

    #[test]
    fn apply_rule_modifiers_anchor() {
        let rule = FilterRuleSpec::include("*.rs".to_owned());
        let modifiers = RuleModifierState {
            anchor_root: true,
            sender: None,
            receiver: None,
            perishable: false,
            xattr_only: false,
            negate: false,
        };
        let result = apply_rule_modifiers(rule, modifiers).expect("apply");
        assert!(result.pattern().starts_with('/'));
    }

    #[test]
    fn apply_rule_modifiers_sender() {
        let rule = FilterRuleSpec::include("*.rs".to_owned());
        let modifiers = RuleModifierState {
            anchor_root: false,
            sender: Some(true),
            receiver: None,
            perishable: false,
            xattr_only: false,
            negate: false,
        };
        let result = apply_rule_modifiers(rule, modifiers).expect("apply");
        assert!(result.applies_to_sender());
    }

    #[test]
    fn apply_rule_modifiers_receiver() {
        let rule = FilterRuleSpec::include("*.rs".to_owned());
        let modifiers = RuleModifierState {
            anchor_root: false,
            sender: None,
            receiver: Some(true),
            perishable: false,
            xattr_only: false,
            negate: false,
        };
        let result = apply_rule_modifiers(rule, modifiers).expect("apply");
        assert!(result.applies_to_receiver());
    }

    #[test]
    fn apply_rule_modifiers_perishable() {
        let rule = FilterRuleSpec::include("*.rs".to_owned());
        let modifiers = RuleModifierState {
            anchor_root: false,
            sender: None,
            receiver: None,
            perishable: true,
            xattr_only: false,
            negate: false,
        };
        let result = apply_rule_modifiers(rule, modifiers).expect("apply");
        assert!(result.is_perishable());
    }

    #[test]
    fn apply_rule_modifiers_xattr_only_for_include() {
        let rule = FilterRuleSpec::include("*.rs".to_owned());
        let modifiers = RuleModifierState {
            anchor_root: false,
            sender: None,
            receiver: None,
            perishable: false,
            xattr_only: true,
            negate: false,
        };
        let result = apply_rule_modifiers(rule, modifiers).expect("apply");
        assert!(result.is_xattr_only());
        assert!(result.applies_to_sender());
        assert!(result.applies_to_receiver());
    }

    #[test]
    fn apply_rule_modifiers_xattr_only_for_exclude() {
        let rule = FilterRuleSpec::exclude("*.rs".to_owned());
        let modifiers = RuleModifierState {
            anchor_root: false,
            sender: None,
            receiver: None,
            perishable: false,
            xattr_only: true,
            negate: false,
        };
        let result = apply_rule_modifiers(rule, modifiers).expect("apply");
        assert!(result.is_xattr_only());
    }

    #[test]
    fn xattr_modifier_keeps_a_protect_rule_receiver_only() {
        // upstream: exclude.c:1355-1357 - `P` sets FILTRULE_RECEIVER_SIDE, and
        // :1438 `case 'x'` sets only FILTRULE_XATTR. The two are independent, so
        // `Px` stays receiver-only. Forcing both sides here would erase exactly
        // the distinction the `P` prefix exists to express, and upstream
        // ACCEPTS `Px` (measured against rsync 3.5.0: exit 0).
        let rule = FilterRuleSpec::protect("*.rs".to_owned());
        let modifiers = RuleModifierState {
            anchor_root: false,
            sender: None,
            receiver: None,
            perishable: false,
            xattr_only: true,
            negate: false,
        };
        let result = apply_rule_modifiers(rule, modifiers).expect("upstream accepts `Px`");
        assert!(
            result.is_xattr_only(),
            "the x modifier must set the xattr bit"
        );
        assert!(
            !result.applies_to_sender() && result.applies_to_receiver(),
            "P binds receiver-only; the x modifier must not widen it"
        );
    }

    #[test]
    fn apply_rule_modifiers_negate() {
        let rule = FilterRuleSpec::exclude("*.rs".to_owned());
        let modifiers = RuleModifierState {
            anchor_root: false,
            sender: None,
            receiver: None,
            perishable: false,
            xattr_only: false,
            negate: true,
        };
        let result = apply_rule_modifiers(rule, modifiers).expect("apply");
        assert!(result.is_negated());
    }

    #[test]
    fn apply_rule_modifiers_empty_state() {
        let rule = FilterRuleSpec::include("*.rs".to_owned());
        let modifiers = RuleModifierState::default();
        let result = apply_rule_modifiers(rule.clone(), modifiers).expect("apply");
        assert_eq!(result.pattern(), rule.pattern());
    }
}
