//! Message matching strategies using the Strategy pattern.
//!
//! This module provides flexible message matching for interop validation:
//! - `ExactMatcher`: Matches exact text (current behavior)
//! - `PatternMatcher`: Matches using regex patterns
//! - `GroupMatcher`: Requires at least N messages from a group to match (Composite pattern)

use regex::Regex;
use std::sync::OnceLock;

/// Result of a match operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MatchResult {
    /// Message matched successfully
    Matched,
    /// Message did not match
    NotMatched,
    /// Match is optional - not required but acceptable if present
    Optional,
}

/// Strategy trait for message matching (Strategy pattern).
pub trait MessageMatcher: std::fmt::Debug + Send + Sync {
    /// Check if the given message text matches this matcher.
    fn matches(&self, text: &str, role: Option<&str>) -> MatchResult;

    /// Get a description of what this matcher expects.
    fn description(&self) -> String;

    /// Whether this matcher is optional (not required to match).
    fn is_optional(&self) -> bool {
        false
    }

    /// Get the scenario this matcher belongs to.
    #[allow(dead_code)] // Part of trait API, may be used for debugging
    fn scenario(&self) -> &str;
}

/// Exact text matcher - matches message text exactly.
#[derive(Debug, Clone)]
pub struct ExactMatcher {
    pub text: String,
    pub role: Option<String>,
    pub scenario: String,
    pub optional: bool,
}

impl MessageMatcher for ExactMatcher {
    fn matches(&self, text: &str, role: Option<&str>) -> MatchResult {
        let text_matches = self.text == text;
        let role_matches = match (&self.role, role) {
            (Some(expected), Some(actual)) => expected == actual,
            (Some(_), None) => false,
            (None, _) => true, // No role requirement
        };

        if text_matches && role_matches {
            if self.optional {
                MatchResult::Optional
            } else {
                MatchResult::Matched
            }
        } else {
            MatchResult::NotMatched
        }
    }

    fn description(&self) -> String {
        self.text.clone()
    }

    fn is_optional(&self) -> bool {
        self.optional
    }

    fn scenario(&self) -> &str {
        &self.scenario
    }
}

/// Pattern matcher - matches message text using regex.
#[derive(Debug)]
pub struct PatternMatcher {
    pub pattern: String,
    pub compiled: OnceLock<Regex>,
    pub role: Option<String>,
    pub scenario: String,
    pub optional: bool,
}

impl PatternMatcher {
    pub fn new(pattern: String, role: Option<String>, scenario: String, optional: bool) -> Self {
        Self {
            pattern,
            compiled: OnceLock::new(),
            role,
            scenario,
            optional,
        }
    }

    fn regex(&self) -> &Regex {
        self.compiled.get_or_init(|| {
            Regex::new(&self.pattern)
                .unwrap_or_else(|e| panic!("Invalid regex pattern '{}': {}", self.pattern, e))
        })
    }
}

impl MessageMatcher for PatternMatcher {
    fn matches(&self, text: &str, role: Option<&str>) -> MatchResult {
        let text_matches = self.regex().is_match(text);
        let role_matches = match (&self.role, role) {
            (Some(expected), Some(actual)) => expected == actual,
            (Some(_), None) => false,
            (None, _) => true,
        };

        if text_matches && role_matches {
            if self.optional {
                MatchResult::Optional
            } else {
                MatchResult::Matched
            }
        } else {
            MatchResult::NotMatched
        }
    }

    fn description(&self) -> String {
        format!("pattern: {}", self.pattern)
    }

    fn is_optional(&self) -> bool {
        self.optional
    }

    fn scenario(&self) -> &str {
        &self.scenario
    }
}

/// Group matcher - requires at least N messages from a group to match (Composite pattern).
///
/// This is useful for race-condition scenarios where different messages may appear
/// depending on timing, but at least one should always be present.
#[derive(Debug)]
pub struct GroupMatcher {
    pub name: String,
    pub matchers: Vec<Box<dyn MessageMatcher>>,
    pub require_at_least: usize,
    pub scenario: String,
}

impl GroupMatcher {
    pub fn new(
        name: String,
        matchers: Vec<Box<dyn MessageMatcher>>,
        require_at_least: usize,
        scenario: String,
    ) -> Self {
        Self {
            name,
            matchers,
            require_at_least,
            scenario,
        }
    }

    /// Check how many messages from the group matched in the actual messages.
    pub fn count_matches(&self, actual_messages: &[(String, Option<String>)]) -> usize {
        let mut matched_count = 0;
        for (text, role) in actual_messages {
            for matcher in &self.matchers {
                if matcher.matches(text, role.as_deref()) != MatchResult::NotMatched {
                    matched_count += 1;
                    break; // Count each actual message only once
                }
            }
        }
        matched_count
    }

    /// Check if the group requirement is satisfied.
    pub fn is_satisfied(&self, actual_messages: &[(String, Option<String>)]) -> bool {
        self.count_matches(actual_messages) >= self.require_at_least
    }
}

impl MessageMatcher for GroupMatcher {
    fn matches(&self, text: &str, role: Option<&str>) -> MatchResult {
        // A group matcher matches if any of its sub-matchers match
        for matcher in &self.matchers {
            let result = matcher.matches(text, role);
            if result != MatchResult::NotMatched {
                return MatchResult::Optional; // Group members are effectively optional individually
            }
        }
        MatchResult::NotMatched
    }

    fn description(&self) -> String {
        format!(
            "group '{}' (require {} of {})",
            self.name,
            self.require_at_least,
            self.matchers.len()
        )
    }

    fn is_optional(&self) -> bool {
        self.require_at_least == 0
    }

    fn scenario(&self) -> &str {
        &self.scenario
    }
}

/// Validation result for a scenario.
#[derive(Debug, Default)]
pub struct ValidationResult {
    pub unexpected: Vec<String>,
    pub missing: Vec<String>,
    pub unsatisfied_groups: Vec<String>,
}

impl ValidationResult {
    #[cfg(test)]
    pub fn is_valid(&self) -> bool {
        self.unexpected.is_empty() && self.missing.is_empty() && self.unsatisfied_groups.is_empty()
    }

    pub fn differences(&self) -> Vec<String> {
        let mut diffs = Vec::new();
        for msg in &self.unexpected {
            diffs.push(format!("Unexpected message: {}", msg));
        }
        for msg in &self.missing {
            diffs.push(format!("Missing message: {}", msg));
        }
        for group in &self.unsatisfied_groups {
            diffs.push(format!("Unsatisfied group: {}", group));
        }
        diffs
    }
}

/// Validate actual messages against expected matchers.
pub fn validate_messages(
    actual: &[(String, Option<String>)],
    matchers: &[Box<dyn MessageMatcher>],
    groups: &[GroupMatcher],
) -> ValidationResult {
    let mut result = ValidationResult::default();
    let mut matched_actual = vec![false; actual.len()];

    // Check each actual message against matchers
    for (i, (text, role)) in actual.iter().enumerate() {
        let mut found_match = false;
        for matcher in matchers {
            let match_result = matcher.matches(text, role.as_deref());
            if match_result != MatchResult::NotMatched {
                found_match = true;
                matched_actual[i] = true;
                break;
            }
        }

        // Also check against group matchers
        if !found_match {
            for group in groups {
                let match_result = group.matches(text, role.as_deref());
                if match_result != MatchResult::NotMatched {
                    found_match = true;
                    matched_actual[i] = true;
                    break;
                }
            }
        }

        if !found_match {
            result.unexpected.push(text.clone());
        }
    }

    // Check for missing required messages
    for matcher in matchers {
        if matcher.is_optional() {
            continue;
        }

        let mut found = false;
        for (text, role) in actual {
            if matcher.matches(text, role.as_deref()) != MatchResult::NotMatched {
                found = true;
                break;
            }
        }

        if !found {
            result.missing.push(matcher.description());
        }
    }

    // Check group requirements
    for group in groups {
        if !group.is_satisfied(actual) {
            result.unsatisfied_groups.push(format!(
                "{} (matched {}/{} required)",
                group.description(),
                group.count_matches(actual),
                group.require_at_least
            ));
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_exact_matcher() {
        let matcher = ExactMatcher {
            text: "rsync error: test".to_owned(),
            role: Some("sender".to_owned()),
            scenario: "test".to_owned(),
            optional: false,
        };

        assert_eq!(
            matcher.matches("rsync error: test", Some("sender")),
            MatchResult::Matched
        );
        assert_eq!(
            matcher.matches("rsync error: test", Some("receiver")),
            MatchResult::NotMatched
        );
        assert_eq!(
            matcher.matches("rsync error: other", Some("sender")),
            MatchResult::NotMatched
        );
    }

    #[test]
    fn test_exact_matcher_optional() {
        let matcher = ExactMatcher {
            text: "rsync error: test".to_owned(),
            role: None,
            scenario: "test".to_owned(),
            optional: true,
        };

        assert_eq!(
            matcher.matches("rsync error: test", None),
            MatchResult::Optional
        );
    }

    #[test]
    fn test_pattern_matcher() {
        let matcher = PatternMatcher::new(
            r"rsync error: error in IPC code \(code 14\) at io\.c\(\d+\) \[sender\]".to_owned(),
            Some("sender".to_owned()),
            "test".to_owned(),
            false,
        );

        assert_eq!(
            matcher.matches(
                "rsync error: error in IPC code (code 14) at io.c(605) [sender]",
                Some("sender")
            ),
            MatchResult::Matched
        );
        assert_eq!(
            matcher.matches(
                "rsync error: error in IPC code (code 14) at io.c(1532) [sender]",
                Some("sender")
            ),
            MatchResult::Matched
        );
        assert_eq!(
            matcher.matches(
                "rsync error: error in IPC code (code 14) at pipe.c(85) [sender]",
                Some("sender")
            ),
            MatchResult::NotMatched
        );
    }

    #[test]
    fn test_group_matcher() {
        let matchers: Vec<Box<dyn MessageMatcher>> = vec![
            Box::new(PatternMatcher::new(
                r"rsync: connection unexpectedly closed.*".to_owned(),
                None,
                "test".to_owned(),
                false,
            )),
            Box::new(PatternMatcher::new(
                r"rsync: .*write.* failed to write.*Broken pipe.*".to_owned(),
                None,
                "test".to_owned(),
                false,
            )),
        ];

        let group = GroupMatcher::new(
            "ipc_errors".to_owned(),
            matchers,
            1, // At least one must match
            "test".to_owned(),
        );

        // Test with connection closed message
        let actual1 = vec![(
            "rsync: connection unexpectedly closed (0 bytes received so far) [sender]".to_owned(),
            Some("sender".to_owned()),
        )];
        assert!(group.is_satisfied(&actual1));

        // Test with write failed message
        let actual2 = vec![(
            "rsync: writefd_unbuffered failed to write 4 bytes to socket [sender]: Broken pipe (32)"
                .to_owned(),
            Some("sender".to_owned()),
        )];
        assert!(group.is_satisfied(&actual2));

        // Test with neither
        let actual3 = vec![("rsync: some other error".to_owned(), None)];
        assert!(!group.is_satisfied(&actual3));

        // Test with both
        let actual4 = vec![
            (
                "rsync: connection unexpectedly closed (0 bytes received so far) [sender]"
                    .to_owned(),
                Some("sender".to_owned()),
            ),
            (
                "rsync: writefd_unbuffered failed to write 4 bytes to socket [sender]: Broken pipe (32)"
                    .to_owned(),
                Some("sender".to_owned()),
            ),
        ];
        assert!(group.is_satisfied(&actual4));
        assert_eq!(group.count_matches(&actual4), 2);
    }

    #[test]
    fn test_validate_messages() {
        let matchers: Vec<Box<dyn MessageMatcher>> = vec![
            Box::new(ExactMatcher {
                text: "required message".to_owned(),
                role: None,
                scenario: "test".to_owned(),
                optional: false,
            }),
            Box::new(ExactMatcher {
                text: "optional message".to_owned(),
                role: None,
                scenario: "test".to_owned(),
                optional: true,
            }),
        ];

        // Test with required message present
        let actual1 = vec![("required message".to_owned(), None)];
        let result1 = validate_messages(&actual1, &matchers, &[]);
        assert!(result1.is_valid());

        // Test with required message missing
        let actual2 = vec![("other message".to_owned(), None)];
        let result2 = validate_messages(&actual2, &matchers, &[]);
        assert!(!result2.is_valid());
        assert!(!result2.missing.is_empty());
        assert!(!result2.unexpected.is_empty());
    }
}
