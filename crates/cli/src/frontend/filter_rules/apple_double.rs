//! AppleDouble (`._foo`) sidecar exclusion rules for `--apple-double-skip`.
//!
//! Mirrors the structure of [`super::cvs`] but with a single canonical
//! pattern (`._*`). The rule is marked perishable so that explicit include
//! rules supplied earlier in the filter chain win under first-match-wins
//! evaluation, matching the precedence semantics of `--cvs-exclude`.
//!
//! See [`crate::frontend::defaults::APPLE_DOUBLE_EXCLUDE_PATTERNS`] for the
//! authoritative pattern list.

use core::client::FilterRuleSpec;
use core::message::Message;

use crate::frontend::defaults::APPLE_DOUBLE_EXCLUDE_PATTERNS;

/// Appends AppleDouble sidecar exclusion rules to the destination chain.
///
/// The function is infallible today but returns [`Result`] to keep its
/// signature aligned with [`super::cvs::append_cvs_exclude_rules`] in case
/// future patterns require I/O.
pub(crate) fn append_apple_double_exclude_rules(
    destination: &mut Vec<FilterRuleSpec>,
) -> Result<(), Message> {
    for pattern in APPLE_DOUBLE_EXCLUDE_PATTERNS {
        destination.push(FilterRuleSpec::exclude((*pattern).to_owned()).with_perishable(true));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::client::FilterRuleKind;

    #[test]
    fn append_apple_double_adds_dot_underscore_pattern() {
        let mut rules = Vec::new();
        append_apple_double_exclude_rules(&mut rules).unwrap();
        assert!(!rules.is_empty());
        assert!(
            rules
                .iter()
                .any(|rule| rule.kind() == FilterRuleKind::Exclude && rule.pattern() == "._*")
        );
    }

    #[test]
    fn append_apple_double_marks_rules_perishable() {
        let mut rules = Vec::new();
        append_apple_double_exclude_rules(&mut rules).unwrap();
        for rule in &rules {
            assert!(
                rule.is_perishable(),
                "rule {} not perishable",
                rule.pattern()
            );
        }
    }

    #[test]
    fn append_apple_double_appends_without_clearing_existing() {
        let mut rules = vec![FilterRuleSpec::include("keep.txt".to_owned())];
        append_apple_double_exclude_rules(&mut rules).unwrap();
        assert!(rules.len() >= 2);
        assert_eq!(rules[0].kind(), FilterRuleKind::Include);
        assert_eq!(rules[0].pattern(), "keep.txt");
    }
}
