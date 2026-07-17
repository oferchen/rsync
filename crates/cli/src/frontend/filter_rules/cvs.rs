use std::env;
use std::fs;
use std::io::ErrorKind;
use std::path::Path;

use core::client::{DirMergeEnforcedKind, DirMergeOptions, FilterRuleSpec};
use core::message::{Message, Role};
use core::rsync_error;
use protocol::SUPPORTED_PROTOCOL_BOUNDS;

use crate::frontend::defaults::CVS_EXCLUDE_PATTERNS;

pub(crate) fn append_cvs_exclude_rules(
    destination: &mut Vec<FilterRuleSpec>,
) -> Result<(), Message> {
    let mut cvs_rules = cvs_default_exclude_rules()?;

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

/// Builds the global CVS default exclude rules: the built-in pattern list,
/// `$HOME/.cvsignore`, and the `$CVSIGNORE` environment variable, in that
/// order. Unlike [`append_cvs_exclude_rules`], this does NOT add the
/// per-directory `.cvsignore` dir-merge - that layer is contributed
/// separately by a `:C` filter rule.
///
/// upstream: exclude.c:1340 get_cvs_excludes() - the `-C` convenience filter
/// rule (FILTRULE_CVS_IGNORE without a merge flag) populates the global cvs
/// list from these three sources only; the per-directory merge is the
/// separate `:C` rule.
pub(crate) fn cvs_default_exclude_rules() -> Result<Vec<FilterRuleSpec>, Message> {
    // upstream: exclude.c:1350 get_cvs_excludes() - the built-in default_cvsignore()
    // list is parsed with the perishable template only when `protocol_version >= 30`.
    // The `-C` rules are assembled at argument-parse time (before negotiation), so
    // key the gate on the newest protocol oc negotiates with a modern peer
    // (SUPPORTED_PROTOCOL_BOUNDS.1 == 32), which is always >= 30.
    let perishable = SUPPORTED_PROTOCOL_BOUNDS.1 >= 30;
    let mut cvs_rules: Vec<FilterRuleSpec> = CVS_EXCLUDE_PATTERNS
        .iter()
        .map(|pattern| FilterRuleSpec::exclude((*pattern).to_owned()).with_perishable(perishable))
        .collect();

    // upstream: exclude.c:1340-1358 get_cvs_excludes() parses default_cvsignore(),
    // $HOME/.cvsignore, and $CVSIGNORE into ONE shared cvs_filter_list. A bare
    // '!' token keeps FILTRULE_CLEAR_LIST and pop_filter_list (exclude.c:1399)
    // wipes the WHOLE list, including the built-in defaults collected before it.
    // Parse both sources into the shared accumulator (not per-source buffers) so
    // clearing semantics match upstream.
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

    Ok(cvs_rules)
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

        // upstream: exclude.c:1122-1124 parse_rule_tok tentatively sets
        // FILTRULE_CLEAR_LIST for a leading '!' in a NO_PREFIXES CVS list, but
        // exclude.c:1322-1323 clears it again once len > 1. So a bare "!"
        // (len == 1) keeps CLEAR_LIST and pop_filter_list (exclude.c:1399) wipes
        // the whole shared cvs list, including the built-in defaults, whereas
        // "!foo" (len > 1) is a LITERAL exclude of a file named "!foo" - the '!'
        // stays in the pattern because NO_PREFIXES never advances past it.
        if trimmed == "!" {
            destination.clear();
            continue;
        }

        // The `$HOME/.cvsignore` and `$CVSIGNORE` sources are parsed with a plain
        // rule_template (exclude.c:1355-1357), NOT the perishable template; only
        // the built-in default_cvsignore() list (exclude.c:1350) is perishable,
        // so these literal excludes must not be perishable.
        destination.push(FilterRuleSpec::exclude(trimmed.to_owned()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::client::FilterRuleKind;

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
    fn append_cvsignore_tokens_bare_bang_wipes_builtin_defaults() {
        // upstream: exclude.c:1399 pop_filter_list - a bare "!" (len == 1 keeps
        // FILTRULE_CLEAR_LIST) clears the ENTIRE shared cvs_filter_list. The
        // built-in default_cvsignore() patterns collected before it MUST also be
        // wiped, so a '!' in ~/.cvsignore or $CVSIGNORE removes the defaults too.
        let mut rules = vec![
            FilterRuleSpec::exclude("*.o".to_owned()),
            FilterRuleSpec::exclude("core".to_owned()),
        ];
        append_cvsignore_tokens(&mut rules, ["!"].iter().copied());
        assert!(rules.is_empty());
    }

    #[test]
    fn append_cvsignore_tokens_literal_bang_is_exclude_not_removal() {
        // upstream: exclude.c:1322-1323 - "!foo" has len > 1, so the tentative
        // FILTRULE_CLEAR_LIST is cleared and the token becomes a LITERAL exclude
        // of a file named "!foo". It must NOT remove a prior exclude of "*.o".
        let mut rules = Vec::new();
        append_cvsignore_tokens(&mut rules, ["*.o", "!*.o"].iter().copied());
        assert_eq!(rules.len(), 2);
        assert_eq!(rules[0].pattern(), "*.o");
        assert_eq!(rules[1].kind(), FilterRuleKind::Exclude);
        assert_eq!(rules[1].pattern(), "!*.o");
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
}
