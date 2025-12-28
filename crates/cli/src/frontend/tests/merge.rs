use super::common::*;
use super::*;

#[test]
fn merge_directive_options_inherit_parent_configuration() {
    let base = DirMergeOptions::default()
        .inherit(false)
        .exclude_filter_file(true)
        .allow_list_clearing(false)
        .anchor_root(true)
        .allow_comments(false)
        .with_side_overrides(Some(true), Some(false));

    let directive = MergeDirective::new(OsString::from("nested.rules"), None);
    let merged = super::merge_directive_options(&base, &directive);

    assert!(!merged.inherit_rules());
    assert!(merged.excludes_self());
    assert!(!merged.list_clear_allowed());
    assert!(merged.anchor_root_enabled());
    assert!(!merged.allows_comments());
    assert_eq!(merged.sender_side_override(), Some(true));
    assert_eq!(merged.receiver_side_override(), Some(false));
}

#[test]
fn apply_merge_directive_parses_whitespace_risk_and_exclude_if_present() {
    use std::collections::HashSet;
    use tempfile::tempdir;

    let temp = tempdir().expect("tempdir");
    let merge_file = temp.path().join("rules.txt");
    std::fs::write(
        &merge_file,
        "risk logs/** exclude-if-present marker exclude-if-present=.skip\n",
    )
    .expect("write merge rules");

    let options = DirMergeOptions::default().use_whitespace();
    let directive = MergeDirective::new(merge_file.into_os_string(), None).with_options(options);

    let mut rules = Vec::new();
    let mut visited = HashSet::new();
    apply_merge_directive(directive, temp.path(), &mut rules, &mut visited).expect("apply merge");

    assert!(visited.is_empty());
    assert!(
        rules
            .iter()
            .any(|rule| { rule.kind() == FilterRuleKind::Risk && rule.pattern() == "logs/**" })
    );
    assert!(rules.iter().any(|rule| {
        rule.kind() == FilterRuleKind::ExcludeIfPresent && rule.pattern() == "marker"
    }));
    assert!(rules.iter().any(|rule| {
        rule.kind() == FilterRuleKind::ExcludeIfPresent && rule.pattern() == ".skip"
    }));
}

#[test]
fn apply_merge_directive_parses_whitespace_per_dir_alias() {
    use std::collections::HashSet;
    use tempfile::tempdir;

    let temp = tempdir().expect("tempdir");
    let merge_file = temp.path().join("rules.txt");
    std::fs::write(&merge_file, "per-dir .rsync-filter\n").expect("write merge rules");

    let options = DirMergeOptions::default().use_whitespace();
    let directive = MergeDirective::new(merge_file.into_os_string(), None).with_options(options);

    let mut rules = Vec::new();
    let mut visited = HashSet::new();
    apply_merge_directive(directive, temp.path(), &mut rules, &mut visited).expect("apply merge");

    assert!(visited.is_empty());
    let dir_merge_rule = rules
        .iter()
        .find(|rule| rule.kind() == FilterRuleKind::DirMerge)
        .expect("dir-merge rule present");
    assert_eq!(dir_merge_rule.pattern(), ".rsync-filter");
}

#[test]
fn merge_directive_options_respect_child_overrides() {
    let base = DirMergeOptions::default()
        .inherit(false)
        .with_side_overrides(Some(true), Some(false));

    let child_options = DirMergeOptions::default()
        .inherit(true)
        .allow_list_clearing(true)
        .with_enforced_kind(Some(DirMergeEnforcedKind::Include))
        .use_whitespace()
        .with_side_overrides(Some(false), Some(true));
    let directive =
        MergeDirective::new(OsString::from("nested.rules"), None).with_options(child_options);

    let merged = super::merge_directive_options(&base, &directive);

    assert_eq!(merged.enforced_kind(), Some(DirMergeEnforcedKind::Include));
    assert!(merged.uses_whitespace());
    assert_eq!(merged.sender_side_override(), Some(false));
    assert_eq!(merged.receiver_side_override(), Some(true));
}
