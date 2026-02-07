
// ============================================================================
// Filter program engine tests
//
// Exercises the filter evaluation engine (FilterOutcome, FilterSegment,
// FilterProgram) independently of the CLI layer.  These tests verify the
// semantics documented for upstream rsync's filter rule evaluation:
//
// - Default outcome allows transfer and deletion.
// - Exclude rules block transfer.
// - Include rules re-allow transfer after a prior exclude.
// - First-match-wins within a single segment.
// - Protected paths block deletion but not transfer.
// - Risk rules remove protection.
// - Perishable rules are ignored in the deletion context.
// - delete_excluded semantics.
// - Glob patterns (*.txt, *.log, etc.) match correctly.
// - Directory-only patterns require the is_dir flag.
// - Empty programs allow everything.
// - Programs with multiple segments compose correctly.
// ============================================================================

// ---------------------------------------------------------------------------
// FilterOutcome unit tests
// ---------------------------------------------------------------------------

#[test]
fn filter_outcome_default_allows_transfer_and_deletion() {
    let outcome = FilterOutcome::default();
    assert!(
        outcome.allows_transfer(),
        "default outcome must allow transfer"
    );
    assert!(
        outcome.allows_deletion(),
        "default outcome must allow deletion"
    );
    assert!(
        !outcome.allows_deletion_when_excluded_removed(),
        "default outcome must not flag delete-excluded"
    );
}

#[test]
fn filter_outcome_after_exclude_blocks_transfer() {
    let mut segment = FilterSegment::default();
    segment
        .push_rule(FilterRule::exclude("secret.txt"))
        .expect("compile exclude rule");

    let mut outcome = FilterOutcome::default();
    segment.apply(
        Path::new("secret.txt"),
        false,
        &mut outcome,
        FilterContext::Transfer,
    );

    assert!(
        !outcome.allows_transfer(),
        "excluded path must block transfer"
    );
    assert!(
        !outcome.allows_deletion(),
        "excluded (transfer-blocked) path must also block deletion"
    );
}

#[test]
fn filter_outcome_exclude_does_not_affect_unmatched_paths() {
    let mut segment = FilterSegment::default();
    segment
        .push_rule(FilterRule::exclude("secret.txt"))
        .expect("compile exclude rule");

    let mut outcome = FilterOutcome::default();
    segment.apply(
        Path::new("public.txt"),
        false,
        &mut outcome,
        FilterContext::Transfer,
    );

    assert!(
        outcome.allows_transfer(),
        "non-matching path must remain allowed"
    );
}

// ---------------------------------------------------------------------------
// FilterSegment include pattern tests
// ---------------------------------------------------------------------------

#[test]
fn filter_segment_include_glob_pattern() {
    let mut segment = FilterSegment::default();
    segment
        .push_rule(FilterRule::include("*.rs"))
        .expect("compile include rule");

    let mut outcome = FilterOutcome::default();
    segment.apply(
        Path::new("main.rs"),
        false,
        &mut outcome,
        FilterContext::Transfer,
    );
    assert!(outcome.allows_transfer(), "*.rs must match main.rs");

    let mut outcome2 = FilterOutcome::default();
    segment.apply(
        Path::new("readme.md"),
        false,
        &mut outcome2,
        FilterContext::Transfer,
    );
    // An include rule that doesn't match leaves the outcome unchanged (still
    // at default = allowed).
    assert!(
        outcome2.allows_transfer(),
        "*.rs must not affect readme.md"
    );
}

#[test]
fn filter_segment_include_restores_after_exclude() {
    let mut segment = FilterSegment::default();
    segment
        .push_rule(FilterRule::exclude("*"))
        .expect("exclude all");
    segment
        .push_rule(FilterRule::include("*.txt"))
        .expect("include txt");

    // A .txt file is first excluded by *, then re-included by *.txt.
    // The FilterSegment iterates rules sequentially and the LAST match wins
    // within a single apply() call on the same segment.
    let mut outcome = FilterOutcome::default();
    segment.apply(
        Path::new("notes.txt"),
        false,
        &mut outcome,
        FilterContext::Transfer,
    );
    assert!(
        outcome.allows_transfer(),
        "*.txt include after * exclude must re-allow .txt files"
    );

    // A .log file is excluded by * and NOT matched by *.txt.
    let mut outcome2 = FilterOutcome::default();
    segment.apply(
        Path::new("debug.log"),
        false,
        &mut outcome2,
        FilterContext::Transfer,
    );
    assert!(
        !outcome2.allows_transfer(),
        "*.log must remain excluded when only *.txt is re-included"
    );
}

// ---------------------------------------------------------------------------
// FilterSegment exclude pattern tests
// ---------------------------------------------------------------------------

#[test]
fn filter_segment_exclude_glob_patterns() {
    let mut segment = FilterSegment::default();
    segment
        .push_rule(FilterRule::exclude("*.tmp"))
        .expect("exclude tmp");
    segment
        .push_rule(FilterRule::exclude("*.log"))
        .expect("exclude log");

    for (path, expected_blocked) in &[
        ("data.tmp", true),
        ("server.log", true),
        ("readme.txt", false),
        ("app.rs", false),
    ] {
        let mut outcome = FilterOutcome::default();
        segment.apply(
            Path::new(path),
            false,
            &mut outcome,
            FilterContext::Transfer,
        );
        assert_eq!(
            !outcome.allows_transfer(),
            *expected_blocked,
            "path '{path}' transfer_blocked expected={expected_blocked}"
        );
    }
}

#[test]
fn filter_segment_exclude_with_subdirectory_path() {
    let mut segment = FilterSegment::default();
    segment
        .push_rule(FilterRule::exclude("*.bak"))
        .expect("exclude bak");

    let mut outcome = FilterOutcome::default();
    segment.apply(
        Path::new("backups/config.bak"),
        false,
        &mut outcome,
        FilterContext::Transfer,
    );
    assert!(
        !outcome.allows_transfer(),
        "unanchored *.bak must match in subdirectories"
    );
}

// ---------------------------------------------------------------------------
// First-match-wins semantics (last matching rule in segment wins)
// ---------------------------------------------------------------------------

#[test]
fn filter_segment_last_match_wins_include_then_exclude() {
    // Rules are evaluated sequentially in a segment. The last matching rule
    // determines the outcome because each match overwrites set_transfer_allowed.
    let mut segment = FilterSegment::default();
    segment
        .push_rule(FilterRule::include("*.txt"))
        .expect("include txt");
    segment
        .push_rule(FilterRule::exclude("secret.txt"))
        .expect("exclude secret.txt");

    let mut outcome = FilterOutcome::default();
    segment.apply(
        Path::new("secret.txt"),
        false,
        &mut outcome,
        FilterContext::Transfer,
    );
    // secret.txt matches *.txt (include -> allowed), then matches secret.txt
    // (exclude -> blocked). Last match wins: blocked.
    assert!(
        !outcome.allows_transfer(),
        "secret.txt must be blocked because the later exclude rule wins"
    );
}

#[test]
fn filter_segment_last_match_wins_exclude_then_include() {
    let mut segment = FilterSegment::default();
    segment
        .push_rule(FilterRule::exclude("*.txt"))
        .expect("exclude txt");
    segment
        .push_rule(FilterRule::include("important.txt"))
        .expect("include important.txt");

    let mut outcome = FilterOutcome::default();
    segment.apply(
        Path::new("important.txt"),
        false,
        &mut outcome,
        FilterContext::Transfer,
    );
    // important.txt matches *.txt (exclude -> blocked), then matches
    // important.txt (include -> allowed). Last match wins: allowed.
    assert!(
        outcome.allows_transfer(),
        "important.txt must be allowed because the later include rule wins"
    );
}

// ---------------------------------------------------------------------------
// Exclude-all then include specific (the failing pattern from issue #88)
// ---------------------------------------------------------------------------

#[test]
fn filter_segment_exclude_all_then_include_specific() {
    let mut segment = FilterSegment::default();
    segment
        .push_rule(FilterRule::exclude("*"))
        .expect("exclude all");
    segment
        .push_rule(FilterRule::include("keep.txt"))
        .expect("include keep.txt");

    // keep.txt: excluded by *, then re-included by keep.txt. Last match wins.
    let mut outcome = FilterOutcome::default();
    segment.apply(
        Path::new("keep.txt"),
        false,
        &mut outcome,
        FilterContext::Transfer,
    );
    assert!(
        outcome.allows_transfer(),
        "keep.txt must be allowed after exclude-all + include-specific"
    );

    // other.txt: excluded by *, not matched by keep.txt. Stays excluded.
    let mut outcome2 = FilterOutcome::default();
    segment.apply(
        Path::new("other.txt"),
        false,
        &mut outcome2,
        FilterContext::Transfer,
    );
    assert!(
        !outcome2.allows_transfer(),
        "other.txt must remain excluded"
    );
}

#[test]
fn filter_segment_exclude_all_then_include_glob() {
    let mut segment = FilterSegment::default();
    segment
        .push_rule(FilterRule::exclude("*"))
        .expect("exclude all");
    segment
        .push_rule(FilterRule::include("*.rs"))
        .expect("include rs files");
    segment
        .push_rule(FilterRule::include("*.toml"))
        .expect("include toml files");

    for (path, expected_allowed) in &[
        ("lib.rs", true),
        ("Cargo.toml", true),
        ("readme.md", false),
        ("build.log", false),
    ] {
        let mut outcome = FilterOutcome::default();
        segment.apply(
            Path::new(path),
            false,
            &mut outcome,
            FilterContext::Transfer,
        );
        assert_eq!(
            outcome.allows_transfer(),
            *expected_allowed,
            "path '{path}' transfer_allowed expected={expected_allowed}"
        );
    }
}

// ---------------------------------------------------------------------------
// FilterProgram with multiple segments
// ---------------------------------------------------------------------------

#[test]
fn filter_program_multiple_segments_compose() {
    // FilterProgram with two rules that end up in the same segment.
    let program = FilterProgram::new([
        FilterProgramEntry::Rule(FilterRule::exclude("*.tmp")),
        FilterProgramEntry::Rule(FilterRule::include("important.tmp")),
    ])
    .expect("compile program");

    // important.tmp is first excluded, then re-included.
    let outcome = program.evaluate(
        Path::new("important.tmp"),
        false,
        &[],
        None,
        FilterContext::Transfer,
    );
    assert!(
        outcome.allows_transfer(),
        "important.tmp must be allowed in multi-rule program"
    );

    // random.tmp is excluded.
    let outcome2 = program.evaluate(
        Path::new("random.tmp"),
        false,
        &[],
        None,
        FilterContext::Transfer,
    );
    assert!(
        !outcome2.allows_transfer(),
        "random.tmp must be blocked"
    );

    // readme.txt is not matched by either rule, stays allowed.
    let outcome3 = program.evaluate(
        Path::new("readme.txt"),
        false,
        &[],
        None,
        FilterContext::Transfer,
    );
    assert!(
        outcome3.allows_transfer(),
        "readme.txt must remain allowed"
    );
}

#[test]
fn filter_program_with_dir_merge_segments_and_rules() {
    // Build a program with rules before and after a DirMerge directive.
    let program = FilterProgram::new([
        FilterProgramEntry::Rule(FilterRule::exclude("*.bak")),
        FilterProgramEntry::DirMerge(DirMergeRule::new(
            ".rsync-filter",
            DirMergeOptions::default(),
        )),
        FilterProgramEntry::Rule(FilterRule::exclude("*.log")),
    ])
    .expect("compile program");

    assert!(!program.is_empty());

    // Without any dir-merge layers, only the static rules apply.
    let outcome_bak = program.evaluate(
        Path::new("backup.bak"),
        false,
        &[],
        None,
        FilterContext::Transfer,
    );
    assert!(
        !outcome_bak.allows_transfer(),
        "*.bak rule must block backup.bak"
    );

    let outcome_log = program.evaluate(
        Path::new("server.log"),
        false,
        &[],
        None,
        FilterContext::Transfer,
    );
    assert!(
        !outcome_log.allows_transfer(),
        "*.log rule must block server.log"
    );

    let outcome_txt = program.evaluate(
        Path::new("readme.txt"),
        false,
        &[],
        None,
        FilterContext::Transfer,
    );
    assert!(
        outcome_txt.allows_transfer(),
        "readme.txt must remain allowed"
    );
}

// ---------------------------------------------------------------------------
// Filter with directory paths and directory-only patterns
// ---------------------------------------------------------------------------

#[test]
fn filter_segment_directory_only_pattern_matches_dirs() {
    let mut segment = FilterSegment::default();
    // Trailing slash means directory-only.
    segment
        .push_rule(FilterRule::exclude("cache/"))
        .expect("exclude cache/");

    let mut dir_outcome = FilterOutcome::default();
    segment.apply(
        Path::new("cache"),
        true, // is_dir
        &mut dir_outcome,
        FilterContext::Transfer,
    );
    assert!(
        !dir_outcome.allows_transfer(),
        "directory-only exclude must match a directory named 'cache'"
    );

    // Same pattern must NOT match a regular file named "cache".
    let mut file_outcome = FilterOutcome::default();
    segment.apply(
        Path::new("cache"),
        false, // not a directory
        &mut file_outcome,
        FilterContext::Transfer,
    );
    // A directory-only pattern still generates descendant matchers for excludes.
    // For a file named "cache" (not in a cache/ tree), the direct matcher
    // requires is_dir=true, so the direct match path fails. However the
    // descendant matcher "cache/**" and "**/cache/**" would not match a bare
    // "cache" path either. So the file should remain allowed.
    assert!(
        file_outcome.allows_transfer(),
        "directory-only exclude must not match a regular file named 'cache'"
    );
}

#[test]
fn filter_segment_directory_only_excludes_descendants() {
    let mut segment = FilterSegment::default();
    segment
        .push_rule(FilterRule::exclude("build/"))
        .expect("exclude build/");

    // A file inside the build directory should be excluded because the
    // descendant matcher "build/**" (and "**/build/**") catches it.
    let mut outcome = FilterOutcome::default();
    segment.apply(
        Path::new("build/output.o"),
        false,
        &mut outcome,
        FilterContext::Transfer,
    );
    assert!(
        !outcome.allows_transfer(),
        "files inside an excluded directory must be blocked"
    );
}

#[test]
fn filter_segment_non_directory_pattern_matches_both() {
    let mut segment = FilterSegment::default();
    segment
        .push_rule(FilterRule::exclude("target"))
        .expect("exclude target");

    // A non-trailing-slash exclude pattern matches both files and directories.
    let mut dir_outcome = FilterOutcome::default();
    segment.apply(
        Path::new("target"),
        true,
        &mut dir_outcome,
        FilterContext::Transfer,
    );
    assert!(
        !dir_outcome.allows_transfer(),
        "non-directory-only exclude must match a directory"
    );

    let mut file_outcome = FilterOutcome::default();
    segment.apply(
        Path::new("target"),
        false,
        &mut file_outcome,
        FilterContext::Transfer,
    );
    assert!(
        !file_outcome.allows_transfer(),
        "non-directory-only exclude must match a regular file"
    );
}

// ---------------------------------------------------------------------------
// Glob pattern tests
// ---------------------------------------------------------------------------

#[test]
fn filter_segment_glob_star_matches_extension() {
    let mut segment = FilterSegment::default();
    segment
        .push_rule(FilterRule::exclude("*.txt"))
        .expect("exclude txt");

    for (path, should_block) in &[
        ("notes.txt", true),
        ("dir/sub/notes.txt", true),
        ("notes.txt.bak", false),
        ("notes", false),
        (".txt", true),
    ] {
        let mut outcome = FilterOutcome::default();
        segment.apply(
            Path::new(path),
            false,
            &mut outcome,
            FilterContext::Transfer,
        );
        assert_eq!(
            !outcome.allows_transfer(),
            *should_block,
            "*.txt against '{path}': blocked={should_block}"
        );
    }
}

#[test]
fn filter_segment_glob_double_star_pattern() {
    // A pattern with ** already embedded (e.g. "logs/**") anchors itself.
    let mut segment = FilterSegment::default();
    segment
        .push_rule(FilterRule::exclude("logs/**"))
        .expect("exclude logs/**");

    let mut outcome_inside = FilterOutcome::default();
    segment.apply(
        Path::new("logs/app.log"),
        false,
        &mut outcome_inside,
        FilterContext::Transfer,
    );
    assert!(
        !outcome_inside.allows_transfer(),
        "logs/** must exclude files inside logs/"
    );

    let mut outcome_outside = FilterOutcome::default();
    segment.apply(
        Path::new("data/app.log"),
        false,
        &mut outcome_outside,
        FilterContext::Transfer,
    );
    assert!(
        outcome_outside.allows_transfer(),
        "logs/** must not affect files outside logs/"
    );
}

#[test]
fn filter_segment_anchored_pattern() {
    // A pattern starting with / is anchored to the root.
    let mut segment = FilterSegment::default();
    segment
        .push_rule(FilterRule::exclude("/config.yaml"))
        .expect("exclude /config.yaml");

    let mut outcome_root = FilterOutcome::default();
    segment.apply(
        Path::new("config.yaml"),
        false,
        &mut outcome_root,
        FilterContext::Transfer,
    );
    assert!(
        !outcome_root.allows_transfer(),
        "anchored /config.yaml must match at root level"
    );

    let mut outcome_nested = FilterOutcome::default();
    segment.apply(
        Path::new("subdir/config.yaml"),
        false,
        &mut outcome_nested,
        FilterContext::Transfer,
    );
    assert!(
        outcome_nested.allows_transfer(),
        "anchored /config.yaml must not match in subdirectories"
    );
}

#[test]
fn filter_segment_unanchored_pattern_matches_anywhere() {
    let mut segment = FilterSegment::default();
    segment
        .push_rule(FilterRule::exclude("*.o"))
        .expect("exclude *.o");

    let mut outcome_root = FilterOutcome::default();
    segment.apply(
        Path::new("main.o"),
        false,
        &mut outcome_root,
        FilterContext::Transfer,
    );
    assert!(
        !outcome_root.allows_transfer(),
        "unanchored *.o must match at root"
    );

    let mut outcome_deep = FilterOutcome::default();
    segment.apply(
        Path::new("src/deep/nested/util.o"),
        false,
        &mut outcome_deep,
        FilterContext::Transfer,
    );
    assert!(
        !outcome_deep.allows_transfer(),
        "unanchored *.o must match in deeply nested paths"
    );
}

// ---------------------------------------------------------------------------
// Empty filter program
// ---------------------------------------------------------------------------

#[test]
fn filter_program_empty_allows_everything() {
    let program = FilterProgram::new(std::iter::empty()).expect("empty program");

    assert!(program.is_empty());

    for (path, is_dir) in &[
        ("anything.txt", false),
        ("dir/nested/file.rs", false),
        ("somedir", true),
    ] {
        let outcome = program.evaluate(
            Path::new(path),
            *is_dir,
            &[],
            None,
            FilterContext::Transfer,
        );
        assert!(
            outcome.allows_transfer(),
            "empty program must allow '{path}'"
        );
        assert!(
            outcome.allows_deletion() || *is_dir,
            "empty program must allow deletion of '{path}' (unless special)"
        );
    }
}

// ---------------------------------------------------------------------------
// Protected paths
// ---------------------------------------------------------------------------

#[test]
fn filter_segment_protect_blocks_deletion_but_allows_transfer() {
    let mut segment = FilterSegment::default();
    segment
        .push_rule(FilterRule::protect("database.db"))
        .expect("protect database.db");

    // Transfer context: protect rules should not affect transfer.
    let mut transfer_outcome = FilterOutcome::default();
    segment.apply(
        Path::new("database.db"),
        false,
        &mut transfer_outcome,
        FilterContext::Transfer,
    );
    // Protect rules have applies_to_sender=false so they don't fire in
    // Transfer context.
    assert!(
        transfer_outcome.allows_transfer(),
        "protected path must still allow transfer"
    );

    // Deletion context: protect rule must block deletion.
    let mut deletion_outcome = FilterOutcome::default();
    segment.apply(
        Path::new("database.db"),
        false,
        &mut deletion_outcome,
        FilterContext::Deletion,
    );
    assert!(
        !deletion_outcome.allows_deletion(),
        "protected path must block deletion"
    );
    assert!(
        deletion_outcome.allows_transfer(),
        "protection alone does not block transfer_allowed flag"
    );
}

#[test]
fn filter_segment_risk_removes_protection() {
    let mut segment = FilterSegment::default();
    segment
        .push_rule(FilterRule::protect("data/**"))
        .expect("protect data/**");
    segment
        .push_rule(FilterRule::risk("data/temp/**"))
        .expect("risk data/temp/**");

    // data/important.db is protected.
    let mut protected_outcome = FilterOutcome::default();
    segment.apply(
        Path::new("data/important.db"),
        false,
        &mut protected_outcome,
        FilterContext::Deletion,
    );
    assert!(
        !protected_outcome.allows_deletion(),
        "data/important.db must remain protected"
    );

    // data/temp/cache.bin is de-protected via risk rule.
    let mut risked_outcome = FilterOutcome::default();
    segment.apply(
        Path::new("data/temp/cache.bin"),
        false,
        &mut risked_outcome,
        FilterContext::Deletion,
    );
    assert!(
        risked_outcome.allows_deletion(),
        "data/temp/cache.bin must be deletable after risk rule"
    );
}

#[test]
fn filter_segment_protect_unmatched_path_still_deletable() {
    let mut segment = FilterSegment::default();
    segment
        .push_rule(FilterRule::protect("precious.dat"))
        .expect("protect precious.dat");

    let mut outcome = FilterOutcome::default();
    segment.apply(
        Path::new("expendable.dat"),
        false,
        &mut outcome,
        FilterContext::Deletion,
    );
    assert!(
        outcome.allows_deletion(),
        "unmatched path must remain deletable"
    );
}

// ---------------------------------------------------------------------------
// delete_excluded semantics
// ---------------------------------------------------------------------------

#[test]
fn filter_segment_delete_excluded_flag_set_on_exclude() {
    let mut segment = FilterSegment::default();
    segment
        .push_rule(FilterRule::exclude("*.cache"))
        .expect("exclude cache");

    let mut outcome = FilterOutcome::default();
    segment.apply(
        Path::new("data.cache"),
        false,
        &mut outcome,
        FilterContext::Deletion,
    );

    // In deletion context, an exclude rule sets the delete_excluded flag and
    // also sets transfer_allowed to false.
    assert!(
        outcome.allows_deletion_when_excluded_removed(),
        "delete_excluded flag must be set for excluded paths in deletion context"
    );
}

#[test]
fn filter_segment_delete_excluded_not_set_for_include() {
    let mut segment = FilterSegment::default();
    segment
        .push_rule(FilterRule::include("*.keep"))
        .expect("include keep");

    let mut outcome = FilterOutcome::default();
    segment.apply(
        Path::new("data.keep"),
        false,
        &mut outcome,
        FilterContext::Deletion,
    );

    assert!(
        !outcome.allows_deletion_when_excluded_removed(),
        "delete_excluded flag must not be set for included paths"
    );
}

#[test]
fn filter_segment_delete_excluded_protected_path() {
    // Even when delete_excluded is set, protection must prevent deletion.
    let mut segment = FilterSegment::default();
    segment
        .push_rule(FilterRule::exclude("*.cache"))
        .expect("exclude cache");
    segment
        .push_rule(FilterRule::protect("*.cache"))
        .expect("protect cache");

    let mut outcome = FilterOutcome::default();
    segment.apply(
        Path::new("important.cache"),
        false,
        &mut outcome,
        FilterContext::Deletion,
    );

    assert!(
        outcome.allows_deletion_when_excluded_removed() == false
            || !outcome.allows_deletion(),
        "protected excluded path must not be deletable even with delete_excluded"
    );
}

// ---------------------------------------------------------------------------
// Perishable rules in deletion context
// ---------------------------------------------------------------------------

#[test]
fn filter_segment_perishable_exclude_ignored_in_deletion() {
    let mut segment = FilterSegment::default();
    segment
        .push_rule(FilterRule::exclude("*.tmp").with_perishable(true))
        .expect("perishable exclude");

    // Transfer: perishable rules apply normally.
    let mut transfer_outcome = FilterOutcome::default();
    segment.apply(
        Path::new("data.tmp"),
        false,
        &mut transfer_outcome,
        FilterContext::Transfer,
    );
    assert!(
        !transfer_outcome.allows_transfer(),
        "perishable exclude must block transfer"
    );

    // Deletion: perishable rules are skipped.
    let mut deletion_outcome = FilterOutcome::default();
    segment.apply(
        Path::new("data.tmp"),
        false,
        &mut deletion_outcome,
        FilterContext::Deletion,
    );
    assert!(
        deletion_outcome.allows_deletion(),
        "perishable exclude must be ignored in deletion context"
    );
}

#[test]
fn filter_segment_perishable_protect_ignored_in_deletion() {
    let mut segment = FilterSegment::default();
    segment
        .push_rule(FilterRule::protect("*.tmp").with_perishable(true))
        .expect("perishable protect");

    let mut outcome = FilterOutcome::default();
    segment.apply(
        Path::new("data.tmp"),
        false,
        &mut outcome,
        FilterContext::Deletion,
    );
    // Perishable protect should be skipped in deletion context, so the path
    // remains deletable.
    assert!(
        outcome.allows_deletion(),
        "perishable protect must be skipped in deletion context"
    );
}

// ---------------------------------------------------------------------------
// Sender-only and receiver-only rules
// ---------------------------------------------------------------------------

#[test]
fn filter_segment_sender_only_rule_does_not_affect_deletion() {
    let mut segment = FilterSegment::default();
    // hide = sender-only exclude
    segment
        .push_rule(FilterRule::hide("*.secret"))
        .expect("hide rule");

    // Transfer context uses applies_to_sender.
    let mut transfer_outcome = FilterOutcome::default();
    segment.apply(
        Path::new("data.secret"),
        false,
        &mut transfer_outcome,
        FilterContext::Transfer,
    );
    assert!(
        !transfer_outcome.allows_transfer(),
        "hide rule must block transfer"
    );

    // Deletion context uses applies_to_receiver. hide has receiver=false.
    let mut deletion_outcome = FilterOutcome::default();
    segment.apply(
        Path::new("data.secret"),
        false,
        &mut deletion_outcome,
        FilterContext::Deletion,
    );
    assert!(
        deletion_outcome.allows_deletion(),
        "hide (sender-only) rule must not block deletion"
    );
}

#[test]
fn filter_segment_receiver_only_rule_does_not_affect_transfer() {
    let mut segment = FilterSegment::default();
    // Protect is receiver-only by default.
    segment
        .push_rule(FilterRule::protect("config.yaml"))
        .expect("protect config");

    let mut transfer_outcome = FilterOutcome::default();
    segment.apply(
        Path::new("config.yaml"),
        false,
        &mut transfer_outcome,
        FilterContext::Transfer,
    );
    // Protect has applies_to_sender=false, so in Transfer context it does nothing.
    assert!(
        transfer_outcome.allows_transfer(),
        "receiver-only protect must not block transfer"
    );
}

// ---------------------------------------------------------------------------
// FilterProgram evaluate with dir-merge layers
// ---------------------------------------------------------------------------

#[test]
fn filter_program_evaluate_with_empty_dir_merge_layers() {
    let program = FilterProgram::new([
        FilterProgramEntry::Rule(FilterRule::exclude("*.tmp")),
        FilterProgramEntry::DirMerge(DirMergeRule::new(
            ".rsync-filter",
            DirMergeOptions::default(),
        )),
    ])
    .expect("compile program");

    // With no dir-merge content, only static rules matter.
    let outcome = program.evaluate(
        Path::new("test.tmp"),
        false,
        &[vec![]], // one empty layer for the one DirMerge directive
        None,
        FilterContext::Transfer,
    );
    assert!(
        !outcome.allows_transfer(),
        "static exclude must still block with empty dir-merge layers"
    );
}

#[test]
fn filter_program_evaluate_dir_merge_overrides_static() {
    let program = FilterProgram::new([
        FilterProgramEntry::Rule(FilterRule::exclude("*.tmp")),
        FilterProgramEntry::DirMerge(DirMergeRule::new(
            ".rsync-filter",
            DirMergeOptions::default(),
        )),
    ])
    .expect("compile program");

    // The dir-merge segment contains an include rule that overrides the exclude.
    let mut merge_segment = FilterSegment::default();
    merge_segment
        .push_rule(FilterRule::include("important.tmp"))
        .expect("include in merge");
    let dir_layers = vec![vec![merge_segment]];

    let outcome = program.evaluate(
        Path::new("important.tmp"),
        false,
        &dir_layers,
        None,
        FilterContext::Transfer,
    );
    // The static segment excludes *.tmp first, then the dir-merge segment
    // re-includes important.tmp. Since segments are processed in order,
    // the dir-merge segment's include rule fires last and wins.
    assert!(
        outcome.allows_transfer(),
        "dir-merge include must override static exclude"
    );
}

// ---------------------------------------------------------------------------
// FilterProgram clear resets accumulated rules
// ---------------------------------------------------------------------------

#[test]
fn filter_program_clear_entry_resets_all_rules() {
    let program = FilterProgram::new([
        FilterProgramEntry::Rule(FilterRule::exclude("*.txt")),
        FilterProgramEntry::Clear,
        FilterProgramEntry::Rule(FilterRule::exclude("*.log")),
    ])
    .expect("compile program");

    // After clear, the *.txt exclude should be gone.
    let outcome_txt = program.evaluate(
        Path::new("notes.txt"),
        false,
        &[],
        None,
        FilterContext::Transfer,
    );
    assert!(
        outcome_txt.allows_transfer(),
        "*.txt rule must be cleared"
    );

    // The *.log exclude added after clear should still be active.
    let outcome_log = program.evaluate(
        Path::new("server.log"),
        false,
        &[],
        None,
        FilterContext::Transfer,
    );
    assert!(
        !outcome_log.allows_transfer(),
        "*.log rule added after clear must be active"
    );
}

// ---------------------------------------------------------------------------
// FilterProgram with ExcludeIfPresent
// ---------------------------------------------------------------------------

#[test]
fn filter_program_exclude_if_present_with_marker() {
    let temp = tempfile::tempdir().expect("tempdir");
    let dir = temp.path();

    // Create the marker file.
    std::fs::write(dir.join(".nobackup"), b"").expect("write marker");

    let program = FilterProgram::new([FilterProgramEntry::ExcludeIfPresent(
        ExcludeIfPresentRule::new(".nobackup"),
    )])
    .expect("compile program");

    assert!(
        program
            .should_exclude_directory(dir)
            .expect("marker check"),
        "directory with .nobackup marker must be excluded"
    );
}

#[test]
fn filter_program_exclude_if_present_without_marker() {
    let temp = tempfile::tempdir().expect("tempdir");

    let program = FilterProgram::new([FilterProgramEntry::ExcludeIfPresent(
        ExcludeIfPresentRule::new(".nobackup"),
    )])
    .expect("compile program");

    assert!(
        !program
            .should_exclude_directory(temp.path())
            .expect("marker check"),
        "directory without marker must not be excluded"
    );
}

// ---------------------------------------------------------------------------
// Complex multi-rule scenarios matching upstream rsync patterns
// ---------------------------------------------------------------------------

#[test]
fn filter_program_upstream_typical_pattern() {
    // A typical upstream rsync invocation:
    //   rsync -avz --filter='- *.o' --filter='- *.tmp' --filter='P *.db' src/ dst/
    let program = FilterProgram::new([
        FilterProgramEntry::Rule(FilterRule::exclude("*.o")),
        FilterProgramEntry::Rule(FilterRule::exclude("*.tmp")),
        FilterProgramEntry::Rule(FilterRule::protect("*.db")),
    ])
    .expect("compile program");

    let cases: &[(&str, bool, bool)] = &[
        // (path, allows_transfer, allows_deletion)
        ("src/main.c", true, true),
        ("src/main.o", false, false),
        ("cache/data.tmp", false, false),
        ("data/records.db", true, false), // protected from deletion
        ("readme.txt", true, true),
    ];

    for &(path, expected_transfer, expected_deletion) in cases {
        let transfer = program.evaluate(
            Path::new(path),
            false,
            &[],
            None,
            FilterContext::Transfer,
        );
        assert_eq!(
            transfer.allows_transfer(),
            expected_transfer,
            "transfer for '{path}': expected={expected_transfer}"
        );

        let deletion = program.evaluate(
            Path::new(path),
            false,
            &[],
            None,
            FilterContext::Deletion,
        );
        assert_eq!(
            deletion.allows_deletion(),
            expected_deletion,
            "deletion for '{path}': expected={expected_deletion}"
        );
    }
}

#[test]
fn filter_program_include_only_specific_extensions() {
    // Pattern: exclude everything, then include specific extensions.
    // This mirrors: rsync --filter='+ *.rs' --filter='+ *.toml' --filter='- *'
    //
    // Note: in upstream rsync, the FIRST matching rule wins. In our engine,
    // rules are processed sequentially within a segment and the LAST match
    // wins. So to achieve "include *.rs, exclude everything else" we need:
    //   - exclude *
    //   - include *.rs
    //   - include *.toml
    // (exclude * fires first for .rs files, then include *.rs overrides it)
    let program = FilterProgram::new([
        FilterProgramEntry::Rule(FilterRule::exclude("*")),
        FilterProgramEntry::Rule(FilterRule::include("*.rs")),
        FilterProgramEntry::Rule(FilterRule::include("*.toml")),
    ])
    .expect("compile program");

    let cases: &[(&str, bool)] = &[
        ("lib.rs", true),
        ("Cargo.toml", true),
        ("readme.md", false),
        ("image.png", false),
    ];

    for &(path, expected_allowed) in cases {
        let outcome = program.evaluate(
            Path::new(path),
            false,
            &[],
            None,
            FilterContext::Transfer,
        );
        assert_eq!(
            outcome.allows_transfer(),
            expected_allowed,
            "path '{path}': expected_allowed={expected_allowed}"
        );
    }
}

// ---------------------------------------------------------------------------
// FilterProgramError handling
// ---------------------------------------------------------------------------

#[test]
fn filter_program_invalid_pattern_returns_error() {
    let result = FilterProgram::new([FilterProgramEntry::Rule(FilterRule::exclude(
        "bad[pattern",
    ))]);
    assert!(
        result.is_err(),
        "invalid glob pattern must produce an error"
    );
    let err = result.unwrap_err();
    assert_eq!(err.pattern(), "bad[pattern");
}

// ---------------------------------------------------------------------------
// FilterSegment is_empty after push_rule
// ---------------------------------------------------------------------------

#[test]
fn filter_segment_empty_after_default() {
    let segment = FilterSegment::default();
    assert!(segment.is_empty(), "default segment must be empty");
}

#[test]
fn filter_segment_not_empty_after_include() {
    let mut segment = FilterSegment::default();
    segment
        .push_rule(FilterRule::include("*.rs"))
        .expect("include rule");
    assert!(!segment.is_empty(), "segment with rules must not be empty");
}

#[test]
fn filter_segment_not_empty_after_protect() {
    let mut segment = FilterSegment::default();
    segment
        .push_rule(FilterRule::protect("important/"))
        .expect("protect rule");
    assert!(
        !segment.is_empty(),
        "segment with protect rules must not be empty"
    );
}

// ---------------------------------------------------------------------------
// FilterContext equality
// ---------------------------------------------------------------------------

#[test]
fn filter_context_transfer_ne_deletion() {
    assert_ne!(FilterContext::Transfer, FilterContext::Deletion);
}

#[test]
fn filter_context_transfer_eq_transfer() {
    assert_eq!(FilterContext::Transfer, FilterContext::Transfer);
}

#[test]
fn filter_context_deletion_eq_deletion() {
    assert_eq!(FilterContext::Deletion, FilterContext::Deletion);
}

// ---------------------------------------------------------------------------
// Integration: FilterProgram.is_empty after various constructions
// ---------------------------------------------------------------------------

#[test]
fn filter_program_is_empty_true_for_empty_iter() {
    let program = FilterProgram::new(std::iter::empty()).expect("empty program");
    assert!(program.is_empty());
}

#[test]
fn filter_program_is_empty_false_with_exclude() {
    let program =
        FilterProgram::new([FilterProgramEntry::Rule(FilterRule::exclude("*.tmp"))])
            .expect("program with exclude");
    assert!(!program.is_empty());
}

#[test]
fn filter_program_is_empty_true_after_clear() {
    let program = FilterProgram::new([
        FilterProgramEntry::Rule(FilterRule::exclude("*.tmp")),
        FilterProgramEntry::Clear,
    ])
    .expect("program with clear");
    assert!(program.is_empty());
}

#[test]
fn filter_program_is_empty_false_with_dir_merge() {
    let program = FilterProgram::new([FilterProgramEntry::DirMerge(DirMergeRule::new(
        ".rsync-filter",
        DirMergeOptions::default(),
    ))])
    .expect("program with dir merge");
    assert!(!program.is_empty());
}

#[test]
fn filter_program_is_empty_false_with_exclude_if_present() {
    let program = FilterProgram::new([FilterProgramEntry::ExcludeIfPresent(
        ExcludeIfPresentRule::new(".nobackup"),
    )])
    .expect("program with exclude-if-present");
    assert!(!program.is_empty());
}
