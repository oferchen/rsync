use super::CompiledRule;

/// Removes rules whose cleared side flags leave them inactive.
///
/// Called when processing a `!` (clear) rule. Each remaining rule has its
/// applicability flags cleared for the specified sides, and rules that no
/// longer apply to any side are removed from the list.
pub(crate) fn apply_clear_rule(rules: &mut Vec<CompiledRule>, sender: bool, receiver: bool) {
    if !sender && !receiver {
        return;
    }

    rules.retain_mut(|rule| rule.clear_sides(sender, receiver));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FilterAction, FilterRule};

    #[test]
    fn compiled_rule_clear_sides_sender() {
        let rule = FilterRule {
            action: FilterAction::Exclude,
            pattern: "*.tmp".to_owned(),
            applies_to_sender: true,
            applies_to_receiver: true,
            perishable: false,
            xattr_only: false,
            negate: false,
            exclude_only: false,
            no_inherit: false,
        };
        let mut compiled = CompiledRule::new(rule).unwrap();
        let still_active = compiled.clear_sides(true, false);
        assert!(still_active);
        assert!(!compiled.applies_to_sender);
        assert!(compiled.applies_to_receiver);
    }

    #[test]
    fn compiled_rule_clear_sides_receiver() {
        let rule = FilterRule {
            action: FilterAction::Exclude,
            pattern: "*.tmp".to_owned(),
            applies_to_sender: true,
            applies_to_receiver: true,
            perishable: false,
            xattr_only: false,
            negate: false,
            exclude_only: false,
            no_inherit: false,
        };
        let mut compiled = CompiledRule::new(rule).unwrap();
        let still_active = compiled.clear_sides(false, true);
        assert!(still_active);
        assert!(compiled.applies_to_sender);
        assert!(!compiled.applies_to_receiver);
    }

    #[test]
    fn compiled_rule_clear_sides_both() {
        let rule = FilterRule {
            action: FilterAction::Exclude,
            pattern: "*.tmp".to_owned(),
            applies_to_sender: true,
            applies_to_receiver: true,
            perishable: false,
            xattr_only: false,
            negate: false,
            exclude_only: false,
            no_inherit: false,
        };
        let mut compiled = CompiledRule::new(rule).unwrap();
        let still_active = compiled.clear_sides(true, true);
        assert!(!still_active);
        assert!(!compiled.applies_to_sender);
        assert!(!compiled.applies_to_receiver);
    }

    #[test]
    fn apply_clear_rule_empty() {
        let mut rules: Vec<CompiledRule> = vec![];
        apply_clear_rule(&mut rules, true, true);
        assert!(rules.is_empty());
    }

    #[test]
    fn apply_clear_rule_no_change() {
        let rule = FilterRule {
            action: FilterAction::Exclude,
            pattern: "*.tmp".to_owned(),
            applies_to_sender: true,
            applies_to_receiver: true,
            perishable: false,
            xattr_only: false,
            negate: false,
            exclude_only: false,
            no_inherit: false,
        };
        let mut rules = vec![CompiledRule::new(rule).unwrap()];
        apply_clear_rule(&mut rules, false, false);
        assert_eq!(rules.len(), 1);
    }

    #[test]
    fn apply_clear_rule_removes_inactive() {
        let rule = FilterRule {
            action: FilterAction::Exclude,
            pattern: "*.tmp".to_owned(),
            applies_to_sender: true,
            applies_to_receiver: false,
            perishable: false,
            xattr_only: false,
            negate: false,
            exclude_only: false,
            no_inherit: false,
        };
        let mut rules = vec![CompiledRule::new(rule).unwrap()];
        apply_clear_rule(&mut rules, true, false);
        // Rule should be removed since sender is now cleared and receiver was already false
        assert!(rules.is_empty());
    }
}
