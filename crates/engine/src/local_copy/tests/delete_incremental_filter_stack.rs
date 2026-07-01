// PEX (#333): equivalence + complexity gate for the incremental destination
// filter stack used by the local-copy delete DECIDE pass.
//
// The delete pass loads each destination directory's per-dir merge files
// (`.rsync-filter` / `dir-merge` / per-dir `:s`) onto the SAME shared filter
// stacks the source-side `allows()` evaluation reads, then must leave those
// stacks byte-identical when the scan for that directory ends. The former
// implementation cloned the WHOLE source-visible ancestor stack before every
// destination load and restored it verbatim (O(depth) per visit, O(depth^2)
// across the walk). The incremental implementation pushes only this directory's
// destination merge rules and undoes exactly that delta on drop - restoring the
// specific inherited layer indices a `clear` directive wiped and relying on the
// per-index pop for everything else.
//
// These tests are the data-loss safety gate: if the incremental restore is not
// byte-identical to the whole-stack snapshot restore, the source-side deletion
// decisions diverge and the wrong files are deleted (or excluded files are
// resurrected into a populated destination and survive deletion). Every
// scenario below therefore asserts the source-visible filter decisions are
// unchanged across the destination-deletion scan, that the in-scan deletion
// decisions match upstream, and - on a deep tree - that the filter-load work is
// O(depth), not O(depth^2).
//
// upstream: delete.c:65-115 delete_in_dir pushes the destination per-dir filters
// onto a separate delete_filt chain (change_local_filter_dir pops filters at
// depth > current, then push_local_filters loads this dir's rules) and never
// perturbs the sender filter_list; exclude.c:801 push_local_filters keeps
// inherited rules in lp->head an arbitrary number of indices deep.

use crate::local_copy::context::{DirectoryFilterGuard, FILTER_FILE_LOAD_COUNT};

/// A `(relative_path, is_dir)` probe evaluated for its source-side transfer
/// decision. The decision vector for a fixed probe battery is a total,
/// deterministic fingerprint of the source-visible filter state; comparing it
/// before and after a destination-deletion scan proves the incremental restore
/// left that state byte-identical.
type Probe<'a> = (&'a str, bool);

/// Evaluates the source-side `allows()` decision for every probe, yielding a
/// vector that fingerprints the current source-visible filter state.
fn source_decision_fingerprint(context: &CopyContext<'_>, probes: &[Probe<'_>]) -> Vec<bool> {
    probes
        .iter()
        .map(|(name, is_dir)| context.allows(std::path::Path::new(name), *is_dir))
        .collect()
}

/// Builds a `CopyContext` with the given filter program rooted at
/// `destination_root`, matching the way the orchestrator constructs it.
fn context_with_program(program: FilterProgram, destination_root: &Path) -> CopyContext<'static> {
    let options = LocalCopyOptions::new()
        .recursive(true)
        .delete(true)
        .with_filter_program(Some(program));
    CopyContext::new(
        LocalCopyExecution::Apply,
        options,
        None,
        destination_root.to_path_buf(),
    )
}

/// Nested `.rsync-filter` (a rule in the child directory applies only below it):
/// after a destination-deletion scan at the deepest level, the source-visible
/// decision for every probe must be identical to before the scan, and the
/// in-scan deletion decision must honour BOTH the ancestor and the child rule.
#[test]
fn incremental_stack_preserves_source_state_with_nested_rsync_filter() {
    let temp = tempdir().expect("tempdir");
    let root = temp.path();

    // dir/.rsync-filter excludes *.top everywhere below `dir/`.
    // dir/sub/.rsync-filter adds a child-only rule `- *.child` that must apply
    // only at and below `dir/sub`.
    fs::create_dir_all(root.join("dir/sub")).expect("mkdir dir/sub");
    fs::write(root.join("dir/.rsync-filter"), "- *.top\n").expect("write dir filter");
    fs::write(root.join("dir/sub/.rsync-filter"), "- *.child\n").expect("write sub filter");

    let program = FilterProgram::new([FilterProgramEntry::DirMerge(DirMergeRule::new(
        ".rsync-filter",
        DirMergeOptions::default(),
    ))])
    .expect("program compiles");
    let context = context_with_program(program, root);

    // Probe battery spanning both merge-file scopes.
    let probes: &[Probe] = &[
        ("dir/keep.dat", false),
        ("dir/drop.top", false),
        ("dir/sub/keep.dat", false),
        ("dir/sub/drop.top", false),
        ("dir/sub/drop.child", false),
    ];

    // Descend the SOURCE stack exactly as the recursive walk does: one nested
    // guard per level. `dir/sub` is the deepest, where the delete scan runs.
    let _g_dir = context
        .enter_directory(&root.join("dir"), Some(std::path::Path::new("dir")))
        .expect("enter dir");
    let _g_sub = context
        .enter_directory(
            &root.join("dir/sub"),
            Some(std::path::Path::new("dir/sub")),
        )
        .expect("enter dir/sub");

    // Sanity: the child-only rule is live at dir/sub.
    assert!(
        !context.allows(std::path::Path::new("dir/sub/x.child"), false),
        "child .rsync-filter rule must apply at dir/sub",
    );

    let before = source_decision_fingerprint(&context, probes);

    {
        // Destination-deletion scan for dir/sub. The destination directory
        // carries its OWN .rsync-filter with the identical rules, loaded onto
        // the shared stacks and popped on guard drop.
        let del_guard = context
            .enter_destination_for_deletion(
                &root.join("dir/sub"),
                Some(std::path::Path::new("dir/sub")),
            )
            .expect("enter destination for deletion");

        // Deletion decisions must honour ancestor `- *.top` AND child
        // `- *.child`; unmatched entries remain deletable.
        assert!(
            context.allows_deletion(std::path::Path::new("dir/sub/gone.dat"), false),
            "unmatched destination entry stays deletable",
        );
        assert!(
            !context.allows_deletion(std::path::Path::new("dir/sub/keep.child"), false),
            "child-scope filter must protect *.child from deletion",
        );
        assert!(
            !context.allows_deletion(std::path::Path::new("dir/sub/keep.top"), false),
            "ancestor-scope filter must protect *.top from deletion",
        );

        // No `clear` directive -> zero cleared-index restore data captured.
        assert_eq!(
            del_guard.cleared_restore_len(),
            0,
            "no clear directive means no per-index restore capture",
        );
    }

    let after = source_decision_fingerprint(&context, probes);
    assert_eq!(
        before, after,
        "destination-deletion scan must leave source-visible state byte-identical",
    );
}

/// `dir-merge` plus a per-dir `:s` (sender-side) merge file. The destination
/// scan must load the SAME nested `dir-merge .filt2` chain the source side sees,
/// leave the source-visible state byte-identical, and honour the side
/// semantics: a `:s` (sender-only) rule excludes from TRANSFER but does NOT
/// protect from DELETION on the receiver, while a receiver-visible nested rule
/// does. Exercising both directions guards against the incremental stack
/// dropping or mis-scoping the nested registration.
#[test]
fn incremental_stack_preserves_source_state_with_dir_merge_and_perdir_s() {
    let temp = tempdir().expect("tempdir");
    let root = temp.path();

    fs::create_dir_all(root.join("m/inner")).expect("mkdir m/inner");
    // Two nested dir-merge registrations inside m/.filt:
    //  - `dir-merge .snd2` is sender-only (:s) -> excludes from transfer, does
    //    NOT protect from deletion on the receiver;
    //  - `dir-merge .rcv2` is a plain (both-sides) merge -> protects from both.
    // Plus a plain inherited `- *.bak` that protects from deletion.
    fs::write(
        root.join("m/.filt"),
        "dir-merge,s .snd2\ndir-merge .rcv2\n- *.bak\n",
    )
    .expect("write m/.filt");
    fs::write(root.join("m/inner/.snd2"), "- *.snd\n").expect("write inner/.snd2");
    fs::write(root.join("m/inner/.rcv2"), "- *.rcv\n").expect("write inner/.rcv2");

    let program = FilterProgram::new([FilterProgramEntry::DirMerge(DirMergeRule::new(
        ".filt",
        DirMergeOptions::default(),
    ))])
    .expect("program compiles");
    let context = context_with_program(program, root);

    let probes: &[Probe] = &[
        ("m/keep.dat", false),
        ("m/drop.bak", false),
        ("m/inner/keep.dat", false),
        ("m/inner/drop.bak", false),
        ("m/inner/drop.snd", false),
        ("m/inner/drop.rcv", false),
    ];

    let _g_m = context
        .enter_directory(&root.join("m"), Some(std::path::Path::new("m")))
        .expect("enter m");
    let _g_inner = context
        .enter_directory(&root.join("m/inner"), Some(std::path::Path::new("m/inner")))
        .expect("enter m/inner");

    // The nested chain must be loaded on the SOURCE side: both `.snd` and `.rcv`
    // are excluded from transfer.
    assert!(
        !context.allows(std::path::Path::new("m/inner/x.snd"), false),
        "sender-side nested .snd2 must exclude from transfer",
    );
    assert!(
        !context.allows(std::path::Path::new("m/inner/x.rcv"), false),
        "both-sides nested .rcv2 must exclude from transfer",
    );

    let before = source_decision_fingerprint(&context, probes);

    {
        let _del = context
            .enter_destination_for_deletion(
                &root.join("m/inner"),
                Some(std::path::Path::new("m/inner")),
            )
            .expect("enter destination for deletion");

        // Receiver-visible nested rule protects from deletion; the inherited
        // plain `- *.bak` protects too.
        assert!(
            !context.allows_deletion(std::path::Path::new("m/inner/x.rcv"), false),
            "both-sides nested dir-merge .rcv2 `- *.rcv` must protect from deletion",
        );
        assert!(
            !context.allows_deletion(std::path::Path::new("m/inner/x.bak"), false),
            "inherited `- *.bak` must protect from deletion",
        );
        // Sender-only nested rule does NOT protect from deletion on the receiver.
        assert!(
            context.allows_deletion(std::path::Path::new("m/inner/x.snd"), false),
            "sender-only nested rule must NOT protect from deletion",
        );
        assert!(
            context.allows_deletion(std::path::Path::new("m/inner/x.dat"), false),
            "unmatched entry stays deletable",
        );
    }

    let after = source_decision_fingerprint(&context, probes);
    assert_eq!(
        before, after,
        "dir-merge + per-dir :s scan must leave source-visible state byte-identical",
    );
}

/// An anchored `- /file` rule in a per-dir merge file binds to that directory,
/// not the transfer root. The destination scan must apply the same anchoring and
/// leave source state untouched.
#[test]
fn incremental_stack_preserves_source_state_with_anchored_rule() {
    let temp = tempdir().expect("tempdir");
    let root = temp.path();

    fs::create_dir_all(root.join("a/sub")).expect("mkdir a/sub");
    fs::write(root.join("a/.filt"), "- /file1\n").expect("write a/.filt");

    let program = FilterProgram::new([FilterProgramEntry::DirMerge(DirMergeRule::new(
        ".filt",
        DirMergeOptions::default(),
    ))])
    .expect("program compiles");
    let context = context_with_program(program, root);

    let probes: &[Probe] = &[
        ("a/file1", false),
        ("a/other", false),
        ("a/sub/file1", false),
    ];

    let _g_a = context
        .enter_directory(&root.join("a"), Some(std::path::Path::new("a")))
        .expect("enter a");

    let before = source_decision_fingerprint(&context, probes);

    {
        let _del = context
            .enter_destination_for_deletion(&root.join("a"), Some(std::path::Path::new("a")))
            .expect("enter destination for deletion");

        assert!(
            !context.allows_deletion(std::path::Path::new("a/file1"), false),
            "anchored `- /file1` must protect a/file1 from deletion",
        );
        assert!(
            context.allows_deletion(std::path::Path::new("a/sub/file1"), false),
            "anchor must NOT reach the deeper a/sub/file1",
        );
        assert!(
            context.allows_deletion(std::path::Path::new("a/other"), false),
            "unanchored entry stays deletable",
        );
    }

    let after = source_decision_fingerprint(&context, probes);
    assert_eq!(
        before, after,
        "anchored-rule scan must leave source-visible state byte-identical",
    );
}

/// EXCLUDE_SELF: a per-dir merge file with the self-excluding modifier must NOT
/// mark itself deletable, and the scan must leave source state unchanged.
#[test]
fn incremental_stack_excludes_self_and_preserves_source_state() {
    let temp = tempdir().expect("tempdir");
    let root = temp.path();

    fs::create_dir_all(root.join("e")).expect("mkdir e");
    fs::write(root.join("e/.filt"), "- *.junk\n").expect("write e/.filt");

    // EXCLUDE_SELF: the `e` modifier makes the merge file exclude ITSELF.
    let program = FilterProgram::new([FilterProgramEntry::DirMerge(DirMergeRule::new(
        ".filt",
        DirMergeOptions::new().exclude_filter_file(true),
    ))])
    .expect("program compiles");
    let context = context_with_program(program, root);

    let probes: &[Probe] = &[("e/keep.dat", false), ("e/drop.junk", false)];

    let _g_e = context
        .enter_directory(&root.join("e"), Some(std::path::Path::new("e")))
        .expect("enter e");

    let before = source_decision_fingerprint(&context, probes);

    {
        let _del = context
            .enter_destination_for_deletion(&root.join("e"), Some(std::path::Path::new("e")))
            .expect("enter destination for deletion");

        assert!(
            !context.allows_deletion(std::path::Path::new("e/.filt"), false),
            "EXCLUDE_SELF: the merge file must NOT delete itself",
        );
        assert!(
            !context.allows_deletion(std::path::Path::new("e/x.junk"), false),
            "the merge rule `- *.junk` must protect from deletion",
        );
        assert!(
            context.allows_deletion(std::path::Path::new("e/x.dat"), false),
            "unmatched entry stays deletable",
        );
    }

    let after = source_decision_fingerprint(&context, probes);
    assert_eq!(
        before, after,
        "EXCLUDE_SELF scan must leave source-visible state byte-identical",
    );
}

/// A `clear` directive in the destination merge file wipes inherited layer
/// entries the per-index pop cannot rebuild. The incremental restore must
/// capture and reinstate exactly those wiped indices, leaving the source-visible
/// state byte-identical. This is the case the former whole-stack snapshot
/// existed for; it must remain byte-identical without the snapshot.
#[test]
fn incremental_stack_restores_cleared_inherited_layers() {
    let temp = tempdir().expect("tempdir");
    let root = temp.path();

    fs::create_dir_all(root.join("c/child")).expect("mkdir c/child");
    // Parent registers an inheritable rule `- *.parent`.
    fs::write(root.join("c/.filt"), "- *.parent\n").expect("write c/.filt");
    // Child issues `clear` (wiping the inherited `- *.parent` at this index)
    // then adds `- *.child`.
    fs::write(root.join("c/child/.filt"), "clear\n- *.child\n").expect("write child/.filt");

    let program = FilterProgram::new([FilterProgramEntry::DirMerge(DirMergeRule::new(
        ".filt",
        DirMergeOptions::default(),
    ))])
    .expect("program compiles");
    let context = context_with_program(program, root);

    let probes: &[Probe] = &[
        ("c/x.parent", false),
        ("c/keep.dat", false),
        ("c/child/x.parent", false),
        ("c/child/x.child", false),
    ];

    let _g_c = context
        .enter_directory(&root.join("c"), Some(std::path::Path::new("c")))
        .expect("enter c");

    // At `c`, the inherited `- *.parent` is live.
    assert!(
        !context.allows(std::path::Path::new("c/x.parent"), false),
        "parent rule live at c before any child scan",
    );

    let before = source_decision_fingerprint(&context, probes);

    {
        // Destination-deletion scan for c/child, whose .filt clears inherited
        // rules. This wipes the inherited `- *.parent` layer index.
        let del_guard = context
            .enter_destination_for_deletion(
                &root.join("c/child"),
                Some(std::path::Path::new("c/child")),
            )
            .expect("enter destination for deletion");

        // The child `clear` dropped the inherited parent rule for this scope, so
        // *.parent is deletable here while *.child is protected.
        assert!(
            context.allows_deletion(std::path::Path::new("c/child/x.parent"), false),
            "after clear, inherited *.parent no longer protects in child scope",
        );
        assert!(
            !context.allows_deletion(std::path::Path::new("c/child/x.child"), false),
            "child rule `- *.child` protects from deletion",
        );

        // The clear must have captured exactly the wiped index for restore.
        assert!(
            del_guard.cleared_restore_len() >= 1,
            "a clear directive must capture at least one index to restore",
        );
    }

    // After the scan, the inherited `- *.parent` must be live again at `c` -
    // the whole point of the restore. If the clear leaked, this fingerprint
    // diverges and the source-side deletion decisions would be corrupted.
    let after = source_decision_fingerprint(&context, probes);
    assert_eq!(
        before, after,
        "cleared inherited layers must be restored byte-identically after the scan",
    );
    assert!(
        !context.allows(std::path::Path::new("c/x.parent"), false),
        "inherited parent rule must be live again at c after the child scan",
    );
}

/// Deep tree (>= 5 levels): the destination-deletion scan at each level must
/// perform O(depth) filter-file loads across the whole descent - one lookup per
/// directory entered, NOT a re-load of every ancestor at every visit (which
/// would be O(depth^2)). Also asserts source state survives every scan.
#[test]
fn incremental_stack_is_linear_in_depth_on_deep_tree() {
    let temp = tempdir().expect("tempdir");
    let root = temp.path();

    // 6-level tree l0/l1/l2/l3/l4/l5, each carrying a per-dir merge file with a
    // level-specific exclude so inheritance accumulates with depth.
    const DEPTH: usize = 6;
    let mut dir = root.to_path_buf();
    for level in 0..DEPTH {
        dir.push(format!("l{level}"));
        fs::create_dir_all(&dir).expect("mkdir level");
        fs::write(dir.join(".filt"), format!("- *.lvl{level}\n")).expect("write level .filt");
    }

    let program = FilterProgram::new([FilterProgramEntry::DirMerge(DirMergeRule::new(
        ".filt",
        DirMergeOptions::default(),
    ))])
    .expect("program compiles");
    let context = context_with_program(program, root);

    // Descend, holding a guard per level (mirrors the recursive walk). At every
    // level, run a destination-deletion scan and confirm source state is
    // preserved. Count filter-file loads across the ENTIRE descent.
    let mut guards: Vec<DirectoryFilterGuard> = Vec::new();
    let mut rel = std::path::PathBuf::new();
    let mut abs = root.to_path_buf();

    // Probe battery: one path per level that the level's own rule excludes.
    let probe_names: Vec<String> = (0..DEPTH).map(|l| format!("l{l}/x.lvl{l}")).collect();
    let probes: Vec<Probe> = probe_names.iter().map(|s| (s.as_str(), false)).collect();

    // Reset the load counter, then perform the full descent + per-level scans.
    FILTER_FILE_LOAD_COUNT.with(|c| c.set(0));

    for level in 0..DEPTH {
        rel.push(format!("l{level}"));
        abs.push(format!("l{level}"));

        let guard = context
            .enter_directory(&abs, Some(rel.as_path()))
            .expect("enter level");
        guards.push(guard);

        let before = source_decision_fingerprint(&context, &probes);
        {
            let _del = context
                .enter_destination_for_deletion(&abs, Some(rel.as_path()))
                .expect("enter destination for deletion at level");
            // The level's own rule protects its matching entry from deletion.
            let own = format!("{}/x.lvl{level}", rel.display());
            assert!(
                !context.allows_deletion(std::path::Path::new(&own), false),
                "level {level} own rule must protect its entry from deletion",
            );
        }
        let after = source_decision_fingerprint(&context, &probes);
        assert_eq!(
            before, after,
            "level {level} destination scan must leave source state byte-identical",
        );
    }

    let total_loads = FILTER_FILE_LOAD_COUNT.with(|c| c.get());

    // Each level's descent enter_directory loads that level's ONE merge file
    // (inheriting the ancestor segments already resident on the stack - no
    // re-load), and each per-level destination scan loads that level's ONE merge
    // file again. That is 2 loads per level = 2*DEPTH total: strictly linear.
    //
    // A rebuild-per-visit design would re-load every ancestor at each visit:
    // sum(1..=DEPTH) per phase = O(DEPTH^2). The exact-equality assertion below
    // fails loudly if any ancestor re-load creeps back in.
    assert_eq!(
        total_loads,
        2 * DEPTH,
        "filter-file loads must be O(depth) (2 per level), not O(depth^2); got {total_loads} for depth {DEPTH}",
    );

    // Hold the guards to the end so the descent nesting is well-formed on drop.
    drop(guards);
}
