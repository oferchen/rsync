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
