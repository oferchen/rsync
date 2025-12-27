use std::collections::HashSet;
use std::path::Path;

use globset::{GlobBuilder, GlobMatcher};
use logging::debug_log;

use crate::{FilterAction, FilterError, FilterRule};

#[derive(Debug)]
pub(crate) struct CompiledRule {
    pub(crate) action: FilterAction,
    directory_only: bool,
    direct_matchers: Vec<GlobMatcher>,
    descendant_matchers: Vec<GlobMatcher>,
    pub(crate) applies_to_sender: bool,
    pub(crate) applies_to_receiver: bool,
    pub(crate) perishable: bool,
}

impl CompiledRule {
    pub(crate) fn new(rule: FilterRule) -> Result<Self, FilterError> {
        let FilterRule {
            action,
            pattern,
            applies_to_sender,
            applies_to_receiver,
            perishable,
            xattr_only,
        } = rule;
        debug_assert!(
            !xattr_only,
            "xattr-only rules should be filtered before compilation"
        );
        let (anchored, directory_only, core_pattern) = normalise_pattern(&pattern);
        let mut direct_patterns = HashSet::new();
        direct_patterns.insert(core_pattern.clone());
        if !anchored {
            direct_patterns.insert(format!("**/{core_pattern}"));
        }

        let mut descendant_patterns = HashSet::new();
        if directory_only
            || matches!(
                action,
                FilterAction::Exclude | FilterAction::Protect | FilterAction::Risk
            )
        {
            descendant_patterns.insert(format!("{core_pattern}/**"));
            if !anchored {
                descendant_patterns.insert(format!("**/{core_pattern}/**"));
            }
        }

        let direct_matchers = compile_patterns(direct_patterns, &pattern)?;
        let descendant_matchers = compile_patterns(descendant_patterns, &pattern)?;

        Ok(Self {
            action,
            directory_only,
            direct_matchers,
            descendant_matchers,
            applies_to_sender,
            applies_to_receiver,
            perishable,
        })
    }

    pub(crate) fn matches(&self, path: &Path, is_dir: bool) -> bool {
        for matcher in &self.direct_matchers {
            if matcher.is_match(path) && (!self.directory_only || is_dir) {
                debug_log!(Filter, 2, "direct pattern matched: {:?}", path);
                return true;
            }
        }

        if !self.descendant_matchers.is_empty() {
            for matcher in &self.descendant_matchers {
                if matcher.is_match(path) {
                    debug_log!(Filter, 2, "descendant pattern matched: {:?}", path);
                    return true;
                }
            }
        }

        debug_log!(Filter, 3, "no pattern match for: {:?}", path);
        false
    }

    pub(crate) fn clear_sides(&mut self, sender: bool, receiver: bool) -> bool {
        if sender {
            self.applies_to_sender = false;
        }
        if receiver {
            self.applies_to_receiver = false;
        }
        self.applies_to_sender || self.applies_to_receiver
    }
}

pub(crate) fn apply_clear_rule(rules: &mut Vec<CompiledRule>, sender: bool, receiver: bool) {
    if !sender && !receiver {
        return;
    }

    rules.retain_mut(|rule| rule.clear_sides(sender, receiver));
}

fn compile_patterns(
    patterns: HashSet<String>,
    original: &str,
) -> Result<Vec<GlobMatcher>, FilterError> {
    let mut unique: Vec<_> = patterns.into_iter().collect();
    unique.sort();

    let mut matchers = Vec::with_capacity(unique.len());
    for pattern in unique {
        let glob = GlobBuilder::new(&pattern)
            .literal_separator(true)
            .backslash_escape(true)
            .build()
            .map_err(|error| FilterError::new(original.to_string(), error))?;
        matchers.push(glob.compile_matcher());
    }
    Ok(matchers)
}

fn normalise_pattern(pattern: &str) -> (bool, bool, String) {
    let anchored = pattern.starts_with('/');
    let directory_only = pattern.ends_with('/');
    let mut core = pattern;
    if anchored {
        core = &core[1..];
    }
    if directory_only && !core.is_empty() {
        core = &core[..core.len() - 1];
    }
    (anchored, directory_only, core.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn normalise_pattern_plain() {
        let (anchored, dir_only, core) = normalise_pattern("foo");
        assert!(!anchored);
        assert!(!dir_only);
        assert_eq!(core, "foo");
    }

    #[test]
    fn normalise_pattern_anchored() {
        let (anchored, dir_only, core) = normalise_pattern("/foo");
        assert!(anchored);
        assert!(!dir_only);
        assert_eq!(core, "foo");
    }

    #[test]
    fn normalise_pattern_directory_only() {
        let (anchored, dir_only, core) = normalise_pattern("foo/");
        assert!(!anchored);
        assert!(dir_only);
        assert_eq!(core, "foo");
    }

    #[test]
    fn normalise_pattern_anchored_directory() {
        let (anchored, dir_only, core) = normalise_pattern("/foo/");
        assert!(anchored);
        assert!(dir_only);
        assert_eq!(core, "foo");
    }

    #[test]
    fn normalise_pattern_wildcard() {
        let (anchored, dir_only, core) = normalise_pattern("*.txt");
        assert!(!anchored);
        assert!(!dir_only);
        assert_eq!(core, "*.txt");
    }

    #[test]
    fn normalise_pattern_anchored_wildcard() {
        let (anchored, dir_only, core) = normalise_pattern("/*.txt");
        assert!(anchored);
        assert!(!dir_only);
        assert_eq!(core, "*.txt");
    }

    #[test]
    fn normalise_pattern_nested_path() {
        let (anchored, dir_only, core) = normalise_pattern("src/lib/");
        assert!(!anchored);
        assert!(dir_only);
        assert_eq!(core, "src/lib");
    }

    #[test]
    fn normalise_pattern_empty_after_strip() {
        // Edge case: pattern is just "/"
        let (anchored, dir_only, core) = normalise_pattern("/");
        assert!(anchored);
        assert!(dir_only);
        // Core is empty but we don't strip further because it would be empty
        assert_eq!(core, "");
    }

    #[test]
    fn compiled_rule_new_simple_exclude() {
        let rule = FilterRule {
            action: FilterAction::Exclude,
            pattern: "*.bak".to_string(),
            applies_to_sender: true,
            applies_to_receiver: true,
            perishable: false,
            xattr_only: false,
        };
        let compiled = CompiledRule::new(rule).unwrap();
        assert_eq!(compiled.action, FilterAction::Exclude);
        assert!(compiled.applies_to_sender);
        assert!(compiled.applies_to_receiver);
        assert!(!compiled.perishable);
    }

    #[test]
    fn compiled_rule_new_include() {
        let rule = FilterRule {
            action: FilterAction::Include,
            pattern: "*.rs".to_string(),
            applies_to_sender: true,
            applies_to_receiver: true,
            perishable: false,
            xattr_only: false,
        };
        let compiled = CompiledRule::new(rule).unwrap();
        assert_eq!(compiled.action, FilterAction::Include);
    }

    #[test]
    fn compiled_rule_matches_simple() {
        let rule = FilterRule {
            action: FilterAction::Exclude,
            pattern: "*.bak".to_string(),
            applies_to_sender: true,
            applies_to_receiver: true,
            perishable: false,
            xattr_only: false,
        };
        let compiled = CompiledRule::new(rule).unwrap();
        assert!(compiled.matches(Path::new("file.bak"), false));
        assert!(compiled.matches(Path::new("dir/file.bak"), false));
        assert!(!compiled.matches(Path::new("file.txt"), false));
    }

    #[test]
    fn compiled_rule_matches_anchored() {
        let rule = FilterRule {
            action: FilterAction::Exclude,
            pattern: "/build".to_string(),
            applies_to_sender: true,
            applies_to_receiver: true,
            perishable: false,
            xattr_only: false,
        };
        let compiled = CompiledRule::new(rule).unwrap();
        assert!(compiled.matches(Path::new("build"), false));
        // Anchored patterns should not match nested paths
        assert!(!compiled.matches(Path::new("src/build"), false));
    }

    #[test]
    fn compiled_rule_matches_directory_only() {
        let rule = FilterRule {
            action: FilterAction::Exclude,
            pattern: "node_modules/".to_string(),
            applies_to_sender: true,
            applies_to_receiver: true,
            perishable: false,
            xattr_only: false,
        };
        let compiled = CompiledRule::new(rule).unwrap();
        // Directory-only patterns should match directories
        assert!(compiled.matches(Path::new("node_modules"), true));
        // Directory-only patterns should not match files
        assert!(!compiled.matches(Path::new("node_modules"), false));
    }

    #[test]
    fn compiled_rule_matches_descendant() {
        let rule = FilterRule {
            action: FilterAction::Exclude,
            pattern: "build/".to_string(),
            applies_to_sender: true,
            applies_to_receiver: true,
            perishable: false,
            xattr_only: false,
        };
        let compiled = CompiledRule::new(rule).unwrap();
        // Should match the directory itself
        assert!(compiled.matches(Path::new("build"), true));
        // Should match descendants via descendant matchers
        assert!(compiled.matches(Path::new("build/output.o"), false));
        assert!(compiled.matches(Path::new("build/subdir/file.txt"), false));
    }

    #[test]
    fn compiled_rule_clear_sides_sender() {
        let rule = FilterRule {
            action: FilterAction::Exclude,
            pattern: "*.tmp".to_string(),
            applies_to_sender: true,
            applies_to_receiver: true,
            perishable: false,
            xattr_only: false,
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
            pattern: "*.tmp".to_string(),
            applies_to_sender: true,
            applies_to_receiver: true,
            perishable: false,
            xattr_only: false,
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
            pattern: "*.tmp".to_string(),
            applies_to_sender: true,
            applies_to_receiver: true,
            perishable: false,
            xattr_only: false,
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
            pattern: "*.tmp".to_string(),
            applies_to_sender: true,
            applies_to_receiver: true,
            perishable: false,
            xattr_only: false,
        };
        let mut rules = vec![CompiledRule::new(rule).unwrap()];
        apply_clear_rule(&mut rules, false, false);
        assert_eq!(rules.len(), 1);
    }

    #[test]
    fn apply_clear_rule_removes_inactive() {
        let rule = FilterRule {
            action: FilterAction::Exclude,
            pattern: "*.tmp".to_string(),
            applies_to_sender: true,
            applies_to_receiver: false,
            perishable: false,
            xattr_only: false,
        };
        let mut rules = vec![CompiledRule::new(rule).unwrap()];
        apply_clear_rule(&mut rules, true, false);
        // Rule should be removed since sender is now cleared and receiver was already false
        assert!(rules.is_empty());
    }

    #[test]
    fn compiled_rule_protect_action() {
        let rule = FilterRule {
            action: FilterAction::Protect,
            pattern: "important.dat".to_string(),
            applies_to_sender: true,
            applies_to_receiver: true,
            perishable: false,
            xattr_only: false,
        };
        let compiled = CompiledRule::new(rule).unwrap();
        assert_eq!(compiled.action, FilterAction::Protect);
        assert!(compiled.matches(Path::new("important.dat"), false));
    }

    #[test]
    fn compiled_rule_risk_action() {
        let rule = FilterRule {
            action: FilterAction::Risk,
            pattern: "temp.dat".to_string(),
            applies_to_sender: true,
            applies_to_receiver: true,
            perishable: false,
            xattr_only: false,
        };
        let compiled = CompiledRule::new(rule).unwrap();
        assert_eq!(compiled.action, FilterAction::Risk);
    }

    #[test]
    fn compiled_rule_include_matches() {
        let rule = FilterRule {
            action: FilterAction::Include,
            pattern: "*.txt".to_string(),
            applies_to_sender: true,
            applies_to_receiver: true,
            perishable: false,
            xattr_only: false,
        };
        let compiled = CompiledRule::new(rule).unwrap();
        assert_eq!(compiled.action, FilterAction::Include);
        assert!(compiled.matches(Path::new("readme.txt"), false));
    }

    #[test]
    fn compiled_rule_perishable() {
        let rule = FilterRule {
            action: FilterAction::Exclude,
            pattern: "*.log".to_string(),
            applies_to_sender: true,
            applies_to_receiver: true,
            perishable: true,
            xattr_only: false,
        };
        let compiled = CompiledRule::new(rule).unwrap();
        assert!(compiled.perishable);
    }

    #[test]
    fn compiled_rule_complex_glob() {
        let rule = FilterRule {
            action: FilterAction::Exclude,
            pattern: "**/*.o".to_string(),
            applies_to_sender: true,
            applies_to_receiver: true,
            perishable: false,
            xattr_only: false,
        };
        let compiled = CompiledRule::new(rule).unwrap();
        assert!(compiled.matches(Path::new("build/main.o"), false));
        assert!(compiled.matches(Path::new("src/lib/util.o"), false));
    }
}
