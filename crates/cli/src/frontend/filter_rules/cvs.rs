use std::env;
use std::fs;
use std::io::ErrorKind;
use std::path::Path;

use rsync_core::client::{DirMergeEnforcedKind, DirMergeOptions, FilterRuleKind, FilterRuleSpec};
use rsync_core::message::{Message, Role};
use rsync_core::rsync_error;

use crate::frontend::defaults::CVS_EXCLUDE_PATTERNS;

pub(crate) fn append_cvs_exclude_rules(
    destination: &mut Vec<FilterRuleSpec>,
) -> Result<(), Message> {
    let mut cvs_rules: Vec<FilterRuleSpec> = CVS_EXCLUDE_PATTERNS
        .iter()
        .map(|pattern| FilterRuleSpec::exclude((*pattern).to_string()).with_perishable(true))
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
    cvs_rules.push(FilterRuleSpec::dir_merge(".cvsignore".to_string(), options));

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

        destination.push(FilterRuleSpec::exclude(trimmed.to_string()).with_perishable(true));
    }
}

fn remove_cvs_pattern(rules: &mut Vec<FilterRuleSpec>, pattern: &str) {
    rules.retain(|rule| {
        !(matches!(rule.kind(), FilterRuleKind::Exclude) && rule.pattern() == pattern)
    });
}
