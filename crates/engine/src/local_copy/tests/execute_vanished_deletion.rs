// A source that vanishes between the walk and its send.
//
// upstream: sender.c:714-716 reports `file has vanished` and sets
// IOERR_VANISHED, a bit generator.c:304 `delete_in_dir()` does not test - only
// IOERR_GENERAL suppresses deletion - and cleanup.c:213-216 turns it into exit
// 24. The vanish is reported once, by the frame that tried to send the file.

/// Deletes `target` when the first `DataCopied` record arrives, so the planned
/// sibling disappears between the walk and its send.
#[cfg(unix)]
struct VanishOnFirstCopy {
    target: PathBuf,
    fired: bool,
}

#[cfg(unix)]
impl LocalCopyRecordHandler for VanishOnFirstCopy {
    fn handle(&mut self, record: LocalCopyRecord) {
        if !self.fired && record.action() == &LocalCopyAction::DataCopied {
            self.fired = true;
            fs::remove_file(&self.target).expect("remove the vanishing sibling");
        }
    }
}

/// A file that vanishes between the walk and its send sets `IOERR_VANISHED`,
/// which gates no delete pass at any timing.
#[cfg(unix)]
fn assert_vanished_file_gates_no_deletion(options: LocalCopyOptions) {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("src");
    let dest = temp.path().join("dst");
    fs::create_dir_all(source.join("a")).expect("mkdir src/a");
    fs::create_dir_all(source.join("z")).expect("mkdir src/z");
    fs::create_dir_all(dest.join("z")).expect("mkdir dst/z");
    fs::write(source.join("a/1"), b"one").expect("write a/1");
    fs::write(source.join("a/2"), b"two").expect("write a/2");
    fs::write(source.join("z/keep"), b"keep").expect("write keep");
    fs::write(dest.join("z/stale"), b"stale").expect("write stale");
    let operands = vec![trailing_operand(&source), dest.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let mut observer = VanishOnFirstCopy {
        target: source.join("a/2"),
        fired: false,
    };
    let result = plan.execute_with_options_and_handler(
        LocalCopyExecution::Apply,
        options.recursive(true),
        Some(&mut observer),
    );
    assert!(observer.fired, "fixture must remove a/2 before its send");
    assert!(
        !dest.join("z/stale").exists(),
        "a vanished file must not suppress deletion"
    );
    assert_eq!(
        result
            .expect_err("a vanished file exits non-zero")
            .exit_code(),
        24,
        "a vanished file exits RERR_VANISHED"
    );
}

#[cfg(unix)]
#[test]
fn vanished_file_does_not_suppress_delete_during() {
    assert_vanished_file_gates_no_deletion(LocalCopyOptions::default().delete(true));
}

#[cfg(unix)]
#[test]
fn vanished_file_does_not_suppress_delete_after() {
    assert_vanished_file_gates_no_deletion(LocalCopyOptions::default().delete_after(true));
}
