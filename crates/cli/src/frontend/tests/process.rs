use super::common::*;
use super::*;

/// #224: `.e` adds `- rules` once, ahead of the rules the file holds
/// (exclude.c:1558-1572), so the file's own `+ rules` cannot re-include it.
/// oc used to append the exclude after them, by the merge rule's full path.
#[test]
fn process_merge_directive_excludes_the_merge_file_before_its_rules() {
    let temp = test_support::create_tempdir();
    std::fs::create_dir(temp.path().join("sub")).expect("mkdir");
    std::fs::write(temp.path().join("sub/rules"), b"+ rules\n").expect("write rules");

    let mut rules = Vec::new();
    super::process_merge_directive(
        ".e sub/rules",
        &DirMergeOptions::default(),
        temp.path(),
        filters::RuleSource::Argument,
        &mut rules,
        &mut Vec::new(),
        &filters::CvsSession::default(),
    )
    .expect("merge succeeds");

    let summary: Vec<_> = rules
        .iter()
        .map(|rule| (rule.kind(), rule.pattern().to_owned()))
        .collect();
    assert_eq!(
        summary,
        [
            (FilterRuleKind::Exclude, "rules".to_owned()),
            (FilterRuleKind::Include, "rules".to_owned()),
        ]
    );
}

#[test]
fn process_merge_directive_applies_parent_overrides_to_nested_merges() {
    let temp = test_support::create_tempdir();
    let nested = temp.path().join("nested.rules");
    std::fs::write(&nested, b"+ file\n").expect("write nested");

    let options = DirMergeOptions::default()
        .sender_modifier()
        .inherit(false)
        .exclude_filter_file(true)
        .allow_list_clearing(false);

    let mut rules = Vec::new();
    let mut visited = Vec::new();
    super::process_merge_directive(
        "merge nested.rules",
        &options,
        temp.path(),
        filters::RuleSource::File {
            name: "parent.rules",
            line: 1,
        },
        &mut rules,
        &mut visited,
        &filters::CvsSession::default(),
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
