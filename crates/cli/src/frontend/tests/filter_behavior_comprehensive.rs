use super::common::*;
use super::*;

// =============================================================================
// CLI Parsing: -f short option
// =============================================================================

#[test]
fn parse_args_recognises_short_f_with_exclude_rule() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("-f"),
        OsString::from("- *.bak"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.filters.len(), 1);
    assert_eq!(parsed.filters[0], "- *.bak");
}

#[test]
fn parse_args_recognises_short_f_with_include_rule() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("-f"),
        OsString::from("+ *.txt"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.filters.len(), 1);
    assert_eq!(parsed.filters[0], "+ *.txt");
}

#[test]
fn parse_args_recognises_multiple_short_f_options() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("-f"),
        OsString::from("+ *.txt"),
        OsString::from("-f"),
        OsString::from("- *.bak"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.filters.len(), 2);
    assert_eq!(parsed.filters[0], "+ *.txt");
    assert_eq!(parsed.filters[1], "- *.bak");
}

#[test]
fn parse_args_recognises_short_f_in_cluster() {
    // -avf should expand to -a -v -f; -f takes the next argument as the rule
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("-avf"),
        OsString::from("- *.tmp"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert!(parsed.archive);
    assert!(parsed.verbosity > 0);
    assert_eq!(parsed.filters.len(), 1);
    assert_eq!(parsed.filters[0], "- *.tmp");
}

#[test]
fn parse_args_recognises_long_filter_equals() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--filter=- *.log"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.filters.len(), 1);
    assert_eq!(parsed.filters[0], "- *.log");
}

#[test]
fn parse_args_recognises_long_filter_separate() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--filter"),
        OsString::from("+ *.rs"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.filters.len(), 1);
    assert_eq!(parsed.filters[0], "+ *.rs");
}

// =============================================================================
// CLI Parsing: Mixed -f / --filter / -F
// =============================================================================

#[test]
fn parse_args_mixes_short_f_and_long_filter() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("-f"),
        OsString::from("+ *.txt"),
        OsString::from("--filter"),
        OsString::from("- *.bak"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.filters.len(), 2);
    assert_eq!(parsed.filters[0], "+ *.txt");
    assert_eq!(parsed.filters[1], "- *.bak");
}

#[test]
fn parse_args_mixes_short_f_and_filter_equals() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("-f"),
        OsString::from("- *.tmp"),
        OsString::from("--filter=+ keep.txt"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.filters.len(), 2);
    assert_eq!(parsed.filters[0], "- *.tmp");
    assert_eq!(parsed.filters[1], "+ keep.txt");
}

// =============================================================================
// CLI Parsing: Multiple rules processed in order
// =============================================================================

#[test]
fn parse_args_preserves_filter_rule_order() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("-f"),
        OsString::from("+ *.txt"),
        OsString::from("-f"),
        OsString::from("- *.log"),
        OsString::from("--filter"),
        OsString::from("+ important/"),
        OsString::from("--filter=- *"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.filters.len(), 4);
    assert_eq!(parsed.filters[0], "+ *.txt");
    assert_eq!(parsed.filters[1], "- *.log");
    assert_eq!(parsed.filters[2], "+ important/");
    assert_eq!(parsed.filters[3], "- *");
}

// =============================================================================
// CLI Parsing: --exclude/--include are independent from --filter
// =============================================================================

#[test]
fn parse_args_collects_excludes_and_filters_independently() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--exclude"),
        OsString::from("*.bak"),
        OsString::from("-f"),
        OsString::from("+ *.txt"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.excludes.len(), 1);
    assert_eq!(parsed.excludes[0], "*.bak");
    assert_eq!(parsed.filters.len(), 1);
    assert_eq!(parsed.filters[0], "+ *.txt");
}

#[test]
fn parse_args_collects_includes_and_filters_independently() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--include"),
        OsString::from("*.txt"),
        OsString::from("-f"),
        OsString::from("- *.log"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.includes.len(), 1);
    assert_eq!(parsed.includes[0], "*.txt");
    assert_eq!(parsed.filters.len(), 1);
    assert_eq!(parsed.filters[0], "- *.log");
}

// =============================================================================
// CLI Parsing: Clear rule via -f
// =============================================================================

#[test]
fn parse_args_recognises_clear_rule_via_short_f() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("-f"),
        OsString::from("- *.bak"),
        OsString::from("-f"),
        OsString::from("!"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.filters.len(), 2);
    assert_eq!(parsed.filters[0], "- *.bak");
    assert_eq!(parsed.filters[1], "!");
}

// =============================================================================
// CLI Parsing: Merge rule via -f
// =============================================================================

#[test]
fn parse_args_recognises_merge_rule_via_short_f() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("-f"),
        OsString::from("merge /tmp/rules"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.filters.len(), 1);
    assert_eq!(parsed.filters[0], "merge /tmp/rules");
}

#[test]
fn parse_args_recognises_dot_merge_shorthand_via_short_f() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("-f"),
        OsString::from(". /tmp/rules"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.filters.len(), 1);
    assert_eq!(parsed.filters[0], ". /tmp/rules");
}

// =============================================================================
// CLI Parsing: Dir-merge via -f
// =============================================================================

#[test]
fn parse_args_recognises_dir_merge_via_short_f() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("-f"),
        OsString::from(": .rsync-filter"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.filters.len(), 1);
    assert_eq!(parsed.filters[0], ": .rsync-filter");
}

// =============================================================================
// CLI Parsing: -f with protect/risk/show/hide
// =============================================================================

#[test]
fn parse_args_recognises_protect_rule_via_short_f() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("-f"),
        OsString::from("P important.dat"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.filters.len(), 1);
    assert_eq!(parsed.filters[0], "P important.dat");
}

#[test]
fn parse_args_recognises_hide_rule_via_short_f() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("-f"),
        OsString::from("H .hidden"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.filters.len(), 1);
    assert_eq!(parsed.filters[0], "H .hidden");
}

#[test]
fn parse_args_recognises_show_rule_via_short_f() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("-f"),
        OsString::from("S visible/**"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.filters.len(), 1);
    assert_eq!(parsed.filters[0], "S visible/**");
}

#[test]
fn parse_args_recognises_risk_rule_via_short_f() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("-f"),
        OsString::from("R temp/**"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.filters.len(), 1);
    assert_eq!(parsed.filters[0], "R temp/**");
}

// =============================================================================
// Integration: -f excludes files in actual transfers
// =============================================================================

#[test]
fn transfer_with_short_f_exclude_skips_matching_files() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source_root = tmp.path().join("source");
    let dest_root = tmp.path().join("dest");
    std::fs::create_dir_all(&source_root).expect("create source root");
    std::fs::create_dir_all(&dest_root).expect("create dest root");
    std::fs::write(source_root.join("keep.txt"), b"keep").expect("write keep");
    std::fs::write(source_root.join("skip.log"), b"skip").expect("write skip");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-f"),
        OsString::from("- *.log"),
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let copied_root = dest_root.join("source");
    assert!(copied_root.join("keep.txt").exists());
    assert!(!copied_root.join("skip.log").exists());
}

#[test]
fn transfer_with_short_f_include_then_exclude_all() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source_root = tmp.path().join("source");
    let dest_root = tmp.path().join("dest");
    std::fs::create_dir_all(&source_root).expect("create source root");
    std::fs::create_dir_all(&dest_root).expect("create dest root");
    std::fs::write(source_root.join("keep.txt"), b"keep").expect("write keep");
    std::fs::write(source_root.join("skip.log"), b"skip").expect("write skip");

    // Trailing slash so the source directory itself is not evaluated against filters.
    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-f"),
        OsString::from("+ *.txt"),
        OsString::from("-f"),
        OsString::from("- *"),
        source_operand,
        dest_root.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    assert!(dest_root.join("keep.txt").exists());
    assert!(!dest_root.join("skip.log").exists());
}

#[test]
fn transfer_with_short_f_clear_resets_rules() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source_root = tmp.path().join("source");
    let dest_root = tmp.path().join("dest");
    std::fs::create_dir_all(&source_root).expect("create source root");
    std::fs::create_dir_all(&dest_root).expect("create dest root");
    std::fs::write(source_root.join("keep.txt"), b"keep").expect("write keep");
    std::fs::write(source_root.join("also_keep.log"), b"keep").expect("write log");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-f"),
        OsString::from("- *.log"),
        OsString::from("-f"),
        OsString::from("!"),
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let copied_root = dest_root.join("source");
    assert!(copied_root.join("keep.txt").exists());
    // After clear, the exclude is gone so log files are included
    assert!(copied_root.join("also_keep.log").exists());
}

#[test]
fn transfer_with_short_f_merge_applies_rules_from_file() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source_root = tmp.path().join("source");
    let dest_root = tmp.path().join("dest");
    std::fs::create_dir_all(&source_root).expect("create source root");
    std::fs::create_dir_all(&dest_root).expect("create dest root");
    std::fs::write(source_root.join("keep.txt"), b"keep").expect("write keep");
    std::fs::write(source_root.join("skip.tmp"), b"skip").expect("write skip");

    let filter_file = tmp.path().join("rules.txt");
    std::fs::write(&filter_file, "- *.tmp\n").expect("write filter file");

    let filter_arg = format!("merge {}", filter_file.display());
    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-f"),
        OsString::from(filter_arg),
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let copied_root = dest_root.join("source");
    assert!(copied_root.join("keep.txt").exists());
    assert!(!copied_root.join("skip.tmp").exists());
}

// =============================================================================
// Integration: Multiple filter rules processed in order
// =============================================================================

#[test]
fn transfer_with_multiple_filters_order_matters() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source_root = tmp.path().join("source");
    let dest_root = tmp.path().join("dest");
    std::fs::create_dir_all(&source_root).expect("create source root");
    std::fs::create_dir_all(&dest_root).expect("create dest root");
    std::fs::write(source_root.join("file.txt"), b"txt").expect("write txt");
    std::fs::write(source_root.join("file.log"), b"log").expect("write log");
    std::fs::write(source_root.join("file.bak"), b"bak").expect("write bak");

    // First-match-wins: include *.txt, exclude *.log, then everything else passes
    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--filter"),
        OsString::from("+ *.txt"),
        OsString::from("-f"),
        OsString::from("- *.log"),
        OsString::from("-f"),
        OsString::from("- *.bak"),
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let copied_root = dest_root.join("source");
    assert!(copied_root.join("file.txt").exists());
    assert!(!copied_root.join("file.log").exists());
    assert!(!copied_root.join("file.bak").exists());
}

// =============================================================================
// Integration: --filter=RULE with equals sign
// =============================================================================

#[test]
fn transfer_with_filter_equals_excludes_patterns() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source_root = tmp.path().join("source");
    let dest_root = tmp.path().join("dest");
    std::fs::create_dir_all(&source_root).expect("create source root");
    std::fs::create_dir_all(&dest_root).expect("create dest root");
    std::fs::write(source_root.join("keep.txt"), b"keep").expect("write keep");
    std::fs::write(source_root.join("skip.bak"), b"skip").expect("write skip");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--filter=- *.bak"),
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let copied_root = dest_root.join("source");
    assert!(copied_root.join("keep.txt").exists());
    assert!(!copied_root.join("skip.bak").exists());
}

// =============================================================================
// Integration: --exclude is shorthand for --filter='- PATTERN'
// =============================================================================

#[test]
fn exclude_and_filter_exclude_produce_same_result() {
    use tempfile::tempdir;

    // Run with --exclude
    let tmp1 = tempdir().expect("tempdir");
    let source_root1 = tmp1.path().join("source");
    let dest_root1 = tmp1.path().join("dest");
    std::fs::create_dir_all(&source_root1).expect("create source root");
    std::fs::create_dir_all(&dest_root1).expect("create dest root");
    std::fs::write(source_root1.join("keep.txt"), b"keep").expect("write keep");
    std::fs::write(source_root1.join("skip.tmp"), b"skip").expect("write skip");

    let (code1, _, _) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--exclude"),
        OsString::from("*.tmp"),
        source_root1.into_os_string(),
        dest_root1.clone().into_os_string(),
    ]);

    // Run with --filter='- *.tmp'
    let tmp2 = tempdir().expect("tempdir");
    let source_root2 = tmp2.path().join("source");
    let dest_root2 = tmp2.path().join("dest");
    std::fs::create_dir_all(&source_root2).expect("create source root");
    std::fs::create_dir_all(&dest_root2).expect("create dest root");
    std::fs::write(source_root2.join("keep.txt"), b"keep").expect("write keep");
    std::fs::write(source_root2.join("skip.tmp"), b"skip").expect("write skip");

    let (code2, _, _) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-f"),
        OsString::from("- *.tmp"),
        source_root2.into_os_string(),
        dest_root2.clone().into_os_string(),
    ]);

    assert_eq!(code1, 0);
    assert_eq!(code2, 0);

    let copied1 = dest_root1.join("source");
    let copied2 = dest_root2.join("source");

    assert!(copied1.join("keep.txt").exists());
    assert!(!copied1.join("skip.tmp").exists());
    assert!(copied2.join("keep.txt").exists());
    assert!(!copied2.join("skip.tmp").exists());
}

// =============================================================================
// Integration: --include is shorthand for --filter='+ PATTERN'
// =============================================================================

/// Note: --exclude and --include are processed by `apply_filters` in a fixed
/// order: include-from, includes, exclude-from, excludes, then --filter rules.
/// This ensures first-match-wins semantics work correctly when --include and
/// --exclude are used together. The --filter/-f mechanism gives direct control
/// over rule ordering.
///
/// This test verifies that -f with include then exclude-all correctly filters
/// files using first-match-wins semantics where the order is under user control.
#[test]
fn filter_include_then_exclude_all_via_short_f() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source_root = tmp.path().join("source");
    let dest_root = tmp.path().join("dest");
    std::fs::create_dir_all(&source_root).expect("create source root");
    std::fs::create_dir_all(&dest_root).expect("create dest root");
    std::fs::write(source_root.join("keep.txt"), b"keep").expect("write keep");
    std::fs::write(source_root.join("skip.log"), b"skip").expect("write skip");

    // Trailing slash so the source directory itself is not evaluated against filters.
    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());

    let (code, _, _) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-f"),
        OsString::from("+ *.txt"),
        OsString::from("-f"),
        OsString::from("- *"),
        source_operand,
        dest_root.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);

    assert!(dest_root.join("keep.txt").exists());
    assert!(!dest_root.join("skip.log").exists());
}

// =============================================================================
// Integration: --exclude-from is shorthand for --filter='. FILE'
// =============================================================================

#[test]
fn exclude_from_and_filter_merge_produce_same_result() {
    use tempfile::tempdir;

    let filter_content = "*.tmp\n";

    // Run with --exclude-from
    let tmp1 = tempdir().expect("tempdir");
    let source_root1 = tmp1.path().join("source");
    let dest_root1 = tmp1.path().join("dest");
    std::fs::create_dir_all(&source_root1).expect("create source root");
    std::fs::create_dir_all(&dest_root1).expect("create dest root");
    std::fs::write(source_root1.join("keep.txt"), b"keep").expect("write keep");
    std::fs::write(source_root1.join("skip.tmp"), b"skip").expect("write skip");

    let filter_file1 = tmp1.path().join("excludes.txt");
    std::fs::write(&filter_file1, filter_content).expect("write filter file");

    let (code1, _, _) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--exclude-from"),
        filter_file1.as_os_str().to_os_string(),
        source_root1.into_os_string(),
        dest_root1.clone().into_os_string(),
    ]);

    // Run with -f 'merge FILE' (merge files use exclude-only enforced kind for --exclude-from)
    // Note: --exclude-from uses a different mechanism internally (it reads patterns
    // and wraps them as exclude rules), while merge reads filter directives.
    // So instead of testing exact equivalence, we test the same patterns produce
    // the same outcome via -f with explicit exclude rules.
    let tmp2 = tempdir().expect("tempdir");
    let source_root2 = tmp2.path().join("source");
    let dest_root2 = tmp2.path().join("dest");
    std::fs::create_dir_all(&source_root2).expect("create source root");
    std::fs::create_dir_all(&dest_root2).expect("create dest root");
    std::fs::write(source_root2.join("keep.txt"), b"keep").expect("write keep");
    std::fs::write(source_root2.join("skip.tmp"), b"skip").expect("write skip");

    // Using merge with enforced exclude kind (merge,- FILE)
    let filter_file2 = tmp2.path().join("excludes.txt");
    std::fs::write(&filter_file2, filter_content).expect("write filter file");

    let merge_arg = format!("merge,- {}", filter_file2.display());
    let (code2, _, _) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-f"),
        OsString::from(merge_arg),
        source_root2.into_os_string(),
        dest_root2.clone().into_os_string(),
    ]);

    assert_eq!(code1, 0);
    assert_eq!(code2, 0);

    let copied1 = dest_root1.join("source");
    let copied2 = dest_root2.join("source");

    assert!(copied1.join("keep.txt").exists());
    assert!(!copied1.join("skip.tmp").exists());
    assert!(copied2.join("keep.txt").exists());
    assert!(!copied2.join("skip.tmp").exists());
}

// =============================================================================
// locate_filter_arguments: -f tracking
// =============================================================================

#[test]
fn locate_filter_arguments_finds_short_f() {
    use crate::frontend::filter_rules::locate_filter_arguments;

    let args: Vec<OsString> = vec![
        OsString::from("rsync"),
        OsString::from("-f"),
        OsString::from("- *.bak"),
    ];
    let (filter_indices, rsync_filter_indices) = locate_filter_arguments(&args);
    assert_eq!(filter_indices, vec![1]);
    assert!(rsync_filter_indices.is_empty());
}

#[test]
fn locate_filter_arguments_finds_short_f_and_long_filter() {
    use crate::frontend::filter_rules::locate_filter_arguments;

    let args: Vec<OsString> = vec![
        OsString::from("rsync"),
        OsString::from("-f"),
        OsString::from("- *.bak"),
        OsString::from("--filter"),
        OsString::from("+ *.txt"),
    ];
    let (filter_indices, _) = locate_filter_arguments(&args);
    assert_eq!(filter_indices, vec![1, 3]);
}

#[test]
fn locate_filter_arguments_short_f_stops_at_double_dash() {
    use crate::frontend::filter_rules::locate_filter_arguments;

    let args: Vec<OsString> = vec![
        OsString::from("rsync"),
        OsString::from("--"),
        OsString::from("-f"),
        OsString::from("- *.bak"),
    ];
    let (filter_indices, _) = locate_filter_arguments(&args);
    assert!(filter_indices.is_empty());
}

#[test]
fn locate_filter_arguments_short_f_skips_value() {
    use crate::frontend::filter_rules::locate_filter_arguments;

    // After -f, the next argument is the value and should be skipped
    let args: Vec<OsString> = vec![
        OsString::from("rsync"),
        OsString::from("-f"),
        OsString::from("- *.bak"),
        OsString::from("-f"),
        OsString::from("+ *.txt"),
    ];
    let (filter_indices, _) = locate_filter_arguments(&args);
    assert_eq!(filter_indices, vec![1, 3]);
}

#[test]
fn locate_filter_arguments_interleaves_short_f_and_uppercase_f() {
    use crate::frontend::filter_rules::locate_filter_arguments;

    let args: Vec<OsString> = vec![
        OsString::from("rsync"),
        OsString::from("-F"),
        OsString::from("-f"),
        OsString::from("- *.bak"),
        OsString::from("-F"),
    ];
    let (filter_indices, rsync_filter_indices) = locate_filter_arguments(&args);
    assert_eq!(filter_indices, vec![2]);
    assert_eq!(rsync_filter_indices, vec![1, 4]);
}

// =============================================================================
// Filter rule parsing: keyword forms via -f
// =============================================================================

#[test]
fn parse_filter_directive_include_keyword_via_filter_arg() {
    let result =
        parse_filter_directive(OsStr::new("include *.rs")).expect("keyword include parses");
    match result {
        FilterDirective::Rule(spec) => {
            assert_eq!(spec.kind(), FilterRuleKind::Include);
            assert_eq!(spec.pattern(), "*.rs");
        }
        _ => panic!("expected Rule directive"),
    }
}

#[test]
fn parse_filter_directive_exclude_keyword_via_filter_arg() {
    let result =
        parse_filter_directive(OsStr::new("exclude *.bak")).expect("keyword exclude parses");
    match result {
        FilterDirective::Rule(spec) => {
            assert_eq!(spec.kind(), FilterRuleKind::Exclude);
            assert_eq!(spec.pattern(), "*.bak");
        }
        _ => panic!("expected Rule directive"),
    }
}

#[test]
fn parse_filter_directive_dir_merge_long_keyword() {
    let result = parse_filter_directive(OsStr::new("dir-merge .rsync-filter"))
        .expect("dir-merge keyword parses");
    match result {
        FilterDirective::Rule(spec) => {
            assert_eq!(spec.kind(), FilterRuleKind::DirMerge);
            assert_eq!(spec.pattern(), ".rsync-filter");
        }
        _ => panic!("expected Rule directive"),
    }
}

#[test]
fn parse_filter_directive_colon_dir_merge_shorthand() {
    let result =
        parse_filter_directive(OsStr::new(": .rsync-filter")).expect("colon dir-merge parses");
    match result {
        FilterDirective::Rule(spec) => {
            assert_eq!(spec.kind(), FilterRuleKind::DirMerge);
            assert_eq!(spec.pattern(), ".rsync-filter");
        }
        _ => panic!("expected Rule directive"),
    }
}

// =============================================================================
// Integration: Keyword forms work end-to-end
// =============================================================================

#[test]
fn transfer_with_filter_keyword_exclude() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source_root = tmp.path().join("source");
    let dest_root = tmp.path().join("dest");
    std::fs::create_dir_all(&source_root).expect("create source root");
    std::fs::create_dir_all(&dest_root).expect("create dest root");
    std::fs::write(source_root.join("keep.txt"), b"keep").expect("write keep");
    std::fs::write(source_root.join("skip.bak"), b"skip").expect("write skip");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-f"),
        OsString::from("exclude *.bak"),
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let copied_root = dest_root.join("source");
    assert!(copied_root.join("keep.txt").exists());
    assert!(!copied_root.join("skip.bak").exists());
}

#[test]
fn transfer_with_filter_keyword_include_then_exclude_all() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source_root = tmp.path().join("source");
    let dest_root = tmp.path().join("dest");
    std::fs::create_dir_all(&source_root).expect("create source root");
    std::fs::create_dir_all(&dest_root).expect("create dest root");
    std::fs::write(source_root.join("keep.txt"), b"keep").expect("write keep");
    std::fs::write(source_root.join("skip.log"), b"skip").expect("write skip");

    // Trailing slash so the source directory itself is not evaluated against filters.
    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-f"),
        OsString::from("include *.txt"),
        OsString::from("-f"),
        OsString::from("exclude *"),
        source_operand,
        dest_root.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    assert!(dest_root.join("keep.txt").exists());
    assert!(!dest_root.join("skip.log").exists());
}

// =============================================================================
// Integration: Short merge shorthand (. FILE) via -f
// =============================================================================

#[test]
fn transfer_with_short_f_dot_merge_shorthand() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source_root = tmp.path().join("source");
    let dest_root = tmp.path().join("dest");
    std::fs::create_dir_all(&source_root).expect("create source root");
    std::fs::create_dir_all(&dest_root).expect("create dest root");
    std::fs::write(source_root.join("keep.txt"), b"keep").expect("write keep");
    std::fs::write(source_root.join("skip.tmp"), b"skip").expect("write skip");

    let filter_file = tmp.path().join("rules.txt");
    std::fs::write(&filter_file, "- *.tmp\n").expect("write filter file");

    let merge_arg = format!(". {}", filter_file.display());
    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-f"),
        OsString::from(merge_arg),
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let copied_root = dest_root.join("source");
    assert!(copied_root.join("keep.txt").exists());
    assert!(!copied_root.join("skip.tmp").exists());
}

// =============================================================================
// Integration: Mixed -f and --filter order is preserved
// =============================================================================

#[test]
fn transfer_with_mixed_f_and_filter_preserves_order() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source_root = tmp.path().join("source");
    let dest_root = tmp.path().join("dest");
    std::fs::create_dir_all(&source_root).expect("create source root");
    std::fs::create_dir_all(&dest_root).expect("create dest root");
    std::fs::write(source_root.join("a.txt"), b"a").expect("write a");
    std::fs::write(source_root.join("b.log"), b"b").expect("write b");
    std::fs::write(source_root.join("c.bak"), b"c").expect("write c");

    // -f include *.txt, --filter exclude *.log, -f exclude *.bak
    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-f"),
        OsString::from("+ *.txt"),
        OsString::from("--filter"),
        OsString::from("- *.log"),
        OsString::from("-f"),
        OsString::from("- *.bak"),
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let copied_root = dest_root.join("source");
    assert!(copied_root.join("a.txt").exists());
    assert!(!copied_root.join("b.log").exists());
    assert!(!copied_root.join("c.bak").exists());
}

// =============================================================================
// Integration: -f with cluster -avf
// =============================================================================

#[test]
fn transfer_with_cluster_avf_excludes_files() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source_root = tmp.path().join("source");
    let dest_root = tmp.path().join("dest");
    std::fs::create_dir_all(&source_root).expect("create source root");
    std::fs::create_dir_all(&dest_root).expect("create dest root");
    std::fs::write(source_root.join("keep.txt"), b"keep").expect("write keep");
    std::fs::write(source_root.join("skip.bak"), b"skip").expect("write skip");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-avf"),
        OsString::from("- *.bak"),
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    // -v produces output but we just check the transfer result
    let _ = stdout;
    let _ = stderr;

    let copied_root = dest_root.join("source");
    assert!(copied_root.join("keep.txt").exists());
    assert!(!copied_root.join("skip.bak").exists());
}

// =============================================================================
// Integration: Multiple wildcard patterns
// =============================================================================

#[test]
fn transfer_with_multiple_wildcard_exclude_patterns() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source_root = tmp.path().join("source");
    let dest_root = tmp.path().join("dest");
    std::fs::create_dir_all(&source_root).expect("create source root");
    std::fs::create_dir_all(&dest_root).expect("create dest root");
    std::fs::write(source_root.join("keep.txt"), b"keep").expect("write keep");
    std::fs::write(source_root.join("skip.tmp"), b"skip").expect("write tmp");
    std::fs::write(source_root.join("skip.bak"), b"skip").expect("write bak");
    std::fs::write(source_root.join("skip.log"), b"skip").expect("write log");

    let (code, _, _) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-f"),
        OsString::from("- *.tmp"),
        OsString::from("-f"),
        OsString::from("- *.bak"),
        OsString::from("-f"),
        OsString::from("- *.log"),
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);

    let copied_root = dest_root.join("source");
    assert!(copied_root.join("keep.txt").exists());
    assert!(!copied_root.join("skip.tmp").exists());
    assert!(!copied_root.join("skip.bak").exists());
    assert!(!copied_root.join("skip.log").exists());
}

// =============================================================================
// Integration: Directory-only patterns (trailing slash)
// =============================================================================

#[test]
fn transfer_with_directory_only_exclude_pattern() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source_root = tmp.path().join("source");
    let dest_root = tmp.path().join("dest");
    let subdir = source_root.join("skipdir");
    std::fs::create_dir_all(&subdir).expect("create subdir");
    std::fs::create_dir_all(&dest_root).expect("create dest root");
    std::fs::write(source_root.join("keep.txt"), b"keep").expect("write keep");
    std::fs::write(subdir.join("inside.txt"), b"inside").expect("write inside");
    // Create a file named "skipdir" (not a directory) to verify the pattern
    // only excludes the directory, not a file with the same name
    // Actually for simplicity, just verify the directory is excluded

    let (code, _, _) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-r"),
        OsString::from("-f"),
        OsString::from("- skipdir/"),
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);

    let copied_root = dest_root.join("source");
    assert!(copied_root.join("keep.txt").exists());
    assert!(!copied_root.join("skipdir").exists());
}

// =============================================================================
// Integration: Anchored patterns (leading slash)
// =============================================================================

#[test]
fn transfer_with_anchored_pattern_only_matches_root() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source_root = tmp.path().join("source");
    let dest_root = tmp.path().join("dest");
    let subdir = source_root.join("sub");
    std::fs::create_dir_all(&subdir).expect("create subdir");
    std::fs::create_dir_all(&dest_root).expect("create dest root");
    std::fs::write(source_root.join("skip.txt"), b"root skip").expect("write root");
    std::fs::write(subdir.join("skip.txt"), b"sub keep").expect("write sub");

    // Anchored pattern /skip.txt only excludes at root level
    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());

    let (code, _, _) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-r"),
        OsString::from("-f"),
        OsString::from("- /skip.txt"),
        source_operand,
        dest_root.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);

    assert!(!dest_root.join("skip.txt").exists());
    assert!(dest_root.join("sub/skip.txt").exists());
}
