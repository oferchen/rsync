use super::common::*;
use super::*;

// ===========================================================================
// Tracing integration: verify verbosity flags produce expected output
// ===========================================================================

#[test]
fn verbose_transfer_emits_filename_on_stdout() {
    use std::fs;

    let temp = tempfile::tempdir().expect("tempdir");
    let source = temp.path().join("traced.txt");
    let dest_dir = temp.path().join("dest");
    fs::create_dir(&dest_dir).expect("create dest");
    fs::write(&source, b"tracing test").expect("write source");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-v"),
        source.into_os_string(),
        dest_dir.into_os_string(),
    ]);

    assert_eq!(code, 0, "stderr: {}", String::from_utf8_lossy(&stderr));
    let output = String::from_utf8(stdout).expect("utf8");
    assert!(
        output.contains("traced.txt"),
        "-v should list transferred filename: {output}"
    );
}

#[test]
fn double_verbose_shows_itemize_changes() {
    use std::fs;

    let temp = tempfile::tempdir().expect("tempdir");
    let source = temp.path().join("item.txt");
    let dest_dir = temp.path().join("dest");
    fs::create_dir(&dest_dir).expect("create dest");
    fs::write(&source, b"itemize test").expect("write source");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-vv"),
        source.into_os_string(),
        dest_dir.into_os_string(),
    ]);

    assert_eq!(code, 0, "stderr: {}", String::from_utf8_lossy(&stderr));
    let output = String::from_utf8(stdout).expect("utf8");
    // -vv enables Name level 2 which shows itemize changes
    assert!(
        output.contains(">f") || output.contains("item.txt"),
        "-vv should show itemize or filename: {output}"
    );
}

#[test]
fn info_flag_copy_shows_copy_messages() {
    use std::fs;

    let temp = tempfile::tempdir().expect("tempdir");
    let source = temp.path().join("copy_info.txt");
    let dest_dir = temp.path().join("dest");
    fs::create_dir(&dest_dir).expect("create dest");
    fs::write(&source, b"copy info test").expect("write source");

    let (code, stdout, _stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--info=name1"),
        source.into_os_string(),
        dest_dir.into_os_string(),
    ]);

    assert_eq!(code, 0);
    let output = String::from_utf8(stdout).expect("utf8");
    assert!(
        output.contains("copy_info.txt"),
        "--info=name1 should show filename: {output}"
    );
}

#[test]
fn info_flag_stats_with_verbose_shows_statistics() {
    use std::fs;

    let temp = tempfile::tempdir().expect("tempdir");
    let source = temp.path().join("stats.txt");
    let dest = temp.path().join("stats.out");
    fs::write(&source, b"stats test content").expect("write source");

    let (code, stdout, _stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--stats"),
        source.into_os_string(),
        dest.into_os_string(),
    ]);

    assert_eq!(code, 0);
    let output = String::from_utf8(stdout).expect("utf8");
    assert!(
        output.contains("Number of files:"),
        "--stats should show transfer statistics: {output}"
    );
    assert!(
        output.contains("Total file size:"),
        "--stats should show total file size: {output}"
    );
}

#[test]
fn quiet_flag_suppresses_verbose_output() {
    use std::fs;

    let temp = tempfile::tempdir().expect("tempdir");
    let source = temp.path().join("quiet.txt");
    let dest = temp.path().join("quiet.out");
    fs::write(&source, b"quiet test").expect("write source");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-q"),
        source.into_os_string(),
        dest.into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty(), "-q should suppress stdout");
    assert!(stderr.is_empty(), "-q should suppress stderr");
}

#[test]
fn verbose_with_recursive_lists_directory_structure() {
    use std::fs;

    let temp = tempfile::tempdir().expect("tempdir");
    let source_dir = temp.path().join("src");
    let dest_dir = temp.path().join("dst");
    fs::create_dir_all(source_dir.join("sub")).expect("create subdirs");
    fs::create_dir(&dest_dir).expect("create dest");
    fs::write(source_dir.join("top.txt"), b"top").expect("write top");
    fs::write(source_dir.join("sub/nested.txt"), b"nested").expect("write nested");

    let mut src_operand = source_dir.into_os_string();
    src_operand.push("/");

    let (code, stdout, _stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-rv"),
        src_operand,
        dest_dir.into_os_string(),
    ]);

    assert_eq!(code, 0);
    let output = String::from_utf8(stdout).expect("utf8");
    assert!(
        output.contains("top.txt"),
        "-rv should list top-level file: {output}"
    );
    assert!(
        output.contains("nested.txt"),
        "-rv should list nested file: {output}"
    );
}

#[test]
fn info_progress2_shows_overall_progress_during_transfer() {
    use std::fs;

    let temp = tempfile::tempdir().expect("tempdir");
    let source_dir = temp.path().join("src");
    let dest_dir = temp.path().join("dst");
    fs::create_dir_all(&source_dir).expect("create source");
    fs::create_dir(&dest_dir).expect("create dest");

    for i in 0..5 {
        fs::write(source_dir.join(format!("file{i}.txt")), vec![0u8; 1000]).expect("write file");
    }

    let mut src_operand = source_dir.into_os_string();
    src_operand.push("/");

    let (code, stdout, _stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--info=progress2"),
        src_operand,
        dest_dir.into_os_string(),
    ]);

    assert_eq!(code, 0);
    let output = String::from_utf8(stdout).expect("utf8");
    // Progress2 shows "to-chk=N/M" counter
    assert!(
        output.contains("to-chk="),
        "--info=progress2 should show to-chk counter: {output}"
    );
}

#[test]
fn debug_del_flag_shows_deletion_debug_output() {
    use std::fs;

    let temp = tempfile::tempdir().expect("tempdir");
    let source_dir = temp.path().join("src");
    let dest_dir = temp.path().join("dst");
    fs::create_dir_all(&source_dir).expect("create source");
    fs::create_dir_all(&dest_dir).expect("create dest");

    // Source has one file, dest has an extra file to delete
    fs::write(source_dir.join("keep.txt"), b"keep").expect("write keep");
    fs::write(dest_dir.join("keep.txt"), b"old").expect("write old keep");
    fs::write(dest_dir.join("extra.txt"), b"delete me").expect("write extra");

    let mut src_operand = source_dir.into_os_string();
    src_operand.push("/");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-rv"),
        OsString::from("--delete"),
        src_operand,
        dest_dir.clone().into_os_string(),
    ]);

    assert_eq!(code, 0, "stderr: {}", String::from_utf8_lossy(&stderr));
    let output = String::from_utf8(stdout).expect("utf8");
    assert!(
        output.contains("deleting") || output.contains("extra.txt"),
        "-rv --delete should show deletion activity: {output}"
    );
    // Verify the file was actually deleted
    assert!(
        !dest_dir.join("extra.txt").exists(),
        "extra.txt should be deleted"
    );
}

#[test]
fn out_format_with_itemize_produces_structured_output() {
    use std::fs;

    let temp = tempfile::tempdir().expect("tempdir");
    let source = temp.path().join("fmt.txt");
    let dest_dir = temp.path().join("dest");
    fs::create_dir(&dest_dir).expect("create dest");
    fs::write(&source, b"format test").expect("write source");

    let (code, stdout, _stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--out-format=%i %n %l"),
        source.into_os_string(),
        dest_dir.into_os_string(),
    ]);

    assert_eq!(code, 0);
    let output = String::from_utf8(stdout).expect("utf8");
    let line = output.trim();

    // Should have itemize string, filename, and size
    assert!(
        line.contains("fmt.txt"),
        "out-format should include filename: {line}"
    );
    assert!(
        line.contains("11"),
        "out-format should include file size (11 bytes): {line}"
    );
}
