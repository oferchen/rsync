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
    let mut visited = Vec::new();
    apply_merge_directive(
        directive,
        temp.path(),
        &mut rules,
        &mut visited,
        filters::RuleSource::Argument,
    )
    .expect("apply merge");

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
fn apply_merge_directive_rejects_per_dir_alias() {
    use std::collections::HashSet;
    use tempfile::tempdir;

    let temp = tempdir().expect("tempdir");
    let merge_file = temp.path().join("rules.txt");
    std::fs::write(&merge_file, "per-dir .rsync-filter\n").expect("write merge rules");

    let options = DirMergeOptions::default().use_whitespace();
    let directive = MergeDirective::new(merge_file.into_os_string(), None).with_options(options);

    let mut rules = Vec::new();
    let mut visited = Vec::new();
    // "per-dir" is not an upstream directive, so a merge file that uses it must
    // fail to load rather than silently accept the oc-only alias.
    assert!(
        apply_merge_directive(
            directive,
            temp.path(),
            &mut rules,
            &mut visited,
            filters::RuleSource::Argument,
        )
        .is_err()
    );
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

/// A merge-file name with a `..` in it is collapsed lexically before the file
/// is opened, so `a/b/../rules.txt` reads `a/rules.txt` even though `a/b` does
/// not exist.
///
/// This is upstream's rule, not a convenience: `parse_merge_name` cleans the
/// name with `clean_fname(fn, CFN_COLLAPSE_DOT_DOT_DIRS)` *before* the open, so
/// the `..` never reaches the kernel and the intermediate directory need not
/// exist. Handing the raw name to the OS instead fails with ENOENT and the
/// whole run aborts, which is what rsync did until 3.5.0 fixed the off-by-one
/// that left this collapse dead for multi-component paths. Measured against a
/// real rsync 3.5.0 build: it loads the rules and exits 0.
// upstream: exclude.c parse_merge_name(); util1.c clean_fname()
#[test]
fn apply_merge_directive_collapses_dot_dot_before_opening_the_file() {
    use std::collections::HashSet;
    use tempfile::tempdir;

    let temp = tempdir().expect("tempdir");
    std::fs::create_dir(temp.path().join("a")).expect("mkdir a");
    std::fs::write(temp.path().join("a/rules.txt"), "- *.log\n").expect("write merge rules");
    // `a/b` deliberately does not exist: only the lexical collapse can find the
    // file, so this fails if the raw name is passed through to open().

    let directive = MergeDirective::new(OsString::from("a/b/../rules.txt"), None);
    let mut rules = Vec::new();
    let mut visited = Vec::new();
    apply_merge_directive(
        directive,
        temp.path(),
        &mut rules,
        &mut visited,
        filters::RuleSource::Argument,
    )
    .expect("apply merge");

    assert!(
        rules
            .iter()
            .any(|rule| { rule.kind() == FilterRuleKind::Exclude && rule.pattern() == "*.log" })
    );
}

/// The collapse must not invent a file: a `..` name whose collapsed form does
/// not exist still fails, so the rewrite cannot mask a genuinely bad rule.
// upstream: exclude.c parse_merge_name(); util1.c clean_fname()
#[test]
fn apply_merge_directive_still_fails_when_the_collapsed_name_is_absent() {
    use std::collections::HashSet;
    use tempfile::tempdir;

    let temp = tempdir().expect("tempdir");
    std::fs::create_dir(temp.path().join("a")).expect("mkdir a");

    let directive = MergeDirective::new(OsString::from("a/b/../missing.txt"), None);
    let mut rules = Vec::new();
    let mut visited = Vec::new();
    assert!(
        apply_merge_directive(
            directive,
            temp.path(),
            &mut rules,
            &mut visited,
            filters::RuleSource::Argument,
        )
        .is_err()
    );
}

/// MEASURED against rsync 3.5.0 (`filter-merge-content-echo`): an unparsable
/// line inside a merge file must be described, never echoed. The merge file is
/// named by a rule the peer can choose, so echoing any line of it turns the
/// filter parser into a read-any-line oracle for every file this process can
/// open.
///
/// upstream: `rule_text` (exclude.c:88-123) is the chokepoint, and
/// `rule_src_where` (exclude.c:71-85) renders an operator-named file as
/// `FILE line N`.
#[test]
fn an_unparsable_line_in_a_merge_file_is_described_not_echoed() {
    use tempfile::tempdir;

    const SECRET: &str = "TOP-SECRET-PASSWORD-abc123";

    let temp = tempdir().expect("tempdir");
    let merge_file = temp.path().join("rules");
    std::fs::write(&merge_file, format!("{SECRET}\n")).expect("write merge rules");

    let directive = MergeDirective::new(merge_file.into_os_string(), None);
    let mut rules = Vec::new();
    let mut visited = Vec::new();
    let error = apply_merge_directive(
        directive,
        temp.path(),
        &mut rules,
        &mut visited,
        filters::RuleSource::Argument,
    )
    .expect_err("an unparsable merged line must be refused");
    let rendered = error.to_string();

    assert!(
        !rendered.contains(SECRET),
        "the merge file's contents must not be echoed: {rendered}"
    );
    assert!(
        rendered.contains("rules line 1"),
        "the diagnostic must locate the rule instead: {rendered}"
    );
}

/// The nested case, and the reason the enclosing provenance has to travel: the
/// INNER file's own path came out of the OUTER file's contents, so upstream
/// clears `rule_src_file` and reports only where it was named
/// (`named_by_file` / `rule_src_named_at`, exclude.c:1735-1755). Reporting the
/// inner path would leak a name the peer chose.
#[test]
fn a_merge_file_named_by_another_file_reports_where_it_was_named() {
    use tempfile::tempdir;

    const SECRET: &str = "TOP-SECRET-PASSWORD-abc123";

    let temp = tempdir().expect("tempdir");
    let inner = temp.path().join("inner-bad");
    std::fs::write(&inner, format!("{SECRET}\n")).expect("write inner");
    let outer = temp.path().join("outer-known");
    std::fs::write(&outer, ". inner-bad\n").expect("write outer");

    let directive = MergeDirective::new(outer.into_os_string(), None);
    let mut rules = Vec::new();
    let mut visited = Vec::new();
    let error = apply_merge_directive(
        directive,
        temp.path(),
        &mut rules,
        &mut visited,
        filters::RuleSource::Argument,
    )
    .expect_err("an unparsable line in the nested file must be refused");
    let rendered = error.to_string();

    assert!(
        !rendered.contains(SECRET),
        "the nested file's contents must not be echoed: {rendered}"
    );
    assert!(
        !rendered.contains("inner-bad line"),
        "the nested file's own name is peer-chosen and must not locate the rule: {rendered}"
    );
    // The outer file is named by the operator, so its path IS printable; the
    // tempdir prefix makes it absolute here, hence the two-part assertion.
    assert!(
        rendered.contains("a file named at") && rendered.contains("outer-known line 1"),
        "the diagnostic must name where the nested merge was requested: {rendered}"
    );
}

/// Non-vacuity companion: the redaction is a PROVENANCE decision. An
/// argument-sourced rule is still echoed verbatim, because that text is the
/// operator's own (`rule_text` returns it unchanged, exclude.c:110-116).
/// Without this, replacing the funnel with blanket suppression would also
/// satisfy the two tests above.
#[test]
fn an_argument_sourced_rule_is_still_echoed_verbatim() {
    const SECRET: &str = "TOP-SECRET-PASSWORD-abc123";

    let error = parse_filter_directive(OsStr::new(SECRET), filters::RuleSource::Argument)
        .expect_err("an unparsable argument must still be refused");

    assert!(
        error.to_string().contains(SECRET),
        "an operator's own text stays visible: {error}"
    );
}
