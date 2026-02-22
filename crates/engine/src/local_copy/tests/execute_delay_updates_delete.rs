// Tests for the --delay-updates + --delete-during interaction.
//
// When --delay-updates is active, files are staged in `.~tmp~` subdirectories
// and renamed into place only after all transfers complete. If --delete is
// set with During timing, the timing must be promoted to After so that staged
// `.~tmp~` entries are not mistakenly treated as extraneous and removed
// before the rename sweep runs.
//
// Key behaviors verified:
// 1. Extraneous destination files are deleted after staged files are renamed.
// 2. No `.~tmp~` staging artifacts remain after the transfer.
// 3. The summary correctly reports both transferred and deleted counts.

// ==================== delay-updates + delete-during interaction ====================

/// Verifies that when --delay-updates and --delete (During timing) are both
/// active, the extraneous destination file C is not removed until AFTER files
/// A and B have been renamed from staging into their final locations.
///
/// If deletion timing were not promoted from During to After, the executor
/// could delete `.~tmp~` staging directories mid-transfer or delete C before
/// the rename sweep, potentially causing data loss or incorrect results.
#[test]
fn delay_updates_delete_during_deletes_after_rename() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source");
    let dest = temp.path().join("dest");
    fs::create_dir_all(&source).expect("create source");
    fs::create_dir_all(&dest).expect("create dest");

    // Source: files A and B.
    fs::write(source.join("a.txt"), b"content of a").expect("write a");
    fs::write(source.join("b.txt"), b"content of b").expect("write b");

    // Dest: files A, B (shorter content to ensure different size avoids
    // quick-check skipping), and extraneous C that must be deleted after rename.
    fs::write(dest.join("a.txt"), b"old a").expect("write old a");
    fs::write(dest.join("b.txt"), b"old b").expect("write old b");
    fs::write(dest.join("c.txt"), b"extraneous").expect("write c");

    let mut source_operand = source.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    // --delete defaults to During timing; --delay-updates must promote it to
    // After so that staged .~tmp~ files are not removed mid-transfer.
    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .delete(true)
                .delay_updates(true),
        )
        .expect("copy succeeds");

    // A and B must have the updated source content.
    assert_eq!(
        fs::read(dest.join("a.txt")).expect("read a"),
        b"content of a",
        "a.txt must contain source content after delay-updates rename"
    );
    assert_eq!(
        fs::read(dest.join("b.txt")).expect("read b"),
        b"content of b",
        "b.txt must contain source content after delay-updates rename"
    );

    // C must have been deleted (it is absent from the source).
    assert!(
        !dest.join("c.txt").exists(),
        "c.txt should have been deleted as extraneous"
    );

    // Exactly 2 files transferred and 1 file deleted.
    assert_eq!(summary.files_copied(), 2, "expected 2 files transferred");
    assert_eq!(summary.items_deleted(), 1, "expected 1 file deleted");

    // No `.~tmp~` staging directory must remain.
    assert!(
        !dest.join(".~tmp~").exists(),
        ".~tmp~ staging directory must not remain after flush"
    );
}

/// Verifies that after a delay-updates + delete transfer across a nested
/// directory tree, extraneous files in subdirectories are also removed and
/// no staging artifacts remain anywhere in the tree.
#[test]
fn delay_updates_delete_during_removes_extraneous_in_subdirs() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source");
    let dest = temp.path().join("dest");
    fs::create_dir_all(source.join("sub")).expect("create source/sub");
    fs::create_dir_all(dest.join("sub")).expect("create dest/sub");

    // Source has one file at root and one in sub/.
    fs::write(source.join("root.txt"), b"root content").expect("write root");
    fs::write(source.join("sub").join("child.txt"), b"child content").expect("write child");

    // Dest mirrors source but also has extraneous files at both levels.
    fs::write(dest.join("root.txt"), b"old root").expect("write old root");
    fs::write(dest.join("extra_root.txt"), b"extra at root").expect("write extra root");
    fs::write(dest.join("sub").join("child.txt"), b"old child").expect("write old child");
    fs::write(dest.join("sub").join("extra_sub.txt"), b"extra in sub").expect("write extra sub");

    let mut source_operand = source.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .delete(true)
                .delay_updates(true),
        )
        .expect("copy succeeds");

    // Updated files must have source content.
    assert_eq!(
        fs::read(dest.join("root.txt")).expect("read root"),
        b"root content"
    );
    assert_eq!(
        fs::read(dest.join("sub").join("child.txt")).expect("read child"),
        b"child content"
    );

    // Extraneous files must be gone.
    assert!(
        !dest.join("extra_root.txt").exists(),
        "extra_root.txt must be deleted"
    );
    assert!(
        !dest.join("sub").join("extra_sub.txt").exists(),
        "extra_sub.txt must be deleted"
    );

    // 2 files transferred, 2 deleted.
    assert_eq!(summary.files_copied(), 2);
    assert_eq!(summary.items_deleted(), 2);

    // Walk the entire destination tree and assert no `.~tmp~` dirs remain.
    fn assert_no_staging(dir: &Path) {
        for entry in fs::read_dir(dir).expect("read dir") {
            let entry = entry.expect("dir entry");
            let name = entry.file_name();
            assert_ne!(
                name.to_string_lossy().as_ref(),
                ".~tmp~",
                "staging dir must not remain under {}",
                dir.display()
            );
            if entry.file_type().expect("file type").is_dir() {
                assert_no_staging(&entry.path());
            }
        }
    }
    assert_no_staging(&dest);
}
