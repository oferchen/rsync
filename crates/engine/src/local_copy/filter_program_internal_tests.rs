use super::*;
use rsync_filters::FilterRule;
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::tempdir;

#[test]
fn filter_program_reports_merge_and_marker_rules() {
    let dir_merge = DirMergeRule::new(".rsync-filter", DirMergeOptions::default());
    let program = FilterProgram::new([
        FilterProgramEntry::Rule(FilterRule::include("keep/**")),
        FilterProgramEntry::DirMerge(dir_merge.clone()),
        FilterProgramEntry::ExcludeIfPresent(ExcludeIfPresentRule::new(".rsyncignore")),
    ])
    .expect("compile filter program");

    assert!(!program.is_empty());
    assert_eq!(program.dir_merge_rules(), [dir_merge]);

    let temp = tempdir().expect("tempdir");
    let root = temp.path();
    assert!(
        !program
            .should_exclude_directory(root)
            .expect("marker check succeeds")
    );

    let marker_path = root.join(".rsyncignore");
    fs::write(&marker_path, b"ignored").expect("write marker");
    assert!(
        program
            .should_exclude_directory(root)
            .expect("marker detected")
    );
}

#[test]
fn filter_program_is_empty_after_clear_entries() {
    let program = FilterProgram::new([
        FilterProgramEntry::Rule(FilterRule::exclude("*.tmp")),
        FilterProgramEntry::Clear,
    ])
    .expect("compile program");
    assert!(program.is_empty());
}

#[test]
fn filter_program_xattr_rules_control_allowance() {
    let program = FilterProgram::new([
        FilterProgramEntry::Rule(FilterRule::exclude("user.skip").with_xattr_only(true)),
        FilterProgramEntry::Rule(FilterRule::include("user.keep").with_xattr_only(true)),
    ])
    .expect("compile program");

    assert!(program.has_xattr_rules());
    assert!(!program.allows_xattr("user.skip"));
    assert!(program.allows_xattr("user.keep"));
    assert!(program.allows_xattr("user.other"));
}

#[test]
fn filter_segment_apply_updates_transfer_and_deletion_outcomes() {
    let mut segment = FilterSegment::default();
    segment
        .push_rule(FilterRule::exclude("*.tmp"))
        .expect("exclude rule added");
    segment
        .push_rule(FilterRule::include("keep.txt"))
        .expect("include rule added");
    segment
        .push_rule(FilterRule::protect("protected/**"))
        .expect("protect rule added");
    segment
        .push_rule(FilterRule::risk("protected/override/**"))
        .expect("risk rule added");

    let mut allowed = FilterOutcome::default();
    segment.apply(
        Path::new("keep.txt"),
        false,
        &mut allowed,
        FilterContext::Transfer,
    );
    assert!(allowed.allows_transfer());

    let mut blocked = FilterOutcome::default();
    segment.apply(
        Path::new("note.tmp"),
        false,
        &mut blocked,
        FilterContext::Transfer,
    );
    assert!(!blocked.allows_transfer());

    let mut deletion = FilterOutcome::default();
    segment.apply(
        Path::new("protected/data.bin"),
        false,
        &mut deletion,
        FilterContext::Deletion,
    );
    assert!(!deletion.allows_deletion());

    segment.apply(
        Path::new("protected/override/data.bin"),
        false,
        &mut deletion,
        FilterContext::Deletion,
    );
    assert!(deletion.allows_deletion());
}

#[test]
fn perishable_rules_are_ignored_for_deletion_context() {
    let mut segment = FilterSegment::default();
    segment
        .push_rule(FilterRule::exclude("*.tmp").with_perishable(true))
        .expect("perishable rule added");

    let mut transfer = FilterOutcome::default();
    segment.apply(
        Path::new("note.tmp"),
        false,
        &mut transfer,
        FilterContext::Transfer,
    );
    assert!(!transfer.allows_transfer());

    let mut deletion = FilterOutcome::default();
    segment.apply(
        Path::new("note.tmp"),
        false,
        &mut deletion,
        FilterContext::Deletion,
    );
    assert!(deletion.allows_deletion());
}

#[test]
fn filter_program_evaluate_applies_dir_merge_layers() {
    let program = FilterProgram::new([
        FilterProgramEntry::Rule(FilterRule::exclude("*.tmp")),
        FilterProgramEntry::DirMerge(DirMergeRule::new(
            PathBuf::from(".rsync-filter"),
            DirMergeOptions::default(),
        )),
    ])
    .expect("compile program");

    let mut merge_segment = FilterSegment::default();
    merge_segment
        .push_rule(FilterRule::include("dir/allowed.txt"))
        .expect("add merge rule");
    let dir_layers = vec![vec![merge_segment.clone()]];

    let mut ephemeral_segment = FilterSegment::default();
    ephemeral_segment
        .push_rule(FilterRule::include("dir/ephemeral.txt"))
        .expect("add ephemeral rule");
    let ephemeral_layers = vec![(0usize, ephemeral_segment)];

    let blocked = program.evaluate(
        Path::new("dir/blocked.tmp"),
        false,
        &dir_layers,
        Some(&ephemeral_layers),
        FilterContext::Transfer,
    );
    assert!(!blocked.allows_transfer());

    let allowed_from_merge = program.evaluate(
        Path::new("dir/allowed.txt"),
        false,
        &dir_layers,
        Some(&ephemeral_layers),
        FilterContext::Transfer,
    );
    assert!(allowed_from_merge.allows_transfer());

    let allowed_from_ephemeral = program.evaluate(
        Path::new("dir/ephemeral.txt"),
        false,
        &dir_layers,
        Some(&ephemeral_layers),
        FilterContext::Transfer,
    );
    assert!(allowed_from_ephemeral.allows_transfer());
}

#[test]
fn filter_program_should_exclude_directory_reports_io_errors() {
    let temp = tempdir().expect("tempdir");
    let long_name = "x".repeat(5000);
    let program = FilterProgram::new([FilterProgramEntry::ExcludeIfPresent(
        ExcludeIfPresentRule::new(&long_name),
    )])
    .expect("compile program");

    let error = program
        .should_exclude_directory(temp.path())
        .expect_err("long path should trigger error");
    if let LocalCopyErrorKind::Io { action, path, .. } = error.kind() {
        assert_eq!(*action, "inspect exclude-if-present marker");
        assert!(path.ends_with(&long_name));
    } else {
        panic!("unexpected error: {error:?}");
    }
}
