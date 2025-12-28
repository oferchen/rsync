use core::client::{FilterRuleKind, FilterRuleSpec};
use core::message::{Message, Role};
use core::rsync_error;

#[derive(Default)]
pub(super) struct RuleModifierState {
    anchor_root: bool,
    sender: Option<bool>,
    receiver: Option<bool>,
    perishable: bool,
    xattr_only: bool,
}

pub(super) fn parse_rule_modifiers(
    modifiers: &str,
    directive: &str,
    allow_perishable: bool,
    allow_xattr: bool,
) -> Result<RuleModifierState, Message> {
    let mut state = RuleModifierState::default();

    for modifier in modifiers.chars() {
        let lower = modifier.to_ascii_lowercase();
        match lower {
            '/' => {
                state.anchor_root = true;
            }
            's' => {
                state.sender = Some(true);
                if state.receiver.is_none() {
                    state.receiver = Some(false);
                }
            }
            'r' => {
                state.receiver = Some(true);
                if state.sender.is_none() {
                    state.sender = Some(false);
                }
            }
            'p' => {
                if allow_perishable {
                    state.perishable = true;
                } else {
                    let message = rsync_error!(
                        1,
                        format!(
                            "filter rule '{directive}' uses unsupported modifier '{}'",
                            modifier
                        )
                    )
                    .with_role(Role::Client);
                    return Err(message);
                }
            }
            'x' => {
                if allow_xattr {
                    state.xattr_only = true;
                } else {
                    let message = rsync_error!(
                        1,
                        format!(
                            "filter rule '{directive}' uses unsupported modifier '{}'",
                            modifier
                        )
                    )
                    .with_role(Role::Client);
                    return Err(message);
                }
            }
            _ => {
                let message = rsync_error!(
                    1,
                    format!(
                        "filter rule '{directive}' uses unsupported modifier '{}'",
                        modifier
                    )
                )
                .with_role(Role::Client);
                return Err(message);
            }
        }
    }

    Ok(state)
}

pub(super) fn apply_rule_modifiers(
    mut rule: FilterRuleSpec,
    modifiers: RuleModifierState,
    directive: &str,
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

    if modifiers.xattr_only {
        if !matches!(
            rule.kind(),
            FilterRuleKind::Include | FilterRuleKind::Exclude
        ) {
            let message = rsync_error!(
                1,
                format!(
                    "filter rule '{directive}' cannot combine 'x' modifiers with this directive"
                )
            )
            .with_role(Role::Client);
            return Err(message);
        }
        rule = rule
            .with_xattr_only(true)
            .with_sender(true)
            .with_receiver(true);
    }

    Ok(rule)
}

#[cfg(test)]
mod tests {
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
        let result = parse_rule_modifiers("", "+", true, true).expect("parse");
        assert!(!result.anchor_root);
        assert!(result.sender.is_none());
        assert!(result.receiver.is_none());
    }

    #[test]
    fn parse_rule_modifiers_anchor_root() {
        let result = parse_rule_modifiers("/", "+", true, true).expect("parse");
        assert!(result.anchor_root);
    }

    #[test]
    fn parse_rule_modifiers_sender_only() {
        let result = parse_rule_modifiers("s", "+", true, true).expect("parse");
        assert_eq!(result.sender, Some(true));
        assert_eq!(result.receiver, Some(false));
    }

    #[test]
    fn parse_rule_modifiers_receiver_only() {
        let result = parse_rule_modifiers("r", "+", true, true).expect("parse");
        assert_eq!(result.receiver, Some(true));
        assert_eq!(result.sender, Some(false));
    }

    #[test]
    fn parse_rule_modifiers_sender_and_receiver() {
        let result = parse_rule_modifiers("sr", "+", true, true).expect("parse");
        assert_eq!(result.sender, Some(true));
        assert_eq!(result.receiver, Some(true));
    }

    #[test]
    fn parse_rule_modifiers_perishable_when_allowed() {
        let result = parse_rule_modifiers("p", "+", true, true).expect("parse");
        assert!(result.perishable);
    }

    #[test]
    fn parse_rule_modifiers_perishable_when_disallowed() {
        let result = parse_rule_modifiers("p", "+", false, true);
        assert!(result.is_err());
    }

    #[test]
    fn parse_rule_modifiers_xattr_when_allowed() {
        let result = parse_rule_modifiers("x", "+", true, true).expect("parse");
        assert!(result.xattr_only);
    }

    #[test]
    fn parse_rule_modifiers_xattr_when_disallowed() {
        let result = parse_rule_modifiers("x", "+", true, false);
        assert!(result.is_err());
    }

    #[test]
    fn parse_rule_modifiers_unknown_modifier() {
        let result = parse_rule_modifiers("z", "+", true, true);
        assert!(result.is_err());
    }

    #[test]
    fn parse_rule_modifiers_case_insensitive() {
        let result = parse_rule_modifiers("SR", "+", true, true).expect("parse");
        assert_eq!(result.sender, Some(true));
        assert_eq!(result.receiver, Some(true));
    }

    #[test]
    fn parse_rule_modifiers_complex_combination() {
        let result = parse_rule_modifiers("/srp", "+", true, true).expect("parse");
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
        };
        let result = apply_rule_modifiers(rule, modifiers, "+").expect("apply");
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
        };
        let result = apply_rule_modifiers(rule, modifiers, "+").expect("apply");
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
        };
        let result = apply_rule_modifiers(rule, modifiers, "+").expect("apply");
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
        };
        let result = apply_rule_modifiers(rule, modifiers, "+").expect("apply");
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
        };
        let result = apply_rule_modifiers(rule, modifiers, "+").expect("apply");
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
        };
        let result = apply_rule_modifiers(rule, modifiers, "-").expect("apply");
        assert!(result.is_xattr_only());
    }

    #[test]
    fn apply_rule_modifiers_xattr_only_for_non_include_exclude_fails() {
        let rule = FilterRuleSpec::protect("*.rs".to_owned());
        let modifiers = RuleModifierState {
            anchor_root: false,
            sender: None,
            receiver: None,
            perishable: false,
            xattr_only: true,
        };
        let result = apply_rule_modifiers(rule, modifiers, "P");
        assert!(result.is_err());
    }

    #[test]
    fn apply_rule_modifiers_empty_state() {
        let rule = FilterRuleSpec::include("*.rs".to_owned());
        let modifiers = RuleModifierState::default();
        let result = apply_rule_modifiers(rule.clone(), modifiers, "+").expect("apply");
        assert_eq!(result.pattern(), rule.pattern());
    }
}
