// Multi-source `--delete` convergence pins, measured against upstream rsync
// 3.5.0 (flist.c:2499 send_file_list folds every operand into ONE flist;
// flist.c:3364-3382 flist_sort_and_clean keeps the FIRST duplicate;
// generator.c:1924-1927 delete_in_dir sweeps each merged-flist directory once,
// during the walk; generator.c:364-396 do_delete_pass for --delete-before;
// generator.c:2901-2902 for --delete-after).
//
// Each test pins a cell of the measured upstream matrix that oc formerly
// diverged on, plus two already-correct control cells that must not move.

/// Builds the shared two-source fixture:
/// `srcA/{a.txt, common.txt=contentA, sub/{fileA.txt, shared.txt=sharedA}}`,
/// `srcB/{b.txt, common.txt=contentB, sub/{fileB.txt, shared.txt=sharedB}}`,
/// `dest/{junk.txt, a.txt, b.txt, common.txt=stale, sub/{junk_sub.txt, fileB.txt}}`.
/// Every dest entry except `junk.txt`/`sub/junk_sub.txt` is protected by one
/// of the sources; `common.txt` and `sub/shared.txt` are duplicate names whose
/// FIRST source must win.
fn multi_source_delete_fixture(temp: &tempfile::TempDir) -> (PathBuf, PathBuf, PathBuf) {
    let src_a = temp.path().join("srcA");
    let src_b = temp.path().join("srcB");
    let dest = temp.path().join("dest");
    fs::create_dir_all(src_a.join("sub")).expect("create srcA");
    fs::create_dir_all(src_b.join("sub")).expect("create srcB");
    fs::create_dir_all(dest.join("sub")).expect("create dest");
    fs::write(src_a.join("a.txt"), b"onlyA").expect("write a");
    fs::write(src_a.join("common.txt"), b"contentA").expect("write commonA");
    fs::write(src_a.join("sub/fileA.txt"), b"subA").expect("write fileA");
    fs::write(src_a.join("sub/shared.txt"), b"sharedA").expect("write sharedA");
    fs::write(src_b.join("b.txt"), b"onlyB").expect("write b");
    fs::write(src_b.join("common.txt"), b"contentB").expect("write commonB");
    fs::write(src_b.join("sub/fileB.txt"), b"subB").expect("write fileB");
    fs::write(src_b.join("sub/shared.txt"), b"sharedB").expect("write sharedB");
    fs::write(dest.join("junk.txt"), b"junk").expect("write junk");
    fs::write(dest.join("a.txt"), b"onlyA").expect("write dest a");
    fs::write(dest.join("b.txt"), b"onlyB").expect("write dest b");
    fs::write(dest.join("common.txt"), b"stale").expect("write dest common");
    fs::write(dest.join("sub/junk_sub.txt"), b"subjunk").expect("write junk_sub");
    // Stale content (different size) so the quick check always re-copies it,
    // making the sub/ transfer deterministic for the timing pin.
    fs::write(dest.join("sub/fileB.txt"), b"stale-subB").expect("write dest fileB");
    (src_a, src_b, dest)
}

/// Appends a trailing separator so the operand copies its CONTENTS.
fn contents_operand(path: &Path) -> std::ffi::OsString {
    let mut operand = path.to_path_buf().into_os_string();
    operand.push(std::path::MAIN_SEPARATOR.to_string());
    operand
}

fn record_index(
    records: &[LocalCopyRecord],
    action: &LocalCopyAction,
    relative: &str,
) -> Option<usize> {
    records
        .iter()
        .position(|r| r.action() == action && r.relative_path() == Path::new(relative))
}

/// Upstream matrix cell `--delete srcA/ srcB/ dest/`: the merged flist
/// protects every source's entries from the sweep, deletes exactly the two
/// extraneous names, and the FIRST source's copy of a duplicate name wins at
/// every depth (flist.c:3364-3382 "keep the first one").
#[test]
fn multi_source_delete_protects_siblings_and_first_source_wins() {
    let temp = tempdir().expect("tempdir");
    let (src_a, src_b, dest) = multi_source_delete_fixture(&temp);
    let operands = vec![
        contents_operand(&src_a),
        contents_operand(&src_b),
        dest.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default()
        .delete(true)
        .collect_events(true);
    let report = plan
        .execute_with_report(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    assert!(!dest.join("junk.txt").exists(), "extraneous root entry");
    assert!(
        !dest.join("sub/junk_sub.txt").exists(),
        "extraneous sub entry"
    );
    assert_eq!(report.summary().items_deleted(), 2);
    assert!(dest.join("a.txt").exists(), "srcA entry must survive");
    assert!(dest.join("b.txt").exists(), "srcB entry must survive");
    assert!(dest.join("sub/fileA.txt").exists());
    assert!(dest.join("sub/fileB.txt").exists());
    assert_eq!(
        fs::read(dest.join("common.txt")).expect("read common"),
        b"contentA",
        "duplicate root name: FIRST source wins (flist.c:3364-3382)"
    );
    assert_eq!(
        fs::read(dest.join("sub/shared.txt")).expect("read shared"),
        b"sharedA",
        "duplicate nested name: FIRST source wins (flist.c:3364-3382)"
    );

    // Upstream itemizes a duplicate name once - the later copy is dropped
    // from the merged flist, never transferred.
    let common_records = report
        .records()
        .iter()
        .filter(|r| r.relative_path() == Path::new("common.txt"))
        .count();
    assert_eq!(common_records, 1, "one record per merged duplicate name");
    let shared_records = report
        .records()
        .iter()
        .filter(|r| r.relative_path() == Path::new("sub/shared.txt"))
        .count();
    assert_eq!(shared_records, 1, "one record per merged nested duplicate");
}

/// Upstream `--delete` (delete-during) sweeps a directory as the generator
/// reaches it, BEFORE that directory's transfers (generator.c:1924-1927) -
/// never as one deferred pass after every transfer. Formerly oc downgraded a
/// multi-source during sweep to an end-of-run pass.
#[test]
fn multi_source_delete_during_sweeps_each_directory_before_its_transfers() {
    let temp = tempdir().expect("tempdir");
    let (src_a, src_b, dest) = multi_source_delete_fixture(&temp);
    let operands = vec![
        contents_operand(&src_a),
        contents_operand(&src_b),
        dest.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default()
        .delete(true)
        .collect_events(true);
    let report = plan
        .execute_with_report(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");
    let records = report.records();

    let junk_deleted = record_index(records, &LocalCopyAction::EntryDeleted, "junk.txt")
        .expect("junk.txt deletion record");
    let junk_sub_deleted =
        record_index(records, &LocalCopyAction::EntryDeleted, "sub/junk_sub.txt")
            .expect("sub/junk_sub.txt deletion record");
    let first_copy = records
        .iter()
        .position(|r| matches!(r.action(), LocalCopyAction::DataCopied))
        .expect("at least one copy record");
    let file_b_copied = record_index(records, &LocalCopyAction::DataCopied, "sub/fileB.txt")
        .expect("sub/fileB.txt copy record");

    assert!(
        junk_deleted < first_copy,
        "root sweep must precede the root's transfers (delete at {junk_deleted}, first copy at {first_copy})"
    );
    assert!(
        junk_sub_deleted < file_b_copied,
        "sub/ sweep must precede sub/'s remaining transfers (delete at {junk_sub_deleted}, copy at {file_b_copied})"
    );
}

/// Upstream matrix cell `--delete-before srcA/ srcB/ dest/`: one delete pass
/// over the merged flist (generator.c:364-396 do_delete_pass) - a later
/// source's pass must never remove what an earlier source just copied.
/// Formerly oc ran an unprotected pass per source and srcB's pass deleted
/// srcA's freshly copied `a.txt` and `sub/fileA.txt`.
#[test]
fn multi_source_delete_before_preserves_earlier_sources_copies() {
    let temp = tempdir().expect("tempdir");
    let (src_a, src_b, dest) = multi_source_delete_fixture(&temp);
    let operands = vec![
        contents_operand(&src_a),
        contents_operand(&src_b),
        dest.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default()
        .delete(true)
        .delete_before(true)
        .collect_events(true);
    let report = plan
        .execute_with_report(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    assert!(
        dest.join("a.txt").exists(),
        "srcA file must survive srcB's pass"
    );
    assert!(
        dest.join("sub/fileA.txt").exists(),
        "srcA nested file must survive srcB's pass"
    );
    assert!(dest.join("b.txt").exists());
    assert!(dest.join("sub/fileB.txt").exists());
    assert!(!dest.join("junk.txt").exists());
    assert!(!dest.join("sub/junk_sub.txt").exists());
    assert_eq!(report.summary().items_deleted(), 2);
}

/// Upstream matrix cell `--delete-after srcA/ srcB/ dest/`: deletions run
/// after every transfer (generator.c:2901-2902) and the duplicate-name winner
/// is still the FIRST source.
#[test]
fn multi_source_delete_after_keeps_end_phase_and_first_source_wins() {
    let temp = tempdir().expect("tempdir");
    let (src_a, src_b, dest) = multi_source_delete_fixture(&temp);
    let operands = vec![
        contents_operand(&src_a),
        contents_operand(&src_b),
        dest.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default()
        .delete(true)
        .delete_after(true)
        .collect_events(true);
    let report = plan
        .execute_with_report(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");
    let records = report.records();

    assert_eq!(
        fs::read(dest.join("common.txt")).expect("read common"),
        b"contentA",
        "duplicate root name: FIRST source wins under --delete-after too"
    );
    assert_eq!(report.summary().items_deleted(), 2);

    let last_copy = records
        .iter()
        .rposition(|r| matches!(r.action(), LocalCopyAction::DataCopied))
        .expect("copy records");
    let first_delete = records
        .iter()
        .position(|r| matches!(r.action(), LocalCopyAction::EntryDeleted))
        .expect("delete records");
    assert!(
        last_copy < first_delete,
        "--delete-after must delete only after every transfer"
    );
}

/// Upstream matrix cell `--delete srcA/ srcB dest/`: the named directory
/// operand is an entry of the merged flist, so the root sweep must never
/// remove `dest/srcB` - in either operand order. Formerly oc copied the srcB
/// tree and then deleted it wholesale.
#[test]
fn named_directory_operand_survives_sibling_delete_sweep() {
    let temp = tempdir().expect("tempdir");
    let (src_a, src_b, dest) = multi_source_delete_fixture(&temp);
    let operands = vec![
        contents_operand(&src_a),
        src_b.clone().into_os_string(),
        dest.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default()
        .delete(true)
        .collect_events(true);
    let report = plan
        .execute_with_report(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    assert!(
        dest.join("srcB").is_dir(),
        "named operand's tree must survive"
    );
    assert!(dest.join("srcB/b.txt").exists());
    assert!(dest.join("srcB/sub/fileB.txt").exists());
    // Extraneous entries unprotected by the merged view are still deleted:
    // junk.txt, b.txt (srcB lands under dest/srcB, not the root), and the
    // sub/ strays srcB no longer protects.
    assert!(!dest.join("junk.txt").exists());
    assert!(!dest.join("b.txt").exists());
    assert!(!dest.join("sub/junk_sub.txt").exists());
    assert!(!dest.join("sub/fileB.txt").exists());
    assert_eq!(report.summary().items_deleted(), 4);
}

/// Same cell with the operand order reversed (`srcB srcA/ dest/`): protection
/// must not depend on which operand queues its sweep first.
#[test]
fn named_directory_operand_first_survives_later_source_sweep() {
    let temp = tempdir().expect("tempdir");
    let (src_a, src_b, dest) = multi_source_delete_fixture(&temp);
    let operands = vec![
        src_b.clone().into_os_string(),
        contents_operand(&src_a),
        dest.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default()
        .delete(true)
        .collect_events(true);
    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    assert!(
        dest.join("srcB").is_dir(),
        "named operand's tree must survive"
    );
    assert!(dest.join("srcB/b.txt").exists());
    assert!(!dest.join("junk.txt").exists());
}

/// Upstream matrix cell `--delete srcB/b.txt srcA/ dest/`: a named FILE
/// operand is a merged-flist entry too and must survive the sibling source's
/// root sweep - even though the sweep is queued by an operand processed later.
#[test]
fn named_file_operand_survives_sibling_delete_sweep() {
    let temp = tempdir().expect("tempdir");
    let (src_a, src_b, dest) = multi_source_delete_fixture(&temp);
    let operands = vec![
        src_b.join("b.txt").into_os_string(),
        contents_operand(&src_a),
        dest.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default()
        .delete(true)
        .collect_events(true);
    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    assert!(
        dest.join("b.txt").exists(),
        "named file operand must survive the sibling source's sweep"
    );
    assert_eq!(fs::read(dest.join("b.txt")).expect("read b"), b"onlyB");
    assert!(!dest.join("junk.txt").exists());
}

/// Non-vacuity control: the single-source cell already matched upstream and
/// must not move - `b.txt` has no protector here, so it IS deleted along with
/// the strays, and the timing machinery is untouched for one source.
#[test]
fn single_source_delete_cell_unchanged() {
    let temp = tempdir().expect("tempdir");
    let (src_a, _src_b, dest) = multi_source_delete_fixture(&temp);
    let operands = vec![contents_operand(&src_a), dest.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default()
        .delete(true)
        .collect_events(true);
    let report = plan
        .execute_with_report(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    assert!(!dest.join("junk.txt").exists());
    assert!(!dest.join("b.txt").exists(), "unprotected without srcB");
    assert!(!dest.join("sub/junk_sub.txt").exists());
    assert!(
        !dest.join("sub/fileB.txt").exists(),
        "unprotected without srcB"
    );
    assert!(dest.join("a.txt").exists());
    assert!(dest.join("sub/fileA.txt").exists());
    assert_eq!(report.summary().items_deleted(), 4);
}

/// Dry-run cell: upstream reports each deletion once because the merged
/// flist's directory is swept once (generator.c:1924-1927). Two operands
/// visiting the same destination directory must not re-report the sweep the
/// first visit already decided - observable only under `--dry-run`, where no
/// unlink empties the directory between visits.
#[test]
fn multi_source_delete_dry_run_reports_each_deletion_once() {
    let temp = tempdir().expect("tempdir");
    let (src_a, src_b, dest) = multi_source_delete_fixture(&temp);
    let operands = vec![
        contents_operand(&src_a),
        contents_operand(&src_b),
        dest.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default()
        .delete(true)
        .collect_events(true);
    let report = plan
        .execute_with_report(LocalCopyExecution::DryRun, options)
        .expect("dry run succeeds");

    let deletion_records = report
        .records()
        .iter()
        .filter(|r| matches!(r.action(), LocalCopyAction::EntryDeleted))
        .count();
    assert_eq!(
        deletion_records, 2,
        "each extraneous entry is reported exactly once"
    );
    assert!(dest.join("junk.txt").exists(), "dry run must not delete");
}

/// Non-vacuity control: two no-trailing-slash operands never sweep the
/// destination ROOT (upstream's flist has no "." entry, so `dest/junk.txt`
/// survives) - already correct before the convergence and pinned so the new
/// root pass cannot over-reach.
#[test]
fn named_operands_without_contents_copy_leave_destination_root_alone() {
    let temp = tempdir().expect("tempdir");
    let (src_a, src_b, dest) = multi_source_delete_fixture(&temp);
    let operands = vec![
        src_a.clone().into_os_string(),
        src_b.clone().into_os_string(),
        dest.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default()
        .delete(true)
        .collect_events(true);
    let report = plan
        .execute_with_report(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    assert!(
        dest.join("junk.txt").exists(),
        "root is not part of the transfer"
    );
    assert!(dest.join("sub/junk_sub.txt").exists());
    assert!(dest.join("srcA/a.txt").exists());
    assert!(dest.join("srcB/b.txt").exists());
    assert_eq!(report.summary().items_deleted(), 0);
}
