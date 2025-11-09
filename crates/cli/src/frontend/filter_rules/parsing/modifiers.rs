use rsync_core::client::{FilterRuleKind, FilterRuleSpec};
use rsync_core::message::{Message, Role};
use rsync_core::rsync_error;

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
