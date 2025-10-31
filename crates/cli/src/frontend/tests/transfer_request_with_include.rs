use super::common::*;
use super::*;

#[test]
fn transfer_request_with_include_from_reinstate_patterns() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source_root = tmp.path().join("source");
    let dest_root = tmp.path().join("dest");
    let keep_dir = source_root.join("keep");
    std::fs::create_dir_all(&keep_dir).expect("create keep dir");
    std::fs::create_dir_all(&dest_root).expect("create dest root");
    std::fs::write(keep_dir.join("file.txt"), b"keep").expect("write keep");
    std::fs::write(source_root.join("skip.tmp"), b"skip").expect("write skip");

    let include_file = tmp.path().join("includes.txt");
    std::fs::write(&include_file, "keep/\nkeep/**\n").expect("write include file");

    let mut expected_rules = Vec::new();
    expected_rules.push(FilterRuleSpec::exclude("*".to_string()));
    append_filter_rules_from_files(
        &mut expected_rules,
        &[include_file.as_os_str().to_os_string()],
        FilterRuleKind::Include,
    )
    .expect("load include patterns");

    let engine_rules = expected_rules.iter().filter_map(|rule| match rule.kind() {
        FilterRuleKind::Include => Some(EngineFilterRule::include(rule.pattern())),
        FilterRuleKind::Exclude => Some(EngineFilterRule::exclude(rule.pattern())),
        FilterRuleKind::Clear => None,
        FilterRuleKind::Protect => Some(EngineFilterRule::protect(rule.pattern())),
        FilterRuleKind::Risk => Some(EngineFilterRule::risk(rule.pattern())),
        FilterRuleKind::ExcludeIfPresent => None,
        FilterRuleKind::DirMerge => None,
    });
    let filter_set = FilterSet::from_rules(engine_rules).expect("filters");
    assert!(filter_set.allows(std::path::Path::new("keep"), true));
    assert!(filter_set.allows(std::path::Path::new("keep/file.txt"), false));
    assert!(!filter_set.allows(std::path::Path::new("skip.tmp"), false));

    let mut source_operand = source_root.clone().into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--exclude"),
        OsString::from("*"),
        OsString::from("--include-from"),
        include_file.as_os_str().to_os_string(),
        source_operand,
        dest_root.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    assert!(dest_root.join("keep/file.txt").exists());
    assert!(!dest_root.join("skip.tmp").exists());
}
