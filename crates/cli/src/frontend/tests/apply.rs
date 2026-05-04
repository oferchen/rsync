use super::common::*;
use super::*;

#[test]
fn apply_merge_directive_resolves_relative_paths() {
    use tempfile::tempdir;

    let temp = tempdir().expect("tempdir");
    let outer = temp.path().join("outer.rules");
    let subdir = temp.path().join("nested");
    std::fs::create_dir(&subdir).expect("create nested dir");
    let child = subdir.join("child.rules");
    let grand = subdir.join("grand.rules");

    std::fs::write(&outer, b"+ outer\nmerge nested/child.rules\n").expect("write outer");
    std::fs::write(&child, b"+ child\nmerge grand.rules\n").expect("write child");
    std::fs::write(&grand, b"+ grand\n").expect("write grand");

    let mut rules = Vec::new();
    let mut visited = HashSet::new();
    let directive = MergeDirective::new(OsString::from("outer.rules"), None)
        .with_options(DirMergeOptions::default().allow_list_clearing(true));
    super::apply_merge_directive(directive, temp.path(), &mut rules, &mut visited)
        .expect("merge succeeds");

    assert!(visited.is_empty());
    let patterns: Vec<_> = rules.iter().map(|rule| rule.pattern().to_owned()).collect();
    assert_eq!(patterns, vec!["outer", "child", "grand"]);
}

#[test]
fn apply_merge_directive_respects_forced_include() {
    use tempfile::tempdir;

    let temp = tempdir().expect("tempdir");
    let path = temp.path().join("filters.rules");
    std::fs::write(&path, b"alpha\n!\nbeta\n").expect("write filters");

    let mut rules = vec![FilterRuleSpec::exclude("existing".to_owned())];
    let mut visited = HashSet::new();
    let directive = MergeDirective::new(path.into_os_string(), Some(FilterRuleKind::Include))
        .with_options(
            DirMergeOptions::default()
                .with_enforced_kind(Some(DirMergeEnforcedKind::Include))
                .allow_list_clearing(true),
        );
    super::apply_merge_directive(directive, temp.path(), &mut rules, &mut visited)
        .expect("merge succeeds");

    assert!(visited.is_empty());
    let patterns: Vec<_> = rules.iter().map(|rule| rule.pattern().to_owned()).collect();
    assert_eq!(patterns, vec!["beta"]);
}
