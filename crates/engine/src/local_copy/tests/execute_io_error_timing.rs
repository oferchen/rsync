// Which I/O errors gate which delete pass, and when.
//
// upstream: generator.c:304 `delete_in_dir()` is the only gate, testing
// `io_error & IOERR_GENERAL && !ignore_errors`. The generator's `io_error`
// carries file-list errors as each list arrives (flist.c:2736, flist.c:3070),
// but a sender open or read failure reaches it only once the whole send loop
// has finished (sender.c:811-812 `send_msg_int(MSG_IO_ERROR, io_error)`).
// So a transfer-time failure never suppresses a `--delete-during` or
// `--delete-delay` decision taken during the walk; only the `--delete-after`
// pass, which runs after the transfer, sees it. `IOERR_VANISHED` is a separate
// bit and gates no deletion at all.

/// Makes `path` unreadable; returns `false` (after a loud skip notice) when the
/// process can still read it, i.e. it runs with `CAP_DAC_OVERRIDE`.
#[cfg(unix)]
fn make_unreadable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o000)).expect("chmod 000");
    if fs::File::open(path).is_ok() {
        eprintln!(
            "SKIP: {} is still readable after chmod 000 (running privileged)",
            path.display()
        );
        return false;
    }
    true
}

#[cfg(unix)]
fn restore_readable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o644));
}

#[cfg(unix)]
fn trailing_operand(path: &Path) -> OsString {
    let mut operand = path.as_os_str().to_os_string();
    operand.push(std::path::MAIN_SEPARATOR.to_string());
    operand
}

/// An unreadable regular file in `a/`, sorted before the extraneous entry's
/// directory `z/`, must not keep the walk-decided deletion of `z/stale`.
#[cfg(unix)]
fn assert_transfer_open_failure_keeps_walk_decided_deletion(options: LocalCopyOptions) {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("src");
    let dest = temp.path().join("dst");
    fs::create_dir_all(source.join("a")).expect("mkdir src/a");
    fs::create_dir_all(source.join("z")).expect("mkdir src/z");
    fs::create_dir_all(dest.join("z")).expect("mkdir dst/z");
    fs::write(source.join("a/bad"), b"bad").expect("write bad");
    fs::write(source.join("z/keep"), b"keep").expect("write keep");
    fs::write(dest.join("z/stale"), b"stale").expect("write stale");
    if !make_unreadable(&source.join("a/bad")) {
        return;
    }
    let operands = vec![trailing_operand(&source), dest.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let result = plan.execute_with_options(LocalCopyExecution::Apply, options.recursive(true));
    restore_readable(&source.join("a/bad"));
    assert!(
        !dest.join("z/stale").exists(),
        "a send-time open failure must not suppress a walk-decided deletion"
    );
    let error = result.expect_err("an unreadable source must fail the run");
    assert_eq!(
        error.exit_code(),
        23,
        "unreadable source exits RERR_PARTIAL"
    );
}

#[cfg(unix)]
#[test]
fn delete_during_survives_a_later_send_time_open_failure() {
    assert_transfer_open_failure_keeps_walk_decided_deletion(
        LocalCopyOptions::default().delete(true),
    );
}

#[cfg(unix)]
#[test]
fn delete_delay_survives_a_later_send_time_open_failure() {
    assert_transfer_open_failure_keeps_walk_decided_deletion(
        LocalCopyOptions::default().delete_delay(true),
    );
}

/// `--delete-after` runs once the transfer is over, so a send-time open
/// failure in a LATER directory still suppresses a deletion in an earlier one.
#[cfg(unix)]
#[test]
fn delete_after_is_suppressed_by_any_send_time_open_failure() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("src");
    let dest = temp.path().join("dst");
    fs::create_dir_all(source.join("a")).expect("mkdir src/a");
    fs::create_dir_all(source.join("z")).expect("mkdir src/z");
    fs::create_dir_all(dest.join("a")).expect("mkdir dst/a");
    fs::write(source.join("a/keep"), b"keep").expect("write keep");
    fs::write(dest.join("a/stale"), b"stale").expect("write stale");
    fs::write(source.join("z/bad"), b"bad").expect("write bad");
    if !make_unreadable(&source.join("z/bad")) {
        return;
    }
    let operands = vec![trailing_operand(&source), dest.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default()
        .recursive(true)
        .delete_after(true);
    let result = plan.execute_with_options(LocalCopyExecution::Apply, options);
    restore_readable(&source.join("z/bad"));
    assert!(
        dest.join("a/stale").exists(),
        "--delete-after must see a send-time IO error from any directory"
    );
    assert_eq!(result.expect_err("unreadable source").exit_code(), 23);
}

/// A walk-time general error (an unreadable directory) still suppresses the
/// `--delete-after` pass, as before: the after pass sees every `IOERR_GENERAL`.
#[cfg(unix)]
#[test]
fn delete_after_is_suppressed_by_a_walk_time_error() {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("src");
    let dest = temp.path().join("dst");
    fs::create_dir_all(source.join("bad")).expect("mkdir src/bad");
    fs::create_dir_all(source.join("z")).expect("mkdir src/z");
    fs::create_dir_all(dest.join("z")).expect("mkdir dst/z");
    fs::write(source.join("bad/inner"), b"inner").expect("write inner");
    fs::write(source.join("z/keep"), b"keep").expect("write keep");
    fs::write(dest.join("z/stale"), b"stale").expect("write stale");
    fs::set_permissions(source.join("bad"), fs::Permissions::from_mode(0o000)).expect("chmod");
    if fs::read_dir(source.join("bad")).is_ok() {
        eprintln!("SKIP: unreadable directory is still readable (running privileged)");
        let _ = fs::set_permissions(source.join("bad"), fs::Permissions::from_mode(0o755));
        return;
    }
    let operands = vec![trailing_operand(&source), dest.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default()
        .recursive(true)
        .delete_after(true);
    let result = plan.execute_with_options(LocalCopyExecution::Apply, options);
    let _ = fs::set_permissions(source.join("bad"), fs::Permissions::from_mode(0o755));
    assert!(
        dest.join("z/stale").exists(),
        "a walk-time error must keep z/stale"
    );
    assert!(result.is_err(), "an unreadable directory must fail the run");
}
