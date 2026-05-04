use std::env;
use std::fs;
use std::io::ErrorKind;
use std::path::Path;

use core::client::{DirMergeEnforcedKind, DirMergeOptions, FilterRuleKind, FilterRuleSpec};
use core::message::{Message, Role};
use core::rsync_error;

use crate::frontend::defaults::CVS_EXCLUDE_PATTERNS;

pub(crate) fn append_cvs_exclude_rules(
    destination: &mut Vec<FilterRuleSpec>,
) -> Result<(), Message> {
    let mut cvs_rules: Vec<FilterRuleSpec> = CVS_EXCLUDE_PATTERNS
        .iter()
        .map(|pattern| FilterRuleSpec::exclude((*pattern).to_owned()).with_perishable(true))
        .collect();

    if let Some(home) = env::var_os("HOME").filter(|value| !value.is_empty()) {
        let path = Path::new(&home).join(".cvsignore");
        match fs::read(&path) {
            Ok(contents) => {
                let owned = String::from_utf8_lossy(&contents).into_owned();
                append_cvsignore_tokens(&mut cvs_rules, owned.split_whitespace());
            }
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => {
                let text = format!(
                    "failed to read '{}' for --cvs-exclude: {error}",
                    path.display()
                );
                return Err(rsync_error!(1, text).with_role(Role::Client));
            }
        }
    }

    if let Some(value) = env::var_os("CVSIGNORE").filter(|value| !value.is_empty()) {
        let owned = value.to_string_lossy().into_owned();
        append_cvsignore_tokens(&mut cvs_rules, owned.split_whitespace());
    }

    let options = DirMergeOptions::default()
        .with_enforced_kind(Some(DirMergeEnforcedKind::Exclude))
        .use_whitespace()
        .allow_comments(false)
        .inherit(false)
        .allow_list_clearing(true);
    cvs_rules.push(FilterRuleSpec::dir_merge(".cvsignore".to_owned(), options));

    destination.extend(cvs_rules);
    Ok(())
}

fn append_cvsignore_tokens<'a, I>(destination: &mut Vec<FilterRuleSpec>, tokens: I)
where
    I: IntoIterator<Item = &'a str>,
{
    for token in tokens {
        let trimmed = token.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        if trimmed == "!" {
            destination.clear();
            continue;
        }

        if let Some(remainder) = trimmed.strip_prefix('!') {
            if remainder.is_empty() {
                continue;
            }
            remove_cvs_pattern(destination, remainder);
            continue;
        }

        destination.push(FilterRuleSpec::exclude(trimmed.to_owned()).with_perishable(true));
    }
}

fn remove_cvs_pattern(rules: &mut Vec<FilterRuleSpec>, pattern: &str) {
    rules.retain(|rule| {
        !(matches!(rule.kind(), FilterRuleKind::Exclude) && rule.pattern() == pattern)
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn append_cvsignore_tokens_adds_basic_pattern() {
        let mut rules = Vec::new();
        append_cvsignore_tokens(&mut rules, ["*.o"].iter().copied());
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].kind(), FilterRuleKind::Exclude);
        assert_eq!(rules[0].pattern(), "*.o");
    }

    #[test]
    fn append_cvsignore_tokens_adds_multiple_patterns() {
        let mut rules = Vec::new();
        append_cvsignore_tokens(&mut rules, ["*.o", "*.a", "*.so"].iter().copied());
        assert_eq!(rules.len(), 3);
        assert_eq!(rules[0].pattern(), "*.o");
        assert_eq!(rules[1].pattern(), "*.a");
        assert_eq!(rules[2].pattern(), "*.so");
    }

    #[test]
    fn append_cvsignore_tokens_skips_empty_tokens() {
        let mut rules = Vec::new();
        append_cvsignore_tokens(&mut rules, ["*.o", "", "*.a"].iter().copied());
        assert_eq!(rules.len(), 2);
    }

    #[test]
    fn append_cvsignore_tokens_skips_hash_comments() {
        let mut rules = Vec::new();
        append_cvsignore_tokens(&mut rules, ["*.o", "# comment", "*.a"].iter().copied());
        assert_eq!(rules.len(), 2);
    }

    #[test]
    fn append_cvsignore_tokens_handles_bang_clear() {
        let mut rules = Vec::new();
        append_cvsignore_tokens(&mut rules, ["*.o", "*.a"].iter().copied());
        assert_eq!(rules.len(), 2);
        append_cvsignore_tokens(&mut rules, ["!"].iter().copied());
        assert_eq!(rules.len(), 0);
    }

    #[test]
    fn append_cvsignore_tokens_handles_bang_removal() {
        let mut rules = Vec::new();
        append_cvsignore_tokens(&mut rules, ["*.o", "*.a", "*.so"].iter().copied());
        assert_eq!(rules.len(), 3);
        append_cvsignore_tokens(&mut rules, ["!*.a"].iter().copied());
        assert_eq!(rules.len(), 2);
        assert_eq!(rules[0].pattern(), "*.o");
        assert_eq!(rules[1].pattern(), "*.so");
    }

    #[test]
    fn append_cvsignore_tokens_ignores_empty_bang() {
        let mut rules = Vec::new();
        append_cvsignore_tokens(&mut rules, ["*.o"].iter().copied());
        append_cvsignore_tokens(&mut rules, ["!"].iter().copied());
        assert!(rules.is_empty());
    }

    #[test]
    fn append_cvsignore_tokens_trims_whitespace() {
        let mut rules = Vec::new();
        append_cvsignore_tokens(&mut rules, ["  *.o  ", "  *.a"].iter().copied());
        assert_eq!(rules.len(), 2);
        assert_eq!(rules[0].pattern(), "*.o");
        assert_eq!(rules[1].pattern(), "*.a");
    }

    #[test]
    fn remove_cvs_pattern_removes_matching_exclude() {
        let mut rules = vec![
            FilterRuleSpec::exclude("*.o".to_owned()),
            FilterRuleSpec::exclude("*.a".to_owned()),
        ];
        remove_cvs_pattern(&mut rules, "*.o");
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].pattern(), "*.a");
    }

    #[test]
    fn remove_cvs_pattern_preserves_non_matching() {
        let mut rules = vec![
            FilterRuleSpec::exclude("*.o".to_owned()),
            FilterRuleSpec::exclude("*.a".to_owned()),
        ];
        remove_cvs_pattern(&mut rules, "*.xyz");
        assert_eq!(rules.len(), 2);
    }

    #[test]
    fn remove_cvs_pattern_preserves_include_rules() {
        let mut rules = vec![
            FilterRuleSpec::exclude("*.o".to_owned()),
            FilterRuleSpec::include("*.o".to_owned()),
        ];
        remove_cvs_pattern(&mut rules, "*.o");
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].kind(), FilterRuleKind::Include);
    }

    #[test]
    fn remove_cvs_pattern_removes_all_matching() {
        let mut rules = vec![
            FilterRuleSpec::exclude("*.o".to_owned()),
            FilterRuleSpec::exclude("*.o".to_owned()),
        ];
        remove_cvs_pattern(&mut rules, "*.o");
        assert!(rules.is_empty());
    }

    #[test]
    fn remove_cvs_pattern_empty_rules() {
        let mut rules: Vec<FilterRuleSpec> = Vec::new();
        remove_cvs_pattern(&mut rules, "*.o");
        assert!(rules.is_empty());
    }
}
