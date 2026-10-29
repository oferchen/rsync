use super::*;
use filters::FilterRule;
use std::path::{Path, PathBuf};

#[test]
fn filter_program_reports_merge_rules() {
    let dir_merge = DirMergeRule::new(".rsync-filter", DirMergeOptions::default());
    let program = FilterProgram::new([
        FilterProgramEntry::Rule(FilterRule::include("keep/**")),
        FilterProgramEntry::DirMerge(dir_merge.clone()),
    ])
    .expect("compile filter program");

    assert!(!program.is_empty());
    assert_eq!(program.dir_merge_rules(), [dir_merge]);
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

#[cfg(all(unix, feature = "xattr"))]
#[test]
fn filter_program_xattr_rules_control_allowance() {
    let program = FilterProgram::new([
        FilterProgramEntry::Rule(FilterRule::exclude("user.skip").with_xattr_only(true)),
        FilterProgramEntry::Rule(FilterRule::include("user.keep").with_xattr_only(true)),
    ])
    .expect("compile program");

    assert!(program.has_xattr_rules());
    for side in [filters::XattrSide::Sender, filters::XattrSide::Receiver] {
        // Neither rule carries a side prefix, so both participate on either end
        // exactly as an upstream rule with `elide == 0` does (exclude.c:1010).
        assert!(!program.allows_xattr("user.skip", side));
        assert!(program.allows_xattr("user.keep", side));
        assert!(program.allows_xattr("user.other", side));
    }
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
    // Risk (more specific) precedes protect (broad): upstream check_filter()
    // returns on the first matching rule (exclude.c:1207-1210), so the override
    // sub-path is un-protected while the broad protect still guards its siblings.
    segment
        .push_rule(FilterRule::risk("protected/override/**"))
        .expect("risk rule added");
    segment
        .push_rule(FilterRule::protect("protected/**"))
        .expect("protect rule added");

    let mut allowed = FilterOutcome::default();
    segment.apply(
        Path::new("keep.txt"),
        false,
        &mut allowed,
        FilterContext::Transfer,
        None,
    );
    assert!(allowed.allows_transfer());

    let mut blocked = FilterOutcome::default();
    segment.apply(
        Path::new("note.tmp"),
        false,
        &mut blocked,
        FilterContext::Transfer,
        None,
    );
    assert!(!blocked.allows_transfer());

    let mut deletion = FilterOutcome::default();
    segment.apply(
        Path::new("protected/data.bin"),
        false,
        &mut deletion,
        FilterContext::Deletion,
        None,
    );
    assert!(!deletion.allows_deletion());

    // Each path is evaluated with a fresh outcome (program.rs builds one per
    // path); the override sub-path matches the risk rule first and stays
    // deletable, independent of the earlier protected/data.bin evaluation.
    let mut override_outcome = FilterOutcome::default();
    segment.apply(
        Path::new("protected/override/data.bin"),
        false,
        &mut override_outcome,
        FilterContext::Deletion,
        None,
    );
    assert!(override_outcome.allows_deletion());
}

#[test]
fn perishable_rules_protect_top_level_deletion_context() {
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
        None,
    );
    assert!(!transfer.allows_transfer());

    // upstream: exclude.c:1193 / delete.c:205-211 - the top-level delete scan runs
    // with `ignore_perishable` unset, so a perishable exclude protects a
    // matching candidate just like a plain exclude.
    let mut deletion = FilterOutcome::default();
    segment.apply(
        Path::new("note.tmp"),
        false,
        &mut deletion,
        FilterContext::Deletion,
        None,
    );
    assert!(!deletion.allows_deletion());
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
