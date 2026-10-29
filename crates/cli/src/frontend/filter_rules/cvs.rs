use core::client::{DirMergeEnforcedKind, DirMergeOptions, FilterRuleSpec};
use filters::{CvsIgnorePattern, CvsTemplate};
use protocol::SUPPORTED_PROTOCOL_BOUNDS;

/// Appends the full `-C`/`--cvs-exclude` rule set to `destination`: the
/// per-directory `.cvsignore` dir-merge, then the global CVS ignore list. Each
/// rule is tagged with a CVS origin so the wire projection can honor upstream's
/// local-only / protocol-gated forwarding.
///
/// upstream: exclude.c:1951-1954 send_filter_list() parses `:C` and then `-C`
/// after every other rule, and `-C` goes through rule_template(0), so it is a
/// plain `C` rule like any other (exclude.c:1595-1597).
pub(crate) fn append_cvs_exclude_rules(destination: &mut Vec<FilterRuleSpec>) {
    let options = DirMergeOptions::default()
        .with_enforced_kind(Some(DirMergeEnforcedKind::Exclude))
        .use_whitespace()
        .allow_comments(false)
        .inherit(false)
        .allow_list_clearing(true);
    let mut cvs_rules = vec![FilterRuleSpec::dir_merge(".cvsignore".to_owned(), options)];
    cvs_rules.extend(cvs_default_exclude_rules(CvsTemplate::default()));

    // upstream: exclude.c:1944-1969 send_filter_list() - the `-C` built-in
    // excludes and the `:C` per-directory merge stay local on a receiving client
    // and (for `:C`) only cross the wire on protocol >= 29. Tag every rule this
    // path produces so the wire projection can honor that gating; the `-C` flag
    // is independently forwarded to the peer in argv (remote::flags), which runs
    // its own get_cvs_excludes(), so transmitting these rules on a pull would be
    // both redundant and non-upstream.
    destination.extend(cvs_rules.into_iter().map(|rule| rule.with_cvs_origin(true)));
}

/// Builds the global CVS ignore list a `C` rule with `template` stands for, under
/// the process's established template (see `CvsTemplate::established`): the built-in
/// patterns, `$HOME/.cvsignore`, and `$CVSIGNORE`, in that order. Unlike
/// [`append_cvs_exclude_rules`], this does NOT add the per-directory
/// `.cvsignore` dir-merge - that layer is contributed separately by a `:C` rule.
///
/// upstream: exclude.c:1489-1507 get_cvs_excludes(), whose list
/// `filters::cvs_ignore_list` owns. The built-in patterns are perishable at
/// protocol >= 30 (exclude.c:1498-1500); the rules are assembled at
/// argument-parse time, before negotiation, so the gate is keyed on the newest
/// protocol oc supports (SUPPORTED_PROTOCOL_BOUNDS.1, currently 33).
pub(crate) fn cvs_default_exclude_rules(template: CvsTemplate) -> Vec<FilterRuleSpec> {
    let template = template.established();
    let builtin_perishable = SUPPORTED_PROTOCOL_BOUNDS.1 >= 30;
    filters::cvs_ignore_list()
        .iter()
        .map(|entry| cvs_rule(template, entry, builtin_perishable))
        .collect()
}

/// The rule for one pattern of the CVS ignore list under `template`.
fn cvs_rule(
    template: CvsTemplate,
    entry: &CvsIgnorePattern,
    builtin_perishable: bool,
) -> FilterRuleSpec {
    let rule = if template.include {
        FilterRuleSpec::include(entry.pattern.clone())
    } else {
        FilterRuleSpec::exclude(entry.pattern.clone())
    };
    let rule = rule
        .with_perishable(template.perishable_for(entry, builtin_perishable))
        .with_negate(template.negate);
    match template.sides() {
        Some((sender, receiver)) => rule.with_sender(sender).with_receiver(receiver),
        None => rule,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::client::FilterRuleKind;

    #[test]
    fn append_cvs_exclude_rules_marks_every_rule_cvs_origin() {
        // upstream: exclude.c:1944-1969 send_filter_list() - the `-C` built-in
        // excludes and the `:C` per-directory merge follow the CVS send gate
        // (local on a receiving client, `:C` only on protocol >= 29). Every rule
        // this path produces must carry the origin marker so build_wire_format_rules
        // can forward it to the wire projection.
        let mut rules = Vec::new();
        append_cvs_exclude_rules(&mut rules);
        assert!(!rules.is_empty());
        assert!(
            rules.iter().all(FilterRuleSpec::is_cvs_origin),
            "every -C rule must be tagged cvs_origin"
        );
        // upstream: exclude.c:1951-1954 parses `:C` before `-C`, so the
        // per-directory merge leads the list.
        assert_eq!(rules[0].kind(), FilterRuleKind::DirMerge);
        assert_eq!(rules[0].pattern(), ".cvsignore");
    }

    #[test]
    fn cvs_default_exclude_rules_are_not_cvs_origin() {
        // The `--filter` CVS rule path (FilterDirective::CvsDefaults) uses
        // cvs_default_exclude_rules() directly and does NOT forward `-C` in argv,
        // so upstream transmits those rules on the wire; they must not be tagged
        // cvs_origin (which would wrongly keep them local on a pull).
        let rules = cvs_default_exclude_rules(CvsTemplate::default());
        assert!(!rules.is_empty());
        assert!(rules.iter().all(|r| !r.is_cvs_origin()));
    }

    /// upstream: exclude.c:1263, 1447-1456 - every CVS pattern inherits the `C`
    /// rule's include, negate and perishable flags and its side. MEASURED
    /// against rsync 3.5.1: `--filter=+C --filter='- *.o'` keeps `a.o`.
    #[test]
    fn cvs_template_reaches_every_rule() {
        let template = CvsTemplate {
            include: true,
            negate: true,
            perishable: true,
            sender_side: true,
            receiver_side: false,
        };
        let rules = cvs_default_exclude_rules(template);
        assert!(!rules.is_empty());
        for rule in &rules {
            assert_eq!(rule.kind(), FilterRuleKind::Include);
            assert!(rule.is_negated() && rule.is_perishable());
            assert!(rule.applies_to_sender() && !rule.applies_to_receiver());
        }
    }
}
