use super::common::*;
use super::*;

#[test]
fn process_merge_directive_applies_parent_overrides_to_nested_merges() {
    use tempfile::tempdir;

    let temp = tempdir().expect("tempdir");
    let nested = temp.path().join("nested.rules");
    std::fs::write(&nested, b"+ file\n").expect("write nested");

    let options = DirMergeOptions::default()
        .sender_modifier()
        .inherit(false)
        .exclude_filter_file(true)
        .allow_list_clearing(false);

    let mut rules = Vec::new();
    let mut visited = HashSet::new();
    super::process_merge_directive(
        "merge nested.rules",
        &options,
        temp.path(),
        "parent.rules",
        &mut rules,
        &mut visited,
    )
    .expect("merge succeeds");

    assert!(visited.is_empty());
    let include_rule = rules
        .iter()
        .find(|rule| rule.pattern() == "file")
        .expect("include rule present");
    assert!(include_rule.applies_to_sender());
    assert!(!include_rule.applies_to_receiver());
    assert!(rules.iter().any(|rule| rule.pattern() == "nested.rules"));
}
